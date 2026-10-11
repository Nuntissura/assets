//! Two deterministic write paths:
//!
//! * byte-exact: [`replace_content`] swaps the text of one `Content` element by its recorded byte
//!   span and [`write_idml`] re-emits every part verbatim, so every other byte of every part (and
//!   every unknown part) is identical;
//! * canonical: [`export_idml`] regenerates the designmap, spread and story parts of the supported
//!   subset from the typed model (fixed attribute and element order, fixed ZIP timestamps) and
//!   copies every other part verbatim. Whatever the decoder reported as `LossKind::Unsupported` is
//!   not in the model and therefore not written by this path.
use crate::container;
use crate::idml_decode::{IDPKG_NS, canceled, parse_xml, text_of};
use crate::model::*;
use crate::{IdmlError, Result};
use hsk_studio_accord::CancellationToken;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::ops::Range;

const XML_DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";

fn esc(s: &str, attribute: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            '\r' => out.push_str("&#13;"),
            '\t' | '\n' if attribute => {
                let _ = write!(out, "&#{};", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

fn num(v: f64) -> String {
    format!("{v}")
}
fn nums(v: &[f64]) -> String {
    v.iter().map(|x| num(*x)).collect::<Vec<_>>().join(" ")
}
fn attr_out(out: &mut String, key: &str, value: &str) {
    let _ = write!(out, " {key}=\"{}\"", esc(value, true));
}
fn attrs_out(out: &mut String, attrs: &Attrs) {
    for (k, v) in attrs {
        attr_out(out, k, v);
    }
}
fn link_out(l: &Option<String>) -> &str {
    l.as_deref().unwrap_or("n")
}

fn wrapper(element: &str, dom_version: &str, body: &str) -> String {
    let mut s = String::from(XML_DECL);
    let _ = write!(s, "<idPkg:{element} xmlns:idPkg=\"{IDPKG_NS}\"");
    if !dom_version.is_empty() {
        attr_out(&mut s, "DOMVersion", dom_version);
    }
    s.push_str(">\n");
    s.push_str(body);
    let _ = writeln!(s, "</idPkg:{element}>");
    s
}

fn designmap_xml(m: &IdmlModel) -> String {
    let mut s = String::from(XML_DECL);
    let _ = write!(s, "<Document xmlns:idPkg=\"{IDPKG_NS}\"");
    if !m.dom_version.is_empty() {
        attr_out(&mut s, "DOMVersion", &m.dom_version);
    }
    attrs_out(&mut s, &m.document_attrs);
    let story_list: Vec<&str> = m.stories.iter().map(|st| st.id.as_str()).collect();
    attr_out(&mut s, "StoryList", &story_list.join(" "));
    s.push_str(">\n");
    for item in &m.designmap {
        match item {
            DesignmapItem::Opaque(o) => {
                s.push_str(&o.xml);
                s.push('\n');
            }
            DesignmapItem::Spread(i) => {
                let _ = writeln!(
                    s,
                    "<idPkg:Spread src=\"{}\"/>",
                    esc(&m.spreads[*i].part, true)
                );
            }
            DesignmapItem::Story(i) => {
                let _ = writeln!(
                    s,
                    "<idPkg:Story src=\"{}\"/>",
                    esc(&m.stories[*i].part, true)
                );
            }
        }
    }
    s.push_str("</Document>\n");
    s
}

fn element_out(out: &mut String, tag: &str, attrs: impl FnOnce(&mut String), children: &[Opaque]) {
    let _ = write!(out, "<{tag}");
    attrs(out);
    if children.is_empty() {
        out.push_str("/>\n");
        return;
    }
    out.push_str(">\n");
    for c in children {
        out.push_str(&c.xml);
        out.push('\n');
    }
    let _ = writeln!(out, "</{tag}>");
}

fn spread_xml(m: &IdmlModel, s: &SpreadModel) -> String {
    let mut body = String::new();
    let _ = write!(body, "<Spread");
    attr_out(&mut body, "Self", &s.id);
    attr_out(&mut body, "PageCount", &s.pages.len().to_string());
    attr_out(&mut body, "ItemTransform", &nums(&s.item_transform));
    attrs_out(&mut body, &s.attrs);
    body.push_str(">\n");
    for item in &s.items {
        match item {
            SpreadItem::Page(i) => {
                let p = &s.pages[*i];
                element_out(
                    &mut body,
                    "Page",
                    |o| {
                        attr_out(o, "Self", &p.id);
                        attr_out(o, "Name", &p.name);
                        attr_out(o, "GeometricBounds", &nums(&p.bounds_pt));
                        attr_out(o, "ItemTransform", &nums(&p.item_transform));
                        attrs_out(o, &p.attrs);
                    },
                    &p.children,
                );
            }
            SpreadItem::Frame(i) => {
                let f = &s.frames[*i];
                element_out(
                    &mut body,
                    "TextFrame",
                    |o| {
                        attr_out(o, "Self", &f.id);
                        attr_out(o, "ParentStory", &f.story_id);
                        attr_out(o, "PreviousTextFrame", link_out(&f.previous));
                        attr_out(o, "NextTextFrame", link_out(&f.next));
                        attr_out(o, "ItemTransform", &nums(&f.item_transform));
                        attrs_out(o, &f.attrs);
                    },
                    &f.children,
                );
            }
            SpreadItem::Opaque(o) => {
                body.push_str(&o.xml);
                body.push('\n');
            }
        }
    }
    body.push_str("</Spread>\n");
    wrapper("Spread", &m.dom_version, &body)
}

fn story_xml(m: &IdmlModel, st: &StoryModel) -> String {
    let mut body = String::new();
    body.push_str("<Story");
    attr_out(&mut body, "Self", &st.id);
    attrs_out(&mut body, &st.attrs);
    body.push_str(">\n");
    for h in &st.head {
        body.push_str(&h.xml);
        body.push('\n');
    }
    for para in &st.paragraphs {
        body.push_str("<ParagraphStyleRange");
        if !para.style.is_empty() {
            attr_out(&mut body, "AppliedParagraphStyle", &para.style);
        }
        attrs_out(&mut body, &para.attrs);
        body.push_str(">\n");
        if let Some(p) = &para.props {
            body.push_str(&p.xml);
            body.push('\n');
        }
        for run in &para.runs {
            body.push_str("<CharacterStyleRange");
            if !run.char_style.is_empty() {
                attr_out(&mut body, "AppliedCharacterStyle", &run.char_style);
            }
            attrs_out(&mut body, &run.attrs);
            body.push('>');
            if let Some(p) = &run.props {
                body.push_str(&p.xml);
            }
            if !run.text.is_empty() || !run.br {
                let _ = write!(body, "<Content>{}</Content>", esc(&run.text, false));
            }
            if run.br {
                body.push_str("<Br/>");
            }
            body.push_str("</CharacterStyleRange>\n");
        }
        body.push_str("</ParagraphStyleRange>\n");
    }
    body.push_str("</Story>\n");
    wrapper("Story", &m.dom_version, &body)
}

/// Canonical export of the supported subset (see the module docs). Deterministic: the same model
/// and package always yield the same bytes.
pub fn export_idml(import: &IdmlImport, cancel: &CancellationToken) -> Result<Vec<u8>> {
    export_model(&import.model, &import.package, cancel)
}

pub fn export_model(
    m: &IdmlModel,
    package: &IdmlPackage,
    cancel: &CancellationToken,
) -> Result<Vec<u8>> {
    canceled(cancel)?;
    let mut generated: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    generated.insert("designmap.xml".into(), designmap_xml(m).into_bytes());
    for s in &m.spreads {
        canceled(cancel)?;
        generated.insert(s.part.clone(), spread_xml(m, s).into_bytes());
    }
    for st in &m.stories {
        canceled(cancel)?;
        generated.insert(st.part.clone(), story_xml(m, st).into_bytes());
    }
    let mut order: Vec<(String, Vec<u8>)> = Vec::new();
    for p in &package.parts {
        let bytes = generated.remove(&p.name).unwrap_or_else(|| p.bytes.clone());
        order.push((p.name.clone(), bytes));
    }
    order.extend(generated);
    let refs: Vec<(&str, &[u8])> = order
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    container::write(&refs, true)
}

/// One text replacement against an exact part version.
#[derive(Clone, Debug)]
pub struct ContentEdit<'a> {
    /// Story part name.
    pub part: &'a str,
    /// Identity of the part bytes the span was read from (`StoryModel::part_sha256`).
    pub expected_part_sha256: [u8; 32],
    /// `RunModel::span` of the `Content` element.
    pub span: Range<usize>,
    pub new_text: &'a str,
}

fn xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r') || (c >= ' ' && c != '\u{FFFE}' && c != '\u{FFFF}')
}

/// Replaces the text of one plain `Content` element. The new text is XML-escaped; no other byte of
/// any part changes. Refuses (typed) a stale part, a span that is not a `Content` element, a
/// `Content` holding special-character instructions, and text with characters XML cannot carry.
pub fn replace_content(
    package: &IdmlPackage,
    edit: &ContentEdit,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<IdmlPackage> {
    canceled(cancel)?;
    let part = package
        .part(edit.part)
        .ok_or(IdmlError::InvalidEdit("no_such_part"))?;
    if part.sha256 != edit.expected_part_sha256 {
        return Err(IdmlError::StaleSource);
    }
    if !edit.new_text.chars().all(xml_char) {
        return Err(IdmlError::InvalidEdit("illegal_xml_character"));
    }
    let text = text_of(part)?;
    let doc = parse_xml(text, limits)?;
    let node = doc
        .descendants()
        .find(|n| n.is_element() && n.range() == edit.span)
        .ok_or(IdmlError::InvalidEdit("span_is_not_an_element"))?;
    if node.tag_name().name() != "Content" {
        return Err(IdmlError::InvalidEdit("not_content"));
    }
    if node.children().any(|c| !c.is_text()) {
        return Err(IdmlError::InvalidEdit("content_has_non_text"));
    }
    let mut out = String::with_capacity(text.len() + edit.new_text.len());
    out.push_str(&text[..edit.span.start]);
    let _ = write!(out, "<Content>{}</Content>", esc(edit.new_text, false));
    out.push_str(&text[edit.span.end..]);
    let mut next = package.clone();
    if let Some(slot) = next.parts.iter_mut().find(|p| p.name == edit.part) {
        *slot = IdmlPart::new(slot.name.clone(), out.into_bytes());
    }
    Ok(next)
}

/// Re-emits every part verbatim in package order as a deterministic IDML container.
pub fn write_idml(package: &IdmlPackage) -> Result<Vec<u8>> {
    if package.parts.first().map(|p| p.name.as_str()) != Some("mimetype") {
        return Err(IdmlError::Mimetype("not_first"));
    }
    let refs: Vec<(&str, &[u8])> = package
        .parts
        .iter()
        .map(|p| (p.name.as_str(), p.bytes.as_slice()))
        .collect();
    container::write(&refs, true)
}
