//! Bounded PDF object editing, projection proposals, forms/annotations/signature states,
//! prepress/accessibility/redaction/full-save. PDF bytes are provider transport, never separate
//! live document authority.
//!
//! Redaction (Operator decision D27, "Flatten + narrow"): every page carrying a region is replaced by
//! its raster with the regions painted opaque (`flatten.rs`, renderer hayro-interpret into `paint.rs`);
//! the whole-file needle search is a second check, verified against Studio's own strict PDF reading
//! (DESCRIPTOR `proof.claim`, `proof.supported_profile`, `proof.limitations`).
//!
//! The read half of STUDIO-MODULE-PRESS (SMB-CRATE-SEAM-SPLITS, slice 1): PDF syntax, reading, object
//! editing, redaction, flatten, sanitized save and vector reading. `hsk-studio-press` is the public
//! face; the `#[doc(hidden)]` modules are the seam `hsk-studio-press-write` builds on, not a public
//! contract.
#![forbid(unsafe_code)]

#[doc(hidden)]
pub mod canon;
#[doc(hidden)]
pub mod cmap;
#[doc(hidden)]
pub mod content;
mod determination;
mod encoding;
mod encoding_data;
#[doc(hidden)]
pub mod error;
#[doc(hidden)]
pub mod filters;
#[doc(hidden)]
pub mod flatten;
mod flatten_purge;
mod font;
mod norm;
mod norm_data;
#[doc(hidden)]
pub mod paint;
#[doc(hidden)]
pub mod pdf_edit;
mod raster;
#[doc(hidden)]
pub mod reader;
mod render_bounds;
mod redaction;
mod sanitized_save;
mod show_text;
mod std14_data;
#[doc(hidden)]
pub mod syntax;
mod text_codec;
mod text_fields;
mod vector_read;
mod vector_source;
#[doc(hidden)]
pub mod writer;

pub use error::{Limits, PressError, RedactionClass, SinkError, TextCause};
pub use flatten::{FLATTEN_FILL, FlattenCause, FlattenNote, FlattenedPage, RENDER_STACK_BYTES};
pub use redaction::{RedactionRegion, RedactionReport, UnsupportedPolicy};
pub use raster::RASTER_CLASS;
pub use sanitized_save::{
    MIN_DISTINCTIVE_NEEDLE, NotProven, PayloadHit, PayloadLocation, PayloadProof, RasterStream, RedactOptions, RedactedOutput, RedactionLoss,
    SanitizePolicy, SanitizeReport, StructureTreePolicy, TextCollision, redact_and_save, redact_and_save_opts, redact_and_save_with,
    sanitize_full_rewrite, verify_payload_absent, verify_payload_absent_unicode,
};
pub use text_fields::{
    TextFieldOutcome, TextFieldPolicy, TextFieldReport, TextMode, TextOutcomeKind, TextRedactedOutput, TextRole,
    redact_text_fields_and_save,
};
pub use pdf_edit::{
    AnnotationView, ArtifactSink, EditReceipt, FieldView, Inspection, ObjectModelProposal, PageView, PdfDoc, PdfScalar,
    SaveReceipt, SignatureView, sha256_hex,
};
pub use syntax::ObjRef;
pub use vector_read::{
    GRADIENT_SAMPLES, MAX_VECTOR_ELEMENTS, MAX_VECTOR_FONT_BYTES, MAX_VECTOR_ITEMS, MAX_VECTOR_PAGES, VectorColour, VectorDocument, VectorEl, VectorFont, VectorGlyph,
    VectorIcc, VectorItem, VectorLayer, VectorMode, VectorNote, VectorPage, VectorPaint, VectorSpace, VectorStroke, read_vector_document, read_vector_pages,
};
