//! What a connection reports to its owner, and the bounded, cancellable
//! delivery that never blocks the worker on a slow consumer.

use crate::{
    Error, Result,
    limits::POLL,
    protocol::{endpoint::EndpointServerWelcome, *},
};
use crossbeam_channel::{SendTimeoutError, Sender};
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug)]
pub enum ClientEvent {
    Connected(EndpointServerWelcome),
    Snapshot(Arc<ClientShellSnapshot>),
    /// Complete text baseline, including after a cell patch. Graphics assets are
    /// still connection-relative wire data; this client does not render images.
    Surface(Arc<PaneSurfaceFrame>),
    Response {
        request_id: String,
        response: Value,
    },
    /// Queued command was not sent (stale boot, unsupported method, or busy).
    CommandRejected {
        request_id: Option<String>,
        reason: Error,
    },
    /// Notifications, clipboard, title, bell, and other non-surface wire events.
    Message(ServerMessage),
    Disconnected {
        reason: String,
    },
}

pub(crate) fn deliver(
    tx: &Sender<ClientEvent>,
    mut event: ClientEvent,
    stop: &AtomicBool,
) -> Result<()> {
    let mut reported_backpressure = false;
    loop {
        if stop.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        match tx.send_timeout(event, POLL) {
            Ok(()) => return Ok(()),
            Err(SendTimeoutError::Timeout(e)) => {
                if !reported_backpressure {
                    tracing::debug!(category = "event_queue", "client backpressure");
                    reported_backpressure = true;
                }
                event = e;
            }
            Err(SendTimeoutError::Disconnected(_)) => {
                return Err(Error::EventReceiverDropped);
            }
        }
    }
}
