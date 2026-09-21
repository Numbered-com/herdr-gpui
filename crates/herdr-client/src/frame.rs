//! Incremental frame reads that survive socket timeouts. Restarting a partial
//! read would silently corrupt framing, so the prefix and payload are retained.

use crate::{Error, Result, limits::TIMEOUT, protocol::*};
use std::{
    io::{self, Read},
    time::Instant,
};

/// Keep partially read prefixes/payloads across socket timeouts. Restarting
/// read_exact after a timeout would silently corrupt framing.
pub(crate) struct FrameReader {
    pub(crate) bytes: Vec<u8>,
    pub(crate) target: usize,
    pub(crate) started: Option<Instant>,
}
impl FrameReader {
    pub(crate) fn new() -> Self {
        Self {
            bytes: Vec::new(),
            target: 4,
            started: None,
        }
    }
    pub(crate) fn poll(
        &mut self,
        stream: &mut (impl Read + ?Sized),
    ) -> Result<Option<ServerMessage>> {
        if self.started.is_some_and(|t| t.elapsed() > TIMEOUT) {
            tracing::warn!(category = "partial_frame", "client timeout");
            return Err(Error::PartialFrameTimeout);
        }
        let mut buf = [0; 8192];
        let len = buf.len().min(self.target - self.bytes.len());
        match stream.read(&mut buf[..len]) {
            Ok(0) => {
                return Err(Error::SocketClosed);
            }
            Ok(n) => {
                self.started.get_or_insert_with(Instant::now);
                self.bytes.extend_from_slice(&buf[..n]);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        }
        if self.bytes.len() != self.target {
            return Ok(None);
        }
        if self.target == 4 {
            let prefix = self
                .bytes
                .as_slice()
                .try_into()
                .map_err(|_| Error::FramePrefix)?;
            let len = u32::from_le_bytes(prefix) as usize;
            if len == 0 || len > MAX_GRAPHICS_FRAME_SIZE {
                return Err(Error::FrameLength);
            }
            self.target += len;
            return Ok(None);
        }
        let message = decode_payload(&self.bytes[4..])?;
        self.bytes.clear();
        self.target = 4;
        self.started = None;
        Ok(Some(message))
    }
}
