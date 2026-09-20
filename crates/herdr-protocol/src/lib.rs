//! Stable gen1 wire model with no terminal or server runtime dependencies.
pub mod endpoint;
mod wire;
use serde::{Serialize, de::DeserializeOwned};
use std::io::{self, Read, Write};
pub use wire::*;

pub const MAX_FRAME_SIZE: usize = 2 * 1024 * 1024;
pub const MAX_GRAPHICS_FRAME_SIZE: usize = 32 * 1024 * 1024;

fn invalid(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

/// Bincode 2 standard configuration, framed by a u32 little-endian byte length.
pub fn encode_message<M: Serialize>(message: &M, limit: usize) -> io::Result<Vec<u8>> {
    // A bounded writer also prevents oversized outbound payload allocations.
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(invalid("frame exceeds limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut out = Bounded {
        bytes: vec![0; 4],
        limit: limit.min(u32::MAX as usize).saturating_add(4),
    };
    bincode::serde::encode_into_std_write(message, &mut out, bincode::config::standard())
        .map_err(invalid)?;
    let len = (out.bytes.len() - 4) as u32;
    if len == 0 {
        return Err(invalid("empty frame"));
    }
    out.bytes[..4].copy_from_slice(&len.to_le_bytes());
    Ok(out.bytes)
}

pub fn decode_payload<M: DeserializeOwned>(payload: &[u8]) -> io::Result<M> {
    if payload.is_empty() || payload.len() > MAX_GRAPHICS_FRAME_SIZE {
        return Err(invalid("frame exceeds limit"));
    }
    let (message, used) = bincode::serde::decode_from_slice(
        payload,
        bincode::config::standard().with_limit::<MAX_GRAPHICS_FRAME_SIZE>(),
    )
    .map_err(invalid)?;
    if used != payload.len() {
        return Err(invalid("trailing frame bytes"));
    }
    Ok(message)
}

pub fn write_message<W: Write, M: Serialize>(
    writer: &mut W,
    message: &M,
    limit: usize,
) -> io::Result<()> {
    writer.write_all(&encode_message(message, limit)?)?;
    writer.flush()
}

pub fn read_message<R: Read, M: DeserializeOwned>(reader: &mut R, limit: usize) -> io::Result<M> {
    let mut prefix = [0; 4];
    reader.read_exact(&mut prefix)?;
    let len = u32::from_le_bytes(prefix) as usize;
    if len == 0 || len > limit.min(MAX_GRAPHICS_FRAME_SIZE) {
        return Err(invalid("frame exceeds limit"));
    }
    let mut payload = vec![0; len];
    reader.read_exact(&mut payload)?;
    decode_payload(&payload)
}

impl FrameData {
    pub fn validate(&self) -> io::Result<()> {
        if self.cells.len() != usize::from(self.width) * usize::from(self.height) {
            return Err(invalid("cell count does not match frame dimensions"));
        }
        if self.cells.iter().any(|c| {
            c.hyperlink
                .is_some_and(|i| i as usize >= self.hyperlinks.len())
        }) {
            return Err(invalid("invalid hyperlink index"));
        }
        if self
            .cursor
            .as_ref()
            .is_some_and(|c| c.visible && (c.x >= self.width || c.y >= self.height))
        {
            return Err(invalid("cursor outside frame"));
        }
        Ok(())
    }
}

impl PaneSurfaceFrame {
    /// Apply baseline cell patches atomically. Optional delta/reuse codecs are not needed.
    pub fn apply_patch(&mut self, patch: PaneSurfacePatch) -> io::Result<()> {
        if patch.boot_id != self.boot_id
            || patch.projection_revision != self.projection_revision
            || patch.base_surface_revision != self.surface_revision
            || self.surface_revision.checked_add(1) != Some(patch.surface_revision)
            || self.popup.is_some()
        {
            return Err(invalid("surface patch identity mismatch"));
        }
        self.frame.validate()?;
        for row in &patch.rows {
            if row.y >= self.frame.height
                || usize::from(row.x) + row.cells.len() > usize::from(self.frame.width)
                || row.cells.iter().any(|c| {
                    c.hyperlink
                        .is_some_and(|i| i as usize >= self.frame.hyperlinks.len())
                })
            {
                return Err(invalid("surface patch row outside frame"));
            }
        }
        for pane in &patch.panes {
            if !self.panes.iter().any(|p| {
                p.pane_id == pane.pane_id && p.rect == pane.rect && p.inner_rect == pane.inner_rect
            }) {
                return Err(invalid("surface patch changed pane geometry"));
            }
        }
        if patch
            .cursor
            .as_ref()
            .is_some_and(|c| c.visible && (c.x >= self.frame.width || c.y >= self.frame.height))
        {
            return Err(invalid("patch cursor outside frame"));
        }
        for row in patch.rows {
            let start = usize::from(row.y) * usize::from(self.frame.width) + usize::from(row.x);
            self.frame.cells[start..start + row.cells.len()].clone_from_slice(&row.cells);
        }
        for pane in patch.panes {
            if let Some(existing) = self.panes.iter_mut().find(|p| p.pane_id == pane.pane_id) {
                *existing = pane;
            }
        }
        self.frame.cursor = patch.cursor;
        self.surface_revision = patch.surface_revision;
        Ok(())
    }
}
