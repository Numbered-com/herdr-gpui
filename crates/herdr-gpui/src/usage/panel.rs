//! The usage panel a status bar segment opens: one tab per signed-in agent on
//! the selected host, its account, every limit with its pace, whatever else
//! its service reports, and links to the service's own pages.

use super::{
    Reading,
    model::{Pace, Provider, Section, Severity, Window as Limit, countdown},
    render::ago,
};
use crate::{menu::Page, window::HerdrWindow};
use gpui::{prelude::*, *};
use std::time::{Instant, SystemTime};

pub(crate) const PANEL_WIDTH: f32 = 340.;
/// The gap between the status bar and the panel it opened.
pub(crate) const PANEL_GAP: f32 = 6.;

impl HerdrWindow {
    pub(crate) fn open_usage(
        &mut self,
        provider: Provider,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.open_menu(window, cx) {
            self.menu.anchor = anchor;
            self.menu.page = Some(Page::Usage(provider));
        }
    }

    /// Left and right step through the tabs, as the pointer does.
    pub(crate) fn usage_key(
        &mut self,
        provider: Provider,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        let providers: Vec<_> = self
            .usage
            .current()
            .map(|entry| entry.readings.iter().map(|r| r.provider).collect())
            .unwrap_or_default();
        let Some(index) = providers.iter().position(|p| *p == provider) else {
            return false;
        };
        let count = providers.len();
        let next = match event.keystroke.key.as_str() {
            "left" => (index + count - 1) % count,
            "right" => (index + 1) % count,
            _ => return false,
        };
        self.menu.page = Some(Page::Usage(providers[next]));
        cx.notify();
        true
    }

    pub(crate) fn render_usage_panel(
        &self,
        provider: Provider,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = &self.theme;
        let font = &self.config.ui;
        let small = px(font.size * 0.9);
        let now = SystemTime::now();
        let entry = self.usage.current();
        let readings = entry.map_or(&[][..], |entry| &entry.readings[..]);
        let reading = readings.iter().find(|r| r.provider == provider);
        let rule = || div().h(px(1.)).my(px(6.)).bg(rgb(theme.active));
        let mut view = div()
            .id("usage-panel")
            .debug_selector(|| "usage-panel".into())
            .flex()
            .flex_col()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.menu.usage_scroll)
            .px(px(6.))
            .py(px(4.));
        let service = provider.service();
        let account = reading
            .and_then(|r| r.report.as_ref())
            .map(|report| report.account.clone())
            .unwrap_or_default();
        let updated = entry
            .and_then(|entry| entry.updated)
            .map(|at| {
                format!(
                    "Updated {} ago",
                    ago(now.duration_since(at).unwrap_or_default())
                )
            })
            .unwrap_or_else(|| {
                if self.usage.busy() {
                    "Updating…".into()
                } else {
                    "Not updated yet".into()
                }
            });
        let host = self
            .endpoints
            .get(self.selected_endpoint)
            .map(|endpoint| endpoint.label.clone())
            .unwrap_or_default();
        view = view.child(
            div()
                .debug_selector(|| "usage-panel-header".into())
                .px(px(8.))
                .py(px(6.))
                .flex()
                .justify_between()
                .gap(px(12.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(px(font.size * 1.25))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(service.name()),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(small)
                                .text_color(rgb(theme.muted))
                                .child(format!("{updated} · {host}")),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .max_w(px(PANEL_WIDTH * 0.6))
                        .flex()
                        .flex_col()
                        .items_end()
                        .text_color(rgb(theme.muted))
                        .children(
                            account
                                .email
                                .map(|email| div().max_w_full().truncate().child(email)),
                        )
                        .children(account.plan.map(|plan| div().text_size(small).child(plan))),
                ),
        );
        let errors = reading
            .and_then(|r| r.error.clone())
            .into_iter()
            .chain(entry.and_then(|entry| entry.error.clone()));
        for error in errors {
            view = view.child(
                div()
                    .px(px(8.))
                    .pb(px(4.))
                    .text_size(small)
                    .text_color(rgb(theme.palette[3]))
                    .child(error),
            );
        }
        if let Some(report) = reading.and_then(|r| r.report.as_ref()) {
            view = view.child(rule());
            for limit in &report.windows {
                view = view.child(self.usage_limit(limit, now));
            }
            for section in &report.sections {
                view = view.child(rule()).child(self.usage_section(section, now));
            }
        } else if reading.is_none() {
            view = view.child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .text_color(rgb(theme.muted))
                    .child(format!("{} is not signed in on {host}.", service.name())),
            );
        }
        let body = view
            .child(rule())
            .child(self.usage_action(
                "usage-refresh-row",
                "icons/refresh.svg",
                "Refresh",
                cx.listener(|this, _, _, cx| {
                    this.usage.refresh(Instant::now());
                    cx.notify();
                }),
            ))
            .child(self.usage_action(
                "usage-dashboard",
                "icons/chart.svg",
                "Usage Dashboard",
                move |_, _, cx| cx.open_url(service.dashboard()),
            ))
            .child(self.usage_action(
                "usage-status",
                "icons/pulse.svg",
                "Status Page",
                move |_, _, cx| cx.open_url(service.status_page()),
            ));
        // The tabs sit on the panel's bottom edge, beside the status bar it
        // rises from: switching agents changes the panel's height above them,
        // so they never move under the pointer.
        div()
            .flex()
            .flex_col()
            .min_h_0()
            .child(body.flex_shrink())
            .when(readings.len() > 1, |panel| {
                panel.child(
                    div()
                        .flex_none()
                        .px(px(6.))
                        .pb(px(6.))
                        .child(div().h(px(1.)).mb(px(6.)).bg(rgb(theme.active)))
                        .child(self.usage_tabs(readings, provider, cx)),
                )
            })
    }

    fn usage_tabs(
        &self,
        readings: &[Reading],
        selected: Provider,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = &self.theme;
        div()
            .flex()
            .gap(px(4.))
            .children(readings.iter().map(|reading| {
                let provider = reading.provider;
                let chosen = provider == selected;
                let (background, text) = if chosen {
                    let wash = theme.primary_wash();
                    (Some(wash), theme.text_on(wash))
                } else {
                    (None, theme.muted)
                };
                let key = provider.key();
                div()
                    .id(SharedString::from(format!("usage-tab-{key}")))
                    .debug_selector(move || format!("usage-tab-{key}"))
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(2.))
                    .py(px(6.))
                    .rounded(px(crate::config::corners::CONTROL))
                    .cursor_pointer()
                    .text_color(rgb(text))
                    .when_some(background, |tab, background| tab.bg(rgb(background)))
                    .when(!chosen, |tab| tab.hover(|s| s.bg(rgb(theme.active))))
                    .child(
                        svg()
                            .path(provider.icon().path())
                            .size(px(16.))
                            .text_color(rgb(text)),
                    )
                    .child(provider.name())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.menu.page = Some(Page::Usage(provider));
                        cx.notify();
                    }))
            }))
    }

    /// `Weekly 89% left`, its reset, a bar of what is left with a tick where
    /// an even spend would be, and whether the rest lasts.
    fn usage_limit(&self, limit: &Limit, now: SystemTime) -> impl IntoElement {
        let theme = &self.theme;
        let small = px(self.config.ui.size * 0.9);
        let pace = limit.pace(now);
        div()
            .px(px(8.))
            .py(px(6.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .gap(px(8.))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(format!(
                        "{} {}% left",
                        limit.kind.title(),
                        limit.left()
                    )))
                    .children(limit.resets_in(now).map(|left| {
                        div()
                            .flex_none()
                            .text_size(small)
                            .text_color(rgb(theme.muted))
                            .child(format!("Resets in {}", countdown(left)))
                    })),
            )
            .child(bar(100. - limit.used, limit.used, pace, theme))
            .children(pace.map(|pace| {
                div()
                    .text_size(small)
                    .text_color(rgb(theme.muted))
                    .child(pace.describe(limit.used))
            }))
    }

    fn usage_section(&self, section: &Section, now: SystemTime) -> AnyElement {
        let theme = &self.theme;
        let small = px(self.config.ui.size * 0.9);
        let heading = |title: &str| {
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_owned())
        };
        match section {
            Section::Limit(limit) => self.usage_limit(limit, now).into_any_element(),
            Section::Facts { title, facts } => div()
                .px(px(8.))
                .py(px(6.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(heading(title))
                .children(facts.iter().map(|(label, value)| {
                    div()
                        .flex()
                        .justify_between()
                        .gap(px(8.))
                        .text_size(small)
                        .child(div().text_color(rgb(theme.muted)).child(label.clone()))
                        .child(div().child(value.clone()))
                }))
                .into_any_element(),
            Section::Shares { title, shares } => div()
                .px(px(8.))
                .py(px(6.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(heading(title))
                .children(shares.iter().map(|(label, share)| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(small)
                        .child(
                            div()
                                .w(px(96.))
                                .flex_none()
                                .truncate()
                                .text_color(rgb(theme.muted))
                                .child(label.clone()),
                        )
                        .child(div().flex_1().child(bar(*share, 0., None, theme)))
                        .child(
                            div()
                                .w(px(36.))
                                .flex_none()
                                .flex()
                                .justify_end()
                                .child(format!("{}%", share.round())),
                        )
                }))
                .into_any_element(),
        }
    }

    fn usage_action(
        &self,
        id: &'static str,
        icon: &'static str,
        label: &'static str,
        action: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        let theme = &self.theme;
        div()
            .id(id)
            .debug_selector(move || id.into())
            .px(px(8.))
            .py(px(6.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(crate::config::corners::CONTROL))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(theme.active)))
            .child(
                svg()
                    .path(icon)
                    .size(px(14.))
                    .flex_none()
                    .text_color(rgb(theme.foreground)),
            )
            .child(label)
            .on_click(move |event, window, cx| {
                cx.stop_propagation();
                action(event, window, cx);
            })
    }
}

/// A full-width bar filled to `fill` percent, colored by how much of the
/// limit is `used`, with a tick where an even spend would have left it.
fn bar(fill: f32, used: f32, pace: Option<Pace>, theme: &crate::config::Theme) -> impl IntoElement {
    let color = match Severity::from(used) {
        Severity::Normal => crate::menu::accent(theme),
        Severity::Warning => rgb(theme.palette[3]),
        Severity::Critical => rgb(theme.palette[1]),
    };
    div()
        .relative()
        .h(px(6.))
        .w_full()
        .rounded_full()
        .bg(rgb(theme.active))
        .child(
            div()
                .h_full()
                .w(relative(fill.clamp(0., 100.) / 100.))
                .rounded_full()
                .bg(color),
        )
        .children(pace.map(|pace| {
            div()
                .absolute()
                .top(px(-2.))
                .h(px(10.))
                .w(px(2.))
                .left(relative((100. - pace.expected).clamp(0., 100.) / 100.))
                .bg(rgb(theme.foreground))
        }))
}
