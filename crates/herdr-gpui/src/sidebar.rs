use super::{Command, HerdrWindow, NavigationTarget};
use crate::config::{FontConfig, Theme};
use gpui::{prelude::*, *};
use herdr_client::protocol::{AgentStatus, ClientShellAgent, ClientShellWorkspace};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

const SIDEBAR_WIDTH: f32 = 232.;
const ROW_PADDING: f32 = 12.;
const STATUS_WIDTH: f32 = 8.;
// Unknown stays a smaller dot so it reads as "no reported status" next to the full ones.
const STATUS_DOT_UNKNOWN: f32 = 3.;
pub(super) const LABEL_GAP: f32 = 8.;
// Children clear the gutter their tree lines run in, which starts at the
// parent's label column so the trunk lines up under the parent's branch.
const TREE_GUTTER: f32 = ROW_PADDING + STATUS_WIDTH + LABEL_GAP;
const CHILD_INDENT: f32 = TREE_GUTTER - ROW_PADDING + 12.;
pub(super) const ARROW_RESERVE: f32 = 18.;
pub(super) const HOST_ARROW_WIDTH: f32 = 12.;
pub(super) const HOST_GAP: f32 = 6.;
pub(super) const ICON_RESERVE: f32 = 18.;
pub(super) static GITHUB_ICON: LazyLock<Arc<Image>> = LazyLock::new(|| {
    Arc::new(Image::from_bytes(
        ImageFormat::Svg,
        include_bytes!("../../../assets/icons/github.svg").to_vec(),
    ))
});
#[cfg(any(test, feature = "integration-test"))]
pub(super) const LABEL_WIDTH: f32 =
    SIDEBAR_WIDTH - 1. - 2. * ROW_PADDING - STATUS_WIDTH - LABEL_GAP;

impl HerdrWindow {
    fn save_sidebar_width(&mut self) {
        self.sidebar_modified = true;
        self.save_chrome();
    }

    /// One file holds the whole chrome, so every save carries both fields.
    pub(super) fn save_chrome(&self) {
        if let Some(preferences) = &self.sidebar_preferences {
            preferences.save(crate::preferences::Chrome {
                sidebar_width: self.sidebar_width,
                agent_sort: self.agent_sort,
            });
        }
    }

    pub(super) fn render_sidebar(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let width = sidebar_width(self.sidebar_width, f32::from(window.viewport_size().width));
        // Hide secondary status in narrow windows, retaining useful host label space.
        let show_host_status = width >= 200.;
        let host_label_width = (width
            - 1.
            - 2. * ROW_PADDING
            - HOST_ARROW_WIDTH
            - HOST_GAP
            - if show_host_status { HOST_GAP + 67. } else { 0. })
        .max(0.);
        let view = cx.entity().downgrade();
        let font = &self.config.sidebar;
        let theme = &self.theme;
        let mut spaces = div()
            .id("spaces-scroll")
            .debug_selector(|| "spaces-scroll".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
        let mut agents = div()
            .id("agents-scroll")
            .debug_selector(|| "agents-scroll".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
        spaces = spaces.track_scroll(&self.sidebar_scroll[0]);
        agents = agents.track_scroll(&self.sidebar_scroll[1]);
        let multi = self.endpoints.len() > 1;
        let mut agent_count = 0;
        // Child positions of the highlighted rows, for the one-time reveal below.
        // Agent rows are counted by `agent_count`, which indexes that list.
        let mut space_rows = 0usize;
        let mut highlighted = [None; 2];
        for (endpoint_index, endpoint) in self.endpoints.iter().enumerate() {
            let selected = endpoint_index == self.selected_endpoint;
            let endpoint_id = endpoint.id.clone();
            if multi {
                let collapse_id = endpoint_id.clone();
                let select_id = endpoint_id.clone();
                spaces = spaces.child(
                    div()
                        .id(SharedString::from(format!("host-{endpoint_id}")))
                        .debug_selector(|| format!("host-{endpoint_id}"))
                        .h(px(line_height(font) + 16.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(HOST_GAP))
                        .px(px(12.))
                        .when(selected, |row| row.bg(rgb(theme.active)))
                        .text_color(rgb(if endpoint.enabled {
                            theme.foreground
                        } else {
                            theme.muted
                        }))
                        .cursor_pointer()
                        .child(
                            div()
                                .id(SharedString::from(format!("collapse-host-{endpoint_id}")))
                                .w(px(HOST_ARROW_WIDTH))
                                .flex_none()
                                .child(label_text(if endpoint.collapsed {
                                    "\u{25b8}"
                                } else {
                                    "\u{25be}"
                                }))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    if let Some(endpoint) =
                                        this.endpoints.iter_mut().find(|e| e.id == collapse_id)
                                    {
                                        endpoint.collapsed = !endpoint.collapsed;
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                // As with workspace labels, avoid zero-basis text measurement.
                                .w(px(host_label_width))
                                .flex_none()
                                .overflow_hidden()
                                .child(
                                    div()
                                        .w(px(host_label_width))
                                        .truncate()
                                        .child(label_text(&endpoint.label)),
                                ),
                        )
                        .when(show_host_status, |row| {
                            row.child(
                                div()
                                    .w(px(67.))
                                    .flex_none()
                                    .text_right()
                                    .text_size(px(font.size * 0.75))
                                    .text_color(rgb(theme.muted))
                                    .child(endpoint.status()),
                            )
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.select_endpoint(&select_id, cx);
                            window.focus(&this.focus);
                        })),
                );
                space_rows += 1;
            }
            let live = if selected { &self.live } else { &endpoint.live };
            let Some(snapshot) = &live.snapshot else {
                continue;
            };
            let collapsed_repos = if endpoint_index == 0 {
                &self.collapsed_repos
            } else {
                &endpoint.collapsed_repos
            };
            let entries = visible_workspace_entries(&snapshot.workspaces, collapsed_repos);
            // A child closes the group when no child follows it.
            let closes: Vec<bool> = (0..entries.len())
                .map(|position| {
                    entries[position].1 && !entries.get(position + 1).is_some_and(|next| next.1)
                })
                .collect();
            for (position, (index, indented, group)) in entries.into_iter().enumerate() {
                if multi && endpoint.collapsed {
                    break;
                }
                let workspace = &snapshot.workspaces[index];
                if selected && workspace.focused {
                    highlighted[0] = Some(space_rows);
                }
                space_rows += 1;
                let id = workspace.workspace_id.clone();
                let context_id = id.clone();
                let context_endpoint = endpoint_id.clone();
                let navigate_endpoint = endpoint_id.clone();
                let collapse_endpoint = endpoint_id.clone();
                let reserve_arrow = group.is_some() || indented;
                let tree = match (indented, closes[position]) {
                    (false, _) => RowTree::None,
                    (true, false) => RowTree::Child,
                    (true, true) => RowTree::LastChild,
                };
                let arrow = group.map(|key| {
                    let collapsed = collapsed_repos.contains(&key);
                    div()
                        .id(SharedString::from(format!("collapse-{endpoint_id}-{id}")))
                        .debug_selector(move || format!("collapse-{index}"))
                        .w(px(ARROW_RESERVE - LABEL_GAP))
                        .h(px(2. * line_height(font)))
                        .flex_none()
                        .text_size(px(16.))
                        .text_color(rgb(theme.muted))
                        .hover(|s| s.text_color(rgb(theme.foreground)))
                        .child(label_text(if collapsed { "\u{25b8}" } else { "\u{25be}" }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            let collapsed = if collapse_endpoint == super::endpoint::LOCAL {
                                &mut this.collapsed_repos
                            } else if let Some(endpoint) = this
                                .endpoints
                                .iter_mut()
                                .find(|e| e.id == collapse_endpoint)
                            {
                                &mut endpoint.collapsed_repos
                            } else {
                                return;
                            };
                            if !collapsed.remove(&key) {
                                collapsed.insert(key.clone());
                            }
                            cx.notify();
                        }))
                });
                spaces = spaces.child(
                    row(
                        workspace_label(workspace, indented),
                        first_text([workspace.branch.as_deref()], ""),
                        workspace.agent_status,
                        selected && workspace.focused,
                        tree,
                        reserve_arrow,
                        width,
                        (!indented).then(|| {
                            self.avatars
                                .as_ref()
                                .filter(|_| endpoint_index == 0)
                                .and_then(|avatars| avatars.image(&workspace.new_workspace_cwd))
                                .unwrap_or_else(|| GITHUB_ICON.clone())
                        }),
                        arrow,
                        workspace_pr(workspace, &self.menu.pr_cache, theme),
                        (font, theme),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            if this.menu.page.is_none()
                                && this.select_endpoint(&context_endpoint, cx)
                            {
                                this.open_workspace_menu(&context_id, event.position, window, cx);
                            }
                        }),
                    )
                    .id(SharedString::from(format!("workspace-{endpoint_id}-{id}")))
                    .when(multi, |row| {
                        row.debug_selector(|| format!("workspace-{endpoint_id}-{id}"))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.navigate_endpoint(
                            &navigate_endpoint,
                            NavigationTarget::Workspace(&id),
                            cx,
                        );
                        window.focus(&this.focus);
                    })),
                );
            }
            for agent in sorted_agents(&snapshot.agents, self.agent_sort) {
                if selected && agent.focused {
                    highlighted[1] = Some(agent_count);
                }
                agent_count += 1;
                let id = agent.pane_id.clone();
                let navigate_endpoint = endpoint_id.clone();
                let (name, kind) = agent_labels(agent);
                let detail = if multi {
                    format!("{} / {kind}", endpoint.label)
                } else {
                    kind.to_owned()
                };
                agents = agents.child(
                    row(
                        name,
                        &detail,
                        agent.agent_status,
                        selected && agent.focused,
                        RowTree::None,
                        false,
                        width,
                        None,
                        None,
                        None,
                        (font, theme),
                    )
                    .id(SharedString::from(format!("agent-{endpoint_id}-{id}")))
                    .when(multi, |row| {
                        row.debug_selector(|| format!("agent-{endpoint_id}-{id}"))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.navigate_endpoint(&navigate_endpoint, NavigationTarget::Pane(&id), cx);
                        window.focus(&this.focus);
                    })),
                );
            }
        }
        // Follow the selection, but only once a frame has measured the viewport:
        // the handle resolves the request against the previous frame's bounds, so
        // an unmeasured list would scroll to a meaningless offset. Recording what
        // was revealed keeps later frames from undoing the user's own scrolling.
        for (list, row) in highlighted.iter().enumerate() {
            let Some(row) = *row else { continue };
            if self.sidebar_revealed[list].get() != Some(row)
                && self.sidebar_scroll[list].bounds().size.height > px(0.)
            {
                self.sidebar_scroll[list].scroll_to_item(row);
                self.sidebar_revealed[list].set(Some(row));
            }
        }
        if agent_count == 0 {
            agents = agents.child(
                div()
                    .px(px(12.))
                    .text_color(rgb(theme.muted))
                    .truncate()
                    .child("no agents"),
            );
        }
        div()
            .id("sidebar")
            .debug_selector(|| "sidebar".into())
            .relative()
            .w(px(width))
            .flex_none()
            .h_full()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .font_family(font.family.clone())
            .text_size(px(font.size))
            .line_height(px(line_height(font)))
            .text_color(rgb(theme.foreground))
            .bg(rgb(theme.surface))
            .border_r_1()
            .border_color(rgb(theme.active))
            // Zero flex bases keep long workspace lists from displacing agents.
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(header("spaces", font, theme))
                    .child(spaces)
                    .child(
                        div()
                            .flex_none()
                            .h(px(line_height(font) + 10.))
                            .px(px(12.))
                            .flex()
                            .items_center()
                            // Menu hugs the sidebar's edge, as in the terminal client.
                            .justify_between()
                            .text_color(rgb(theme.muted))
                            .gap(px(20.))
                            .child(
                                div()
                                    .id("new-workspace")
                                    .cursor_pointer()
                                    .hover(|s| s.text_color(rgb(theme.foreground)))
                                    .child("new")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.command(Command::Workspace, window, cx)
                                    })),
                            )
                            .child(
                                div()
                                    .id("sidebar-menu")
                                    .debug_selector(|| "sidebar-menu".into())
                                    .cursor_pointer()
                                    .hover(|s| s.text_color(rgb(theme.foreground)))
                                    .child(label_text("menu"))
                                    .on_click(cx.listener(
                                        |this, event: &ClickEvent, window, cx| {
                                            this.menu.anchor = event.position();
                                            this.open_menu(window, cx);
                                        },
                                    )),
                            ),
                    ),
            )
            .child(div().h(px(1.)).flex_none().bg(rgb(theme.active)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        header("agents", font, theme)
                            .justify_between()
                            .child(agents_sort(self, cx)),
                    )
                    .child(agents),
            )
            .child(
                div()
                    .id("sidebar-resize")
                    .debug_selector(|| "sidebar-resize".into())
                    .absolute()
                    .right_0()
                    .top_0()
                    .h_full()
                    .w(px(6.))
                    .cursor(CursorStyle::ResizeLeftRight)
                    .hover(|s| s.bg(rgba(0x78a9ff44)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.sidebar_modified = true;
                            if event.click_count == 2 {
                                this.sidebar_drag = None;
                                this.sidebar_width = None;
                                this.save_sidebar_width();
                            } else {
                                this.sidebar_drag = Some((f32::from(event.position.x), width));
                            }
                            cx.notify();
                        }),
                    ),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, _| {
                        // Capture globally so dragging continues outside the narrow divider,
                        // and terminal handlers never receive the resize gesture's release.
                        let moving = view.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                            if phase == DispatchPhase::Capture {
                                let _ = moving.update(cx, |this, cx| {
                                    if let Some((start, width)) = this.sidebar_drag {
                                        this.sidebar_width = Some(sidebar_width(
                                            Some(width + f32::from(event.position.x) - start),
                                            f32::from(window.viewport_size().width),
                                        ));
                                        cx.stop_propagation();
                                        cx.notify();
                                    }
                                });
                            }
                        });
                        let released = view.clone();
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture && event.button == MouseButton::Left
                            {
                                let _ = released.update(cx, |this, cx| {
                                    if this.sidebar_drag.take().is_some() {
                                        this.save_sidebar_width();
                                        cx.stop_propagation();
                                        cx.notify();
                                    }
                                });
                            }
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
    }
}

// Preserve the sidebar's compact 12px font / 16px line defaults as fonts scale.
fn line_height(font: &FontConfig) -> f32 {
    font.size * 4. / 3.
}

fn sidebar_width(preferred: Option<f32>, window_width: f32) -> f32 {
    // Keep useful label space and reserve at least 240 logical pixels for the terminal.
    preferred
        .unwrap_or(SIDEBAR_WIDTH)
        .clamp(160., 480.)
        .min((window_width - 240.).max(0.))
}

fn header(label: &'static str, font: &FontConfig, theme: &Theme) -> Div {
    div()
        .flex_none()
        .h(px(line_height(font) + 12.))
        .px(px(12.))
        .flex()
        .items_center()
        .text_size(px(font.size))
        .text_color(rgb(theme.muted))
        .child(label)
}

/// Upstream's agents panel ends its header with the current sort, which a
/// click flips. An active agent view names itself there instead, and cannot be
/// re-sorted, so the label is inert while one is on.
fn agents_sort(window: &HerdrWindow, cx: &mut Context<HerdrWindow>) -> Stateful<Div> {
    let theme = &window.theme;
    let view = window
        .live
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.agent_view_label.clone());
    let label = view
        .clone()
        .unwrap_or_else(|| window.agent_sort.label().into());
    div()
        .id("agents-sort")
        .debug_selector(|| "agents-sort".into())
        .flex_none()
        .min_w_0()
        .truncate()
        .text_color(rgb(theme.muted))
        .when(view.is_none(), |sort| {
            sort.cursor_pointer()
                .hover(|style| style.text_color(rgb(theme.foreground)))
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.agent_sort = this.agent_sort.toggled();
                    this.agent_sort_modified = true;
                    this.save_chrome();
                    cx.notify();
                }))
        })
        .child(label_text(&label))
}

/// Attention first, then the most recent change, as upstream orders it.
fn status_priority(status: AgentStatus) -> u8 {
    match status {
        AgentStatus::Blocked => 4,
        AgentStatus::Done => 3,
        AgentStatus::Working => 2,
        AgentStatus::Idle => 1,
        AgentStatus::Unknown => 0,
    }
}

/// The agents of one endpoint in the order the panel paints them.
fn sorted_agents(
    agents: &[ClientShellAgent],
    sort: crate::preferences::AgentSort,
) -> Vec<&ClientShellAgent> {
    let mut ordered: Vec<_> = agents.iter().collect();
    if sort == crate::preferences::AgentSort::Priority {
        ordered.sort_by_key(|agent| {
            (
                std::cmp::Reverse(status_priority(agent.agent_status)),
                std::cmp::Reverse(agent.state_change_seq),
            )
        });
    }
    ordered
}

/// Where a row sits in its worktree group, which decides whether the gutter
/// carries a trunk through the row or ends in an elbow.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RowTree {
    None,
    Child,
    LastChild,
}

/// Trunk and tick for a child row, given the gutter the row reserves between
/// the parent's label column and the child's status dot. Snapped to whole
/// device pixels and painted as quads rather than borders: a bordered box
/// rounds each edge on its own, which left the trunk thinner than its tick.
fn tree_lines(
    gutter: Bounds<Pixels>,
    tree: RowTree,
    font: &FontConfig,
    scale: f32,
) -> [Bounds<Pixels>; 2] {
    let device = |value: Pixels| f32::from(value) * scale;
    let logical = |value: f32| px(value / scale);
    let weight = scale.round().max(1.);
    let snap = |value: Pixels| logical(device(value).round());
    // The trunk runs down the gutter's leading edge; the tick crosses it at the
    // status dot's middle row and stops where the dot begins.
    let x = snap(gutter.origin.x);
    let middle =
        logical((device(gutter.origin.y + px(4. + line_height(font) / 2.)) - weight / 2.).round());
    let end = if tree == RowTree::LastChild {
        middle + logical(weight)
    } else {
        snap(gutter.bottom())
    };
    [
        Bounds::from_corners(
            point(x, snap(gutter.origin.y)),
            point(x + logical(weight), end),
        ),
        Bounds::from_corners(
            point(x, middle),
            point(snap(gutter.right()), middle + logical(weight)),
        ),
    ]
}

/// Cached pull request state for a worktree row: the number carries the
/// lifecycle color, the counts sit under it.
struct PrBadge {
    number: String,
    color: u32,
    additions: String,
    deletions: String,
}

impl PrBadge {
    fn new(pr: &crate::pull_request::PullRequest, theme: &Theme) -> Self {
        Self {
            number: format!("#{}", pr.number),
            color: pr.color(theme),
            additions: format!("+{}", compact(pr.additions)),
            deletions: format!("-{}", compact(pr.deletions)),
        }
    }

    /// Reserved width. Sidebar labels are monospace by default and digits are
    /// near-uniform elsewhere, so an em-fraction per glyph bounds both lines;
    /// a wider face truncates the counts rather than eating the label.
    fn width(&self, font: &FontConfig) -> f32 {
        let glyphs = self
            .number
            .chars()
            .count()
            .max(self.additions.chars().count() + self.deletions.chars().count() + 1);
        (font.size * 0.62 * glyphs as f32).ceil()
    }
}

/// Four digits of churn is already a big diff; abbreviate past that so the
/// column stays narrow enough to leave the branch readable.
fn compact(lines: u64) -> String {
    match lines {
        0..=9999 => lines.to_string(),
        _ => format!("{}k", lines / 1000),
    }
}

#[allow(clippy::too_many_arguments)]
fn row(
    name: &str,
    detail: &str,
    status: AgentStatus,
    focused: bool,
    tree: RowTree,
    reserve_arrow: bool,
    width: f32,
    workspace_icon: Option<Arc<Image>>,
    arrow: Option<Stateful<Div>>,
    pr: Option<PrBadge>,
    appearance: (&FontConfig, &Theme),
) -> Div {
    let (font, theme) = appearance;
    let icon_reserve = if workspace_icon.is_some() {
        ICON_RESERVE
    } else {
        0.
    };
    let indent = if tree == RowTree::None {
        0.
    } else {
        CHILD_INDENT
    };
    let pr_reserve = pr
        .as_ref()
        .map(|badge| badge.width(font) + LABEL_GAP)
        .unwrap_or_default();
    let arrow_reserve = if reserve_arrow { ARROW_RESERVE } else { 0. };
    let arrow_absent = arrow.is_none();
    let label_width = (width
        - 1.
        - 2. * ROW_PADDING
        - STATUS_WIDTH
        - LABEL_GAP
        - indent
        - pr_reserve
        - arrow_reserve)
        .max(0.);
    div()
        .debug_selector(|| format!("row-{name}"))
        .h(px(2. * line_height(font) + 8.))
        .w_full()
        .min_w_0()
        .flex_none()
        .relative()
        .pl(px(ROW_PADDING + indent))
        .pr(px(ROW_PADDING))
        .flex()
        .items_start()
        .gap(px(LABEL_GAP))
        .py(px(4.))
        .cursor_pointer()
        .when(focused, |s| s.bg(rgb(theme.active)))
        .hover(|s| s.bg(rgb(theme.active)))
        // Tree lines run in the indent the row already reserves, so a child is
        // tied to its parent without box-drawing glyphs in the label.
        .when(tree != RowTree::None, |row| {
            let (color, font) = (theme.muted, font.clone());
            row.child(
                div()
                    .debug_selector(|| format!("tree-{name}"))
                    .absolute()
                    // Between the parent's label column and this row's own dot.
                    .left(px(TREE_GUTTER))
                    .w(px(ROW_PADDING + CHILD_INDENT - TREE_GUTTER))
                    .top_0()
                    .bottom_0()
                    .child(
                        canvas(
                            |_, _, _| (),
                            move |bounds, _, window, _| {
                                for line in tree_lines(bounds, tree, &font, window.scale_factor()) {
                                    window.paint_quad(fill(line, rgb(color)));
                                }
                            },
                        )
                        .size_full(),
                    ),
            )
        })
        .child(status_indicator(status, font))
        .child(
            div()
                .flex()
                .flex_col()
                // Avoid zero-basis measurement: GPUI 0.2.2 mutates text run
                // lengths when truncating and reuses them on wider measurements.
                .w(px(label_width))
                .flex_none()
                .overflow_hidden()
                .debug_selector(|| format!("column-{name}"))
                .child(
                    div()
                        .relative()
                        .w(px(label_width))
                        .h(px(line_height(font)))
                        .when_some(workspace_icon, |title, image| {
                            title.child(
                                div()
                                    .debug_selector(|| format!("github-{name}"))
                                    .absolute()
                                    .left_0()
                                    .top(px((line_height(font) - 12.) / 2.))
                                    .size(px(12.))
                                    .flex_none()
                                    .overflow_hidden()
                                    .child(
                                        img(image)
                                            .size_full()
                                            .rounded_full()
                                            .with_fallback(|| {
                                                img(GITHUB_ICON.clone())
                                                    .size_full()
                                                    .rounded_full()
                                                    .into_any_element()
                                            })
                                            .with_loading(|| {
                                                img(GITHUB_ICON.clone())
                                                    .size_full()
                                                    .rounded_full()
                                                    .into_any_element()
                                            }),
                                    ),
                            )
                        })
                        .child(
                            div()
                                .debug_selector(|| format!("name-{name}"))
                                .ml(px(icon_reserve))
                                .w(px((label_width - icon_reserve).max(0.)))
                                .flex_none()
                                .truncate()
                                .child(label_text(name)),
                        ),
                )
                .child(
                    div()
                        .debug_selector(|| format!("detail-{name}"))
                        .w(px(label_width))
                        .truncate()
                        .text_color(rgb(theme.muted))
                        .child(label_text(detail)),
                ),
        )
        // The collapse column comes first so the badge can hug the row's edge;
        // a reserved-but-empty column keeps every badge on the same right edge.
        .when_some(arrow, |row, arrow| row.child(arrow))
        .when(arrow_absent && reserve_arrow, |row| {
            row.child(div().w(px(ARROW_RESERVE - LABEL_GAP)).flex_none())
        })
        .when_some(pr, |row, badge| {
            row.child(
                div()
                    .debug_selector(|| format!("pr-{name}"))
                    .w(px(badge.width(font)))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .items_end()
                    .overflow_hidden()
                    .child(
                        div()
                            .h(px(line_height(font)))
                            .flex_none()
                            .truncate()
                            .text_color(rgb(badge.color))
                            .child(label_text(&badge.number)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .overflow_hidden()
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(rgb(theme.palette[2]))
                                    .child(label_text(&badge.additions)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(rgb(theme.muted))
                                    .child(label_text("/")),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(rgb(theme.palette[1]))
                                    .child(label_text(&badge.deletions)),
                            ),
                    ),
            )
        })
}

#[cfg(any(test, feature = "integration-test"))]
pub(crate) mod layout_tests;

#[cfg(all(feature = "integration-test", target_os = "macos"))]
pub(crate) mod native_tests;

#[cfg(not(any(test, feature = "integration-test")))]
pub(crate) fn label_text(text: &str) -> SharedString {
    text.to_owned().into()
}

#[cfg(any(test, feature = "integration-test"))]
pub(crate) fn label_text(text: &str) -> layout_tests::ProbeText {
    layout_tests::ProbeText(text.to_owned().into())
}

fn first_text<'a>(values: impl IntoIterator<Item = Option<&'a str>>, fallback: &'a str) -> &'a str {
    values
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or(fallback)
}

fn agent_labels(agent: &ClientShellAgent) -> (&str, &str) {
    let kind = first_text(
        [agent.display_agent.as_deref(), agent.agent.as_deref()],
        "agent",
    );
    let name = first_text(
        [
            agent.name.as_deref(),
            agent.title.as_deref(),
            agent.terminal_title_stripped.as_deref(),
        ],
        kind,
    );
    (name, kind)
}

// Match the expanded upstream shell order, including orphaned linked worktrees.
fn workspace_entries(workspaces: &[ClientShellWorkspace]) -> Vec<(usize, bool)> {
    let mut groups = HashMap::<&str, (Option<usize>, Vec<usize>)>::new();
    for (index, workspace) in workspaces.iter().enumerate() {
        if let Some(worktree) = &workspace.worktree {
            let (parent, members) = groups.entry(&worktree.key).or_default();
            if !worktree.is_linked_worktree && parent.is_none() {
                *parent = Some(index);
            }
            members.push(index);
        }
    }
    let mut emitted = HashSet::new();
    let mut entries = Vec::with_capacity(workspaces.len());
    for (index, workspace) in workspaces.iter().enumerate() {
        let group = workspace.worktree.as_ref().and_then(|tree| {
            let (parent, members) = groups.get(tree.key.as_str())?;
            Some((tree.key.as_str(), (*parent)?, members))
        });
        if let Some((key, parent, members)) = group {
            if emitted.insert(key) {
                entries.push((parent, false));
                entries.extend(members.iter().filter(|&&i| i != parent).map(|&i| (i, true)));
            }
        } else {
            entries.push((index, false));
        }
    }
    entries
}

fn visible_workspace_entries(
    workspaces: &[ClientShellWorkspace],
    collapsed: &HashSet<String>,
) -> Vec<(usize, bool, Option<String>)> {
    let entries = workspace_entries(workspaces);
    entries
        .iter()
        .enumerate()
        .filter_map(|(position, &(index, child))| {
            let key = workspaces[index].worktree.as_ref().map(|tree| &tree.key);
            if child && key.is_some_and(|key| collapsed.contains(key)) {
                return None;
            }
            let group = (!child && entries.get(position + 1).is_some_and(|entry| entry.1))
                .then(|| key.cloned())
                .flatten();
            Some((index, child, group))
        })
        .collect()
}

/// Cached pull request for a worktree row, if the prefetch already has one.
/// Rendering only reads: a missing entry simply shows no badge.
fn workspace_pr(
    workspace: &ClientShellWorkspace,
    cache: &crate::pull_request::Cache,
    theme: &Theme,
) -> Option<PrBadge> {
    let key = workspace.worktree.as_ref()?.key.as_str();
    let branch = workspace.branch.as_deref()?;
    cache.peek(key, branch).map(|pr| PrBadge::new(pr, theme))
}

fn workspace_label(workspace: &ClientShellWorkspace, indented: bool) -> &str {
    let branch = (indented && !workspace.custom_label)
        .then_some(workspace.branch.as_deref())
        .flatten()
        .map(|branch| branch.strip_prefix("worktree/").unwrap_or(branch));
    first_text([branch, Some(&workspace.label)], "workspace")
}

fn status_indicator(status: AgentStatus, font: &FontConfig) -> Div {
    // Upstream dots: working/blocked/done filled, idle hollow, unknown a small dot.
    let (diameter, filled, color) = status_style(status);
    div()
        .size(px(STATUS_WIDTH))
        .mt(px((line_height(font) - STATUS_WIDTH) / 2.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .size(px(diameter))
                .rounded_full()
                .border_1()
                .border_color(rgb(color))
                .when(filled, |dot| dot.bg(rgb(color))),
        )
}

/// Upstream draws status from its own palette, defaulting to Catppuccin Mocha,
/// and never from the terminal's ANSI colors. Matching those literals keeps a
/// dot the same color in both clients whatever terminal theme is loaded, where
/// ANSI slots would drift: Xcode Dark paints its cyan purple.
fn status_style(status: AgentStatus) -> (f32, bool, u32) {
    match status {
        AgentStatus::Working => (STATUS_WIDTH, true, 0xf9e2af),
        AgentStatus::Blocked => (STATUS_WIDTH, true, 0xf38ba8),
        AgentStatus::Done => (STATUS_WIDTH, true, 0x94e2d5),
        AgentStatus::Idle => (STATUS_WIDTH, false, 0xa6e3a1),
        AgentStatus::Unknown => (STATUS_DOT_UNKNOWN, true, 0x6c7086),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::{
        AgentStatus, ClientShellAgent, ClientShellWorkspace, STATUS_DOT_UNKNOWN, STATUS_WIDTH,
        agent_labels, first_text, layout_tests, status_style, workspace_entries, workspace_label,
    };

    #[test]
    fn section_headings_use_the_configured_sidebar_font_size() {
        use gpui::{Styled, px};
        for size in [12., 16., 20.] {
            let font = super::FontConfig {
                family: "Menlo".into(),
                size,
            };
            for label in ["spaces", "agents"] {
                let mut heading = super::header(label, &font, &super::Theme::default());
                assert_eq!(
                    heading.text_style().as_ref().unwrap().font_size,
                    Some(px(size).into())
                );
            }
        }
    }

    #[test]
    fn hierarchy_uses_git_metadata_and_emits_each_workspace_once() {
        let mut workspaces = layout_tests::snapshot(7).workspaces;
        for workspace in &mut workspaces {
            workspace.worktree = None;
            workspace.label = "same label".into();
            workspace.branch = Some("main".into());
        }
        for (index, key, linked) in [
            (0, "/repo/.git", true),
            (2, "/repo/.git", false),
            (3, "/orphan/.git", true),
            (4, "/repo/.git", true),
            (5, "/other/.git", false),
            (6, "/orphan/.git", true),
        ] {
            workspaces[index].worktree = Some(herdr_client::protocol::ClientShellWorktree {
                key: key.into(),
                label: "same repo name".into(),
                is_linked_worktree: linked,
            });
        }
        workspaces[2].branch = Some("develop".into());
        assert_eq!(
            workspace_entries(&workspaces),
            vec![
                (2, false),
                (0, true),
                (4, true),
                (1, false),
                (3, false),
                (5, false),
                (6, false),
            ]
        );
        workspaces[2].worktree = None;
        assert_eq!(
            workspace_entries(&workspaces),
            (0..7).map(|i| (i, false)).collect::<Vec<_>>()
        );
        assert!(workspace_entries(&[]).is_empty());
    }

    #[test]
    fn collapse_uses_repository_identity_without_mutating_selection() {
        let mut workspaces = layout_tests::snapshot(7).workspaces;
        workspaces[4].focused = true;
        let before = workspaces.clone();
        let collapsed = std::collections::HashSet::from(["/fixture/agent-launcher/.git".into()]);
        let entries = super::visible_workspace_entries(&workspaces, &collapsed);
        assert_eq!(
            entries.iter().map(|entry| entry.0).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 6]
        );
        assert_eq!(entries.iter().filter(|entry| entry.2.is_some()).count(), 1);
        assert_eq!(workspaces, before);
        workspaces[3].label = "renamed".into();
        assert_eq!(
            super::visible_workspace_entries(&workspaces, &collapsed).len(),
            5
        );
        workspaces.remove(5);
        workspaces.remove(4);
        assert!(
            super::visible_workspace_entries(&workspaces, &collapsed)
                .iter()
                .all(|entry| entry.2.is_none())
        );
        workspaces.remove(3);
        assert_eq!(
            super::visible_workspace_entries(&workspaces, &collapsed).len(),
            4
        );
    }

    #[test]
    fn child_labels_follow_upstream_custom_label_and_branch_rules() {
        let mut workspace = layout_tests::snapshot(1).workspaces.remove(0);
        workspace.branch = Some("worktree/fix-sidebar".into());
        assert_eq!(workspace_label(&workspace, true), "fix-sidebar");
        assert_eq!(workspace_label(&workspace, false), "herdr");
        workspace.custom_label = true;
        assert_eq!(workspace_label(&workspace, true), "herdr");
        workspace.custom_label = false;
        workspace.branch = None;
        assert_eq!(workspace_label(&workspace, true), "herdr");
    }

    #[test]
    fn text_fallback_skips_missing_and_blank_metadata() {
        assert_eq!(first_text([None, Some(" \t"), Some(" main ")], ""), "main");
        assert_eq!(first_text([None, Some("")], ""), "");
        assert_eq!(first_text([Some(" ")], "workspace"), "workspace");
    }

    #[test]
    fn agent_name_title_and_kind_fallbacks() {
        let mut agent = ClientShellAgent {
            pane_id: "p".into(),
            workspace_id: "w".into(),
            tab_id: "t".into(),
            name: Some("review".into()),
            title: Some("Fix sidebar".into()),
            display_agent: Some("Claude Code".into()),
            agent: Some("claude".into()),
            terminal_title: Some("raw title".into()),
            terminal_title_stripped: Some("terminal".into()),
            agent_status: AgentStatus::Unknown,
            state_change_seq: 0,
            state_labels: vec![],
            tokens: vec![],
            focused: false,
        };
        assert_eq!(agent_labels(&agent), ("review", "Claude Code"));
        agent.name = Some(" ".into());
        assert_eq!(agent_labels(&agent), ("Fix sidebar", "Claude Code"));
        agent.title = None;
        agent.display_agent = None;
        assert_eq!(agent_labels(&agent), ("terminal", "claude"));
        agent.terminal_title_stripped = None;
        assert_eq!(agent_labels(&agent), ("claude", "claude"));
        agent.agent = None;
        assert_eq!(agent_labels(&agent), ("agent", "agent"));
    }

    #[test]
    fn status_colors_match_upstream_and_ignore_the_theme() {
        // The literals are upstream's default palette (Catppuccin Mocha), which
        // its status dots use whatever terminal colors are loaded.
        for (status, color) in [
            (AgentStatus::Working, 0xf9e2af),
            (AgentStatus::Blocked, 0xf38ba8),
            (AgentStatus::Done, 0x94e2d5),
            (AgentStatus::Idle, 0xa6e3a1),
            (AgentStatus::Unknown, 0x6c7086),
        ] {
            assert_eq!(status_style(status).2, color);
        }
        for name in crate::config::Theme::BUILTIN_NAMES {
            let theme = crate::config::Theme::builtin(name).unwrap();
            for status in [
                AgentStatus::Working,
                AgentStatus::Blocked,
                AgentStatus::Done,
                AgentStatus::Idle,
                AgentStatus::Unknown,
            ] {
                let color = status_style(status).2;
                assert!(
                    !theme.palette.contains(&color) || theme.palette[..16].contains(&color),
                    "{name}: dots must not be read out of the theme"
                );
            }
        }
    }

    #[test]
    fn status_shapes_match_upstream_dots_and_wire_casing() {
        let snapshot = layout_tests::snapshot(1);
        for (wire, status) in [
            ("idle", AgentStatus::Idle),
            ("working", AgentStatus::Working),
            ("blocked", AgentStatus::Blocked),
            ("done", AgentStatus::Done),
            ("unknown", AgentStatus::Unknown),
        ] {
            let mut value = serde_json::to_value(&snapshot.workspaces[0]).unwrap();
            value["agent_status"] = wire.into();
            let workspace: ClientShellWorkspace = serde_json::from_value(value).unwrap();
            assert_eq!(workspace.agent_status, status);
            let mut value = serde_json::to_value(&snapshot.agents[0]).unwrap();
            value["agent_status"] = wire.into();
            let agent: ClientShellAgent = serde_json::from_value(value).unwrap();
            assert_eq!(agent.agent_status, status);
            assert_eq!(serde_json::to_value(status).unwrap(), wire);
            let (diameter, filled, color) = status_style(status);
            assert_eq!(
                color,
                match status {
                    AgentStatus::Working => 0xf9e2af,
                    AgentStatus::Blocked => 0xf38ba8,
                    AgentStatus::Done => 0x94e2d5,
                    AgentStatus::Idle => 0xa6e3a1,
                    AgentStatus::Unknown => 0x6c7086,
                }
            );
            assert_eq!(filled, status != AgentStatus::Idle);
            assert_eq!(
                diameter,
                if status == AgentStatus::Unknown {
                    STATUS_DOT_UNKNOWN
                } else {
                    STATUS_WIDTH
                }
            );
        }
    }
}
