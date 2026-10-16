//! IDML/INDD/print interchange conversion; no claim INDD readable without actual decoder proof.
#![forbid(unsafe_code)]

mod container;
mod descriptor;
mod epub;
mod error;
mod folio_export;
mod idml_decode;
mod idml_encode;
mod idml_longdoc;
mod ids;
mod indd_adapter;
mod model;
mod pdf_export;
mod proposal;
mod reflow;

pub use container::IDML_MIMETYPE;
pub use descriptor::DESCRIPTOR;
pub use epub::{
    EPUB_MIMETYPE, EpubAccessibility, EpubError, EpubInspection, EpubLoss, EpubOptions, EpubOutput, EpubStory, EpubStyles, MAX_EPUB_CHAPTERS,
    MAX_EPUB_TEXT_BYTES, export_epub, inspect_epub,
};
pub use error::{IdmlError, PreservedBlob, Result};
pub use folio_export::{ExportNames, FolioExport, FolioExportInput, StoryInput, export_folio};
pub use idml_decode::{decode_idml, import_idml, open_idml};
pub use idml_encode::{ContentEdit, export_idml, export_model, replace_content, write_idml};
pub use idml_longdoc::{LongDocExportInput, LongDocImport, export_folio_longdoc, import_longdoc};
pub use ids::{IdMint, SequentialIdMint};
pub use indd_adapter::{IndKind, identify_indd, import_indd};
pub use model::*;
pub use pdf_export::{
    PdfAssets, PdfExportError, PdfExportInput, PdfExportOutput, PdfFont, PdfGlyph, PdfLongDoc, PdfOutlineItem, PdfPageLabel, PdfStory, PdfTable,
    PlacedMask, PlacedRaster, export_pdf, export_pdf_with, outline_from_toc, page_labels_from_sections, pdf_glyphs_from_type,
};
pub use reflow::{
    FixedAdvanceShaper, ParagraphShaper, ReflowOptions, ShapeContext, ThreadReflow, reflow_import,
};

/// The layout crate whose `Composer`, `LayoutError`, `LayoutProposal` and `SourceSpan` appear in the
/// reflow API.
pub use hsk_studio_layout as layout;

/// The PDF writer whose `ExportOptions`, `Limits`, `Colour` and `ExportReceipt` appear in the PDF
/// export API.
pub use hsk_studio_press as press;

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{Observation, Observe, Outcome, Receipt, SinkPort};

/// Maps an import/export result to exactly one Observe terminal outcome (no ids, part names or
/// text). An import that carries losses is still `Success`; the loss list is the detail. Blocked
/// native editability is `Failure(Unavailable)`, unsupported input `Failure(Unsupported)`, hostile
/// or invalid input `Failure(Validation)`, cancellation `Canceled`. `correlation_id` and `revision`
/// must equal the values `observe` was created with.
pub fn report<T>(
    result: &Result<T>,
    correlation_id: u64,
    revision: u64,
    cancel: &CancellationToken,
    observe: &mut Observe,
    sink: &mut (impl SinkPort + ?Sized),
) -> std::result::Result<Receipt, hsk_studio_observe::Error> {
    let outcome = match result {
        Ok(_) => Outcome::Success,
        Err(error) => match error.failure_code() {
            None => Outcome::Canceled,
            Some(code) => Outcome::Failure(code),
        },
    };
    observe.emit(
        Observation {
            correlation_id,
            revision,
            outcome,
            progress: None,
            private_project_text: None,
        },
        cancel,
        sink,
    )
}
