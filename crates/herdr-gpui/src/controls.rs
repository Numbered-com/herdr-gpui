use herdr_client::protocol::ClientShellSnapshot;
use serde_json::{Value, json};

#[derive(Clone, Copy)]
pub enum Command {
    Workspace,
    Tab,
    SplitRight,
    SplitDown,
    NextTab,
    PreviousTab,
}

pub fn request(command: Command, snapshot: &ClientShellSnapshot) -> Option<(&'static str, Value)> {
    Some(match command {
        Command::Workspace => {
            let mut params = json!({"focus": true});
            if let Some(id) = &snapshot.focused_workspace_id {
                params["source_workspace_id"] = json!(id);
            }
            ("workspace.create", params)
        }
        Command::Tab => (
            "tab.create",
            json!({"workspace_id": snapshot.focused_workspace_id.as_ref()?, "focus": true}),
        ),
        Command::SplitRight | Command::SplitDown => (
            "pane.split",
            json!({
                "target_pane_id": snapshot.focused_pane_id.as_ref()?,
                "direction": if matches!(command, Command::SplitRight) { "right" } else { "down" },
                "focus": true,
            }),
        ),
        Command::NextTab | Command::PreviousTab => {
            let workspace = snapshot.focused_workspace_id.as_ref()?;
            let tabs: Vec<_> = snapshot
                .tabs
                .iter()
                .filter(|t| &t.workspace_id == workspace)
                .collect();
            let index = tabs
                .iter()
                .position(|t| Some(&t.tab_id) == snapshot.focused_tab_id.as_ref())?;
            let next = if matches!(command, Command::NextTab) {
                (index + 1) % tabs.len()
            } else {
                (index + tabs.len() - 1) % tabs.len()
            };
            ("tab.focus", json!({"tab_id": tabs[next].tab_id}))
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn snapshot() -> ClientShellSnapshot {
        serde_json::from_str(include_str!(
            "../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
        ))
        .unwrap()
    }

    #[test]
    fn creation_uses_daemon_cwd_and_explicit_targets() {
        let s = snapshot();
        assert_eq!(
            request(Command::Workspace, &s).unwrap(),
            (
                "workspace.create",
                json!({"source_workspace_id": s.focused_workspace_id, "focus": true})
            )
        );
        assert_eq!(
            request(Command::Tab, &s).unwrap(),
            (
                "tab.create",
                json!({"workspace_id": s.focused_workspace_id, "focus": true})
            )
        );
        for (command, direction) in [(Command::SplitRight, "right"), (Command::SplitDown, "down")] {
            assert_eq!(
                request(command, &s).unwrap(),
                (
                    "pane.split",
                    json!({"target_pane_id": s.focused_pane_id, "direction": direction, "focus": true})
                )
            );
        }
    }

    #[test]
    fn empty_session_can_create_workspace_only() {
        let mut s = snapshot();
        s.focused_workspace_id = None;
        s.focused_tab_id = None;
        s.focused_pane_id = None;
        s.tabs.clear();
        assert_eq!(
            request(Command::Workspace, &s).unwrap().1,
            json!({"focus": true})
        );
        for command in [
            Command::Tab,
            Command::SplitRight,
            Command::NextTab,
            Command::PreviousTab,
        ] {
            assert!(request(command, &s).is_none());
        }
    }

    #[test]
    fn tab_cycle_ignores_missing_or_foreign_focus() {
        let mut s = snapshot();
        let mut tab = s.tabs[0].clone();
        tab.tab_id = "foreign-tab".into();
        tab.workspace_id = "other-workspace".into();
        s.tabs.push(tab);
        for focus in [None, Some("removed-tab"), Some("foreign-tab")] {
            s.focused_tab_id = focus.map(str::to_owned);
            for command in [Command::NextTab, Command::PreviousTab] {
                assert!(request(command, &s).is_none());
            }
        }
        s.tabs.clear();
        for command in [Command::NextTab, Command::PreviousTab] {
            assert!(request(command, &s).is_none());
        }
    }

    #[test]
    fn tab_cycle_wraps_and_stays_in_workspace() {
        let mut s = snapshot();
        let mut tab = s.tabs[0].clone();
        tab.workspace_id = s.focused_workspace_id.clone().unwrap();
        tab.tab_id = "first".into();
        let mut second = tab.clone();
        second.tab_id = "second".into();
        let mut other = tab.clone();
        other.workspace_id = "other".into();
        s.tabs = vec![tab, other, second];
        s.focused_tab_id = Some("first".into());
        for command in [Command::NextTab, Command::PreviousTab] {
            assert_eq!(request(command, &s).unwrap().1, json!({"tab_id": "second"}));
        }
        s.focused_tab_id = Some("second".into());
        assert_eq!(
            request(Command::NextTab, &s).unwrap().1,
            json!({"tab_id": "first"})
        );
        s.tabs.truncate(1);
        s.focused_tab_id = Some("first".into());
        assert_eq!(
            request(Command::PreviousTab, &s).unwrap().1,
            json!({"tab_id": "first"})
        );
    }
}
