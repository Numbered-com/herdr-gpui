//! The status bar's usage segments: per agent, a meter for the window closest
//! to its limit and each window's share used with its time to reset. Details
//! live in a tooltip, so the bar stays one quiet line.

use super::{
    Reading,
    model::{Severity, Window as Limit},
};
use crate::window::HerdrWindow;
use gpui::{prelude::*, *};
use std::time::{Duration, SystemTime};

const METER_WIDTH: f32 = 40.;

impl HerdrWindow {
    /// Nothing when usage is hidden or no agent on the host is signed in.
    pub(crate) fn render_usage(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let entry = self.usage.current();
        let readings = entry.map_or(&[][..], |entry| &entry.readings[..]);
        let host_error = entry.and_then(|entry| entry.error.clone());
        let busy = self.usage.busy();
        if !self.config.show_usage || (readings.is_empty() && host_error.is_none() && !busy) {
            return None;
        }
        let now = SystemTime::now();
        let host = self
            .endpoints
            .get(self.selected_endpoint)
            .map(|endpoint| endpoint.label.clone())
            .unwrap_or_default();
        let updated = entry.and_then(|entry| entry.updated).map(|at| {
            format!(
                "Updated {} ago",
                ago(now.duration_since(at).unwrap_or_default())
            )
        });
        let theme = &self.theme;
        let mut row = div()
            .id("usage")
            .debug_selector(|| "usage".into())
            .flex()
            .flex_shrink()
            .min_w_0()
            .overflow_hidden()
            .items_center()
            .gap(px(12.));
        for reading in readings {
            let mut lines = vec![SharedString::from(
                match reading
                    .report
                    .as_ref()
                    .and_then(|report| report.plan.as_deref())
                {
                    Some(plan) => format!("{} · {plan} on {host}", reading.provider.name()),
                    None => format!("{} on {host}", reading.provider.name()),
                },
            )];
            if let Some(report) = &reading.report {
                lines.extend(
                    report
                        .windows
                        .iter()
                        .map(|window| window.detail(now).into()),
                );
            }
            lines.extend(reading.error.iter().map(|error| error.clone().into()));
            lines.extend(updated.iter().map(|updated| updated.clone().into()));
            row = row.child(self.usage_segment(reading, now, lines));
        }
        if let Some(error) = host_error.filter(|_| readings.is_empty()) {
            row = row.child(
                div()
                    .id("usage-host-error")
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(theme.muted))
                    .child("Usage unavailable")
                    .tooltip(hint(vec![error.into()], theme)),
            );
        }
        let refresh = svg()
            .path("icons/refresh.svg")
            .size(px(11.))
            .text_color(rgb(theme.muted));
        Some(
            row.child(
                div()
                    .id("usage-refresh")
                    .debug_selector(|| "usage-refresh".into())
                    .size(px(18.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(crate::config::corners::CONTROL))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(theme.active)))
                    .child(if busy {
                        refresh
                            .with_animation(
                                "usage-refreshing",
                                Animation::new(Duration::from_secs(1)).repeat(),
                                |icon, delta| {
                                    icon.with_transformation(Transformation::rotate(percentage(
                                        delta,
                                    )))
                                },
                            )
                            .into_any_element()
                    } else {
                        refresh.into_any_element()
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.usage.refresh(std::time::Instant::now());
                        cx.notify();
                    })),
            ),
        )
    }

    fn usage_segment(
        &self,
        reading: &Reading,
        now: SystemTime,
        lines: Vec<SharedString>,
    ) -> impl IntoElement {
        let theme = &self.theme;
        let key = reading.provider.key();
        let mut segment = div()
            .id(SharedString::from(format!("usage-{key}")))
            .debug_selector(move || format!("usage-{key}"))
            .flex()
            .flex_shrink()
            .min_w_0()
            .overflow_hidden()
            .items_center()
            .gap(px(6.))
            .tooltip(hint(lines, theme))
            .child(
                svg()
                    .path(reading.provider.icon().path())
                    .size(px(12.))
                    .flex_none()
                    .text_color(rgb(theme.foreground)),
            );
        let Some(report) = &reading.report else {
            // Signed in, but never read successfully.
            return segment.child(
                div()
                    .whitespace_nowrap()
                    .text_color(rgb(theme.muted))
                    .child("--"),
            );
        };
        if let Some(tightest) = report.tightest() {
            segment = segment.child(meter(tightest, theme));
        }
        let mut labels = div()
            .flex()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .gap(px(4.));
        for (index, window) in report.windows.iter().enumerate() {
            if index > 0 {
                labels = labels.child(div().text_color(rgb(theme.muted)).child("·"));
            }
            labels = labels.child(
                div()
                    .text_color(rgb(color(window.used.into(), theme, theme.foreground)))
                    .child(window.label(now)),
            );
        }
        segment
            .child(labels)
            // The numbers are the last good ones; the tooltip says why.
            .when(reading.error.is_some(), |segment| {
                segment.child(
                    div()
                        .flex_none()
                        .text_color(rgb(theme.palette[3]))
                        .child("!"),
                )
            })
    }
}

fn color(severity: Severity, theme: &crate::config::Theme, normal: u32) -> u32 {
    match severity {
        Severity::Normal => normal,
        Severity::Warning => theme.palette[3],
        Severity::Critical => theme.palette[1],
    }
}

fn meter(window: &Limit, theme: &crate::config::Theme) -> impl IntoElement {
    div()
        .w(px(METER_WIDTH))
        .h(px(5.))
        .flex_none()
        .rounded_full()
        .overflow_hidden()
        .bg(rgb(theme.active))
        .child(
            div()
                .h_full()
                .w(px(METER_WIDTH * window.used / 100.))
                .rounded_full()
                .bg(rgb(color(window.used.into(), theme, theme.muted))),
        )
}

fn ago(elapsed: Duration) -> String {
    match elapsed.as_secs() {
        0..60 => "less than a minute".into(),
        seconds => super::model::countdown(Duration::from_secs(seconds - seconds % 60)),
    }
}

fn hint(
    lines: Vec<SharedString>,
    theme: &crate::config::Theme,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let (foreground, muted, surface) = (theme.foreground, theme.muted, theme.surface);
    move |_, cx| {
        let lines = lines.clone();
        cx.new(|_| UsageHint {
            lines,
            foreground,
            muted,
            surface,
        })
        .into()
    }
}

struct UsageHint {
    lines: Vec<SharedString>,
    foreground: u32,
    muted: u32,
    surface: u32,
}

impl Render for UsageHint {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .px(px(8.))
            .py(px(6.))
            .rounded(px(crate::config::corners::CONTROL))
            .shadow_md()
            .text_size(px(12.))
            .bg(rgb(self.surface))
            .text_color(rgb(self.muted))
            .children(self.lines.iter().enumerate().map(|(index, line)| {
                div()
                    .when(index == 0, |line| {
                        line.font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(self.foreground))
                    })
                    .child(line.clone())
            }))
    }
}
