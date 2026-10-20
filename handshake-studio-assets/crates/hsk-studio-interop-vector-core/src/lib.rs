//! Core half of STUDIO-MODULE-INTEROP_VECTOR (SMB-CRATE-SEAM-SPLITS slice 3a): the vector model,
//! ids, errors, the .ai import through the PDF drawing port and the Press provider port, and the
//! other vector interchange formats (lane SMB-DXF-EPS-IMPORT): DXF (feature `dxf`), the DWG refusal,
//! EPS (DSC and preview in process, vector content through the isolated `stet` worker of features
//! `eps` / `eps-worker`) and EPSF export.
//! `hsk-studio-interop-vector` is the public face; the `#[doc(hidden)]` modules are the seam
//! `hsk-studio-interop-vector-svg` builds on, not a public contract.
#![forbid(unsafe_code)]

mod ai_adapter;
mod ai_import;
mod cad;
#[cfg(feature = "dxf")]
mod dxf_aci;
#[cfg(feature = "dxf")]
mod dxf_import;
mod eps;
mod eps_export;
mod eps_worker;
#[doc(hidden)]
pub mod error;
#[doc(hidden)]
pub mod ids;
#[doc(hidden)]
pub mod model;
mod pdf_port;

pub use ai_adapter::{AiKind, identify_ai, import_ai};
pub use ai_import::{
    ARTBOARD_GAP_PT, AiFontFace, AiImport, AiTextLayer, MAX_TEXT_LAYERS, MAX_TEXT_RUN_BYTES, PdfColour, PdfDrawItem, PdfDrawing, PdfDrawingPage, PdfDrawingProvider, PdfEl,
    PdfFont, PdfGlyph, PdfIcc, PdfLayer, PdfPaint, PdfSpace, PdfStroke, PdfTextFont, PdfTextRun, import_ai_document, import_ai_with,
};
pub use cad::{CadKind, DWG_ROUTE, identify_cad, import_dwg};
#[cfg(feature = "dxf")]
pub use dxf_import::import_dxf;
pub use eps::{
    DOS_EPS_HEADER_LEN, DOS_EPS_MAGIC, EPS_ROUTE, EpsInfo, EpsPdfConverter, EpsPreview, EpsPreviewImport, MAX_PREVIEW_PIXELS, MAX_PREVIEW_SIDE, import_eps, import_eps_preview,
    import_eps_with, read_eps,
};
pub use eps_export::{EpsExport, export_eps, export_eps_proposal};
pub use eps_worker::{EPS_WORKER_PANIC_EXIT, EPS_WORKER_PROGRAM, EPS_WORKER_PROTOCOL};
#[cfg(feature = "eps")]
pub use eps_worker::{EpsWorkerOptions, WorkerEpsConverter};
#[cfg(feature = "eps-worker")]
pub use eps_worker::eps_worker_main;
pub use error::{PreservedBlob, Result, VectorError};
pub use ids::{IdMint, SequentialIdMint};
pub use model::*;
pub use pdf_port::{NoPdfProvider, PdfVectorProvider};
