//! PDF/X-4 writing support and a self-check of the rules this crate implements (lane
//! SMB-LAYOUT-PDF-EXPORT-3). The rules below are this crate's reading of ISO 15930-7 (PDF/X-4) as
//! recalled, NOT checked against the published text (UNVERIFIED, D5): a file that passes them is
//! still `conformance: not_proven` (STU-PDF-002) until an external validator (for example veraPDF)
//! accepts it. The self-check reads the written bytes back through the crate's strict reader; it
//! does not share code with the writer beyond that reader.
#![deny(clippy::arithmetic_side_effects)]
use super::ExportError;
use crate::error::Limits;
use crate::pdf_edit::{PdfDoc, text_string};
use crate::syntax::Object;
use hsk_studio_accord::CancellationToken;

/// What the writer puts into `/GTS_PDFXVersion` and the XMP `pdfxid:GTS_PDFXVersion`.
pub const PDFX4_VERSION: &str = "PDF/X-4";

/// One self-checked rule: a stable id and whether the file satisfies it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfXRule {
    pub id: &'static str,
    pub passed: bool,
}

/// The self-check result. `conformance` stays `not_proven` whatever the rules say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfXReport {
    pub rules: Vec<PdfXRule>,
    pub conformance: &'static str,
}
impl PdfXReport {
    pub fn all_passed(&self) -> bool {
        self.rules.iter().all(|r| r.passed)
    }
    pub fn failed(&self) -> Vec<&'static str> {
        self.rules.iter().filter(|r| !r.passed).map(|r| r.id).collect()
    }
}

/// XML-escapes `s` for an XMP text value.
pub(super) fn xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// A PDF date string `D:YYYYMMDDHHmmSS` with an optional `Z` or `+HH'mm'` offset (ISO 32000-1
/// 7.9.4); the year, month and day are required by this check.
pub(super) fn valid_date(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("D:") else { return false };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.len() < 8 || digits.len() > 14 || digits.len() & 1 != 0 {
        return false;
    }
    let tail = &rest[digits.len()..];
    tail.is_empty() || tail == "Z" || (tail.len() == 7 && (tail.starts_with('+') || tail.starts_with('-')))
}

/// The ISO 8601 form of a PDF date for XMP (`2026-10-15T00:00:00Z`).
pub(super) fn xmp_date(s: &str) -> String {
    let d: Vec<char> = s.trim_start_matches("D:").chars().take(14).collect();
    let part = |a: usize, b: usize, def: &str| -> String {
        if d.len() >= b { d[a..b].iter().collect() } else { def.to_owned() }
    };
    format!("{}-{}-{}T{}:{}:{}Z", part(0, 4, "0000"), part(4, 6, "01"), part(6, 8, "01"), part(8, 10, "00"), part(10, 12, "00"), part(12, 14, "00"))
}

/// Self-check of the PDF/X-4 rules this crate implements, on the written bytes.
pub fn check_pdfx4(bytes: &[u8], limits: &Limits, cancel: &CancellationToken) -> Result<PdfXReport, ExportError> {
    let doc = PdfDoc::open(bytes, limits, cancel)?;
    let inspection = doc.inspect();
    let mut rules = Vec::new();
    let mut rule = |id: &'static str, passed: bool| rules.push(PdfXRule { id, passed });
    let version_ok = inspection.version.split_once('.').and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?))).is_some_and(|(a, b)| a > 1 || (a == 1 && b >= 6));
    rule("pdfx4.header_version_1_6", version_ok);
    rule("pdfx4.no_encryption", !inspection.encrypted);
    let catalog = doc.catalog()?;
    let intents: Vec<&Object> = catalog.get("OutputIntents").map(|o| doc.resolve(o)).and_then(Object::as_array).map(|a| a.iter().map(|o| doc.resolve(o)).collect()).unwrap_or_default();
    let pdfx: Vec<_> = intents.iter().filter_map(|o| o.as_dict()).filter(|d| d.get("S").is_some_and(|s| s.is_name("GTS_PDFX"))).collect();
    let profile_ok = pdfx.first().and_then(|d| d.get("DestOutputProfile")).map(|o| doc.resolve(o)).is_some_and(|o| {
        let Object::Stream(s) = o else { return false };
        let mut budget = limits.max_decoded_total;
        crate::filters::decode_stream(s, limits, &mut budget).is_ok_and(|icc| icc.get(36..40) == Some(b"acsp"))
    });
    rule("pdfx4.one_gts_pdfx_output_intent", pdfx.len() == 1);
    rule("pdfx4.dest_output_profile_icc", profile_ok);
    let info = doc.info_ref().and_then(|r| doc.dict_of(r.num));
    let info_text = |k: &str| -> Option<String> {
        match info?.get(k).map(|o| doc.resolve(o)) {
            Some(Object::Str(s)) => Some(text_string(s)),
            _ => None,
        }
    };
    rule("pdfx4.info_gts_pdfxversion", info_text("GTS_PDFXVersion").is_some_and(|v| v.starts_with(PDFX4_VERSION)));
    rule("pdfx4.info_dates", info_text("CreationDate").is_some_and(|d| valid_date(&d)) && info_text("ModDate").is_some_and(|d| valid_date(&d)));
    rule("pdfx4.info_trapped", info.and_then(|i| i.get("Trapped")).is_some_and(|t| t.is_name("True") || t.is_name("False")));
    let xmp_ok = match catalog.get("Metadata").map(|o| doc.resolve(o)) {
        Some(Object::Stream(s)) => {
            let mut budget = limits.max_decoded_total;
            crate::filters::decode_stream(s, limits, &mut budget).is_ok_and(|x| {
                let t = String::from_utf8_lossy(&x);
                t.contains("pdfxid:GTS_PDFXVersion") && t.contains(PDFX4_VERSION)
            })
        }
        _ => false,
    };
    rule("pdfx4.xmp_metadata_pdfxid", xmp_ok);
    rule("pdfx4.trailer_id", matches!(doc.trailer.get("ID"), Some(Object::Array(a)) if a.len() == 2));
    let mut boxes_ok = !doc.pages.is_empty();
    for num in &doc.pages {
        let Some(d) = doc.dict_of(*num) else {
            boxes_ok = false;
            continue;
        };
        let media = doc.rect_of(doc.inherited(*num, "MediaBox"));
        let trim = doc.rect_of(d.get("TrimBox")).or_else(|| doc.rect_of(d.get("ArtBox")));
        let bleed = doc.rect_of(d.get("BleedBox"));
        let inside = |a: [f64; 4], b: [f64; 4]| a[0] >= b[0] && a[1] >= b[1] && a[2] <= b[2] && a[3] <= b[3];
        boxes_ok &= match (media, trim) {
            (Some(m), Some(t)) => inside(t, m) && bleed.is_none_or(|b| inside(t, b) && inside(b, m)),
            _ => false,
        };
    }
    rule("pdfx4.page_trim_inside_bleed_inside_media", boxes_ok);
    let mut fonts_ok = true;
    for e in doc.objects.values() {
        let Some(d) = e.obj.as_dict() else { continue };
        if !d.get("Type").is_some_and(|t| t.is_name("FontDescriptor")) {
            continue;
        }
        fonts_ok &= ["FontFile", "FontFile2", "FontFile3"].iter().any(|k| d.get(k).is_some());
    }
    rule("pdfx4.fonts_embedded", fonts_ok);
    Ok(PdfXReport { rules, conformance: super::EXPORT_CONFORMANCE })
}
