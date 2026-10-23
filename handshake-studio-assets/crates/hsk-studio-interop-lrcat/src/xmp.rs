#![deny(clippy::arithmetic_side_effects)]
//! Bounded read-only decoder of an XMP packet (RDF/XML serialization per the public Adobe XMP
//! specification, Part 1) for the library fields Lightroom writes: `xmp:Rating`, `xmp:Label`,
//! `dc:subject` and `lr:hierarchicalSubject`. Used for the packet Lightroom keeps per image in
//! `Adobe_AdditionalMetadata.xmp` and for sidecar bytes a caller grants; it opens no path and
//! writes nothing (XMP write belongs to the CKC owner, STU-ASSET-019). The XML is tokenized per
//! the XML 1.0 rules for the subset XMP uses: entity and character references are decoded before
//! any comparison, comments, processing instructions and CDATA are recognised as such, names are
//! resolved through `xmlns` declarations (the namespace URI decides, never the prefix), and a
//! DOCTYPE is refused (no entity expansion). Every bound is checked before the allocation it
//! guards. `lr:hierarchicalSubject` and its `|` separator are UNVERIFIED against a Lightroom-written
//! packet (D5 oracle).
//! Prefixes `xmp`, `dc`, `lr` and `rdf` used in docs name the namespaces below.

pub const NS_RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const NS_XMP: &str = "http://ns.adobe.com/xap/1.0/";
pub const NS_DC: &str = "http://purl.org/dc/elements/1.1/";
pub const NS_LR: &str = "http://ns.adobe.com/lightroom/1.0/";

pub const HARD_MAX_XMP_INPUT_BYTES: u32 = 64 * 1024 * 1024;
pub const HARD_MAX_XMP_DEPTH: u32 = 256;
pub const HARD_MAX_XMP_NODES: u32 = 1 << 20;
pub const HARD_MAX_XMP_KEYWORDS: u32 = 65_536;
pub const HARD_MAX_XMP_TEXT_BYTES: u32 = 1024 * 1024;

/// Per-packet bounds. `max_nodes` counts elements, attributes and namespace declarations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XmpLimits {
    pub max_input_bytes: u32,
    pub max_depth: u32,
    pub max_nodes: u32,
    pub max_keywords: u32,
    /// Longest decoded attribute value or kept text value.
    pub max_text_bytes: u32,
}

impl Default for XmpLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 4 * 1024 * 1024,
            max_depth: 64,
            max_nodes: 65_536,
            max_keywords: 4096,
            max_text_bytes: 64 * 1024,
        }
    }
}

impl XmpLimits {
    pub fn validate(&self) -> Result<(), XmpError> {
        let zero = self.max_input_bytes == 0
            || self.max_depth == 0
            || self.max_nodes == 0
            || self.max_keywords == 0
            || self.max_text_bytes == 0;
        let over = self.max_input_bytes > HARD_MAX_XMP_INPUT_BYTES
            || self.max_depth > HARD_MAX_XMP_DEPTH
            || self.max_nodes > HARD_MAX_XMP_NODES
            || self.max_keywords > HARD_MAX_XMP_KEYWORDS
            || self.max_text_bytes > HARD_MAX_XMP_TEXT_BYTES;
        if zero || over {
            Err(XmpError::Configuration)
        } else {
            Ok(())
        }
    }
}

/// Typed refusal of one packet; codes are stable and carry no packet text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XmpError {
    Configuration,
    InputTooLarge,
    NotUtf8,
    DepthExceeded,
    NodesExceeded,
    KeywordsExceeded,
    TextTooLong,
    DoctypeRefused,
    Syntax(&'static str),
}

impl XmpError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Configuration => "xmp_limit_configuration",
            Self::InputTooLarge => "xmp_input_too_large",
            Self::NotUtf8 => "xmp_not_utf8",
            Self::DepthExceeded => "xmp_depth_exceeded",
            Self::NodesExceeded => "xmp_nodes_exceeded",
            Self::KeywordsExceeded => "xmp_keywords_exceeded",
            Self::TextTooLong => "xmp_text_too_long",
            Self::DoctypeRefused => "xmp_doctype_refused",
            Self::Syntax(code) => code,
        }
    }
}

/// `xmp:Rating` per the XMP specification: -1 rejected, 0 unrated, 1..5 stars.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XmpRating {
    Rejected,
    Stars(u8),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct XmpLibraryFields {
    pub rating: Option<XmpRating>,
    pub label: Option<String>,
    /// `dc:subject` bag items in document order.
    pub keywords: Vec<String>,
    /// `lr:hierarchicalSubject` bag items split at `|` (root first).
    pub hierarchical_keywords: Vec<Vec<String>>,
    /// Aggregated (code, count) notes for values present but not mapped, in code order.
    pub notes: Vec<(&'static str, u32)>,
    /// Accounted bytes of the kept values, for the caller's retention budget.
    pub retained_bytes: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ns {
    Rdf,
    Xmp,
    Dc,
    Lr,
    Other,
}

fn ns_of(uri: &str) -> Ns {
    match uri {
        NS_RDF => Ns::Rdf,
        NS_XMP => Ns::Xmp,
        NS_DC => Ns::Dc,
        NS_LR => Ns::Lr,
        _ => Ns::Other,
    }
}

/// What a kept text value of an open element feeds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sink {
    None,
    Rating,
    Label,
    Keyword,
    Hierarchical,
}

struct Open<'a> {
    qname: &'a str,
    ns: Ns,
    local: &'a str,
    bindings: usize,
    sink: Sink,
    text: Option<String>,
}

struct Decoder<'a> {
    s: &'a str,
    i: usize,
    limits: XmpLimits,
    nodes: u32,
    bindings: Vec<(&'a str, Ns)>,
    stack: Vec<Open<'a>>,
    out: XmpLibraryFields,
    notes: Vec<(&'static str, u32)>,
    rating_seen: bool,
    label_seen: bool,
}

const VALUE_BYTES: u64 = 24;

impl<'a> Decoder<'a> {
    fn rest(&self) -> &'a str {
        self.s.get(self.i..).unwrap_or("")
    }
    fn bump(&mut self, n: usize) {
        self.i = self.i.saturating_add(n).min(self.s.len());
    }
    fn node(&mut self) -> Result<(), XmpError> {
        if self.nodes >= self.limits.max_nodes {
            return Err(XmpError::NodesExceeded);
        }
        self.nodes = self.nodes.saturating_add(1);
        Ok(())
    }
    fn note(&mut self, code: &'static str) {
        match self.notes.iter_mut().find(|(c, _)| *c == code) {
            Some((_, n)) => *n = n.saturating_add(1),
            None => self.notes.push((code, 1)),
        }
    }
    fn skip_past(&mut self, end: &str, code: &'static str) -> Result<&'a str, XmpError> {
        let rest = self.rest();
        let at = rest.find(end).ok_or(XmpError::Syntax(code))?;
        let body = &rest[..at];
        self.bump(at.saturating_add(end.len()));
        Ok(body)
    }
    fn skip_ws(&mut self) {
        let n = self.rest().len().saturating_sub(self.rest().trim_start_matches([' ', '\t', '\r', '\n']).len());
        self.bump(n);
    }
    fn name(&mut self) -> Result<&'a str, XmpError> {
        let rest = self.rest();
        let end = rest
            .find(|c: char| c.is_ascii_whitespace() || matches!(c, '=' | '>' | '/' | '<' | '"' | '\''))
            .unwrap_or(rest.len());
        if end == 0 {
            return Err(XmpError::Syntax("xmp_bad_name"));
        }
        self.bump(end);
        Ok(&rest[..end])
    }

    /// Decodes character data, replacing entity and character references, and hands each
    /// decoded piece to `sink` (which enforces the length cap before appending).
    fn decode_refs(raw: &str, mut sink: impl FnMut(&str) -> Result<(), XmpError>) -> Result<(), XmpError> {
        let mut rest = raw;
        while let Some(amp) = rest.find('&') {
            sink(&rest[..amp])?;
            let after = &rest[amp..];
            let semi = after.find(';').filter(|s| *s <= 12).ok_or(XmpError::Syntax("xmp_bad_reference"))?;
            let name = &after[1..semi];
            let mut buf = [0u8; 4];
            let piece: &str = match name {
                "lt" => "<",
                "gt" => ">",
                "amp" => "&",
                "quot" => "\"",
                "apos" => "'",
                _ => {
                    let code = if let Some(hex) = name.strip_prefix("#x") {
                        u32::from_str_radix(hex, 16).ok()
                    } else if let Some(dec) = name.strip_prefix('#') {
                        dec.parse::<u32>().ok()
                    } else {
                        return Err(XmpError::Syntax("xmp_unknown_entity"));
                    };
                    let ch = code
                        .and_then(char::from_u32)
                        .filter(|c| *c != '\0')
                        .ok_or(XmpError::Syntax("xmp_bad_reference"))?;
                    ch.encode_utf8(&mut buf)
                }
            };
            sink(piece)?;
            rest = &after[semi.saturating_add(1)..];
        }
        sink(rest)
    }

    /// Decodes into `out`, checking `max_text_bytes` before every append.
    fn decode_into(&self, raw: &str, out: &mut String) -> Result<(), XmpError> {
        let cap = self.limits.max_text_bytes as usize;
        Self::decode_refs(raw, |piece| {
            if out.len().saturating_add(piece.len()) > cap {
                return Err(XmpError::TextTooLong);
            }
            out.push_str(piece);
            Ok(())
        })
    }

    fn resolve(&self, qname: &'a str, is_attr: bool) -> (Ns, &'a str) {
        let (prefix, local) = match qname.split_once(':') {
            Some((p, l)) => (p, l),
            None if is_attr => return (Ns::Other, ""),
            None => ("", qname),
        };
        let ns = self
            .bindings
            .iter()
            .rev()
            .find(|(p, _)| *p == prefix)
            .map_or(Ns::Other, |(_, ns)| *ns);
        (ns, local)
    }

    fn keep(&mut self, sink: Sink, value: String) -> Result<(), XmpError> {
        self.out.retained_bytes = self
            .out
            .retained_bytes
            .saturating_add(VALUE_BYTES)
            .saturating_add(value.len() as u64);
        match sink {
            Sink::None => {}
            Sink::Rating => {
                if self.rating_seen {
                    self.note("xmp_property_repeated");
                    return Ok(());
                }
                self.rating_seen = true;
                self.out.rating = match value.trim().parse::<f64>() {
                    Ok(-1.0) => Some(XmpRating::Rejected),
                    Ok(r) if r.fract() == 0.0 && (0.0..=5.0).contains(&r) => Some(XmpRating::Stars(r as u8)),
                    Ok(_) => {
                        self.note("xmp_rating_out_of_range");
                        None
                    }
                    Err(_) => {
                        self.note("xmp_rating_not_number");
                        None
                    }
                };
            }
            Sink::Label => {
                if self.label_seen {
                    self.note("xmp_property_repeated");
                    return Ok(());
                }
                self.label_seen = true;
                if !value.is_empty() {
                    self.out.label = Some(value);
                }
            }
            Sink::Keyword | Sink::Hierarchical => {
                let total = self
                    .out
                    .keywords
                    .len()
                    .saturating_add(self.out.hierarchical_keywords.len());
                if total >= self.limits.max_keywords as usize {
                    return Err(XmpError::KeywordsExceeded);
                }
                if sink == Sink::Keyword {
                    self.out.keywords.push(value);
                } else {
                    let path: Vec<String> = value.split('|').map(str::to_owned).collect();
                    if path.iter().any(String::is_empty) {
                        self.note("xmp_hierarchy_empty_segment");
                    }
                    self.out.hierarchical_keywords.push(path);
                }
            }
        }
        Ok(())
    }

    fn start_tag(&mut self) -> Result<(), XmpError> {
        self.bump(1);
        let qname = self.name()?;
        self.node()?;
        if self.stack.len() >= self.limits.max_depth as usize {
            return Err(XmpError::DepthExceeded);
        }
        let mark = self.bindings.len();
        let mut attrs: Vec<(&'a str, String)> = Vec::new();
        let empty = loop {
            self.skip_ws();
            let rest = self.rest();
            if rest.starts_with("/>") {
                self.bump(2);
                break true;
            }
            if rest.starts_with('>') {
                self.bump(1);
                break false;
            }
            if rest.is_empty() {
                return Err(XmpError::Syntax("xmp_unterminated_tag"));
            }
            let an = self.name()?;
            self.skip_ws();
            if !self.rest().starts_with('=') {
                return Err(XmpError::Syntax("xmp_attribute_without_value"));
            }
            self.bump(1);
            self.skip_ws();
            let quote = self.rest().chars().next();
            let q = match quote {
                Some(q @ ('"' | '\'')) => q,
                _ => return Err(XmpError::Syntax("xmp_unquoted_attribute")),
            };
            self.bump(1);
            let raw = self.skip_past(if q == '"' { "\"" } else { "'" }, "xmp_unterminated_attribute")?;
            if raw.contains('<') {
                return Err(XmpError::Syntax("xmp_lt_in_attribute"));
            }
            self.node()?;
            let mut value = String::new();
            self.decode_into(raw, &mut value)?;
            if an == "xmlns" {
                self.bindings.push(("", ns_of(&value)));
            } else if let Some(p) = an.strip_prefix("xmlns:") {
                self.bindings.push((p, ns_of(&value)));
            } else {
                attrs.push((an, value));
            }
        };
        let (ns, local) = self.resolve(qname, false);
        let parent = self.stack.last().map(|o| (o.ns, o.local, o.sink));
        let grand = self
            .stack
            .len()
            .checked_sub(2)
            .and_then(|g| self.stack.get(g))
            .map(|o| (o.ns, o.local));
        let in_description = matches!(parent, Some((Ns::Rdf, "Description", _)));
        if ns == Ns::Rdf && local == "Description" {
            for (an, value) in attrs {
                match self.resolve(an, true) {
                    (Ns::Xmp, "Rating") => self.keep(Sink::Rating, value)?,
                    (Ns::Xmp, "Label") => self.keep(Sink::Label, value)?,
                    _ => {}
                }
            }
        }
        let sink = match (ns, local) {
            (Ns::Xmp, "Rating") if in_description => Sink::Rating,
            (Ns::Xmp, "Label") if in_description => Sink::Label,
            (Ns::Rdf, "li") => match (parent, grand) {
                (Some((Ns::Rdf, "Bag" | "Seq" | "Alt", _)), Some((Ns::Dc, "subject"))) => Sink::Keyword,
                (Some((Ns::Rdf, "Bag" | "Seq" | "Alt", _)), Some((Ns::Lr, "hierarchicalSubject"))) => Sink::Hierarchical,
                _ => Sink::None,
            },
            _ => Sink::None,
        };
        if empty {
            if sink != Sink::None {
                self.keep(sink, String::new())?;
            }
            self.bindings.truncate(mark);
        } else {
            self.stack.push(Open {
                qname,
                ns,
                local,
                bindings: mark,
                sink,
                text: (sink != Sink::None).then(String::new),
            });
        }
        Ok(())
    }

    fn end_tag(&mut self) -> Result<(), XmpError> {
        self.bump(2);
        let qname = self.name()?;
        self.skip_ws();
        if !self.rest().starts_with('>') {
            return Err(XmpError::Syntax("xmp_unterminated_tag"));
        }
        self.bump(1);
        let open = self.stack.pop().ok_or(XmpError::Syntax("xmp_unmatched_end"))?;
        if open.qname != qname {
            return Err(XmpError::Syntax("xmp_mismatched_end"));
        }
        self.bindings.truncate(open.bindings);
        if let Some(text) = open.text {
            self.keep(open.sink, text)?;
        }
        Ok(())
    }

    fn text(&mut self, raw: &str, decode: bool) -> Result<(), XmpError> {
        let cap = self.limits.max_text_bytes as usize;
        if let Some(open) = self.stack.last()
            && open.text.is_some()
        {
            let mut piece = String::new();
            if decode {
                self.decode_into(raw, &mut piece)?;
            } else {
                if raw.len() > cap {
                    return Err(XmpError::TextTooLong);
                }
                piece.push_str(raw);
            }
            let Some(buf) = self.stack.last_mut().and_then(|o| o.text.as_mut()) else {
                return Ok(());
            };
            if buf.len().saturating_add(piece.len()) > cap {
                return Err(XmpError::TextTooLong);
            }
            buf.push_str(&piece);
        } else if decode && raw.contains('&') {
            // References are validated even where the text is not kept (no silent acceptance).
            Self::decode_refs(raw, |_| Ok(()))?;
        }
        Ok(())
    }

    fn run(mut self) -> Result<XmpLibraryFields, XmpError> {
        if self.rest().starts_with('\u{feff}') {
            self.bump('\u{feff}'.len_utf8());
        }
        loop {
            let rest = self.rest();
            if rest.is_empty() {
                break;
            }
            if rest.starts_with("<!--") {
                self.bump(4);
                self.skip_past("-->", "xmp_unterminated_comment")?;
            } else if rest.starts_with("<![CDATA[") {
                self.bump(9);
                let body = self.skip_past("]]>", "xmp_unterminated_cdata")?;
                self.text(body, false)?;
            } else if rest.starts_with("<?") {
                self.bump(2);
                self.skip_past("?>", "xmp_unterminated_pi")?;
            } else if rest.starts_with("<!") {
                return Err(XmpError::DoctypeRefused);
            } else if rest.starts_with("</") {
                self.end_tag()?;
            } else if rest.starts_with('<') {
                self.start_tag()?;
            } else {
                let end = rest.find('<').unwrap_or(rest.len());
                let raw = &rest[..end];
                self.bump(end);
                self.text(raw, true)?;
            }
        }
        if !self.stack.is_empty() {
            return Err(XmpError::Syntax("xmp_unclosed_element"));
        }
        let mut out = self.out;
        let mut notes = self.notes;
        notes.sort_unstable();
        out.notes = notes;
        Ok(out)
    }
}

/// Decodes the library fields of one XMP packet. Input size, UTF-8, nesting depth, node count,
/// keyword count and value length are checked before the allocation each guards.
pub fn decode_xmp_packet(bytes: &[u8], limits: &XmpLimits) -> Result<XmpLibraryFields, XmpError> {
    limits.validate()?;
    if bytes.len() > limits.max_input_bytes as usize {
        return Err(XmpError::InputTooLarge);
    }
    let s = std::str::from_utf8(bytes).map_err(|_| XmpError::NotUtf8)?;
    let d = Decoder {
        s,
        i: 0,
        limits: *limits,
        nodes: 0,
        bindings: Vec::new(),
        stack: Vec::new(),
        out: XmpLibraryFields::default(),
        notes: Vec::new(),
        rating_seen: false,
        label_seen: false,
    };
    d.run()
}

/// Shape of a stored metadata value: an XMP text packet, a value that looks like a zlib stream
/// after a 4-byte length prefix (not inflated here), or something else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PacketShape {
    Text,
    CompressedLooking,
    Other,
}

pub(crate) fn packet_shape(bytes: &[u8]) -> PacketShape {
    let trimmed = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let first = trimmed.iter().find(|b| !b.is_ascii_whitespace());
    if first == Some(&b'<') {
        return PacketShape::Text;
    }
    let zlib = |h: &[u8]| match h {
        [a, b, ..] => a & 0x0f == 8 && u16::from_be_bytes([*a, *b]).checked_rem(31) == Some(0),
        _ => false,
    };
    if zlib(bytes) || bytes.get(4..).is_some_and(zlib) {
        PacketShape::CompressedLooking
    } else {
        PacketShape::Other
    }
}

/// Adler-32 (RFC 1950) of `data`.
fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a = a.wrapping_add(u32::from(x));
            b = b.wrapping_add(a);
        }
        a = a.checked_rem(MOD).unwrap_or(0);
        b = b.checked_rem(MOD).unwrap_or(0);
    }
    b.checked_shl(16).unwrap_or(0) | a
}

/// Inflates a zlib stream (RFC 1950), optionally preceded by a 4-byte big-endian length, through
/// the workspace's single shared DEFLATE (`hsk_studio_package::inflate_exact`, D16). The output
/// is capped at `cap` before it is produced; the zlib header (CM 8, FCHECK, no preset
/// dictionary), the Adler-32 trailer and a declared length are all checked. Stable codes only.
pub(crate) fn inflate_packet(bytes: &[u8], cap: usize) -> Result<Vec<u8>, &'static str> {
    let header_ok = |h: &[u8]| match h {
        [a, b, ..] => a & 0x0f == 8 && u16::from_be_bytes([*a, *b]).checked_rem(31) == Some(0),
        _ => false,
    };
    let (declared, stream) = if header_ok(bytes) {
        (None, bytes)
    } else {
        let (len, rest) = bytes.split_at_checked(4).ok_or("xmp_zlib_truncated")?;
        let len = u32::from_be_bytes([len[0], len[1], len[2], len[3]]);
        (Some(usize::try_from(len).map_err(|_| "xmp_inflate_over_limit")?), rest)
    };
    if let Some(n) = declared
        && n > cap
    {
        return Err("xmp_inflate_over_limit");
    }
    if stream.get(1).is_some_and(|f| f & 0x20 != 0) {
        return Err("xmp_zlib_preset_dictionary");
    }
    let body_end = stream.len().checked_sub(4).filter(|e| *e >= 2).ok_or("xmp_zlib_truncated")?;
    let body = &stream[2..body_end];
    let trailer = &stream[body_end..];
    let out = hsk_studio_package::inflate_exact(body, declared.unwrap_or(cap)).map_err(|f| match f {
        hsk_studio_package::InflateFail::OutputLimit => "xmp_inflate_over_limit",
        hsk_studio_package::InflateFail::Invalid | hsk_studio_package::InflateFail::Trailing => "xmp_inflate_failed",
    })?;
    if declared.is_some_and(|n| n != out.len()) {
        return Err("xmp_inflate_length_mismatch");
    }
    if adler32(&out).to_be_bytes() != [trailer[0], trailer[1], trailer[2], trailer[3]] {
        return Err("xmp_zlib_checksum");
    }
    Ok(out)
}
