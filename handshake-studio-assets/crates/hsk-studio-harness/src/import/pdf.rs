//! PDF is carried, not imported (SMB-CONVERT-NATIVE CN-3/CN-5, remediation R24; feature
//! `import-pdf`). Page import needs a public page raster from press (decision P1, open), so the
//! converted package holds an empty native document (no artboards, no layers), the original PDF as
//! the `original` import entry and an import receipt with the page count and media boxes from
//! `hsk_studio_press::PdfDoc::proposal`; every page is the typed loss `page[n]`
//! `pdf_pages_not_imported` (preserved opaque: its bytes are in the original entry). Press refusals
//! (malformed, over limits) pass through unchanged as `ImportError::Importer { format: pdf, .. }`.
//! Test: `tests/import_pdf.rs` `pdf_is_carried_with_its_original_and_a_pages_not_imported_receipt`.
#![deny(clippy::arithmetic_side_effects)]

use super::with_package::{RECEIPT_NOT_PROVEN, SourceMint};
use super::{ImportError, ImportedDocument, SourceFormat};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{self as folio, Inspection, Resolver};
use hsk_studio_package::{ImportEntry, ImportRole, MemoryAssetSource, sha256_hex_of};
use hsk_studio_press as press;
use serde_json::json;

const FORMAT: SourceFormat = SourceFormat::Pdf;

pub(crate) fn import_pdf(bytes: &[u8], resolver: &dyn Resolver, cancel: &CancellationToken) -> Result<ImportedDocument, ImportError> {
    let refusal = |e: press::PressError| match e {
        press::PressError::Canceled => ImportError::Canceled,
        other => ImportError::Importer { format: FORMAT, code: other.code() },
    };
    let doc = press::PdfDoc::open(bytes, &press::Limits::default(), cancel).map_err(refusal)?;
    let proposal = doc.proposal(cancel).map_err(refusal)?;
    drop(doc);
    cancel.check().map_err(|_| ImportError::Canceled)?;

    let digest = sha256_hex_of(bytes);
    let mut mint = SourceMint::from_sha256_hex(&digest);
    let document_id = mint.next("SDOC");
    // An empty native document: nothing of the PDF is mapped to editable content.
    let wire = json!({
        "schema_id": "hsk.studio.document@1",
        "document_id": document_id,
        "revision": 1,
        "geometry_unit": "pt",
        "colour_mode": 2,
        "bits_per_channel": 8,
        "working_space_rgb": null,
        "working_space_cmyk": null,
        "working_space_gray": null,
        "working_space_spot": null,
        "blending_space": null,
        "artboards": [],
        "page_spreads": [],
        "layers": [],
        "graph": {"schema_id": "hsk.studio.layer_graph@1", "operations": [], "edges": [], "outputs": []},
    });
    let wire = serde_json::to_vec(&wire).map_err(|_| ImportError::Package("document_encode"))?;
    let snapshot = match folio::inspect_bytes(&wire, folio::Budget::default(), cancel, resolver) {
        Ok(Inspection::Editable(s)) => s,
        Ok(Inspection::ReadOnly(_)) => return Err(ImportError::Package("folio_read_only")),
        Err(d) if d.code == folio::Code::Canceled => return Err(ImportError::Canceled),
        Err(_) => return Err(ImportError::Package("folio_rejected")),
    };

    let pages: Vec<_> = proposal
        .pages
        .iter()
        .map(|p| json!({"index": p.index, "media_box": p.media_box, "rotate": p.rotate, "content_streams": p.content_streams, "annotations": p.annotations.len()}))
        .collect();
    let loss: Vec<_> = proposal
        .pages
        .iter()
        .map(|p| json!({"path": format!("page[{}]", p.index), "kind": "preserved_opaque", "code": "pdf_pages_not_imported"}))
        .collect();
    let original = ImportEntry::new(ImportRole::Original, bytes.to_vec());
    let receipt = json!({
        "receipt": "import",
        "receipt_version": 1,
        "source_format": "pdf",
        "importer": "hsk-studio-press (carried, not imported)",
        "source": {"sha256": digest, "byte_len": bytes.len(), "entry": original.name()},
        "layers": 0,
        "tiles": 0,
        "mask_tiles": 0,
        "records": false,
        "loss": loss,
        "not_proven": RECEIPT_NOT_PROVEN,
        "pdf": {
            "status": "carried_not_imported",
            "reason": "no public press page raster for import (decision P1 open)",
            "version": proposal.version, "encrypted": proposal.encrypted,
            "page_count": proposal.pages.len(), "pages": pages,
            "form_fields": proposal.form_fields.len(), "signatures": proposal.signatures.len(),
        },
    });
    let receipt_bytes = serde_json::to_vec(&receipt).map_err(|_| ImportError::Package("receipt_encode"))?;
    let imports = vec![ImportEntry::new(ImportRole::Receipt, receipt_bytes), original];
    Ok(ImportedDocument { format: FORMAT, snapshot, assets: MemoryAssetSource::new(), imports, receipt })
}
