use super::HerdrWindow;
use crate::config::Config;
use gpui::{prelude::*, *};

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
}

pub(super) struct MenuState {
    pub page: Option<Page>,
    // Selection epoch and connection generation fence captured modal actions.
    target: (u64, u64),
    pub anchor: Point<Pixels>,
    pub(super) focus: FocusHandle,
    selected: usize,
    keybinds_scroll: ScrollHandle,
    pub(super) keybinds_search: Option<Entity<crate::search_input::SearchInput>>,
    _keybinds_subscription: Option<Subscription>,
    pub(super) preferences_scroll: ScrollHandle,
    pub(super) themes: Option<crate::theme_picker::ThemePicker>,
    pub(super) palette: Option<crate::palette::Palette>,
    pub(super) close: Option<crate::close_modal::CloseConfirmation>,
    pub(super) tab: Option<crate::tab_menu::TabMenu>,
}

/// Mix the theme's blue with foreground so accents remain readable on dark themes.
pub(super) fn accent(theme: &crate::config::Theme) -> Rgba {
    rgb(theme.foreground).blend(rgba((theme.palette[4] << 8) | 0x70))
}

impl MenuState {
    pub fn new(cx: &App) -> Self {
        Self {
            page: None,
            target: (0, 0),
            anchor: Point::default(),
            focus: cx.focus_handle(),
            selected: 0,
            keybinds_scroll: ScrollHandle::new(),
            keybinds_search: None,
            _keybinds_subscription: None,
            preferences_scroll: ScrollHandle::new(),
            themes: None,
            palette: None,
            close: None,
            tab: None,
        }
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
        // Load both before replacing either, so invalid themes preserve the UI.
        match Config::load().and_then(|config| {
            let theme = config.theme()?;
            Ok((config, theme))
        }) {
            Ok((config, theme)) => {
                self.cancel_theme_preview(cx);
                self.config = config;
                self.theme = theme;
                crate::log_window::set_appearance(&self.config, &self.theme, cx);
                self.wheel = Default::default();
                self.last_queued_options = None;
                self.local_error = None;
            }
            Err(error) => self.local_error = Some(format!("Reload GUI config: {error}")),
        }
        self.dismiss_menu(window, cx);
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
        self.menu.tab = None;
        self.menu.target = (
            self.selection_epoch,
            self.endpoints[self.selected_endpoint].generation,
        );
        self.menu.page = Some(Page::Menu);
        self.menu.selected = 0;
        self.marked.clear();
        window.focus(&self.menu.focus);
        cx.notify();
        true
    }

    pub(super) fn dismiss_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.cancel_theme_preview(cx) {
            return;
        }
        self.menu.page = None;
        self.update_preview = None;
        self.menu.close = None;
        self.menu.tab = None;
        window.focus(&self.focus);
        cx.notify();
    }

    pub(super) fn menu_target_current(&self) -> bool {
        self.menu.target
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
        let pointer_anchored = matches!(page, Page::Tab | Page::RenameTab);
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
            .when(pointer_anchored, |panel| {
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
                        .when(index == self.menu.selected, |row| row.bg(rgb(theme.active)))
                        .hover(|row| row.bg(rgb(theme.active)))
                        .child(item)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.activate_menu(item, window, cx);
                        })),
                );
            }
        } else if pointer_anchored {
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
