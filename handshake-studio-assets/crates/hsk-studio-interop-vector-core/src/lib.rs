//! Core half of STUDIO-MODULE-INTEROP_VECTOR (SMB-CRATE-SEAM-SPLITS slice 3a): the vector model,
//! ids, errors, the .ai import through the PDF drawing port and the Press provider port.
//! `hsk-studio-interop-vector` is the public face; the `#[doc(hidden)]` modules are the seam
//! `hsk-studio-interop-vector-svg` builds on, not a public contract.
#![forbid(unsafe_code)]

mod ai_adapter;
mod ai_import;
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
pub use error::{PreservedBlob, Result, VectorError};
pub use ids::{IdMint, SequentialIdMint};
pub use model::*;
pub use pdf_port::{NoPdfProvider, PdfVectorProvider};
