use super::dialog_input::DialogInput;
use super::{HerdrWindow, sidebar};
use gpui::{prelude::*, *};
use herdr_client::protocol::{ClientShellSnapshot, ClientShellWorkspace, ClientShellWorktree};

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Page {
    Menu,
    Preferences,
    Keybinds,
    Update,
    Workspace,
    Dialog(WorkspaceAction),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkspaceAction {
    Rename,
    Close,
    NewWorktree,
}

struct WorkspaceTarget {
    boot_id: String,
    id: String,
    label: String,
    worktree: Option<ClientShellWorktree>,
    close_members: Vec<String>,
}

impl WorkspaceTarget {
    fn new(snapshot: &ClientShellSnapshot, workspace: &ClientShellWorkspace) -> Self {
        Self {
            boot_id: snapshot.boot_id.clone(),
            id: workspace.workspace_id.clone(),
            label: workspace.label.clone(),
            worktree: workspace.worktree.clone(),
            close_members: close_members(snapshot, workspace),
        }
    }

    fn can_create(&self) -> bool {
        self.worktree
            .as_ref()
            .is_some_and(|tree| !tree.is_linked_worktree)
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
    selected: usize,
    target: Option<WorkspaceTarget>,
    pub input: Option<DialogInput>,
    error: Option<String>,
}

impl MenuState {
    pub fn new(cx: &App) -> Self {
        Self {
            page: None,
            anchor: Point::default(),
            focus: cx.focus_handle(),
            selected: 0,
            target: None,
            input: None,
            error: None,
        }
    }

    pub fn reset(&mut self) {
        self.page = None;
        self.target = None;
        self.input = None;
        self.error = None;
    }
}

impl HerdrWindow {
    pub(super) fn open_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu.reset();
        self.menu.page = Some(Page::Menu);
        self.menu.selected = 0;
        self.marked.clear();
        window.focus(&self.menu.focus);
        cx.notify();
    }

    fn dismiss_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        self.menu.selected = 0;
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
        };
        self.menu.page = Some(Page::Dialog(action));
        self.menu.error = None;
        cx.notify();
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
            let (method, params) = target.request(snapshot, action, text)?;
            let handle = self.connection.handle.as_ref().ok_or("Disconnected.")?;
            handle
                .request(&target.boot_id, method, params)
                .map_err(|error| format!("{method}: {error}"))
        })();
        match result {
            Ok(_) => {
                self.local_error = None;
                self.dismiss_menu(window, cx);
            }
            Err(error) => {
                self.menu.error = Some(error);
                cx.notify();
            }
        }
    }

    fn menu_items(&self) -> Vec<&'static str> {
        let mut items = vec!["settings", "keybinds"];
        if self.live.status.is_connected() {
            items.push("reload config");
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
            "settings" => self.menu.page = Some(Page::Preferences),
            "keybinds" => self.menu.page = Some(Page::Keybinds),
            "update ready" => self.menu.page = Some(Page::Update),
            "reload config" => {
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
        let mut panel = div()
            .id("menu-panel")
            .debug_selector(|| "menu-panel".into())
            .when(!pointer_anchored, |panel| {
                panel.absolute().left(px(56.)).bottom(
                    (window.viewport_size().height - self.menu.anchor.y + px(12.)).max(px(30.)),
                )
            })
            .w(px(if matches!(page, Page::Menu | Page::Workspace) {
                180.
            } else {
                420.
            })
            .min(window.viewport_size().width - px(24.)))
            .max_h(window.viewport_size().height / 2. - px(12.))
            .overflow_y_scroll()
            .p(px(6.))
            .rounded(px(5.))
            .border_1()
            .border_color(rgb(sidebar::ACTIVE))
            .bg(rgb(sidebar::BACKGROUND))
            .text_color(rgb(sidebar::FOREGROUND))
            .font_family("Menlo")
            .text_size(px(12.))
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
                        .h(px(28.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .when(index == self.menu.selected, |row| {
                            row.bg(rgb(sidebar::ACTIVE))
                        })
                        .hover(|row| row.bg(rgb(sidebar::ACTIVE)))
                        .child(item)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.activate_menu(item, window, cx);
                        })),
                );
            }
        } else if page == Page::Workspace {
            for (index, (action, label)) in self.workspace_items().into_iter().enumerate() {
                panel = panel.child(
                    div()
                        .id(label)
                        .debug_selector(move || format!("workspace-menu-{label}"))
                        .h(px(28.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .when(index == self.menu.selected, |row| {
                            row.bg(rgb(sidebar::ACTIVE))
                        })
                        .hover(|row| row.bg(rgb(sidebar::ACTIVE)))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.open_workspace_dialog(action, cx);
                        })),
                );
            }
        } else if let Page::Dialog(action) = page {
            if let Some(target) = &self.menu.target {
                let (title, detail, submit) = match action {
                    WorkspaceAction::Rename => ("Rename workspace", "Edit the workspace label.".to_owned(), "Rename"),
                    WorkspaceAction::Close => (target.close_label(), format!("Close {} workspace(s) and terminate their running terminals? Checkout files and branches are not deleted.", target.close_members.len()), target.close_label()),
                    WorkspaceAction::NewWorktree => ("New worktree", "Branch (optional). Blank uses the daemon default. Base: HEAD. Repository trust is not granted.".to_owned(), "Create"),
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
                            .text_color(rgb(0xf38ba8))
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
                                .when(action == WorkspaceAction::Close, |button| {
                                    button.text_color(rgb(0xf38ba8))
                                })
                                .child(submit)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.submit_workspace_dialog(window, cx);
                                })),
                        ),
                );
            }
        } else {
            let (title, rows) = match page {
                Page::Preferences => ("Preferences (read-only)", vec![
                    format!("Connection: {}", self.live.status),
                    format!("Target: {:?}", self.connection.target),
                    "Terminal font: Menlo, 14 px (default)".into(),
                    "Sidebar font: Menlo, 12 px (default)".into(),
                    "Daemon configuration editing is not exposed by this native client. No configuration path is assumed.".into(),
                    "Reload config asks the connected daemon to reload its own configuration.".into(),
                ]),
                Page::Keybinds => ("Native keybinds", vec![
                    "Cmd-N   New workspace".into(), "Cmd-T   New tab".into(),
                    "Cmd-D   Split right".into(), "Cmd-Shift-D   Split down".into(),
                    "Cmd-Shift-] / [   Next / previous tab".into(),
                    "Cmd-V   Paste into terminal".into(), "Cmd-Q   Quit GUI (daemon stays running)".into(),
                    "Menu: Up / Down, Enter; Escape or outside click to dismiss.".into(),
                    "Daemon/TUI custom keybindings are not native GUI shortcuts.".into(),
                ]),
                _ => {
                    let snapshot = self.live.snapshot.as_ref();
                    ("Update ready", vec![
                        format!("Version: {}", snapshot.and_then(|s| s.update_available.as_deref()).unwrap_or("unavailable")),
                        "Suggested command (review and run yourself):".into(),
                        snapshot.map(|s| s.update_install_command.clone()).filter(|s| !s.trim().is_empty()).unwrap_or("No install command provided by daemon.".into()),
                        "Nothing is installed or executed by this panel.".into(),
                    ])
                }
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
                cx.stop_propagation();
                window.prevent_default();
                match event.keystroke.key.as_str() {
                    "escape" => this.dismiss_menu(window, cx),
                    "enter" if matches!(this.menu.page, Some(Page::Dialog(_))) => {
                        this.submit_workspace_dialog(window, cx)
                    }
                    "up" | "down" if this.menu.page == Some(Page::Workspace) => {
                        let count = this.workspace_items().len();
                        if count > 0 {
                            this.menu.selected = (this.menu.selected
                                + if event.keystroke.key == "up" {
                                    count - 1
                                } else {
                                    1
                                })
                                % count;
                        }
                        cx.notify();
                    }
                    "enter" if this.menu.page == Some(Page::Workspace) => {
                        if let Some((action, _)) = this.workspace_items().get(this.menu.selected) {
                            this.open_workspace_dialog(*action, cx);
                        }
                    }
                    "up" | "down" if this.menu.page == Some(Page::Menu) => {
                        let count = this.menu_items().len();
                        this.menu.selected = (this.menu.selected
                            + if event.keystroke.key == "up" {
                                count - 1
                            } else {
                                1
                            })
                            % count;
                        cx.notify();
                    }
                    "enter" if this.menu.page == Some(Page::Menu) => {
                        if let Some(item) = this.menu_items().get(this.menu.selected) {
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
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::{WorkspaceAction, WorkspaceTarget, sidebar};

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
