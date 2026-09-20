//! Client-local seen/done semantics, matching Herdr's EndpointAgentPresentation.
//! A filled dot can mean an unseen completion, not only a working agent.
use crate::protocol::{AgentStatus, ClientShellAgent, ClientShellSnapshot, PaneSurfaceFrame};
use std::collections::HashMap;

/// Pure status projection. Only idle/done depend on this client's acknowledgement.
pub fn effective_agent_status(agent: &ClientShellAgent, acknowledged: Option<u64>) -> AgentStatus {
    match agent.agent_status {
        AgentStatus::Idle | AgentStatus::Done => {
            if acknowledged.is_some_and(|sequence| sequence >= agent.state_change_seq) {
                AgentStatus::Idle
            } else {
                AgentStatus::Done
            }
        }
        status => status,
    }
}

pub fn status_priority(status: AgentStatus) -> u8 {
    match status {
        AgentStatus::Blocked => 4,
        AgentStatus::Done => 3,
        AgentStatus::Working => 2,
        AgentStatus::Idle => 1,
        AgentStatus::Unknown => 0,
    }
}

#[derive(Clone, Debug, Default)]
pub struct AgentPresentation {
    boot_id: Option<String>,
    acknowledged: HashMap<String, u64>,
}

impl AgentPresentation {
    /// Establish a seen baseline on first attach/new boot, then retain unseen
    /// transitions until a coherent surface is actually presented by this client.
    pub fn project_snapshot(&mut self, snapshot: &mut ClientShellSnapshot) {
        if self.boot_id.as_deref() != Some(&snapshot.boot_id) {
            self.boot_id = Some(snapshot.boot_id.clone());
            self.acknowledged = snapshot
                .agents
                .iter()
                .map(|agent| (agent.pane_id.clone(), agent.state_change_seq))
                .collect();
        }
        self.acknowledged.retain(|pane_id, _| {
            snapshot
                .agents
                .iter()
                .any(|agent| &agent.pane_id == pane_id)
        });
        for agent in &mut snapshot.agents {
            agent.agent_status =
                effective_agent_status(agent, self.acknowledged.get(&agent.pane_id).copied());
        }
        for tab in &mut snapshot.tabs {
            if let Some(status) = snapshot
                .agents
                .iter()
                .filter(|agent| agent.tab_id == tab.tab_id)
                .map(|agent| agent.agent_status)
                .max_by_key(|status| status_priority(*status))
            {
                tab.agent_status = status;
            }
        }
        for workspace in &mut snapshot.workspaces {
            if let Some(status) = snapshot
                .agents
                .iter()
                .filter(|agent| agent.workspace_id == workspace.workspace_id)
                .map(|agent| agent.agent_status)
                .max_by_key(|status| status_priority(*status))
            {
                workspace.agent_status = status;
            }
        }
    }

    /// Call on presentation, not on socket receipt. Off-screen panes and stale
    /// surfaces must not consume completion indicators.
    pub fn acknowledge_surface(
        &mut self,
        snapshot: &mut ClientShellSnapshot,
        surface: &PaneSurfaceFrame,
        focused: bool,
    ) -> bool {
        if !focused
            || self.boot_id.as_deref() != Some(&surface.boot_id)
            || snapshot.boot_id != surface.boot_id
            || snapshot.revision != surface.projection_revision
        {
            return false;
        }
        let mut changed = false;
        for pane in &surface.panes {
            let Some(agent) = snapshot
                .agents
                .iter()
                .find(|agent| agent.pane_id == pane.pane_id)
            else {
                continue;
            };
            let sequence = self.acknowledged.entry(agent.pane_id.clone()).or_default();
            if *sequence < agent.state_change_seq {
                *sequence = agent.state_change_seq;
                changed = true;
            }
        }
        if changed {
            self.project_snapshot(snapshot);
        }
        changed
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn snapshot() -> ClientShellSnapshot {
        serde_json::from_str(include_str!(
            "../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
        ))
        .unwrap()
    }

    #[test]
    fn decode_and_projection_preserve_real_activity_and_unknown_statuses() {
        let fixture = snapshot();
        let mut agent = fixture.agents[0].clone();
        for (wire, status) in [
            ("working", AgentStatus::Working),
            ("blocked", AgentStatus::Blocked),
            ("future-status", AgentStatus::Unknown),
        ] {
            let mut value = serde_json::to_value(&agent).unwrap();
            value["agent_status"] = wire.into();
            agent = serde_json::from_value(value).unwrap();
            assert_eq!(effective_agent_status(&agent, None), status);
            assert_eq!(effective_agent_status(&agent, Some(u64::MAX)), status);
        }
        for status in [AgentStatus::Idle, AgentStatus::Done] {
            agent.agent_status = status;
            assert_eq!(effective_agent_status(&agent, None), AgentStatus::Done);
            assert_eq!(effective_agent_status(&agent, Some(8)), AgentStatus::Done);
            assert_eq!(effective_agent_status(&agent, Some(9)), AgentStatus::Idle);
        }
    }

    #[test]
    fn aggregates_use_tui_priority_and_preserve_agentless_resources() {
        let mut presentation = AgentPresentation::default();
        let mut initial = snapshot();
        initial.agents[0].agent_status = AgentStatus::Idle;
        presentation.project_snapshot(&mut initial);
        let mut next = initial.clone();
        next.agents[0].state_change_seq += 1;
        for (status, expected) in [
            (AgentStatus::Working, AgentStatus::Done),
            (AgentStatus::Blocked, AgentStatus::Blocked),
        ] {
            let mut next = next.clone();
            let mut additional = next.agents[0].clone();
            additional.pane_id = "second".into();
            additional.agent_status = status;
            next.agents.push(additional);
            presentation.project_snapshot(&mut next);
            assert_eq!(next.tabs[0].agent_status, expected);
            assert_eq!(next.workspaces[0].agent_status, expected);
        }
        next.agents.clear();
        next.tabs[0].agent_status = AgentStatus::Unknown;
        next.workspaces[0].agent_status = AgentStatus::Working;
        presentation.project_snapshot(&mut next);
        assert_eq!(next.tabs[0].agent_status, AgentStatus::Unknown);
        assert_eq!(next.workspaces[0].agent_status, AgentStatus::Working);
    }
}
