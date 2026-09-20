use super::{HerdrWindow, LiveState};
use crate::config::Config;
use gpui::{prelude::*, *};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Page {
    Menu,
    Preferences,
    Keybinds,
    Update,
}

pub(super) struct MenuState {
    pub page: Option<Page>,
    pub anchor: Point<Pixels>,
    focus: FocusHandle,
    selected: usize,
}

impl MenuState {
    pub fn new(cx: &App) -> Self {
        Self {
            page: None,
            anchor: Point::default(),
            focus: cx.focus_handle(),
            selected: 0,
        }
    }
}

impl HerdrWindow {
    pub(super) fn open_keybinds(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_menu(window, cx);
        self.menu.page = Some(Page::Keybinds);
    }

    pub(super) fn open_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu.page = Some(Page::Menu);
        self.menu.selected = 0;
        self.marked.clear();
        window.focus(&self.menu.focus);
        cx.notify();
    }

    fn dismiss_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu.page = None;
        window.focus(&self.focus);
        cx.notify();
    }

    fn menu_items(&self) -> Vec<&'static str> {
        let mut items = vec!["settings", "keybinds", "reload GUI config"];
        if self.live.connected {
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
        items.push(if self.handle.is_some() {
            "detach"
        } else {
            "reconnect"
        });
        items
    }

    fn activate_menu(&mut self, item: &str, window: &mut Window, cx: &mut Context<Self>) {
        match item {
            "settings" => self.menu.page = Some(Page::Preferences),
            "keybinds" => self.open_keybinds(window, cx),
            "update ready" => self.menu.page = Some(Page::Update),
            "reload GUI config" => {
                // Load both before replacing either, so invalid themes preserve the UI.
                match Config::load().and_then(|config| {
                    let theme = config.theme()?;
                    Ok((config, theme))
                }) {
                    Ok((config, theme)) => {
                        self.config = config;
                        self.theme = theme;
                        self.wheel = Default::default();
                        self.sent_size = None;
                        self.local_error = None;
                    }
                    Err(error) => self.local_error = Some(format!("Reload GUI config: {error}")),
                }
                self.dismiss_menu(window, cx);
            }
            "reload daemon config" => {
                if let (Some(handle), Some(snapshot)) = (&self.handle, &self.live.snapshot) {
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
                if let Some(handle) = self.handle.take() {
                    handle.disconnect();
                }
                // Isolate any final events from the detached connection.
                self.live = LiveState::default();
                self.live.status = "Detached (daemon still running)".into();
                self.live.set_outer_focus(self.active);
                self.inbox = Arc::new(Mutex::new(self.live.clone()));
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
        let font = &self.config.ui;
        let theme = &self.theme;
        let viewport = window.viewport_size();
        let mut panel = div()
            .id("menu-panel")
            .debug_selector(|| "menu-panel".into())
            .when(page == Page::Menu, |panel| {
                panel
                    .absolute()
                    .left(px(56.))
                    .bottom((viewport.height - self.menu.anchor.y + px(12.)).max(px(30.)))
                    .w(px(180.))
                    .max_h((viewport.height / 2. - px(12.)).max(px(0.)))
            })
            .when(page != Page::Menu, |panel| {
                panel
                    .w((viewport.width - px(32.)).max(px(0.)).min(px(480.)))
                    .max_h((viewport.height - px(32.)).max(px(0.)))
            })
            .overflow_y_scroll()
            .p(px(6.))
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
                        .when(index == self.menu.selected, |row| row.bg(rgb(theme.active)))
                        .hover(|row| row.bg(rgb(theme.active)))
                        .child(item)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.activate_menu(item, window, cx);
                        })),
                );
            }
        } else {
            let (title, rows) = match page {
                Page::Preferences => ("Preferences (read-only)", vec![
                    format!("Connection: {}", self.live.status),
                    format!("Target: {:?}", self.target),
                    format!("GUI config: {}", Config::path().map(|path| path.display().to_string()).unwrap_or_else(|error| format!("unavailable ({error})"))),
                    format!("Theme: {}", self.config.theme),
                    format!("Terminal font: {}, {} px", self.config.terminal.family, self.config.terminal.size),
                    format!("Sidebar font: {}, {} px", self.config.sidebar.family, self.config.sidebar.size),
                    format!("Tabs font: {}, {} px", self.config.tabs.family, self.config.tabs.size),
                    format!("UI font: {}, {} px", font.family, font.size),
                    "Edit the GUI config file to change theme and sidebar, tabs, terminal, or ui fonts (family and size), then choose reload GUI config. Invalid configuration leaves the current appearance unchanged.".into(),
                    "Reload daemon config is separate and asks the connected daemon to reload its own configuration.".into(),
                ]),
                Page::Keybinds => ("Native keybinds", vec![
                    "Cmd-N   New workspace".into(), "Cmd-T   New tab".into(),
                    "Cmd-D   Split right".into(), "Cmd-Shift-D   Split down".into(),
                    "Cmd-Shift-] / [   Next / previous tab".into(),
                    "Cmd-V   Paste into terminal".into(), "Cmd-Q   Quit GUI (daemon stays running)".into(),
                    "Cmd-/   Show native keybinds".into(),
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
            .when(page != Page::Menu, |overlay| {
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
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                cx.stop_propagation();
                window.prevent_default();
                match event.keystroke.key.as_str() {
                    "escape" => this.dismiss_menu(window, cx),
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
            .child(panel)
    }
}
