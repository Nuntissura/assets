//! Base half of STUDIO-MODULE-INTEROP_FIG (SMB-CRATE-SEAM-SPLITS-2): the bounded .fig container,
//! version policy, Kiwi schema/census and value tree, the typed node tree (`decode_document`), the
//! import receipt and loss list, and the Folio layer graph builder.
//! `hsk-studio-interop-fig` is the public face; the `#[doc(hidden)]` modules are the seam
//! `hsk-studio-interop-fig-native` builds on, not a public contract.
#![forbid(unsafe_code)]

#[doc(hidden)]
pub mod container;
mod error;
#[doc(hidden)]
pub mod graph;
#[doc(hidden)]
pub mod import;
mod kiwi;
mod kiwi_value;
#[doc(hidden)]
pub mod nodes;
#[doc(hidden)]
pub mod semantic_decode;
mod version_map;

pub use container::{
    CANVAS_PART, FigChunk, FigContainer, FigKind, FigPart, IMAGES_PREFIX, Limits, META_PART,
    MetaKeys, Packaging, Sha256Bytes, THUMBNAIL_PART, hex, open_fig,
};
pub use error::{FigError, Result};
pub use import::{
    Census, DefinitionSummary, FieldSummary, FigImport, FigOutcome, FigReceipt, LossEntry,
    LossKind, PartName, SanitizePolicy, SchemaSummary, SourceId, import_fig, report,
    sanitized_summary,
};
pub use kiwi_value::{KiwiField, KiwiValue, decode_kiwi, decode_message_tree};
pub use nodes::{
    AutoLayout, COMMON_NODE_FIELDS, Color, ComponentPropDef, DerivedGlyph, DerivedSymbolNode,
    DerivedText, Effect, ExportSetting, FigDocument, FigNode,
    FramePayload, Geometry, GradientStop, InstanceOverride, InstancePayload, Interaction,
    InteractionAction, Matrix, NetRegion, NetSegment, NetVertex, NodeLoss, NodePayload, Paint,
    PaintKind, PathCommand, TextLine, TextPayload, TextRun, VariableMode, VariablePayload, VariableValue,
    VariantProp, Vec2, VectorNetwork, decode_document, decode_path_blob, document_from_tree,
};
pub use semantic_decode::{EditableStatus, decode_semantic};
pub use version_map::{OBSERVED_VERSIONS, VersionPolicy, check_version};
