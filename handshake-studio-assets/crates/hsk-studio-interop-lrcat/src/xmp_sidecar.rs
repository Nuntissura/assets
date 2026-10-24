#![deny(clippy::arithmetic_side_effects)]
//! XMP sidecar codec for develop settings and library metadata (lane SMB-LR-LIBRARY-WRITE).
//! Bytes in, bytes out: this module opens no path and writes no file; the caller reads and writes
//! sidecar bytes through the CKC port (`read_xmp_sidecar` / `write_xmp_sidecar_bytes`, 14.12
//! "bounded explicit XMP interchange read/write through CKC is permitted as external transport,
//! never live settings authority"). The develop record stays the authority (STU-RAW, 14.12): a
//! sidecar is an interchange copy.
//!
//! Edit model: the packet is tokenized with byte spans (XML 1.0 subset XMP uses; names resolved by
//! namespace URI, references decoded, comments/PI/CDATA recognised, DOCTYPE refused). A write
//! changes ONLY the spans of managed properties whose value actually changes (attribute value,
//! whole property element, or an insertion before a `rdf:Description` tag end); every other byte
//! of the input, including unknown namespaces, complex `crs:` structures (tone-curve sequences,
//! mask groups), comments, whitespace and quoting, is copied verbatim. A write whose values all
//! equal the packet's returns the input bytes unchanged. Equality is decided on the formatted
//! value, so `+0.5` in the file and 0.5 in the recipe are equal.
//!
//! Managed `crs:` keys are listed in [`CRS_KEYS`] / [`CRS_FLAGS`]: Camera Raw names as Lightroom
//! Classic writes them per community references, UNVERIFIED (D5), mapped 1:1 onto develop's own
//! sliders (develop scale mapping UNVERIFIED as in `develop_map`). Absolute white balance is
//! reported (`WhiteBalance`/`Temperature`/`Tint`), not applied: develop needs the raw to resolve
//! it (`recipe_for_white`). Library fields: `xmp:Rating` (-1 rejected), `xmp:Label`, `dc:subject`,
//! `lr:hierarchicalSubject` (`|` separated).
//!
//! Retouch areas (lane SMB-LR-HEALING-1, STU-RAW-103 decode of STU-RAW-124): `crs:RetouchAreas` (an
//! `rdf:Seq` of structures: `rdf:li` property attributes, `rdf:parseType="Resource"` children or one
//! `rdf:Description`) decodes into develop `RetouchArea` records. Field names are the CamelCase of
//! the spec's record fields (`SpotType`, `HealVersion`, `Feather`, `Opacity`, `Seed`, `Method`,
//! `SourceState`, `SourceX`, `SourceY`, `OffsetY`, `CenterX`/`CentreX`, `CenterY`/`CentreY`, `Radius`,
//! `Masks` = `rdf:Seq` of `Radius`, `Flow`, `Feather`, `Dabs` = `rdf:Seq` of `"d x y"`), UNVERIFIED
//! against Lightroom Classic (D5); `SourceState` maps only the observed `sourceSetExplicitly`
//! (or `source_set_explicitly`) and `engine_chosen`. Unknown fields are loss notes (`<key>.<field>`,
//! `crs_retouch_field_unknown`) and the area keeps decoding; an area missing a required field, with
//! an unmapped value or out of develop's ranges is dropped with a loss note. The list is never
//! written back by this codec (the packet bytes stay verbatim on write).
use crate::develop_map::WhiteBalanceSource;
use crate::xmp::{NS_DC, NS_LR, NS_RDF, NS_XMP, XmpError, XmpLimits, XmpRating};
use hsk_studio_develop::retouch::{MAX_AREA_MASKS, MAX_RETOUCH_AREAS, RetouchArea, RetouchMethod, RetouchStack, SourceState, SpotType};
use hsk_studio_develop::{AbsoluteWhiteBalance, Brush, DevelopRecipe, NormalizedCrop, VignetteStyle, parse_dab, white_xy};
use hsk_studio_kernel_bindings::{AssetId, CkcActor, CkcCatalogPort, CkcError, CkcReceipt, SidecarPlacement, XmpSidecarBytesWrite};
use std::collections::BTreeMap;

pub const NS_CRS: &str = "http://ns.adobe.com/camera-raw-settings/1.0/";
const NS_XMLNS_PREFIX: &str = "xmlns";

// ------------------------------------------------------------------------------------------------
// Tokenizer with spans

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    start: usize,
    end: usize,
}

#[derive(Clone, Debug)]
struct Attr {
    qname: String,
    /// From the whitespace before the name to the closing quote (removal span).
    full: Span,
    /// Inside the quotes (raw, undecoded).
    value: Span,
}

#[derive(Clone, Debug)]
struct Elem {
    qname: String,
    ns: String,
    local: String,
    /// Start tag `<` .. after `>`.
    start: Span,
    self_closing: bool,
    /// End tag `</` .. after `>`.
    end: Option<Span>,
    parent: Option<usize>,
    children: Vec<usize>,
    /// Raw text spans (CDATA content included, flagged).
    text: Vec<(Span, bool)>,
    attrs: Vec<Attr>,
    decls: Vec<(String, String)>,
}

impl Elem {
    fn full(&self) -> Span {
        Span { start: self.start.start, end: self.end.map_or(self.start.end, |e| e.end) }
    }
    fn prefix(&self) -> Option<&str> {
        self.qname.split_once(':').map(|(p, _)| p)
    }
}

fn find(b: &[u8], from: usize, pat: &[u8]) -> Option<usize> {
    let hay = b.get(from..)?;
    hay.windows(pat.len()).position(|w| w == pat).map(|p| p.saturating_add(from))
}

fn name_ok(n: &str) -> bool {
    !n.is_empty() && !n.bytes().any(|c| c.is_ascii_whitespace() || b"<>/=\"'&".contains(&c))
}

fn syntax(code: &'static str) -> XmpError {
    XmpError::Syntax(code)
}

/// Decodes the five predefined entities and character references.
fn decode(raw: &str, max: usize) -> Result<String, XmpError> {
    let mut out = String::with_capacity(raw.len().min(max));
    let mut rest = raw;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        let semi = after.find(';').ok_or(syntax("xmp_bad_reference"))?;
        let ent = &after[1..semi];
        let ch = match ent {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let code = if let Some(h) = ent.strip_prefix("#x") {
                    u32::from_str_radix(h, 16).ok()
                } else if let Some(d) = ent.strip_prefix('#') {
                    d.parse::<u32>().ok()
                } else {
                    None
                };
                code.and_then(char::from_u32).ok_or(syntax("xmp_bad_reference"))?
            }
        };
        out.push(ch);
        if out.len() > max {
            return Err(XmpError::TextTooLong);
        }
        rest = &after[semi.saturating_add(1)..];
    }
    out.push_str(rest);
    if out.len() > max {
        return Err(XmpError::TextTooLong);
    }
    Ok(out)
}

fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '&' => o.push_str("&amp;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            c => o.push(c),
        }
    }
    o
}

/// Parsed packet: the original text plus spans.
#[derive(Clone, Debug)]
pub struct XmpSidecar {
    text: String,
    elems: Vec<Elem>,
    /// (ns, local) -> property
    props: BTreeMap<(String, String), Prop>,
    descs: Vec<usize>,
    limits: XmpLimits,
}

#[derive(Clone, Debug)]
enum Prop {
    Attr { elem: usize, attr: usize },
    Elem { elem: usize },
}

impl XmpSidecar {
    pub fn parse(bytes: &[u8], limits: &XmpLimits) -> Result<Self, XmpError> {
        limits.validate()?;
        if bytes.len() > limits.max_input_bytes as usize {
            return Err(XmpError::InputTooLarge);
        }
        let text = std::str::from_utf8(bytes).map_err(|_| XmpError::NotUtf8)?.to_owned();
        let b = text.as_bytes();
        let n = b.len();
        let mut elems: Vec<Elem> = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        let mut nodes = 0u32;
        let mut i = 0usize;
        let mut roots = 0u32;
        let bump = |nodes: &mut u32| -> Result<(), XmpError> {
            *nodes = nodes.saturating_add(1);
            if *nodes > limits.max_nodes { Err(XmpError::NodesExceeded) } else { Ok(()) }
        };
        while i < n {
            if b[i] != b'<' {
                let next = find(b, i, b"<").unwrap_or(n);
                let span = Span { start: i, end: next };
                if next.saturating_sub(i) > limits.max_text_bytes as usize {
                    return Err(XmpError::TextTooLong);
                }
                decode(&text[i..next], limits.max_text_bytes as usize)?;
                match stack.last() {
                    Some(&top) => elems[top].text.push((span, false)),
                    None if !text[i..next].trim().is_empty() => return Err(syntax("xmp_text_outside_root")),
                    None => {}
                }
                i = next;
                continue;
            }
            let rest = &b[i..];
            if rest.starts_with(b"<?") {
                i = find(b, i, b"?>").ok_or(syntax("xmp_unterminated_pi"))?.saturating_add(2);
                bump(&mut nodes)?;
            } else if rest.starts_with(b"<!--") {
                i = find(b, i.saturating_add(4), b"-->").ok_or(syntax("xmp_unterminated_comment"))?.saturating_add(3);
                bump(&mut nodes)?;
            } else if rest.starts_with(b"<![CDATA[") {
                let s = i.saturating_add(9);
                let e = find(b, s, b"]]>").ok_or(syntax("xmp_unterminated_cdata"))?;
                let top = *stack.last().ok_or(syntax("xmp_text_outside_root"))?;
                elems[top].text.push((Span { start: s, end: e }, true));
                i = e.saturating_add(3);
            } else if rest.starts_with(b"<!") {
                return Err(XmpError::DoctypeRefused);
            } else if rest.starts_with(b"</") {
                let e = find(b, i, b">").ok_or(syntax("xmp_unterminated_tag"))?;
                let name = text[i.saturating_add(2)..e].trim();
                let top = stack.pop().ok_or(syntax("xmp_unbalanced_end_tag"))?;
                if elems[top].qname != name {
                    return Err(syntax("xmp_tag_mismatch"));
                }
                elems[top].end = Some(Span { start: i, end: e.saturating_add(1) });
                i = e.saturating_add(1);
            } else {
                // Start tag.
                let mut j = i.saturating_add(1);
                while j < n && !b[j].is_ascii_whitespace() && b[j] != b'>' && b[j] != b'/' {
                    j = j.saturating_add(1);
                }
                let qname = text[i.saturating_add(1)..j].to_owned();
                if !name_ok(&qname) {
                    return Err(syntax("xmp_bad_name"));
                }
                let mut attrs = Vec::new();
                let self_closing;
                loop {
                    let ws = j;
                    while j < n && b[j].is_ascii_whitespace() {
                        j = j.saturating_add(1);
                    }
                    if j >= n {
                        return Err(syntax("xmp_unterminated_tag"));
                    }
                    if b[j] == b'>' {
                        self_closing = false;
                        j = j.saturating_add(1);
                        break;
                    }
                    if b[j] == b'/' {
                        if b.get(j.saturating_add(1)) != Some(&b'>') {
                            return Err(syntax("xmp_bad_tag"));
                        }
                        self_closing = true;
                        j = j.saturating_add(2);
                        break;
                    }
                    if ws == j {
                        return Err(syntax("xmp_attribute_spacing"));
                    }
                    let ns = j;
                    while j < n && !b[j].is_ascii_whitespace() && b[j] != b'=' && b[j] != b'>' {
                        j = j.saturating_add(1);
                    }
                    let aname = text[ns..j].to_owned();
                    if !name_ok(&aname) {
                        return Err(syntax("xmp_bad_name"));
                    }
                    while j < n && b[j].is_ascii_whitespace() {
                        j = j.saturating_add(1);
                    }
                    if b.get(j) != Some(&b'=') {
                        return Err(syntax("xmp_attribute_value"));
                    }
                    j = j.saturating_add(1);
                    while j < n && b[j].is_ascii_whitespace() {
                        j = j.saturating_add(1);
                    }
                    let q = *b.get(j).ok_or(syntax("xmp_unterminated_tag"))?;
                    if q != b'"' && q != b'\'' {
                        return Err(syntax("xmp_attribute_value"));
                    }
                    let vs = j.saturating_add(1);
                    let ve = find(b, vs, &[q]).ok_or(syntax("xmp_unterminated_tag"))?;
                    if b[vs..ve].contains(&b'<') {
                        return Err(syntax("xmp_attribute_value"));
                    }
                    if ve.saturating_sub(vs) > limits.max_text_bytes as usize {
                        return Err(XmpError::TextTooLong);
                    }
                    j = ve.saturating_add(1);
                    if attrs.iter().any(|a: &Attr| a.qname == aname) {
                        return Err(syntax("xmp_attribute_repeated"));
                    }
                    attrs.push(Attr { qname: aname, full: Span { start: ws, end: j }, value: Span { start: vs, end: ve } });
                    bump(&mut nodes)?;
                }
                bump(&mut nodes)?;
                for a in &attrs {
                    decode(&text[a.value.start..a.value.end], limits.max_text_bytes as usize)?;
                }
                if stack.len() >= limits.max_depth as usize {
                    return Err(XmpError::DepthExceeded);
                }
                let mut decls = Vec::new();
                for a in &attrs {
                    if a.qname == NS_XMLNS_PREFIX {
                        decls.push((String::new(), decode(&text[a.value.start..a.value.end], limits.max_text_bytes as usize)?));
                    } else if let Some(p) = a.qname.strip_prefix("xmlns:") {
                        decls.push((p.to_owned(), decode(&text[a.value.start..a.value.end], limits.max_text_bytes as usize)?));
                    }
                }
                let parent = stack.last().copied();
                if parent.is_none() {
                    roots = roots.saturating_add(1);
                    if roots > 1 {
                        return Err(syntax("xmp_multiple_roots"));
                    }
                }
                let idx = elems.len();
                elems.push(Elem {
                    qname,
                    ns: String::new(),
                    local: String::new(),
                    start: Span { start: i, end: j },
                    self_closing,
                    end: None,
                    parent,
                    children: Vec::new(),
                    text: Vec::new(),
                    attrs,
                    decls,
                });
                if let Some(p) = parent {
                    elems[p].children.push(idx);
                }
                if !self_closing {
                    stack.push(idx);
                }
                i = j;
            }
        }
        if !stack.is_empty() {
            return Err(syntax("xmp_unclosed_element"));
        }
        if roots == 0 {
            return Err(syntax("xmp_no_root"));
        }
        let mut doc = Self { text, elems, props: BTreeMap::new(), descs: Vec::new(), limits: *limits };
        doc.resolve()?;
        Ok(doc)
    }

    fn lookup(&self, elem: usize, prefix: &str) -> Option<String> {
        if prefix == "xml" {
            return Some("http://www.w3.org/XML/1998/namespace".into());
        }
        let mut cur = Some(elem);
        while let Some(e) = cur {
            if let Some((_, uri)) = self.elems[e].decls.iter().find(|(p, _)| p == prefix) {
                return Some(uri.clone());
            }
            cur = self.elems[e].parent;
        }
        None
    }

    fn resolve_name(&self, elem: usize, qname: &str, is_attr: bool) -> Result<(String, String), XmpError> {
        match qname.split_once(':') {
            Some((p, l)) => Ok((self.lookup(elem, p).ok_or(syntax("xmp_unbound_prefix"))?, l.to_owned())),
            None if is_attr => Ok((String::new(), qname.to_owned())),
            None => Ok((self.lookup(elem, "").unwrap_or_default(), qname.to_owned())),
        }
    }

    fn resolve(&mut self) -> Result<(), XmpError> {
        for e in 0..self.elems.len() {
            let (ns, local) = self.resolve_name(e, &self.elems[e].qname.clone(), false)?;
            self.elems[e].ns = ns;
            self.elems[e].local = local;
        }
        for e in 0..self.elems.len() {
            if !(self.elems[e].ns == NS_RDF && self.elems[e].local == "Description") {
                continue;
            }
            // A description inside a structure item (`rdf:li` / another description, e.g. one
            // `crs:RetouchAreas` entry) holds that item's fields, not top-level properties.
            let mut up = self.elems[e].parent;
            let mut nested = false;
            while let Some(p) = up {
                let pe = &self.elems[p];
                if pe.ns == NS_RDF && (pe.local == "li" || pe.local == "Description") {
                    nested = true;
                    break;
                }
                up = pe.parent;
            }
            if nested {
                continue;
            }
            self.descs.push(e);
            for (ai, a) in self.elems[e].attrs.clone().iter().enumerate() {
                if a.qname == NS_XMLNS_PREFIX || a.qname.starts_with("xmlns:") {
                    continue;
                }
                let (ns, local) = self.resolve_name(e, &a.qname, true)?;
                if ns.is_empty() || ns == NS_RDF {
                    continue;
                }
                if self.props.insert((ns, local), Prop::Attr { elem: e, attr: ai }).is_some() {
                    return Err(syntax("xmp_property_repeated"));
                }
            }
            for &c in &self.elems[e].children.clone() {
                let key = (self.elems[c].ns.clone(), self.elems[c].local.clone());
                if self.props.insert(key, Prop::Elem { elem: c }).is_some() {
                    return Err(syntax("xmp_property_repeated"));
                }
            }
        }
        if self.descs.is_empty() {
            return Err(syntax("xmp_no_description"));
        }
        Ok(())
    }

    pub fn bytes(&self) -> &[u8] {
        self.text.as_bytes()
    }

    /// Number of properties (attribute or element form) across all descriptions.
    pub fn property_count(&self) -> usize {
        self.props.len()
    }

    fn raw(&self, s: Span) -> &str {
        &self.text[s.start..s.end]
    }

    /// Simple text value of a property; `Err` when it exists but is not a simple value.
    fn scalar(&self, ns: &str, local: &str) -> Option<Result<String, XmpError>> {
        let max = self.limits.max_text_bytes as usize;
        Some(match self.props.get(&(ns.to_owned(), local.to_owned()))? {
            Prop::Attr { elem, attr } => decode(self.raw(self.elems[*elem].attrs[*attr].value), max),
            Prop::Elem { elem } => {
                let e = &self.elems[*elem];
                if !e.children.is_empty() || e.attrs.iter().any(|a| !a.qname.starts_with("xmlns")) {
                    Err(syntax("xmp_property_shape"))
                } else {
                    let mut s = String::new();
                    for (span, cdata) in &e.text {
                        if *cdata {
                            s.push_str(self.raw(*span));
                        } else {
                            match decode(self.raw(*span), max) {
                                Ok(t) => s.push_str(&t),
                                Err(e) => return Some(Err(e)),
                            }
                        }
                        if s.len() > max {
                            return Some(Err(XmpError::TextTooLong));
                        }
                    }
                    Ok(s)
                }
            }
        })
    }

    /// Items of an `rdf:Bag`/`Seq`/`Alt` property; `Err` for another shape.
    fn list(&self, ns: &str, local: &str) -> Option<Result<Vec<String>, XmpError>> {
        let Prop::Elem { elem } = self.props.get(&(ns.to_owned(), local.to_owned()))? else {
            return Some(Err(syntax("xmp_property_shape")));
        };
        let e = &self.elems[*elem];
        let containers: Vec<usize> = e.children.clone();
        let [c] = containers.as_slice() else { return Some(Err(syntax("xmp_property_shape"))) };
        let c = &self.elems[*c];
        if c.ns != NS_RDF || !matches!(c.local.as_str(), "Bag" | "Seq" | "Alt") {
            return Some(Err(syntax("xmp_property_shape")));
        }
        if c.children.len() > self.limits.max_keywords as usize {
            return Some(Err(XmpError::KeywordsExceeded));
        }
        let mut out = Vec::with_capacity(c.children.len());
        for &li in &c.children {
            let li = &self.elems[li];
            if li.ns != NS_RDF || li.local != "li" || !li.children.is_empty() {
                return Some(Err(syntax("xmp_property_shape")));
            }
            let mut s = String::new();
            for (span, cdata) in &li.text {
                if *cdata {
                    s.push_str(self.raw(*span));
                } else {
                    match decode(self.raw(*span), self.limits.max_text_bytes as usize) {
                        Ok(t) => s.push_str(&t),
                        Err(e) => return Some(Err(e)),
                    }
                }
            }
            out.push(s);
        }
        Some(Ok(out))
    }

    /// Decoded text of a simple element (no child elements).
    fn elem_text(&self, e: usize) -> Result<String, XmpError> {
        let max = self.limits.max_text_bytes as usize;
        let mut s = String::new();
        for (span, cdata) in &self.elems[e].text {
            if *cdata {
                s.push_str(self.raw(*span));
            } else {
                s.push_str(&decode(self.raw(*span), max)?);
            }
            if s.len() > max {
                return Err(XmpError::TextTooLong);
            }
        }
        Ok(s)
    }

    /// The `rdf:li` items of the single `rdf:Seq`/`rdf:Bag` child of element `e`.
    fn seq_items(&self, e: usize) -> Result<&[usize], &'static str> {
        let [c] = self.elems[e].children.as_slice() else { return Err("crs_retouch_shape") };
        let c = &self.elems[*c];
        if c.ns != NS_RDF || !matches!(c.local.as_str(), "Seq" | "Bag") {
            return Err("crs_retouch_shape");
        }
        if c.children.iter().any(|&li| self.elems[li].ns != NS_RDF || self.elems[li].local != "li") {
            return Err("crs_retouch_shape");
        }
        Ok(&c.children)
    }

    /// Fields of one structure item: `crs:` property attributes and child elements of the `rdf:li`
    /// and of its single `rdf:Description` child, as (local name, simple text) and (local name,
    /// complex element); fields of other namespaces are counted in the third value.
    fn item_fields(&self, li: usize) -> Result<StructFields, &'static str> {
        let mut holders = vec![li];
        if let [d] = self.elems[li].children.as_slice()
            && self.elems[*d].ns == NS_RDF
            && self.elems[*d].local == "Description"
        {
            holders.push(*d);
        }
        let max = self.limits.max_text_bytes as usize;
        let mut fields = StructFields::default();
        for h in holders {
            for a in &self.elems[h].attrs {
                if a.qname == NS_XMLNS_PREFIX || a.qname.starts_with("xmlns:") {
                    continue;
                }
                let (ns, local) = self.resolve_name(h, &a.qname, true).map_err(|_| "crs_retouch_shape")?;
                if ns == NS_RDF {
                    continue;
                }
                if ns != NS_CRS {
                    fields.foreign = fields.foreign.saturating_add(1);
                    continue;
                }
                fields.simple.push((local, decode(self.raw(a.value), max).map_err(|_| "crs_retouch_shape")?));
            }
            for &c in &self.elems[h].children {
                let ce = &self.elems[c];
                if ce.ns == NS_RDF && ce.local == "Description" {
                    continue;
                }
                if ce.ns != NS_CRS {
                    fields.foreign = fields.foreign.saturating_add(1);
                } else if ce.children.is_empty() {
                    fields.simple.push((ce.local.clone(), self.elem_text(c).map_err(|_| "crs_retouch_shape")?));
                } else {
                    fields.complex.push((ce.local.clone(), c));
                }
            }
        }
        Ok(fields)
    }
}

/// Fields of one XMP structure item (see `XmpSidecar::item_fields`).
#[derive(Default)]
struct StructFields {
    simple: Vec<(String, String)>,
    complex: Vec<(String, usize)>,
    foreign: u32,
}

fn retouch_number(text: &str) -> Result<f64, &'static str> {
    text.trim().parse::<f64>().ok().filter(|v| v.is_finite()).ok_or("crs_value_not_number")
}

/// One `crs:Masks` item: a brush mask in the STU-RAW-121 geometry.
fn read_retouch_mask(doc: &XmpSidecar, li: usize, key: &str, losses: &mut Vec<(String, &'static str)>) -> Result<Brush, &'static str> {
    let fields = doc.item_fields(li)?;
    let mut brush = Brush { dabs: Vec::new(), radius: f64::NAN, flow: 1.0, feather: 0.0 };
    for (name, value) in &fields.simple {
        match name.as_str() {
            "Radius" => brush.radius = retouch_number(value)?,
            "Flow" => brush.flow = retouch_number(value)?,
            "Feather" => brush.feather = retouch_number(value)?,
            _ => losses.push((format!("{key}.{name}"), "crs_retouch_field_unknown")),
        }
    }
    for (name, e) in &fields.complex {
        if name != "Dabs" {
            losses.push((format!("{key}.{name}"), "crs_retouch_field_unknown"));
            continue;
        }
        let items = doc.seq_items(*e)?;
        if items.len() > hsk_studio_develop::local::MAX_DABS {
            return Err("crs_retouch_too_many");
        }
        let mut dabs = Vec::with_capacity(items.len());
        for &d in items {
            let text = doc.elem_text(d).map_err(|_| "crs_retouch_shape")?;
            dabs.push(parse_dab(&text).map_err(|_| "crs_value_not_number")?);
        }
        brush.dabs = dabs;
    }
    if brush.radius.is_nan() {
        return Err("crs_retouch_field_missing");
    }
    Ok(brush)
}

/// One `crs:RetouchAreas` item (see the module documentation).
fn read_retouch_area(doc: &XmpSidecar, li: usize, key: &str, losses: &mut Vec<(String, &'static str)>) -> Result<RetouchArea, &'static str> {
    let fields = doc.item_fields(li)?;
    let get = |names: &[&str]| fields.simple.iter().find(|(n, _)| names.contains(&n.as_str())).map(|(_, v)| v.trim());
    let need = |names: &[&str]| get(names).ok_or("crs_retouch_field_missing");
    let number = |names: &[&str], default: f64| get(names).map_or(Ok(default), retouch_number);
    let spot_type = SpotType::from_key(need(&["SpotType"])?).ok_or("crs_retouch_value_unmapped")?;
    let source_state = match need(&["SourceState"])? {
        "sourceSetExplicitly" | "source_set_explicitly" => SourceState::SourceSetExplicitly,
        "engine_chosen" => SourceState::EngineChosen,
        _ => return Err("crs_retouch_value_unmapped"),
    };
    let method = match get(&["Method"]) {
        None => RetouchMethod::Gaussian,
        Some(m) => RetouchMethod::from_key(m).ok_or("crs_retouch_value_unmapped")?,
    };
    let heal_version = get(&["HealVersion"]).map_or(Ok(0), |v| v.parse::<u32>().map_err(|_| "crs_value_not_number"))?;
    let seed = get(&["Seed"]).map_or(Ok(0), |v| v.parse::<u64>().map_err(|_| "crs_value_not_number"))?;
    const KNOWN: [&str; 15] = [
        "SpotType", "HealVersion", "Feather", "Opacity", "Seed", "Method", "SourceState", "SourceX", "SourceY", "OffsetY", "CenterX",
        "CentreX", "CenterY", "CentreY", "Radius",
    ];
    for (name, _) in &fields.simple {
        if !KNOWN.contains(&name.as_str()) {
            losses.push((format!("{key}.{name}"), "crs_retouch_field_unknown"));
        }
    }
    let mut masks = Vec::new();
    for (name, e) in &fields.complex {
        if name != "Masks" {
            losses.push((format!("{key}.{name}"), "crs_retouch_field_unknown"));
            continue;
        }
        let items = doc.seq_items(*e)?;
        if items.len() > MAX_AREA_MASKS {
            return Err("crs_retouch_too_many");
        }
        for (j, &m) in items.iter().enumerate() {
            masks.push(read_retouch_mask(doc, m, &format!("{key}.Masks[{j}]"), losses)?);
        }
    }
    let area = RetouchArea {
        spot_type,
        heal_version,
        feather: number(&["Feather"], 0.0)?,
        opacity: number(&["Opacity"], 1.0)?,
        seed,
        method,
        source_state,
        masks,
        source_x: retouch_number(need(&["SourceX"])?)?,
        source_y: retouch_number(need(&["SourceY"])?)?,
        offset_y: number(&["OffsetY"], 0.0)?,
        centre_x: retouch_number(need(&["CenterX", "CentreX"])?)?,
        centre_y: retouch_number(need(&["CenterY", "CentreY"])?)?,
        radius: number(&["Radius"], 0.0)?,
    };
    let one = RetouchStack { areas: vec![area], ..RetouchStack::default() };
    one.validate().map_err(|_| "crs_retouch_value_out_of_range")?;
    one.areas.into_iter().next().ok_or("crs_retouch_shape")
}

/// `crs:RetouchAreas` -> develop records; per-area failures are loss notes, a list that is not a
/// sequence, over [`MAX_RETOUCH_AREAS`] or over develop's aggregate dab budget is one loss.
fn read_retouch_areas(doc: &XmpSidecar, prop: usize, losses: &mut Vec<(String, &'static str)>) -> Result<Vec<RetouchArea>, &'static str> {
    let items = doc.seq_items(prop)?;
    if items.len() > MAX_RETOUCH_AREAS {
        return Err("crs_retouch_too_many");
    }
    let mut areas = Vec::with_capacity(items.len());
    for (i, &li) in items.iter().enumerate() {
        let key = format!("RetouchAreas[{i}]");
        match read_retouch_area(doc, li, &key, losses) {
            Ok(area) => areas.push(area),
            Err(code) => losses.push((key, code)),
        }
    }
    let stack = RetouchStack { areas, ..RetouchStack::default() };
    stack.validate().map_err(|_| "crs_retouch_too_many")?;
    Ok(stack.areas)
}

// ------------------------------------------------------------------------------------------------
// crs: mapping table

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fmt {
    /// `+12`, `0`, `-5`
    Signed0,
    /// `+1.0`
    Signed1,
    /// `+0.50`
    Signed2,
    /// `25`
    Plain0,
    /// `0.123456`
    Plain6,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gate {
    None,
    Detail,
    Colour,
    Grayscale,
    ToneCurve,
    Lens,
    Transform,
    Effects,
    Calibration,
    Crop,
}

/// One managed numeric `crs:` key.
pub struct CrsKey {
    pub key: &'static str,
    get: fn(&DevelopRecipe) -> f64,
    set: fn(&mut DevelopRecipe, f64),
    lo: f64,
    hi: f64,
    fmt: Fmt,
    gate: Gate,
}

/// One managed boolean `crs:` key (`True`/`False`).
pub struct CrsFlag {
    pub key: &'static str,
    get: fn(&DevelopRecipe) -> bool,
    set: fn(&mut DevelopRecipe, bool),
}

macro_rules! num {
    ($key:literal, [$($p:tt)+], $lo:expr, $hi:expr, $fmt:ident, $gate:ident) => {
        CrsKey { key: $key, get: |r| r.$($p)+, set: |r, v| r.$($p)+ = v, lo: $lo, hi: $hi, fmt: Fmt::$fmt, gate: Gate::$gate }
    };
}
macro_rules! flag {
    ($key:literal, [$($p:tt)+]) => {
        CrsFlag { key: $key, get: |r| r.$($p)+, set: |r, v| r.$($p)+ = v }
    };
}
macro_rules! band {
    ($h:literal, $s:literal, $l:literal, $g:literal, $i:literal) => {
        [
            num!($h, [adjust.mixer.bands[$i].hue], -100.0, 100.0, Signed0, Colour),
            num!($s, [adjust.mixer.bands[$i].saturation], -100.0, 100.0, Signed0, Colour),
            num!($l, [adjust.mixer.bands[$i].luminance], -100.0, 100.0, Signed0, Colour),
            num!($g, [adjust.grayscale.bands[$i]], -100.0, 100.0, Signed0, Grayscale),
        ]
    };
}

const BANDS: [[CrsKey; 4]; 8] = [
    band!("HueAdjustmentRed", "SaturationAdjustmentRed", "LuminanceAdjustmentRed", "GrayMixerRed", 0),
    band!("HueAdjustmentOrange", "SaturationAdjustmentOrange", "LuminanceAdjustmentOrange", "GrayMixerOrange", 1),
    band!("HueAdjustmentYellow", "SaturationAdjustmentYellow", "LuminanceAdjustmentYellow", "GrayMixerYellow", 2),
    band!("HueAdjustmentGreen", "SaturationAdjustmentGreen", "LuminanceAdjustmentGreen", "GrayMixerGreen", 3),
    band!("HueAdjustmentAqua", "SaturationAdjustmentAqua", "LuminanceAdjustmentAqua", "GrayMixerAqua", 4),
    band!("HueAdjustmentBlue", "SaturationAdjustmentBlue", "LuminanceAdjustmentBlue", "GrayMixerBlue", 5),
    band!("HueAdjustmentPurple", "SaturationAdjustmentPurple", "LuminanceAdjustmentPurple", "GrayMixerPurple", 6),
    band!("HueAdjustmentMagenta", "SaturationAdjustmentMagenta", "LuminanceAdjustmentMagenta", "GrayMixerMagenta", 7),
];

/// Managed numeric keys (besides the 32 colour-mixer / gray-mixer band keys in the same order as
/// develop's `BAND_NAMES`).
pub const CRS_KEYS: &[CrsKey] = &[
    num!("Exposure2012", [exposure_stops], -5.0, 5.0, Signed2, None),
    num!("Contrast2012", [adjust.tone.contrast], -100.0, 100.0, Signed0, None),
    num!("Highlights2012", [adjust.tone.highlights], -100.0, 100.0, Signed0, None),
    num!("Shadows2012", [adjust.tone.shadows], -100.0, 100.0, Signed0, None),
    num!("Whites2012", [adjust.tone.whites], -100.0, 100.0, Signed0, None),
    num!("Blacks2012", [adjust.tone.blacks], -100.0, 100.0, Signed0, None),
    num!("IncrementalTemperature", [adjust.white_balance.temperature], -100.0, 100.0, Signed0, None),
    num!("IncrementalTint", [adjust.white_balance.tint], -100.0, 100.0, Signed0, None),
    num!("Vibrance", [adjust.presence.vibrance], -100.0, 100.0, Signed0, None),
    num!("Saturation", [adjust.presence.saturation], -100.0, 100.0, Signed0, None),
    num!("Clarity2012", [adjust.presence.clarity], -100.0, 100.0, Signed0, Detail),
    num!("Texture", [adjust.detail.texture], -100.0, 100.0, Signed0, Detail),
    num!("Dehaze", [adjust.detail.dehaze], -100.0, 100.0, Signed0, Detail),
    num!("Sharpness", [adjust.detail.sharpen.amount], 0.0, 150.0, Plain0, Detail),
    num!("SharpenRadius", [adjust.detail.sharpen.radius], 0.5, 3.0, Signed1, Detail),
    num!("SharpenDetail", [adjust.detail.sharpen.detail], 0.0, 100.0, Plain0, Detail),
    num!("SharpenEdgeMasking", [adjust.detail.sharpen.masking], 0.0, 100.0, Plain0, Detail),
    num!("LuminanceSmoothing", [adjust.detail.noise.luminance], 0.0, 100.0, Plain0, Detail),
    num!("ColorNoiseReduction", [adjust.detail.noise.colour], 0.0, 100.0, Plain0, Detail),
    num!("ParametricShadows", [adjust.parametric.shadows], -100.0, 100.0, Signed0, ToneCurve),
    num!("ParametricDarks", [adjust.parametric.darks], -100.0, 100.0, Signed0, ToneCurve),
    num!("ParametricLights", [adjust.parametric.lights], -100.0, 100.0, Signed0, ToneCurve),
    num!("ParametricHighlights", [adjust.parametric.highlights], -100.0, 100.0, Signed0, ToneCurve),
    num!("ParametricShadowSplit", [adjust.parametric.shadow_split], 0.0, 100.0, Plain0, ToneCurve),
    num!("ParametricMidtoneSplit", [adjust.parametric.midtone_split], 0.0, 100.0, Plain0, ToneCurve),
    num!("ParametricHighlightSplit", [adjust.parametric.highlight_split], 0.0, 100.0, Plain0, ToneCurve),
    num!("SplitToningShadowHue", [adjust.grading.shadows.hue], 0.0, 360.0, Plain0, Colour),
    num!("SplitToningShadowSaturation", [adjust.grading.shadows.saturation], 0.0, 100.0, Plain0, Colour),
    num!("ColorGradeShadowLum", [adjust.grading.shadows.luminance], -100.0, 100.0, Signed0, Colour),
    num!("ColorGradeMidtoneHue", [adjust.grading.midtones.hue], 0.0, 360.0, Plain0, Colour),
    num!("ColorGradeMidtoneSat", [adjust.grading.midtones.saturation], 0.0, 100.0, Plain0, Colour),
    num!("ColorGradeMidtoneLum", [adjust.grading.midtones.luminance], -100.0, 100.0, Signed0, Colour),
    num!("SplitToningHighlightHue", [adjust.grading.highlights.hue], 0.0, 360.0, Plain0, Colour),
    num!("SplitToningHighlightSaturation", [adjust.grading.highlights.saturation], 0.0, 100.0, Plain0, Colour),
    num!("ColorGradeHighlightLum", [adjust.grading.highlights.luminance], -100.0, 100.0, Signed0, Colour),
    num!("ColorGradeGlobalHue", [adjust.grading.global.hue], 0.0, 360.0, Plain0, Colour),
    num!("ColorGradeGlobalSat", [adjust.grading.global.saturation], 0.0, 100.0, Plain0, Colour),
    num!("ColorGradeGlobalLum", [adjust.grading.global.luminance], -100.0, 100.0, Signed0, Colour),
    num!("ColorGradeBlending", [adjust.grading.blending], 0.0, 100.0, Plain0, Colour),
    num!("SplitToningBalance", [adjust.grading.balance], -100.0, 100.0, Signed0, Colour),
    num!("LensProfileDistortionScale", [adjust.optics.lens_profile_distortion_scale], 0.0, 200.0, Plain0, Lens),
    num!("LensProfileVignettingScale", [adjust.optics.lens_profile_vignetting_scale], 0.0, 200.0, Plain0, Lens),
    num!("LensManualDistortionAmount", [adjust.optics.manual_distortion_amount], -100.0, 100.0, Signed0, Lens),
    num!("VignetteAmount", [adjust.optics.manual_vignette_amount], -100.0, 100.0, Signed0, Lens),
    num!("DefringePurpleAmount", [adjust.optics.defringe.purple_amount], 0.0, 20.0, Plain0, Lens),
    num!("DefringePurpleHueLo", [adjust.optics.defringe.purple_hue_low], 0.0, 100.0, Plain0, Lens),
    num!("DefringePurpleHueHi", [adjust.optics.defringe.purple_hue_high], 0.0, 100.0, Plain0, Lens),
    num!("DefringeGreenAmount", [adjust.optics.defringe.green_amount], 0.0, 20.0, Plain0, Lens),
    num!("DefringeGreenHueLo", [adjust.optics.defringe.green_hue_low], 0.0, 100.0, Plain0, Lens),
    num!("DefringeGreenHueHi", [adjust.optics.defringe.green_hue_high], 0.0, 100.0, Plain0, Lens),
    num!("PerspectiveVertical", [adjust.geometry.perspective_vertical], -100.0, 100.0, Signed0, Transform),
    num!("PerspectiveHorizontal", [adjust.geometry.perspective_horizontal], -100.0, 100.0, Signed0, Transform),
    num!("PerspectiveRotate", [adjust.geometry.perspective_rotate], -10.0, 10.0, Signed1, Transform),
    num!("PerspectiveScale", [adjust.geometry.perspective_scale], 50.0, 150.0, Plain0, Transform),
    num!("PerspectiveAspect", [adjust.geometry.perspective_aspect], -100.0, 100.0, Signed0, Transform),
    num!("PerspectiveX", [adjust.geometry.perspective_offset_x], -100.0, 100.0, Signed1, Transform),
    num!("PerspectiveY", [adjust.geometry.perspective_offset_y], -100.0, 100.0, Signed1, Transform),
    num!("GrainAmount", [adjust.effects.grain.amount], 0.0, 100.0, Plain0, Effects),
    num!("GrainSize", [adjust.effects.grain.size], 0.0, 100.0, Plain0, Effects),
    num!("GrainFrequency", [adjust.effects.grain.frequency], 0.0, 100.0, Plain0, Effects),
    num!("PostCropVignetteAmount", [adjust.effects.vignette.amount], -100.0, 100.0, Signed0, Effects),
    num!("PostCropVignetteMidpoint", [adjust.effects.vignette.midpoint], 0.0, 100.0, Plain0, Effects),
    num!("PostCropVignetteRoundness", [adjust.effects.vignette.roundness], -100.0, 100.0, Signed0, Effects),
    num!("PostCropVignetteFeather", [adjust.effects.vignette.feather], 0.0, 100.0, Plain0, Effects),
    num!("PostCropVignetteHighlightContrast", [adjust.effects.vignette.highlight_contrast], 0.0, 100.0, Plain0, Effects),
    num!("ShadowTint", [adjust.calibration.shadow_tint], -100.0, 100.0, Signed0, Calibration),
    num!("RedHue", [adjust.calibration.red_hue], -100.0, 100.0, Signed0, Calibration),
    num!("RedSaturation", [adjust.calibration.red_saturation], -100.0, 100.0, Signed0, Calibration),
    num!("GreenHue", [adjust.calibration.green_hue], -100.0, 100.0, Signed0, Calibration),
    num!("GreenSaturation", [adjust.calibration.green_saturation], -100.0, 100.0, Signed0, Calibration),
    num!("BlueHue", [adjust.calibration.blue_hue], -100.0, 100.0, Signed0, Calibration),
    num!("BlueSaturation", [adjust.calibration.blue_saturation], -100.0, 100.0, Signed0, Calibration),
    num!("CropTop", [crop.top], 0.0, 1.0, Plain6, Crop),
    num!("CropLeft", [crop.left], 0.0, 1.0, Plain6, Crop),
    num!("CropBottom", [crop.bottom], 0.0, 1.0, Plain6, Crop),
    num!("CropRight", [crop.right], 0.0, 1.0, Plain6, Crop),
    num!("CropAngle", [crop.angle], -45.0, 45.0, Signed2, Crop),
];

/// Managed boolean keys: develop's twelve group switches (the `Enable*` names the spec's enables
/// were captured from, UNVERIFIED) and three feature switches.
pub const CRS_FLAGS: &[CrsFlag] = &[
    flag!("ConvertToGrayscale", [adjust.grayscale.convert]),
    flag!("LensProfileEnable", [adjust.optics.lens_profile_enable]),
    flag!("AutoLateralCA", [adjust.optics.auto_lateral_chromatic_aberration]),
    flag!("EnableCalibration", [enables.enable_calibration]),
    flag!("EnableColorAdjustments", [enables.enable_colour_adjustments]),
    flag!("EnableDetail", [enables.enable_detail]),
    flag!("EnableEffects", [enables.enable_effects]),
    flag!("EnableGrayscaleMix", [enables.enable_grayscale_mix]),
    flag!("EnableLensCorrections", [enables.enable_lens_corrections]),
    flag!("EnableMaskGroupBasedCorrections", [enables.enable_mask_group_based_corrections]),
    flag!("EnableRedEye", [enables.enable_red_eye]),
    flag!("EnableRetouch", [enables.enable_retouch]),
    flag!("EnableToneCurve", [enables.enable_tone_curve]),
    flag!("EnableTransform", [enables.enable_transform]),
    flag!("EnableDistractionRemoval", [enables.enable_distraction_removal]),
];

fn all_keys() -> impl Iterator<Item = &'static CrsKey> {
    CRS_KEYS.iter().chain(BANDS.iter().flatten())
}

fn gate_enable(g: Gate) -> Option<&'static str> {
    Some(match g {
        Gate::None | Gate::Crop => return None,
        Gate::Detail => "EnableDetail",
        Gate::Colour => "EnableColorAdjustments",
        Gate::Grayscale => "EnableGrayscaleMix",
        Gate::ToneCurve => "EnableToneCurve",
        Gate::Lens => "EnableLensCorrections",
        Gate::Transform => "EnableTransform",
        Gate::Effects => "EnableEffects",
        Gate::Calibration => "EnableCalibration",
    })
}

fn fmt_num(v: f64, f: Fmt) -> String {
    let signed = |s: String, pos: bool| if pos { format!("+{s}") } else { s };
    match f {
        Fmt::Signed0 => {
            let r = v.round();
            let r = if r == 0.0 { 0.0 } else { r };
            signed(format!("{r:.0}"), r > 0.0)
        }
        Fmt::Signed1 => {
            let s = format!("{v:.1}");
            let s = if s == "-0.0" { "0.0".to_owned() } else { s };
            signed(s.clone(), !s.starts_with('-') && s != "0.0")
        }
        Fmt::Signed2 => {
            let s = format!("{v:.2}");
            let s = if s == "-0.00" { "0.00".to_owned() } else { s };
            signed(s.clone(), !s.starts_with('-') && s != "0.00")
        }
        Fmt::Plain0 => {
            let r = v.round();
            format!("{:.0}", if r == 0.0 { 0.0 } else { r })
        }
        Fmt::Plain6 => format!("{v:.6}"),
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim() {
        "True" | "true" | "1" => Some(true),
        "False" | "false" | "0" => Some(false),
        _ => None,
    }
}

fn fmt_bool(b: bool) -> &'static str {
    if b { "True" } else { "False" }
}

fn style_value(s: VignetteStyle) -> &'static str {
    match s {
        VignetteStyle::HighlightPriority => "1",
        VignetteStyle::ColourPriority => "2",
    }
}

/// Library fields of a sidecar.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LibrarySidecar {
    pub rating: Option<XmpRating>,
    pub label: Option<String>,
    pub keywords: Vec<String>,
    /// Root-first segments; written `|` separated.
    pub hierarchical_keywords: Vec<Vec<String>>,
}

/// Result of reading a sidecar onto a base recipe.
#[derive(Clone, Debug, PartialEq)]
pub struct SidecarRead {
    pub recipe: DevelopRecipe,
    pub library: LibrarySidecar,
    /// Absolute white balance as stated; applying it needs the raw (`recipe_for_white`).
    pub white_balance: WhiteBalanceSource,
    /// Managed keys applied to the recipe.
    pub applied: Vec<&'static str>,
    /// (key, code) for managed keys present but not applied, in document key order.
    pub losses: Vec<(String, &'static str)>,
    /// Properties not managed here (kept verbatim by any later write).
    pub preserved_properties: u32,
}

/// The values a reader assumes for keys a packet does not carry: develop's identity adjustments,
/// zero exposure and the full crop, on `r`'s other fields.
fn defaults_of(r: &DevelopRecipe) -> DevelopRecipe {
    let mut d = r.clone();
    d.exposure_stops = 0.0;
    d.enables = hsk_studio_develop::Enables::all_disabled();
    d.adjust = hsk_studio_develop::Adjustments::default();
    d.crop = NormalizedCrop { orientation: r.crop.orientation, ..NormalizedCrop::full() };
    d
}

fn crop_identity(c: &NormalizedCrop) -> bool {
    c.top == 0.0 && c.left == 0.0 && c.bottom == 1.0 && c.right == 1.0 && c.angle == 0.0
}

/// Reads a sidecar packet onto `base` (keys absent from the packet keep `base`'s values).
pub fn read_sidecar(bytes: &[u8], base: &DevelopRecipe, limits: &XmpLimits) -> Result<SidecarRead, XmpError> {
    let doc = XmpSidecar::parse(bytes, limits)?;
    let mut r = base.clone();
    let defaults = defaults_of(base);
    let mut applied = Vec::new();
    let mut losses: Vec<(String, &'static str)> = Vec::new();
    let mut managed = 0u32;
    let mut touched: Vec<Gate> = Vec::new();
    let has_crop = doc.scalar(NS_CRS, "HasCrop").map(|v| v.ok().as_deref().and_then(parse_bool));
    if has_crop.is_some() {
        managed = managed.saturating_add(1);
    }
    for k in all_keys() {
        let Some(v) = doc.scalar(NS_CRS, k.key) else { continue };
        managed = managed.saturating_add(1);
        if k.gate == Gate::Crop && has_crop == Some(Some(false)) {
            losses.push((k.key.to_owned(), "crs_crop_disabled"));
            continue;
        }
        let Ok(text) = v else {
            losses.push((k.key.to_owned(), "crs_shape_unsupported"));
            continue;
        };
        let Ok(x) = text.trim().parse::<f64>() else {
            losses.push((k.key.to_owned(), "crs_value_not_number"));
            continue;
        };
        if !x.is_finite() || x < k.lo || x > k.hi {
            losses.push((k.key.to_owned(), "crs_value_out_of_range"));
            continue;
        }
        if fmt_num(x, k.fmt) != fmt_num((k.get)(&defaults), k.fmt) {
            touched.push(k.gate);
        }
        (k.set)(&mut r, x);
        applied.push(k.key);
    }
    // Grain seed (u32) and vignette style (enumeration).
    if let Some(v) = doc.scalar(NS_CRS, "GrainSeed") {
        managed = managed.saturating_add(1);
        match v.ok().and_then(|t| t.trim().parse::<u32>().ok()) {
            Some(seed) => {
                r.adjust.effects.grain.seed = seed;
                applied.push("GrainSeed");
            }
            None => losses.push(("GrainSeed".into(), "crs_value_not_number")),
        }
    }
    if let Some(v) = doc.scalar(NS_CRS, "PostCropVignetteStyle") {
        managed = managed.saturating_add(1);
        let style = match v.ok().as_deref().map(str::trim) {
            Some("1") => Some(VignetteStyle::HighlightPriority),
            Some("2") => Some(VignetteStyle::ColourPriority),
            _ => None,
        };
        match style {
            Some(st) => {
                r.adjust.effects.vignette.style = st;
                applied.push("PostCropVignetteStyle");
            }
            None => losses.push(("PostCropVignetteStyle".into(), "crs_vignette_style_unsupported")),
        }
    }
    let mut explicit: Vec<&'static str> = Vec::new();
    for f in CRS_FLAGS {
        let Some(v) = doc.scalar(NS_CRS, f.key) else { continue };
        managed = managed.saturating_add(1);
        match v.ok().as_deref().and_then(parse_bool) {
            Some(b) => {
                (f.set)(&mut r, b);
                applied.push(f.key);
                explicit.push(f.key);
            }
            None => losses.push((f.key.to_owned(), "crs_value_not_boolean")),
        }
    }
    // A group with authored values and no explicit switch is enabled (the slider acts in Lightroom).
    for g in touched {
        if let Some(name) = gate_enable(g)
            && !explicit.contains(&name)
            && let Some(f) = CRS_FLAGS.iter().find(|f| f.key == name)
        {
            (f.set)(&mut r, true);
        }
    }
    // White balance: reported, not applied.
    let wb_text = doc.scalar(NS_CRS, "WhiteBalance").and_then(Result::ok);
    let white_balance = match wb_text.as_deref() {
        None | Some("As Shot") => WhiteBalanceSource::AsShot,
        Some("Custom") => {
            let num = |k| doc.scalar(NS_CRS, k).and_then(Result::ok).and_then(|t| t.trim().parse::<f64>().ok());
            match (num("Temperature"), num("Tint")) {
                (Some(kelvin), Some(tint)) if white_xy(kelvin, tint).is_ok() => {
                    WhiteBalanceSource::Absolute(AbsoluteWhiteBalance { kelvin, tint })
                }
                _ => {
                    losses.push(("WhiteBalance".into(), "crs_white_balance_invalid"));
                    WhiteBalanceSource::Unmapped
                }
            }
        }
        Some(_) => {
            losses.push(("WhiteBalance".into(), "crs_white_balance_preset_unmapped"));
            WhiteBalanceSource::Unmapped
        }
    };
    for k in ["WhiteBalance", "Temperature", "Tint"] {
        if doc.scalar(NS_CRS, k).is_some() {
            managed = managed.saturating_add(1);
        }
    }
    if let Some(Prop::Elem { elem }) = doc.props.get(&(NS_CRS.to_owned(), "RetouchAreas".to_owned())) {
        managed = managed.saturating_add(1);
        match read_retouch_areas(&doc, *elem, &mut losses) {
            Ok(areas) => {
                r.adjust.retouch.areas = areas;
                applied.push("RetouchAreas");
            }
            Err(code) => losses.push(("RetouchAreas".into(), code)),
        }
    }
    r.validate().map_err(|_| syntax("crs_recipe_invalid"))?;
    let library = read_library(&doc, &mut losses, &mut managed)?;
    let preserved_properties = u32::try_from(doc.property_count()).unwrap_or(u32::MAX).saturating_sub(managed);
    Ok(SidecarRead { recipe: r, library, white_balance, applied, losses, preserved_properties })
}

fn read_library(doc: &XmpSidecar, losses: &mut Vec<(String, &'static str)>, managed: &mut u32) -> Result<LibrarySidecar, XmpError> {
    let mut lib = LibrarySidecar::default();
    if let Some(v) = doc.scalar(NS_XMP, "Rating") {
        *managed = managed.saturating_add(1);
        match v.ok().and_then(|t| t.trim().parse::<i64>().ok()) {
            Some(-1) => lib.rating = Some(XmpRating::Rejected),
            Some(n @ 0..=5) => lib.rating = u8::try_from(n).ok().map(XmpRating::Stars),
            _ => losses.push(("xmp:Rating".into(), "xmp_rating_out_of_range")),
        }
    }
    if let Some(v) = doc.scalar(NS_XMP, "Label") {
        *managed = managed.saturating_add(1);
        match v {
            Ok(t) => lib.label = Some(t),
            Err(_) => losses.push(("xmp:Label".into(), "xmp_label_shape")),
        }
    }
    let shape = |v: Result<Vec<String>, XmpError>, key: &str, losses: &mut Vec<(String, &'static str)>| match v {
        Ok(items) => Ok(Some(items)),
        Err(XmpError::Syntax("xmp_property_shape")) => {
            losses.push((key.to_owned(), "xmp_property_shape"));
            Ok(None)
        }
        Err(e) => Err(e),
    };
    if let Some(v) = doc.list(NS_DC, "subject") {
        *managed = managed.saturating_add(1);
        lib.keywords = shape(v, "dc:subject", losses)?.unwrap_or_default();
    }
    if let Some(v) = doc.list(NS_LR, "hierarchicalSubject") {
        *managed = managed.saturating_add(1);
        let items = shape(v, "lr:hierarchicalSubject", losses)?.unwrap_or_default();
        lib.hierarchical_keywords = items.into_iter().map(|s| s.split('|').map(str::to_owned).collect()).collect();
    }
    Ok(lib)
}

// ------------------------------------------------------------------------------------------------
// Write

/// What a write sets; `None` leaves that part of the packet as it is.
#[derive(Clone, Copy, Debug, Default)]
pub struct SidecarUpdate<'a> {
    pub recipe: Option<&'a DevelopRecipe>,
    pub library: Option<&'a LibrarySidecar>,
}

enum Want {
    Scalar(Option<String>),
    List(Vec<String>, &'static str),
}

const TEMPLATE: &str = "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  <rdf:Description rdf:about=\"\">\n  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>\n";

fn preferred_prefix(ns: &str) -> &'static str {
    match ns {
        NS_CRS => "crs",
        NS_XMP => "xmp",
        NS_DC => "dc",
        NS_LR => "lr",
        _ => "ns",
    }
}

/// Serializes `update` into `original` (or a fresh packet when `None`), changing only the spans of
/// managed properties whose formatted value differs. Refusals are typed; nothing is partially
/// written (the result is a new buffer).
pub fn write_sidecar(original: Option<&[u8]>, update: &SidecarUpdate<'_>, limits: &XmpLimits) -> Result<Vec<u8>, XmpError> {
    let doc = XmpSidecar::parse(original.unwrap_or(TEMPLATE.as_bytes()), limits)?;
    let mut wants: Vec<(&'static str, String, Want)> = Vec::new();
    if let Some(r) = update.recipe {
        r.validate().map_err(|_| syntax("crs_recipe_invalid"))?;
        for k in all_keys() {
            let v = (k.get)(r);
            if !v.is_finite() {
                return Err(syntax("crs_value_not_finite"));
            }
            wants.push((NS_CRS, k.key.to_owned(), Want::Scalar(Some(fmt_num(v, k.fmt)))));
        }
        wants.push((NS_CRS, "GrainSeed".into(), Want::Scalar(Some(r.adjust.effects.grain.seed.to_string()))));
        wants.push((NS_CRS, "PostCropVignetteStyle".into(), Want::Scalar(Some(style_value(r.adjust.effects.vignette.style).into()))));
        for f in CRS_FLAGS {
            wants.push((NS_CRS, f.key.to_owned(), Want::Scalar(Some(fmt_bool((f.get)(r)).into()))));
        }
        wants.push((NS_CRS, "HasCrop".into(), Want::Scalar(Some(fmt_bool(!crop_identity(&r.crop)).into()))));
        match r.adjust.absolute_white_balance {
            Some(wb) => {
                wants.push((NS_CRS, "WhiteBalance".into(), Want::Scalar(Some("Custom".into()))));
                wants.push((NS_CRS, "Temperature".into(), Want::Scalar(Some(fmt_num(wb.kelvin, Fmt::Plain0)))));
                wants.push((NS_CRS, "Tint".into(), Want::Scalar(Some(fmt_num(wb.tint, Fmt::Signed0)))));
            }
            None => {
                // As shot: Lightroom keeps the as-shot Temperature/Tint numbers; leave them.
                wants.push((NS_CRS, "WhiteBalance".into(), Want::Scalar(Some("As Shot".into()))));
            }
        }
    }
    if let Some(l) = update.library {
        let rating = l.rating.map(|r| match r {
            XmpRating::Rejected => "-1".to_owned(),
            XmpRating::Stars(n) => n.to_string(),
        });
        if let Some(XmpRating::Stars(n)) = l.rating
            && n > 5
        {
            return Err(syntax("xmp_rating_out_of_range"));
        }
        if l.keywords.len().saturating_add(l.hierarchical_keywords.len()) > limits.max_keywords as usize {
            return Err(XmpError::KeywordsExceeded);
        }
        let mut hier = Vec::with_capacity(l.hierarchical_keywords.len());
        for path in &l.hierarchical_keywords {
            if path.is_empty() || path.iter().any(|s| s.is_empty() || s.contains('|')) {
                return Err(syntax("xmp_keyword_separator"));
            }
            hier.push(path.join("|"));
        }
        for s in l.keywords.iter().chain(hier.iter()).chain(l.label.iter()) {
            if s.len() > limits.max_text_bytes as usize {
                return Err(XmpError::TextTooLong);
            }
            if s.chars().any(|c| c.is_control() && c != '\t') {
                return Err(syntax("xmp_control_character"));
            }
        }
        wants.push((NS_XMP, "Rating".into(), Want::Scalar(rating)));
        wants.push((NS_XMP, "Label".into(), Want::Scalar(l.label.clone())));
        wants.push((NS_DC, "subject".into(), Want::List(l.keywords.clone(), "Bag")));
        wants.push((NS_LR, "hierarchicalSubject".into(), Want::List(hier, "Bag")));
    }
    let defaults = update.recipe.map(defaults_of);
    apply_wants(&doc, &wants, defaults.as_ref(), update.recipe, limits)
}

fn same_scalar(current: &str, want: &str) -> bool {
    if current == want {
        return true;
    }
    match (current.trim().parse::<f64>(), want.parse::<f64>()) {
        (Ok(a), Ok(b)) => a == b,
        _ => parse_bool(current).is_some() && parse_bool(current) == parse_bool(want),
    }
}

/// Whether an absent key may stay absent: its wanted value equals what a reader infers.
fn absent_is_fine(ns: &str, local: &str, want: &str, defaults: Option<&DevelopRecipe>, recipe: Option<&DevelopRecipe>) -> bool {
    if ns != NS_CRS {
        return false;
    }
    let (Some(d), Some(r)) = (defaults, recipe) else { return false };
    if let Some(k) = all_keys().find(|k| k.key == local) {
        return fmt_num((k.get)(d), k.fmt) == want;
    }
    match local {
        "GrainSeed" => d.adjust.effects.grain.seed.to_string() == want,
        "PostCropVignetteStyle" => style_value(d.adjust.effects.vignette.style) == want,
        "HasCrop" => want == "False",
        "WhiteBalance" => want == "As Shot",
        _ => {
            let Some(f) = CRS_FLAGS.iter().find(|f| f.key == local) else { return false };
            // A reader enables a gate whose sliders differ from the defaults; a feature switch
            // defaults to its value in the default adjustments.
            let inferred = match all_keys().find(|k| gate_enable(k.gate) == Some(local)) {
                Some(_) => all_keys().any(|k| gate_enable(k.gate) == Some(local) && fmt_num((k.get)(r), k.fmt) != fmt_num((k.get)(d), k.fmt)),
                None => (f.get)(d),
            };
            fmt_bool(inferred) == want
        }
    }
}

fn apply_wants(
    doc: &XmpSidecar,
    wants: &[(&'static str, String, Want)],
    defaults: Option<&DevelopRecipe>,
    recipe: Option<&DevelopRecipe>,
    limits: &XmpLimits,
) -> Result<Vec<u8>, XmpError> {
    // (start, end, replacement); insertions have start == end.
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let target = doc.descs[0];
    let desc = &doc.elems[target];
    let rdf = desc.prefix().map_or(String::new(), |p| format!("{p}:"));
    let mut attr_text = String::new();
    let mut child_text = String::new();
    let mut declared: Vec<(String, String)> = Vec::new();
    let prefix_for = |ns: &str, declared: &mut Vec<(String, String)>, attr_text: &mut String| -> String {
        if let Some((p, _)) = declared.iter().find(|(_, u)| u == ns) {
            return p.clone();
        }
        // An in-scope prefix bound to this namespace at the target description.
        let mut cur = Some(target);
        let mut seen: Vec<String> = Vec::new();
        while let Some(e) = cur {
            for (p, u) in &doc.elems[e].decls {
                if !seen.contains(p) {
                    if u == ns && !p.is_empty() {
                        return p.clone();
                    }
                    seen.push(p.clone());
                }
            }
            cur = doc.elems[e].parent;
        }
        let base = preferred_prefix(ns);
        let mut p = base.to_owned();
        let mut i = 1u32;
        while seen.contains(&p) || declared.iter().any(|(q, _)| *q == p) {
            p = format!("{base}{i}");
            i = i.saturating_add(1);
        }
        attr_text.push_str(&format!("\n    xmlns:{p}=\"{}\"", escape(ns)));
        declared.push((p.clone(), ns.to_owned()));
        p
    };
    for (ns, local, want) in wants {
        let existing = doc.props.get(&((*ns).to_owned(), local.clone()));
        match want {
            Want::Scalar(value) => match (existing, value) {
                (None, None) => {}
                (None, Some(v)) => {
                    if absent_is_fine(ns, local, v, defaults, recipe) {
                        continue;
                    }
                    let p = prefix_for(ns, &mut declared, &mut attr_text);
                    attr_text.push_str(&format!("\n   {p}:{local}=\"{}\"", escape(v)));
                }
                (Some(prop), v) => {
                    let current = doc.scalar(ns, local).expect("property exists");
                    let same = match (&current, v) {
                        (Ok(c), Some(v)) => same_scalar(c, v),
                        _ => false,
                    };
                    if same {
                        continue;
                    }
                    match (prop, v) {
                        (Prop::Attr { elem, attr }, Some(v)) => {
                            let a = &doc.elems[*elem].attrs[*attr];
                            edits.push((a.value.start, a.value.end, escape(v)));
                        }
                        (Prop::Attr { elem, attr }, None) => {
                            let a = &doc.elems[*elem].attrs[*attr];
                            edits.push((a.full.start, a.full.end, String::new()));
                        }
                        (Prop::Elem { elem }, Some(v)) => {
                            let e = &doc.elems[*elem];
                            if current.is_err() {
                                return Err(syntax("xmp_property_shape"));
                            }
                            let full = e.full();
                            edits.push((full.start, full.end, format!("<{q}>{}</{q}>", escape(v), q = e.qname)));
                        }
                        (Prop::Elem { elem }, None) => {
                            let full = doc.elems[*elem].full();
                            edits.push((full.start, full.end, String::new()));
                        }
                    }
                }
            },
            Want::List(items, kind) => {
                let serial = |qname: &str, indent: &str| {
                    let mut s = format!("<{qname}>\n{indent} <{rdf}{kind}>\n");
                    for it in items {
                        s.push_str(&format!("{indent}  <{rdf}li>{}</{rdf}li>\n", escape(it)));
                    }
                    s.push_str(&format!("{indent} </{rdf}{kind}>\n{indent}</{qname}>"));
                    s
                };
                match existing {
                    None if items.is_empty() => {}
                    None => {
                        let p = prefix_for(ns, &mut declared, &mut attr_text);
                        child_text.push_str("\n   ");
                        child_text.push_str(&serial(&format!("{p}:{local}"), "   "));
                    }
                    Some(prop) => {
                        let current = doc.list(ns, local).expect("property exists");
                        if current.as_ref().ok() == Some(items) {
                            continue;
                        }
                        let Prop::Elem { elem } = prop else { return Err(syntax("xmp_property_shape")) };
                        let e = &doc.elems[*elem];
                        let full = e.full();
                        let replacement = if items.is_empty() { String::new() } else { serial(&e.qname, "   ") };
                        edits.push((full.start, full.end, replacement));
                    }
                }
            }
        }
    }
    if !attr_text.is_empty() || !child_text.is_empty() {
        let tail = if desc.self_closing { desc.start.end.saturating_sub(2) } else { desc.start.end.saturating_sub(1) };
        let mut repl = attr_text;
        if desc.self_closing {
            if child_text.is_empty() {
                repl.push_str("/>");
            } else {
                repl.push('>');
                repl.push_str(&child_text);
                repl.push_str(&format!("\n  </{}>", desc.qname));
            }
        } else {
            repl.push('>');
        }
        edits.push((tail, desc.start.end, repl));
        if !desc.self_closing && !child_text.is_empty() {
            let at = desc.end.map_or(desc.start.end, |e| e.start);
            edits.push((at, at, format!("{child_text}\n  ")));
        }
    }
    edits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    let mut out = doc.text.clone();
    for (s, e, r) in edits {
        out.replace_range(s..e, &r);
    }
    if out.len() > limits.max_input_bytes as usize {
        return Err(XmpError::InputTooLarge);
    }
    // The result must parse back (defence against a span bug corrupting the packet).
    XmpSidecar::parse(out.as_bytes(), limits)?;
    Ok(out.into_bytes())
}

// ------------------------------------------------------------------------------------------------
// Through the CKC port (CKC owns the sidecar I/O; Studio owns the serialization)

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidecarSyncError {
    Xmp(XmpError),
    Port(CkcError),
}

impl SidecarSyncError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Xmp(e) => e.code(),
            Self::Port(e) => e.code(),
        }
    }
}

/// Reads the asset's sidecar through the port onto `base`; `None` when the asset has none. The
/// returned revision is the precondition for a later [`write_sidecar_via_port`].
pub fn read_sidecar_via_port<P: CkcCatalogPort + ?Sized>(
    port: &P,
    actor: &CkcActor,
    asset_id: &AssetId,
    base: &DevelopRecipe,
    limits: &XmpLimits,
) -> Result<Option<(SidecarRead, u64)>, SidecarSyncError> {
    let Some(current) = port.read_xmp_sidecar(actor, asset_id).map_err(SidecarSyncError::Port)? else {
        return Ok(None);
    };
    let read = read_sidecar(&current.bytes, base, limits).map_err(SidecarSyncError::Xmp)?;
    Ok(Some((read, current.revision)))
}

/// Re-serializes the asset's current sidecar (or a fresh packet) with `update` and writes it
/// through the port under the sidecar-revision precondition read here. `Ok(None)`: the bytes
/// would not change, so no command is sent. A concurrent sidecar change is
/// `Port(StaleRevision { current })`.
pub fn write_sidecar_via_port<P: CkcCatalogPort + ?Sized>(
    port: &P,
    actor: &CkcActor,
    asset_id: &AssetId,
    update: &SidecarUpdate<'_>,
    placement: SidecarPlacement,
    idempotency_key: &str,
    limits: &XmpLimits,
) -> Result<Option<CkcReceipt>, SidecarSyncError> {
    let current = port.read_xmp_sidecar(actor, asset_id).map_err(SidecarSyncError::Port)?;
    let bytes = write_sidecar(current.as_ref().map(|c| c.bytes.as_slice()), update, limits).map_err(SidecarSyncError::Xmp)?;
    if current.as_ref().is_some_and(|c| c.bytes == bytes) {
        return Ok(None);
    }
    let request = XmpSidecarBytesWrite {
        idempotency_key: idempotency_key.to_owned(),
        asset_id: asset_id.clone(),
        expected_sidecar_revision: current.map(|c| c.revision),
        bytes,
        placement,
    };
    port.write_xmp_sidecar_bytes(actor, &request).map(Some).map_err(SidecarSyncError::Port)
}
