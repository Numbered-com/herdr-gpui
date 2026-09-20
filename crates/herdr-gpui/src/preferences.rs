use crate::{HerdrWindow, config::Config};
use gpui::{prelude::*, *};

impl HerdrWindow {
    pub(super) fn render_preferences(&self, cx: &mut Context<Self>) -> Div {
        let theme = &self.theme;
        let font = &self.config.ui;
        let accent = rgb(theme.foreground).blend(rgba((theme.palette[4] << 8) | 0x70));
        let section = |title: &'static str| {
            div()
                .pt(px(12.))
                .pb(px(6.))
                .text_size(px(font.size * 0.85))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(accent)
                .child(title)
        };
        let row = |id: &'static str, label: &'static str, value: String| {
            div()
                .debug_selector(move || id.into())
                .flex()
                .min_w_0()
                .gap(px(12.))
                .py(px(7.))
                .border_b_1()
                .border_color(rgb(theme.active))
                .child(
                    div()
                        .w(relative(0.3))
                        .flex_none()
                        .min_w_0()
                        .text_color(rgb(theme.muted))
                        .child(label),
                )
                .child(div().flex_1().min_w_0().text_right().child(value))
        };
        let note = |text: &'static str| {
            div()
                .min_w_0()
                .py(px(10.))
                .text_color(rgb(theme.muted))
                .child(text)
        };
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .min_w_0()
                .px(px(8.))
                .py(px(6.))
                .rounded(px(4.))
                .border_1()
                .border_color(rgb(theme.active))
                .bg(rgb(theme.background))
                .text_color(accent)
                .cursor_pointer()
                .hover(|style| style.bg(rgb(theme.active)))
                .child(label)
        };
        let mut body = div()
            .id("preferences-body")
            .debug_selector(|| "preferences-body".into())
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .track_scroll(&self.menu.preferences_scroll)
            .px(px(16.))
            .py(px(8.))
            .child(section("APPEARANCE"))
            .child(row("preferences-theme", "Theme", self.config.theme.clone()))
            .child(div().py(px(10.)).child(
                button("preferences-choose-theme", "Choose theme").on_click(cx.listener(
                    |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_theme_picker(window, cx);
                    },
                )),
            ))
            .child(section("FONTS"));
        for (id, label, value) in [
            ("preferences-font-sidebar", "Sidebar", &self.config.sidebar),
            ("preferences-font-tabs", "Tabs", &self.config.tabs),
            (
                "preferences-font-terminal",
                "Terminal",
                &self.config.terminal,
            ),
            ("preferences-font-ui", "UI", &self.config.ui),
        ] {
            body = body.child(row(
                id,
                label,
                format!("{}, {} px", value.family, value.size),
            ));
        }
        body = body
            .child(note(
                "Font families and sizes are read-only here. Sizes are logical pixels, independent of display scaling.",
            ))
            .child(section("CONFIGURATION"))
            .child(
                div()
                    .text_color(rgb(theme.muted))
                    .py(px(7.))
                    .child("GUI config file"),
            )
            .child(
                div()
                    .debug_selector(|| "preferences-config-path".into())
                    .w_full()
                    .min_w_0()
                    .p(px(10.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(rgb(theme.active))
                    .bg(rgb(theme.background))
                    .child(
                        Config::path()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|error| format!("Unavailable ({error})")),
                    ),
            )
            .child(note(
                "Edit the GUI config file to change theme or font family and size, then reload GUI config. Invalid configuration leaves the current appearance unchanged.",
            ))
            .child(
                button("preferences-reload-config", "Reload GUI config").on_click(cx.listener(
                    |this, _, window, cx| {
                        cx.stop_propagation();
                        this.reload_gui_config(window, cx);
                    },
                )),
            )
            .child(note(
                "Daemon configuration is separate. Reloading GUI config does not reload daemon settings.",
            ))
            .child(section("CONNECTION"))
            .child(row(
                "preferences-connection-status",
                "Status",
                self.live.status_text(self.local_error.as_deref()),
            ))
            .child(row(
                "preferences-connection-target",
                "Target",
                format!("{:?}", self.connection.target),
            ));

        div()
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .min_w_0()
            .font_family(font.family.clone())
            .text_size(px(font.size))
            .line_height(px(font.line_height()))
            .text_color(rgb(theme.foreground))
            .child(
                div()
                    .debug_selector(|| "preferences-header".into())
                    .flex()
                    .items_center()
                    .flex_none()
                    .gap(px(12.))
                    .p(px(16.))
                    .border_b_1()
                    .border_color(rgb(theme.active))
                    .child(
                        div()
                            .flex_none()
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
                                    .child("Preferences"),
                            )
                            .child(
                                div()
                                    .text_color(rgb(theme.muted))
                                    .child("Current GUI settings"),
                            ),
                    )
                    .child(
                        div()
                            .id("preferences-close")
                            .debug_selector(|| "preferences-close".into())
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
                            .on_click(cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.dismiss_menu(window, cx);
                            })),
                    ),
            )
            .child(body)
            .child(
                div()
                    .debug_selector(|| "preferences-footer".into())
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
