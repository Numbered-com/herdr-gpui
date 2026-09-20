// objc 0.2's selectors expand a legacy cargo-clippy cfg in the native test adapter.
#![cfg_attr(feature = "integration-test", allow(unexpected_cfgs))]
mod app_icon;
mod controls;
mod endpoint;
mod input;
mod menu;
#[cfg(feature = "integration-test")]
mod performance;
mod sidebar;
#[cfg(feature = "integration-test")]
mod smoke;
mod state;
mod terminal;
mod terminal_painter;

use controls::Command;
use gpui::{prelude::*, *};
use herdr_client::{ClientHandle, ConnectOptions, ConnectTarget, protocol::*};
use state::LiveState;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use terminal::*;

actions!(
    herdr,
    [
        Quit,
        Reconnect,
        NewWorkspace,
        NewTab,
        SplitRight,
        SplitDown,
        NextTab,
        PreviousTab
    ]
);

struct HerdrWindow {
    endpoints: Vec<endpoint::Endpoint>,
    selected_endpoint: usize,
    selection_epoch: u64,
    catalog: endpoint::Catalog,
    activation_deadline: Option<std::time::Instant>,
    pending_navigation: Option<(String, String)>,
    pending_releases: Vec<endpoint::Release>,
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
                        let old_pane = this
                            .live
                            .snapshot
                            .as_ref()
                            .and_then(|s| s.focused_pane_id.clone());
                        this.poll_endpoints(cx);
                        if old_pane
                            != this
                                .live
                                .snapshot
                                .as_ref()
                                .and_then(|s| s.focused_pane_id.clone())
                        {
                            this.marked.clear();
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
        let mut this = Self {
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
        this
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
        let focused = self.active && self.endpoints[self.selected_endpoint].surface_requested();
        // Update the authoritative event inbox, not just the rendered clone.
        if let Ok(mut state) = self.inbox.try_lock() {
            state.set_outer_focus(self.active && self.input_ready());
        }
        if self.sent_focus == Some(focused) {
            return;
        }
        if let (Some(handle), Some(snapshot)) = (&self.handle, &self.live.snapshot)
            && handle.set_focus(&snapshot.boot_id, focused).is_ok()
        {
            self.sent_focus = Some(focused);
        }
    }

    fn send(&mut self, event: ClientPaneInputEvent, cx: &mut Context<Self>) {
        if self.menu.page.is_some() || !self.input_ready() {
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
        if !self.input_ready() {
            return;
        }
        let (method, params) = match kind {
            "workspace" => ("workspace.focus", serde_json::json!({"workspace_id": id})),
            "tab" => ("tab.focus", serde_json::json!({"tab_id": id})),
            _ => ("pane.focus", serde_json::json!({"pane_id": id})),
        };
        self.request_focus_change(method, params, Some((kind.into(), id.into())));
        self.marked.clear();
        cx.notify();
    }

    fn request_focus_change(
        &mut self,
        method: &str,
        params: serde_json::Value,
        focus: Option<(String, String)>,
    ) {
        if let (Some(handle), Some(snapshot)) = (&self.handle, &self.live.snapshot)
            && let Ok(mut state) = self.inbox.lock()
        {
            if let Err(error) = handle.request(&snapshot.boot_id, method, params) {
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
        if self.menu.page.is_some()
            || self.activation_deadline.is_some()
            || !self.endpoints[self.selected_endpoint].surface_requested()
        {
            return;
        }
        #[cfg(feature = "integration-test")]
        {
            self.input_probe.actions += 1;
        }
        if let Some(snapshot) = &self.live.snapshot
            && let Some((method, params)) = controls::request(command, snapshot)
        {
            self.request_focus_change(method, params, None);
            self.marked.clear();
        }
        window.focus(&self.focus);
        cx.notify();
    }

    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.menu.page.is_some() || !self.input_ready() {
            return;
        }
        let (Some(handle), Some(snapshot), Some(surface)) =
            (&self.handle, &self.live.snapshot, &self.live.surface)
        else {
            return;
        };
        let x = (event.position.x - self.bounds.origin.x).to_f64() as f32;
        let y = (event.position.y - self.bounds.origin.y).to_f64() as f32;
        let Some(target) = wheel_target(surface, x, y, self.cell_width) else {
            self.wheel = WheelAccumulator::default();
            return;
        };
        let lines = self.wheel.lines(&target.id, target.popup, event);
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
        let font = font("Menlo");
        self.cell_width = self.painter.borrow_mut().cell_width(&font, window, cx);
        let sidebar = self.render_sidebar(cx);
        let mut tabs = div()
            .id("tabs")
            .flex()
            .flex_none()
            .h(px(40.))
            .overflow_x_scroll()
            .bg(rgb(sidebar::BACKGROUND))
            .text_color(rgb(sidebar::FOREGROUND))
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
                            sidebar::ACTIVE
                        } else {
                            sidebar::BACKGROUND
                        }))
                        .child(tab.label.clone())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.navigate("tab", &id, cx);
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
        let inbox = self.inbox.clone();
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
            .bg(rgb(BACKGROUND))
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
                            / CELL_HEIGHT as f64)
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
                                ),
                                cell_width_px: cell_width.round().max(1.) as u32,
                                cell_height_px: CELL_HEIGHT as u32,
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
                                        * CELL_HEIGHT
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
        let status = self
            .local_error
            .as_ref()
            .or(self.live.error.as_ref())
            .map(|e| format!("{}: {e}", self.live.status))
            .unwrap_or_else(|| self.live.status.clone());
        div()
            .on_action(cx.listener(|this, _: &Reconnect, window, cx| {
                if this.menu.page.is_some() {
                    return;
                }
                this.reconnect();
                window.focus(&this.focus);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &NewWorkspace, window, cx| {
                this.command(Command::Workspace, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &NewTab, window, cx| this.command(Command::Tab, window, cx)),
            )
            .on_action(cx.listener(|this, _: &SplitRight, window, cx| {
                this.command(Command::SplitRight, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SplitDown, window, cx| {
                this.command(Command::SplitDown, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| {
                this.command(Command::NextTab, window, cx)
            }))
            .on_action(cx.listener(|this, _: &PreviousTab, window, cx| {
                this.command(Command::PreviousTab, window, cx)
            }))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(FOREGROUND))
            .font_family(".SystemUIFont")
            .text_sm()
            .child(
                div().flex().flex_1().min_h_0().child(sidebar).child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .flex()
                                .flex_none()
                                .bg(rgb(sidebar::BACKGROUND))
                                .text_color(rgb(sidebar::FOREGROUND))
                                .child(tabs.flex_1().min_w_0())
                                .child(
                                    div()
                                        .id("new-tab")
                                        .px_4()
                                        .flex()
                                        .items_center()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgb(sidebar::ACTIVE)))
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
                    .h(px(22.))
                    .overflow_hidden()
                    .items_center()
                    .gap(px(6.))
                    .px_3()
                    .bg(rgb(sidebar::BACKGROUND))
                    .text_color(rgb(sidebar::FOREGROUND))
                    .child(div().size(px(6.)).flex_none().rounded_full().bg(rgb(
                        if self.live.connected {
                            0x78c998
                        } else {
                            0xe27c7c
                        },
                    )))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_xs()
                            .child(status),
                    )
                    .when(!self.marked.is_empty(), |d| {
                        d.child(format!("Composing: {}", self.marked))
                    }),
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
                    "herdr-gpui [--socket CLIENT_SOCKET | --session NAME [--dev]]\nConnects to Local and saved SSH hosts; never installs remote software.\n--socket isolates the GUI to one existing daemon."
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
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-n", NewWorkspace, None),
            KeyBinding::new("cmd-t", NewTab, None),
            KeyBinding::new("cmd-d", SplitRight, None),
            KeyBinding::new("cmd-shift-d", SplitDown, None),
            KeyBinding::new("cmd-shift-]", NextTab, None),
            KeyBinding::new("cmd-shift-[", PreviousTab, None),
        ]);
        cx.set_menus(vec![
            Menu {
                name: "Herdr".into(),
                items: vec![MenuItem::action("Quit Herdr", Quit)],
            },
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("New Workspace", NewWorkspace),
                    MenuItem::action("New Tab", NewTab),
                ],
            },
            Menu {
                name: "Terminal".into(),
                items: vec![
                    MenuItem::action("Split Vertically (Right)", SplitRight),
                    MenuItem::action("Split Horizontally (Down)", SplitDown),
                    MenuItem::separator(),
                    MenuItem::action("Next Tab", NextTab),
                    MenuItem::action("Previous Tab", PreviousTab),
                    MenuItem::separator(),
                    MenuItem::action("Reconnect", Reconnect),
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
