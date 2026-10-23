//! Studio Affinity interop (lane SMB-AFFINITY-INTEROP-1, parity row AF-02; lead decision 2026-10-16):
//! a clean-room, read-only reader of the KA container that every Affinity document (`.af`, `.afphoto`,
//! `.afdesign`, `.afpub`, V1-V3) and resource file uses.
//!
//! Scope: header, chained stream directory (`#FAT`/`#FT2`/`#FT3`/`#FT4`, head revision), per-stream
//! stored/zlib/zstd payloads with predictor inverse and CRC-32 check, the embedded `Thmb` preview and the
//! image/font/PDF resource blobs as content-addressed assets. NOT decoded: the `doc.dat` tagged object
//! graph (layers, vectors, text, adjustments): callers record [`LOSS_LAYERS_NOT_DECODED`]. No writer:
//! native write waits until a written file is verified in Affinity (lead decision 2026-10-16).
//!
//! Supported route for editable content: export PSD, PDF or SVG from Affinity ([`EXPORT_ROUTES`]).
//!
//! Hostile input (V4-01/02): input, directory-chain, entry, name, stream, ratio and total-output caps
//! ([`Limits`]); cyclic directory offsets refused; counts checked against the bytes left before
//! allocation; inflation never exceeds the declared size; every decoded stream is CRC-checked.
//!
//! Provenance: VMDevCpp/afread (MIT, Copyright (c) 2020-2021 Vladimir Mamonov) for the container layout;
//! Asset-Hoard/afphoto (MIT, Copyright (c) 2026 Mark Gandolfo) for the preview-in-the-clear observation;
//! the lane's own hexdumps for the `Thmb` block fields. GPL and unlicensed readers were not opened.
//! Real Affinity-written behaviour beyond the 9 AFDesignLoad V1 designs is UNVERIFIED until the
//! Operator-attended session SMB-AFFINITY-ORACLE-OPERATOR.
#![forbid(unsafe_code)]
#![deny(clippy::arithmetic_side_effects)]

mod container;
mod error;
mod limits;
mod stream;
mod thumbnail;

pub use container::{
    Codec, Directory, DirectoryKind, Entry, EntryState, Folder, Generation, KaContainer, KaHeader, Stream, identify,
};
pub use error::{AffinityError, Result};
pub use limits::Limits;
pub use thumbnail::{BlobKind, ResourceBlob, Resources, Thumbnail, hex, sha256};

/// Typed loss for every import of a native file: the object graph (layers) is not decoded.
pub const LOSS_LAYERS_NOT_DECODED: &str = "affinity_layers_not_decoded";

/// Affinity's own interchange exports that Studio opens with layers (Designer 2 file-format table; the
/// V3 export set is UNVERIFIED).
pub const EXPORT_ROUTES: &[&str] = &["psd", "pdf", "svg"];

/// The complete public surface.
pub const PUBLIC_ENTRYPOINTS: &[&str] = &[
    "identify",
    "KaContainer::parse",
    "KaContainer::streams",
    "KaContainer::read_stream",
    "KaContainer::read_entry",
    "KaContainer::thumbnail",
    "KaContainer::resources",
];
