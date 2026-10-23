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

/// The ISO 8601 form of a PDF date for XMP (`2026-10-15T00:00:00Z`; an offset `+01'00'` becomes
/// `+01:00`, so the XMP date names the same instant as the Info date, PDF/A-2 6.6.3).
pub(super) fn xmp_date(s: &str) -> String {
    let rest = s.trim_start_matches("D:");
    let n = rest.chars().take_while(char::is_ascii_digit).count().min(14);
    let d: Vec<char> = rest.chars().take(n).collect();
    let part = |a: usize, b: usize, def: &str| -> String {
        if d.len() >= b { d[a..b].iter().collect() } else { def.to_owned() }
    };
    let tail = rest.get(n..).unwrap_or("");
    let zone = match (tail.get(..1), tail.get(1..3), tail.get(4..6)) {
        (Some(sign @ ("+" | "-")), Some(h), Some(m)) => format!("{sign}{h}:{m}"),
        _ => "Z".to_owned(),
    };
    format!("{}-{}-{}T{}:{}:{}{zone}", part(0, 4, "0000"), part(4, 6, "01"), part(6, 8, "01"), part(8, 10, "00"), part(10, 12, "00"), part(12, 14, "00"))
}

/// What the writer puts into the XMP `pdfaid:part` / `pdfaid:conformance` for PDF/A-2b.
pub const PDFA2B_PART: &str = "2";
pub const PDFA2B_CONFORMANCE: &str = "B";

/// One XMP extension schema entry (PDF/A-2 6.6.2.3.2) declaring a single Text property.
fn extension_schema(schema: &str, uri: &str, prefix: &str, property: &str, description: &str) -> String {
    format!(
        "<rdf:li rdf:parseType=\"Resource\">\n<pdfaSchema:schema>{schema}</pdfaSchema:schema>\n<pdfaSchema:namespaceURI>{uri}</pdfaSchema:namespaceURI>\n<pdfaSchema:prefix>{prefix}</pdfaSchema:prefix>\n<pdfaSchema:property><rdf:Seq><rdf:li rdf:parseType=\"Resource\">\n<pdfaProperty:name>{property}</pdfaProperty:name>\n<pdfaProperty:valueType>Text</pdfaProperty:valueType>\n<pdfaProperty:category>internal</pdfaProperty:category>\n<pdfaProperty:description>{description}</pdfaProperty:description>\n</rdf:li></rdf:Seq></pdfaSchema:property>\n</rdf:li>\n"
    )
}

/// The document XMP packet: `dc:title` (when titled), `pdf:Producer`, the PDF/X-4 properties
/// (dates, `pdf:Trapped`, `pdfxid:GTS_PDFXVersion`) when `x4` is set, and the PDF/A-2b
/// identification with extension schemas for the properties XMP 2005 does not predefine
/// (`pdf:Trapped`, `pdfxid:GTS_PDFXVersion`; veraPDF 6.6.2.3.1) when `pdfa2b`.
pub(super) fn xmp_packet(title: Option<&str>, x4: Option<(&str, &str, bool)>, pdfa2b: bool) -> String {
    let mut ns = String::from(" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\"");
    let mut body = String::new();
    if let Some(t) = title {
        body.push_str(&format!("<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:title>\n", xml(t)));
    }
    body.push_str("<pdf:Producer>hsk-studio-press</pdf:Producer>\n");
    if let Some((created, modified, trapped)) = x4 {
        ns.push_str(" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmlns:pdfxid=\"http://www.npes.org/pdfx/ns/id/\"");
        body.push_str(&format!(
            "<xmp:CreateDate>{}</xmp:CreateDate>\n<xmp:ModifyDate>{}</xmp:ModifyDate>\n<pdf:Trapped>{}</pdf:Trapped>\n<pdfxid:GTS_PDFXVersion>{PDFX4_VERSION}</pdfxid:GTS_PDFXVersion>\n",
            xmp_date(created),
            xmp_date(modified),
            if trapped { "True" } else { "False" },
        ));
    }
    if pdfa2b {
        ns.push_str(" xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\"");
        body.push_str(&format!("<pdfaid:part>{PDFA2B_PART}</pdfaid:part>\n<pdfaid:conformance>{PDFA2B_CONFORMANCE}</pdfaid:conformance>\n"));
        if x4.is_some() {
            ns.push_str(" xmlns:pdfaExtension=\"http://www.aiim.org/pdfa/ns/extension/\" xmlns:pdfaSchema=\"http://www.aiim.org/pdfa/ns/schema#\" xmlns:pdfaProperty=\"http://www.aiim.org/pdfa/ns/property#\"");
            body.push_str("<pdfaExtension:schemas><rdf:Bag>\n");
            body.push_str(&extension_schema("Adobe PDF Schema", "http://ns.adobe.com/pdf/1.3/", "pdf", "Trapped", "Whether the document has been trapped"));
            body.push_str(&extension_schema("PDF/X ID Schema", "http://www.npes.org/pdfx/ns/id/", "pdfxid", "GTS_PDFXVersion", "ID of PDF/X standard"));
            body.push_str("</rdf:Bag></pdfaExtension:schemas>\n");
        }
    }
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n<rdf:Description rdf:about=\"\"{ns}>\n{body}</rdf:Description>\n</rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>"
    )
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
            // Parsed and matched by namespace URI (F-R31-2): a comment or decoy text no longer satisfies it.
            crate::filters::decode_stream(s, limits, &mut budget)
                .is_ok_and(|x| std::str::from_utf8(&x).ok().and_then(xmp_pdfx_version).is_some_and(|v| v == PDFX4_VERSION))
        }
        _ => false,
    };
    rule("pdfx4.xmp_metadata_pdfxid", xmp_ok);
    rule("pdfx4.trailer_id", matches!(doc.seam_trailer().get("ID"), Some(Object::Array(a)) if a.len() == 2));
    let mut boxes_ok = !doc.seam_pages().is_empty();
    for num in doc.seam_pages() {
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
    for e in doc.seam_objects().values() {
        let Some(d) = e.obj.as_dict() else { continue };
        if !d.get("Type").is_some_and(|t| t.is_name("FontDescriptor")) {
            continue;
        }
        fonts_ok &= ["FontFile", "FontFile2", "FontFile3"].iter().any(|k| d.get(k).is_some());
    }
    rule("pdfx4.fonts_embedded", fonts_ok);
    Ok(PdfXReport { rules, conformance: super::EXPORT_CONFORMANCE })
}

/// Namespace URI of the PDF/X identification schema (`pdfxid` by convention; any prefix bound to it counts).
const PDFXID_NS: &str = "http://www.npes.org/pdfx/ns/id/";
/// Namespace URI of RDF.
const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
/// Element nesting an XMP packet may reach before it is refused (hostile input bound; no recursion).
const XMP_MAX_DEPTH: usize = 64;

/// One open element of [`xmp_pdfx_version`]'s walk.
struct XmlOpen<'a> {
    qname: &'a str,
    uri: String,
    local: &'a str,
    /// Namespace bindings in scope before this element (truncated back on its end tag).
    scope_len: usize,
    text: String,
    has_child: bool,
}

impl XmlOpen<'_> {
    fn is_description(&self) -> bool {
        self.uri == RDF_NS && self.local == "Description"
    }
    fn is_pdfx_version(&self) -> bool {
        self.uri == PDFXID_NS && self.local == "GTS_PDFXVersion"
    }
}

/// The value of the XMP property `GTS_PDFXVersion` in namespace [`PDFXID_NS`], whatever its
/// prefix: a property element directly inside an `rdf:Description` with text content only, or a
/// property attribute of an `rdf:Description` (both by namespace URI; RDF/XML abbreviation). The
/// packet is tokenized (start, end and empty tags, attributes in either quote, the five predefined
/// entities and character references, comments, processing instructions, CDATA) and must be well
/// formed to this reader: matched end tags, bound prefixes, one root element, depth at most
/// [`XMP_MAX_DEPTH`]; a DOCTYPE (entity declarations) is refused. `None` when it is not, or when the
/// property is absent, structured, or given more than once. Lane SMB-R31-FINDINGS (F-R31-2): the
/// former substring test over the raw text accepted a comment, a decoy attribute or a prefix bound
/// to another namespace.
fn xmp_pdfx_version(text: &str) -> Option<String> {
    let mut scopes: Vec<(&str, String)> = vec![("xml", "http://www.w3.org/XML/1998/namespace".to_owned())];
    let mut stack: Vec<XmlOpen<'_>> = Vec::new();
    let mut found: Vec<String> = Vec::new();
    let mut roots = 0usize;
    let mut rest = text.strip_prefix('\u{feff}').unwrap_or(text);
    loop {
        let (chars, after) = rest.split_at(rest.find('<').unwrap_or(rest.len()));
        match stack.last_mut() {
            Some(top) => top.text.push_str(&xml_unescape(chars)?),
            None if !chars.trim().is_empty() => return None,
            None => {}
        }
        if after.is_empty() {
            break;
        }
        if let Some(r) = after.strip_prefix("<?") {
            rest = r.get(r.find("?>")?..)?.strip_prefix("?>")?;
        } else if let Some(r) = after.strip_prefix("<!--") {
            rest = r.get(r.find("-->")?..)?.strip_prefix("-->")?;
        } else if let Some(r) = after.strip_prefix("<![CDATA[") {
            let end = r.find("]]>")?;
            stack.last_mut()?.text.push_str(r.get(..end)?);
            rest = r.get(end..)?.strip_prefix("]]>")?;
        } else if after.starts_with("<!") {
            return None;
        } else if let Some(r) = after.strip_prefix("</") {
            let end = r.find('>')?;
            let open = stack.pop()?;
            if r.get(..end)?.trim_end() != open.qname {
                return None;
            }
            scopes.truncate(open.scope_len);
            if open.is_pdfx_version() && stack.last().is_some_and(XmlOpen::is_description) {
                if open.has_child {
                    return None;
                }
                found.push(open.text.trim().to_owned());
            }
            rest = r.get(end..)?.strip_prefix('>')?;
        } else {
            let r = after.strip_prefix('<')?;
            let end = tag_end(r)?;
            let whole = r.get(..end)?;
            let (body, empty) = match whole.strip_suffix('/') {
                Some(b) => (b, true),
                None => (whole, false),
            };
            rest = r.get(end..)?.strip_prefix('>')?;
            let (qname, mut attrs_text) = body.split_at(body.find(char::is_whitespace).unwrap_or(body.len()));
            if qname.is_empty() || stack.len() >= XMP_MAX_DEPTH {
                return None;
            }
            let scope_len = scopes.len();
            let mut attrs: Vec<(&str, String)> = Vec::new();
            loop {
                attrs_text = attrs_text.trim_start();
                if attrs_text.is_empty() {
                    break;
                }
                let eq = attrs_text.find('=')?;
                let name = attrs_text.get(..eq)?.trim_end();
                let value_text = attrs_text.get(eq..)?.strip_prefix('=')?.trim_start();
                let quote = value_text.chars().next().filter(|c| *c == '"' || *c == '\'')?;
                let value_text = value_text.strip_prefix(quote)?;
                let close = value_text.find(quote)?;
                let value = xml_unescape(value_text.get(..close)?)?;
                attrs_text = value_text.get(close..)?.strip_prefix(quote)?;
                if name.is_empty() || name.contains(char::is_whitespace) {
                    return None;
                }
                if name == "xmlns" {
                    scopes.push(("", value));
                } else if let Some(prefix) = name.strip_prefix("xmlns:") {
                    scopes.push((prefix, value));
                } else {
                    attrs.push((name, value));
                }
            }
            let (uri, local) = xml_resolve(&scopes, qname, true)?;
            if stack.is_empty() {
                roots = roots.checked_add(1)?;
            }
            let parent_is_description = stack.last().is_some_and(XmlOpen::is_description);
            if let Some(parent) = stack.last_mut() {
                parent.has_child = true;
            }
            let open = XmlOpen { qname, uri, local, scope_len, text: String::new(), has_child: false };
            if open.is_description() {
                for (name, value) in &attrs {
                    let (attr_uri, attr_local) = xml_resolve(&scopes, name, false)?;
                    if attr_uri == PDFXID_NS && attr_local == "GTS_PDFXVersion" {
                        found.push(value.trim().to_owned());
                    }
                }
            }
            if empty {
                scopes.truncate(scope_len);
                if open.is_pdfx_version() && parent_is_description {
                    found.push(String::new());
                }
            } else {
                stack.push(open);
            }
        }
    }
    if !stack.is_empty() || roots != 1 || found.len() != 1 {
        return None;
    }
    found.pop()
}

/// Byte index of the `>` closing a tag body (quotes respected); `None` at a `<` or the end first.
fn tag_end(s: &str) -> Option<usize> {
    let mut quote = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            (None, '>') => return Some(i),
            (None, '<') => return None,
            _ => {}
        }
    }
    None
}

/// `(namespace URI, local name)` of a qualified name; an unprefixed attribute has no namespace, an
/// unprefixed element the default one (empty when none is bound). `None` for an unbound prefix.
fn xml_resolve<'a>(scopes: &[(&str, String)], qname: &'a str, element: bool) -> Option<(String, &'a str)> {
    let (prefix, local) = match qname.split_once(':') {
        Some((p, l)) if !p.is_empty() && !l.is_empty() && !l.contains(':') => (p, l),
        Some(_) => return None,
        None if !element => return Some((String::new(), qname)),
        None => ("", qname),
    };
    match scopes.iter().rev().find(|(p, _)| *p == prefix) {
        Some((_, uri)) => Some((uri.clone(), local)),
        None if prefix.is_empty() => Some((String::new(), local)),
        None => None,
    }
}

/// Character data with the five predefined entities and character references replaced; `None`
/// for any other entity or a malformed reference.
fn xml_unescape(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        let (head, tail) = rest.split_at(amp);
        out.push_str(head);
        let tail = tail.strip_prefix('&')?;
        let semi = tail.find(';')?;
        let entity = tail.get(..semi)?;
        out.push(match entity {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let code = match entity.strip_prefix("#x") {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => entity.strip_prefix('#')?.parse::<u32>().ok()?,
                };
                char::from_u32(code)?
            }
        });
        rest = tail.get(semi..)?.strip_prefix(';')?;
    }
    out.push_str(rest);
    Some(out)
}

#[cfg(test)]
mod xmp_tests {
    use super::{PDFX4_VERSION, xmp_packet, xmp_pdfx_version};

    /// The former rule: substring search over the raw XMP text.
    fn text_search(t: &str) -> bool {
        t.contains("pdfxid:GTS_PDFXVersion") && t.contains(PDFX4_VERSION)
    }

    fn wrap(ns: &str, body: &str) -> String {
        format!(
            "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\"{ns}>{body}</rdf:Description></rdf:RDF></x:xmpmeta>\n<?xpacket end=\"w\"?>"
        )
    }

    /// F-R31-2: packets on which the former text search answers wrongly, read by namespace URI.
    #[test]
    fn pdfx_version_is_matched_by_namespace_not_by_text() {
        let npes = " xmlns:pdfxid=\"http://www.npes.org/pdfx/ns/id/\"";
        // The writer's own packet, with and without the PDF/A-2b extension schemas.
        for pdfa in [false, true] {
            let own = xmp_packet(Some("t"), Some(("D:20261015000000Z", "D:20261015000000Z", false)), pdfa);
            assert_eq!(xmp_pdfx_version(&own).as_deref(), Some(PDFX4_VERSION), "pdfa2b={pdfa}");
            assert!(text_search(&own));
        }
        let pdf = "xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\"";
        let cases: [(&str, String, Option<&str>); 6] = [
            (
                "decoy in a comment",
                wrap(npes, &format!("<!-- <pdfxid:GTS_PDFXVersion>PDF/X-4</pdfxid:GTS_PDFXVersion> --><pdf:Producer {pdf}>p</pdf:Producer>")),
                None,
            ),
            ("prefix bound to another namespace", wrap(" xmlns:pdfxid=\"http://example.com/decoy/\"", "<pdfxid:GTS_PDFXVersion>PDF/X-4</pdfxid:GTS_PDFXVersion>"), None),
            ("decoy attribute text", wrap(npes, &format!("<pdf:Keywords {pdf} note=\"pdfxid:GTS_PDFXVersion PDF/X-4\">k</pdf:Keywords>")), None),
            (
                "another version, the marker text elsewhere",
                wrap(npes, "<pdfxid:GTS_PDFXVersion>PDF/X-1a:2001</pdfxid:GTS_PDFXVersion><dc:title xmlns:dc=\"http://purl.org/dc/elements/1.1/\">PDF/X-4</dc:title>"),
                None,
            ),
            ("another prefix for the namespace", wrap(" xmlns:id=\"http://www.npes.org/pdfx/ns/id/\"", "<id:GTS_PDFXVersion>PDF/X-4</id:GTS_PDFXVersion>"), Some(PDFX4_VERSION)),
            ("property attribute, escaped", wrap(" xmlns:x4=\"http://www.npes.org/pdfx/ns/id/\" x4:GTS_PDFXVersion=\"PDF&#x2F;X-4\"", ""), Some(PDFX4_VERSION)),
        ];
        for (what, packet, want) in &cases {
            // The rule's reading: the property is present and names PDF/X-4.
            assert_eq!(xmp_pdfx_version(packet).filter(|v| v == PDFX4_VERSION).as_deref(), *want, "{what}");
            assert_ne!(text_search(packet), want.is_some(), "{what}: the text search answers this one wrongly");
        }
        // Malformed or hostile packets are refused, never read leniently.
        let deep = format!("{}{}", "<a>".repeat(100), "</a>".repeat(100));
        for bad in [
            wrap(npes, "<pdfxid:GTS_PDFXVersion>PDF/X-4</pdfxid:Other>"),
            wrap(npes, "<pdfxid:GTS_PDFXVersion>PDF/X-4</pdfxid:GTS_PDFXVersion><pdfxid:GTS_PDFXVersion>PDF/X-4</pdfxid:GTS_PDFXVersion>"),
            format!("<!DOCTYPE x [<!ENTITY v \"PDF/X-4\">]>{}", wrap(npes, "<pdfxid:GTS_PDFXVersion>&v;</pdfxid:GTS_PDFXVersion>")),
            wrap(npes, "<pdfxid:GTS_PDFXVersion>PDF/X-4"),
            wrap(npes, "<pdfxid:GTS_PDFXVersion><rdf:Alt><rdf:li>PDF/X-4</rdf:li></rdf:Alt></pdfxid:GTS_PDFXVersion>"),
            deep,
        ] {
            assert_eq!(xmp_pdfx_version(&bad), None, "{bad}");
        }
    }
}
