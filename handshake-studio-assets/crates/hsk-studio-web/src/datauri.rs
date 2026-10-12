//! `data:` URLs: what a browser makes of one, and whether the gate may report it `Inline`, must scan it, or denies it.
//!
//! Reading (each step cites the algorithm it implements):
//! - URL Standard basic URL parser: leading and trailing C0 controls and spaces removed, every ASCII tab or newline removed,
//!   the fragment (from the first `#`) excluded; the serialization of the opaque path percent-encodes C0 controls and code
//!   points above U+007E (and a space before `?`), the query additionally space, `"`, `<` and `>` (`serialized_header`);
//! - Fetch "data: URL processor": MIME type = the code points before the first `,` (ASCII whitespace stripped), no comma =
//!   failure; body = percent-decoding of the rest; `;base64` suffix = forgiving-base64 decode (Infra), failure = failure;
//!   a leading `;` gets `text/plain`; an unparsable MIME type is `text/plain;charset=US-ASCII`;
//! - MIME Sniffing "parse a MIME type" (essence and the `charset` parameter, HTTP quoted strings per Fetch);
//! - a payload read as CSS is decoded per CSS Syntax 3 3.2 and the Encoding Standard: BOM first, then the `charset`
//!   parameter, then a leading `@charset "...";` (UTF-16 labels read as UTF-8), then the environment encoding (the
//!   document's, which this crate reads as text: UTF-8). Supported decoders: UTF-8, UTF-16BE/LE, windows-1252 (all its
//!   labels, incl. `us-ascii` and `iso-8859-1`) and replacement; any other label refuses the payload (fail closed).
//!
//! Policy (`decide`): the carrier's load context (`carriers::Load`) is decided first, then the payload's type within it
//! (R12-F1: a type that is a style sheet in one context is a script, a text track or a manifest in another):
//! - script (`script src`, svg `script href`, `modulepreload`, `preload as=script|worker`): executed whatever the type, so
//!   only the types Fetch blocks for script-like destinations are `Inline` (`audio/*`, `image/*`, `video/*`, `text/csv`:
//!   Fetch Standard 2.10 "should response to request be blocked due to its MIME type?"; HTML "fetch a classic script" does
//!   no MIME type check); `text/css` and every other type is denied;
//! - text track: only images, audio and video stay `Inline`; no text type, `text/css` included, is kept from WebVTT;
//! - manifest (`link rel=manifest|search`): parsed whatever the type, its URLs cannot be scanned: denied;
//! - image, font (`link rel=icon`, `preload as=image|font`), beacons, base and identifiers: any payload is decoded as an
//!   image or font or loads nothing: `Inline` (a `text/css` payload is scanned anyway: its references are reported);
//! - style sheet (`link rel=stylesheet`, `@import`): a same-origin payload of any type is read as CSS in quirks mode, so
//!   the types that are inert elsewhere are scanned as CSS and every other type is denied;
//! - documents, navigations, SVG resource documents, CSS values, prefetched resources, media: `text/css` is scanned;
//!   `Inline` only for types that load nothing further there (raster images, non-playlist audio/video, fonts, plain text,
//!   octet streams); HTML, XML (incl. SVG), scripts, JSON, text tracks, playlists, PDF, multipart and unknown or sniffed
//!   types are denied: their references cannot be scanned (no XML or script reader) and a browser loads them;
//! - a carrier with several contexts (`link rel="icon stylesheet"`) takes the strictest decision (`decide_all`).
//!
//! Proof: `datauri::tests` (each algorithm with escape, decoy and case inputs) and the gate tests in
//! `tests/css_url_gate.rs` (`data_*`, `r12_*`).
#![deny(clippy::arithmetic_side_effects)]

use crate::carriers::{Load, Loads};

/// The header of a `data:` URL (Fetch data: URL processor steps 1-7, 11-14), and the encoded body.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DataUrl<'a> {
    /// `type/subtype`, ASCII-lowercased.
    pub(crate) essence: String,
    /// The `charset` parameter.
    pub(crate) charset: Option<String>,
    base64: bool,
    /// Body as written after the comma and before the fragment (tabs and newlines not yet removed).
    body: &'a str,
}

/// The gate's decision for a `data:` reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Report `Inline`: nothing further loads from it in this carrier.
    Inline,
    /// Report `Inline` and scan the payload as CSS.
    ScanCss,
    /// Report `Denied(InlineDocument)`.
    Deny,
}

impl Decision {
    /// Strictness: reading the payload as CSS reports more than leaving it inline; denying reports it as unreadable.
    const fn rank(self) -> u8 {
        match self {
            Decision::Inline => 0,
            Decision::ScanCss => 1,
            Decision::Deny => 2,
        }
    }
}

/// Longest MIME part (UTF-8 bytes before the comma, tabs and newlines removed) that is read; a longer one is an unknown
/// type (V4-02: no copy of an unbounded header before the payload budget is charged).
const MAX_HEADER_BYTES: usize = 4096;

fn is_url_ws(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r')
}

fn is_token(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '!' | '#'
                | '$'
                | '%'
                | '&'
                | '\''
                | '*'
                | '+'
                | '-'
                | '.'
                | '^'
                | '_'
                | '`'
                | '|'
                | '~'
        )
}

fn is_quoted_token(c: char) -> bool {
    c == '\t' || (' '..='~').contains(&c) || ('\u{80}'..='\u{ff}').contains(&c)
}

fn is_http_ws(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | ' ')
}

/// The MIME part as the URL serializer writes it (opaque path state, then query state after the first `?`; the input's
/// tabs and newlines already removed).
fn serialized_header(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut query = false;
    let mut it = raw.chars().filter(|c| !is_url_ws(*c)).peekable();
    while let Some(c) = it.next() {
        // C0 control percent-encode set: C0 controls and every code point above U+007E.
        let c0_set = !(' '..='~').contains(&c);
        let encode = if query {
            c0_set || matches!(c, ' ' | '"' | '#' | '<' | '>')
        } else if c == '?' {
            query = true;
            false
        } else if c == ' ' {
            it.peek().is_some_and(|n| matches!(n, '?' | '#'))
        } else {
            c0_set
        };
        if encode {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Fetch "collect an HTTP quoted string" with extract-value: `s` starts after the opening quote; returns the value and the
/// rest after the closing quote (or the end).
fn quoted(s: &str) -> (String, &str) {
    let mut value = String::new();
    let mut it = s.char_indices();
    while let Some((i, c)) = it.next() {
        match c {
            '"' => return (value, s.get(i.saturating_add(1)..).unwrap_or_default()),
            '\\' => match it.next() {
                Some((_, n)) => value.push(n),
                None => {
                    value.push('\\');
                    return (value, "");
                }
            },
            c => value.push(c),
        }
    }
    (value, "")
}

/// MIME Sniffing "parse a MIME type": `(essence, charset)`, `None` on failure.
fn parse_mime(input: &str) -> Option<(String, Option<String>)> {
    let input = input.trim_matches(is_http_ws);
    let (ty, rest) = input.split_once('/')?;
    if ty.is_empty() || !ty.chars().all(is_token) {
        return None;
    }
    let (subtype, mut params) = match rest.find(';') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let subtype = subtype.trim_end_matches(is_http_ws);
    if subtype.is_empty() || !subtype.chars().all(is_token) {
        return None;
    }
    let essence = format!(
        "{}/{}",
        ty.to_ascii_lowercase(),
        subtype.to_ascii_lowercase()
    );
    let mut charset: Option<String> = None;
    // `params` is empty or starts with `;`.
    while let Some(after) = params.strip_prefix(';') {
        let after = after.trim_start_matches(is_http_ws);
        let name_end = after.find([';', '=']).unwrap_or(after.len());
        let name = after[..name_end].to_ascii_lowercase();
        let mut rest = &after[name_end..];
        if rest.starts_with(';') {
            params = rest;
            continue;
        }
        let Some(value_start) = rest.strip_prefix('=') else {
            break;
        };
        if value_start.is_empty() {
            break;
        }
        let value = if let Some(q) = value_start.strip_prefix('"') {
            let (value, after_quote) = quoted(q);
            rest = &after_quote[after_quote.find(';').unwrap_or(after_quote.len())..];
            value
        } else {
            let end = value_start.find(';').unwrap_or(value_start.len());
            rest = &value_start[end..];
            let v = value_start[..end].trim_end_matches(is_http_ws);
            if v.is_empty() {
                params = rest;
                continue;
            }
            v.to_owned()
        };
        if name == "charset"
            && charset.is_none()
            && value.chars().all(is_quoted_token)
            && name.chars().all(is_token)
        {
            charset = Some(value);
        }
        params = rest;
    }
    Some((essence, charset))
}

/// Parse `reference` (the text a browser hands to the URL parser) as a `data:` URL; `None` when it is not one or has no
/// comma (the fetch fails and nothing loads).
pub(crate) fn parse(reference: &str) -> Option<DataUrl<'_>> {
    let t = reference.trim_matches(|c: char| c <= ' ');
    // Scheme: the first five code points that are not tab or newline spell `data:` (ASCII case-insensitive).
    let mut seen = 0usize;
    let mut after_scheme = None;
    for (i, c) in t.char_indices() {
        if is_url_ws(c) {
            continue;
        }
        let want = b"data:".get(seen).copied().map(char::from)?;
        if !c.eq_ignore_ascii_case(&want) {
            return None;
        }
        seen = seen.saturating_add(1);
        if seen == 5 {
            after_scheme = Some(i.saturating_add(1));
            break;
        }
    }
    let rest = t.get(after_scheme?..)?;
    let rest = &rest[..rest.find('#').unwrap_or(rest.len())];
    let comma = rest.find(',')?;
    let body = &rest[comma.saturating_add(1)..];
    let header_len = rest[..comma]
        .bytes()
        .filter(|b| !matches!(b, b'\t' | b'\n' | b'\r'))
        .count();
    if header_len > MAX_HEADER_BYTES {
        // V4-02: the header is not copied or serialized beyond the bound; its type stays unknown, which every carrier
        // except images, beacons and identifiers denies (test `oversized_headers_are_an_unknown_type`).
        return Some(DataUrl {
            essence: String::new(),
            charset: None,
            base64: false,
            body,
        });
    }
    let header = serialized_header(&rest[..comma]);
    let mut mime = header.trim_matches(|c: char| c.is_ascii_whitespace());
    let mut base64 = false;
    let lower = mime.to_ascii_lowercase();
    if let Some(head) = lower.strip_suffix("base64") {
        let head = head.trim_end_matches(' ');
        if head.ends_with(';') {
            base64 = true;
            mime = &mime[..head.len().saturating_sub(1)];
        }
    }
    let mime = if mime.starts_with(';') {
        format!("text/plain{mime}")
    } else {
        mime.to_owned()
    };
    let (essence, charset) =
        parse_mime(&mime).unwrap_or_else(|| ("text/plain".to_owned(), Some("US-ASCII".to_owned())));
    Some(DataUrl {
        essence,
        charset,
        base64,
        body,
    })
}

fn hex(b: u8) -> Option<u8> {
    char::from(b)
        .to_digit(16)
        .and_then(|d| u8::try_from(d).ok())
}

/// URL Standard "percent-decode" over the body with tabs and newlines removed.
fn percent_decode(body: &str) -> Vec<u8> {
    let bytes: Vec<u8> = body
        .bytes()
        .filter(|b| !matches!(b, b'\t' | b'\n' | b'\r'))
        .collect();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        let pair = bytes
            .get(i.saturating_add(1))
            .and_then(|h| hex(*h))
            .zip(bytes.get(i.saturating_add(2)).and_then(|l| hex(*l)));
        match (b, pair) {
            (b'%', Some((h, l))) => {
                out.push(h.saturating_mul(16).saturating_add(l));
                i = i.saturating_add(3);
            }
            _ => {
                out.push(b);
                i = i.saturating_add(1);
            }
        }
    }
    out
}

/// Infra "forgiving-base64 decode" of the isomorphic decode of `bytes`; `None` = failure.
fn forgiving_base64(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut data: Vec<u8> = bytes
        .iter()
        .copied()
        .filter(|b| !matches!(b, b'\t' | b'\n' | 0x0c | b'\r' | b' '))
        .collect();
    // `len & 3` = the code point length modulo 4 (every remaining unit is one byte = one code point).
    if data.len() & 3 == 0 {
        if data.ends_with(b"==") {
            data.truncate(data.len().saturating_sub(2));
        } else if data.ends_with(b"=") {
            data.truncate(data.len().saturating_sub(1));
        }
    }
    if data.len() & 3 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(data.len());
    let mut buffer = 0u32;
    let mut bits = 0u8;
    for b in data {
        let n = match b {
            b'A'..=b'Z' => b.saturating_sub(b'A'),
            b'a'..=b'z' => b.saturating_sub(b'a').saturating_add(26),
            b'0'..=b'9' => b.saturating_sub(b'0').saturating_add(52),
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        buffer = buffer.wrapping_shl(6) | u32::from(n);
        bits = bits.saturating_add(6);
        if bits == 24 {
            let [_, b1, b2, b3] = buffer.to_be_bytes();
            out.extend_from_slice(&[b1, b2, b3]);
            buffer = 0;
            bits = 0;
        }
    }
    match bits {
        // 12 bits: discard the last four, one byte.
        12 => {
            let [.., b] = buffer.wrapping_shr(4).to_be_bytes();
            out.push(b);
        }
        // 18 bits: discard the last two, two bytes.
        18 => {
            let [.., b1, b2] = buffer.wrapping_shr(2).to_be_bytes();
            out.extend_from_slice(&[b1, b2]);
        }
        _ => {}
    }
    Some(out)
}

impl DataUrl<'_> {
    /// The body bytes (percent-decoded, base64-decoded when flagged); `None` when base64 decoding fails (the browser's
    /// fetch fails too). Allocates at most twice the body length.
    pub(crate) fn body(&self) -> Option<Vec<u8>> {
        let decoded = percent_decode(self.body);
        if self.base64 {
            forgiving_base64(&decoded)
        } else {
            Some(decoded)
        }
    }

    /// The body's encoded length, charged to the gate's aggregate budget before anything is decoded: `body` holds at most
    /// this many bytes (plus one working copy of the same size), the CSS text at most three times as many (one U+FFFD per
    /// byte).
    pub(crate) fn encoded_len(&self) -> usize {
        self.body.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Encoding {
    Utf8,
    Utf16Be,
    Utf16Le,
    Windows1252,
    Replacement,
}

/// Encoding Standard "get an encoding" for the decoders this module implements; `None` for every other label (supported
/// elsewhere or unknown: the payload is refused).
fn encoding(label: &str) -> Option<Encoding> {
    let l = label
        .trim_matches(|c: char| c.is_ascii_whitespace())
        .to_ascii_lowercase();
    Some(match l.as_str() {
        "unicode-1-1-utf-8" | "unicode11utf8" | "unicode20utf8" | "utf-8" | "utf8"
        | "x-unicode20utf8" => Encoding::Utf8,
        "unicodefffe" | "utf-16be" => Encoding::Utf16Be,
        "csunicode" | "iso-10646-ucs-2" | "ucs-2" | "unicode" | "unicodefeff" | "utf-16"
        | "utf-16le" => Encoding::Utf16Le,
        "ansi_x3.4-1968" | "ascii" | "cp1252" | "cp819" | "csisolatin1" | "ibm819"
        | "iso-8859-1" | "iso-ir-100" | "iso8859-1" | "iso88591" | "iso_8859-1"
        | "iso_8859-1:1987" | "l1" | "latin1" | "us-ascii" | "windows-1252" | "x-cp1252" => {
            Encoding::Windows1252
        }
        "csiso2022kr" | "hz-gb-2312" | "iso-2022-cn" | "iso-2022-cn-ext" | "iso-2022-kr"
        | "replacement" => Encoding::Replacement,
        _ => return None,
    })
}

/// windows-1252 code points of bytes 0x80-0x9F (Encoding Standard index-windows-1252.txt pointers 0-31; 0xA0-0xFF are
/// U+00A0-U+00FF).
const WINDOWS_1252_HIGH: [u16; 32] = [
    0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0x008D, 0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178,
];

fn decode(bytes: &[u8], encoding: Encoding) -> String {
    match encoding {
        Encoding::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        Encoding::Utf16Be | Encoding::Utf16Le => {
            let units = bytes.chunks(2).map(|c| match (c, encoding) {
                ([a, b], Encoding::Utf16Be) => Some(u16::from_be_bytes([*a, *b])),
                ([a, b], _) => Some(u16::from_le_bytes([*a, *b])),
                _ => None,
            });
            let mut whole = Vec::with_capacity(bytes.len());
            let mut odd = false;
            for u in units {
                match u {
                    Some(u) => whole.push(u),
                    None => odd = true,
                }
            }
            // A lone leading surrogate and a lone byte at the end are one error (Encoding Standard UTF-16 decoder, end of
            // queue with a leading byte or surrogate pending).
            let pending_surrogate = whole.last().is_some_and(|u| (0xd800..=0xdbff).contains(u));
            let mut out: String = char::decode_utf16(whole)
                .map(|r| r.unwrap_or('\u{fffd}'))
                .collect();
            if odd && !pending_surrogate {
                out.push('\u{fffd}');
            }
            out
        }
        Encoding::Windows1252 => bytes
            .iter()
            .map(|&b| match b {
                0x80..=0x9f => WINDOWS_1252_HIGH
                    .get(usize::from(b.saturating_sub(0x80)))
                    .and_then(|u| char::from_u32(u32::from(*u)))
                    .unwrap_or('\u{fffd}'),
                b => char::from(b),
            })
            .collect(),
        Encoding::Replacement => {
            if bytes.is_empty() {
                String::new()
            } else {
                "\u{fffd}".to_owned()
            }
        }
    }
}

/// The label of a leading `@charset "...";` (CSS Syntax 3 3.2: the bytes `@charset "`, label bytes 0x00-0x21 or
/// 0x23-0x7F, `";`, all within the first 1024 bytes).
fn charset_rule(bytes: &[u8]) -> Option<&str> {
    let rest = bytes.strip_prefix(b"@charset \"")?;
    let end = rest.iter().position(|b| *b == b'"')?;
    let label = rest.get(..end)?;
    let within = end.saturating_add(12) <= 1024;
    (within && rest.get(end.saturating_add(1)) == Some(&b';') && label.is_ascii())
        .then(|| std::str::from_utf8(label).ok())
        .flatten()
}

/// The CSS text a browser decodes from `bytes` (CSS Syntax 3 3.2 + Encoding Standard decode); `None` when the encoding
/// the browser would use is not implemented here (refused).
pub(crate) fn css_text(bytes: &[u8], charset: Option<&str>) -> Option<String> {
    // BOM sniff: more authoritative than any label.
    if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return Some(decode(rest, Encoding::Utf8));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return Some(decode(rest, Encoding::Utf16Be));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return Some(decode(rest, Encoding::Utf16Le));
    }
    let fallback = if let Some(label) = charset {
        encoding(label)?
    } else if let Some(label) = charset_rule(bytes) {
        match encoding(label)? {
            Encoding::Utf16Be | Encoding::Utf16Le => Encoding::Utf8,
            e => e,
        }
    } else {
        // Environment encoding: the referring document's, read by this crate as UTF-8 text.
        Encoding::Utf8
    };
    Some(decode(bytes, fallback))
}

/// Types that load nothing further in a document, resource, media, track or CSS-value carrier. Every XML type (`+xml`)
/// can be an XML document (with XHTML, SVG or style sheet processing instructions): never inert.
fn inert(essence: &str) -> bool {
    let (ty, sub) = essence.split_once('/').unwrap_or((essence, ""));
    if sub.ends_with("+xml") {
        return false;
    }
    match ty {
        "image" | "font" => true,
        "audio" | "video" => !sub.contains("mpegurl"),
        "text" => sub == "plain",
        "application" => {
            matches!(sub, "octet-stream" | "ogg" | "vnd.ms-fontobject")
                || sub.starts_with("font-")
                || sub.starts_with("x-font-")
        }
        _ => false,
    }
}

/// Fetch "should response to request be blocked due to its MIME type?" for a script-like destination (Fetch Standard
/// 2.10): `audio/*`, `image/*`, `video/*` and `text/csv` are blocked. HTML "fetch a classic script" does no MIME type check
/// of its own ("for historical reasons"), so every other type, `text/css` included, is executed.
fn script_blocked(essence: &str) -> bool {
    let ty = essence.split_once('/').map_or(essence, |(t, _)| t);
    matches!(ty, "image" | "audio" | "video") || essence == "text/csv"
}

/// What the gate does with a `data:` reference in a carrier with load context `load`. The load context is decided first
/// (what a browser does with the fetched resource); the payload's type only decides within it (module docs).
pub(crate) fn decide(load: Load, url: &DataUrl<'_>) -> Decision {
    let essence = url.essence.as_str();
    let css = essence == "text/css";
    match load {
        // Executed or parsed as a non-CSS format whatever the type says: `text/css` is not a style sheet here (R12-F1,
        // R12-P1). A script carrier keeps `Inline` only for the types Fetch blocks for scripts, a text track only for the
        // inert media types (no text type is excluded from WebVTT), a manifest or OpenSearch description for none.
        Load::Script => {
            if script_blocked(essence) {
                Decision::Inline
            } else {
                Decision::Deny
            }
        }
        Load::Track => {
            let ty = essence.split_once('/').map_or(essence, |(t, _)| t);
            if matches!(ty, "image" | "audio" | "video") && inert(essence) {
                Decision::Inline
            } else {
                Decision::Deny
            }
        }
        Load::Manifest => Decision::Deny,
        // Decoded as an image or a font, or nothing is loaded: any payload is inert there; a CSS payload is scanned
        // anyway (its references are reported).
        Load::Image | Load::Font | Load::Beacon | Load::None | Load::Base => {
            if css {
                Decision::ScanCss
            } else {
                Decision::Inline
            }
        }
        // A style sheet reads every type that is inert elsewhere as CSS (quirks mode) and denies the rest.
        Load::Stylesheet => {
            if css || inert(essence) {
                Decision::ScanCss
            } else {
                Decision::Deny
            }
        }
        // Documents, resource documents, media: CSS is scanned, inert types stay inline, the rest is denied.
        Load::Document | Load::Resource | Load::Media | Load::Inline => {
            if css {
                Decision::ScanCss
            } else if inert(essence) {
                Decision::Inline
            } else {
                Decision::Deny
            }
        }
    }
}

/// The decision for a carrier with several load contexts (`Loads`): every context applies, so the strictest decision wins
/// (`Inline` < `ScanCss` < `Deny`). No context at all is `Deny` (fail closed; `Load::Script` is not the strictest single
/// reading: for `image/svg+xml` it is `Inline` while `Manifest`, `Track`, `Document` and `Resource` deny, test
/// `load_context_decides_before_the_type`). The collector hands this function no empty set: `carriers::link_loads` falls
/// back to a document (test `link_keywords_choose_the_load_contexts`) and `animated_loads` returns at least one context for
/// every target shape (test `animated_loads_returns_a_context_for_every_target_shape`).
pub(crate) fn decide_all(loads: Loads, url: &DataUrl<'_>) -> Decision {
    let mut decision: Option<Decision> = None;
    for load in loads.iter() {
        let next = decide(load, url);
        decision = Some(match decision {
            Some(prev) if prev.rank() >= next.rank() => prev,
            _ => next,
        });
    }
    decision.unwrap_or(Decision::Deny)
}

#[cfg(test)]
mod tests;
