use crate::{
    state::{ConnectionStatus, LiveState},
    terminal::InputTarget,
};
use herdr_client::{
    ClientEvent, ClientHandle, ConnectOptions, ConnectTarget, connect_with_connector,
    protocol::ClientPaneInputEvent,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

pub(crate) struct ConnectionBridge {
    pub target: ConnectTarget,
    pub handle: Option<ClientHandle>,
    pub inbox: Arc<Mutex<LiveState>>,
    pub drained: Arc<AtomicBool>,
}

impl ConnectionBridge {
    pub fn new(target: ConnectTarget) -> Self {
        Self {
            target,
            handle: None,
            inbox: Arc::new(Mutex::new(LiveState::default())),
            drained: Arc::new(AtomicBool::new(true)),
        }
    }

    fn reset(&mut self, status: ConnectionStatus, active: bool) {
        if let Some(handle) = self.handle.take() {
            handle.disconnect();
        }
        let mut state = LiveState::default();
        state.status = status;
        state.set_outer_focus(active);
        // Old readers and deferred paint acknowledgements retain only the old inbox.
        self.inbox = Arc::new(Mutex::new(state));
        self.drained = Arc::new(AtomicBool::new(true));
    }

    pub fn detach(&mut self, active: bool) {
        self.reset(ConnectionStatus::Detached, active);
    }

    pub fn reconnect(&mut self, options: ConnectOptions, active: bool, surface_active: bool) {
        self.reset(ConnectionStatus::Connecting, active);
        self.start(options, surface_active, |events| {
            std::thread::Builder::new()
                .name("herdr-gui-events".into())
                .spawn(events)
                .map(|_| ())
        });
    }

    fn start(
        &mut self,
        options: ConnectOptions,
        surface_active: bool,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
    ) {
        self.drained = Arc::new(AtomicBool::new(false));
        let target = self.target.clone();
        let startup_inbox = self.inbox.clone();
        let result =
            connect_with_connector(target, options, surface_active, move |target, stop| {
                let result = crate::daemon::connect(target, stop, || {
                    if let Ok(mut state) = startup_inbox.lock()
                        && state.status == ConnectionStatus::Connecting
                    {
                        state.daemon_starting();
                    }
                });
                if let Ok(stream) = &result {
                    let local = crate::daemon::is_local_peer(stream);
                    if let Ok(mut state) = startup_inbox.lock() {
                        state.local_daemon_peer = local;
                        state.dirty = true;
                    }
                }
                if result
                    .as_ref()
                    .is_err_and(crate::daemon::is_missing_installation)
                    && let Ok(mut state) = startup_inbox.lock()
                    && state.status == ConnectionStatus::StartingDaemon
                {
                    state.missing_installation = true;
                    state.dirty = true;
                }
                result
            })
            .and_then(|client| {
                self.handle = Some(client.handle);
                let inbox = self.inbox.clone();
                let drained = self.drained.clone();
                // Drain ordered events even while GPUI is busy; retain only coherent state.
                spawn(Box::new(move || {
                    while let Ok(event) = client.events.recv() {
                        if let Ok(mut state) = inbox.lock() {
                            state.apply(event);
                        }
                    }
                    drained.store(true, Ordering::Release);
                }))
            });
        if let Err(error) = result {
            self.drained.store(true, Ordering::Release);
            if let Some(handle) = self.handle.take() {
                handle.disconnect();
            }
            if let Ok(mut state) = self.inbox.lock() {
                state.apply(ClientEvent::Disconnected {
                    reason: error.to_string(),
                });
            }
        }
    }

    pub fn take_update(&self) -> Option<LiveState> {
        let mut state = self.inbox.try_lock().ok()?;
        if !state.dirty {
            return None;
        }
        state.dirty = false;
        // Deliver the single response once rather than cloning a potentially large
        // checkout list into every subsequent surface update.
        let response = state
            .dialog_response
            .as_mut()
            .and_then(|(_, result)| result.take());
        let mut update = state.clone();
        if let Some((_, result)) = &mut update.dialog_response {
            *result = response;
        }
        Some(update)
    }

    pub fn request_dialog(
        &self,
        boot_id: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<String, String> {
        // Register while holding the mailbox so even an immediate rejection is retained.
        let mut state = self
            .inbox
            .try_lock()
            .map_err(|_| "Connection busy. Try again.")?;
        let id = self
            .handle
            .as_ref()
            .ok_or("Disconnected.")?
            .request(boot_id, method, params)
            .map_err(|error| error.to_string())?;
        state.dialog_response = Some((id.clone(), None));
        Ok(id)
    }

    pub fn send_input(
        handle: &ClientHandle,
        boot_id: &str,
        target: &InputTarget,
        event: ClientPaneInputEvent,
    ) -> Result<(), herdr_client::SendError> {
        match target {
            InputTarget::Pane(id) => handle.send_input(boot_id, id, [event]),
            InputTarget::Popup(id) => handle.send_popup_input(boot_id, id, [event]),
        }
    }
}

impl Drop for ConnectionBridge {
    fn drop(&mut self) {
        // Detach this client only; never kill a daemon or PTY.
        if let Some(handle) = &self.handle {
            handle.disconnect();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn bridge() -> ConnectionBridge {
        ConnectionBridge::new(ConnectTarget::Socket("/unused-connection-test.sock".into()))
    }

    #[test]
    fn synchronous_startup_failure_survives_mailbox_and_focus_updates() {
        let mut bridge = bridge();
        let mut options = ConnectOptions::default();
        options.surface_size.cols = 0;
        bridge.reconnect(options, true, true);
        let failed = bridge.take_update().unwrap();
        assert_eq!(failed.status, ConnectionStatus::Disconnected);
        assert!(failed.error.is_some());
        assert!(bridge.handle.is_none());
        assert!(bridge.take_update().is_none());
        bridge.inbox.lock().unwrap().set_outer_focus(false);
        let next = bridge.take_update().unwrap();
        assert_eq!(next.status, failed.status);
        assert_eq!(next.error, failed.error);
    }

    #[test]
    fn event_reader_startup_failure_is_authoritative() {
        let mut bridge = bridge();
        bridge.start(ConnectOptions::default(), true, |_| {
            Err(std::io::Error::other("reader startup failed"))
        });
        let state = bridge.take_update().unwrap();
        assert_eq!(state.status, ConnectionStatus::Disconnected);
        assert_eq!(state.error.as_deref(), Some("reader startup failed"));
        assert!(bridge.handle.is_none());
        bridge.inbox.lock().unwrap().set_outer_focus(true);
        assert_eq!(
            bridge.take_update().unwrap().status,
            ConnectionStatus::Disconnected
        );
    }

    #[test]
    fn detach_and_reconnect_fence_old_inboxes() {
        let mut bridge = bridge();
        let old = bridge.inbox.clone();
        old.lock().unwrap().local_daemon_peer = true;
        old.lock().unwrap().dialog_response = Some(("remove".into(), None));
        bridge.detach(true);
        old.lock().unwrap().apply(ClientEvent::Response {
            request_id: "remove".into(),
            response: serde_json::json!({"error":{"code":"dirty_worktree_requires_force"}}),
        });
        old.lock().unwrap().missing_installation = true;
        old.lock().unwrap().apply(ClientEvent::Disconnected {
            reason: "old connection".into(),
        });
        let detached = bridge.take_update().unwrap();
        assert_eq!(detached.status, ConnectionStatus::Detached);
        assert!(!detached.missing_installation);
        assert!(!detached.local_daemon_peer);
        assert!(detached.error.is_none());
        assert!(detached.dialog_response.is_none());
        assert!(detached.snapshot.is_none() && detached.surface.is_none());
        let old = bridge.inbox.clone();
        old.lock().unwrap().local_daemon_peer = true;
        let mut options = ConnectOptions::default();
        options.surface_size.cols = 0;
        bridge.reconnect(options, false, true);
        old.lock().unwrap().apply(ClientEvent::Disconnected {
            reason: "detached connection".into(),
        });
        let failed = bridge.take_update().unwrap();
        assert_eq!(failed.status, ConnectionStatus::Disconnected);
        assert!(!failed.local_daemon_peer);
        assert_ne!(failed.error.as_deref(), Some("detached connection"));
        assert!(!Arc::ptr_eq(&old, &bridge.inbox));
    }

    #[test]
    fn dialog_result_moves_out_once_without_losing_pending_registration() {
        let bridge = bridge();
        bridge.inbox.lock().unwrap().dialog_response = Some(("list".into(), None));
        assert_eq!(
            bridge.take_update().unwrap().dialog_response,
            Some(("list".into(), None))
        );
        bridge.inbox.lock().unwrap().apply(ClientEvent::Response {
            request_id: "list".into(),
            response: serde_json::json!({"result":{}}),
        });
        assert!(
            bridge
                .take_update()
                .unwrap()
                .dialog_response
                .unwrap()
                .1
                .is_some()
        );
        bridge.inbox.lock().unwrap().set_outer_focus(true);
        assert_eq!(
            bridge.take_update().unwrap().dialog_response,
            Some(("list".into(), None))
        );
    }
}
