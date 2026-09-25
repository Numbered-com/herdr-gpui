//! Superset's single-line rows: one icon slot that reports the row's state,
//! the name, and the pull request's change counts on the right. The focused
//! row is filled and marked with a stripe down its leading edge.

use super::super::{
    agents::status_style,
    cell::{AgentRow, RowContext, RowLayout, RowState, WorkspaceRow},
    glyph_width, label_text, line_height,
    row::{RowIcon, RowTree, removing_indicator},
};
use super::wash;
use crate::config::{FontConfig, Theme};
use gpui::{prelude::*, *};
use herdr_client::protocol::AgentStatus;

pub(in super::super) struct Superset;

/// Width of the leading edge stripe on the focused row.
const STRIPE: f32 = 2.;

/// Geometry shared by both kinds of row, scaled from the sidebar font so a
/// larger font grows the icon slot with the text.
struct Metrics {
    line: f32,
    icon: f32,
    gap: f32,
    small: f32,
}

impl Metrics {
    fn new(cx: &RowContext<'_>) -> Self {
        let line = line_height(cx.font);
        Self {
            line,
            icon: (cx.font.size * 1.5).round().max(line),
            gap: cx.look.density.gap(),
            small: (cx.font.size * 0.8).round(),
        }
    }

    fn small_glyph(&self, font: &FontConfig) -> f32 {
        glyph_width(font) * self.small / font.size
    }
}

/// The row shell: padding, the state fill, and the stripe. Content is laid
/// out by the caller at the widths it measured.
fn shell(key: &str, state: RowState, indent: f32, metrics: &Metrics, cx: &RowContext<'_>) -> Div {
    let theme = cx.theme;
    let (active, hover, stripe) = (theme.active, wash(theme.foreground, 0x0d), theme.foreground);
    div()
        .debug_selector(|| format!("row-{key}"))
        .relative()
        .w_full()
        .min_w_0()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(metrics.gap))
        .py(px(metrics.gap))
        .pl(px(cx.look.content_x() + indent))
        .pr(px(cx.look.content_x()))
        .cursor_pointer()
        .when(state.selected, |row| row.bg(rgb(active)))
        .when(!state.selected && state.highlighted, |row| row.bg(hover))
        .when(!state.selected, |row| {
            row.hover(move |style| style.bg(hover))
        })
        .when(state.selected, |row| {
            row.child(
                div()
                    .debug_selector(|| format!("highlight-{key}"))
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(STRIPE))
                    .rounded_r(px(STRIPE))
                    .bg(rgb(stripe)),
            )
        })
}

/// A status dot pinned to the icon slot's top right corner. Unknown draws
/// nothing: the icon alone already says there is nothing to report.
fn status_badge(status: AgentStatus, theme: &Theme) -> Option<Div> {
    if status == AgentStatus::Unknown {
        return None;
    }
    let (diameter, filled, color) = status_style(status);
    Some(
        div()
            .absolute()
            .top(px(-2.))
            .right(px(-2.))
            .size(px(diameter))
            .rounded_full()
            .border_1()
            .border_color(rgb(color))
            .bg(rgb(if filled { color } else { theme.surface })),
    )
}

fn icon_slot(key: &str, metrics: &Metrics) -> Div {
    div()
        .debug_selector(|| format!("icon-{key}"))
        .relative()
        .size(px(metrics.icon))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
}

fn truncated(text: &str, width: f32) -> Div {
    div()
        .w(px(width.max(0.)))
        .flex_none()
        .overflow_hidden()
        .child(
            div()
                .w(px(width.max(0.)))
                .truncate()
                .child(label_text(text)),
        )
}

impl RowLayout for Superset {
    fn workspace(&self, row: WorkspaceRow<'_>, state: RowState, cx: &RowContext<'_>) -> Div {
        let status = row.status();
        let WorkspaceRow {
            label,
            tree,
            icon,
            fold,
            badge,
            removing,
            ..
        } = row;
        let (theme, font) = (cx.theme, cx.font);
        let metrics = Metrics::new(cx);
        let indent = if tree == RowTree::None {
            0.
        } else {
            cx.look.density.padding()
        };
        let pr = badge.as_ref().and_then(|badge| badge.pr.as_ref());
        let dirty = badge.as_ref().is_some_and(|badge| badge.dirty);
        let small_glyph = metrics.small_glyph(font);
        // The right cluster is sized from its text so the name can take the
        // rest at a fixed width, which is what GPUI 0.2.2 needs to ellipsize.
        let dirty_size = (metrics.line * 0.75).round().min(15.);
        let fold_width = if fold.is_some() {
            metrics.icon * 0.6
        } else {
            0.
        };
        let available = cx.look.content_width(cx.width) - indent - metrics.icon - metrics.gap;
        // Icons keep their size; the counts give way to them on a narrow
        // sidebar and are clipped rather than pushed past the row's edge.
        let icons = [if dirty { dirty_size } else { 0. }, fold_width]
            .into_iter()
            .filter(|width| *width > 0.)
            .fold(0., |total, width| total + width + metrics.gap);
        let stats_width = pr.map_or(0., |pr| {
            // Both counts, and a glyph's gap between them.
            let glyphs = pr.additions.chars().count() + pr.deletions.chars().count() + 1;
            (glyphs as f32 * small_glyph)
                .ceil()
                .min((available - icons - metrics.gap).max(0.))
        });
        let cluster = icons
            + if pr.is_some() {
                stats_width + metrics.gap
            } else {
                0.
            };
        let name_width = (available - cluster).max(0.);
        let muted = theme.muted;
        let icon_color = if state.selected {
            theme.foreground
        } else {
            muted
        };
        let slot = icon_slot(label, &metrics);
        let slot = if removing {
            slot.child(removing_indicator(theme))
        } else {
            // A pull request outranks the owner: its color is its state.
            let glyph = match (pr, icon) {
                (Some(pr), _) => branch_icon(pr.color, metrics.icon * 0.7).into_any_element(),
                (None, RowIcon::None) => {
                    branch_icon(icon_color, metrics.icon * 0.7).into_any_element()
                }
                (None, icon) => div()
                    .size(px(metrics.icon * 0.7))
                    .child(icon.element(icon_color))
                    .into_any_element(),
            };
            slot.child(glyph).children(status_badge(status, theme))
        };
        let (additions, deletions) = if state.selected {
            (theme.palette[2], theme.palette[1])
        } else {
            (muted, muted)
        };
        shell(label, state, indent, &metrics, cx)
            .child(slot)
            .child(
                truncated(label, name_width)
                    .debug_selector(|| format!("name-{label}"))
                    .text_color(rgb(if state.selected {
                        theme.foreground
                    } else {
                        theme.subtext()
                    })),
            )
            .when(dirty, |row| {
                row.child(
                    crate::icons::uncommitted(theme, dirty_size)
                        .debug_selector(|| format!("dirty-{label}")),
                )
            })
            .when_some(pr, |row, pr| {
                row.child(
                    div()
                        .debug_selector(|| format!("pr-{label}"))
                        .w(px(stats_width))
                        .flex_none()
                        .overflow_hidden()
                        .flex()
                        .justify_end()
                        .gap(px(small_glyph))
                        .text_size(px(metrics.small))
                        .child(
                            div()
                                .text_color(rgb(additions))
                                .child(label_text(&pr.additions)),
                        )
                        .child(
                            div()
                                .text_color(rgb(deletions))
                                .child(label_text(&pr.deletions)),
                        ),
                )
            })
            .when_some(fold, |row, fold| {
                row.child(fold.element(theme).w(px(fold_width)).text_size(px(14.)))
            })
    }

    fn agent(&self, agent: AgentRow<'_>, state: RowState, cx: &RowContext<'_>) -> Div {
        let (theme, font) = (cx.theme, cx.font);
        let metrics = Metrics::new(cx);
        let key = agent.key.as_str();
        let available = cx.look.content_width(cx.width) - metrics.icon - metrics.gap;
        // Where the agent runs trails its name in the smaller, muted face, and
        // never takes more than half of the row.
        let place = agent.place.map(|(workspace, _)| match cx.host {
            Some(host) => format!("{host} \u{b7} {workspace}"),
            None => workspace.to_owned(),
        });
        let place_width = place.as_ref().map_or(0., |place| {
            (place.chars().count() as f32 * metrics.small_glyph(font))
                .ceil()
                .min(available / 2.)
        });
        let name_width = available - place_width - if place.is_some() { metrics.gap } else { 0. };
        let color = if state.selected {
            theme.foreground
        } else {
            theme.subtext()
        };
        shell(key, state, 0., &metrics, cx)
            .child(
                icon_slot(key, &metrics)
                    .child(
                        svg()
                            .path(agent.icon.path())
                            .size(px(metrics.icon * 0.7))
                            .text_color(rgb(color)),
                    )
                    .children(status_badge(agent.status, theme)),
            )
            .child(
                truncated(agent.name, name_width)
                    .debug_selector(|| format!("name-{key}"))
                    .text_color(rgb(color)),
            )
            .when_some(place, |row, place| {
                row.child(
                    truncated(&place, place_width)
                        .debug_selector(|| format!("detail-{key}"))
                        .text_size(px(metrics.small))
                        .text_color(rgb(theme.muted)),
                )
            })
    }
}

fn branch_icon(color: u32, size: f32) -> Svg {
    svg()
        .path("icons/git-branch.svg")
        .size(px(size))
        .flex_none()
        .text_color(rgb(color))
}
