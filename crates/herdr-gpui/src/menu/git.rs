//! The titlebar's Git actions popup: commit, push, and pull request creation
//! for the focused local checkout.
use super::*;
use crate::git::{Action, Status};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Row {
    Commit,
    Push,
    PullRequest,
}

impl Row {
    fn icon(self) -> &'static str {
        match self {
            Self::Commit => "icons/pencil.svg",
            Self::Push => "icons/chevron-up.svg",
            Self::PullRequest => "icons/git-branch.svg",
        }
    }
}

/// Stats as the titlebar and the popup header both show them.
pub(super) fn summary(status: Option<Status>) -> String {
    let Some(status) = status else {
        return "Checking working tree...".into();
    };
    if !status.dirty() {
        return "No uncommitted changes".into();
    }
    let mut parts = vec![format!("+{} -{}", status.additions, status.deletions)];
    if status.untracked > 0 {
        parts.push(format!(
            "{} untracked {}",
            status.untracked,
            if status.untracked == 1 {
                "entry"
            } else {
                "entries"
            }
        ));
    }
    parts.join(", ")
}

impl HerdrWindow {
    /// Follow the focused checkout and drain worker results. Called from the
    /// window's poll task, never from a render or input path.
    pub(crate) fn update_git(&mut self) -> bool {
        let input = self.git_input();
        let now = std::time::Instant::now();
        let mut changed = self.git.track(input, self.active, now);
        changed |= self.git.poll(now);
        changed
    }

    /// Local, owned daemon sockets only: the same trust boundary the PR lookup
    /// uses, because both run Git against the user's own checkouts.
    fn git_input(&self) -> Option<crate::pull_request::Input> {
        if self.selected_endpoint != 0
            || !self.live.local_daemon_peer
            || !self.live.status.is_connected()
        {
            return None;
        }
        let snapshot = self.live.snapshot.as_ref()?;
        let workspace = snapshot
            .workspaces
            .iter()
            .find(|workspace| {
                Some(workspace.workspace_id.as_str()) == snapshot.focused_workspace_id.as_deref()
            })
            .or_else(|| {
                snapshot
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.focused)
            })?;
        crate::pull_request::repository_input(
            workspace.worktree.as_ref(),
            workspace.branch.as_deref(),
        )
        .ok()
    }

    pub(crate) fn open_git_menu(
        &mut self,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu.page.is_some() {
            self.dismiss_menu(window, cx);
            return;
        }
        if self.git.tracked().is_none() {
            return;
        }
        self.menu.reset();
        self.menu.anchor = anchor;
        self.menu.page = Some(Page::Git);
        self.marked.clear();
        window.focus(&self.menu.focus);
        cx.notify();
    }

    /// The pull request already prefetched for the focused branch, if any.
    fn git_pull_request(&self) -> Option<&crate::pull_request::PullRequest> {
        let input = self.git.tracked()?;
        self.menu
            .github
            .connected()
            .then(|| self.menu.pr_cache.peek(&input.repo_key, &input.branch))
            .flatten()
            .filter(|pr| pr.state == "OPEN")
    }

    pub(super) fn git_rows(&self) -> Vec<(Row, String)> {
        if self.git.tracked().is_none() {
            return Vec::new();
        }
        vec![
            (Row::Commit, "Commit...".into()),
            (Row::Push, "Push".into()),
            match self.git_pull_request() {
                Some(pr) => (
                    Row::PullRequest,
                    format!("Open pull request #{}", pr.number),
                ),
                None => (Row::PullRequest, "Create pull request".into()),
            },
        ]
    }

    pub(super) fn activate_git_row(
        &mut self,
        row: Row,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.git.running().is_some() {
            return;
        }
        match row {
            Row::Commit => {
                self.menu.page = Some(Page::GitCommit);
                self.menu.input = Some(DialogInput::default());
            }
            Row::Push => self.start_git(Action::Push),
            Row::PullRequest => {
                if let Some(url) = self.git_pull_request().map(|pr| pr.url.clone()) {
                    cx.open_url(&url);
                    self.dismiss_menu(window, cx);
                    return;
                }
                self.start_git(Action::CreatePullRequest);
            }
        }
        cx.notify();
    }

    fn start_git(&mut self, action: Action) {
        let token = self
            .menu
            .github
            .profile
            .as_ref()
            .map(|profile| profile.token.clone());
        if let Err(error) = self.git.start(action, token) {
            self.menu.error = Some(error.to_string());
        } else {
            self.menu.error = None;
        }
    }

    pub(super) fn submit_git_commit(&mut self, cx: &mut Context<Self>) {
        if self
            .menu
            .input
            .as_ref()
            .is_some_and(|input| input.marked.is_some())
        {
            return;
        }
        let message = self
            .menu
            .input
            .as_ref()
            .map(|input| input.text.trim().to_owned())
            .unwrap_or_default();
        self.start_git(Action::Commit(message));
        if self.menu.error.is_none() {
            self.menu.input = None;
            self.menu.page = Some(Page::Git);
        }
        cx.notify();
    }

    pub(super) fn git_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows = self.git_rows();
        match event.keystroke.key.as_str() {
            "escape" => self.dismiss_menu(window, cx),
            "up" | "down" if !rows.is_empty() => {
                let selected = self
                    .menu
                    .git_selected
                    .and_then(|selected| rows.iter().position(|(row, _)| *row == selected));
                let index = match (selected, event.keystroke.key.as_str()) {
                    (None, "up") => rows.len() - 1,
                    (None, _) => 0,
                    (Some(index), "up") => (index + rows.len() - 1) % rows.len(),
                    (Some(index), _) => (index + 1) % rows.len(),
                };
                self.menu.git_selected = Some(rows[index].0);
                cx.notify();
            }
            "enter" => {
                if let Some(row) = self
                    .menu
                    .git_selected
                    .filter(|row| rows.iter().any(|(candidate, _)| candidate == row))
                {
                    self.activate_git_row(row, window, cx);
                }
            }
            _ => {}
        }
    }

    pub(super) fn render_git_menu(&self, cx: &mut Context<Self>) -> Div {
        let theme = &self.theme;
        let font = &self.config.ui;
        let mut panel = div().flex().flex_col();
        if let Some(input) = self.git.tracked() {
            panel = panel.child(
                div()
                    .debug_selector(|| "git-menu-branch".into())
                    .px(px(8.))
                    .pt(px(4.))
                    .truncate()
                    .child(input.branch.clone()),
            );
        }
        panel = panel.child(
            div()
                .debug_selector(|| "git-menu-summary".into())
                .px(px(8.))
                .pb(px(4.))
                .text_color(rgb(theme.muted))
                .child(summary(self.git.status())),
        );
        let running = self.git.running().is_some();
        for (row, label) in self.git_rows() {
            let selected = self.menu.git_selected == Some(row);
            panel = panel.child(
                div()
                    .id(SharedString::from(label.clone()))
                    .debug_selector({
                        let label = label.clone();
                        move || format!("git-menu-{label}")
                    })
                    .min_h(px(font.line_height() + 12.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(3.))
                    .when(!running, |item| item.cursor_pointer())
                    .when(running, |item| item.text_color(rgb(theme.muted)))
                    .when(selected && !running, |item| item.bg(rgb(theme.active)))
                    .on_hover(cx.listener(move |this, hovered, _, cx| {
                        if *hovered {
                            this.menu.git_selected = Some(row);
                        } else if this.menu.git_selected == Some(row) {
                            this.menu.git_selected = None;
                        }
                        cx.notify();
                    }))
                    .child(
                        svg()
                            .path(row.icon())
                            .size(px(14.))
                            .flex_none()
                            .text_color(rgb(theme.muted)),
                    )
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.activate_git_row(row, window, cx);
                    })),
            );
        }
        if let Some(action) = self.git.running() {
            panel = panel.child(
                div()
                    .debug_selector(|| "git-menu-running".into())
                    .p(px(8.))
                    .text_color(rgb(theme.palette[3]))
                    .child(action.running_label()),
            );
        }
        if let Some(outcome) = self.git.outcome() {
            panel = panel.child(
                div()
                    .debug_selector(|| "git-menu-outcome".into())
                    .p(px(8.))
                    .child(outcome.message.clone()),
            );
            if let Some(url) = outcome.url.clone() {
                panel = panel.child(
                    div()
                        .id("git-menu-open")
                        .debug_selector(|| "git-menu-open".into())
                        .px(px(8.))
                        .pb(px(8.))
                        .cursor_pointer()
                        .text_color(accent(theme))
                        .child("Open in browser")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            cx.open_url(&url);
                            this.dismiss_menu(window, cx);
                        })),
                );
            }
        }
        for error in self
            .git
            .error()
            .into_iter()
            .chain(self.menu.error.as_deref())
        {
            panel = panel.child(
                div()
                    .debug_selector(|| "git-menu-error".into())
                    .p(px(8.))
                    .text_color(rgb(theme.palette[1]))
                    .child(error.to_owned()),
            );
        }
        panel
    }

    pub(super) fn render_git_commit(&self, cx: &mut Context<Self>) -> Div {
        let theme = &self.theme;
        let mut panel = div()
            .flex()
            .flex_col()
            .child(div().p(px(8.)).child("Commit"))
            .child(div().px(px(8.)).text_color(rgb(theme.muted)).child(format!(
                "Stages every change in the checkout, then commits. {}",
                summary(self.git.status())
            )));
        if self.menu.input.is_some() {
            panel = panel.child(self.render_dialog_input(cx));
        }
        if let Some(error) = &self.menu.error {
            panel = panel.child(
                div()
                    .debug_selector(|| "git-commit-error".into())
                    .p(px(8.))
                    .text_color(rgb(theme.palette[1]))
                    .child(error.clone()),
            );
        }
        panel.child(
            div()
                .flex()
                .gap(px(16.))
                .p(px(8.))
                .child(
                    div()
                        .id("git-commit-cancel")
                        .cursor_pointer()
                        .child("Cancel (Escape)")
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.dismiss_menu(window, cx);
                        })),
                )
                .child(
                    div()
                        .id("git-commit-submit")
                        .debug_selector(|| "git-commit-submit".into())
                        .cursor_pointer()
                        .child("Commit")
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.submit_git_commit(cx);
                        })),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::{Page, Row, summary};
    use crate::git::Status;
    use gpui::TestAppContext;
    use std::sync::Arc;

    fn status(additions: u64, deletions: u64, untracked: u64) -> Status {
        Status {
            additions,
            deletions,
            untracked,
        }
    }

    #[test]
    fn summary_reads_as_a_sentence_before_and_after_the_first_refresh() {
        assert_eq!(summary(None), "Checking working tree...");
        assert_eq!(summary(Some(Status::default())), "No uncommitted changes");
        assert_eq!(summary(Some(status(12, 3, 0))), "+12 -3");
        assert_eq!(summary(Some(status(12, 3, 1))), "+12 -3, 1 untracked entry");
        assert_eq!(summary(Some(status(0, 0, 4))), "+0 -0, 4 untracked entries");
    }

    #[gpui::test]
    fn only_a_local_daemon_checkout_is_tracked(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        cx.update(|_, cx| {
            view.update(cx, |view, _| {
                assert!(view.git_input().is_none(), "no connection, no checkout");
                view.live.status = crate::state::ConnectionStatus::Connected;
                view.live.local_daemon_peer = true;
                let mut snapshot = crate::sidebar::layout_tests::snapshot(6);
                snapshot.focused_workspace_id = Some("w3".into());
                view.live.snapshot = Some(Arc::new(snapshot));
                let input = view.git_input().unwrap();
                assert_eq!(input.repo_key, "/fixture/agent-launcher/.git");
                assert_eq!(input.branch, "develop");
                assert_eq!(input.checkout, None, "the checkout is resolved by Git");
                // A workspace without worktree metadata cannot be acted on.
                if let Some(snapshot) = view.live.snapshot.as_ref().map(Arc::clone) {
                    let mut snapshot = (*snapshot).clone();
                    snapshot.focused_workspace_id = Some("w0".into());
                    view.live.snapshot = Some(Arc::new(snapshot));
                }
                assert!(view.git_input().is_none());
                if let Some(snapshot) = view.live.snapshot.as_ref().map(Arc::clone) {
                    let mut snapshot = (*snapshot).clone();
                    snapshot.focused_workspace_id = Some("w3".into());
                    view.live.snapshot = Some(Arc::new(snapshot));
                }
                assert!(view.git_input().is_some());
                view.live.local_daemon_peer = false;
                assert!(view.git_input().is_none(), "remote peers run no local Git");
                view.live.local_daemon_peer = true;
                view.selected_endpoint = 0;
                view.live.status = crate::state::ConnectionStatus::Disconnected;
                assert!(view.git_input().is_none());
            })
        });
    }

    #[gpui::test]
    fn the_menu_commits_through_a_dialog_and_refuses_an_empty_message(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        let input = crate::pull_request::Input {
            checkout: None,
            repo_key: "/fixture/agent-launcher/.git".into(),
            branch: "develop".into(),
        };
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.git = crate::git::Git::fixture(input.clone(), status(12, 3, 1));
                view.open_git_menu(gpui::point(gpui::px(900.), gpui::px(20.)), window, cx);
                assert_eq!(view.menu.page, Some(Page::Git));
                let rows: Vec<_> = view
                    .git_rows()
                    .into_iter()
                    .map(|(_, label)| label)
                    .collect();
                assert_eq!(rows, ["Commit...", "Push", "Create pull request"]);
                view.activate_git_row(Row::Commit, window, cx);
                assert_eq!(view.menu.page, Some(Page::GitCommit));
                view.submit_git_commit(cx);
                assert_eq!(
                    view.menu.error.as_deref(),
                    Some(crate::Error::GitCommitMessage.to_string().as_str())
                );
                assert_eq!(view.menu.page, Some(Page::GitCommit), "the dialog stays open");
                assert!(view.git.running().is_none());
                if let Some(dialog) = view.menu.input.as_mut() {
                    dialog.text = "fix: keep the popup open".into();
                }
                view.submit_git_commit(cx);
                assert_eq!(view.menu.error, None);
                assert_eq!(view.menu.page, Some(Page::Git));
                assert!(matches!(
                    view.git.running(),
                    Some(crate::git::Action::Commit(message)) if message == "fix: keep the popup open"
                ));
                // A second action cannot start while the first is running.
                view.activate_git_row(Row::Push, window, cx);
                assert!(matches!(
                    view.git.running(),
                    Some(crate::git::Action::Commit(_))
                ));
                view.dismiss_menu(window, cx);
                assert_eq!(view.menu.page, None);
                assert!(
                    view.git.running().is_some(),
                    "dismissing the popup does not cancel queued work"
                );
            })
        });
    }
}
