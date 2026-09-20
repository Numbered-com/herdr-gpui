//! Stable gen1 wire model with no terminal or server runtime dependencies.
#![doc = include_str!("../README.md")]
pub mod endpoint;
mod error;
mod wire;
pub use error::{Error, Result};
use serde::{Serialize, de::DeserializeOwned};
use std::io::{self, Read, Write};
pub use wire::*;

pub const MAX_FRAME_SIZE: usize = 2 * 1024 * 1024;
pub const MAX_GRAPHICS_FRAME_SIZE: usize = 32 * 1024 * 1024;

/// Bincode 2 standard configuration, framed by a u32 little-endian byte length.
pub fn encode_message<M: Serialize>(message: &M, limit: usize) -> Result<Vec<u8>> {
    // A bounded writer also prevents oversized outbound payload allocations.
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    Error::FrameLimit,
                ));
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
    bincode::serde::encode_into_std_write(message, &mut out, bincode::config::standard())?;
    let len = (out.bytes.len() - 4) as u32;
    if len == 0 {
        return Err(Error::EmptyFrame);
    }
    out.bytes[..4].copy_from_slice(&len.to_le_bytes());
    Ok(out.bytes)
}

pub fn decode_payload<M: DeserializeOwned>(payload: &[u8]) -> Result<M> {
    if payload.is_empty() || payload.len() > MAX_GRAPHICS_FRAME_SIZE {
        return Err(Error::FrameLimit);
    }
    let (message, used) = bincode::serde::decode_from_slice(
        payload,
        bincode::config::standard().with_limit::<MAX_GRAPHICS_FRAME_SIZE>(),
    )?;
    if used != payload.len() {
        return Err(Error::TrailingBytes);
    }
    Ok(message)
}

pub fn write_message<W: Write, M: Serialize>(
    writer: &mut W,
    message: &M,
    limit: usize,
) -> Result<()> {
    writer.write_all(&encode_message(message, limit)?)?;
    Ok(writer.flush()?)
}

pub fn read_message<R: Read, M: DeserializeOwned>(reader: &mut R, limit: usize) -> Result<M> {
    let mut prefix = [0; 4];
    reader.read_exact(&mut prefix)?;
    let len = u32::from_le_bytes(prefix) as usize;
    if len == 0 || len > limit.min(MAX_GRAPHICS_FRAME_SIZE) {
        return Err(Error::FrameLimit);
    }
    let mut payload = vec![0; len];
    reader.read_exact(&mut payload)?;
    decode_payload(&payload)
}

impl FrameData {
    pub fn validate(&self) -> Result<()> {
        if self.cells.len() != usize::from(self.width) * usize::from(self.height) {
            return Err(Error::CellCount);
        }
        if self.cells.iter().any(|c| {
            c.hyperlink
                .is_some_and(|i| i as usize >= self.hyperlinks.len())
        }) {
            return Err(Error::HyperlinkIndex);
        }
        if self
            .cursor
            .as_ref()
            .is_some_and(|c| c.visible && (c.x >= self.width || c.y >= self.height))
        {
            return Err(Error::CursorBounds);
        }
        Ok(())
    }
}

impl PaneSurfaceFrame {
    /// Apply baseline cell patches atomically. Optional delta/reuse codecs are not needed.
    pub fn apply_patch(&mut self, patch: PaneSurfacePatch) -> Result<()> {
        if patch.boot_id != self.boot_id
            || patch.projection_revision != self.projection_revision
            || patch.base_surface_revision != self.surface_revision
            || self.surface_revision.checked_add(1) != Some(patch.surface_revision)
            || self.popup.is_some()
        {
            return Err(Error::PatchIdentity);
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
                return Err(Error::PatchRowBounds);
            }
        }
        for pane in &patch.panes {
            if !self.panes.iter().any(|p| {
                p.pane_id == pane.pane_id && p.rect == pane.rect && p.inner_rect == pane.inner_rect
            }) {
                return Err(Error::PatchGeometry);
            }
        }
        if patch
            .cursor
            .as_ref()
            .is_some_and(|c| c.visible && (c.x >= self.frame.width || c.y >= self.frame.height))
        {
            return Err(Error::PatchCursorBounds);
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
