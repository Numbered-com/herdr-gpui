use super::{Command, HerdrWindow};
use crate::config::{FontConfig, Theme};
use gpui::{prelude::*, *};
use herdr_client::protocol::{AgentStatus, ClientShellAgent, ClientShellWorkspace};
use std::collections::{HashMap, HashSet};

const SIDEBAR_WIDTH: f32 = 232.;
const ROW_PADDING: f32 = 12.;
const STATUS_WIDTH: f32 = 5.;
const LABEL_GAP: f32 = 8.;
const CHILD_INDENT: f32 = 16.;
const ARROW_RESERVE: f32 = 18.;
pub(super) const LABEL_WIDTH: f32 =
    SIDEBAR_WIDTH - 1. - 2. * ROW_PADDING - STATUS_WIDTH - LABEL_GAP;

impl HerdrWindow {
    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> Stateful<Div> {
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
        if let Some(snapshot) = &self.live.snapshot {
            #[cfg(feature = "integration-test")]
            {
                spaces = spaces.track_scroll(&self.sidebar_scroll[0]);
                agents = agents.track_scroll(&self.sidebar_scroll[1]);
            }
            for (index, indented, group) in
                visible_workspace_entries(&snapshot.workspaces, &self.collapsed_repos)
            {
                let workspace = &snapshot.workspaces[index];
                let id = workspace.workspace_id.clone();
                spaces = spaces.child(
                    row(
                        workspace_label(workspace, indented),
                        first_text([workspace.branch.as_deref()], ""),
                        workspace.agent_status,
                        workspace.focused,
                        indented,
                        group.is_some() || indented,
                        (font, theme),
                    )
                    .when_some(group, |row, key| {
                        let collapsed = self.collapsed_repos.contains(&key);
                        row.child(
                            div()
                                .id(SharedString::from(format!("collapse-{id}")))
                                .debug_selector(move || format!("collapse-{index}"))
                                .w(px(ARROW_RESERVE - LABEL_GAP))
                                .h(px(2. * line_height(font)))
                                .flex_none()
                                .child(label_text(if collapsed { ">" } else { "v" }))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    if !this.collapsed_repos.remove(&key) {
                                        this.collapsed_repos.insert(key.clone());
                                    }
                                    cx.notify();
                                })),
                        )
                    })
                    .id(SharedString::from(format!("workspace-{id}")))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.navigate("workspace", &id, cx);
                        window.focus(&this.focus);
                    })),
                );
            }
            for agent in &snapshot.agents {
                let id = agent.pane_id.clone();
                let (name, kind) = agent_labels(agent);
                agents = agents.child(
                    row(
                        name,
                        kind,
                        agent.agent_status,
                        agent.focused,
                        false,
                        false,
                        (font, theme),
                    )
                    .id(SharedString::from(format!("agent-{id}")))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.navigate("pane", &id, cx);
                        window.focus(&this.focus);
                    })),
                );
            }
        }
        if self
            .live
            .snapshot
            .as_ref()
            .is_none_or(|s| s.agents.is_empty())
        {
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
            .w(px(SIDEBAR_WIDTH))
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
                    .child(header("agents", font, theme))
                    .child(agents),
            )
    }
}

// Preserve the sidebar's compact 12px font / 16px line defaults as fonts scale.
fn line_height(font: &FontConfig) -> f32 {
    font.size * 4. / 3.
}

fn header(label: &'static str, font: &FontConfig, theme: &Theme) -> Div {
    div()
        .flex_none()
        .h(px(line_height(font) + 12.))
        .px(px(12.))
        .flex()
        .items_center()
        .text_size(px(font.size * 5. / 6.))
        .text_color(rgb(theme.muted))
        .child(label)
}

fn row(
    name: &str,
    detail: &str,
    status: AgentStatus,
    focused: bool,
    indented: bool,
    reserve_arrow: bool,
    appearance: (&FontConfig, &Theme),
) -> Div {
    let (font, theme) = appearance;
    let indent = if indented { CHILD_INDENT } else { 0. };
    let label_width = LABEL_WIDTH - indent - if reserve_arrow { ARROW_RESERVE } else { 0. };
    div()
        .debug_selector(|| format!("row-{name}"))
        .h(px(2. * line_height(font) + 8.))
        .w_full()
        .min_w_0()
        .flex_none()
        .pl(px(ROW_PADDING + indent))
        .pr(px(ROW_PADDING))
        .flex()
        .items_start()
        .gap(px(LABEL_GAP))
        .py(px(4.))
        .cursor_pointer()
        .when(focused, |s| s.bg(rgb(theme.active)))
        .hover(|s| s.bg(rgb(theme.active)))
        .child(status_indicator(status, font, theme))
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
                        .debug_selector(|| format!("name-{name}"))
                        .w(px(label_width))
                        .truncate()
                        .child(label_text(name)),
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
}

#[cfg(any(test, feature = "integration-test"))]
pub(crate) mod layout_tests;

#[cfg(all(feature = "integration-test", target_os = "macos"))]
pub(crate) mod native_tests;

#[cfg(not(any(test, feature = "integration-test")))]
fn label_text(text: &str) -> SharedString {
    text.to_owned().into()
}

#[cfg(any(test, feature = "integration-test"))]
fn label_text(text: &str) -> layout_tests::ProbeText {
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

fn workspace_label(workspace: &ClientShellWorkspace, indented: bool) -> &str {
    let branch = (indented && !workspace.custom_label)
        .then_some(workspace.branch.as_deref())
        .flatten()
        .map(|branch| branch.strip_prefix("worktree/").unwrap_or(branch));
    first_text([branch, Some(&workspace.label)], "workspace")
}

fn status_indicator(status: AgentStatus, font: &FontConfig, theme: &Theme) -> Div {
    // Upstream dots: working/blocked/done filled, idle hollow, unknown a small dot.
    let (diameter, filled, color) = status_style(status, theme);
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

fn status_style(status: AgentStatus, theme: &Theme) -> (f32, bool, u32) {
    match status {
        AgentStatus::Working => (STATUS_WIDTH, true, theme.palette[3]),
        AgentStatus::Blocked => (STATUS_WIDTH, true, theme.palette[1]),
        AgentStatus::Done => (STATUS_WIDTH, true, theme.palette[6]),
        AgentStatus::Idle => (STATUS_WIDTH, false, theme.palette[2]),
        AgentStatus::Unknown => (2., true, theme.muted),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::{
        AgentStatus, ClientShellAgent, ClientShellWorkspace, STATUS_WIDTH, agent_labels,
        first_text, layout_tests, status_style, workspace_entries, workspace_label,
    };

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
    fn status_colors_follow_the_supplied_theme() {
        let mut theme = crate::config::Theme::default();
        theme.palette[1] = 0x112233;
        theme.palette[2] = 0x223344;
        theme.palette[3] = 0x334455;
        theme.palette[6] = 0x667788;
        theme.muted = 0x778899;
        for (status, color) in [
            (AgentStatus::Blocked, 0x112233),
            (AgentStatus::Idle, 0x223344),
            (AgentStatus::Working, 0x334455),
            (AgentStatus::Done, 0x667788),
            (AgentStatus::Unknown, 0x778899),
        ] {
            assert_eq!(status_style(status, &theme).2, color);
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
            let theme = crate::config::Theme::default();
            let (diameter, filled, color) = status_style(status, &theme);
            assert_eq!(
                color,
                match status {
                    AgentStatus::Working => theme.palette[3],
                    AgentStatus::Blocked => theme.palette[1],
                    AgentStatus::Done => theme.palette[6],
                    AgentStatus::Idle => theme.palette[2],
                    AgentStatus::Unknown => theme.muted,
                }
            );
            assert_eq!(filled, status != AgentStatus::Idle);
            assert_eq!(
                diameter,
                if status == AgentStatus::Unknown {
                    2.
                } else {
                    STATUS_WIDTH
                }
            );
        }
    }
}
