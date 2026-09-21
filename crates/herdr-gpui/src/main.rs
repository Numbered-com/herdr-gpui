// objc 0.2's selectors expand a legacy cargo-clippy cfg in the native test adapter.
#![cfg_attr(feature = "integration-test", allow(unexpected_cfgs))]
mod about;
mod app_icon;
mod avatars;
mod cli;
mod close_modal;
mod config;
mod connection;
mod controls;
mod daemon;
mod diagnostics;
mod dialog_input;
mod endpoint;
mod error;
mod github;
pub use error::{Error, Result};
mod icons;
mod input;
mod log_window;
mod menu;
mod palette;
#[cfg(feature = "integration-test")]
mod performance;
mod preferences;
mod pull_request;
mod search_input;
mod sidebar;
#[cfg(feature = "integration-test")]
mod smoke;
mod state;
mod tab_menu;
mod terminal;
mod terminal_painter;
mod theme_picker;
mod titlebar;
mod update_panel;
mod updater;
mod worktree_banner;

use connection::ConnectionBridge;
use controls::Command;
use gpui::{prelude::*, *};
use herdr_client::{ConnectOptions, ConnectTarget, protocol::*};
use state::{ConnectionStatus, LiveState};
#[cfg(feature = "integration-test")]
use std::sync::Arc;
use std::time::Duration;
use terminal::*;

// Every window carries the product name; the focused space follows it.
const WINDOW_TITLE: &str = "Herdr";

// Even tab cells, as on herdr.dev, so short labels do not collapse to a sliver.
const TAB_WIDTH: f32 = 64.;
// The reference strip is a shallow band: chrome, not a toolbar.
const TAB_HEIGHT: f32 = 24.;

// Release builds embed the same calendar version (YYYYMMDD.COUNTER) used for the
// tag, the bundle, and the downloadable artifacts. Local builds are not releases,
// so they carry no version the updater or an issue report could act on.
const APP_VERSION: &str = match option_env!("HERDR_RELEASE_VERSION") {
    Some(version) => version,
    None => "dev",
};

// Only the signed release pipeline sets that tag. Local `just run` and worktree
// builds are unsigned, so features that depend on a stable code identity (such as
// macOS Keychain access) must treat them as development builds.
const RELEASE_BUILD: bool = option_env!("HERDR_RELEASE_VERSION").is_some();

actions!(
    herdr,
    [
        Quit,
        ShowHerdrNotDetected,
        ShowLogs,
        CheckForUpdates,
        ShowUpdatePreview
    ]
);

#[derive(Clone, PartialEq, serde::Deserialize, Action)]
#[action(no_json)]
struct RunCommand {
    command: Command,
}

fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
    cx.bind_keys(
        controls::COMMANDS
            .iter()
            .filter(|info| !info.shortcut.is_empty() && info.command != Command::Quit)
            .map(|info| {
                KeyBinding::new(
                    info.shortcut,
                    RunCommand {
                        command: info.command,
                    },
                    None,
                )
            }),
    );
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NavigationTarget<T> {
    Workspace(T),
    Tab(T),
    Pane(T),
}

type OwnedNavigationTarget = NavigationTarget<String>;

impl<T: AsRef<str>> NavigationTarget<T> {
    fn as_ref(&self) -> NavigationTarget<&str> {
        match self {
            Self::Workspace(id) => NavigationTarget::Workspace(id.as_ref()),
            Self::Tab(id) => NavigationTarget::Tab(id.as_ref()),
            Self::Pane(id) => NavigationTarget::Pane(id.as_ref()),
        }
    }

    fn to_owned(&self) -> OwnedNavigationTarget {
        match self.as_ref() {
            NavigationTarget::Workspace(id) => NavigationTarget::Workspace(id.to_owned()),
            NavigationTarget::Tab(id) => NavigationTarget::Tab(id.to_owned()),
            NavigationTarget::Pane(id) => NavigationTarget::Pane(id.to_owned()),
        }
    }
}

struct HerdrWindow {
    updater: updater::Updater,
    update_preview: Option<updater::State>,
    config: config::Config,
    theme: config::Theme,
    config_load: Option<Task<()>>,
    endpoints: Vec<endpoint::Endpoint>,
    selected_endpoint: usize,
    selection_epoch: u64,
    catalog: endpoint::Catalog,
    activation_deadline: Option<std::time::Instant>,
    pending_navigation: Option<OwnedNavigationTarget>,
    pending_releases: Vec<endpoint::Release>,
    selected_generation: u64,
    live: LiveState,
    focus: FocusHandle,
    options: ConnectOptions,
    last_queued_options: Option<ConnectOptions>,
    active: bool,
    sent_focus: Option<bool>,
    bounds: Bounds<Pixels>,
    /// Last title pushed to the OS, so the window is renamed only when it changes.
    title: String,
    cell_width: f32,
    painter: std::rc::Rc<std::cell::RefCell<terminal_painter::TerminalPainter>>,
    marked: String,
    local_error: Option<String>,
    menu: menu::MenuState,
    install_warning_shown: bool,
    collapsed_repos: std::collections::HashSet<String>,
    sidebar_visible: bool,
    wheel: WheelAccumulator,
    sidebar_width: Option<f32>,
    sidebar_drag: Option<(f32, f32)>,
    sidebar_preferences: Option<preferences::Preferences>,
    sidebar_modified: bool,
    agent_sort: preferences::AgentSort,
    /// Keeps a toggle made before the stored chrome arrives from being undone.
    agent_sort_modified: bool,
    avatars: Option<avatars::Avatars>,
    #[cfg(feature = "integration-test")]
    input_probe: smoke::InputProbe,
    /// Spaces and agents lists, in that order.
    sidebar_scroll: [ScrollHandle; 2],
    /// The row each list has scrolled into view, so a new selection is revealed
    /// while the user's own scrolling of an unchanged one is left alone.
    sidebar_revealed: [std::cell::Cell<Option<usize>>; 2],
    _poll: Task<()>,
    _activation: Subscription,
}

impl HerdrWindow {
    fn new(
        target: ConnectTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
        #[cfg(feature = "integration-test")] sidebar_test: bool,
    ) -> Self {
        #[cfg(feature = "integration-test")]
        let target = if sidebar_test {
            ConnectTarget::Socket("/unused-sidebar-fixture.sock".into())
        } else {
            target
        };
        let focus = cx.focus_handle();
        window.focus(&focus);
        let timer = cx.background_executor().clone();
        let poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                timer.timer(Duration::from_millis(16)).await;
                if this
                    .update_in(cx, |this, window, cx| {
                        if this.updater.poll() {
                            match this.updater.commit_restart() {
                                Ok(true) => {
                                    cx.quit();
                                    return;
                                }
                                Ok(false) => {}
                                Err(error) => eprintln!("App update restart failed: {error}"),
                            }
                            cx.notify();
                        }
                        if this.avatars.as_mut().is_some_and(|avatars| avatars.poll()) {
                            cx.notify();
                        }
                        if let Some(chrome) =
                            this.sidebar_preferences.as_mut().and_then(|p| p.loaded())
                        {
                            if !this.sidebar_modified {
                                this.sidebar_width = chrome.sidebar_width;
                            }
                            if !this.agent_sort_modified {
                                this.agent_sort = chrome.agent_sort;
                            }
                            cx.notify();
                        }
                        let old_pane = this
                            .live
                            .snapshot
                            .as_ref()
                            .and_then(|s| s.focused_pane_id.clone());
                        this.poll_endpoints(cx);
                        this.update_deletion_dialog();
                        this.poll_tab_rename(window, cx);
                        if old_pane
                            != this
                                .live
                                .snapshot
                                .as_ref()
                                .and_then(|s| s.focused_pane_id.clone())
                        {
                            this.marked.clear();
                        }
                        if this.update_workspace_pr() {
                            cx.notify();
                        }
                        if this.live.missing_installation && !this.install_warning_shown {
                            this.install_warning_shown = true;
                            this.show_install_modal(window, cx);
                        }
                        this.resize();
                        this.report_focus();
                        this.sync_window_title(window);
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut this = Self {
            updater: updater::Updater::default(),
            update_preview: None,
            config: config::Config::default(),
            theme: config::Theme::default(),
            config_load: None,
            catalog: endpoint::Catalog::new(&target),
            endpoints: vec![endpoint::Endpoint::new(
                endpoint::LOCAL.into(),
                "Local".into(),
                target,
                true,
            )],
            selected_endpoint: 0,
            selection_epoch: 0,
            activation_deadline: None,
            pending_navigation: None,
            pending_releases: Vec::new(),
            selected_generation: 0,
            live: LiveState::default(),
            focus,
            options: ConnectOptions::default(),
            last_queued_options: None,
            active: window.is_window_active(),
            sent_focus: None,
            bounds: Bounds::default(),
            title: WINDOW_TITLE.to_owned(),
            cell_width: 9.,
            painter: Default::default(),
            marked: String::new(),
            local_error: None,
            menu: menu::MenuState::new(cx),
            install_warning_shown: false,
            collapsed_repos: Default::default(),
            sidebar_visible: true,
            wheel: WheelAccumulator::default(),
            sidebar_width: None,
            sidebar_drag: None,
            sidebar_preferences: None,
            sidebar_modified: false,
            agent_sort: preferences::AgentSort::default(),
            agent_sort_modified: false,
            avatars: None,
            #[cfg(feature = "integration-test")]
            input_probe: smoke::InputProbe::default(),
            sidebar_scroll: Default::default(),
            sidebar_revealed: Default::default(),
            _poll: poll,
            _activation: cx.observe_window_activation(window, |this, window, cx| {
                this.active = window.is_window_active();
                this.report_focus();
                cx.notify();
            }),
        };
        #[cfg(feature = "integration-test")]
        if sidebar_test {
            this._poll = Task::ready(());
            this.live.snapshot = Some(Arc::new(sidebar::layout_tests::snapshot(40)));
            this.endpoints[0].live = this.live.clone();
            if let Ok(mut inbox) = this.endpoints[0].connection.inbox.lock() {
                *inbox = this.live.clone();
            }
            return this;
        }
        this.sidebar_preferences = this.endpoints[0]
            .connection
            .target
            .socket_path()
            .ok()
            .map(|path| preferences::Preferences::new(&path));
        this.avatars = Some(avatars::Avatars::new());
        this.reconnect();
        log_window::set_appearance(&this.config, &this.theme, cx);
        this.load_gui_config(cx);
        this
    }

    fn resize(&mut self) {
        if self.last_queued_options == Some(self.options) {
            return;
        }
        if let (Some(handle), Some(snapshot)) = (
            &self.endpoints[self.selected_endpoint].connection.handle,
            &self.live.snapshot,
        ) {
            match handle.resize(&snapshot.boot_id, self.options) {
                Ok(()) => self.last_queued_options = Some(self.options),
                Err(error) => self.local_error = Some(format!("Resize: {error}")),
            }
        }
    }

    /// macOS lists every window in the Window menu by title. Windows onto the
    /// same daemon are told apart by the space each one is showing.
    fn sync_window_title(&mut self, window: &mut Window) {
        let title = self
            .live
            .snapshot
            .as_ref()
            .and_then(|snapshot| {
                let focused = snapshot.focused_workspace_id.as_deref()?;
                snapshot
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.workspace_id == focused)
            })
            .map_or_else(
                || WINDOW_TITLE.to_owned(),
                |workspace| {
                    format!(
                        "{WINDOW_TITLE} \u{2014} {}",
                        sidebar::workspace_label(workspace, false)
                    )
                },
            );
        if self.title != title {
            window.set_window_title(&title);
            self.title = title;
        }
    }

    fn report_focus(&mut self) {
        let focused = self.active && self.endpoints[self.selected_endpoint].surface_requested();
        // Update the authoritative event inbox, not just the rendered clone.
        if let Ok(mut state) = self.endpoints[self.selected_endpoint]
            .connection
            .inbox
            .try_lock()
        {
            state.set_outer_focus(self.active && self.input_ready());
        }
        if self.sent_focus == Some(focused) {
            return;
        }
        if let (Some(handle), Some(snapshot)) = (
            &self.endpoints[self.selected_endpoint].connection.handle,
            &self.live.snapshot,
        ) && handle.set_focus(&snapshot.boot_id, focused).is_ok()
        {
            self.sent_focus = Some(focused);
        }
    }

    fn send(&mut self, event: ClientPaneInputEvent, cx: &mut Context<Self>) {
        if self.menu.page.is_some() || !self.input_ready() {
            return;
        }
        if let (Some(handle), Some(snapshot), Some(surface)) = (
            &self.endpoints[self.selected_endpoint].connection.handle,
            &self.live.snapshot,
            &self.live.surface,
        ) {
            let target = if let Some(popup) = &surface.popup {
                InputTarget::Popup(popup.terminal_id.clone())
            } else if let Some(pane) = &snapshot.focused_pane_id {
                InputTarget::Pane(pane.clone())
            } else {
                return;
            };
            if let Err(error) =
                ConnectionBridge::send_input(handle, &snapshot.boot_id, &target, event)
            {
                self.local_error = Some(format!("Input not sent: {error}"));
                cx.notify();
            }
        }
    }

    fn navigate(&mut self, target: NavigationTarget<&str>, cx: &mut Context<Self>) {
        if self.menu.page.is_some() || !self.input_ready() {
            return;
        }
        self.request_focus_change(
            "Navigate",
            Some(target.to_owned()),
            |handle, boot| match target {
                NavigationTarget::Workspace(id) => handle.focus_workspace(boot, id),
                NavigationTarget::Tab(id) => handle.focus_tab(boot, id),
                NavigationTarget::Pane(id) => handle.focus_pane(boot, id),
            },
        );
        self.marked.clear();
        cx.notify();
    }

    fn request_focus_change(
        &mut self,
        method: &str,
        focus: Option<OwnedNavigationTarget>,
        enqueue: impl FnOnce(
            &herdr_client::ClientHandle,
            &str,
        ) -> Result<String, herdr_client::SendError>,
    ) {
        if let (Some(handle), Some(snapshot)) = (
            &self.endpoints[self.selected_endpoint].connection.handle,
            &self.live.snapshot,
        ) {
            if let Err(error) = enqueue(handle, &snapshot.boot_id) {
                self.local_error = Some(format!("{method}: {error}"));
            } else {
                self.fence_focus_change(focus);
            }
        }
    }

    fn fence_focus_change(&mut self, focus: Option<OwnedNavigationTarget>) {
        if !self.live.supports_surface {
            return;
        }
        if let (Some(handle), Some(snapshot)) = (
            &self.endpoints[self.selected_endpoint].connection.handle,
            &self.live.snapshot,
        ) && let Ok(mut state) = self.endpoints[self.selected_endpoint]
            .connection
            .inbox
            .lock()
        {
            // An ordered surface barrier prevents input hitting the previous
            // pane while navigation/creation and its projection are in flight.
            // Register the barrier under the inbox lock before input can resume.
            let (request, failed) = match handle.set_surface_active(&snapshot.boot_id, true) {
                Ok(request) => (request, false),
                Err(error) => {
                    self.local_error = Some(error.to_string());
                    (String::new(), true)
                }
            };
            state.activation = Some(state::SurfaceActivation {
                request,
                boot: snapshot.boot_id.clone(),
                revision: None,
                failed,
                focus,
                active: true,
            });
            state.surface = None;
            state.dirty = true;
            self.live = state.clone();
            self.activation_deadline = Some(
                std::time::Instant::now()
                    + if failed {
                        Duration::ZERO
                    } else {
                        Duration::from_secs(5)
                    },
            );
        }
    }

    fn command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.page.is_some() {
            return;
        }
        #[cfg(feature = "integration-test")]
        {
            self.input_probe.actions += 1;
        }
        match command {
            Command::Logs => {
                log_window::open(cx);
                return;
            }
            Command::NewWindow => {
                // Another client of the same launch target, not another daemon.
                open_additional_window(self.endpoints[0].connection.target.clone(), cx);
                return;
            }
            Command::ClosePane | Command::CloseTab => {
                self.open_close_confirmation(command, window, cx);
                return;
            }
            Command::Palette | Command::WorkspacePicker => {
                self.open_palette(command == Command::WorkspacePicker, window, cx);
                return;
            }
            Command::Keybinds => {
                self.open_keybinds(window, cx);
                return;
            }
            Command::Themes => {
                self.open_theme_picker(window, cx);
                return;
            }
            Command::Settings => {
                self.open_preferences(window, cx);
                return;
            }
            Command::About => {
                self.open_about(window, cx);
                return;
            }
            Command::ToggleSidebar => self.sidebar_visible = !self.sidebar_visible,
            Command::Reconnect => self.reconnect(),
            Command::Quit => {
                cx.quit();
                return;
            }
            _ => {}
        }
        if self.activation_deadline.is_some()
            || !self.endpoints[self.selected_endpoint].surface_requested()
        {
            return;
        }
        if let Some(snapshot) = &self.live.snapshot
            && let Some((method, params)) = controls::request(command, snapshot)
        {
            self.request_focus_change(method, None, |handle, boot| {
                handle.request(boot, method, params)
            });
            self.marked.clear();
        }
        window.focus(&self.focus);
        cx.notify();
    }

    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.menu.page.is_some() || !self.input_ready() {
            return;
        }
        let (Some(handle), Some(snapshot), Some(surface)) = (
            &self.endpoints[self.selected_endpoint].connection.handle,
            &self.live.snapshot,
            &self.live.surface,
        ) else {
            return;
        };
        let x = (event.position.x - self.bounds.origin.x).to_f64() as f32;
        let y = (event.position.y - self.bounds.origin.y).to_f64() as f32;
        let cell_height = self.config.terminal.line_height();
        let Some(target) = wheel_target(surface, x, y, self.cell_width, cell_height) else {
            self.wheel = WheelAccumulator::default();
            return;
        };
        let lines = self.wheel.lines(&target.target, event, cell_height);
        cx.stop_propagation();
        if lines == 0 {
            return;
        }
        let input = target.event(lines, event.modifiers);
        let result = ConnectionBridge::send_input(handle, &snapshot.boot_id, &target.target, input);
        if let Err(error) = result {
            self.local_error = Some(format!("Wheel input not sent: {error}"));
            cx.notify();
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(feature = "integration-test")]
        {
            self.input_probe.keys += 1;
        }
        if event.keystroke.modifiers.platform && event.keystroke.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.send(ClientPaneInputEvent::Paste(text), cx);
            }
            cx.stop_propagation();
            window.prevent_default();
        } else if self.marked.is_empty()
            && let Some(input) = key_input(event)
        {
            self.send(input, cx);
            cx.stop_propagation();
            window.prevent_default();
        }
    }
}

#[cfg(test)]
mod window_tests {
    #![allow(clippy::unwrap_used)]
    use super::{Command, HerdrWindow, WINDOW_TITLE};
    use crate::sidebar::layout_tests::{fixture_window, snapshot};
    use std::sync::Arc;

    fn main_windows(cx: &mut gpui::App) -> Vec<gpui::WindowHandle<HerdrWindow>> {
        cx.windows()
            .iter()
            .filter_map(gpui::AnyWindowHandle::downcast::<HerdrWindow>)
            .collect()
    }

    #[gpui::test]
    fn new_window_adds_one_client_of_the_same_target_without_disturbing_the_first(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(fixture_window);
        let target = view.update(cx, |view, _| view.endpoints[0].connection.target.clone());
        let before = cx.update(|_, cx| main_windows(cx));
        assert_eq!(before.len(), 1);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.command(Command::NewWindow, window, cx));
        });
        cx.run_until_parked();
        let opened = cx.update(|_, cx| main_windows(cx));
        assert_eq!(opened.len(), 2, "one more window onto the same daemon");
        let second = opened
            .into_iter()
            .find(|handle| !before.contains(handle))
            .unwrap();
        let first_inbox = view.update(cx, |view, _| view.endpoints[0].connection.inbox.clone());
        cx.update(|_, cx| {
            // The new window is a separate client: its own endpoint and inbox.
            second
                .update(cx, |second, _, _| {
                    assert_eq!(second.endpoints.len(), 1);
                    assert_eq!(second.endpoints[0].connection.target, target);
                    assert!(!Arc::ptr_eq(
                        &second.endpoints[0].connection.inbox,
                        &first_inbox
                    ));
                })
                .unwrap();
        });
        // The originating window keeps its own selection and error state.
        view.read_with(cx, |view, _| {
            assert_eq!(view.selected_endpoint, 0);
            assert!(view.local_error.is_none());
        });
    }

    #[gpui::test]
    fn window_title_follows_the_focused_space_of_that_window(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(fixture_window);
        cx.update(|window, cx| {
            view.update(cx, |view, _| {
                let mut snapshot = snapshot(4);
                snapshot.focused_workspace_id = Some("w0".into());
                view.live.snapshot = Some(Arc::new(snapshot));
            });
            view.update(cx, |view, _| view.sync_window_title(window));
        });
        assert_eq!(
            view.read_with(cx, |view, _| view.title.clone()),
            format!("{WINDOW_TITLE} \u{2014} herdr")
        );
        // An unknown focus falls back to the bare product name.
        cx.update(|window, cx| {
            view.update(cx, |view, _| {
                view.live.snapshot = None;
                view.sync_window_title(window);
            });
        });
        assert_eq!(
            view.read_with(cx, |view, _| view.title.clone()),
            WINDOW_TITLE
        );
    }
}

#[cfg(all(test, feature = "integration-test"))]
mod tests {
    use super::{ConnectTarget, HerdrWindow};

    #[gpui::test]
    fn resize_tracks_cell_metrics_and_retries_failed_options(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            HerdrWindow::new(
                ConnectTarget::Socket("/unused-resize-test.sock".into()),
                window,
                cx,
                true,
            )
        });
        view.update(cx, |view, _| {
            let client = herdr_client::connect(
                view.endpoints[view.selected_endpoint]
                    .connection
                    .target
                    .clone(),
                view.options,
            )
            .unwrap_or_else(|error| panic!("cannot create test client: {error}"));
            client.handle.disconnect();
            view.endpoints[view.selected_endpoint].connection.handle = Some(client.handle);
            let queued = view.options;
            view.last_queued_options = Some(queued);
            view.resize();
            assert!(
                view.local_error.is_none(),
                "identical options are not resent"
            );
            view.options.cell_width_px += 1;
            view.resize();
            assert!(
                view.local_error.is_some(),
                "cell metrics alone trigger a send"
            );
            assert_eq!(view.last_queued_options, Some(queued));
            view.local_error = None;
            view.resize();
            assert!(view.local_error.is_some(), "failed options are retried");
        });
    }
}

impl Render for HerdrWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.restore_menu_focus(window);
        let font = font(self.config.terminal.family.clone());
        let cell_height = self.config.terminal.line_height();
        self.painter.borrow_mut().set_appearance(
            self.config.terminal.size,
            cell_height,
            self.theme.clone(),
        );
        self.cell_width = self.painter.borrow_mut().cell_width(&font, window, cx);
        let sidebar = self.render_sidebar(window, cx);
        let mut tabs = div()
            .id("tabs")
            .flex()
            .flex_none()
            .h(px((self.config.tabs.size * 1.6 + 4.).max(TAB_HEIGHT)))
            .font_family(self.config.tabs.family.clone())
            .text_size(px(self.config.tabs.size))
            .overflow_x_scroll()
            .bg(rgb(self.theme.surface))
            .text_color(rgb(self.theme.foreground))
            .items_center();
        if let Some(snapshot) = &self.live.snapshot {
            for tab in snapshot
                .tabs
                .iter()
                .filter(|t| Some(&t.workspace_id) == snapshot.focused_workspace_id.as_ref())
            {
                let id = tab.tab_id.clone();
                let context_id = id.clone();
                let close_id = id.clone();
                // Selected tabs carry the theme's accent, so the choice reads as
                // primary rather than as the hover tint used elsewhere; the rest
                // recede into the strip, as they do in the reference UI.
                let (background, text) = if tab.focused {
                    let background = self.theme.primary_wash();
                    (background, self.theme.text_on(background))
                } else {
                    (self.theme.surface, self.theme.muted)
                };
                tabs = tabs.child(
                    div()
                        .id(SharedString::from(format!("tab-{id}")))
                        .debug_selector({
                            let id = id.clone();
                            move || format!("tab-{id}")
                        })
                        .pl(px(12.))
                        // The close button hugs the tab's inner right edge, well
                        // clear of the label it would otherwise crowd.
                        .pr(px(3.))
                        .py(px(2.))
                        // Even cells divided by a single rule, as in the reference UI.
                        .min_w(px(TAB_WIDTH))
                        .border_r_1()
                        .border_color(rgb(self.theme.active))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .cursor_pointer()
                        .bg(rgb(background))
                        .text_color(rgb(text))
                        .child(tab.label.clone())
                        .child(
                            div()
                                .id("close-tab")
                                .debug_selector({
                                    let id = id.clone();
                                    move || format!("close-tab-{id}")
                                })
                                .size(px(18.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(3.))
                                .hover(move |s| s.bg(rgba((text << 8) | 0x24)))
                                .child(
                                    svg()
                                        .path("icons/close.svg")
                                        .debug_selector({
                                            let id = id.clone();
                                            move || format!("close-tab-icon-{id}")
                                        })
                                        .size(px(12.))
                                        .text_color(rgb(text)),
                                )
                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.open_tab_close(&close_id, window, cx);
                                })),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.open_tab_menu(&context_id, event.position, window, cx);
                            }),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.navigate(NavigationTarget::Tab(&id), cx);
                            window.focus(&this.focus);
                        })),
                );
            }
        }
        let surface = self
            .live
            .surface
            .clone()
            .filter(|_| self.live.surface_ready());
        let entity = cx.entity();
        let paint_entity = entity.clone();
        let focus = self.focus.clone();
        let cell_width = self.cell_width;
        let painter = self.painter.clone();
        let terminal = div()
            .id("terminal")
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .bg(rgb(self.theme.background))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus);
                    if let Some(surface) = &this.live.surface
                        && surface.popup.is_none()
                    {
                        let col = ((event.position.x - this.bounds.origin.x).to_f64()
                            / this.cell_width as f64)
                            .floor() as u16;
                        let row = ((event.position.y - this.bounds.origin.y).to_f64()
                            / this.config.terminal.line_height() as f64)
                            .floor() as u16;
                        let pane = surface
                            .panes
                            .iter()
                            .find(|p| {
                                col >= p.rect.x
                                    && col < p.rect.x.saturating_add(p.rect.width)
                                    && row >= p.rect.y
                                    && row < p.rect.y.saturating_add(p.rect.height)
                            })
                            .map(|p| p.pane_id.clone());
                        if let Some(id) = pane {
                            this.navigate(NavigationTarget::Pane(&id), cx);
                        }
                    }
                }),
            )
            .child(
                canvas(
                    move |bounds, _, cx| {
                        entity.update(cx, |this, _| {
                            this.bounds = bounds;
                            this.options = ConnectOptions {
                                surface_size: viewport(
                                    bounds.size.width.to_f64() as f32,
                                    bounds.size.height.to_f64() as f32,
                                    cell_width,
                                    cell_height,
                                ),
                                cell_width_px: cell_width.round().max(1.) as u32,
                                cell_height_px: cell_height.round().max(1.) as u32,
                            };
                            this.resize();
                        });
                    },
                    move |bounds, _, window, cx| {
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, paint_entity.clone()),
                            cx,
                        );
                        if let Some(surface) = &surface {
                            painter.borrow_mut().paint_frame(
                                &surface.frame,
                                bounds.origin,
                                cell_width,
                                &font,
                                window,
                                cx,
                            );
                            if let Some(popup) = &surface.popup {
                                let offset = popup_origin(
                                    &surface.frame,
                                    &popup.frame,
                                    cell_width,
                                    cell_height,
                                );
                                painter.borrow_mut().paint_frame(
                                    &popup.frame,
                                    bounds.origin + offset,
                                    cell_width,
                                    &font,
                                    window,
                                    cx,
                                );
                            }
                        }
                    },
                )
                .size_full(),
            );
        let status = self.live.status_text(self.local_error.as_deref());
        div()
            .on_action(cx.listener(|this, action: &RunCommand, window, cx| {
                this.command(action.command, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowHerdrNotDetected, window, cx| {
                this.show_install_modal(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CheckForUpdates, window, cx| {
                this.open_app_update(false, window, cx);
                this.updater.check();
            }))
            .on_action(cx.listener(|this, _: &ShowUpdatePreview, window, cx| {
                this.open_app_update(true, window, cx);
            }))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(self.theme.background))
            .text_color(rgb(self.theme.foreground))
            .font_family(self.config.ui.family.clone())
            .text_size(px(self.config.ui.size))
            .child(self.render_titlebar(cx))
            .children(worktree_banner::render(
                env!("HERDR_BUILD_WORKTREE") == "1",
                env!("HERDR_BUILD_BRANCH"),
                env!("HERDR_BUILD_PR"),
            ))
            .child(
                div()
                    .debug_selector(|| "window-body".into())
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .when(self.sidebar_visible, |row| row.child(sidebar))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .flex_none()
                                    .bg(rgb(self.theme.surface))
                                    .text_color(rgb(self.theme.foreground))
                                    // Tabs size to their content and shrink when the
                                    // row is full, so the button sits after the last
                                    // tab instead of at the far right of the window.
                                    .child(tabs.flex_shrink().min_w_0())
                                    .child(
                                        div()
                                            .id("new-tab")
                                            .debug_selector(|| "new-tab".into())
                                            .w(px(34.))
                                            .min_h(px(TAB_HEIGHT))
                                            .border_r_1()
                                            .border_color(rgb(self.theme.active))
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .hover(|s| s.bg(rgb(self.theme.active)))
                                            .child(
                                                svg()
                                                    .path("icons/plus.svg")
                                                    .debug_selector(|| "new-tab-icon".into())
                                                    .size(px(14.))
                                                    // Quiet like the unselected tabs beside it.
                                                    .text_color(rgb(self.theme.muted)),
                                            )
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.command(Command::Tab, window, cx)
                                            })),
                                    ),
                            )
                            .child(terminal),
                    ),
            )
            .child(
                div()
                    .id("connection-status")
                    .debug_selector(|| "connection-status".into())
                    .flex()
                    .flex_none()
                    .h(px((self.config.ui.size * 1.5 + 4.).max(22.)))
                    .overflow_hidden()
                    .items_center()
                    .gap(px(6.))
                    .px_3()
                    .bg(rgb(self.theme.surface))
                    .text_color(rgb(self.theme.foreground))
                    .child(
                        if matches!(self.live.status, ConnectionStatus::StartingDaemon) {
                            div()
                                .size(px(8.))
                                .flex_none()
                                .rounded_full()
                                .bg(rgb(self.theme.palette[3]))
                                .with_animation(
                                    "daemon-starting-loader",
                                    Animation::new(Duration::from_secs(1)).repeat(),
                                    |dot, delta| {
                                        dot.opacity(
                                            0.3 + 0.7 * (delta * std::f32::consts::PI).sin(),
                                        )
                                    },
                                )
                                .into_any_element()
                        } else {
                            div()
                                .size(px(6.))
                                .flex_none()
                                .rounded_full()
                                .bg(rgb(if self.live.status.is_connected() {
                                    self.theme.palette[2]
                                } else {
                                    self.theme.palette[1]
                                }))
                                .into_any_element()
                        },
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(status),
                    )
                    .when(!self.marked.is_empty(), |d| {
                        d.child(
                            div()
                                .min_w_0()
                                .max_w(px(160.))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(format!("Composing: {}", self.marked)),
                        )
                    })
                    .child(
                        div()
                            .id("status-theme")
                            .debug_selector(|| "status-theme".into())
                            .flex_none()
                            .px_2()
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(self.theme.active)))
                            .child("Theme")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_theme_picker(window, cx);
                            })),
                    )
                    .child(
                        div()
                            .id("status-keybinds")
                            .debug_selector(|| "status-keybinds".into())
                            .flex_none()
                            .px_2()
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(self.theme.active)))
                            .child("? Keybinds")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_keybinds(window, cx);
                            })),
                    )
                    .child(
                        div()
                            .id("report-issue")
                            .debug_selector(|| "report-issue".into())
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .px_2()
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(self.theme.active)))
                            .child(
                                div()
                                    .size(px(12.))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .border_1()
                                    .border_color(rgb(self.theme.foreground))
                                    .child(
                                        div()
                                            .size(px(3.))
                                            .rounded_full()
                                            .bg(rgb(self.theme.foreground)),
                                    ),
                            )
                            .child("Report issue")
                            .on_click(|_, _, cx| {
                                cx.open_url(&format!(
                                    "https://github.com/penso/herdr-gpui/issues/new?template=bug_report.yml&version={}",
                                    APP_VERSION.replace('+', "%2B"),
                                ));
                            }),
                    )
                    .child(
                        div()
                            .id("status-version")
                            .debug_selector(|| "status-version".into())
                            .flex_none()
                            .whitespace_nowrap()
                            .text_color(rgb(self.theme.muted))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(self.theme.active)))
                            .child(if matches!(self.updater.state(), updater::State::Available { .. } | updater::State::Ready { .. }) {
                                "Update available"
                            } else {
                                APP_VERSION
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_app_update(false, window, cx);
                            })),
                    ),
            )
            .when(self.menu.page.is_some(), |root| {
                root.child(self.render_menu(window, cx))
            })
    }
}

/// The native menu bar. About leads the application menu, as on classic macOS.
fn menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "Herdr".into(),
            items: vec![
                MenuItem::action(
                    "About Herdr",
                    RunCommand {
                        command: Command::About,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action(
                    "Command Palette",
                    RunCommand {
                        command: Command::Palette,
                    },
                ),
                MenuItem::action(
                    "Settings",
                    RunCommand {
                        command: Command::Settings,
                    },
                ),
                MenuItem::action(
                    "Keybinds",
                    RunCommand {
                        command: Command::Keybinds,
                    },
                ),
                MenuItem::action("Check for Updates...", CheckForUpdates),
                MenuItem::separator(),
                MenuItem::action("Quit Herdr", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action(
                    "New Workspace",
                    RunCommand {
                        command: Command::Workspace,
                    },
                ),
                MenuItem::action(
                    "New Tab",
                    RunCommand {
                        command: Command::Tab,
                    },
                ),
                MenuItem::action(
                    "Switch Workspace",
                    RunCommand {
                        command: Command::WorkspacePicker,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action(
                    "Close Pane...",
                    RunCommand {
                        command: Command::ClosePane,
                    },
                ),
                MenuItem::action(
                    "Close Tab...",
                    RunCommand {
                        command: Command::CloseTab,
                    },
                ),
            ],
        },
        Menu {
            name: "Terminal".into(),
            items: vec![
                MenuItem::action(
                    "Split Vertically (Right)",
                    RunCommand {
                        command: Command::SplitRight,
                    },
                ),
                MenuItem::action(
                    "Split Horizontally (Down)",
                    RunCommand {
                        command: Command::SplitDown,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action(
                    "Next Tab",
                    RunCommand {
                        command: Command::NextTab,
                    },
                ),
                MenuItem::action(
                    "Previous Tab",
                    RunCommand {
                        command: Command::PreviousTab,
                    },
                ),
                MenuItem::action(
                    "Toggle Pane Zoom",
                    RunCommand {
                        command: Command::Zoom,
                    },
                ),
                MenuItem::action(
                    "Toggle Sidebar",
                    RunCommand {
                        command: Command::ToggleSidebar,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action(
                    "Reconnect",
                    RunCommand {
                        command: Command::Reconnect,
                    },
                ),
            ],
        },
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action(
                    "New Window",
                    RunCommand {
                        command: Command::NewWindow,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action("GPUI Logs", ShowLogs),
            ],
        },
        Menu {
            name: "QA".into(),
            items: vec![
                MenuItem::action("Show herdr non-detected modal", ShowHerdrNotDetected),
                MenuItem::action("Show app update available", ShowUpdatePreview),
            ],
        },
    ]
}

/// Opens one main window onto `target`. Every window is an independent client
/// of that daemon: its own connection, surface lease, and workspace focus.
fn open_window(
    target: ConnectTarget,
    updater: updater::Updater,
    cx: &mut App,
    #[cfg(feature = "integration-test")] fixture: bool,
) -> anyhow::Result<WindowHandle<HerdrWindow>> {
    // Cascade rather than stack windows exactly, so a new one is visible at once.
    let existing = cx
        .windows()
        .iter()
        .filter(|handle| handle.downcast::<HerdrWindow>().is_some())
        .count();
    let step = px(28. * existing.min(6) as f32);
    let mut bounds = Bounds::centered(None, size(px(1200.), px(780.)), cx);
    bounds.origin += point(step, step);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(640.), px(400.))),
            titlebar: Some(titlebar::options(WINDOW_TITLE)),
            app_id: Some("so.pen.herdr-gpui".into()),
            ..Default::default()
        },
        |window, cx| {
            cx.new(|cx| {
                let mut view = HerdrWindow::new(
                    target,
                    window,
                    cx,
                    #[cfg(feature = "integration-test")]
                    fixture,
                );
                view.updater = updater;
                view
            })
        },
    )
}

/// Opens another window from inside the focused window's own update.
fn open_additional_window(target: ConnectTarget, cx: &mut App) {
    cx.defer(move |cx| {
        let opened = open_window(
            target,
            updater::Updater::secondary(),
            cx,
            #[cfg(feature = "integration-test")]
            false,
        );
        match opened {
            Ok(handle) => {
                let _ = handle.update(cx, |_, window, _| window.activate_window());
            }
            Err(_) => tracing::error!("Unable to open an additional Herdr window"),
        }
    });
}

fn main() -> std::process::ExitCode {
    let exit = run();
    if exit != std::process::ExitCode::SUCCESS {
        return exit;
    }
    #[cfg(feature = "integration-test")]
    return std::process::ExitCode::from(
        smoke::EXIT_CODE.load(std::sync::atomic::Ordering::SeqCst),
    );
    #[cfg(not(feature = "integration-test"))]
    std::process::ExitCode::SUCCESS
}

fn run() -> std::process::ExitCode {
    use cli::{LaunchMode, LaunchOptions};
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if let Some(exit) = updater::run_helper(&args) {
        return exit;
    }
    let LaunchOptions { target, mode } = match LaunchOptions::parse(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!(
                "{error}\nUsage: herdr-gpui [--socket CLIENT_SOCKET | --session NAME [--dev]]"
            );
            return std::process::ExitCode::from(2);
        }
    };
    if mode == LaunchMode::BuildInfo {
        print!("{}", cli::build_info());
        return std::process::ExitCode::SUCCESS;
    }
    if mode == LaunchMode::Help {
        println!(
            "herdr-gpui [--socket CLIENT_SOCKET | --session NAME [--dev]]\nConnects to Local and saved SSH hosts; never installs remote software.\nStarts the local Herdr daemon if needed; never stops it.\nExplicit --socket and --dev targets are attach-only; --socket isolates the GUI to one existing daemon."
        );
        println!(
            "  --build-info        Print the executable's build identity without starting the GUI"
        );
        #[cfg(feature = "integration-test")]
        println!(
            "  --integration-test  Run native GUI checks (requires explicit --socket)\n  --sidebar-test      Run native sidebar fixtures without connecting to a daemon\n  --performance-test  Measure native dense-terminal hover/scroll without a daemon (macOS)"
        );
        return std::process::ExitCode::SUCCESS;
    }
    #[cfg(feature = "integration-test")]
    let integration_test = mode == LaunchMode::Integration;
    #[cfg(feature = "integration-test")]
    let sidebar_test = mode == LaunchMode::Sidebar;
    #[cfg(feature = "integration-test")]
    let performance_test = mode == LaunchMode::Performance;
    #[cfg(feature = "integration-test")]
    if mode != LaunchMode::Normal {
        smoke::EXIT_CODE.store(1, std::sync::atomic::Ordering::SeqCst);
    }
    let startup_failed = std::rc::Rc::new(std::cell::Cell::new(false));
    if let Err(error) = diagnostics::init() {
        eprintln!("Unable to initialize diagnostics: {error}");
        return std::process::ExitCode::FAILURE;
    }
    tracing::info!(
        version = APP_VERSION,
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        "GPUI client starting"
    );
    let failed = startup_failed.clone();
    Application::new().with_assets(icons::Icons).run(move |cx| {
        app_icon::install();
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &ShowLogs, cx| log_window::open(cx));
        bind_keys(cx);
        cx.set_menus(menus());
        cx.on_window_closed(move |cx| {
            if cx.windows().is_empty() {
                #[cfg(feature = "integration-test")]
                if performance_test {
                    std::process::exit(1);
                }
                cx.quit();
            }
        })
        .detach();
        // Native test modes and CLI invocations never start an updater worker.
        let updater = if mode == LaunchMode::Normal {
            updater::Updater::start()
        } else {
            updater::Updater::default()
        };
        let opened = open_window(
            target,
            updater,
            cx,
            #[cfg(feature = "integration-test")]
            {
                sidebar_test || performance_test
            },
        );
        match opened {
            Ok(_window) => {
                #[cfg(feature = "integration-test")]
                if performance_test {
                    performance::start(_window, cx);
                }
                #[cfg(feature = "integration-test")]
                if integration_test {
                    smoke::start(_window, cx);
                }
                #[cfg(feature = "integration-test")]
                if sidebar_test {
                    smoke::start_sidebar(_window, cx);
                }
            }
            Err(error) => {
                tracing::error!("Unable to open main window");
                eprintln!("Unable to open Herdr window: {error}");
                failed.set(true);
                #[cfg(feature = "integration-test")]
                if performance_test {
                    std::process::exit(1);
                }
                cx.quit();
            }
        }
        cx.activate(true);
    });
    if startup_failed.get() {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}
