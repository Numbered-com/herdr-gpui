use super::{HerdrWindow, LiveState};
use crate::config::Config;
use gpui::{prelude::*, *};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Page {
    Menu,
    Preferences,
    Keybinds,
    Themes,
    Update,
}

pub(super) struct MenuState {
    pub page: Option<Page>,
    pub anchor: Point<Pixels>,
    focus: FocusHandle,
    selected: usize,
    keybinds_scroll: ScrollHandle,
    pub(super) themes: Option<crate::theme_picker::ThemePicker>,
}

impl MenuState {
    pub fn new(cx: &App) -> Self {
        Self {
            page: None,
            anchor: Point::default(),
            focus: cx.focus_handle(),
            selected: 0,
            keybinds_scroll: ScrollHandle::new(),
            themes: None,
        }
    }
}

impl HerdrWindow {
    pub(super) fn open_keybinds(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_menu(window, cx);
        self.menu.page = Some(Page::Keybinds);
        self.menu.keybinds_scroll.set_offset(Point::default());
    }

    pub(super) fn open_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu.page = Some(Page::Menu);
        self.menu.selected = 0;
        self.marked.clear();
        window.focus(&self.menu.focus);
        cx.notify();
    }

    pub(super) fn dismiss_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu.page = None;
        window.focus(&self.focus);
        cx.notify();
    }

    fn menu_items(&self) -> Vec<&'static str> {
        let mut items = vec!["settings", "keybinds", "themes", "reload GUI config"];
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
            "themes" => self.open_theme_picker(window, cx),
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
            .when(!matches!(page, Page::Keybinds | Page::Themes), |panel| {
                panel.overflow_y_scroll().p(px(6.))
            })
            .when(matches!(page, Page::Keybinds | Page::Themes), |panel| {
                panel
                    .flex()
                    .flex_col()
                    .h(px(560. * (font.size / 12.)).min((viewport.height - px(32.)).max(px(0.))))
                    .overflow_hidden()
                    .shadow_lg()
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
        } else if page == Page::Keybinds {
            panel = panel.child(self.render_keybinds(cx));
        } else if page == Page::Themes {
            panel = panel.child(self.render_theme_picker(cx));
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
                if this.menu.page == Some(Page::Themes) {
                    this.theme_picker_key(event, window, cx);
                    return;
                }
                cx.stop_propagation();
                window.prevent_default();
                match event.keystroke.key.as_str() {
                    "escape" => this.dismiss_menu(window, cx),
                    "up" | "down" | "pageup" | "pagedown"
                        if this.menu.page == Some(Page::Keybinds) =>
                    {
                        let scroll = &this.menu.keybinds_scroll;
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

    fn render_keybinds(&self, cx: &mut Context<Self>) -> Div {
        let theme = &self.theme;
        let font = &self.config.ui;
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
        for (section, shortcuts) in [
            (
                "WORKSPACES & PANES",
                vec![
                    ("Cmd N", "New workspace"),
                    ("Cmd T", "New tab"),
                    ("Cmd D", "Split right"),
                    ("Cmd Shift D", "Split down"),
                ],
            ),
            (
                "NAVIGATION",
                vec![("Cmd Shift ]", "Next tab"), ("Cmd Shift [", "Previous tab")],
            ),
            (
                "APPLICATION",
                vec![
                    ("Cmd V", "Paste into terminal"),
                    ("Cmd /", "Show keybinds"),
                    ("Cmd Q", "Quit GUI; daemon stays running"),
                ],
            ),
        ] {
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
                                .children(keys.split_whitespace().map(|key| {
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
                                        .child(key.to_owned())
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
