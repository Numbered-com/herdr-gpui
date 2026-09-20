// objc 0.2's selectors expand a legacy cargo-clippy cfg in the native test adapter.
#![cfg_attr(feature = "integration-test", allow(unexpected_cfgs))]
mod app_icon;
mod avatars;
mod cli;
mod close_modal;
mod config;
mod connection;
mod controls;
mod daemon;
mod endpoint;
mod icons;
mod input;
mod menu;
mod palette;
#[cfg(feature = "integration-test")]
mod performance;
mod preferences;
mod search_input;
mod sidebar;
#[cfg(feature = "integration-test")]
mod smoke;
mod state;
mod tab_menu;
mod terminal;
mod terminal_painter;
mod theme_picker;
#[cfg(target_os = "macos")]
mod titlebar;
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

actions!(herdr, [Quit, ShowHerdrNotDetected]);

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
    config: config::Config,
    theme: config::Theme,
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
    avatars: Option<avatars::Avatars>,
    #[cfg(feature = "integration-test")]
    input_probe: smoke::InputProbe,
    #[cfg(feature = "integration-test")]
    sidebar_scroll: [ScrollHandle; 2],
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
                        if this.avatars.as_mut().is_some_and(|avatars| avatars.poll()) {
                            cx.notify();
                        }
                        if let Some(width) =
                            this.sidebar_preferences.as_mut().and_then(|p| p.loaded())
                            && !this.sidebar_modified
                        {
                            this.sidebar_width = width;
                            cx.notify();
                        }
                        let old_pane = this
                            .live
                            .snapshot
                            .as_ref()
                            .and_then(|s| s.focused_pane_id.clone());
                        this.poll_endpoints(cx);
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
                        if this.live.missing_installation && !this.install_warning_shown {
                            this.install_warning_shown = true;
                            this.show_install_modal(window, cx);
                        }
                        this.resize();
                        this.report_focus();
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let fixture = false;
        #[cfg(feature = "integration-test")]
        let fixture = fixture || sidebar_test;
        let loaded = if fixture {
            Ok(config::Config::default())
        } else {
            config::Config::load()
        };
        let (config, theme, config_error) =
            match loaded.and_then(|config| config.theme().map(|theme| (config, theme))) {
                Ok((config, theme)) => (config, theme, None),
                Err(error) => (
                    config::Config::default(),
                    config::Theme::default(),
                    Some(error),
                ),
            };
        let mut this = Self {
            config,
            theme,
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
            avatars: None,
            #[cfg(feature = "integration-test")]
            input_probe: smoke::InputProbe::default(),
            #[cfg(feature = "integration-test")]
            sidebar_scroll: Default::default(),
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
        if config_error.is_some() {
            this.local_error = config_error;
        }
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
        if !self.input_ready() {
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
        ) && let Ok(mut state) = self.endpoints[self.selected_endpoint]
            .connection
            .inbox
            .lock()
        {
            if let Err(error) = enqueue(handle, &snapshot.boot_id) {
                self.local_error = Some(format!("{method}: {error}"));
            } else if self.live.supports_surface {
                // An ordered surface barrier prevents input hitting the previous
                // pane while navigation/creation and its projection are in flight.
                // Hold the inbox lock until both requests and the fence are set.
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
            .h(px((self.config.tabs.size * 1.5 + 8.).max(32.)))
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
                tabs = tabs.child(
                    div()
                        .id(SharedString::from(format!("tab-{id}")))
                        .debug_selector({
                            let id = id.clone();
                            move || format!("tab-{id}")
                        })
                        .px_4()
                        .py(px(4.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .cursor_pointer()
                        .bg(rgb(if tab.focused {
                            self.theme.active
                        } else {
                            self.theme.surface
                        }))
                        .child(tab.label.clone())
                        .child(
                            div()
                                .id("close-tab")
                                .debug_selector({
                                    let id = id.clone();
                                    move || format!("close-tab-{id}")
                                })
                                .size(px(24.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(4.))
                                .hover(|s| s.bg(rgba((self.theme.foreground << 8) | 0x24)))
                                .child(
                                    svg()
                                        .path("icons/close.svg")
                                        .debug_selector({
                                            let id = id.clone();
                                            move || format!("close-tab-icon-{id}")
                                        })
                                        .size(px(16.))
                                        .text_color(rgb(self.theme.foreground)),
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
        let snapshot = self.live.snapshot.clone();
        let inbox = self.endpoints[self.selected_endpoint]
            .connection
            .inbox
            .clone();
        let paint_epoch = self.selection_epoch;
        let paint_generation = self.endpoints[self.selected_endpoint].generation;
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
                            if window.is_window_active()
                                && let Some(snapshot) = &snapshot
                            {
                                let snapshot = snapshot.clone();
                                let surface = surface.clone();
                                // Defer projection/COW work until after paint. On contention,
                                // retry via another draw, never by acknowledging inbox cells.
                                cx.defer(move |cx| {
                                    let owned = paint_entity.read(cx).owns_paint(
                                        paint_epoch,
                                        paint_generation,
                                        &inbox,
                                    );
                                    if !owned {
                                        return;
                                    }
                                    match inbox.try_lock() {
                                        Ok(mut state) => {
                                            state.acknowledge_presented_surface(
                                                &snapshot, &surface, true,
                                            );
                                        }
                                        Err(std::sync::TryLockError::WouldBlock) => {
                                            paint_entity.update(cx, |_, cx| cx.notify());
                                        }
                                        Err(std::sync::TryLockError::Poisoned(_)) => {}
                                    }
                                });
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
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(self.theme.background))
            .text_color(rgb(self.theme.foreground))
            .font_family(self.config.ui.family.clone())
            .text_size(px(self.config.ui.size))
            .map(|root| {
                #[cfg(target_os = "macos")]
                let root = root.child(titlebar::render(self.theme.surface, self.theme.foreground));
                root
            })
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
                                    .child(tabs.flex_1().min_w_0())
                                    .child(
                                        div()
                                            .id("new-tab")
                                            .debug_selector(|| "new-tab".into())
                                            .w(px(44.))
                                            .min_h(px(32.))
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
                                                    .size(px(18.))
                                                    .text_color(rgb(self.theme.foreground)),
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
                                cx.open_url(
                                    "https://github.com/penso/herdr-gpui/issues/new/choose",
                                );
                            }),
                    ),
            )
            .when(self.menu.page.is_some(), |root| {
                root.child(self.render_menu(window, cx))
            })
    }
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
    let LaunchOptions { target, mode } = match LaunchOptions::parse(std::env::args_os().skip(1)) {
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
    let failed = startup_failed.clone();
    Application::new().with_assets(icons::Icons).run(move |cx| {
        app_icon::install();
        cx.on_action(|_: &Quit, cx| cx.quit());
        bind_keys(cx);
        cx.set_menus(vec![
            Menu {
                name: "Herdr".into(),
                items: vec![
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
                name: "QA".into(),
                items: vec![MenuItem::action(
                    "Show herdr non-detected modal",
                    ShowHerdrNotDetected,
                )],
            },
        ]);
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
        let bounds = Bounds::centered(None, size(px(1200.), px(780.)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(640.), px(400.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Herdr".into()),
                    appears_transparent: cfg!(target_os = "macos"),
                    traffic_light_position: cfg!(target_os = "macos")
                        .then(|| point(px(9.), px(9.))),
                }),
                app_id: Some("so.pen.herdr-gpui".into()),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    HerdrWindow::new(
                        target,
                        window,
                        cx,
                        #[cfg(feature = "integration-test")]
                        {
                            sidebar_test || performance_test
                        },
                    )
                })
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
