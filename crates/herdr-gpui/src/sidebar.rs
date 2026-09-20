use super::{Command, HerdrWindow, NavigationTarget};
use gpui::{prelude::*, *};
use herdr_client::protocol::{AgentStatus, ClientShellAgent, ClientShellWorkspace};
use std::collections::{HashMap, HashSet};

pub(super) const BACKGROUND: u32 = 0x1c1c22;
pub(super) const FOREGROUND: u32 = 0xc1bdce;
const MUTED: u32 = 0x827e91;
pub(super) const ACTIVE: u32 = 0x2b2933;
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
        #[cfg(feature = "integration-test")]
        {
            spaces = spaces.track_scroll(&self.sidebar_scroll[0]);
            agents = agents.track_scroll(&self.sidebar_scroll[1]);
        }
        let multi = self.endpoints.len() > 1;
        let mut agent_count = 0;
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
                        .h(px(32.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(12.))
                        .when(selected, |row| row.bg(rgb(ACTIVE)))
                        .text_color(rgb(if endpoint.enabled { FOREGROUND } else { MUTED }))
                        .cursor_pointer()
                        .child(
                            div()
                                .id(SharedString::from(format!("collapse-host-{endpoint_id}")))
                                .w(px(12.))
                                .child(if endpoint.collapsed { ">" } else { "v" })
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
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(endpoint.label.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(9.))
                                .text_color(rgb(MUTED))
                                .child(endpoint.status()),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.select_endpoint(&select_id, cx);
                            window.focus(&this.focus);
                        })),
                );
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
            for (index, indented, group) in
                visible_workspace_entries(&snapshot.workspaces, collapsed_repos)
            {
                if multi && endpoint.collapsed {
                    break;
                }
                let workspace = &snapshot.workspaces[index];
                let id = workspace.workspace_id.clone();
                let navigate_endpoint = endpoint_id.clone();
                let collapse_endpoint = endpoint_id.clone();
                spaces = spaces.child(
                    row(
                        workspace_label(workspace, indented),
                        first_text([workspace.branch.as_deref()], ""),
                        workspace.agent_status,
                        selected && workspace.focused,
                        indented,
                        group.is_some() || indented,
                    )
                    .when_some(group, |row, key| {
                        let collapsed = collapsed_repos.contains(&key);
                        row.child(
                            div()
                                .id(SharedString::from(format!("collapse-{endpoint_id}-{id}")))
                                .debug_selector(move || format!("collapse-{index}"))
                                .w(px(ARROW_RESERVE - LABEL_GAP))
                                .h(px(32.))
                                .flex_none()
                                .child(label_text(if collapsed { ">" } else { "v" }))
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
                                })),
                        )
                    })
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
            for agent in &snapshot.agents {
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
                        false,
                        false,
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
        if agent_count == 0 {
            agents = agents.child(
                div()
                    .px(px(12.))
                    .text_color(rgb(MUTED))
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
            .font_family("Menlo")
            .text_size(px(12.))
            .line_height(px(16.))
            .text_color(rgb(FOREGROUND))
            .bg(rgb(BACKGROUND))
            .border_r_1()
            .border_color(rgb(ACTIVE))
            // Zero flex bases keep long workspace lists from displacing agents.
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(header("spaces"))
                    .child(spaces)
                    .child(
                        div()
                            .flex_none()
                            .h(px(26.))
                            .px(px(12.))
                            .flex()
                            .items_center()
                            .text_color(rgb(MUTED))
                            .gap(px(20.))
                            .child(
                                div()
                                    .id("new-workspace")
                                    .cursor_pointer()
                                    .hover(|s| s.text_color(rgb(FOREGROUND)))
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
                                    .hover(|s| s.text_color(rgb(FOREGROUND)))
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
            .child(div().h(px(1.)).flex_none().bg(rgb(ACTIVE)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(header("agents"))
                    .child(agents),
            )
    }
}

fn header(label: &'static str) -> Div {
    div()
        .flex_none()
        .h(px(28.))
        .px(px(12.))
        .flex()
        .items_center()
        .text_size(px(10.))
        .text_color(rgb(MUTED))
        .child(label)
}

fn row(
    name: &str,
    detail: &str,
    status: AgentStatus,
    focused: bool,
    indented: bool,
    reserve_arrow: bool,
) -> Div {
    let indent = if indented { CHILD_INDENT } else { 0. };
    let label_width = LABEL_WIDTH - indent - if reserve_arrow { ARROW_RESERVE } else { 0. };
    div()
        .debug_selector(|| format!("row-{name}"))
        .h(px(40.))
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
        .when(focused, |s| s.bg(rgb(ACTIVE)))
        .hover(|s| s.bg(rgb(0x26252e)))
        .child(status_indicator(status))
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
                        .text_color(rgb(MUTED))
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

fn status_indicator(status: AgentStatus) -> Div {
    // Upstream dots: working/blocked/done filled, idle hollow, unknown a small dot.
    let (diameter, filled, color) = status_style(status);
    div()
        .size(px(STATUS_WIDTH))
        .mt(px(5.))
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

fn status_style(status: AgentStatus) -> (f32, bool, u32) {
    match status {
        AgentStatus::Working => (STATUS_WIDTH, true, 0xf9e2af),
        AgentStatus::Blocked => (STATUS_WIDTH, true, 0xf38ba8),
        AgentStatus::Done => (STATUS_WIDTH, true, 0x94e2d5),
        AgentStatus::Idle => (STATUS_WIDTH, false, 0xa6e3a1),
        AgentStatus::Unknown => (2., true, MUTED),
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
                    AgentStatus::Unknown => super::MUTED,
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
