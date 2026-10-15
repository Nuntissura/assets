//! Offline .fig proprietary-version decoder and conversion adapter owner; decoder availability
//! NOT_PROVEN. Preserve original/source-qualified IDs and map
//! components/variants/variables/modes/auto-layout/prototypes with loss receipts. SVG/PNG/token/API
//! interchange does not replace required editable .fig import.
//!
//! Slices: bounded container, identity, opaque preservation, loss receipts and version rejection;
//! Kiwi census; a generic schema-driven Kiwi value tree (`decode_kiwi`, `decode_message_tree`) and
//! a typed node tree (`decode_document`: tree, properties, paints, effects, geometry, text runs,
//! components/instances, variables, prototype interactions, typed losses). Nothing is mapped into
//! the Studio native document yet: editable support is reported NOT_PROVEN on every import
//! (`FigImport::editable`; test: fig_container_bounded_identity_loss).
#![forbid(unsafe_code)]

mod container;
mod descriptor;
mod effects;
mod error;
mod geometry;
mod graph;
mod import;
mod kiwi;
mod kiwi_value;
mod native;
mod nodes;
mod semantic_decode;
mod text_out;
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
pub use kiwi_value::{KiwiField, KiwiValue, decode_kiwi, decode_message_tree};
pub use native::{
    ExpandedNode, FigTextLayout, FigTextLine, FigTextRunSpec, FigVectorPath, FrameBackground,
    IdMapEntry, IdMint, InstanceExpansion, MAX_EXPANDED_NODES, MAX_INSTANCE_DEPTH,
    MAX_OVERRIDE_PATH, NATIVE_NOT_PROVEN, NativeConfig, NativeProposal, NativeRecords,
    RecordMapEntry, SequentialIdMint, id_index, read_native_records, to_native,
};
pub use text_out::{
    FigFontGrant, FigTextOutput, FigTextOutputRun, FigTextOutputs, MissingFont,
    resolve_text_outputs,
};
pub use nodes::{
    AutoLayout, COMMON_NODE_FIELDS, Color, ComponentPropDef, DerivedGlyph, DerivedSymbolNode,
    DerivedText, Effect, FigDocument, FigNode,
    FramePayload, Geometry, GradientStop, InstanceOverride, InstancePayload, Interaction,
    InteractionAction, Matrix, NetRegion, NetSegment, NetVertex, NodeLoss, NodePayload, Paint,
    PaintKind, PathCommand, TextLine, TextPayload, TextRun, VariableMode, VariablePayload, VariableValue,
    VariantProp, Vec2, VectorNetwork, decode_document, decode_path_blob, document_from_tree,
};
pub use semantic_decode::{EditableStatus, decode_semantic};
pub use version_map::{OBSERVED_VERSIONS, VersionPolicy, check_version};
