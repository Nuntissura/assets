//! SMB-CONVERT-NATIVE remediation R24 (class pdf-carry-receipt-missing): a `%PDF-` source is carried,
//! not imported (feature `import-pdf`): the package holds the original PDF and an import receipt with
//! the page count, media boxes and one typed `pdf_pages_not_imported` loss per page; nothing editable
//! is invented. The PDF bytes are assembled here with a computed xref table (no press encoder).
#![cfg(feature = "import-pdf")]

use hsk_studio_accord::CancellationToken;
use hsk_studio_folio as folio;
use hsk_studio_harness::import::{SourceFormat, convert_to_native, sniff};
use hsk_studio_harness::{Harness, References, Selection, asset_local_test_context};
use hsk_studio_package::{ImportRole, Limits, read_package, sha256_hex_of};
use serde_json::Value;

/// Two pages: A4 portrait (595x842) and US Letter landscape via its own MediaBox (792x612).
fn two_page_pdf() -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 595 842] >>",
        "<< /Type /Page /Parent 2 0 R /Contents 5 0 R >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 792 612] /Contents 5 0 R >>",
        "<< /Length 7 >>\nstream\n0 0 m S\nendstream",
    ];
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).bytes());
    }
    let xref_at = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
    for at in offsets {
        out.extend(format!("{at:010} 00000 n \n").bytes());
    }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n", objects.len() + 1).bytes());
    out
}

#[test]
fn pdf_is_carried_with_its_original_and_a_pages_not_imported_receipt() {
    let bytes = two_page_pdf();
    assert_eq!(sniff(&bytes), Some(SourceFormat::Pdf));
    let cancel = CancellationToken::default();
    let (package, imported) = convert_to_native(&bytes, &TrustAll, &cancel).expect("carried");
    assert_eq!(imported.format, SourceFormat::Pdf);
    assert!(imported.snapshot.document().layers.is_empty(), "nothing editable is invented");
    let receipt = &imported.receipt;
    assert_eq!(receipt["source_format"], "pdf");
    assert_eq!(receipt["layers"], 0);
    assert_eq!(receipt["source"]["sha256"], sha256_hex_of(&bytes));
    assert_eq!(receipt["pdf"]["status"], "carried_not_imported");
    assert_eq!(receipt["pdf"]["page_count"], 2);
    let boxes: Vec<Value> = receipt["pdf"]["pages"].as_array().unwrap().iter().map(|p| p["media_box"].clone()).collect();
    assert_eq!(boxes, vec![serde_json::json!([0.0, 0.0, 595.0, 842.0]), serde_json::json!([0.0, 0.0, 792.0, 612.0])]);
    let loss: Vec<(String, String)> = receipt["loss"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["path"].as_str().unwrap().to_string(), e["code"].as_str().unwrap().to_string()))
        .collect();
    assert_eq!(
        loss,
        vec![("page[0]".into(), "pdf_pages_not_imported".into()), ("page[1]".into(), "pdf_pages_not_imported".into())]
    );

    // The package carries the original byte-exact and the receipt it reports.
    let opened = read_package(&package, &Limits::default(), folio::Budget::default(), &TrustAll, &cancel).unwrap();
    assert_eq!(opened.import_bytes(ImportRole::Original), Some(&bytes[..]));
    let stored: Value = serde_json::from_slice(opened.import_bytes(ImportRole::Receipt).unwrap()).unwrap();
    assert_eq!(&stored, receipt);
    assert!(opened.assets.is_empty());

    // Open directly: the carried document opens in a live session with no layers.
    let h = Harness::new(Selection { providers: Vec::new(), references: References::TrustAll }).unwrap();
    let mut run = h.run(asset_local_test_context(), &cancel);
    assert!(run.open_import(&bytes));
    assert!(run.summary());
    let report = run.finish("import");
    assert_eq!(report["verdict"], "pass", "{report}");
    assert_eq!(report["ops"][0]["detail"]["import"]["format"], "pdf", "{report}");
    assert_eq!(report["ops"][1]["detail"]["layers"].as_array().map(Vec::len), Some(0), "{report}");
}

struct TrustAll;
impl folio::Resolver for TrustAll {
    fn primitive(&self, _: &str, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn profile(&self, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn tile(&self, _: &folio::TileRef) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}
