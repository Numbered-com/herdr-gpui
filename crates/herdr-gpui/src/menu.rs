use super::dialog_input::DialogInput;
use super::{HerdrWindow, NavigationTarget};
use crate::config::Config;
use gpui::{prelude::*, *};
use herdr_client::protocol::{ClientShellSnapshot, ClientShellWorkspace, ClientShellWorktree};

mod github;
mod pr;

/// Breathing room between a popup and the window's edges, so a list that had
/// to be clamped still shows that it stops short of the frame.
const MENU_MARGIN: f32 = 8.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Page {
    Menu,
    About,
    Preferences,
    Keybinds,
    Themes,
    Palette,
    ConfirmClose,
    Update,
    AppUpdate,
    Install,
    Tab,
    RenameTab,
    Workspace,
    GitHub,
    Dialog(WorkspaceAction),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkspaceAction {
    Rename,
    Close,
    NewWorktree,
    DeleteWorktree,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkspaceMenuAction {
    Dialog(WorkspaceAction),
    /// Fold or unfold the worktree group this workspace heads. Applied at once:
    /// it changes only the sidebar's own view, never the daemon's state.
    Collapse,
    Expand,
    PullRequest,
}

impl WorkspaceMenuAction {
    /// Embedded icon for the row, so each action is recognizable before reading.
    /// The pull request section draws its own header rather than a menu row.
    fn icon(self) -> Option<&'static str> {
        Some(match self {
            Self::Dialog(WorkspaceAction::Rename) => "icons/pencil.svg",
            Self::Dialog(WorkspaceAction::Close) => "icons/close.svg",
            Self::Dialog(WorkspaceAction::NewWorktree) => "icons/plus.svg",
            Self::Dialog(WorkspaceAction::DeleteWorktree) => "icons/trash.svg",
            Self::Collapse => "icons/chevron-up.svg",
            Self::Expand => "icons/chevron-down.svg",
            Self::PullRequest => return None,
        })
    }
}

struct WorkspaceTarget {
    boot_id: String,
    id: String,
    label: String,
    worktree: Option<ClientShellWorktree>,
    close_members: Vec<String>,
    branch: Option<String>,
}

impl WorkspaceTarget {
    fn new(snapshot: &ClientShellSnapshot, workspace: &ClientShellWorkspace) -> Self {
        Self {
            boot_id: snapshot.boot_id.clone(),
            id: workspace.workspace_id.clone(),
            label: workspace.label.clone(),
            worktree: workspace.worktree.clone(),
            close_members: close_members(snapshot, workspace),
            branch: workspace.branch.clone(),
        }
    }

    fn can_create(&self) -> bool {
        self.worktree
            .as_ref()
            .is_some_and(|tree| !tree.is_linked_worktree)
    }

    fn can_delete(&self) -> bool {
        self.worktree
            .as_ref()
            .is_some_and(|tree| tree.is_linked_worktree)
    }

    /// The worktree key this workspace heads, when other checkouts hang off it.
    fn group_key(&self) -> Option<&str> {
        self.worktree
            .as_ref()
            .filter(|tree| !tree.is_linked_worktree && self.close_members.len() > 1)
            .map(|tree| tree.key.as_str())
    }

    fn close_label(&self) -> &'static str {
        if self.close_members.len() > 1 {
            "Close group"
        } else {
            "Close"
        }
    }

    fn request(
        &self,
        snapshot: &ClientShellSnapshot,
        action: WorkspaceAction,
        text: &str,
    ) -> crate::Result<(&'static str, serde_json::Value)> {
        let workspace = snapshot
            .workspaces
            .iter()
            .find(|w| w.workspace_id == self.id)
            .filter(|_| snapshot.boot_id == self.boot_id)
            .ok_or(crate::Error::StaleWorkspace)?;
        let params = match action {
            WorkspaceAction::Rename => {
                let label = text.trim();
                if label.is_empty() {
                    return Err(crate::Error::EmptyWorkspaceLabel);
                }
                (
                    "workspace.rename",
                    serde_json::json!({"workspace_id": self.id, "label": label}),
                )
            }
            WorkspaceAction::Close => {
                if self.worktree != workspace.worktree
                    || self.close_members != close_members(snapshot, workspace)
                {
                    return Err(crate::Error::WorkspaceGroupChanged);
                }
                (
                    "workspace.close",
                    serde_json::json!({"workspace_id": self.id, "close_group": true}),
                )
            }
            WorkspaceAction::NewWorktree => {
                if !self.can_create() || self.worktree != workspace.worktree {
                    return Err(crate::Error::WorkspaceRepositoryChanged);
                }
                let mut params = serde_json::json!({"workspace_id": self.id, "base": "HEAD", "focus": true, "trust_repository": false});
                if !text.trim().is_empty() {
                    params["branch"] = text.trim().into();
                }
                ("worktree.create", params)
            }
            WorkspaceAction::DeleteWorktree => {
                if !self.can_delete() || self.worktree != workspace.worktree {
                    return Err(crate::Error::WorkspaceCheckoutChanged);
                }
                (
                    "worktree.remove",
                    serde_json::json!({"workspace_id": self.id, "force": false, "trust_repository": false}),
                )
            }
        };
        Ok(params)
    }
}

fn close_members(snapshot: &ClientShellSnapshot, workspace: &ClientShellWorkspace) -> Vec<String> {
    let mut members: Vec<_> = snapshot
        .workspaces
        .iter()
        .filter(|w| {
            w.workspace_id == workspace.workspace_id
                || workspace.worktree.as_ref().is_some_and(|tree| {
                    !tree.is_linked_worktree
                        && w.worktree
                            .as_ref()
                            .is_some_and(|other| tree.key == other.key)
                })
        })
        .map(|w| w.workspace_id.clone())
        .collect();
    members.sort();
    members
}

pub(super) struct MenuState {
    pub page: Option<Page>,
    // Selection epoch and connection generation fence captured modal actions.
    endpoint_target: (u64, u64),
    pub anchor: Point<Pixels>,
    pub focus: FocusHandle,
    selected: Option<usize>,
    workspace_selected: Option<WorkspaceMenuAction>,
    target: Option<WorkspaceTarget>,
    pub input: Option<DialogInput>,
    error: Option<String>,
    deletion: Option<Deletion>,
    /// The correlated `worktree.create` request, so the dialog can report the
    /// daemon's answer and follow the checkout it actually created.
    creation: Option<String>,
    keybinds_scroll: ScrollHandle,
    pub(super) keybinds_search: Option<Entity<crate::search_input::SearchInput>>,
    _keybinds_subscription: Option<Subscription>,
    pub(super) preferences_scroll: ScrollHandle,
    pub(super) themes: Option<crate::theme_picker::ThemePicker>,
    pub(super) palette: Option<crate::palette::Palette>,
    pub(super) close: Option<crate::close_modal::CloseConfirmation>,
    pub(super) tab: Option<crate::tab_menu::TabMenu>,
    pub(super) pr: crate::pull_request::Lookup,
    /// Also read by the sidebar, which paints each worktree's cached PR badge.
    pub(super) pr_cache: crate::pull_request::Cache,
    pr_cache_connection: Option<std::sync::Weak<std::sync::Mutex<crate::state::LiveState>>>,
    pr_snapshot: Option<std::sync::Weak<ClientShellSnapshot>>,
    pub(super) github: crate::github::Auth,
    github_selected: Option<github::Action>,
    github_scroll: ScrollHandle,
    pr_connection: Option<std::sync::Weak<std::sync::Mutex<crate::state::LiveState>>>,
}

struct Deletion {
    pending: Option<String>,
    path: Option<String>,
    force: bool,
}

impl Deletion {
    fn confirmed(&self, text: &str) -> bool {
        self.pending.is_none()
            && self.path.is_some()
            && text == if self.force { "FORCE DELETE" } else { "DELETE" }
    }
}

/// Mix the theme's blue with foreground so accents remain readable on dark themes.
pub(super) fn accent(theme: &crate::config::Theme) -> Rgba {
    rgb(theme.foreground).blend(rgba((theme.palette[4] << 8) | 0x70))
}

impl MenuState {
    fn apply_deletion_response(&mut self, id: &str, result: crate::state::DialogResponse) {
        let Some(deletion) = &mut self.deletion else {
            return;
        };
        if deletion.pending.as_deref() != Some(id) {
            return;
        }
        deletion.pending = None;
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                self.error = Some(error.to_string());
                return;
            }
        };
        if let Some(error) = response.get("error") {
            self.error = Some(format!(
                "{}: {}",
                error["code"].as_str().unwrap_or("endpoint_error"),
                error["message"].as_str().unwrap_or("Invalid daemon error")
            ));
            if deletion.path.is_some()
                && !deletion.force
                && error["code"] == "dirty_worktree_requires_force"
            {
                deletion.force = true;
                self.input = Some(DialogInput::default());
            }
            return;
        }
        let result = &response["result"];
        let Some(target) = &self.target else {
            return;
        };
        if deletion.path.is_none() && result["type"] == "worktree_list" {
            let entry = result["worktrees"].as_array().and_then(|entries| {
                let mut matches = entries
                    .iter()
                    .filter(|entry| entry["open_workspace_id"] == target.id);
                let entry = matches.next()?;
                (matches.next().is_none()
                    && entry["is_linked_worktree"] == true
                    && entry["is_bare"] == false)
                    .then_some(entry)
            });
            deletion.path = entry
                .and_then(|entry| entry["path"].as_str())
                .filter(|path| !path.is_empty())
                .map(str::to_owned);
            if deletion.path.is_none() {
                self.error = Some("Daemon did not identify a unique linked checkout. Dismiss and reopen the menu.".into());
            }
        } else if result["type"] == "worktree_removed"
            && result["workspace_id"] == target.id
            && result["path"].as_str() == deletion.path.as_deref()
            && result["forced"] == deletion.force
        {
            self.reset();
        } else {
            self.error = Some(
                "Unexpected daemon response. Review current workspace state before retrying."
                    .into(),
            );
        }
    }

    pub fn new(cx: &App) -> Self {
        Self {
            page: None,
            endpoint_target: (0, 0),
            anchor: Point::default(),
            focus: cx.focus_handle(),
            selected: None,
            workspace_selected: None,
            target: None,
            input: None,
            error: None,
            deletion: None,
            creation: None,
            keybinds_scroll: ScrollHandle::new(),
            keybinds_search: None,
            _keybinds_subscription: None,
            preferences_scroll: ScrollHandle::new(),
            themes: None,
            palette: None,
            close: None,
            pr: Default::default(),
            pr_cache: Default::default(),
            pr_cache_connection: None,
            pr_snapshot: None,
            github: Default::default(),
            github_selected: None,
            github_scroll: ScrollHandle::new(),
            pr_connection: None,
            tab: None,
        }
    }

    pub fn reset(&mut self) {
        self.tab = None;
        self.github_selected = None;
        self.github_scroll.set_offset(Point::default());
        if self.github.busy() {
            self.github.cancel();
        }
        self.page = None;
        self.selected = None;
        self.workspace_selected = None;
        self.target = None;
        self.input = None;
        self.error = None;
        self.deletion = None;
        self.creation = None;
        self.close = None;
        self.pr.clear();
        self.pr_connection = None;
    }
}

impl HerdrWindow {
    pub(super) fn open_keybinds(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open_menu(window, cx) {
            return;
        }
        self.menu.page = Some(Page::Keybinds);
        self.menu.keybinds_scroll.set_offset(Point::default());
        let search = cx.new(crate::search_input::SearchInput::new);
        search.update(cx, |input, cx| {
            input.set_placeholder("Search shortcuts...", cx);
            input.set_appearance(self.config.ui.clone(), self.theme.clone(), cx);
            window.focus(&input.focus);
        });
        self.menu._keybinds_subscription = Some(cx.subscribe(
            &search,
            |this, _, _: &crate::search_input::Changed, cx| {
                this.menu.keybinds_scroll.set_offset(Point::default());
                cx.notify();
            },
        ));
        self.menu.keybinds_search = Some(search);
    }

    pub(super) fn open_preferences(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open_menu(window, cx) {
            return;
        }
        self.menu.page = Some(Page::Preferences);
        self.menu.preferences_scroll.set_offset(Point::default());
    }

    pub(super) fn reload_gui_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.theme_save_in_flight() {
            return;
        }
        self.load_gui_config(cx);
        self.dismiss_menu(window, cx);
    }

    pub(super) fn load_gui_config(&mut self, cx: &mut Context<Self>) {
        self.load_gui_config_with(
            || {
                let config = Config::load()?;
                let theme = config.theme()?;
                Ok((config, theme))
            },
            cx,
        );
    }

    fn load_gui_config_with(
        &mut self,
        load: impl FnOnce() -> crate::Result<(Config, crate::config::Theme)> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.config_load.is_some() {
            return;
        }
        let load = cx.background_executor().spawn(async move { load() });
        self.config_load = Some(cx.spawn(async move |this, cx| {
            let loaded = load.await;
            let _ = this.update(cx, |this, cx| {
                this.config_load = None;
                // Apply a coherent pair only after both have loaded successfully.
                match loaded {
                    Ok((config, theme)) => {
                        if this.avatars.is_some() && this.menu.github.initialize(&config) {
                            this.menu.pr_cache.clear();
                            this.menu.pr.clear();
                            this.menu.pr_connection = None;
                        }
                        this.config = config;
                        this.theme = theme;
                        crate::log_window::set_appearance(&this.config, &this.theme, cx);
                        this.wheel = Default::default();
                        this.last_queued_options = None;
                        this.local_error = None;
                    }
                    Err(error) => this.local_error = Some(format!("Load GUI config: {error}")),
                }
                cx.notify();
            });
        }));
    }

    pub(super) fn show_install_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open_menu(window, cx) {
            return;
        }
        self.menu.page = Some(Page::Install);
    }

    pub(super) fn open_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.cancel_theme_preview(cx) {
            return false;
        }
        self.menu.reset();
        self.menu.endpoint_target = (
            self.selection_epoch,
            self.endpoints[self.selected_endpoint].generation,
        );
        self.menu.page = Some(Page::Menu);
        self.marked.clear();
        window.focus(&self.menu.focus);
        cx.notify();
        true
    }

    pub(super) fn dismiss_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.cancel_theme_preview(cx) {
            return;
        }
        self.update_preview = None;
        self.menu.reset();
        window.focus(&self.focus);
        cx.notify();
    }

    pub(super) fn restore_menu_focus(&self, window: &mut Window) {
        if self.menu.page.is_none() && self.menu.focus.is_focused(window) {
            window.focus(&self.focus);
        }
    }

    pub(super) fn open_workspace_menu(
        &mut self,
        id: &str,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu.page.is_some() || !self.live.status.is_connected() {
            return;
        }
        let Some(snapshot) = &self.live.snapshot else {
            return;
        };
        let Some(workspace) = snapshot.workspaces.iter().find(|w| w.workspace_id == id) else {
            return;
        };
        self.menu.reset();
        self.menu.endpoint_target = (
            self.selection_epoch,
            self.endpoints[self.selected_endpoint].generation,
        );
        self.menu.target = Some(WorkspaceTarget::new(snapshot, workspace));
        self.menu.anchor = anchor;
        self.menu.page = Some(Page::Workspace);
        self.refresh_workspace_pr();
        self.marked.clear();
        window.focus(&self.menu.focus);
        cx.notify();
    }

    fn workspace_items(&self) -> Vec<(WorkspaceMenuAction, &'static str)> {
        use WorkspaceMenuAction::Dialog;
        let Some(target) = &self.menu.target else {
            return vec![];
        };
        let mut items = vec![
            (Dialog(WorkspaceAction::Rename), "Rename"),
            (Dialog(WorkspaceAction::Close), target.close_label()),
        ];
        if target.can_create() {
            items.push((Dialog(WorkspaceAction::NewWorktree), "New worktree"));
        }
        if target.can_delete() {
            items.push((
                Dialog(WorkspaceAction::DeleteWorktree),
                "Delete worktree checkout",
            ));
        }
        // Only a workspace that heads a group of checkouts can fold anything.
        if let Some(key) = target.group_key() {
            items.push(if self.collapsed_repos_for_selection().contains(key) {
                (WorkspaceMenuAction::Expand, "Expand group")
            } else {
                (WorkspaceMenuAction::Collapse, "Collapse group")
            });
        }
        items
    }

    /// The collapsed set the sidebar paints for the selected endpoint.
    fn collapsed_repos_for_selection(&self) -> &std::collections::HashSet<String> {
        if self.selected_endpoint == 0 {
            &self.collapsed_repos
        } else {
            &self.endpoints[self.selected_endpoint].collapsed_repos
        }
    }

    /// The collapsed set the sidebar paints for the selected endpoint.
    fn collapsed_repos_mut(&mut self) -> &mut std::collections::HashSet<String> {
        if self.selected_endpoint == 0 {
            &mut self.collapsed_repos
        } else {
            &mut self.endpoints[self.selected_endpoint].collapsed_repos
        }
    }

    fn toggle_selected_group(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self
            .menu
            .target
            .as_ref()
            .and_then(|target| target.group_key())
            .map(str::to_owned)
        else {
            return;
        };
        let collapsed = self.collapsed_repos_mut();
        if !collapsed.remove(&key) {
            collapsed.insert(key);
        }
        self.menu.reset();
        cx.notify();
    }

    fn open_workspace_dialog(&mut self, action: WorkspaceAction, cx: &mut Context<Self>) {
        let Some(target) = &self.menu.target else {
            return;
        };
        self.menu.input = match action {
            WorkspaceAction::Rename => Some(DialogInput::new(target.label.clone())),
            // Propose the daemon's own branch shape, selected so typing replaces it.
            WorkspaceAction::NewWorktree => {
                Some(DialogInput::new(crate::worktree::proposed_branch()))
            }
            WorkspaceAction::Close => None,
            WorkspaceAction::DeleteWorktree => Some(DialogInput::default()),
        };
        self.menu.page = Some(Page::Dialog(action));
        self.menu.pr.clear();
        self.menu.pr_connection = None;
        self.menu.error = None;
        if action == WorkspaceAction::DeleteWorktree {
            let result = self.endpoints[self.selected_endpoint]
                .connection
                .request_dialog(
                    &target.boot_id,
                    "worktree.list",
                    serde_json::json!({"workspace_id": target.id, "trust_repository": false}),
                );
            self.menu.deletion = Some(Deletion {
                pending: result.as_ref().ok().cloned(),
                path: None,
                force: false,
            });
            self.menu.error = result.err().map(|error| error.to_string());
        }
        cx.notify();
    }

    fn workspace_menu_actions(&self) -> Vec<WorkspaceMenuAction> {
        let mut actions: Vec<_> = self
            .workspace_items()
            .into_iter()
            .map(|(action, _)| action)
            .collect();
        if self.menu.github.connected() && self.menu.pr.value.is_some() {
            actions.push(WorkspaceMenuAction::PullRequest);
        }
        actions
    }

    fn activate_workspace_menu(&mut self, action: WorkspaceMenuAction, cx: &mut Context<Self>) {
        match action {
            WorkspaceMenuAction::Dialog(action) => self.open_workspace_dialog(action, cx),
            WorkspaceMenuAction::Collapse | WorkspaceMenuAction::Expand => {
                self.toggle_selected_group(cx)
            }
            WorkspaceMenuAction::PullRequest => self.open_workspace_pr(cx),
        }
    }

    /// The checkout the daemon would create for the branch currently drafted.
    /// Only a preview: the daemon derives the path it actually uses.
    fn checkout_preview(&self) -> String {
        let repo = self
            .menu
            .target
            .as_ref()
            .and_then(|target| target.worktree.as_ref())
            .map(|worktree| worktree.label.as_str());
        let root = self
            .live
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.worktree_directory.as_str())
            .filter(|root| !root.is_empty());
        let branch = self
            .menu
            .input
            .as_ref()
            .map_or("", |input| input.text.trim());
        match (repo, root) {
            _ if branch.is_empty() => "Checkout: named by the daemon".to_owned(),
            (Some(repo), Some(root)) => format!(
                "Checkout: {}",
                crate::worktree::checkout_preview(root, repo, branch)
            ),
            _ => "Checkout: chosen by the daemon".to_owned(),
        }
    }

    /// Apply the daemon's answer to whichever worktree dialog is waiting for it.
    pub(super) fn update_workspace_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.deletion.is_none() && self.menu.creation.is_none() {
            return;
        }
        if !self.menu_target_current()
            || !self.live.status.is_connected()
            || self.menu.target.as_ref().is_some_and(|target| {
                self.live
                    .snapshot
                    .as_ref()
                    .is_none_or(|snapshot| snapshot.boot_id != target.boot_id)
            })
        {
            self.menu.reset();
            return;
        }
        let Some((id, Some(result))) = &self.live.dialog_response else {
            return;
        };
        if self.menu.deletion.is_some() {
            let (id, result) = (id.clone(), result.clone());
            self.menu.apply_deletion_response(&id, result);
            return;
        }
        if self.menu.creation.as_deref() != Some(id.as_str()) {
            return;
        }
        let result = result.clone();
        self.menu.creation = None;
        self.apply_creation_response(result, window, cx);
    }

    /// Follow the checkout the daemon created. The daemon switches its own
    /// session, but this client shell keeps its own location, so the new
    /// workspace is only selected (and revealed in the sidebar) once this
    /// client focuses it.
    fn apply_creation_response(
        &mut self,
        result: crate::state::DialogResponse,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                self.menu.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        if let Some(error) = response.get("error") {
            self.menu.error = Some(format!(
                "{}: {}",
                error["code"].as_str().unwrap_or("endpoint_error"),
                error["message"].as_str().unwrap_or("Invalid daemon error")
            ));
            cx.notify();
            return;
        }
        let result = &response["result"];
        let created = (result["type"] == "worktree_created")
            .then(|| result["workspace"]["workspace_id"].as_str())
            .flatten()
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        let Some(created) = created else {
            self.menu.error = Some(
                "Unexpected daemon response. Review current workspace state before retrying."
                    .into(),
            );
            cx.notify();
            return;
        };
        let endpoint = self.endpoints[self.selected_endpoint].id.clone();
        // A folded group would hide the new checkout the sidebar is about to select.
        let group = self
            .menu
            .target
            .as_ref()
            .and_then(|target| target.worktree.as_ref())
            .map(|worktree| worktree.key.clone());
        self.dismiss_menu(window, cx);
        if let Some(group) = group {
            self.collapsed_repos_mut().remove(&group);
        }
        self.navigate_endpoint(&endpoint, NavigationTarget::Workspace(&created), cx);
    }

    fn submit_workspace_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Page::Dialog(action)) = self.menu.page else {
            return;
        };
        if self
            .menu
            .input
            .as_ref()
            .is_some_and(|input| input.marked.is_some())
        {
            return;
        }
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
            let text = self
                .menu
                .input
                .as_ref()
                .map(|input| input.text.as_str())
                .unwrap_or("");
            let (method, mut params) = target.request(snapshot, action, text)?;
            if action == WorkspaceAction::DeleteWorktree {
                let deletion = self
                    .menu
                    .deletion
                    .as_ref()
                    .ok_or(crate::Error::MissingDeletion)?;
                if deletion.pending.is_some() {
                    return Ok(false);
                }
                if deletion.path.is_none() {
                    return Err(crate::Error::DeletionLookup);
                }
                let confirmation = if deletion.force {
                    "FORCE DELETE"
                } else {
                    "DELETE"
                };
                if !deletion.confirmed(text) {
                    return Err(crate::Error::DeletionConfirmation(confirmation));
                }
                params["force"] = deletion.force.into();
                let id = self.endpoints[self.selected_endpoint]
                    .connection
                    .request_dialog(&target.boot_id, method, params)?;
                if let Some(deletion) = &mut self.menu.deletion {
                    deletion.pending = Some(id);
                }
                self.menu.error = None;
                return Ok(true);
            }
            if action == WorkspaceAction::NewWorktree {
                if self.menu.creation.is_some() {
                    return Ok(false);
                }
                // Correlated, so the daemon's failure reaches the dialog and the
                // created checkout can be focused once it exists.
                let id = self.endpoints[self.selected_endpoint]
                    .connection
                    .request_dialog(&target.boot_id, method, params)?;
                self.menu.creation = Some(id);
                self.menu.error = None;
                return Ok(true);
            }
            let handle = self.endpoints[self.selected_endpoint]
                .connection
                .handle
                .as_ref()
                .ok_or(crate::Error::NotConnected)?;
            handle
                .request(&target.boot_id, method, params)
                .map(|_| action != WorkspaceAction::Rename)
                .map_err(|source| crate::Error::Request { method, source })
        })();
        match result {
            Ok(focus_changed) => {
                self.local_error = None;
                if focus_changed {
                    self.fence_focus_change(None);
                }
                // Both worktree dialogs stay open until the daemon answers.
                if matches!(
                    action,
                    WorkspaceAction::DeleteWorktree | WorkspaceAction::NewWorktree
                ) {
                    cx.notify();
                } else {
                    self.dismiss_menu(window, cx);
                }
            }
            Err(error) => {
                self.menu.error = Some(error.to_string());
                cx.notify();
            }
        }
    }

    pub(super) fn menu_target_current(&self) -> bool {
        self.menu.endpoint_target
            == (
                self.selection_epoch,
                self.endpoints[self.selected_endpoint].generation,
            )
    }

    fn menu_items(&self) -> Vec<&'static str> {
        let mut items = vec![
            "settings",
            "keybinds",
            "themes",
            "commands",
            "workspaces",
            "reload GUI config",
            "app updates",
            "preview app update",
            "GitHub sign-in",
            "about",
        ];
        if self.live.status.is_connected() {
            items.push("reload daemon config");
        }
        if self
            .live
            .snapshot
            .as_ref()
            .is_some_and(|s| s.update_available.is_some())
        {
            items.push("update ready");
        }
        items.push(
            if self.endpoints[self.selected_endpoint]
                .connection
                .handle
                .is_some()
            {
                "detach"
            } else {
                "reconnect"
            },
        );
        items
    }

    fn activate_menu(&mut self, item: &str, window: &mut Window, cx: &mut Context<Self>) {
        match item {
            "GitHub sign-in" => self.menu.page = Some(Page::GitHub),
            "about" => self.open_about(window, cx),
            "settings" => self.open_preferences(window, cx),
            "keybinds" => self.open_keybinds(window, cx),
            "themes" => self.open_theme_picker(window, cx),
            "commands" => self.open_palette(false, window, cx),
            "workspaces" => self.open_palette(true, window, cx),
            "update ready" => self.menu.page = Some(Page::Update),
            "app updates" => self.open_app_update(false, window, cx),
            "preview app update" => self.open_app_update(true, window, cx),
            "reload GUI config" => self.reload_gui_config(window, cx),
            "reload daemon config" => {
                if let (Some(handle), Some(snapshot)) = (
                    &self.endpoints[self.selected_endpoint].connection.handle,
                    &self.live.snapshot,
                ) {
                    self.local_error = handle
                        .request(
                            &snapshot.boot_id,
                            "server.reload_config",
                            serde_json::json!({}),
                        )
                        .err()
                        .map(|error| format!("Reload config: {error}"));
                }
                self.dismiss_menu(window, cx);
            }
            "detach" => {
                self.detach_endpoint();
                self.dismiss_menu(window, cx);
            }
            "reconnect" => {
                self.reconnect();
                self.dismiss_menu(window, cx);
            }
            _ => {}
        }
        cx.notify();
    }

    pub(super) fn render_menu(&self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let page = self.menu.page.unwrap_or(Page::Menu);
        let font = &self.config.ui;
        let theme = &self.theme;
        let viewport = window.viewport_size();
        let pointer_anchored = matches!(
            page,
            Page::Workspace | Page::Dialog(_) | Page::Tab | Page::RenameTab
        );
        let mut panel = div()
            .id("menu-panel")
            .debug_selector(|| "menu-panel".into())
            .when(matches!(page, Page::Workspace | Page::Dialog(_)), |panel| {
                panel
                    .w(px(if page == Page::Workspace {
                        340.
                    } else if page == Page::Dialog(WorkspaceAction::DeleteWorktree) {
                        480.
                    } else {
                        420.
                    })
                    .min((viewport.width - px(24.)).max(px(0.))))
                    .max_h(
                        (if matches!(
                            page,
                            Page::Workspace | Page::Dialog(WorkspaceAction::DeleteWorktree)
                        ) {
                            viewport.height - px(24.)
                        } else {
                            viewport.height / 2. - px(12.)
                        })
                        .max(px(0.)),
                    )
            })
            .when(page == Page::Menu, |panel| {
                // Open on whichever side of the anchor has room, and keep a
                // margin from the window chrome and the bottom edge: a clamped
                // list then reads as scrollable rather than clipped.
                let chrome = px(crate::titlebar::HEIGHT
                    + crate::worktree_banner::reserved(env!("HERDR_BUILD_WORKTREE") == "1"));
                let band = (viewport.height - chrome - px(2. * MENU_MARGIN)).max(px(60.));
                let room = |side: Pixels| side.clamp(px(0.), band).max(px(60.)).min(band);
                let above = room(self.menu.anchor.y - px(12. + MENU_MARGIN) - chrome);
                let below = room(viewport.height - self.menu.anchor.y - px(12. + MENU_MARGIN));
                let panel = panel.absolute().left(px(56.)).w(px(180.));
                if above >= below {
                    panel
                        .bottom(
                            (viewport.height - self.menu.anchor.y + px(12.)).max(px(MENU_MARGIN)),
                        )
                        .max_h(above)
                } else {
                    panel.top(self.menu.anchor.y + px(12.)).max_h(below)
                }
            })
            .when(matches!(page, Page::Tab | Page::RenameTab), |panel| {
                panel
                    .w((viewport.width - px(24.))
                        .max(px(0.))
                        .min(px(if page == Page::Tab { 180. } else { 360. })))
                    .max_h((viewport.height - px(24.)).max(px(0.)))
            })
            .when(page != Page::Menu && !pointer_anchored, |panel| {
                panel
                    .w((viewport.width - px(32.)).max(px(0.)).min(px(480.)))
                    .max_h((viewport.height - px(32.)).max(px(0.)))
            })
            .when(
                !matches!(
                    page,
                    Page::Keybinds
                        | Page::Themes
                        | Page::Palette
                        | Page::Preferences
                        | Page::AppUpdate
                        | Page::GitHub
                ),
                |panel| panel.overflow_y_scroll().p(px(6.)),
            )
            .when(
                matches!(
                    page,
                    Page::Keybinds | Page::Themes | Page::Palette | Page::Preferences
                ),
                |panel| {
                    panel
                        .flex()
                        .flex_col()
                        .h(px(560. * (font.size / 12.))
                            .min((viewport.height - px(32.)).max(px(0.))))
                        .overflow_hidden()
                        .shadow_lg()
                },
            )
            .when(page == Page::GitHub, |panel| {
                panel
                    .w((viewport.width - px(32.)).max(px(0.)).min(px(400.)))
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .shadow_lg()
            })
            .when(page == Page::Install, |panel| {
                panel
                    .w((viewport.width - px(24.)).max(px(0.)).min(px(420.)))
                    .max_h((viewport.height - px(24.)).max(px(0.)))
            })
            .when(page == Page::AppUpdate, |panel| {
                panel.flex().flex_col().overflow_hidden().shadow_lg()
            })
            .when(page == Page::About, |panel| {
                panel.w((viewport.width - px(24.)).max(px(0.)).min(px(340.)))
            })
            .rounded(px(5.))
            .border_1()
            .border_color(rgb(theme.active))
            .bg(rgb(theme.surface))
            .text_color(rgb(theme.foreground))
            .font_family(font.family.clone())
            .text_size(px(font.size))
            .line_height(px(font.line_height()))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| cx.stop_propagation());
        if page == Page::Menu {
            for (index, item) in self.menu_items().into_iter().enumerate() {
                panel = panel.child(
                    div()
                        .id(item)
                        .debug_selector(move || format!("menu-{item}"))
                        .min_h(px(font.line_height() + 12.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .when(Some(index) == self.menu.selected, |row| {
                            row.bg(rgb(theme.active))
                        })
                        .on_hover(cx.listener(move |this, hovered, _, cx| {
                            if *hovered {
                                this.menu.selected = Some(index);
                            } else if this.menu.selected == Some(index) {
                                this.menu.selected = None;
                            }
                            cx.notify();
                        }))
                        .child(item)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.activate_menu(item, window, cx);
                        })),
                );
            }
        } else if page == Page::GitHub {
            panel = panel.child(self.render_github_auth(cx));
        } else if page == Page::Workspace {
            for (action, label) in self.workspace_items() {
                panel = panel.child(
                    div()
                        .id(label)
                        .debug_selector(move || format!("workspace-menu-{label}"))
                        .min_h(px(font.line_height() + 12.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .cursor_pointer()
                        .rounded(px(3.))
                        .when(Some(action) == self.menu.workspace_selected, |row| {
                            row.bg(rgb(theme.active))
                        })
                        .on_hover(cx.listener(move |this, hovered, _, cx| {
                            if *hovered {
                                this.menu.workspace_selected = Some(action);
                            } else if this.menu.workspace_selected == Some(action) {
                                this.menu.workspace_selected = None;
                            }
                            cx.notify();
                        }))
                        .when_some(action.icon(), |row, icon| {
                            row.child(
                                svg()
                                    .path(icon)
                                    .debug_selector(move || format!("workspace-menu-icon-{label}"))
                                    .size(px(14.))
                                    .flex_none()
                                    .text_color(rgb(theme.muted)),
                            )
                        })
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.activate_workspace_menu(action, cx);
                        })),
                );
            }
            if self.menu.github.connected() {
                panel = panel.child(self.render_workspace_pr(
                    (px(340.).min((viewport.width - px(24.)).max(px(0.))) - px(30.)).max(px(0.)),
                    cx,
                ));
            }
        } else if let Page::Dialog(action) = page {
            if let Some(target) = &self.menu.target {
                let (title, detail, submit) = match action {
                    WorkspaceAction::Rename => (
                        "Rename workspace",
                        "Edit the workspace label.".to_owned(),
                        "Rename",
                    ),
                    WorkspaceAction::Close => (
                        target.close_label(),
                        format!(
                            "Close {} workspace(s) and terminate their running terminals? Checkout files and branches are not deleted.",
                            target.close_members.len()
                        ),
                        target.close_label(),
                    ),
                    WorkspaceAction::NewWorktree => {
                        let creating = self.menu.creation.is_some();
                        (
                            "New worktree",
                            format!(
                                "Branch. Blank uses the daemon default. Base: HEAD. Repository trust is not granted.{}",
                                if creating {
                                    "\n\nWaiting for daemon. Dismissing does not cancel a queued operation."
                                } else {
                                    ""
                                }
                            ),
                            if creating { "Creating..." } else { "Create" },
                        )
                    }
                    WorkspaceAction::DeleteWorktree => {
                        let deletion = self.menu.deletion.as_ref();
                        let path = deletion
                            .and_then(|d| d.path.as_deref())
                            .unwrap_or("Waiting for daemon checkout lookup...");
                        let force = deletion.is_some_and(|d| d.force);
                        (
                            "Delete worktree checkout",
                            format!(
                                "Checkout: {path}\n\nDeletes checkout files and closes its workspace and terminals. Branches are preserved. The daemon does not check for unpushed commits. Detached commits may become unreachable.\n\n{}\n\n{}",
                                if force {
                                    "WARNING: Force deletion discards modified and untracked files, including submodule contents. Type FORCE DELETE to confirm."
                                } else {
                                    "Git may reject modified/untracked files or submodules. Ignored files are not protected. Type DELETE to confirm."
                                },
                                if deletion.is_some_and(|d| d.pending.is_some()) {
                                    "Waiting for daemon. Dismissing does not cancel a queued operation."
                                } else {
                                    ""
                                }
                            ),
                            if force {
                                "Force delete"
                            } else {
                                "Delete checkout"
                            },
                        )
                    }
                };
                panel = panel
                    .child(div().p(px(8.)).child(title))
                    .child(div().p(px(8.)).truncate().child(target.label.clone()))
                    .child(div().p(px(8.)).child(detail));
                if self.menu.input.is_some() {
                    panel = panel.child(self.render_dialog_input(cx));
                }
                if action == WorkspaceAction::NewWorktree {
                    panel = panel.child(
                        div()
                            .debug_selector(|| "dialog-checkout".into())
                            .p(px(8.))
                            .text_color(rgb(theme.muted))
                            .child(self.checkout_preview()),
                    );
                }
                if let Some(error) = &self.menu.error {
                    panel = panel.child(
                        div()
                            .debug_selector(|| "dialog-error".into())
                            .p(px(8.))
                            .text_color(rgb(theme.palette[1]))
                            .child(error.clone()),
                    );
                }
                panel = panel.child(
                    div()
                        .flex()
                        .gap(px(16.))
                        .p(px(8.))
                        .child(
                            div()
                                .id("dialog-cancel")
                                .cursor_pointer()
                                .child("Cancel (Escape)")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.dismiss_menu(window, cx);
                                })),
                        )
                        .child(
                            div()
                                .id("dialog-submit")
                                .debug_selector(|| "dialog-submit".into())
                                .cursor_pointer()
                                .when(
                                    matches!(
                                        action,
                                        WorkspaceAction::Close | WorkspaceAction::DeleteWorktree
                                    ),
                                    |button| button.text_color(rgb(theme.palette[1])),
                                )
                                .child(submit)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.submit_workspace_dialog(window, cx);
                                })),
                        ),
                );
            }
        } else if matches!(page, Page::Tab | Page::RenameTab) {
            panel = panel.child(self.render_tab_menu(cx));
        } else if page == Page::Keybinds {
            panel = panel.child(self.render_keybinds(cx));
        } else if page == Page::Themes {
            panel = panel.child(self.render_theme_picker(cx));
        } else if page == Page::Palette {
            panel = panel.child(self.render_palette(cx));
        } else if page == Page::ConfirmClose {
            panel = panel.child(self.render_close_confirmation(cx));
        } else if page == Page::Preferences {
            panel = panel.child(self.render_preferences(cx));
        } else if page == Page::AppUpdate {
            panel = panel.child(self.render_app_update(window, cx));
        } else if page == Page::About {
            panel = panel.child(self.render_about(cx));
        } else if page == Page::Install {
            panel = panel
                .child(div().p(px(8.)).child("Herdr must be installed"))
                .child(div().p(px(8.)).child(
                    "Install Herdr first, then choose Terminal > Reconnect. The Install button opens the Herdr website; nothing is installed automatically.",
                ))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(8.))
                        .p(px(8.))
                        .child(
                            div()
                                .id("menu-install")
                                .debug_selector(|| "menu-install".into())
                                .p(px(8.))
                                .rounded(px(3.))
                                .bg(rgb(theme.active))
                                .cursor_pointer()
                                .child("Install")
                                .on_click(|_, _, cx| {
                                    cx.stop_propagation();
                                    cx.open_url(crate::about::WEBSITE);
                                }),
                        )
                        .child(
                            div()
                                .id("menu-dismiss")
                                .debug_selector(|| "menu-dismiss".into())
                                .p(px(8.))
                                .rounded(px(3.))
                                .hover(|button| button.bg(rgb(theme.active)))
                                .cursor_pointer()
                                .child("Dismiss")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.dismiss_menu(window, cx);
                                })),
                        ),
                );
        } else {
            let (title, rows) = {
                let snapshot = self.live.snapshot.as_ref();
                (
                    "Update ready",
                    vec![
                        format!(
                            "Version: {}",
                            snapshot
                                .and_then(|s| s.update_available.as_deref())
                                .unwrap_or("unavailable")
                        ),
                        "Suggested command (review and run yourself):".into(),
                        snapshot
                            .map(|s| s.update_install_command.clone())
                            .filter(|s| !s.trim().is_empty())
                            .unwrap_or("No install command provided by daemon.".into()),
                        "Nothing is installed or executed by this panel.".into(),
                    ],
                )
            };
            panel = panel.child(div().p(px(8.)).child(title));
            for text in rows {
                panel = panel.child(div().p(px(8.)).child(text));
            }
            panel = panel.child(
                div()
                    .id("menu-close")
                    .p(px(8.))
                    .cursor_pointer()
                    .child("close (Escape)")
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.dismiss_menu(window, cx);
                    })),
            );
        }
        div()
            .id("menu-overlay")
            .absolute()
            .inset_0()
            .when(page != Page::Menu && !pointer_anchored, |overlay| {
                overlay
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgba((theme.background << 8) | 0xb0))
            })
            .occlude()
            .track_focus(&self.menu.focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.dismiss_menu(window, cx);
                }),
            )
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.dismiss_menu(window, cx);
                }),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if let Some(input) = this.menu.input.as_mut() {
                    if input.key(&event.keystroke, cx) {
                        cx.stop_propagation();
                        window.prevent_default();
                        cx.notify();
                        return;
                    }
                    // Let the platform deliver printable text and IME navigation/commit.
                    if input.marked.is_some()
                        || !matches!(event.keystroke.key.as_str(), "escape" | "enter")
                    {
                        return;
                    }
                }
                if matches!(this.menu.page, Some(Page::Tab | Page::RenameTab)) {
                    this.tab_menu_key(event, window, cx);
                    return;
                }
                if this.menu.page == Some(Page::Palette) {
                    this.palette_key(event, window, cx);
                    return;
                }
                if this.menu.page == Some(Page::ConfirmClose) {
                    this.close_confirmation_key(event, window, cx);
                    return;
                }
                if this.menu.page == Some(Page::GitHub) {
                    this.github_key(event, window, cx);
                    return;
                }
                if this.menu.page == Some(Page::Themes) {
                    this.theme_picker_key(event, window, cx);
                    return;
                }
                if this.menu.page == Some(Page::Keybinds)
                    && (this
                        .menu
                        .keybinds_search
                        .as_ref()
                        .is_some_and(|search| search.read(cx).is_composing())
                        || !matches!(
                            event.keystroke.key.as_str(),
                            "escape" | "up" | "down" | "pageup" | "pagedown"
                        ))
                {
                    // Printable input and IME commands must reach the native text handler.
                    return;
                }
                cx.stop_propagation();
                window.prevent_default();
                match event.keystroke.key.as_str() {
                    "escape" => this.dismiss_menu(window, cx),
                    "enter" if matches!(this.menu.page, Some(Page::Dialog(_))) => {
                        this.submit_workspace_dialog(window, cx)
                    }
                    "up" | "down" if this.menu.page == Some(Page::Workspace) => {
                        let actions = this.workspace_menu_actions();
                        let selected = this.menu.workspace_selected.and_then(|selected| {
                            actions.iter().position(|action| *action == selected)
                        });
                        if !actions.is_empty() {
                            let index = match (selected, event.keystroke.key.as_str()) {
                                (None, "up") => actions.len() - 1,
                                (None, _) => 0,
                                (Some(index), "up") => (index + actions.len() - 1) % actions.len(),
                                (Some(index), _) => (index + 1) % actions.len(),
                            };
                            this.menu.workspace_selected = Some(actions[index]);
                        }
                        cx.notify();
                    }
                    "up" | "down" if this.menu.page == Some(Page::Menu) => {
                        let count = this.menu_items().len();
                        if count > 0 {
                            this.menu.selected =
                                Some(match (this.menu.selected, event.keystroke.key.as_str()) {
                                    (None, "up") => count - 1,
                                    (None, _) => 0,
                                    (Some(index), "up") => (index + count - 1) % count,
                                    (Some(index), _) => (index + 1) % count,
                                });
                        }
                        cx.notify();
                    }
                    "enter" if this.menu.page == Some(Page::Workspace) => {
                        if let Some(action) = this
                            .menu
                            .workspace_selected
                            .filter(|action| this.workspace_menu_actions().contains(action))
                        {
                            this.activate_workspace_menu(action, cx);
                        }
                    }
                    "up" | "down" | "pageup" | "pagedown"
                        if matches!(this.menu.page, Some(Page::Keybinds | Page::Preferences)) =>
                    {
                        let scroll = if this.menu.page == Some(Page::Preferences) {
                            &this.menu.preferences_scroll
                        } else {
                            &this.menu.keybinds_scroll
                        };
                        let key = event.keystroke.key.as_str();
                        let distance = if key.starts_with("page") {
                            scroll.bounds().size.height * 0.8
                        } else {
                            px(this.config.ui.line_height() * 3.)
                        };
                        let direction = if key.ends_with("up") { 1. } else { -1. };
                        scroll.set_offset(scroll.offset() + point(px(0.), distance * direction));
                        cx.notify();
                    }
                    "enter" if this.menu.page == Some(Page::Install) => {
                        cx.open_url(crate::about::WEBSITE);
                    }
                    "enter" if this.menu.page == Some(Page::About) => {
                        this.dismiss_menu(window, cx);
                    }
                    "enter" if this.menu.page == Some(Page::Menu) => {
                        if let Some(item) = this
                            .menu
                            .selected
                            .and_then(|index| this.menu_items().get(index).copied())
                        {
                            this.activate_menu(item, window, cx);
                        }
                    }
                    _ => {}
                }
            }))
            .child(if pointer_anchored {
                anchored()
                    .position(self.menu.anchor)
                    .snap_to_window_with_margin(Edges::all(px(12.)))
                    .child(panel)
                    .into_any_element()
            } else {
                panel.into_any_element()
            })
    }
}

#[cfg(test)]
pub(crate) mod workspace_tests {
    #![allow(clippy::unwrap_used)]
    use super::{WorkspaceAction, WorkspaceTarget};
    use crate::sidebar;

    pub(crate) fn submit_focus_change(
        view: &mut super::HerdrWindow,
        method: &str,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<super::HerdrWindow>,
    ) {
        let action = match method {
            "workspace.close" => WorkspaceAction::Close,
            "worktree.create" => WorkspaceAction::NewWorktree,
            "worktree.remove" => WorkspaceAction::DeleteWorktree,
            _ => panic!("unexpected fixture action"),
        };
        let snapshot = std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap());
        snapshot.workspaces = sidebar::layout_tests::snapshot(7).workspaces;
        let index = if action == WorkspaceAction::DeleteWorktree {
            4
        } else {
            3
        };
        let target = WorkspaceTarget::new(snapshot, &snapshot.workspaces[index]);
        view.open_menu(window, cx);
        view.menu.target = Some(target);
        view.menu.page = Some(super::Page::Dialog(action));
        if action == WorkspaceAction::DeleteWorktree {
            view.menu.input = Some(super::DialogInput::new("DELETE".into()));
            view.menu.deletion = Some(super::Deletion {
                pending: None,
                path: Some("/fixture/checkout".into()),
                force: false,
            });
        }
        view.submit_workspace_dialog(window, cx);
        assert!(view.menu.error.is_none());
        // Both worktree operations wait for their own correlated response.
        let pending = match action {
            WorkspaceAction::DeleteWorktree => view.menu.deletion.as_ref().unwrap().pending.clone(),
            WorkspaceAction::NewWorktree => view.menu.creation.clone(),
            _ => None,
        };
        if pending.is_some() {
            let inbox = view.endpoints[view.selected_endpoint]
                .connection
                .inbox
                .lock()
                .unwrap();
            assert_eq!(
                inbox.dialog_response.as_ref().map(|(id, _)| id),
                pending.as_ref()
            );
        }
        view.dismiss_menu(window, cx);
    }

    #[gpui::test]
    fn signed_out_workspace_has_no_github_section_or_requests(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(sidebar::layout_tests::fixture_window);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.live.status = crate::state::ConnectionStatus::Connected;
                view.open_workspace_menu("w3", Default::default(), window, cx);
                view.refresh_workspace_pr();
                assert!(!view.menu.pr.loading);
                assert!(view.menu.pr.message.is_none());
                assert!(!view.menu.github.busy());
                assert!(!view.menu.github.loading_profile());
                cx.notify();
            })
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("workspace-pr").is_none());
    }

    #[gpui::test]
    fn workspace_dialogs_and_prs_are_fenced_by_host_and_generation(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(sidebar::layout_tests::fixture_window);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.menu.github = crate::github::Auth::connected_fixture();
                view.live.status = crate::state::ConnectionStatus::Connected;
                view.endpoints[0].live = view.live.clone();
                view.endpoints[0].live.supports_surface = true;
                let mut remote = crate::endpoint::Endpoint::new(
                    "ssh:fixture".into(),
                    "Remote".into(),
                    herdr_client::ConnectTarget::Ssh {
                        target: "unused".into(),
                        session: "default".into(),
                    },
                    true,
                );
                // Identical boot/workspace IDs must not make different hosts interchangeable.
                remote.live = view.live.clone();
                remote.live.local_daemon_peer = true;
                view.endpoints.push(remote);
                view.open_workspace_menu("w3", Default::default(), window, cx);
                view.open_workspace_dialog(WorkspaceAction::Rename, cx);
                view.endpoints[0].generation += 1;
                view.submit_workspace_dialog(window, cx);
                assert_eq!(
                    view.menu.error.as_deref(),
                    Some("The selected connection changed or is not ready. Cancel and try again.")
                );
                assert!(view.select_endpoint("ssh:fixture", cx));
                assert!(view.menu.page.is_none());
                assert!(view.menu.input.is_none());
                view.open_workspace_menu("w3", Default::default(), window, cx);
                assert!(
                    view.menu
                        .pr
                        .message
                        .as_deref()
                        .unwrap()
                        .contains("requires your owned local session socket")
                );
                view.open_workspace_dialog(WorkspaceAction::DeleteWorktree, cx);
                assert!(view.select_endpoint(crate::endpoint::LOCAL, cx));
                assert!(view.menu.deletion.is_none());
            });
        });
    }

    pub(crate) fn check_pr_fences(
        view: &gpui::Entity<super::HerdrWindow>,
        cx: &mut gpui::VisualTestContext,
    ) {
        use std::sync::{Arc, Mutex};
        cx.update(|_, cx| {
            view.update(cx, |view, _| {
                view.menu.github = crate::github::Auth::connected_fixture();
                let snapshot = view.live.snapshot.clone();
                let inbox = view.endpoints[view.selected_endpoint]
                    .connection
                    .inbox
                    .clone();
                let status = view.live.status;
                let target_id = view.menu.target.as_ref().unwrap().id.clone();
                for change in 0..5 {
                    view.live.snapshot = snapshot.clone();
                    view.live.status = status;
                    view.endpoints[view.selected_endpoint].connection.inbox = inbox.clone();
                    view.menu.pr_connection = Some(Arc::downgrade(&inbox));
                    view.menu.pr.value = Some(crate::pull_request::fixture().unwrap());
                    view.menu.pr.message = None;
                    let snapshot = Arc::make_mut(view.live.snapshot.as_mut().unwrap());
                    match change {
                        0 => snapshot.boot_id = "restarted".into(),
                        1 => snapshot
                            .workspaces
                            .retain(|workspace| workspace.workspace_id != target_id),
                        2 => {
                            snapshot
                                .workspaces
                                .iter_mut()
                                .find(|workspace| workspace.workspace_id == target_id)
                                .unwrap()
                                .branch = Some("other".into())
                        }
                        3 => {
                            view.endpoints[view.selected_endpoint].connection.inbox =
                                Arc::new(Mutex::new(Default::default()))
                        }
                        _ => view.live.status = crate::state::ConnectionStatus::Detached,
                    }
                    assert!(view.update_workspace_pr());
                    assert!(view.menu.pr.value.is_none());
                }
                view.live.snapshot = snapshot;
                view.live.status = status;
                view.endpoints[view.selected_endpoint].connection.inbox = inbox;
                view.menu.pr_connection = None;
                view.menu.pr.clear();
                let connection_target = view.endpoints[view.selected_endpoint]
                    .connection
                    .target
                    .clone();
                let local_peer = view.live.local_daemon_peer;
                let supports_workspace_get = view.live.supports_workspace_get;
                view.live.supports_workspace_get = true;
                for target in [
                    herdr_client::ConnectTarget::Local,
                    herdr_client::ConnectTarget::Socket("/local-or-forwarded.sock".into()),
                ] {
                    view.endpoints[view.selected_endpoint].connection.target = target;
                    view.live.local_daemon_peer = false;
                    view.refresh_workspace_pr();
                    assert!(
                        view.menu
                            .pr
                            .message
                            .as_deref()
                            .unwrap()
                            .contains("requires your owned local session socket")
                    );
                    view.live.local_daemon_peer = true;
                    view.refresh_workspace_pr();
                    // Menu open is cache-only, even on a newer daemon.
                    assert!(view.menu.pr.loading);
                    assert!(view.menu.pr.message.is_none());
                }
                view.live.supports_workspace_get = false;
                view.refresh_workspace_pr();
                assert!(
                    view.menu.pr.loading,
                    "older local daemon uses Git registry worker"
                );
                assert!(
                    view.live.dialog_response.is_none(),
                    "no workspace.get request or dialog slot registration"
                );
                assert!(
                    view.menu.pr_connection.is_some(),
                    "fallback keeps reconnect fence"
                );
                view.endpoints[view.selected_endpoint].connection.target = connection_target;
                view.live.local_daemon_peer = local_peer;
                view.live.supports_workspace_get = supports_workspace_get;
                view.menu.pr.clear();
            })
        });
    }

    pub(crate) fn check_menu_interactions(
        view: &gpui::Entity<super::HerdrWindow>,
        cx: &mut gpui::VisualTestContext,
    ) {
        use gpui::{Modifiers, point, px};
        let selection = |view: &super::HerdrWindow| {
            if view.menu.page == Some(super::Page::Workspace) {
                view.menu.workspace_selected.and_then(|selected| {
                    view.workspace_menu_actions()
                        .iter()
                        .position(|action| *action == selected)
                })
            } else {
                view.menu.selected
            }
        };
        let (page, count, first, second) = cx.update(|_, cx| {
            let view = view.read(cx);
            assert_eq!(view.menu.selected, None);
            if view.menu.page == Some(super::Page::Workspace) {
                (
                    super::Page::Workspace,
                    view.workspace_items().len(),
                    "workspace-menu-Rename",
                    "workspace-menu-Close group",
                )
            } else {
                (
                    super::Page::Menu,
                    view.menu_items().len(),
                    "menu-settings",
                    "menu-keybinds",
                )
            }
        });
        cx.simulate_keystrokes("enter");
        cx.update(|_, cx| {
            assert!(view.read(cx).menu.page == Some(page));
            assert_eq!(view.read(cx).menu.selected, None);
            assert!(view.read(cx).menu.input.is_none());
        });
        let first = cx.debug_bounds(first).unwrap().center();
        let second = cx.debug_bounds(second).unwrap().center();
        let outside = point(px(790.), px(590.));
        for (position, selected) in [(second, Some(1)), (first, Some(0)), (outside, None)] {
            cx.simulate_mouse_move(position, None, Modifiers::default());
            cx.update(|window, cx| {
                window.draw(cx).clear();
                assert_eq!(selection(view.read(cx)), selected);
            });
        }
        // Leaving the hovered row also leaves Enter inert.
        cx.simulate_keystrokes("enter");
        cx.update(|_, cx| assert!(view.read(cx).menu.page == Some(page)));
        for (keys, selected) in [
            ("up", count - 1),
            ("down", 0),
            ("up", count - 1),
            ("down down", 1),
        ] {
            cx.simulate_keystrokes(keys);
            cx.update(|window, cx| {
                window.draw(cx).clear();
                assert_eq!(selection(view.read(cx)), Some(selected));
            });
        }
        cx.simulate_mouse_move(first, None, Modifiers::default());
        cx.update(|window, cx| {
            window.draw(cx).clear();
            assert_eq!(selection(view.read(cx)), Some(0));
        });
        // Keyboard selection replaces hover even while the pointer stays on the first row.
        cx.simulate_keystrokes("down");
        cx.update(|window, cx| {
            window.draw(cx).clear();
            assert_eq!(selection(view.read(cx)), Some(1));
        });
        cx.simulate_mouse_move(second, None, Modifiers::default());
        cx.update(|window, cx| {
            window.draw(cx).clear();
            assert_eq!(selection(view.read(cx)), Some(1));
        });
        cx.simulate_mouse_move(outside, None, Modifiers::default());
        cx.update(|window, cx| {
            window.draw(cx).clear();
            assert_eq!(selection(view.read(cx)), None);
        });
        cx.simulate_keystrokes("down");
        cx.update(|_, cx| assert_eq!(selection(view.read(cx)), Some(0)));
        cx.simulate_mouse_move(second, None, Modifiers::default());
        cx.update(|window, cx| {
            window.draw(cx).clear();
            assert_eq!(selection(view.read(cx)), Some(1));
        });
        cx.simulate_keystrokes("enter");
        cx.update(|_, cx| {
            assert!(
                view.read(cx).menu.page
                    == Some(if page == super::Page::Workspace {
                        super::Page::Dialog(WorkspaceAction::Close)
                    } else {
                        super::Page::Keybinds
                    })
            );
        });
        cx.simulate_mouse_move(outside, None, Modifiers::default());
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                let anchor = view.menu.anchor;
                let target = view.menu.target.as_ref().map(|target| target.id.clone());
                view.dismiss_menu(window, cx);
                if let Some(target) = target {
                    view.open_workspace_menu(&target, anchor, window, cx);
                } else {
                    view.open_menu(window, cx);
                }
                assert_eq!(view.menu.selected, None);
            });
            window.draw(cx).clear();
        });
    }

    /// Opening the dialog prepares the same branch and checkout the terminal
    /// client proposes, rather than an empty field.
    #[gpui::test]
    fn new_worktree_dialog_proposes_a_branch_and_previews_its_checkout(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(sidebar::layout_tests::fixture_window);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                let snapshot = std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap());
                snapshot.workspaces = sidebar::layout_tests::snapshot(7).workspaces;
                snapshot.worktree_directory = "/endpoint/.herdr/worktrees".into();
                view.live.status = crate::state::ConnectionStatus::Connected;
                view.open_workspace_menu("w3", Default::default(), window, cx);
                view.open_workspace_dialog(WorkspaceAction::NewWorktree, cx);
                let branch = view.menu.input.as_ref().unwrap().text.clone();
                assert!(branch.starts_with("worktree/"), "{branch}");
                // Selected, so the first keystroke replaces the proposal.
                assert_eq!(view.menu.input.as_ref().unwrap().selection, 0..branch.len());
                assert_eq!(
                    view.checkout_preview(),
                    format!(
                        "Checkout: /endpoint/.herdr/worktrees/agent-launcher/{}",
                        crate::worktree::branch_to_path_slug(&branch)
                    )
                );
                // A blank field defers to the daemon instead of guessing a path.
                view.menu.input = Some(super::DialogInput::new("  ".into()));
                assert_eq!(view.checkout_preview(), "Checkout: named by the daemon");
                view.menu.input = Some(super::DialogInput::new("feature/Login v2".into()));
                assert_eq!(
                    view.checkout_preview(),
                    "Checkout: /endpoint/.herdr/worktrees/agent-launcher/feature-login-v2"
                );
                // Without a reported worktree directory no path is invented.
                std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap())
                    .worktree_directory
                    .clear();
                assert_eq!(view.checkout_preview(), "Checkout: chosen by the daemon");
            });
        });
    }

    /// The daemon switches only its own session, so the client follows the
    /// created checkout itself; failures stay visible in the open dialog.
    #[gpui::test]
    fn worktree_creation_reports_failures_and_follows_the_created_checkout(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(sidebar::layout_tests::fixture_window);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                let snapshot = std::sync::Arc::make_mut(view.live.snapshot.as_mut().unwrap());
                snapshot.workspaces = sidebar::layout_tests::snapshot(7).workspaces;
                view.live.status = crate::state::ConnectionStatus::Connected;
                view.open_workspace_menu("w3", Default::default(), window, cx);
                view.open_workspace_dialog(WorkspaceAction::NewWorktree, cx);
                view.menu.creation = Some("create".into());
                let dialog = Some(super::Page::Dialog(WorkspaceAction::NewWorktree));

                view.apply_creation_response(
                    Ok(serde_json::json!({"error":{"code":"worktree_create_failed","message":"branch already checked out"}})),
                    window,
                    cx,
                );
                assert!(
                    view.menu
                        .error
                        .as_deref()
                        .unwrap()
                        .contains("branch already checked out")
                );
                assert_eq!(view.menu.page, dialog);
                assert!(view.pending_navigation.is_none());

                view.apply_creation_response(
                    Ok(serde_json::json!({"result":{"type":"worktree_list","worktrees":[]}})),
                    window,
                    cx,
                );
                assert!(
                    view.menu
                        .error
                        .as_deref()
                        .unwrap()
                        .starts_with("Unexpected daemon response")
                );
                assert_eq!(view.menu.page, dialog);

                view.apply_creation_response(
                    Err(std::sync::Arc::new(crate::Error::Client(
                        herdr_client::Error::Disconnected,
                    ))),
                    window,
                    cx,
                );
                assert_eq!(view.menu.page, dialog);
                assert!(view.pending_navigation.is_none());

                // Only the correlated response closes the dialog and navigates.
                let created = serde_json::json!({"result":{"type":"worktree_created","workspace":{"workspace_id":"w6"},"tab":{"tab_id":"t9"}}});
                view.collapsed_repos
                    .insert("/fixture/agent-launcher/.git".to_owned());
                view.menu.creation = Some("create".into());
                view.live.dialog_response = Some(("unrelated".into(), Some(Ok(created.clone()))));
                view.update_workspace_dialog(window, cx);
                assert_eq!(view.menu.page, dialog);
                assert_eq!(view.menu.creation.as_deref(), Some("create"));

                view.live.dialog_response = Some(("create".into(), Some(Ok(created))));
                view.update_workspace_dialog(window, cx);
                assert!(view.menu.page.is_none());
                assert!(view.menu.creation.is_none());
                assert_eq!(
                    view.pending_navigation,
                    Some(crate::NavigationTarget::Workspace("w6".into()))
                );
                // A folded group cannot hide the checkout that was just created.
                assert!(view.collapsed_repos.is_empty());
            });
        });
    }

    #[test]
    fn deletion_schema_and_target_validation() {
        let mut snapshot = sidebar::layout_tests::snapshot(7);
        let target = WorkspaceTarget::new(&snapshot, &snapshot.workspaces[4]);
        assert_eq!(
            target
                .request(&snapshot, WorkspaceAction::DeleteWorktree, "")
                .unwrap(),
            (
                "worktree.remove",
                serde_json::json!({"workspace_id":"w4", "force":false, "trust_repository":false})
            )
        );
        for index in [0, 3] {
            assert!(
                WorkspaceTarget::new(&snapshot, &snapshot.workspaces[index])
                    .request(&snapshot, WorkspaceAction::DeleteWorktree, "")
                    .is_err()
            );
        }
        snapshot.workspaces[4].worktree = None;
        assert!(
            target
                .request(&snapshot, WorkspaceAction::DeleteWorktree, "")
                .is_err()
        );
        snapshot.workspaces[4].worktree = target.worktree.clone();
        snapshot.boot_id = "replacement".into();
        assert!(
            target
                .request(&snapshot, WorkspaceAction::DeleteWorktree, "")
                .is_err()
        );
        snapshot.boot_id = target.boot_id.clone();
        snapshot.workspaces.remove(4);
        assert!(
            target
                .request(&snapshot, WorkspaceAction::DeleteWorktree, "")
                .is_err()
        );
    }

    #[gpui::test]
    fn deletion_lookup_dirty_force_errors_success_and_cancel(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let snapshot = sidebar::layout_tests::snapshot(7);
            let mut menu = super::MenuState::new(cx);
            menu.target = Some(WorkspaceTarget::new(&snapshot, &snapshot.workspaces[4]));
            menu.page = Some(super::Page::Dialog(WorkspaceAction::DeleteWorktree));
            menu.deletion = Some(super::Deletion { pending: Some("list".into()), path: None, force: false });
            let lookup = serde_json::json!({"result":{"type":"worktree_list", "worktrees":[{"open_workspace_id":"w4", "path":"/daemon/checkout", "is_linked_worktree":true, "is_bare":false}]}});
            menu.apply_deletion_response("unrelated", Ok(lookup.clone()));
            assert!(menu.deletion.as_ref().unwrap().path.is_none());
            menu.apply_deletion_response("list", Ok(lookup));
            let deletion = menu.deletion.as_mut().unwrap();
            assert_eq!(deletion.path.as_deref(), Some("/daemon/checkout"));
            for text in ["", "delete", " DELETE", "FORCE DELETE"] { assert!(!deletion.confirmed(text)); }
            assert!(deletion.confirmed("DELETE"));
            deletion.pending = Some("remove".into());
            assert!(!deletion.confirmed("DELETE"));
            menu.input = Some(super::DialogInput::new("DELETE".into()));
            menu.apply_deletion_response("remove", Ok(serde_json::json!({"error":{"code":"dirty_worktree_requires_force", "message":"modified or untracked files"}})));
            assert!(menu.error.as_ref().unwrap().contains("modified or untracked files"));
            assert_eq!(menu.input.as_ref().unwrap().text, "");
            let deletion = menu.deletion.as_mut().unwrap();
            assert!(deletion.force);
            assert!(!deletion.confirmed("DELETE"));
            assert!(deletion.confirmed("FORCE DELETE"));
            deletion.pending = Some("forced".into());
            menu.apply_deletion_response("forced", Err(std::sync::Arc::new(crate::Error::Client(herdr_client::Error::UnsupportedMethod))));
            assert_eq!(menu.error.as_deref(), Some("method not advertised by endpoint"));
            menu.deletion.as_mut().unwrap().pending = Some("success".into());
            menu.apply_deletion_response("success", Ok(serde_json::json!({"result":{"type":"worktree_removed", "workspace_id":"w4", "path":"/daemon/checkout", "forced":true}})));
            assert!(menu.page.is_none());
            menu.deletion = Some(super::Deletion { pending: Some("late".into()), path: None, force: false });
            menu.reset();
            menu.apply_deletion_response("late", Err(std::sync::Arc::new(crate::Error::Client(herdr_client::Error::Disconnected))));
            assert!(menu.deletion.is_none());
            assert!(menu.error.is_none());
        });
    }

    #[gpui::test]
    fn deletion_fails_closed_on_lookup_and_does_not_force_generic_errors(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            let snapshot = sidebar::layout_tests::snapshot(7);
            for response in [
                serde_json::json!({"result":{"type":"worktree_list", "worktrees":[]}}),
                serde_json::json!({"error":{"code":"worktree_remove_failed", "message":"is not a working tree"}}),
                serde_json::json!({"result":{"type":"unexpected"}}),
            ] {
                let mut menu = super::MenuState::new(cx);
                menu.target = Some(WorkspaceTarget::new(&snapshot, &snapshot.workspaces[4]));
                menu.deletion = Some(super::Deletion { pending: Some("id".into()), path: None, force: false });
                menu.apply_deletion_response("id", Ok(response));
                assert!(menu.error.is_some());
                let deletion = menu.deletion.as_ref().unwrap();
                assert!(!deletion.force);
                assert!(!deletion.confirmed("DELETE"));
            }
        });
    }

    #[gpui::test]
    fn reset_drops_target_draft_composition_and_error(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let snapshot = sidebar::layout_tests::snapshot(7);
            let mut menu = super::MenuState::new(cx);
            menu.target = Some(WorkspaceTarget::new(&snapshot, &snapshot.workspaces[3]));
            menu.page = Some(super::Page::Dialog(WorkspaceAction::Rename));
            let mut input = super::DialogInput::new("draft".into());
            input.replace(None, "composition", true, None);
            menu.input = Some(input);
            menu.error = Some("old connection error".into());
            menu.reset();
            assert!(menu.page.is_none());
            assert!(menu.target.is_none());
            assert!(menu.input.is_none());
            assert!(menu.error.is_none());
        });
    }

    #[test]
    fn actions_target_clicked_workspace_and_match_daemon_schemas() {
        let snapshot = sidebar::layout_tests::snapshot(7);
        let target = WorkspaceTarget::new(&snapshot, &snapshot.workspaces[3]);
        assert!(target.can_create());
        assert_eq!(target.close_label(), "Close group");
        assert_eq!(target.close_members, ["w3", "w4", "w5"]);
        assert_eq!(
            target
                .request(&snapshot, WorkspaceAction::Rename, "new label")
                .unwrap(),
            (
                "workspace.rename",
                serde_json::json!({"workspace_id": "w3", "label": "new label"})
            )
        );
        assert_eq!(
            target
                .request(&snapshot, WorkspaceAction::Close, "")
                .unwrap(),
            (
                "workspace.close",
                serde_json::json!({"workspace_id": "w3", "close_group": true})
            )
        );
        for branch in ["", "  ", " feature/test "] {
            let (method, params) = target
                .request(&snapshot, WorkspaceAction::NewWorktree, branch)
                .unwrap();
            assert_eq!(method, "worktree.create");
            let mut expected = serde_json::json!({"workspace_id": "w3", "base": "HEAD", "focus": true, "trust_repository": false});
            if !branch.trim().is_empty() {
                expected["branch"] = branch.trim().into();
            }
            assert_eq!(params, expected);
        }
        for index in [0, 4, 5] {
            let target = WorkspaceTarget::new(&snapshot, &snapshot.workspaces[index]);
            assert_eq!(target.close_label(), "Close");
            assert_eq!(target.close_members.len(), 1);
            assert!(!target.can_create());
            assert!(
                target
                    .request(&snapshot, WorkspaceAction::NewWorktree, "")
                    .is_err()
            );
        }
        let mut standalone = snapshot.clone();
        standalone
            .workspaces
            .retain(|w| w.workspace_id != "w4" && w.workspace_id != "w5");
        let target = WorkspaceTarget::new(&standalone, &standalone.workspaces[3]);
        assert!(target.can_create());
        assert_eq!(target.close_label(), "Close");
    }

    #[test]
    fn stale_boot_target_and_changed_close_members_are_rejected() {
        let mut snapshot = sidebar::layout_tests::snapshot(7);
        let target = WorkspaceTarget::new(&snapshot, &snapshot.workspaces[3]);
        snapshot.boot_id = "replacement".into();
        for action in [
            WorkspaceAction::Rename,
            WorkspaceAction::Close,
            WorkspaceAction::NewWorktree,
        ] {
            assert!(target.request(&snapshot, action, "valid label").is_err());
        }
        snapshot.boot_id = target.boot_id.clone();
        snapshot.workspaces.swap(0, 6);
        assert!(
            target
                .request(&snapshot, WorkspaceAction::Close, "")
                .is_ok()
        );
        snapshot.workspaces.retain(|w| w.workspace_id != "w4");
        assert!(
            target
                .request(&snapshot, WorkspaceAction::Close, "")
                .is_err()
        );
        assert!(
            target
                .request(&snapshot, WorkspaceAction::Rename, "valid label")
                .is_ok()
        );
        snapshot.workspaces.retain(|w| w.workspace_id != "w3");
        assert!(
            target
                .request(&snapshot, WorkspaceAction::Rename, "valid label")
                .is_err()
        );
    }

    #[test]
    fn rename_trims_unicode_whitespace_and_rejects_blank_labels() {
        let snapshot = sidebar::layout_tests::snapshot(7);
        let target = WorkspaceTarget::new(&snapshot, &snapshot.workspaces[3]);
        for text in ["", " \t\r\n", "\u{2003}\u{3000}"] {
            assert!(matches!(
                target.request(&snapshot, WorkspaceAction::Rename, text),
                Err(crate::Error::EmptyWorkspaceLabel)
            ));
        }
        assert_eq!(
            target
                .request(
                    &snapshot,
                    WorkspaceAction::Rename,
                    " \u{3000}new label\u{2003} "
                )
                .unwrap(),
            (
                "workspace.rename",
                serde_json::json!({"workspace_id": "w3", "label": "new label"})
            )
        );
    }
}

impl HerdrWindow {
    fn render_keybinds(&self, cx: &mut Context<Self>) -> Div {
        use crate::controls::{COMMANDS, Command};

        let theme = &self.theme;
        let font = &self.config.ui;
        let query = self
            .menu
            .keybinds_search
            .as_ref()
            .map(|search| search.read(cx).text())
            .unwrap_or("");
        let accent = accent(theme);
        let mut body = div()
            .id("keybinds-body")
            .debug_selector(|| "keybinds-body".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.menu.keybinds_scroll)
            .px(px(16.))
            .py(px(8.));
        let mut groups = [
            ("WORKSPACES & PANES", Vec::new()),
            ("NAVIGATION", Vec::new()),
            ("APPLICATION", vec![("cmd-v", "Paste into terminal")]),
        ];
        for info in COMMANDS.iter().filter(|info| !info.shortcut.is_empty()) {
            let group = match info.command {
                Command::Workspace
                | Command::Tab
                | Command::SplitRight
                | Command::SplitDown
                | Command::Zoom
                | Command::ClosePane
                | Command::CloseTab => 0,
                Command::NextTab
                | Command::PreviousTab
                | Command::FocusLeft
                | Command::FocusRight
                | Command::FocusUp
                | Command::FocusDown
                | Command::NextPane
                | Command::PreviousPane
                | Command::TabNumber(_)
                | Command::WorkspacePicker => 1,
                Command::ToggleSidebar
                | Command::Settings
                | Command::Keybinds
                | Command::Themes
                | Command::Palette
                | Command::Reconnect
                | Command::Quit
                | Command::Logs
                | Command::About => 2,
            };
            groups[group].1.push((info.shortcut, info.label));
        }
        let total: usize = groups.iter().map(|(_, shortcuts)| shortcuts.len()).sum();
        let mut count = 0;
        for (section, shortcuts) in groups {
            let shortcuts: Vec<_> = shortcuts
                .into_iter()
                .filter(|(keys, description)| shortcut_matches(query, keys, description, section))
                .collect();
            if shortcuts.is_empty() {
                continue;
            }
            count += shortcuts.len();
            body = body.child(
                div()
                    .pt(px(12.))
                    .pb(px(6.))
                    .text_size(px(font.size * 0.85))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(accent)
                    .child(section),
            );
            for (keys, description) in shortcuts {
                body = body.child(
                    div()
                        .debug_selector(|| format!("shortcut-{description}"))
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .py(px(7.))
                        .border_b_1()
                        .border_color(rgb(theme.active))
                        .child(
                            div()
                                .debug_selector(|| format!("keys-{description}"))
                                .w(relative(0.45))
                                .flex_none()
                                .flex()
                                .flex_wrap()
                                .gap(px(4.))
                                .children(keys.split('-').map(|key| {
                                    let mut chars = key.chars();
                                    let key: String = chars
                                        .next()
                                        .map(|first| first.to_ascii_uppercase())
                                        .into_iter()
                                        .chain(chars)
                                        .collect();
                                    div()
                                        .flex_none()
                                        .px(px(6.))
                                        .py(px(2.))
                                        .rounded(px(4.))
                                        .border_1()
                                        .border_color(rgb(theme.active))
                                        .bg(rgb(theme.background))
                                        .text_size(px(font.size * 0.9))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(key)
                                })),
                        )
                        .child(
                            div()
                                .debug_selector(|| format!("description-{description}"))
                                .flex_1()
                                .min_w_0()
                                .child(description),
                        ),
                );
            }
        }
        if count == 0 {
            body = body.child(
                div()
                    .debug_selector(|| "keybinds-empty".into())
                    .py(px(20.))
                    .text_color(rgb(theme.muted))
                    .child("No matching shortcuts. Try an action name or key combination."),
            );
        }
        body = body.child(
            div()
                .py(px(14.))
                .text_color(rgb(theme.muted))
                .child("Native GUI shortcuts only. Terminal applications and daemon/TUI keybindings keep their own shortcuts."),
        );
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .child(
                div()
                    .debug_selector(|| "keybinds-header".into())
                    .flex()
                    .items_center()
                    .flex_none()
                    .gap(px(12.))
                    .p(px(16.))
                    .border_b_1()
                    .border_color(rgb(theme.active))
                    .child(
                        div()
                            .w(px(3.))
                            .h(px(font.size * 2.5))
                            .rounded_full()
                            .bg(accent),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(font.size * 1.35))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Keyboard Shortcuts"),
                            )
                            .child(
                                div()
                                    .text_color(rgb(theme.muted))
                                    .child("Your Herdr quick reference"),
                            ),
                    )
                    .child(
                        div()
                            .id("menu-close")
                            .debug_selector(|| "keybinds-close".into())
                            .flex_none()
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .text_color(rgb(theme.muted))
                            .hover(|style| {
                                style
                                    .bg(rgb(theme.active))
                                    .text_color(rgb(theme.foreground))
                            })
                            .child("Close")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.dismiss_menu(window, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "keybinds-search-area".into())
                    .flex_none()
                    .px(px(16.))
                    .py(px(8.))
                    .when_some(self.menu.keybinds_search.clone(), |area, search| {
                        area.child(search)
                    })
                    .child(
                        div()
                            .debug_selector(|| "keybinds-count".into())
                            .pt(px(4.))
                            .text_color(rgb(theme.muted))
                            .child(format!("{count} of {total} shortcuts")),
                    ),
            )
            .child(body)
            .child(
                div()
                    .debug_selector(|| "keybinds-footer".into())
                    .flex_none()
                    .px(px(16.))
                    .py(px(10.))
                    .border_t_1()
                    .border_color(rgb(theme.active))
                    .text_color(rgb(theme.muted))
                    .child("Esc to close  /  click outside to dismiss"),
            )
    }
}

fn shortcut_matches(query: &str, keys: &str, description: &str, section: &str) -> bool {
    let query = query.to_lowercase().replace(['-', '+'], " ");
    if query
        .split_whitespace()
        .next()
        .is_some_and(|token| matches!(token, "cmd" | "ctrl" | "alt" | "shift"))
    {
        // A key combination should match keycaps, not letters in an action's name.
        return query
            .split_whitespace()
            .all(|token| keys.split('-').any(|key| key == token));
    }
    let text = format!("{keys} {description} {section}")
        .to_lowercase()
        .replace('-', " ");
    query.split_whitespace().all(|token| text.contains(token))
}

#[cfg(test)]
mod tests {
    #[gpui::test]
    #[allow(clippy::unwrap_used)]
    fn config_load_is_coherent_bounded_and_cancellable(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        view.update(cx, |view, cx| {
            view.load_gui_config_with(
                || {
                    let config = crate::config::Config {
                        theme: "Nord".into(),
                        ..Default::default()
                    };
                    let theme = config.theme()?;
                    Ok((config, theme))
                },
                cx,
            );
            view.load_gui_config_with(|| panic!("only one config load at a time"), cx);
            assert_eq!(view.config.theme, "Default");
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            assert_eq!(view.config.theme, "Nord");
            assert_eq!(view.theme, view.config.theme().unwrap());
            assert!(view.config_load.is_none());
            view.load_gui_config_with(|| Err(crate::Error::EmptyTheme), cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                assert_eq!(view.config.theme, "Nord");
                assert_eq!(view.theme, view.config.theme().unwrap());
                assert!(
                    view.local_error
                        .as_deref()
                        .unwrap()
                        .contains("theme must not be empty")
                );
                view.load_gui_config_with(|| Ok((Default::default(), Default::default())), cx);
                view.open_theme_picker(window, cx);
                assert!(view.config_load.is_none());
            });
        });
        cx.run_until_parked();
        view.update(cx, |view, _| assert_eq!(view.config.theme, "Nord"));
    }

    #[test]
    fn shortcut_search_matches_labels_keys_and_sections() {
        for query in ["", "pane close", "CMD+W", "cmd-w", "workspaces"] {
            assert!(super::shortcut_matches(
                query,
                "cmd-w",
                "Close Pane",
                "WORKSPACES & PANES"
            ));
        }
        assert!(!super::shortcut_matches(
            "zoom",
            "cmd-w",
            "Close Pane",
            "WORKSPACES & PANES"
        ));
        assert!(super::shortcut_matches(
            "cmd shift p",
            "cmd-shift-p",
            "Command Palette",
            "APPLICATION"
        ));
        assert!(!super::shortcut_matches(
            "cmd+p",
            "cmd-d",
            "Split Right",
            "WORKSPACES & PANES"
        ));
    }
}
