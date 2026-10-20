//! Native half of STUDIO-MODULE-INTEROP_FIG (SMB-CRATE-SEAM-SPLITS-2): `to_native` (Folio document,
//! design records, stories, instance expansion), effects and geometry mapping, text outputs
//! (`resolve_text_outputs`) and frame exports (PDF/raster through Press), on the base half's
//! container, import, node tree and layer graph. `hsk-studio-interop-fig` is the public face.
#![forbid(unsafe_code)]

// The base half's modules and root items under the names these modules have always used
// (`crate::nodes`, `crate::FigError`, ...).
use hsk_studio_interop_fig_base::{
    FigError, Result, SourceId, container, graph, import, nodes, semantic_decode,
};

mod effects;
mod export;
mod geometry;
mod native;
mod text_out;

pub use export::{
    DEFAULT_JPEG_QUALITY, FigDecodedImage, FigExportFormat, FigExportPlan, FigImageSource,
    FigPdfExport, FigRasterExport, MAX_EXPORT_SCALE, MAX_EXPORT_SIDE_PT, export_frame_pdf,
    export_frame_raster, plan_frame_exports, render_frame_export,
};
pub use native::{
    ExpandedNode, FigGlyphOutline, FigTextLayout, FigTextLine, FigTextRunSpec, FigVectorPath, FrameBackground,
    IdMapEntry, IdMint, InstanceExpansion, MAX_EXPANDED_NODES, MAX_INSTANCE_DEPTH,
    MAX_OVERRIDE_PATH, NATIVE_NOT_PROVEN, NativeConfig, NativeProposal, NativeRecords,
    RecordMapEntry, SequentialIdMint, id_index, read_native_records, to_native,
};
pub use text_out::{
    FigFontGrant, FigTextOutput, FigTextOutputRun, FigTextOutputs, MissingFont,
    resolve_text_outputs,
};
