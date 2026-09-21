//! Stable gen1 wire model with no terminal or server runtime dependencies.
#![doc = include_str!("../README.md")]
pub mod endpoint;

mod codec;
mod error;
mod frame;
mod wire;

pub use codec::{
    MAX_FRAME_SIZE, MAX_GRAPHICS_FRAME_SIZE, decode_payload, encode_message, read_message,
    write_message,
};
pub use error::{Error, Result};
pub use wire::*;
