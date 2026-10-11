//! Granted asset resolution. The crate does not fetch, open or resolve anything itself: every reference found
//! in the source is classified, and only relative references reach the caller's `AssetPort`, which alone decides
//! what is granted. External, protocol-relative, `file:`, script and other scheme URLs are typed `Denied`
//! without the port being consulted (tests: `external_asset_denial_cancel_stale` plus the R5 regressions
//! `css_escapes_cannot_hide_external_urls`, `css_decoys_case_and_whitespace_follow_the_tokenizer`,
//! `character_references_cannot_hide_schemes`, `base_href_re_targets_every_relative_reference`,
//! `more_url_attributes_and_foreign_content_are_scanned`, `ambiguous_relative_paths_never_reach_the_port`, and the R6
//! regressions listed in `DESCRIPTOR` assets.proof).
//!
//! Values are read the way a browser reads them before they are classified. The document comes from `dom::parse`,
//! whose tokenizer states and tree-construction context follow the HTML parser (or refuse the document typed), so
//! every `style` is RAWTEXT exactly where a browser makes it RAWTEXT. The texts the gate interprets are the texts the
//! parser hands on, not the raw span bytes (R9 tests `foreign_style_cdata_is_preprocessed_before_css`,
//! `text_consumers_read_the_text_a_browser_builds`, `raw_nul_in_url_attributes_reads_like_a_reference_to_it`; the
//! differential check `style-sheet-text`): attribute values are decoded with input-stream preprocessing, the
//! attribute-value U+0000 rule and the exact attribute-value character-reference rules (`charref`, complete WHATWG
//! table); style text is `WebDoc::sheet` (HTML: the RAWTEXT body preprocessed; foreign: text nodes and CDATA sections
//! under the foreign-content rules, preprocessed, concatenated); style text is then tokenized per CSS Syntax Level 3
//! (`cssurl`). Raw spans only locate a reference (the whole value or content span when decoding moved offsets).
#![deny(clippy::arithmetic_side_effects)]

use crate::charref::{Context, decode};
use crate::cssurl::{RefKind, css_references};
use crate::dom::{NodeId, NodeKind, WebDoc};
use crate::error::{LimitKind, WebError};
use hsk_studio_accord::CancellationToken;
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssetDecision {
    Granted { id: String },
    Denied,
    Unavailable,
}

/// Caller-owned grant lookup for relative references. Must not block, fetch or have side effects. A reference
/// that climbs out with plain `..` segments is the port's containment decision: the document location is not
/// known here (backslashes and percent-encoded dots/slashes are refused before the port, see
/// `DenyReason::AmbiguousPath`).
pub trait AssetPort: Send + Sync {
    fn resolve(&self, reference: &str) -> AssetDecision;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DenyReason {
    /// `http:`, `https:` and any other scheme with an authority or side effect (`ftp:`, `blob:`, `mailto:`, ...).
    ExternalScheme,
    /// `//host/...`
    ProtocolRelative,
    FileScheme,
    /// `javascript:` and `vbscript:`.
    ScriptScheme,
    /// Port answered `Denied`, the reference carries control characters, or it is a bad-url token.
    NotGranted,
    /// The document has a `<base href>` (any value, any position): it re-targets every relative reference, so none is
    /// offered to the port. The base itself is reported with this reason unless it is already denied for its scheme.
    BaseHref,
    /// `<iframe srcdoc>`: an inline document whose own references are not scanned.
    InlineDocument,
    /// A relative path with a backslash or a percent-encoded dot, slash or backslash: browsers and file-system ports
    /// disagree on what it means.
    AmbiguousPath,
    /// A function inside a CSS URL function (`image-set()`, `src()`, `image()`, ...) that computes its value at style time
    /// (`var()`, `attr()`, `env()`, custom `--*()` functions, any unknown function): the URL is not in the source.
    ComputedUrl,
}
impl DenyReason {
    pub const fn code(self) -> &'static str {
        match self {
            Self::ExternalScheme => "external_scheme",
            Self::ProtocolRelative => "protocol_relative",
            Self::FileScheme => "file_scheme",
            Self::ScriptScheme => "script_scheme",
            Self::NotGranted => "not_granted",
            Self::BaseHref => "base_href",
            Self::InlineDocument => "inline_document",
            Self::AmbiguousPath => "ambiguous_path",
            Self::ComputedUrl => "computed_url",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssetOutcome {
    Granted {
        id: String,
    },
    Unavailable,
    Denied(DenyReason),
    /// `data:` URI: carried in the source itself, nothing to resolve.
    Inline,
    /// `#fragment`: same document.
    SameDocument,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssetSource {
    /// Lower-cased attribute name.
    Attribute(String),
    InlineStyle,
    StyleBody,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetRef {
    pub node: NodeId,
    pub source: AssetSource,
    /// Decoded reference text as the port saw it (or would have).
    pub reference: String,
    /// Bytes in the source holding the reference (the whole value when decoding changed offsets).
    pub span: Range<usize>,
    pub outcome: AssetOutcome,
}

/// How an attribute value carries references.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// One URL (spaces around it are ignored).
    Url,
    /// `<base href>`: re-targets relative references.
    Base,
    /// ASCII-whitespace separated URLs (`ping`, `archive`).
    UrlList,
    /// Image candidate strings (`srcset`, `imagesrcset`).
    Srcset,
    /// CSS text (`style`, SVG presentation attributes): `url(...)` tokens.
    Css,
    /// `<meta http-equiv=refresh content="N; url=...">`.
    Refresh,
    /// `;`-separated URLs (SVG animation `values` of an animated `href`).
    UrlValues,
    /// Inline HTML document (`srcdoc`).
    InlineDoc,
}

/// URL-bearing attributes (WHATWG HTML attributes index and obsolete features, SVG 2 presentation attributes).
fn attr_kind(tag: &str, name: &str) -> Option<Kind> {
    Some(match (tag, name) {
        // `href` and `xlink:href` on every element: html `a area link`, svg `a image use script feImage ...`.
        (_, "href" | "xlink:href") => Kind::Url,
        // `image` is rewritten to `img` by the HTML parser.
        (
            "img" | "image" | "source" | "video" | "audio" | "iframe" | "frame" | "embed" | "track"
            | "input" | "script",
            "src",
        ) => Kind::Url,
        ("video", "poster") => Kind::Url,
        ("object", "data" | "codebase" | "classid") => Kind::Url,
        ("object", "archive") => Kind::UrlList,
        ("form", "action") => Kind::Url,
        ("button" | "input", "formaction") => Kind::Url,
        ("blockquote" | "del" | "ins" | "q", "cite") => Kind::Url,
        ("a" | "area", "ping") => Kind::UrlList,
        ("img" | "source", "srcset") | ("link", "imagesrcset") => Kind::Srcset,
        ("body" | "table" | "thead" | "tbody" | "tfoot" | "tr" | "td" | "th", "background") => {
            Kind::Url
        }
        ("iframe" | "img", "longdesc") | ("img", "lowsrc") | ("html", "manifest") => Kind::Url,
        ("iframe", "srcdoc") => Kind::InlineDoc,
        (_, "style") => Kind::Css,
        (
            _,
            "fill" | "stroke" | "clip-path" | "cursor" | "filter" | "mask" | "marker-start"
            | "marker-mid" | "marker-end",
        ) => Kind::Css,
        _ => return None,
    })
}

/// Classify without any I/O. `None` = relative: the port decides.
fn classify(reference: &str) -> Option<AssetOutcome> {
    // URL parsing drops tab/newline/CR and strips leading/trailing C0 controls and spaces; mirror that so
    // `java\nscript:` cannot hide a scheme, and treat `\` as `/` for the authority check.
    let cleaned: String = reference
        .trim_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .map(|c| {
            if c == '\\' {
                '/'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    if cleaned.starts_with('#') {
        return Some(AssetOutcome::SameDocument);
    }
    if cleaned.starts_with("//") {
        return Some(AssetOutcome::Denied(DenyReason::ProtocolRelative));
    }
    let scheme_end = cleaned.find(':');
    let first_delim = cleaned.find(['/', '?', '#']);
    if let Some(colon) = scheme_end
        && first_delim.is_none_or(|d| colon < d)
    {
        let scheme = &cleaned[..colon];
        let valid = scheme
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
        if valid {
            return Some(match scheme {
                "data" => AssetOutcome::Inline,
                "file" => AssetOutcome::Denied(DenyReason::FileScheme),
                "javascript" | "vbscript" => AssetOutcome::Denied(DenyReason::ScriptScheme),
                _ => AssetOutcome::Denied(DenyReason::ExternalScheme),
            });
        }
    }
    None
}

/// A relative path (before `?`/`#`) that a browser and a file-system port may read differently.
fn ambiguous_path(reference: &str) -> bool {
    let path = reference.split(['?', '#']).next().unwrap_or(reference);
    path.contains('\\')
        || path.as_bytes().windows(3).any(|w| {
            w[0] == b'%'
                && matches!(
                    (w[1], w[2]),
                    (b'2', b'e' | b'E' | b'f' | b'F') | (b'5', b'c' | b'C')
                )
        })
}

/// Ranges of ASCII-whitespace separated tokens.
fn tokens(s: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, b) in s.bytes().enumerate() {
        match (b.is_ascii_whitespace(), start) {
            (true, Some(st)) => {
                out.push(st..i);
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(st) = start {
        out.push(st..s.len());
    }
    out
}

/// URL ranges of the image candidate strings of a `srcset` value (HTML "parse a srcset attribute": URL = run of
/// non-whitespace, trailing commas stripped, descriptors end at a comma outside parentheses). Candidates with
/// invalid descriptors are still reported: a browser that drops them loads nothing, reporting more is safe.
fn srcset_urls(s: &str) -> Vec<Range<usize>> {
    let b = s.as_bytes();
    let len = b.len();
    let mut out = Vec::new();
    let mut pos = 0usize;
    loop {
        while pos < len && (b[pos].is_ascii_whitespace() || b[pos] == b',') {
            pos = pos.saturating_add(1);
        }
        if pos >= len {
            break;
        }
        let start = pos;
        while pos < len && !b[pos].is_ascii_whitespace() {
            pos = pos.saturating_add(1);
        }
        let mut end = pos;
        if b[end.saturating_sub(1)] == b',' {
            while end > start && b[end.saturating_sub(1)] == b',' {
                end = end.saturating_sub(1);
            }
        } else {
            let mut in_parens = false;
            while pos < len {
                let c = b[pos];
                pos = pos.saturating_add(1);
                match c {
                    b'(' => in_parens = true,
                    b')' => in_parens = false,
                    b',' if !in_parens => break,
                    _ => {}
                }
            }
        }
        if end > start {
            out.push(start..end);
        }
    }
    out
}

/// URL range of a `<meta http-equiv=refresh>` content value (HTML "shared declarative refresh steps").
fn refresh_url(s: &str) -> Option<Range<usize>> {
    let b = s.as_bytes();
    let len = b.len();
    let skip_ws = |mut i: usize| {
        while b.get(i).is_some_and(u8::is_ascii_whitespace) {
            i = i.saturating_add(1);
        }
        i
    };
    let mut pos = skip_ws(0);
    let digits_start = pos;
    while b.get(pos).is_some_and(u8::is_ascii_digit) {
        pos = pos.saturating_add(1);
    }
    if pos == digits_start && b.get(pos) != Some(&b'.') {
        return None;
    }
    while b.get(pos).is_some_and(|c| c.is_ascii_digit() || *c == b'.') {
        pos = pos.saturating_add(1);
    }
    if pos < len {
        if !(matches!(b[pos], b';' | b',') || b[pos].is_ascii_whitespace()) {
            return None;
        }
        pos = skip_ws(pos);
        if matches!(b.get(pos), Some(b';' | b',')) {
            pos = pos.saturating_add(1);
        }
        pos = skip_ws(pos);
    }
    if pos >= len {
        return None;
    }
    // Optional `url =` prefix; a partial prefix means the whole remainder is the URL.
    let mut quoted_from = pos;
    if matches!(b.get(pos), Some(b'u' | b'U')) {
        let mut p = pos.saturating_add(1);
        let full = matches!(b.get(p), Some(b'r' | b'R'))
            && {
                p = p.saturating_add(1);
                matches!(b.get(p), Some(b'l' | b'L'))
            }
            && {
                p = skip_ws(p.saturating_add(1));
                b.get(p) == Some(&b'=')
            };
        if !full {
            return Some(pos..len);
        }
        quoted_from = skip_ws(p.saturating_add(1));
    }
    let mut start = quoted_from;
    let mut end = len;
    if let Some(&q @ (b'\'' | b'"')) = b.get(start) {
        start = start.saturating_add(1);
        if let Some(off) = b[start..].iter().position(|c| *c == q) {
            end = start.saturating_add(off);
        }
    }
    (start <= end).then_some(start..end)
}

fn has_base_href(doc: &WebDoc, cancel: &CancellationToken) -> Result<bool, WebError> {
    for id in 0..doc.len() {
        if id & 0x3ff == 0 {
            cancel.check()?;
        }
        let is_element = doc.node(id).is_some_and(|n| n.kind == NodeKind::Element);
        if is_element
            && doc
                .tag_name(id)
                .is_some_and(|t| t.eq_ignore_ascii_case("base"))
            && doc.attribute(id, "href").is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

struct Collector<'a> {
    doc: &'a WebDoc,
    port: &'a dyn AssetPort,
    base_blocks: bool,
    limit: usize,
    out: Vec<AssetRef>,
}

impl Collector<'_> {
    fn decide(&self, reference: &str, bad: bool) -> AssetOutcome {
        if let Some(done) = classify(reference) {
            return done;
        }
        if bad || reference.chars().any(char::is_control) {
            return AssetOutcome::Denied(DenyReason::NotGranted);
        }
        if self.base_blocks {
            return AssetOutcome::Denied(DenyReason::BaseHref);
        }
        if ambiguous_path(reference) {
            return AssetOutcome::Denied(DenyReason::AmbiguousPath);
        }
        match self.port.resolve(reference) {
            AssetDecision::Granted { id } => AssetOutcome::Granted { id },
            AssetDecision::Denied => AssetOutcome::Denied(DenyReason::NotGranted),
            AssetDecision::Unavailable => AssetOutcome::Unavailable,
        }
    }

    fn push(
        &mut self,
        node: NodeId,
        source: AssetSource,
        reference: String,
        span: Range<usize>,
        outcome: AssetOutcome,
    ) -> Result<(), WebError> {
        if self.out.len() >= self.limit {
            return Err(WebError::LimitExceeded {
                limit: LimitKind::Nodes,
                span,
            });
        }
        self.out.push(AssetRef {
            node,
            source,
            reference,
            span,
            outcome,
        });
        Ok(())
    }

    fn reference(
        &mut self,
        node: NodeId,
        source: AssetSource,
        reference: &str,
        bad: bool,
        span: Range<usize>,
    ) -> Result<(), WebError> {
        let outcome = self.decide(reference, bad);
        self.push(node, source, reference.to_owned(), span, outcome)
    }

    /// References in CSS `css` (the text a browser's CSS parser reads); `locate` maps a range of `css` to the source bytes
    /// it came from (location only). Blocks nested deeper than `cssurl::MAX_NESTING` refuse the scan typed
    /// (`LimitExceeded(Depth)`), so no reference inside them goes unseen.
    fn css(
        &mut self,
        node: NodeId,
        source: &AssetSource,
        css: &str,
        locate: &dyn Fn(&Range<usize>) -> Range<usize>,
    ) -> Result<(), WebError> {
        let room = self.limit.saturating_sub(self.out.len()).saturating_add(1);
        let found = css_references(css, room).map_err(|span| WebError::LimitExceeded {
            limit: LimitKind::Depth,
            span: locate(&span),
        })?;
        for r in found {
            match r.kind {
                RefKind::Url => self.reference(node, source.clone(), &r.value, false, locate(&r.span))?,
                RefKind::Bad => self.reference(node, source.clone(), &r.value, true, locate(&r.span))?,
                RefKind::Computed => self.push(
                    node,
                    source.clone(),
                    r.value,
                    locate(&r.span),
                    AssetOutcome::Denied(DenyReason::ComputedUrl),
                )?,
            }
        }
        Ok(())
    }

    fn attribute(
        &mut self,
        node: NodeId,
        kind: Kind,
        name: &str,
        value_span: Range<usize>,
    ) -> Result<(), WebError> {
        let raw = self.doc.slice(&value_span);
        let decoded = decode(raw, Context::Attribute);
        let source = if name == "style" {
            AssetSource::InlineStyle
        } else {
            AssetSource::Attribute(name.to_owned())
        };
        let text = decoded.text.as_str();
        let exact = text == raw;
        let sub = |r: &Range<usize>| {
            if exact {
                value_span.start.saturating_add(r.start)..value_span.start.saturating_add(r.end)
            } else {
                value_span.clone()
            }
        };
        match kind {
            Kind::Url => {
                if !text.trim().is_empty() {
                    self.reference(node, source, text, false, value_span.clone())?;
                }
            }
            Kind::Base => {
                if !text.trim().is_empty() {
                    let outcome = match classify(text) {
                        Some(AssetOutcome::Denied(reason)) => AssetOutcome::Denied(reason),
                        _ => AssetOutcome::Denied(DenyReason::BaseHref),
                    };
                    self.push(node, source, text.to_owned(), value_span.clone(), outcome)?;
                }
            }
            Kind::UrlValues => {
                let mut from = 0usize;
                for part in text.split(';') {
                    let start = from;
                    from = from.saturating_add(part.len()).saturating_add(1);
                    let lead = part.len().saturating_sub(part.trim_start().len());
                    let item = part.trim();
                    if !item.is_empty() {
                        let r = start.saturating_add(lead)
                            ..start.saturating_add(lead).saturating_add(item.len());
                        self.reference(node, source.clone(), item, false, sub(&r))?;
                    }
                }
            }
            Kind::UrlList => {
                for r in tokens(text) {
                    self.reference(node, source.clone(), &text[r.clone()], false, sub(&r))?;
                }
            }
            Kind::Srcset => {
                for r in srcset_urls(text) {
                    self.reference(node, source.clone(), &text[r.clone()], false, sub(&r))?;
                }
            }
            Kind::Refresh => {
                if let Some(r) = refresh_url(text).filter(|r| !text[r.clone()].trim().is_empty()) {
                    self.reference(node, source, &text[r.clone()], false, sub(&r))?;
                }
            }
            Kind::Css => self.css(node, &source, text, &sub)?,
            Kind::InlineDoc => {
                if !text.trim().is_empty() {
                    self.push(
                        node,
                        source,
                        text.to_owned(),
                        value_span.clone(),
                        AssetOutcome::Denied(DenyReason::InlineDocument),
                    )?;
                }
            }
        }
        Ok(())
    }

    /// A style sheet: exactly the text a browser's CSS parser reads (`WebDoc::sheet`): the RAWTEXT body of an HTML
    /// `style` after input-stream preprocessing (never character-reference decoded), or the child text content of a
    /// foreign `style` (its text nodes' and CDATA sections' character data, each preprocessed as the tokenizer and the
    /// foreign-content rules build it, concatenated in order; comments and child elements contribute nothing). The raw
    /// span bytes are only used to locate a reference.
    fn style_children(&mut self, id: NodeId) -> Result<(), WebError> {
        let Some(sheet) = self.doc.sheet(id) else {
            return Ok(());
        };
        if sheet.text.is_empty() {
            return Ok(());
        }
        self.css(id, &AssetSource::StyleBody, &sheet.text, &|r| sheet.span(r))
    }
}

/// The URL kind an SVG animation element (`set`, `animate`) writes into its target attribute (`attributeName`).
fn animated_kind(doc: &WebDoc, id: NodeId) -> Option<Kind> {
    let target = doc
        .attribute(id, "attributename")
        .and_then(|a| doc.attr_value(a))?
        .trim()
        .to_ascii_lowercase();
    if target == "src" {
        return Some(Kind::Url);
    }
    match attr_kind("", &target)? {
        Kind::Url => Some(Kind::Url),
        Kind::Css => Some(Kind::Css),
        _ => None,
    }
}

/// Every asset-bearing reference in the document, in document order, with the decision for each: URL attributes
/// (`href`/`xlink:href` on any element, `src` on `img image source video audio iframe frame embed track input
/// script`, `poster`, `object data/codebase/classid/archive`, `form action`, `formaction`, `cite`, `ping`,
/// `srcset`/`imagesrcset` candidates, obsolete `background longdesc lowsrc manifest`, `<meta http-equiv=refresh>`
/// URL), `<base href>`, `iframe srcdoc` (reported denied), and in CSS text (`style` attributes, SVG presentation attributes
/// `fill stroke clip-path cursor filter mask marker-*`, `to from by values` of SVG `set`/`animate` whose `attributeName` is
/// `href`, `xlink:href`, `src` or one of those presentation attributes, and `<style>` contents) every URL a browser loads:
/// url tokens, the strings of `url()`, `src()`, `image-set()`, `-webkit-image-set()`, `image()`, `cross-fade()` and
/// `@import`, bad-url tokens (denied) and computed URLs (`Denied(ComputedUrl)`), read by the CSS Syntax 3 tokenizer in
/// `cssurl` (tests: `tests/css_url_gate.rs`). `data:` payloads and `srcdoc` contents are not scanned.
pub fn collect_assets(
    doc: &WebDoc,
    port: &dyn AssetPort,
    cancel: &CancellationToken,
) -> Result<Vec<AssetRef>, WebError> {
    let mut c = Collector {
        doc,
        port,
        base_blocks: has_base_href(doc, cancel)?,
        limit: doc.limits().max_nodes,
        out: Vec::new(),
    };
    for id in 0..doc.len() {
        if id & 0x3ff == 0 {
            cancel.check()?;
        }
        let Some(node) = doc.node(id) else { continue };
        if node.kind != NodeKind::Element {
            continue;
        }
        // The element a browser creates (`<image>` in HTML content is an `img`).
        let tag = doc.local_name(id).unwrap_or_default();
        // The keyword is compared after the same character-reference decoding a browser applies (`&#114efresh`).
        let refresh = tag == "meta"
            && doc
                .attribute(id, "http-equiv")
                .and_then(|a| doc.attr_value(a))
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("refresh"));
        // SVG animation of an href (or of a url()-bearing presentation attribute) loads the values it animates to.
        let animated = if matches!(tag.as_str(), "set" | "animate") {
            animated_kind(doc, id)
        } else {
            None
        };
        for attr in &node.attrs {
            let name = doc.attr_name(attr).to_ascii_lowercase();
            let kind = if tag == "base" && name == "href" {
                Some(Kind::Base)
            } else if refresh && name == "content" {
                Some(Kind::Refresh)
            } else if let Some(kind) =
                animated.filter(|_| matches!(name.as_str(), "to" | "from" | "by" | "values"))
            {
                Some(if kind == Kind::Url && name == "values" {
                    Kind::UrlValues
                } else {
                    kind
                })
            } else {
                attr_kind(&tag, &name)
            };
            let (Some(kind), Some(value_span)) = (kind, attr.value_span.clone()) else {
                continue;
            };
            c.attribute(id, kind, &name, value_span)?;
        }
        if tag == "style" {
            c.style_children(id)?;
        }
    }
    Ok(c.out)
}
