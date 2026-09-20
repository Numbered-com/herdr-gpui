use super::{Command, HerdrWindow};
use gpui::{prelude::*, *};
use herdr_client::protocol::{AgentStatus, ClientShellAgent};

const BACKGROUND: u32 = 0x1c1c22;
const FOREGROUND: u32 = 0xc1bdce;
const MUTED: u32 = 0x827e91;
const ACTIVE: u32 = 0x2b2933;

impl HerdrWindow {
    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut spaces = div()
            .id("spaces-scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
        let mut agents = div()
            .id("agents-scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
        if let Some(snapshot) = &self.live.snapshot {
            for workspace in &snapshot.workspaces {
                let id = workspace.workspace_id.clone();
                spaces = spaces.child(
                    row(
                        first_text([Some(workspace.label.as_str())], "workspace"),
                        first_text([workspace.branch.as_deref()], ""),
                        workspace.agent_status,
                        workspace.focused,
                    )
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
                    row(name, kind, agent.agent_status, agent.focused)
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
                    .text_color(rgb(MUTED))
                    .truncate()
                    .child("no agents"),
            );
        }
        div()
            .id("sidebar")
            .w(px(232.))
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
                            .id("new-workspace")
                            .flex_none()
                            .h(px(26.))
                            .px(px(12.))
                            .flex()
                            .items_center()
                            .text_color(rgb(MUTED))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(ACTIVE)).text_color(rgb(FOREGROUND)))
                            .child("new")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.command(Command::Workspace, window, cx)
                            })),
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

fn row(name: &str, detail: &str, status: AgentStatus, focused: bool) -> Div {
    div()
        .h(px(40.))
        .w_full()
        .min_w_0()
        .flex_none()
        .px(px(12.))
        .flex()
        .items_start()
        .gap(px(8.))
        .py(px(4.))
        .cursor_pointer()
        .when(focused, |s| s.bg(rgb(ACTIVE)))
        .hover(|s| s.bg(rgb(0x26252e)))
        .child(
            div()
                .size(px(5.))
                .mt(px(5.))
                .flex_none()
                .rounded_full()
                .bg(rgb(status_color(status))),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .child(div().min_w_0().truncate().child(name.to_owned()))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(rgb(MUTED))
                        .child(detail.to_owned()),
                ),
        )
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

fn status_color(status: AgentStatus) -> u32 {
    match status {
        AgentStatus::Working => 0xb4a0d8,
        AgentStatus::Blocked => 0xd3ad79,
        AgentStatus::Done => 0x97b59b,
        AgentStatus::Idle => 0x8c92a7,
        AgentStatus::Unknown => 0x595563,
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentStatus, ClientShellAgent, agent_labels, first_text, status_color};

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
    fn snapshot_statuses_have_distinct_colors_including_unknown() {
        let colors = [
            AgentStatus::Idle,
            AgentStatus::Working,
            AgentStatus::Blocked,
            AgentStatus::Done,
            AgentStatus::Unknown,
        ]
        .map(status_color);
        for (index, color) in colors.iter().enumerate() {
            assert!(!colors[..index].contains(color));
        }
        assert_eq!(status_color(AgentStatus::Unknown), 0x595563);
    }
}
