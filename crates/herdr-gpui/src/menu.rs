use super::HerdrWindow;
use super::dialog_input::DialogInput;
use crate::config::Config;
use gpui::{prelude::*, *};
use herdr_client::protocol::{ClientShellSnapshot, ClientShellWorkspace, ClientShellWorktree};

mod pr;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Page {
    Menu,
    Preferences,
    Keybinds,
    Themes,
    Palette,
    ConfirmClose,
    Update,
    Install,
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
    ) -> Result<(&'static str, serde_json::Value), &'static str> {
        let workspace = snapshot
            .workspaces
            .iter()
            .find(|w| w.workspace_id == self.id)
            .filter(|_| snapshot.boot_id == self.boot_id)
            .ok_or("Workspace is no longer available. Dismiss and reopen the menu.")?;
        let params = match action {
            WorkspaceAction::Rename => {
                let label = text.trim();
                if label.is_empty() {
                    return Err("Workspace label must not be empty.");
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
                    return Err("Workspace group changed. Dismiss and review the group again.");
                }
                (
                    "workspace.close",
                    serde_json::json!({"workspace_id": self.id, "close_group": true}),
                )
            }
            WorkspaceAction::NewWorktree => {
                if !self.can_create() || self.worktree != workspace.worktree {
                    return Err("Repository changed. Dismiss and reopen the menu.");
                }
                let mut params = serde_json::json!({"workspace_id": self.id, "base": "HEAD", "focus": true, "trust_repository": false});
                if !text.trim().is_empty() {
                    params["branch"] = text.trim().into();
                }
                ("worktree.create", params)
            }
            WorkspaceAction::DeleteWorktree => {
                if !self.can_delete() || self.worktree != workspace.worktree {
                    return Err("Checkout changed. Dismiss and reopen the menu.");
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
    pub anchor: Point<Pixels>,
    pub focus: FocusHandle,
    selected: Option<usize>,
    target: Option<WorkspaceTarget>,
    pub input: Option<DialogInput>,
    error: Option<String>,
    deletion: Option<Deletion>,
    keybinds_scroll: ScrollHandle,
    pub(super) keybinds_search: Option<Entity<crate::search_input::SearchInput>>,
    _keybinds_subscription: Option<Subscription>,
    pub(super) preferences_scroll: ScrollHandle,
    pub(super) themes: Option<crate::theme_picker::ThemePicker>,
    pub(super) palette: Option<crate::palette::Palette>,
    pub(super) close: Option<crate::close_modal::CloseConfirmation>,
    pub(super) pr: crate::pull_request::Lookup,
    github: crate::github::Auth,
    pr_pending: Option<String>,
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

impl MenuState {
    fn apply_deletion_response(&mut self, id: &str, result: Result<serde_json::Value, String>) {
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
                self.error = Some(error);
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
            anchor: Point::default(),
            focus: cx.focus_handle(),
            selected: None,
            target: None,
            input: None,
            error: None,
            deletion: None,
            keybinds_scroll: ScrollHandle::new(),
            keybinds_search: None,
            _keybinds_subscription: None,
            preferences_scroll: ScrollHandle::new(),
            themes: None,
            palette: None,
            close: None,
            pr: Default::default(),
            github: Default::default(),
            pr_pending: None,
            pr_connection: None,
        }
    }

    pub fn reset(&mut self) {
        if self.github.busy() {
            self.github.cancel();
        }
        self.page = None;
        self.selected = None;
        self.target = None;
        self.input = None;
        self.error = None;
        self.deletion = None;
        self.close = None;
        self.pr.clear();
        self.pr_pending = None;
        self.pr_connection = None;
    }
}

impl HerdrWindow {
    pub(super) fn open_keybinds(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_menu(window, cx);
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
        self.open_menu(window, cx);
        self.menu.page = Some(Page::Preferences);
        self.menu.preferences_scroll.set_offset(Point::default());
    }

    pub(super) fn reload_gui_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Load both before replacing either, so invalid themes preserve the UI.
        match Config::load().and_then(|config| {
            let theme = config.theme()?;
            Ok((config, theme))
        }) {
            Ok((config, theme)) => {
                self.config = config;
                self.theme = theme;
                self.wheel = Default::default();
                self.last_queued_options = None;
                self.local_error = None;
            }
            Err(error) => self.local_error = Some(format!("Reload GUI config: {error}")),
        }
        self.dismiss_menu(window, cx);
    }

    pub(super) fn show_install_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_menu(window, cx);
        self.menu.page = Some(Page::Install);
    }

    pub(super) fn open_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu.reset();
        self.menu.page = Some(Page::Menu);
        self.marked.clear();
        window.focus(&self.menu.focus);
        cx.notify();
    }

    pub(super) fn dismiss_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        self.menu.target = Some(WorkspaceTarget::new(snapshot, workspace));
        self.menu.anchor = anchor;
        self.menu.page = Some(Page::Workspace);
        self.refresh_workspace_pr();
        self.marked.clear();
        window.focus(&self.menu.focus);
        cx.notify();
    }

    fn workspace_items(&self) -> Vec<(WorkspaceAction, &'static str)> {
        let Some(target) = &self.menu.target else {
            return vec![];
        };
        let mut items = vec![
            (WorkspaceAction::Rename, "Rename"),
            (WorkspaceAction::Close, target.close_label()),
        ];
        if target.can_create() {
            items.push((WorkspaceAction::NewWorktree, "New worktree"));
        }
        if target.can_delete() {
            items.push((WorkspaceAction::DeleteWorktree, "Delete worktree checkout"));
        }
        items
    }

    fn open_workspace_dialog(&mut self, action: WorkspaceAction, cx: &mut Context<Self>) {
        let Some(target) = &self.menu.target else {
            return;
        };
        self.menu.input = match action {
            WorkspaceAction::Rename => Some(DialogInput::new(target.label.clone())),
            WorkspaceAction::NewWorktree => Some(DialogInput::default()),
            WorkspaceAction::Close => None,
            WorkspaceAction::DeleteWorktree => Some(DialogInput::default()),
        };
        self.menu.page = Some(Page::Dialog(action));
        self.menu.pr.clear();
        self.menu.pr_pending = None;
        self.menu.pr_connection = None;
        self.menu.error = None;
        if action == WorkspaceAction::DeleteWorktree {
            let result = self.connection.request_dialog(
                &target.boot_id,
                "worktree.list",
                serde_json::json!({"workspace_id": target.id, "trust_repository": false}),
            );
            self.menu.deletion = Some(Deletion {
                pending: result.as_ref().ok().cloned(),
                path: None,
                force: false,
            });
            self.menu.error = result.err();
        }
        cx.notify();
    }

    pub(super) fn update_deletion_dialog(&mut self) {
        if self.menu.deletion.is_none() {
            return;
        }
        if !self.live.status.is_connected()
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
        self.menu.apply_deletion_response(id, result.clone());
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
            if !self.live.status.is_connected() {
                return Err("Disconnected. Dismiss and reconnect before trying again.".to_owned());
            }
            let target = self
                .menu
                .target
                .as_ref()
                .ok_or("Workspace is no longer available.")?;
            let snapshot = self
                .live
                .snapshot
                .as_ref()
                .ok_or("Waiting for workspace state.")?;
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
                    .ok_or("Reopen the deletion dialog.")?;
                if deletion.pending.is_some() {
                    return Ok(());
                }
                if deletion.path.is_none() {
                    return Err("Checkout lookup failed. Dismiss and reopen the menu.".into());
                }
                let confirmation = if deletion.force {
                    "FORCE DELETE"
                } else {
                    "DELETE"
                };
                if !deletion.confirmed(text) {
                    return Err(format!("Type {confirmation} to confirm."));
                }
                params["force"] = deletion.force.into();
                let id = self
                    .connection
                    .request_dialog(&target.boot_id, method, params)?;
                if let Some(deletion) = &mut self.menu.deletion {
                    deletion.pending = Some(id);
                }
                self.menu.error = None;
                return Ok(());
            }
            let handle = self.connection.handle.as_ref().ok_or("Disconnected.")?;
            handle
                .request(&target.boot_id, method, params)
                .map(|_| ())
                .map_err(|error| format!("{method}: {error}"))
        })();
        match result {
            Ok(_) => {
                self.local_error = None;
                if action != WorkspaceAction::DeleteWorktree {
                    self.dismiss_menu(window, cx);
                } else {
                    cx.notify();
                }
            }
            Err(error) => {
                self.menu.error = Some(error);
                cx.notify();
            }
        }
    }

    fn menu_items(&self) -> Vec<&'static str> {
        let mut items = vec![
            "settings",
            "keybinds",
            "themes",
            "commands",
            "workspaces",
            "reload GUI config",
            "GitHub sign-in",
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
        items.push(if self.connection.handle.is_some() {
            "detach"
        } else {
            "reconnect"
        });
        items
    }

    fn activate_menu(&mut self, item: &str, window: &mut Window, cx: &mut Context<Self>) {
        match item {
            "GitHub sign-in" => self.menu.page = Some(Page::GitHub),
            "settings" => self.open_preferences(window, cx),
            "keybinds" => self.open_keybinds(window, cx),
            "themes" => self.open_theme_picker(window, cx),
            "commands" => self.open_palette(false, window, cx),
            "workspaces" => self.open_palette(true, window, cx),
            "update ready" => self.menu.page = Some(Page::Update),
            "reload GUI config" => self.reload_gui_config(window, cx),
            "reload daemon config" => {
                if let (Some(handle), Some(snapshot)) =
                    (&self.connection.handle, &self.live.snapshot)
                {
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
                self.connection.detach(self.active);
                self.live = self.connection.take_update().unwrap_or_default();
                self.local_error = None;
                self.marked.clear();
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
        let pointer_anchored = matches!(page, Page::Workspace | Page::Dialog(_));
        let font = &self.config.ui;
        let theme = &self.theme;
        let viewport = window.viewport_size();
        let mut panel = div()
            .id("menu-panel")
            .debug_selector(|| "menu-panel".into())
            .when(pointer_anchored, |panel| {
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
                panel
                    .absolute()
                    .left(px(56.))
                    .bottom((viewport.height - self.menu.anchor.y + px(12.)).max(px(30.)))
                    .w(px(180.))
                    .max_h((viewport.height / 2. - px(12.)).max(px(0.)))
            })
            .when(page != Page::Menu && !pointer_anchored, |panel| {
                panel
                    .w((viewport.width - px(32.)).max(px(0.)).min(px(480.)))
                    .max_h((viewport.height - px(32.)).max(px(0.)))
            })
            .when(
                !matches!(
                    page,
                    Page::Keybinds | Page::Themes | Page::Palette | Page::Preferences
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
            .when(page == Page::Install, |panel| {
                panel
                    .w((viewport.width - px(24.)).max(px(0.)).min(px(420.)))
                    .max_h((viewport.height - px(24.)).max(px(0.)))
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
            for (index, (action, label)) in self.workspace_items().into_iter().enumerate() {
                panel = panel.child(
                    div()
                        .id(label)
                        .debug_selector(move || format!("workspace-menu-{label}"))
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
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.open_workspace_dialog(action, cx);
                        })),
                );
            }
            panel = panel.child(self.render_workspace_pr(
                (px(340.).min((viewport.width - px(24.)).max(px(0.))) - px(30.)).max(px(0.)),
                cx,
            ));
        } else if let Page::Dialog(action) = page {
            if let Some(target) = &self.menu.target {
                let (title, detail, submit) = match action {
                    WorkspaceAction::Rename => ("Rename workspace", "Edit the workspace label.".to_owned(), "Rename"),
                    WorkspaceAction::Close => (target.close_label(), format!("Close {} workspace(s) and terminate their running terminals? Checkout files and branches are not deleted.", target.close_members.len()), target.close_label()),
                    WorkspaceAction::NewWorktree => ("New worktree", "Branch (optional). Blank uses the daemon default. Base: HEAD. Repository trust is not granted.".to_owned(), "Create"),
                    WorkspaceAction::DeleteWorktree => {
                        let deletion = self.menu.deletion.as_ref();
                        let path = deletion.and_then(|d| d.path.as_deref()).unwrap_or("Waiting for daemon checkout lookup...");
                        let force = deletion.is_some_and(|d| d.force);
                        ("Delete worktree checkout", format!("Checkout: {path}\n\nDeletes checkout files and closes its workspace and terminals. Branches are preserved. The daemon does not check for unpushed commits. Detached commits may become unreachable.\n\n{}\n\n{}", if force { "WARNING: Force deletion discards modified and untracked files, including submodule contents. Type FORCE DELETE to confirm." } else { "Git may reject modified/untracked files or submodules. Ignored files are not protected. Type DELETE to confirm." }, if deletion.is_some_and(|d| d.pending.is_some()) { "Waiting for daemon. Dismissing does not cancel a queued operation." } else { "" }), if force { "Force delete" } else { "Delete checkout" })
                    }
                };
                panel = panel
                    .child(div().p(px(8.)).child(title))
                    .child(div().p(px(8.)).truncate().child(target.label.clone()))
                    .child(div().p(px(8.)).child(detail));
                if self.menu.input.is_some() {
                    panel = panel.child(self.render_dialog_input(cx));
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
                                    cx.open_url("https://herdr.dev/");
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
                if this.menu.page == Some(Page::Palette) {
                    this.palette_key(event, window, cx);
                    return;
                }
                if this.menu.page == Some(Page::ConfirmClose) {
                    this.close_confirmation_key(event, window, cx);
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
                    key if this.menu.page == Some(Page::GitHub)
                        && event.keystroke.modifiers == Modifiers::default() =>
                    {
                        match key {
                            "s" => this.menu.github.start(),
                            "d" => {
                                this.menu.pr.clear();
                                this.menu.github.sign_out();
                            }
                            "c" => this.menu.github.cancel(),
                            "o" if this.menu.github.code().is_some() => {
                                cx.open_url(crate::github::VERIFY_URL)
                            }
                            _ => {}
                        }
                        cx.notify();
                    }
                    "r" if this.menu.page == Some(Page::Workspace)
                        && event.keystroke.modifiers == Modifiers::default() =>
                    {
                        this.refresh_workspace_pr();
                        cx.notify();
                    }
                    "o" if this.menu.page == Some(Page::Workspace)
                        && event.keystroke.modifiers == Modifiers::default() =>
                    {
                        this.open_workspace_pr(cx);
                    }
                    "enter" if matches!(this.menu.page, Some(Page::Dialog(_))) => {
                        this.submit_workspace_dialog(window, cx)
                    }
                    "up" | "down"
                        if matches!(this.menu.page, Some(Page::Workspace | Page::Menu)) =>
                    {
                        let count = if this.menu.page == Some(Page::Workspace) {
                            this.workspace_items().len()
                        } else {
                            this.menu_items().len()
                        };
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
                        if let Some((action, _)) = this
                            .menu
                            .selected
                            .and_then(|index| this.workspace_items().get(index).copied())
                        {
                            this.open_workspace_dialog(action, cx);
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
                        cx.open_url("https://herdr.dev/");
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

    pub(crate) fn check_pr_fences(
        view: &gpui::Entity<super::HerdrWindow>,
        cx: &mut gpui::VisualTestContext,
    ) {
        use std::sync::{Arc, Mutex};
        cx.update(|_, cx| {
            view.update(cx, |view, _| {
                let snapshot = view.live.snapshot.clone();
                let inbox = view.connection.inbox.clone();
                let status = view.live.status;
                let target_id = view.menu.target.as_ref().unwrap().id.clone();
                for change in 0..5 {
                    view.live.snapshot = snapshot.clone();
                    view.live.status = status;
                    view.connection.inbox = inbox.clone();
                    view.menu.pr_connection = Some(Arc::downgrade(&inbox));
                    view.menu.pr.value = Some(crate::pull_request::fixture().unwrap());
                    view.menu.pr.message = None;
                    view.menu.pr_pending = Some("old-request".into());
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
                        3 => view.connection.inbox = Arc::new(Mutex::new(Default::default())),
                        _ => view.live.status = crate::state::ConnectionStatus::Detached,
                    }
                    assert!(view.update_workspace_pr());
                    assert!(view.menu.pr.value.is_none());
                    assert!(view.menu.pr_pending.is_none());
                }
                view.live.snapshot = snapshot;
                view.live.status = status;
                view.connection.inbox = inbox;
                view.menu.pr_connection = None;
                view.menu.pr.clear();
                let connection_target = view.connection.target.clone();
                let local_peer = view.live.local_daemon_peer;
                for target in [
                    herdr_client::ConnectTarget::Local,
                    herdr_client::ConnectTarget::Socket("/local-or-forwarded.sock".into()),
                ] {
                    view.connection.target = target;
                    view.live.local_daemon_peer = false;
                    view.refresh_workspace_pr();
                    assert!(
                        view.menu
                            .pr
                            .message
                            .as_deref()
                            .unwrap()
                            .contains("could not be verified")
                    );
                    view.live.local_daemon_peer = true;
                    view.refresh_workspace_pr();
                    // The fixture has no client handle: reaching request_dialog
                    // proves the verified peer passed the guard in both modes.
                    assert_eq!(view.menu.pr.message.as_deref(), Some("Disconnected."));
                }
                view.connection.target = connection_target;
                view.live.local_daemon_peer = local_peer;
                view.menu.pr.clear();
            })
        });
    }

    pub(crate) fn check_menu_interactions(
        view: &gpui::Entity<super::HerdrWindow>,
        cx: &mut gpui::VisualTestContext,
    ) {
        use gpui::{Modifiers, point, px};
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
                assert_eq!(view.read(cx).menu.selected, selected);
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
                assert_eq!(view.read(cx).menu.selected, Some(selected));
            });
        }
        cx.simulate_mouse_move(first, None, Modifiers::default());
        cx.update(|window, cx| {
            window.draw(cx).clear();
            assert_eq!(view.read(cx).menu.selected, Some(0));
        });
        // Keyboard selection replaces hover even while the pointer stays on the first row.
        cx.simulate_keystrokes("down");
        cx.update(|window, cx| {
            window.draw(cx).clear();
            assert_eq!(view.read(cx).menu.selected, Some(1));
        });
        cx.simulate_mouse_move(second, None, Modifiers::default());
        cx.update(|window, cx| {
            window.draw(cx).clear();
            assert_eq!(view.read(cx).menu.selected, Some(1));
        });
        cx.simulate_mouse_move(outside, None, Modifiers::default());
        cx.update(|window, cx| {
            window.draw(cx).clear();
            assert_eq!(view.read(cx).menu.selected, None);
        });
        cx.simulate_keystrokes("down");
        cx.update(|_, cx| assert_eq!(view.read(cx).menu.selected, Some(0)));
        cx.simulate_mouse_move(second, None, Modifiers::default());
        cx.update(|window, cx| {
            window.draw(cx).clear();
            assert_eq!(view.read(cx).menu.selected, Some(1));
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
            menu.apply_deletion_response("forced", Err("unsupported method".into()));
            assert_eq!(menu.error.as_deref(), Some("unsupported method"));
            menu.deletion.as_mut().unwrap().pending = Some("success".into());
            menu.apply_deletion_response("success", Ok(serde_json::json!({"result":{"type":"worktree_removed", "workspace_id":"w4", "path":"/daemon/checkout", "forced":true}})));
            assert!(menu.page.is_none());
            menu.deletion = Some(super::Deletion { pending: Some("late".into()), path: None, force: false });
            menu.reset();
            menu.apply_deletion_response("late", Err("late error".into()));
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
            assert_eq!(
                target.request(&snapshot, WorkspaceAction::Rename, text),
                Err("Workspace label must not be empty.")
            );
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
        // Mix the theme's blue with foreground so accents remain readable on dark themes.
        let accent = rgb(theme.foreground).blend(rgba((theme.palette[4] << 8) | 0x70));
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
                | Command::Quit => 2,
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
