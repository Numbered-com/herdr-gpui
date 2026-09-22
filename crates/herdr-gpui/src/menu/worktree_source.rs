//! Where a new worktree's branch comes from: a branch typed by hand, an open
//! pull request, or an open issue. The two GitHub tabs are pickers over one
//! bounded listing, filtered as you type; picking a row creates the checkout
//! and leaves a note in it naming the pull request or issue it is for.

use super::{Page, WorkspaceAction};
use crate::{
    HerdrWindow,
    repo_items::{self, Item, Kind},
    search_input::SearchInput,
};
use gpui::{Entity, Subscription, UniformListScrollHandle, prelude::*};

/// Which part of the new worktree dialog is showing. A closed set: the tab
/// strip, the key routing, and the panel geometry all match on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tab {
    /// The branch field and the checkout it previews.
    Branch,
    /// A listing of the repository's open pull requests or issues.
    Items(Kind),
}

impl Tab {
    pub(super) const ALL: [Self; 3] = [
        Self::Branch,
        Self::Items(Kind::PullRequest),
        Self::Items(Kind::Issue),
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Branch => "new",
            Self::Items(kind) => kind.tab_label(),
        }
    }

    pub(super) fn kind(self) -> Option<Kind> {
        match self {
            Self::Branch => None,
            Self::Items(kind) => Some(kind),
        }
    }
}

/// The dialog's tab state and the listing behind its GitHub tabs.
pub(crate) struct WorktreeSource {
    pub(super) tab: Tab,
    pub(super) search: Entity<SearchInput>,
    /// Indices into `lookup.items` for the open tab, in listed order.
    pub(super) filtered: Vec<usize>,
    pub(super) selected: usize,
    pub(super) scroll: UniformListScrollHandle,
    query: String,
    pub(super) lookup: repo_items::Lookup,
    /// The row a creation is running for, kept from the moment it is picked
    /// until the daemon answers, so the note can name what the checkout is for.
    pub(super) pending: Option<Item>,
    _subscription: Subscription,
}

impl WorktreeSource {
    /// Rebuild the visible rows for the open tab. Search matches the number,
    /// title and author of each row, as the theme picker matches names.
    pub(super) fn filter(&mut self, query: &str) {
        self.query = query.to_owned();
        let query = query.trim().to_lowercase();
        let kind = self.tab.kind();
        self.filtered = self
            .lookup
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| Some(item.kind) == kind)
            .filter(|(_, item)| query.is_empty() || item.search_key().contains(&query))
            .map(|(index, _)| index)
            .collect();
        self.selected = 0;
        self.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
    }

    pub(super) fn item(&self, row: usize) -> Option<&Item> {
        self.lookup.items.get(*self.filtered.get(row)?)
    }

    /// How many of this tab's listed rows the search kept, and how many there
    /// were, for the count line above the list.
    pub(super) fn counts(&self) -> (usize, usize) {
        let kind = self.tab.kind();
        (
            self.filtered.len(),
            self.lookup
                .items
                .iter()
                .filter(|item| Some(item.kind) == kind)
                .count(),
        )
    }

    /// A creation is under way, so the list must not start another.
    pub(super) fn busy(&self) -> bool {
        self.pending.is_some()
    }
}

impl HerdrWindow {
    /// The GitHub tab the new worktree dialog is showing, if any. Geometry, key
    /// routing and input isolation all branch on this.
    pub(crate) fn worktree_list_tab(&self) -> Option<Kind> {
        if self.menu.page != Some(Page::Dialog(WorkspaceAction::NewWorktree)) {
            return None;
        }
        self.menu
            .worktree
            .as_ref()
            .and_then(|source| source.tab.kind())
    }

    pub(super) fn open_worktree_source(&mut self, cx: &mut Context<Self>) {
        let search = cx.new(SearchInput::new);
        search.update(cx, |input, cx| {
            input.set_placeholder("Search...", cx);
            input.set_appearance(self.config.ui.clone(), self.theme.clone(), cx);
        });
        let subscription = cx.subscribe(
            &search,
            |this, search, _: &crate::search_input::Changed, cx| {
                let text = search.read(cx).text().to_owned();
                if let Some(source) = &mut this.menu.worktree {
                    source.filter(&text);
                }
                cx.notify();
            },
        );
        self.menu.worktree = Some(WorktreeSource {
            tab: Tab::Branch,
            search,
            filtered: Vec::new(),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            query: String::new(),
            lookup: Default::default(),
            pending: None,
            _subscription: subscription,
        });
    }

    /// Move to `tab`, focusing whichever field that tab types into and starting
    /// the listing the first time a GitHub tab is opened.
    pub(super) fn select_worktree_tab(
        &mut self,
        tab: Tab,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if tab.kind().is_some() && !self.menu.github.connected() {
            return;
        }
        let Some(source) = &mut self.menu.worktree else {
            return;
        };
        if source.busy() {
            return;
        }
        source.tab = tab;
        let search = source.search.clone();
        source.filter(search.read(cx).text());
        match tab {
            Tab::Branch => window.focus(&self.menu.focus),
            Tab::Items(kind) => {
                let placeholder = match kind {
                    Kind::PullRequest => "Search pull requests...",
                    Kind::Issue => "Search issues...",
                };
                search.update(cx, |input, cx| input.set_placeholder(placeholder, cx));
                window.focus(&search.read(cx).focus);
                self.list_repo_items();
            }
        }
        cx.notify();
    }

    /// Ask for the repository's open pull requests and issues. One listing
    /// serves both tabs, and it is only requested once per opened dialog.
    fn list_repo_items(&mut self) {
        let Some(source) = &self.menu.worktree else {
            return;
        };
        if source.lookup.listed() || source.lookup.loading {
            return;
        }
        match self.repo_items_request() {
            Ok((input, token)) => {
                if let Some(source) = &mut self.menu.worktree {
                    source.lookup.list(input, token);
                }
            }
            Err(error) => {
                if let Some(source) = &mut self.menu.worktree {
                    source.lookup.message = Some(error.to_string());
                }
            }
        }
    }

    /// The checkout to read and the token to read GitHub with, under the same
    /// endpoint and staleness rules the PR section already enforces.
    fn repo_items_request(
        &self,
    ) -> crate::Result<(
        crate::pull_request::Input,
        std::sync::Arc<secrecy::SecretString>,
    )> {
        let target = self
            .menu
            .target
            .as_ref()
            .ok_or(crate::Error::StaleWorkspace)?;
        if !self.menu_target_current() {
            return Err(crate::Error::StaleWorkspace);
        }
        if self.selected_endpoint != 0 || !self.live.local_daemon_peer {
            return Err(crate::Error::PrUntrustedEndpoint);
        }
        let token = self
            .menu
            .github
            .profile
            .as_ref()
            .map(|profile| profile.token.clone())
            .ok_or(crate::Error::GitHubAuthentication)?;
        let input = crate::pull_request::repository_input(
            target.worktree.as_ref(),
            target.branch.as_deref(),
        )?;
        Ok((input, token))
    }

    /// Create a checkout for the row at `row` of the open tab. A pull request
    /// branch is made reachable first; an issue's branch is new, so its
    /// creation is queued straight away.
    pub(super) fn create_from_repo_item(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(source) = &self.menu.worktree else {
            return;
        };
        if source.busy() || self.menu.creation.is_some() {
            return;
        }
        let Some(item) = source.item(row).cloned() else {
            return;
        };
        if let Some(owner) = item.fork_owner.clone() {
            self.menu.error = Some(
                crate::Error::ForkPullRequest {
                    number: item.number,
                    owner,
                }
                .to_string(),
            );
            cx.notify();
            return;
        }
        self.menu.error = None;
        match (item.head.clone(), self.repo_items_request()) {
            // An existing pull request branch may only exist on the remote, so
            // `origin/<branch>` is refreshed before the daemon is asked for it.
            (Some(head), Ok((input, token))) => {
                if let Some(source) = &mut self.menu.worktree {
                    source.pending = Some(item);
                    source.lookup.fetch_branch(input, token, &head);
                }
                cx.notify();
            }
            (Some(_), Err(error)) => {
                self.menu.error = Some(error.to_string());
                cx.notify();
            }
            (None, _) => {
                if let Some(source) = &mut self.menu.worktree {
                    source.pending = Some(item.clone());
                }
                self.submit_repo_item(&item, cx);
            }
        }
    }

    /// Queue `worktree.create` for `item`, under the same fences a typed branch
    /// submission uses. The branch is the pull request's own head, or the one
    /// the issue names; an existing branch is checked out by the daemon.
    fn submit_repo_item(&mut self, item: &Item, cx: &mut Context<Self>) {
        let result = (|| {
            if !self.menu_target_current() {
                return Err(crate::Error::StaleConnection);
            }
            if !self.live.status.is_connected() {
                return Err(crate::Error::NotConnected);
            }
            let target = self
                .menu
                .target
                .as_ref()
                .ok_or(crate::Error::StaleWorkspace)?;
            let snapshot = self
                .live
                .snapshot
                .as_ref()
                .ok_or(crate::Error::NoSnapshot)?;
            let (method, params) = item_request(target, snapshot, item)?;
            self.endpoints[self.selected_endpoint]
                .connection
                .request_dialog(&target.boot_id, method, params)
        })();
        match result {
            Ok(id) => {
                self.menu.creation = Some(id);
                self.menu.error = None;
                self.local_error = None;
                self.fence_focus_change(None);
                cx.notify();
            }
            Err(error) => {
                if let Some(source) = &mut self.menu.worktree {
                    source.pending = None;
                }
                self.menu.error = Some(error.to_string());
                cx.notify();
            }
        }
    }

    /// Drain the listing worker: report what it found, and queue the creation a
    /// finished branch fetch was preparing for.
    pub(crate) fn poll_worktree_source(&mut self, cx: &mut Context<Self>) {
        let Some(source) = &mut self.menu.worktree else {
            return;
        };
        if !source.lookup.poll() {
            return;
        }
        let ready = source.lookup.ready.take();
        let query = source.search.read(cx).text().to_owned();
        source.filter(&query);
        let pending = match ready {
            // The fetch succeeded for the branch that is still being created.
            Some(branch)
                if source
                    .pending
                    .as_ref()
                    .is_some_and(|item| item.head.as_deref() == Some(branch.as_str())) =>
            {
                source.pending.clone()
            }
            Some(_) => None,
            None => {
                // A failed fetch leaves nothing to create; its message is shown.
                if source.lookup.message.is_some() {
                    source.pending = None;
                }
                None
            }
        };
        match pending {
            Some(item) => self.submit_repo_item(&item, cx),
            None => cx.notify(),
        }
    }

    /// Write the note naming what the created checkout is for. The daemon owns
    /// the checkout, so its own reported path is used rather than a guess, and
    /// the write itself is file I/O and never runs on the UI thread.
    pub(super) fn write_worktree_note(
        &mut self,
        response: &serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let Some(source) = &mut self.menu.worktree else {
            return;
        };
        let (Some(item), Some(origin)) = (source.pending.take(), source.lookup.origin.clone())
        else {
            return;
        };
        let Some(checkout) = response["workspace"]["worktree"]["checkout_path"]
            .as_str()
            .filter(|path| std::path::Path::new(path).is_absolute())
            .map(std::path::PathBuf::from)
        else {
            self.local_error = Some("The daemon did not report the new checkout path, so no agent context note was written.".into());
            return;
        };
        let write = cx
            .background_executor()
            .spawn(async move { repo_items::write_context(&checkout, &item, &origin) });
        cx.spawn(async move |this, cx| {
            let result = write.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.local_error = Some(error.to_string());
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

/// The `worktree.create` a picked row asks for. It goes through the same
/// validation a typed branch does; only the base and the label differ, and
/// neither widens what may be created.
///
/// A pull request's head may exist only on `origin`, so its base names the
/// remote-tracking ref the fetch just updated. The daemon checks out a branch
/// that already exists locally and ignores the base in that case. An issue's
/// branch is new, so it keeps the dialog's default base of `HEAD`.
pub(super) fn item_request(
    target: &super::WorkspaceTarget,
    snapshot: &herdr_client::protocol::ClientShellSnapshot,
    item: &Item,
) -> crate::Result<(herdr_client::Method, serde_json::Value)> {
    let branch = item.branch();
    let (method, mut params) = target.request(snapshot, WorkspaceAction::NewWorktree, &branch)?;
    if item.head.is_some() {
        params["base"] = format!("origin/{branch}").into();
    }
    // The checkout is named for what it is for, so the sidebar shows it too.
    params["label"] = item.label().into();
    Ok((method, params))
}

/// The finished listing, for tests that drive the tabs without a network.
#[cfg(test)]
impl WorktreeSource {
    pub(crate) fn install(&mut self, origin: repo_items::Origin, items: Vec<Item>) {
        self.lookup.origin = Some(origin);
        self.lookup.items = items;
        self.lookup.loading = false;
    }
}
