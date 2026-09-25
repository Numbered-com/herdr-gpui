//! Orca's worktree cards: a status column beside the name, a quieter meta line
//! with the host, branch, and pull request, and compact single-line agents.
//! Cards are inset and rounded; the focused one is washed and outlined.

use super::super::{
    agents::agent_labels,
    cell::{AgentRow, RowContext, RowLayout, RowState, WorkspaceRow},
    glyph_width, label_text, line_height,
    row::{RowTree, name_line, removing_indicator},
    status_indicator,
};
use super::wash;
use crate::config::{FontConfig, Theme};
use gpui::{prelude::*, *};

pub(in super::super) struct Orca;

/// Space between a card and the sidebar's edges.
const MARGIN: f32 = 4.;
const RADIUS: f32 = 8.;
/// The status column, wider than its dot so the dot has air on both sides.
const STATUS_COLUMN: f32 = 12.;

/// The card or agent strip: inset, rounded, and marked by state. The border
/// is always present, transparent unless selected, so selecting a row never
/// shifts its content.
fn card(key: &str, state: RowState, indent: f32, theme: &Theme) -> Div {
    let hover = wash(theme.foreground, 0x0a);
    div()
        .debug_selector(|| format!("row-{key}"))
        .relative()
        .flex_none()
        .min_w_0()
        .ml(px(MARGIN + indent))
        .mr(px(MARGIN))
        .my(px(1.))
        .rounded(px(RADIUS))
        .border_1()
        .border_color(rgba(0))
        .cursor_pointer()
        .when(state.selected, |card| {
            card.bg(wash(theme.foreground, 0x1a))
                .border_color(wash(theme.foreground, 0x2e))
        })
        .when(!state.selected && state.highlighted, |card| card.bg(hover))
        .when(!state.selected, |card| {
            card.hover(move |style| style.bg(hover))
        })
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

fn small(font: &FontConfig) -> (f32, f32) {
    let size = (font.size * 0.85).round();
    (size, glyph_width(font) * size / font.size)
}

/// A small rounded tag, such as the host a remote card lives on.
fn chip(text: &str, width: f32, size: f32, theme: &Theme) -> Div {
    div()
        .flex_none()
        .px(px(5.))
        .rounded(px(4.))
        .bg(rgb(theme.active))
        .text_size(px(size))
        .text_color(rgb(theme.muted))
        .child(truncated(text, width))
}

impl RowLayout for Orca {
    fn workspace(&self, row: WorkspaceRow<'_>, state: RowState, cx: &RowContext<'_>) -> Div {
        let (branch, status) = (row.branch(), row.status());
        let WorkspaceRow {
            label,
            tree,
            fold,
            grouped,
            badge,
            removing,
            ..
        } = row;
        let (theme, font) = (cx.theme, cx.font);
        let line = line_height(font);
        let gap = 6.;
        let (small_size, small_glyph) = small(font);
        let indent = if tree == RowTree::None {
            0.
        } else {
            cx.look.density.padding()
        };
        // Everything inside the card's border and padding, less the status
        // column: the width both lines divide.
        // Border, padding, the status column, and the gap after it.
        let content = cx.width - 1. - 2. * MARGIN - indent - 2. - 2. * gap - STATUS_COLUMN - gap;
        // The repository's own checkout is marked as the primary one, the way
        // a group's first card reads in Orca.
        let primary = grouped && tree == RowTree::None;
        let primary_width = (7. * small_glyph).ceil() + 10.;
        let fold_width = if fold.is_some() { 14. } else { 0. };
        let title_width = content
            - if primary { primary_width + gap } else { 0. }
            - if fold.is_some() { fold_width + gap } else { 0. };
        // Children are named by their branch already; repeating it is noise.
        let branch = branch.filter(|branch| *branch != label);
        let pr = badge.as_ref().and_then(|badge| badge.pr.as_ref());
        let dirty = badge.as_ref().is_some_and(|badge| badge.dirty);
        let dirty_size = (line * 0.7).round().min(14.);
        let pr_width = pr.map_or(0., |pr| {
            small_size + 3. + (pr.number.chars().count() as f32 * small_glyph).ceil()
        });
        let trailing = pr_width + if dirty { dirty_size + gap } else { 0. };
        let host_width = cx.host.map_or(0., |host| {
            (host.chars().count() as f32 * small_glyph)
                .ceil()
                .min(content / 3.)
        });
        let branch_width = content
            - trailing
            - if trailing > 0. { gap } else { 0. }
            - if cx.host.is_some() {
                host_width + 10. + gap
            } else {
                0.
            };
        let meta = branch.is_some() || cx.host.is_some() || trailing > 0.;
        card(label, state, indent, theme)
            .flex()
            .items_start()
            .gap(px(gap))
            .px(px(gap))
            .py(px(if meta { 5. } else { 7. }))
            .when(removing, |card| card.opacity(0.5))
            .child(
                div()
                    .w(px(STATUS_COLUMN))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .child(if removing {
                        removing_indicator(theme).mt(px((line - 8.) / 2.))
                    } else {
                        status_indicator(status, font)
                    }),
            )
            .child(
                div()
                    .w(px(content.max(0.)))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .h(px(line))
                            .flex()
                            .items_center()
                            .gap(px(gap))
                            .child(
                                truncated(label, title_width)
                                    .debug_selector(|| format!("name-{label}"))
                                    .text_color(rgb(theme.foreground))
                                    .when(state.selected, |title| {
                                        title.font_weight(FontWeight::SEMIBOLD)
                                    }),
                            )
                            .when(primary, |title| {
                                title.child(
                                    div()
                                        .w(px(primary_width))
                                        .flex_none()
                                        .flex()
                                        .justify_center()
                                        .rounded(px(4.))
                                        .border_1()
                                        .border_color(wash(theme.foreground, 0x33))
                                        .bg(wash(theme.foreground, 0x0f))
                                        .text_size(px(small_size))
                                        .text_color(rgb(theme.subtext()))
                                        .child(label_text("primary")),
                                )
                            })
                            .when_some(fold, |title, fold| {
                                title
                                    .child(fold.element(theme).w(px(fold_width)).text_size(px(14.)))
                            }),
                    )
                    .when(meta, |column| {
                        column.child(
                            div()
                                .h(px(line))
                                .flex()
                                .items_center()
                                .gap(px(gap))
                                .text_size(px(small_size))
                                .text_color(rgb(theme.muted))
                                .when_some(cx.host, |meta, host| {
                                    meta.child(chip(host, host_width, small_size, theme))
                                })
                                .when_some(branch, |meta, branch| {
                                    meta.child(
                                        truncated(branch, branch_width)
                                            .debug_selector(|| format!("detail-{label}")),
                                    )
                                })
                                .child(div().flex_1())
                                .when(dirty, |meta| {
                                    meta.child(
                                        crate::icons::uncommitted(theme, dirty_size)
                                            .debug_selector(|| format!("dirty-{label}")),
                                    )
                                })
                                .when_some(pr, |meta, pr| {
                                    meta.child(
                                        div()
                                            .debug_selector(|| format!("pr-{label}"))
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .gap(px(3.))
                                            .text_color(rgb(pr.color))
                                            .child(
                                                svg()
                                                    .path("icons/git-branch.svg")
                                                    .size(px(small_size))
                                                    .text_color(rgb(pr.color)),
                                            )
                                            .child(label_text(&pr.number)),
                                    )
                                }),
                        )
                    }),
            )
    }

    fn agent(&self, agent: AgentRow<'_>, state: RowState, cx: &RowContext<'_>) -> Div {
        let (theme, font) = (cx.theme, cx.font);
        let key = agent.key.as_str();
        let line = line_height(font);
        let (gap, icon) = (4., (line * 0.85).round());
        // Border, padding, then the dot, icon, and text with a gap between each.
        let text = cx.width - 1. - 2. * MARGIN - 2. - 2. * gap - STATUS_COLUMN - icon - 2. * gap;
        // The agent leads here, then where it runs, all on one muted line.
        let (location, name) = agent_labels(agent.name, agent.place, cx.host);
        let segments: Vec<_> = if agent.place.is_some() {
            std::iter::once((name, true))
                .chain(location.into_iter().map(|(text, _)| (text, false)))
                .collect()
        } else {
            location
        };
        let color = if state.selected {
            theme.foreground
        } else {
            theme.subtext()
        };
        card(key, state, 0., theme)
            .rounded(px(4.))
            .h(px(line + 8.))
            .flex()
            .items_center()
            .gap(px(gap))
            .px(px(gap))
            .child(
                div()
                    .w(px(STATUS_COLUMN))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .child(status_indicator(agent.status, font).mt_0()),
            )
            .child(
                svg()
                    .path(agent.icon.path())
                    .size(px(icon))
                    .flex_none()
                    .text_color(rgb(color)),
            )
            .child(
                name_line(
                    &segments,
                    (color, FontWeight::NORMAL, theme.muted),
                    text.max(0.),
                    font,
                )
                .debug_selector(|| format!("name-{key}")),
            )
    }
}
