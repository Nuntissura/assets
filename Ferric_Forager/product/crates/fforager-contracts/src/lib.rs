//! Versioned, bounded, data-only contracts for Ferric Forager.
//!
//! These types deliberately contain no runtime handles. Inputs received across a
//! trust boundary must be validated with the supplied `validate` methods before
//! domain use. Wire compatibility is major-version based: readers reject an
//! unsupported major version and may accept a newer minor only when every
//! unknown field or kind is optional.

#![forbid(unsafe_code)]

pub mod archive;
pub mod ffmpeg;
pub mod framing;
pub mod graph;
pub mod identity;
pub mod protocol;
pub mod resource;
pub mod storage;

pub use archive::*;
pub use ffmpeg::*;
pub use framing::{FrameDecoder, FrameError, FrameLimits};
pub use graph::*;
pub use identity::*;
pub use protocol::*;
pub use resource::*;
pub use storage::*;
