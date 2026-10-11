//! Bounded PDF object editing, projection proposals, forms/annotations/signature states,
//! prepress/accessibility/redaction/full-save. PDF bytes are provider transport, never separate
//! live document authority.
#![forbid(unsafe_code)]

mod content;
mod error;
mod filters;
mod pdf_edit;
mod reader;
mod redaction;
mod sanitized_save;
mod syntax;
mod text_codec;
mod text_fields;
mod writer;

pub use error::{Limits, PressError, RedactionClass, SinkError};
pub use redaction::{RedactionRegion, RedactionReport, UnsupportedPolicy};
pub use sanitized_save::{
    MIN_DISTINCTIVE_NEEDLE, PayloadHit, PayloadLocation, PayloadProof, RedactedOutput, SanitizePolicy, SanitizeReport,
    TextCollision, redact_and_save, redact_and_save_with, sanitize_full_rewrite, verify_payload_absent,
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
pub const DESCRIPTOR: &str = r#"{"schema":"hsk.studio.press.descriptor@1","crate":"hsk-studio-press","module":"STUDIO-MODULE-PRESS","authority":"PDF bytes are provider transport; PdfDoc is a bounded typed projection of one opened input, never a second live document authority","api":["PdfDoc::open","PdfDoc::inspect","PdfDoc::proposal","PdfDoc::edit_entry","PdfDoc::save_full","PdfDoc::save_to","PdfDoc::redact_region","redact_and_save","redact_and_save_with","PdfDoc::redact_text_fields","redact_text_fields_and_save","sanitize_full_rewrite","verify_payload_absent","outcome_for","report"],"reader":{"xref":["table","stream","hybrid XRefStm","Prev chain (loops refused)"],"objects":["direct","object streams"],"filters":["FlateDecode(+PNG/TIFF predictor)","LZWDecode(+predictor, EarlyChange 0|1)","ASCIIHexDecode","ASCII85Decode","RunLengthDecode"],"refused_filters":"typed UnsupportedFilter (DCT, JPX, CCITT, JBIG2, Crypt); every decoder is bounded by max_decoded_bytes/max_decoded_total","no_repair":true},"save":"full rewrite of objects reachable from /Root and /Info only; no incremental tail; unmodified objects copied verbatim","limits_default":{"max_bytes":67108864,"max_objects":200000,"max_depth":32,"max_decoded_bytes":67108864,"max_decoded_total":268435456,"max_xref_sections":64,"max_pages":10000,"max_content_ops":2000000},"refusals":["Canceled","InputLimit","Malformed","Encrypted","PermissionDenied","SignedDocument","NoSuchObject","UnsupportedObject","StaleSource","KeyNotAllowed","UnsupportedFilter","UnsupportedRedactionClass","RedactionIncomplete","NoSuchPage","Sink"],"redaction":{"removes":["text-showing operators (Tj TJ ' \") whose glyph run box may touch the region, whole strings","text-showing operators inside form XObjects (recursive, depth 8) whose box may touch the region","annotations whose /Rect touches the region (plus popups/children) and every AcroForm reference to them"],"output":"page /Contents repointed to a NEW stream; old stream unreachable and dropped by the full save","form_xobjects":"copy-on-write: a form with removed text is copied to a NEW object under a fresh /HskRdN resource name used only by the rewritten Do; the original form is never modified, other uses (outside the region, other pages) keep it, and its resource entry is dropped when no other Do at that level uses it","fail_closed":"images, vectors, shadings, inline images (and forms nested deeper than 8) touching the region refuse with UnsupportedRedactionClass (policy ReportAndContinue lists them in unredacted)","geometry":"exact advance with /Widths else 1000/1000 em per byte (upper bound for standard fonts); box -0.25..+0.95 em; closed-interval overlap","not_in_slice":["glyph-level partial strings","image/vector pixel removal (typed refusal)","Type3 glyph procs, patterns","encrypted input","signed input (SignedDocument)"]},"sanitize":{"classes":["info_metadata","xmp (/Metadata everywhere)","embedded_files (/EmbeddedFiles, /AF, FileAttachment /FS)","javascript (/JavaScript, /AA, JS /A and /OpenAction)","thumbnails (/Thumb)","private_data (/PieceInfo)"],"output":"full rewrite of reachable objects only"},"proof":{"scope":"verify_payload_absent re-opens the OUTPUT and scans raw bytes, every decoded stream (object streams included), string/name objects, content-stream operands, inline-image data and comments, plus the UTF-16BE spelling of ASCII needles; reports xref sections, unreachable objects, unlisted endobj markers, undecodable streams","context_rule":"a needle found as (part of) a TEXT-SHOWING operand (Tj TJ ' \") of a decoded content stream is a kept-text collision (kept_text_collisions), never a leak; a needle anywhere else (raw bytes outside decoded streams, string objects, non-show content operands, inline-image data, comments, decoded non-content streams, and ALL text of annotation /AP appearance streams) is a leak (hits)","token_rule":"a needle that starts or ends with an ASCII letter/digit matches only as a whole token (the neighbouring byte is not an ASCII letter/digit), so 'memory' inside the identifier 'cite.kang2025memoryos' is not a hit while 'the budget plan memo' contains the token 'budget plan'; UTF-16BE spellings match strictly","region_cleared":"all regions complete AND residual_in_region == 0 (a fresh pass over the SAVED bytes selects no text, annotation or unsupported mark in the regions) AND single xref section, no unreachable or unlisted objects AND every stream decoded (streams_not_decodable empty): the in-region instance is gone","fully_decoded":"PayloadProof::is_clean, region_cleared and is_proven all require streams_not_decodable to be empty: a stream with an unimplemented filter is scanned raw only, so a needle stored in its encoded form would otherwise read as proven absent (R5)","is_proven":"region_cleared AND no leak hit: additionally no distinctive removed string survives anywhere outside kept text-showing operands; the same word legitimately present in outline titles or metadata is reported as a hit, never hidden (use region_cleared for per-instance semantics)","short_needles":"removed strings under 6 bytes are not byte-scanned (kerned word pieces, subset glyph codes); residual_in_region covers them"},"text_fields":{"slots":["outline item /Title (every node linked through /First /Last /Next /Prev)","annotation /Contents /T /Subj /RC (page /Annots, direct dictionaries included)","form field /V /DV only with the explicit option widget_values"],"matching":"decoded text (PDFDocEncoding, UTF-16BE/LE, UTF-8 with mark), case-insensitive substring by default; whole_token and case_insensitive=false align with the proof","modes":"Replace(marker) or Remove; marker must not match a needle","rich_text":"/RC (string or XHTML stream) is judged raw, tag-stripped and tag-gapped; any match drops the key","appearance":"an annotation /AP whose show operands match is dropped (viewers regenerate it); unreadable appearance of an edited annotation is dropped","outcomes":"per object and role: Replaced | Removed | Dropped | KeptByPolicy (matches, unreadable); no document text; complete == nothing KeptByPolicy","kept_by_policy":"widget values/defaults/appearance without the option, and every field/widget /T name: reported, never silent, and still proof hits","proof":"annotation appearance streams count as annotation text: a needle in their show operands is a hit, never a kept-text collision","not_covered":["named destination names","action URIs","field options and tooltips","structure tree text","composite-font codes inside appearance streams"],"atomic":"staged copy, committed only on success; signed, encrypted and cancelled inputs refuse"},"never":["execute PDF JavaScript/actions/launch","network","decrypt","claim PDF/X PDF/UA PDF/A conformance"]}"#;
