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
//!
//! Crate layout (SMB-CRATE-SEAM-SPLITS-2): this crate is the module's public face and re-exports,
//! item by item, its two halves: `hsk-studio-interop-fig-base` (container, version policy, Kiwi,
//! node tree, import receipt, layer graph) and `hsk-studio-interop-fig-native` (`to_native`,
//! effects, geometry, text outputs, frame exports). Both halves belong to
//! STUDIO-MODULE-INTEROP_FIG; not a multi-engine facade.

mod descriptor;

pub use hsk_studio_interop_fig_base::{
    CANVAS_PART, FigChunk, FigContainer, FigKind, FigPart, IMAGES_PREFIX, Limits, META_PART,
    MetaKeys, Packaging, Sha256Bytes, THUMBNAIL_PART, hex, open_fig,
};
pub use descriptor::DESCRIPTOR;
pub use hsk_studio_interop_fig_base::{FigError, Result};
pub use hsk_studio_interop_fig_native::{
    DEFAULT_JPEG_QUALITY, FigDecodedImage, FigExportFormat, FigExportPlan, FigImageSource,
    FigPdfExport, FigRasterExport, MAX_EXPORT_SCALE, MAX_EXPORT_SIDE_PT, export_frame_pdf,
    export_frame_raster, plan_frame_exports, render_frame_export,
};
pub use hsk_studio_interop_fig_base::{
    Census, DefinitionSummary, FieldSummary, FigImport, FigOutcome, FigReceipt, LossEntry,
    LossKind, PartName, SanitizePolicy, SchemaSummary, SourceId, import_fig, report,
    sanitized_summary,
};
pub use hsk_studio_interop_fig_base::{KiwiField, KiwiValue, decode_kiwi, decode_message_tree};
pub use hsk_studio_interop_fig_native::{
    ExpandedNode, FigGlyphOutline, FigTextLayout, FigTextLine, FigTextRunSpec, FigVectorPath, FrameBackground,
    IdMapEntry, IdMint, InstanceExpansion, MAX_EXPANDED_NODES, MAX_INSTANCE_DEPTH,
    MAX_OVERRIDE_PATH, NATIVE_NOT_PROVEN, NativeConfig, NativeProposal, NativeRecords,
    RecordMapEntry, SequentialIdMint, id_index, read_native_records, to_native,
};
pub use hsk_studio_interop_fig_native::{
    FigFontGrant, FigTextOutput, FigTextOutputRun, FigTextOutputs, MissingFont,
    resolve_text_outputs,
};
pub use hsk_studio_interop_fig_base::{
    AutoLayout, COMMON_NODE_FIELDS, Color, ComponentPropDef, DerivedGlyph, DerivedSymbolNode,
    DerivedText, Effect, ExportSetting, FigDocument, FigNode,
    FramePayload, Geometry, GradientStop, InstanceOverride, InstancePayload, Interaction,
    InteractionAction, Matrix, NetRegion, NetSegment, NetVertex, NodeLoss, NodePayload, Paint,
    PaintKind, PathCommand, TextLine, TextPayload, TextRun, VariableMode, VariablePayload, VariableValue,
    VariantProp, Vec2, VectorNetwork, decode_document, decode_path_blob, document_from_tree,
};
pub use hsk_studio_interop_fig_base::{EditableStatus, decode_semantic};
pub use hsk_studio_interop_fig_base::{OBSERVED_VERSIONS, VersionPolicy, check_version};
