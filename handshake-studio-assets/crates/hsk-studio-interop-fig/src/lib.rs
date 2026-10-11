//! Offline .fig proprietary-version decoder and conversion adapter owner; decoder availability
//! NOT_PROVEN. Preserve original/source-qualified IDs and map
//! components/variants/variables/modes/auto-layout/prototypes with loss receipts. SVG/PNG/token/API
//! interchange does not replace required editable .fig import.
//!
//! Foundation slice: bounded container, identity, opaque preservation, loss receipts and version
//! rejection only. Editable support is NOT_PROVEN and never claimed.
#![forbid(unsafe_code)]

mod container;
mod descriptor;
mod error;
mod import;
mod semantic_decode;
mod version_map;

pub use container::{
    CANVAS_PART, FigChunk, FigContainer, FigKind, FigPart, IMAGES_PREFIX, Limits, META_PART,
    MetaKeys, Packaging, Sha256Bytes, THUMBNAIL_PART, hex, open_fig,
};
pub use descriptor::DESCRIPTOR;
pub use error::{FigError, Result};
pub use import::{
    Census, DefinitionSummary, FieldSummary, FigImport, FigOutcome, FigReceipt, LossEntry,
    LossKind, PartName, SanitizePolicy, SchemaSummary, SourceId, import_fig, report,
    sanitized_summary,
};
pub use semantic_decode::{EditableStatus, decode_semantic};
pub use version_map::{OBSERVED_VERSIONS, VersionPolicy, check_version};
