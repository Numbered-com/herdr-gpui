// objc 0.2's selectors expand a legacy cargo-clippy cfg in the native test adapter.
#![cfg_attr(feature = "integration-test", allow(unexpected_cfgs))]
mod app_icon;
mod close_modal;
mod config;
mod controls;
mod input;
mod menu;
mod palette;
#[cfg(feature = "integration-test")]
mod performance;
mod search_input;
mod sidebar;
#[cfg(feature = "integration-test")]
mod smoke;
mod state;
mod terminal;
mod terminal_painter;
mod theme_picker;

use controls::Command;
use gpui::{prelude::*, *};
use herdr_client::{ClientHandle, ConnectOptions, ConnectTarget, connect, protocol::*};
use state::LiveState;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use terminal::*;

actions!(herdr, [Quit]);

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

struct HerdrWindow {
    config: config::Config,
    theme: config::Theme,
    target: ConnectTarget,
    handle: Option<ClientHandle>,
    inbox: Arc<Mutex<LiveState>>,
    live: LiveState,
    focus: FocusHandle,
    options: ConnectOptions,
    sent_size: Option<ClientSurfaceSize>,
    active: bool,
    sent_focus: Option<bool>,
    bounds: Bounds<Pixels>,
    cell_width: f32,
    painter: std::rc::Rc<std::cell::RefCell<terminal_painter::TerminalPainter>>,
    marked: String,
    local_error: Option<String>,
    menu: menu::MenuState,
    collapsed_repos: std::collections::HashSet<String>,
    sidebar_visible: bool,
    wheel: WheelAccumulator,
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
        let focus = cx.focus_handle();
        window.focus(&focus);
        let timer = cx.background_executor().clone();
        let poll = cx.spawn(async move |this, cx| {
            loop {
                timer.timer(Duration::from_millis(16)).await;
                if this
                    .update(cx, |this, cx| {
                        let next = this.inbox.try_lock().ok().and_then(|mut state| {
                            if !state.dirty {
                                return None;
                            }
                            state.dirty = false;
                            Some(state.clone())
                        });
                        if let Some(next) = next {
                            if this.live.snapshot.as_ref().map(|s| &s.focused_pane_id)
                                != next.snapshot.as_ref().map(|s| &s.focused_pane_id)
                            {
                                this.marked.clear();
                            }
                            this.live = next;
                            cx.notify();
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
            target,
            handle: None,
            inbox: Arc::new(Mutex::new(LiveState::default())),
            live: LiveState::default(),
            focus,
            options: ConnectOptions::default(),
            sent_size: None,
            active: window.is_window_active(),
            sent_focus: None,
            bounds: Bounds::default(),
            cell_width: 9.,
            painter: Default::default(),
            marked: String::new(),
            local_error: None,
            menu: menu::MenuState::new(cx),
            collapsed_repos: Default::default(),
            sidebar_visible: true,
            wheel: WheelAccumulator::default(),
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
            return this;
        }
        this.reconnect();
        if config_error.is_some() {
            this.local_error = config_error;
        }
        this
    }

    fn reconnect(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.disconnect();
        }
        self.live = LiveState::default();
        self.local_error = None;
        self.marked.clear();
        self.sent_size = None;
        self.wheel = WheelAccumulator::default();
        self.sent_focus = None;
        let mut state = LiveState::default();
        state.set_outer_focus(self.active);
        self.inbox = Arc::new(Mutex::new(state));
        match connect(self.target.clone(), self.options) {
            Ok(client) => {
                self.handle = Some(client.handle);
                let inbox = self.inbox.clone();
                // Always drain ordered events, even while GPUI is busy. Only the
                // latest coherent state is retained, never an unbounded UI queue.
                if let Err(error) = std::thread::Builder::new()
                    .name("herdr-gui-events".into())
                    .spawn(move || {
                        while let Ok(event) = client.events.recv() {
                            let Ok(mut state) = inbox.lock() else {
                                break;
                            };
                            state.apply(event);
                        }
                    })
                {
                    self.local_error = Some(error.to_string());
                    if let Some(handle) = self.handle.take() {
                        handle.disconnect();
                    }
                    self.live.status = "Disconnected".into();
                }
            }
            Err(error) => {
                self.live.status = "Disconnected".into();
                self.local_error = Some(error.to_string());
            }
        }
    }

    fn resize(&mut self) {
        if self.sent_size == Some(self.options.surface_size) {
            return;
        }
        if let (Some(handle), Some(snapshot)) = (&self.handle, &self.live.snapshot) {
            match handle.resize(&snapshot.boot_id, self.options) {
                Ok(()) => self.sent_size = Some(self.options.surface_size),
                Err(error) => self.local_error = Some(format!("Resize: {error}")),
            }
        }
    }

    fn report_focus(&mut self) {
        // Update the authoritative event inbox, not just the rendered clone.
        if let Ok(mut state) = self.inbox.try_lock() {
            state.set_outer_focus(self.active);
        }
        if self.sent_focus == Some(self.active) {
            return;
        }
        if let (Some(handle), Some(snapshot)) = (&self.handle, &self.live.snapshot)
            && handle.set_focus(&snapshot.boot_id, self.active).is_ok()
        {
            self.sent_focus = Some(self.active);
        }
    }

    fn send(&mut self, event: ClientPaneInputEvent, cx: &mut Context<Self>) {
        if self.menu.page.is_some() {
            return;
        }
        if let (Some(handle), Some(snapshot), Some(surface)) =
            (&self.handle, &self.live.snapshot, &self.live.surface)
        {
            let result = if let Some(popup) = &surface.popup {
                handle.send_popup_input(&snapshot.boot_id, &popup.terminal_id, vec![event])
            } else if let Some(pane) = &snapshot.focused_pane_id {
                handle.send_input(&snapshot.boot_id, pane, vec![event])
            } else {
                return;
            };
            if let Err(error) = result {
                self.local_error = Some(format!("Input not sent: {error}"));
                cx.notify();
            }
        }
    }

    fn navigate(&mut self, kind: &str, id: &str, cx: &mut Context<Self>) {
        if let (Some(handle), Some(snapshot)) = (&self.handle, &self.live.snapshot) {
            let result = match kind {
                "workspace" => handle.focus_workspace(&snapshot.boot_id, id),
                "tab" => handle.focus_tab(&snapshot.boot_id, id),
                _ => handle.focus_pane(&snapshot.boot_id, id),
            };
            if let Err(error) = result {
                self.local_error = Some(error.to_string());
            }
        }
        self.marked.clear();
        cx.notify();
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
                self.open_menu(window, cx);
                self.menu.page = Some(menu::Page::Preferences);
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
        if let (Some(handle), Some(snapshot)) = (&self.handle, &self.live.snapshot)
            && let Some((method, params)) = controls::request(command, snapshot)
        {
            self.local_error = handle
                .request(&snapshot.boot_id, method, params)
                .err()
                .map(|error| format!("{method}: {error}"));
            self.marked.clear();
        }
        window.focus(&self.focus);
        cx.notify();
    }

    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.menu.page.is_some() {
            return;
        }
        let (Some(handle), Some(snapshot), Some(surface)) =
            (&self.handle, &self.live.snapshot, &self.live.surface)
        else {
            return;
        };
        let x = (event.position.x - self.bounds.origin.x).to_f64() as f32;
        let y = (event.position.y - self.bounds.origin.y).to_f64() as f32;
        let cell_height = self.config.terminal.line_height();
        let Some(target) = wheel_target(surface, x, y, self.cell_width, cell_height) else {
            self.wheel = WheelAccumulator::default();
            return;
        };
        let lines = self
            .wheel
            .lines(&target.id, target.popup, event, cell_height);
        cx.stop_propagation();
        if lines == 0 {
            return;
        }
        let input = target.event(lines, event.modifiers);
        let result = if target.popup {
            handle.send_popup_input(&snapshot.boot_id, &target.id, vec![input])
        } else {
            handle.send_input(&snapshot.boot_id, &target.id, vec![input])
        };
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

impl Drop for HerdrWindow {
    fn drop(&mut self) {
        // Closing this view detaches the client only; never kill a daemon/PTY.
        if let Some(handle) = &self.handle {
            handle.disconnect();
        }
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
        let sidebar = self.render_sidebar(cx);
        let mut tabs = div()
            .id("tabs")
            .flex()
            .flex_none()
            .h(px((self.config.tabs.size * 1.5 + 16.).max(40.)))
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
                tabs = tabs.child(
                    div()
                        .id(SharedString::from(format!("tab-{id}")))
                        .px_4()
                        .py_2()
                        .flex_none()
                        .cursor_pointer()
                        .bg(rgb(if tab.focused {
                            self.theme.active
                        } else {
                            self.theme.surface
                        }))
                        .child(tab.label.clone())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.navigate("tab", &id, cx);
                            window.focus(&this.focus);
                        })),
                );
            }
        }
        let surface = self.live.surface.clone();
        let snapshot = self.live.snapshot.clone();
        let inbox = self.inbox.clone();
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
                            this.navigate("pane", &id, cx);
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
                                let offset = point(
                                    px(((surface.frame.width.saturating_sub(popup.frame.width))
                                        as f32
                                        * cell_width
                                        / 2.)
                                        .floor()),
                                    px((surface.frame.height.saturating_sub(popup.frame.height))
                                        as f32
                                        * cell_height
                                        / 2.),
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
                                cx.defer(move |cx| match inbox.try_lock() {
                                    Ok(mut state) => {
                                        state.acknowledge_presented_surface(
                                            &snapshot, &surface, true,
                                        );
                                    }
                                    Err(std::sync::TryLockError::WouldBlock) => {
                                        paint_entity.update(cx, |_, cx| cx.notify());
                                    }
                                    Err(std::sync::TryLockError::Poisoned(_)) => {}
                                });
                            }
                        }
                    },
                )
                .size_full(),
            );
        let status = self
            .local_error
            .as_ref()
            .or(self.live.error.as_ref())
            .map(|e| format!("{}: {e}", self.live.status))
            .unwrap_or_else(|| self.live.status.clone());
        div()
            .on_action(cx.listener(|this, action: &RunCommand, window, cx| {
                this.command(action.command, window, cx);
            }))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(self.theme.background))
            .text_color(rgb(self.theme.foreground))
            .font_family(self.config.ui.family.clone())
            .text_size(px(self.config.ui.size))
            .child(
                div()
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
                                            .px_4()
                                            .flex()
                                            .items_center()
                                            .cursor_pointer()
                                            .hover(|s| s.bg(rgb(self.theme.active)))
                                            .child("+")
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
                    .child(div().size(px(6.)).flex_none().rounded_full().bg(rgb(
                        if self.live.connected {
                            self.theme.palette[2]
                        } else {
                            self.theme.palette[1]
                        },
                    )))
                    .child(div().flex_1().min_w_0().overflow_hidden().child(status))
                    .when(!self.marked.is_empty(), |d| {
                        d.child(format!("Composing: {}", self.marked))
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
                    ),
            )
            .when(self.menu.page.is_some(), |root| {
                root.child(self.render_menu(window, cx))
            })
    }
}

fn main() -> std::process::ExitCode {
    run();
    #[cfg(feature = "integration-test")]
    return std::process::ExitCode::from(
        smoke::EXIT_CODE.load(std::sync::atomic::Ordering::SeqCst),
    );
    #[cfg(not(feature = "integration-test"))]
    std::process::ExitCode::SUCCESS
}

fn run() {
    let mut socket = None;
    let mut session = None;
    let mut development = false;
    #[cfg(feature = "integration-test")]
    let mut integration_test = false;
    #[cfg(feature = "integration-test")]
    let mut sidebar_test = false;
    #[cfg(feature = "integration-test")]
    let mut performance_test = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            #[cfg(feature = "integration-test")]
            "--performance-test" => performance_test = true,
            #[cfg(feature = "integration-test")]
            "--sidebar-test" => sidebar_test = true,
            #[cfg(feature = "integration-test")]
            "--integration-test" => {
                integration_test = true;
            }
            "--socket" => {
                socket = Some(
                    args.next()
                        .unwrap_or_else(|| usage_error("--socket requires a path")),
                )
            }
            "--session" => {
                session = Some(
                    args.next()
                        .unwrap_or_else(|| usage_error("--session requires a name")),
                )
            }
            "--dev" => development = true,
            "--help" | "-h" => {
                println!(
                    "herdr-gpui [--socket CLIENT_SOCKET | --session NAME [--dev]]\nConnects to an existing local Herdr daemon; never starts or stops it."
                );
                #[cfg(feature = "integration-test")]
                println!(
                    "  --integration-test  Run native GUI checks (requires explicit --socket)\n  --sidebar-test      Run native sidebar fixtures without connecting to a daemon\n  --performance-test  Measure native dense-terminal hover/scroll without a daemon (macOS)"
                );
                return;
            }
            _ => usage_error(&format!("Unknown option: {arg}")),
        }
    }
    if socket.is_some() && (session.is_some() || development) {
        usage_error("--socket cannot be combined with --session or --dev");
    }
    #[cfg(feature = "integration-test")]
    if sidebar_test && performance_test {
        usage_error("--sidebar-test and --performance-test are mutually exclusive");
    }
    #[cfg(all(feature = "integration-test", not(target_os = "macos")))]
    if performance_test {
        usage_error("--performance-test currently requires macOS native event delivery");
    }
    #[cfg(feature = "integration-test")]
    if (sidebar_test || performance_test)
        && (integration_test || socket.is_some() || session.is_some() || development)
    {
        usage_error(
            "fixture tests cannot be combined with connection options or --integration-test",
        );
    }
    #[cfg(feature = "integration-test")]
    if sidebar_test || performance_test {
        smoke::EXIT_CODE.store(1, std::sync::atomic::Ordering::SeqCst);
    }
    #[cfg(feature = "integration-test")]
    if integration_test {
        if socket.is_none() {
            usage_error("--integration-test requires an explicit --socket");
        }
        smoke::EXIT_CODE.store(1, std::sync::atomic::Ordering::SeqCst);
    }
    let target = match (socket, session) {
        (Some(path), _) => ConnectTarget::Socket(path.into()),
        (_, Some(name)) => ConnectTarget::Session { name, development },
        _ if development => ConnectTarget::Session {
            name: "default".into(),
            development,
        },
        _ => ConnectTarget::Local,
    };
    Application::new().run(move |cx| {
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
                    ..Default::default()
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
                #[cfg(feature = "integration-test")]
                if performance_test {
                    std::process::exit(1);
                }
                cx.quit();
            }
        }
        cx.activate(true);
    });
}

fn usage_error(message: &str) -> ! {
    eprintln!("{message}\nUsage: herdr-gpui [--socket CLIENT_SOCKET | --session NAME [--dev]]");
    std::process::exit(2)
}
