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

/// The one normalisation [`ExportReceipt::with_verapdf_report`] applies before any check and
/// before hashing (lane SMB-R31-FINDINGS; canary C45 machine-path check). veraPDF 1.30.3 writes
/// the validated file's ABSOLUTE path into the job's `<item><name>`, also for a relative input
/// path (and stdin input reports `size="-1"`, losing the size binding), so a committed report
/// carried a machine path and the same bytes validated in two folders hashed differently. The
/// only change: the text of the one `<name>` element, which must sit inside the one
/// `<item ...>...</item>` of the one job, is cut to its last path component (after the last `/` or
/// `\`). Every other byte is kept, so an edit anywhere else still changes the normalised report,
/// whose sha256 the receipt records, or is refused. A normalised report is a fixed point.
/// Refusals: `report_not_utf8`, `not_one_verapdf_job`, `item_name_not_one` (not exactly one
/// `<name>` inside the one item), `item_name_empty`.
pub fn normalize_verapdf_report(report_xml: &[u8]) -> Result<Vec<u8>, ExportError> {
    let refuse = |why| Err(ExportError::ReportRefused(why));
    let Ok(text) = std::str::from_utf8(report_xml) else { return refuse("report_not_utf8") };
    if !text.contains("<report>") || count(text, "job") != 1 || count(text, "validationReport") != 1 {
        return refuse("not_one_verapdf_job");
    }
    let one = |needle: &str| text.matches(needle).count() == 1;
    if count(text, "item") != 1 || !one("</item>") || !one("<name") || !one("</name>") {
        return refuse("item_name_not_one");
    }
    let Some((before, after)) = text.split_once("<name>") else { return refuse("item_name_not_one") };
    let Some((name, tail)) = after.split_once("</name>") else { return refuse("item_name_not_one") };
    // `<item ...>` is the last markup before `<name>` (whitespace between) and `</item>` the first after it.
    let item_open = before.rfind("<item ").and_then(|at| before.get(at..)).map(str::trim_end);
    let inside = item_open.is_some_and(|t| t.ends_with('>') && t.matches('>').count() == 1 && t.matches('<').count() == 1)
        && tail.trim_start().starts_with("</item>");
    if !inside || name.contains('<') {
        return refuse("item_name_not_one");
    }
    let file = name.rsplit(['/', '\\']).next().unwrap_or(name);
    if file.is_empty() {
        return refuse("item_name_empty");
    }
    Ok(format!("{before}<name>{file}</name>{tail}").into_bytes())
}

impl ExportReceipt {
    /// Attaches a veraPDF XML report for `pdf` (the bytes this receipt describes) validated with
    /// the PDF/A-2b profile (`-f 2b --format xml`). Passing: one job, item size = `pdf.len()`,
    /// `jobEndStatus="normal"`, `isCompliant="true"`, zero failed rules and checks, batch summary
    /// one compliant report and no failed jobs. Then `conformance` becomes
    /// [`CONFORMANCE_PDFA2B_VERAPDF`] and the sha256 of the report as normalised by
    /// [`normalize_verapdf_report`] (item name cut to the file name) and the validator are
    /// recorded. PDF/X-4 is not covered: veraPDF 1.30.3 ships no PDF/X profile.
    pub fn with_verapdf_report(mut self, pdf: &[u8], report_xml: &[u8]) -> Result<Self, ExportError> {
        let refuse = |why| Err(ExportError::ReportRefused(why));
        if sha256_hex(pdf) != self.sha256_hex {
            return refuse("pdf_hash_mismatch");
        }
        if report_xml.len() > MAX_REPORT_BYTES {
            return refuse("report_too_large");
        }
        // Every check below and the recorded sha256 are over the normalised report.
        let report_xml = normalize_verapdf_report(report_xml)?;
        let Ok(text) = std::str::from_utf8(&report_xml) else { return refuse("report_not_utf8") };
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
        self.conformance_report_sha256 = Some(sha256_hex(&report_xml));
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

    /// The committed press fixture (normalised: the item name is the file name only).
    const FIXTURE: &str = include_str!("../../../hsk-studio-press/tests/fixtures/verapdf/press-pdfx4-corpus.pdfa-2b.xml");

    /// The normalisation cuts only the item name's directory: a raw veraPDF report (absolute
    /// path, either separator) normalises to the committed fixture, the fixture is a fixed point,
    /// and flipping any byte outside that directory changes the normalised report or is refused.
    #[test]
    fn report_normalisation_cuts_only_the_item_name_directory() {
        let fixed = FIXTURE.as_bytes();
        let name = "<name>press-pdfx4-corpus.pdf</name>";
        assert!(FIXTURE.contains(name) && !FIXTURE.contains(":\\") && !FIXTURE.contains("<name>/"));
        assert_eq!(normalize_verapdf_report(fixed).unwrap(), fixed, "fixed point");
        // A drive-letter directory built at run time (no machine-path literal in the source, V4-03).
        let dir = format!("{}:\\Projects\\LLM projects\\Handshake_Artifacts\\corpus\\", 'D');
        let dir = dir.as_str();
        for d in [dir, "/home/u/corpus/", "rel/"] {
            let raw = FIXTURE.replacen("<name>", &format!("<name>{d}"), 1);
            assert_eq!(normalize_verapdf_report(raw.as_bytes()).unwrap(), fixed, "{d}");
        }
        let raw = FIXTURE.replacen("<name>", &format!("<name>{dir}"), 1);
        let start = raw.find(dir).unwrap();
        let end = start.checked_add(dir.len()).unwrap();
        let mut checked = 0usize;
        for i in (0..raw.len()).filter(|i| !(start..end).contains(i)) {
            let mut edited = raw.clone().into_bytes();
            edited[i] ^= 1;
            let out = normalize_verapdf_report(&edited);
            assert!(out.as_deref() != Ok(fixed), "byte {i} edit not detected");
            checked = checked.checked_add(1).unwrap();
        }
        assert_eq!(Some(checked), raw.len().checked_sub(dir.len()));
        // A name that is not the one item's own name is refused, not redacted.
        let refused = |t: &str| normalize_verapdf_report(t.as_bytes()).err();
        let not_one = Some(ExportError::ReportRefused("item_name_not_one"));
        assert_eq!(refused(&FIXTURE.replacen(name, &format!("{name}{name}"), 1)), not_one);
        assert_eq!(refused(&FIXTURE.replacen(name, &format!("<x/>{name}"), 1)), not_one);
        assert_eq!(refused(&FIXTURE.replacen(name, "", 1)), not_one);
        assert_eq!(refused(&FIXTURE.replacen(name, "<name>a/<b>c</name>", 1)), not_one);
        assert_eq!(refused(&FIXTURE.replacen(name, "<name>dir/</name>", 1)), Some(ExportError::ReportRefused("item_name_empty")));
        assert_eq!(normalize_verapdf_report(&[0xff]).err(), Some(ExportError::ReportRefused("report_not_utf8")));
    }
}
