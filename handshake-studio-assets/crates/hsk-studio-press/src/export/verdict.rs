//! External conformance verdicts (lane SMB-LAYOUT-PDFX-VERAPDF items 1-2). `conformance` on an
//! [`ExportReceipt`] stays `not_proven` unless a passing veraPDF XML report (`--format xml`) for
//! exactly these bytes is attached; the receipt then cites the report's sha256 so anyone can re-run
//! veraPDF and compare. The report is read as text (attribute scan, no XML library); anything that
//! is not one single compliant job of the expected profile is refused with a typed reason.
#![deny(clippy::arithmetic_side_effects)]
use super::{ExportError, ExportReceipt};
use crate::pdf_edit::sha256_hex;

/// `conformance` after a passing veraPDF PDF/A-2b report is attached.
pub const CONFORMANCE_PDFA2B_VERAPDF: &str = "pdfa_2b_verapdf_passed";
/// The veraPDF profile name a PDF/A-2b report must carry.
pub const VERAPDF_PDFA2B_PROFILE: &str = "PDF/A-2b validation profile";
/// Largest report accepted (bytes).
pub const MAX_REPORT_BYTES: usize = 64 << 20;

/// The value of attribute `name` on the only `<tag ...>` element of `text`; `None` when the tag is
/// absent, repeated, or lacks the attribute.
fn only_attr<'a>(text: &'a str, tag: &str, name: &str) -> Option<&'a str> {
    let open = format!("<{tag} ");
    let mut found = text.match_indices(&open);
    let (at, _) = found.next()?;
    if found.next().is_some() {
        return None;
    }
    let start = at.checked_add(open.len())?;
    let rest = text.get(start..)?;
    let head = rest.get(..rest.find('>')?)?;
    let key = format!(" {name}=\"");
    let padded_at = format!(" {head}").find(&key)?;
    // `padded_at` indexes the head with one leading space; the value starts after the key.
    let v = head.get(padded_at.checked_add(key.len())?.checked_sub(1)?..)?;
    v.get(..v.find('"')?)
}

/// Count of `<tag` element openings (`<tag>` or `<tag ...>`).
fn count(text: &str, tag: &str) -> usize {
    let a = text.matches(&format!("<{tag}>")).count();
    let b = text.matches(&format!("<{tag} ")).count();
    a.saturating_add(b)
}

impl ExportReceipt {
    /// Attaches a veraPDF XML report for `pdf` (the bytes this receipt describes) validated with
    /// the PDF/A-2b profile (`-f 2b --format xml`). Passing: one job, item size = `pdf.len()`,
    /// `jobEndStatus="normal"`, `isCompliant="true"`, zero failed rules and checks, batch summary
    /// one compliant report and no failed jobs. Then `conformance` becomes
    /// [`CONFORMANCE_PDFA2B_VERAPDF`] and the report sha256 and validator are recorded. PDF/X-4 is
    /// not covered: veraPDF 1.30.3 ships no PDF/X profile.
    pub fn with_verapdf_report(mut self, pdf: &[u8], report_xml: &[u8]) -> Result<Self, ExportError> {
        let refuse = |why| Err(ExportError::ReportRefused(why));
        if sha256_hex(pdf) != self.sha256_hex {
            return refuse("pdf_hash_mismatch");
        }
        if report_xml.len() > MAX_REPORT_BYTES {
            return refuse("report_too_large");
        }
        let Ok(text) = std::str::from_utf8(report_xml) else { return refuse("report_not_utf8") };
        if !text.contains("<report>") || count(text, "job") != 1 || count(text, "validationReport") != 1 {
            return refuse("not_one_verapdf_job");
        }
        let Some(version) = text
            .match_indices("<releaseDetails ")
            .filter_map(|(i, _)| text.get(i..).and_then(|t| t.get(..t.find('>')?)))
            .find(|h| h.contains(" id=\"core\""))
            .and_then(|h| only_attr(&format!("{h}>"), "releaseDetails", "version").map(str::to_owned))
        else {
            return refuse("no_verapdf_version");
        };
        if only_attr(text, "item", "size") != Some(pdf.len().to_string().as_str()) {
            return refuse("item_size_mismatch");
        }
        let profile = only_attr(text, "validationReport", "profileName");
        if profile != Some(VERAPDF_PDFA2B_PROFILE) {
            return refuse("wrong_profile");
        }
        let checks = [
            ("validationReport", "jobEndStatus", "normal"),
            ("validationReport", "isCompliant", "true"),
            ("details", "failedRules", "0"),
            ("details", "failedChecks", "0"),
            ("validationReports", "compliant", "1"),
            ("validationReports", "nonCompliant", "0"),
            ("validationReports", "failedJobs", "0"),
            ("batchSummary", "totalJobs", "1"),
            ("batchSummary", "failedToParse", "0"),
            ("batchSummary", "veraExceptions", "0"),
        ];
        if checks.iter().any(|(tag, attr, want)| only_attr(text, tag, attr) != Some(want)) {
            return refuse("not_compliant");
        }
        self.conformance = CONFORMANCE_PDFA2B_VERAPDF;
        self.conformance_report_sha256 = Some(sha256_hex(report_xml));
        self.conformance_validator = Some(format!("veraPDF {version}: {VERAPDF_PDFA2B_PROFILE}"));
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_scan() {
        let t = "<a x=\"1\" y=\"2\"><b z=\"3\"></b><c q=\"4\"/><c q=\"5\"/></a>";
        assert_eq!(only_attr(t, "a", "x"), Some("1"));
        assert_eq!(only_attr(t, "a", "y"), Some("2"));
        assert_eq!(only_attr(t, "b", "z"), Some("3"));
        assert_eq!(only_attr(t, "c", "q"), None, "repeated");
        assert_eq!(only_attr(t, "a", "w"), None);
        assert_eq!(only_attr(t, "d", "x"), None);
        assert_eq!(count(t, "c"), 2);
    }
}
