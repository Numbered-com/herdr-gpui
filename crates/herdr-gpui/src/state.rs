use herdr_client::{
    ClientEvent,
    protocol::{ClientShellSnapshot, PaneSurfaceFrame, ServerMessage},
};
use std::sync::Arc;

#[derive(Clone)]
pub struct LiveState {
    pub snapshot: Option<Arc<ClientShellSnapshot>>,
    pub surface: Option<Arc<PaneSurfaceFrame>>,
    pub status: String,
    pub error: Option<String>,
    pub connected: bool,
    pub dirty: bool,
}

impl Default for LiveState {
    fn default() -> Self {
        Self {
            snapshot: None,
            surface: None,
            status: "Connecting...".into(),
            error: None,
            connected: false,
            dirty: true,
        }
    }
}

impl LiveState {
    pub fn apply(&mut self, event: ClientEvent) {
        match event {
            ClientEvent::Connected(_) => {
                self.connected = true;
                self.status = "Connected; waiting for snapshot".into();
            }
            ClientEvent::Snapshot(snapshot) => {
                if self
                    .surface
                    .as_ref()
                    .is_some_and(|s| !coherent(&snapshot, s))
                {
                    self.surface = None;
                }
                self.status = "Connected".into();
                self.snapshot = Some(snapshot);
            }
            ClientEvent::Surface(surface) => {
                if self
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| coherent(s, &surface))
                {
                    self.surface = Some(surface);
                }
            }
            ClientEvent::Disconnected { reason } => {
                self.connected = false;
                self.status = "Disconnected".into();
                self.error = Some(reason);
                self.snapshot = None;
                self.surface = None;
            }
            ClientEvent::CommandRejected { reason, .. } => self.error = Some(reason),
            ClientEvent::Response { response, .. } => {
                if let Some(error) = response.get("error") {
                    self.error = Some(error.to_string());
                }
            }
            ClientEvent::Message(ServerMessage::ClientShellError { message }) => {
                self.error = Some(message)
            }
            _ => return,
        }
        self.dirty = true;
    }
}

fn coherent(snapshot: &ClientShellSnapshot, surface: &PaneSurfaceFrame) -> bool {
    snapshot.boot_id == surface.boot_id && snapshot.revision == surface.projection_revision
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use herdr_client::protocol::FrameData;

    fn snapshot() -> Arc<ClientShellSnapshot> {
        Arc::new(
            serde_json::from_str(include_str!(
                "../../herdr-protocol/tests/fixtures/endpoint-snapshot-v1.json"
            ))
            .unwrap(),
        )
    }

    fn surface(snapshot: &ClientShellSnapshot) -> Arc<PaneSurfaceFrame> {
        Arc::new(PaneSurfaceFrame {
            boot_id: snapshot.boot_id.clone(),
            projection_revision: snapshot.revision,
            surface_revision: 1,
            frame: FrameData {
                cells: vec![],
                width: 0,
                height: 0,
                cursor: None,
                hyperlinks: vec![],
                graphics: vec![],
            },
            panes: vec![],
            splits: vec![],
            popup: None,
            graphics: Default::default(),
        })
    }

    #[test]
    fn snapshot_change_invalidates_cells_until_matching_surface() {
        let mut state = LiveState::default();
        let mut snapshot = snapshot();
        let old = surface(&snapshot);
        state.apply(ClientEvent::Snapshot(snapshot.clone()));
        state.apply(ClientEvent::Surface(old.clone()));
        assert!(state.surface.is_some());
        Arc::make_mut(&mut snapshot).revision += 1;
        state.apply(ClientEvent::Snapshot(snapshot.clone()));
        assert!(state.surface.is_none());
        state.apply(ClientEvent::Surface(old));
        assert!(state.surface.is_none());
        state.apply(ClientEvent::Surface(surface(&snapshot)));
        assert!(state.surface.is_some());
    }

    #[test]
    fn surface_before_snapshot_is_not_retained_or_replayed() {
        let mut state = LiveState::default();
        let snapshot = snapshot();
        let frame = surface(&snapshot);
        state.apply(ClientEvent::Surface(frame.clone()));
        assert!(state.surface.is_none());
        state.apply(ClientEvent::Snapshot(snapshot));
        assert!(state.surface.is_none());
        state.apply(ClientEvent::Surface(frame.clone()));
        assert!(Arc::ptr_eq(state.surface.as_ref().unwrap(), &frame));

        state.apply(ClientEvent::Disconnected {
            reason: "closed".into(),
        });
        state.apply(ClientEvent::Surface(frame));
        assert!(state.snapshot.is_none());
        assert!(state.surface.is_none());
        assert_eq!(state.status, "Disconnected");
        assert_eq!(state.error.as_deref(), Some("closed"));
    }

    #[test]
    fn different_boot_and_disconnect_cannot_retain_old_cells() {
        let mut state = LiveState::default();
        let snapshot = snapshot();
        let mut wrong_boot = surface(&snapshot);
        Arc::make_mut(&mut wrong_boot).boot_id = "old-boot".into();
        state.apply(ClientEvent::Snapshot(snapshot.clone()));
        state.apply(ClientEvent::Surface(wrong_boot));
        assert!(state.surface.is_none());
        state.apply(ClientEvent::Surface(surface(&snapshot)));
        state.apply(ClientEvent::Disconnected {
            reason: "closed".into(),
        });
        assert!(state.snapshot.is_none() && state.surface.is_none());
        assert!(!state.connected);
        assert_eq!(state.error.as_deref(), Some("closed"));
    }
}
