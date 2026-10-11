//! Bounded PDF object editing, projection proposals, forms/annotations/signature states,
//! prepress/accessibility/redaction/full-save. PDF bytes are provider transport, never separate
//! live document authority.
#![forbid(unsafe_code)]

mod error;
mod filters;
mod pdf_edit;
mod reader;
mod syntax;
mod writer;

pub use error::{Limits, PressError, RedactionClass, SinkError};
pub use pdf_edit::{
    AnnotationView, ArtifactSink, EditReceipt, FieldView, Inspection, ObjectModelProposal, PageView, PdfDoc, PdfScalar,
    SaveReceipt, SignatureView, sha256_hex,
};
pub use syntax::ObjRef;

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{Error as ObserveError, FailureCode, Observation, Observe, Outcome, Receipt, SinkPort};

/// Maps a press result onto the closed Observe outcome vocabulary. Only the outcome class is
/// emitted, never document text, object numbers or error detail.
pub fn outcome_for<T>(result: &Result<T, PressError>) -> Outcome {
    match result {
        Ok(_) => Outcome::Success,
        Err(PressError::Canceled) => Outcome::Canceled,
        Err(
            PressError::SignedDocument
            | PressError::Encrypted
            | PressError::PermissionDenied
            | PressError::UnsupportedFilter(_)
            | PressError::UnsupportedRedactionClass(_),
        ) => Outcome::Failure(FailureCode::Unsupported),
        Err(PressError::Sink(_)) => Outcome::Failure(FailureCode::Unavailable),
        Err(_) => Outcome::Failure(FailureCode::Validation),
    }
}

/// Emits one terminal observation through the caller-owned sink.
pub fn report(
    outcome: Outcome,
    correlation_id: u64,
    revision: u64,
    cancel: &CancellationToken,
    observe: &mut Observe,
    sink: &mut (impl SinkPort + ?Sized),
) -> Result<Receipt, ObserveError> {
    observe.emit(
        Observation { correlation_id, revision, outcome, progress: None, private_project_text: None },
        cancel,
        sink,
    )
}

/// Machine-readable capability descriptor.
pub const DESCRIPTOR: &str = r#"{"schema":"hsk.studio.press.descriptor@1","crate":"hsk-studio-press","module":"STUDIO-MODULE-PRESS","authority":"PDF bytes are provider transport; PdfDoc is a bounded typed projection of one opened input, never a second live document authority","api":["PdfDoc::open","PdfDoc::inspect","PdfDoc::proposal","PdfDoc::edit_entry","PdfDoc::save_full","PdfDoc::save_to"],"reader":{"xref":["table","stream","hybrid XRefStm","Prev chain (loops refused)"],"objects":["direct","object streams"],"filters":["FlateDecode(+PNG/TIFF predictor)","ASCIIHexDecode","ASCII85Decode","RunLengthDecode"],"refused_filters":"typed UnsupportedFilter (LZW, DCT, JPX, CCITT, JBIG2, Crypt)","no_repair":true},"save":"full rewrite of objects reachable from /Root and /Info only; no incremental tail; unmodified objects copied verbatim","limits_default":{"max_bytes":67108864,"max_objects":200000,"max_depth":32,"max_decoded_bytes":67108864,"max_decoded_total":268435456,"max_xref_sections":64,"max_pages":10000,"max_content_ops":2000000},"refusals":["Canceled","InputLimit","Malformed","Encrypted","PermissionDenied","SignedDocument","NoSuchObject","UnsupportedObject","StaleSource","KeyNotAllowed","UnsupportedFilter","UnsupportedRedactionClass","RedactionIncomplete","NoSuchPage","Sink"],"never":["execute PDF JavaScript/actions/launch","network","decrypt","claim PDF/X PDF/UA PDF/A conformance"]}"#;
