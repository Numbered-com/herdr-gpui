//! The owner-facing half of a connection: the cloneable handle that queues
//! ordered commands and typed API requests, and the event receiver beside it.
//! Dropping the last handle stops the worker; queueing is never daemon ack.

use crate::{
    Error, Result, SendError,
    event::ClientEvent,
    options::{ConnectOptions, validate_options},
    protocol::*,
};
use crossbeam_channel::{Receiver, Sender, TrySendError};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

pub struct Client {
    pub handle: ClientHandle,
    pub events: Receiver<ClientEvent>,
}
#[derive(Clone)]
pub struct ClientHandle {
    pub(crate) inner: Arc<HandleInner>,
}
pub(crate) struct HandleInner {
    pub(crate) commands: Sender<Command>,
    pub(crate) stop: Arc<AtomicBool>,
    pub(crate) next_request: AtomicU64,
}
impl Drop for HandleInner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}
pub(crate) struct Command {
    pub(crate) boot_id: String,
    pub(crate) bytes: Vec<u8>,
    pub(crate) request: Option<(String, String)>,
}

impl ClientHandle {
    pub fn disconnect(&self) {
        tracing::debug!("disconnect requested");
        self.inner.stop.store(true, Ordering::Release);
    }
    pub fn is_disconnected(&self) -> bool {
        self.inner.stop.load(Ordering::Acquire)
    }

    fn enqueue(
        &self,
        boot_id: &str,
        message: ClientMessage,
        request: Option<(String, String)>,
    ) -> Result<()> {
        if self.is_disconnected() {
            return Err(SendError::Disconnected);
        }
        if boot_id.is_empty() {
            return Err(Error::MissingBootId);
        }
        let bytes = encode_message(&message, MAX_FRAME_SIZE)?;
        self.inner
            .commands
            .try_send(Command {
                boot_id: boot_id.into(),
                bytes,
                request,
            })
            .map_err(|e| match e {
                TrySendError::Full(_) => {
                    tracing::warn!(category = "command_queue", "client backpressure");
                    SendError::Full
                }
                TrySendError::Disconnected(_) => SendError::Disconnected,
            })
    }
    pub fn send_input(
        &self,
        boot_id: &str,
        pane_id: &str,
        events: impl IntoIterator<Item = ClientPaneInputEvent>,
    ) -> Result<()> {
        self.enqueue(
            boot_id,
            ClientMessage::ClientShellPaneInput {
                pane_id: pane_id.into(),
                events: events.into_iter().collect(),
            },
            None,
        )
    }
    pub fn send_popup_input(
        &self,
        boot_id: &str,
        terminal_id: &str,
        events: impl IntoIterator<Item = ClientPaneInputEvent>,
    ) -> Result<()> {
        self.enqueue(
            boot_id,
            ClientMessage::ClientShellPopupInput {
                terminal_id: terminal_id.into(),
                events: events.into_iter().collect(),
            },
            None,
        )
    }
    pub fn resize(&self, boot_id: &str, options: ConnectOptions) -> Result<()> {
        validate_options(options)?;
        self.enqueue(
            boot_id,
            ClientMessage::ClientShellResize {
                cell_width_px: options.cell_width_px,
                cell_height_px: options.cell_height_px,
                surface_size: options.surface_size,
                pixel_mouse: false,
            },
            None,
        )
    }
    pub fn set_focus(&self, boot_id: &str, focused: bool) -> Result<()> {
        self.enqueue(boot_id, ClientMessage::ClientShellFocus { focused }, None)
    }
    /// Queue upstream's surface-interest API, not window focus. Wait for the
    /// matching Response before considering a host activation/deactivation complete.
    pub fn set_surface_active(&self, boot_id: &str, active: bool) -> Result<String> {
        self.request(
            boot_id,
            "client_shell.surface.set",
            json!({"active": active}),
        )
    }
    /// Serialize the API envelope, generate an ID, and queue on the ordered writer.
    /// Only methods advertised in Connected are sent. Responses retain API errors.
    pub fn request(&self, boot_id: &str, method: &str, params: Value) -> Result<String> {
        let id = format!(
            "gpui-{}",
            self.inner.next_request.fetch_add(1, Ordering::Relaxed)
        );
        let request = json!({"id": id, "method": method, "params": params}).to_string();
        self.enqueue(
            boot_id,
            ClientMessage::ClientShellEndpointRequest {
                boot_id: boot_id.into(),
                request,
            },
            Some((id.clone(), method.into())),
        )?;
        Ok(id)
    }
    pub fn focus_pane(&self, boot_id: &str, pane_id: &str) -> Result<String> {
        self.request(boot_id, "pane.focus", json!({"pane_id": pane_id}))
    }
    pub fn focus_tab(&self, boot_id: &str, tab_id: &str) -> Result<String> {
        self.request(boot_id, "tab.focus", json!({"tab_id": tab_id}))
    }
    pub fn focus_workspace(&self, boot_id: &str, workspace_id: &str) -> Result<String> {
        self.request(
            boot_id,
            "workspace.focus",
            json!({"workspace_id": workspace_id}),
        )
    }
}
