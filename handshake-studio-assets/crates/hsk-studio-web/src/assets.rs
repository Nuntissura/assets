//! Granted asset resolution. The crate does not fetch, open or resolve anything itself: every reference found
//! in the source is classified, and only relative references reach the caller's `AssetPort`, which alone decides
//! what is granted. External, protocol-relative, `file:`, script and other scheme URLs are typed `Denied`
//! without the port being consulted (tests: `external_asset_denial_cancel_stale` plus the R5 regressions
//! `css_escapes_cannot_hide_external_urls`, `css_decoys_case_and_whitespace_follow_the_tokenizer`,
//! `character_references_cannot_hide_schemes`, `base_href_re_targets_every_relative_reference`,
//! `more_url_attributes_and_foreign_content_are_scanned`, `ambiguous_relative_paths_never_reach_the_port`, and the R6
//! regressions listed in `DESCRIPTOR` assets.proof).
//!
//! Where references come from: the carrier inventory (`carriers::CARRIERS`, one row per carrier with its specification
//! source and fixture); the scan dispatches through it (`carriers::attribute_path`, `carriers::animation_target`), so
//! every attribute, animation value, element text and CSS token the inventory lists is read by its row's path
//! (`carriers::tests::every_row_has_a_path_and_a_fixture`).
//!
//! Values are read the way a browser reads them before they are classified. The document comes from `dom::parse`,
//! whose tokenizer states and tree-construction context follow the HTML parser (or refuse the document typed), so
//! every `style` is RAWTEXT exactly where a browser makes it RAWTEXT. The texts the gate interprets are the texts the
//! parser hands on, not the raw span bytes (R9 tests `foreign_style_cdata_is_preprocessed_before_css`,
//! `text_consumers_read_the_text_a_browser_builds`, `raw_nul_in_url_attributes_reads_like_a_reference_to_it`; the
//! differential check `style-sheet-text`): attribute values are decoded with input-stream preprocessing, the
//! attribute-value U+0000 rule and the exact attribute-value character-reference rules (`charref`, complete WHATWG
//! table); style text is `WebDoc::sheet` (HTML: the RAWTEXT body preprocessed; foreign: text nodes and CDATA sections
//! under the foreign-content rules, preprocessed, concatenated); an animation `values` list is split on every `;` before
//! each piece is read (the engines' `ParseValues`); style text is then tokenized per CSS Syntax Level 3 (`cssurl`); a
//! `data:` URL is read by the Fetch data: URL processor (`datauri`) and, where its payload is CSS, scanned recursively.
//! Raw spans only locate a reference (the whole value or content span when decoding moved offsets; the outermost
//! `data:` URL for a reference inside its payload).
#![deny(clippy::arithmetic_side_effects)]

use crate::carriers::{self, ANIMATION_ELEMENTS, Load, Loads, Path};
use crate::charref::{Context, decode};
use crate::cssurl::{RefKind, css_references};
use crate::datauri::{self, DataUrl, Decision};
use crate::dom::{NodeId, NodeKind, WebDoc};
use crate::error::{LimitKind, WebError};
use hsk_studio_accord::CancellationToken;
use std::collections::HashMap;
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
    /// Port answered `Denied`, the reference carries control characters, it is a bad-url token, or it is a relative
    /// reference inside a `data:` payload (it resolves against the `data:` URL, which cannot be a base: a browser loads
    /// nothing, and the port is never asked).
    NotGranted,
    /// The document has a `<base href>` (any value, any position) or an `xml:base` attribute: it re-targets relative
    /// references, so none is offered to the port. The base itself is reported with this reason unless it is already
    /// denied for its scheme.
    BaseHref,
    /// An inline document or payload whose own references are not scanned: `<iframe srcdoc>`, `<script
    /// type=speculationrules>`, and a `data:` URL whose payload a browser would load as a document, script, text track,
    /// playlist or other resource that loads further URLs in that carrier (HTML, XML incl. SVG outside image carriers,
    /// scripts, JSON, unknown or sniffed types, or a CSS payload in an encoding this crate does not decode).
    InlineDocument,
    /// A relative path with a backslash or a percent-encoded dot, slash or backslash: browsers and file-system ports
    /// disagree on what it means.
    AmbiguousPath,
    /// A function inside a CSS URL function (`image-set()`, `src()`, `image()`, ...) that computes its value at style time
    /// (`var()`, `attr()`, `env()`, custom `--*()` functions, any unknown function): the URL is not in the source.
    ComputedUrl,
    /// A reference to the document itself (`#...`, the empty string or only URL-parser whitespace) from a carrier whose
    /// browser reads the document's own text as the resource: a style sheet (`link rel=stylesheet`, `@import`; quirks
    /// mode assumes `text/css`), a script (`script src`, svg `script href`), a text track or a manifest. The document
    /// would be parsed as CSS or executed as JavaScript and could load any URL from plain text (R12-F2). Every other
    /// carrier keeps `AssetOutcome::SameDocument`.
    SelfReference,
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
            Self::SelfReference => "self_reference",
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
    /// `data:` URI carried in the source itself, nothing to resolve: a payload that loads nothing further in its carrier,
    /// or a CSS payload whose own references follow it in the result (`AssetRef::data_depth` one higher).
    Inline,
    /// `#fragment`, or a value empty after the URL parser's whitespace removal: the document itself, in a carrier that
    /// does not read the document's text (`DenyReason::SelfReference` otherwise).
    SameDocument,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssetSource {
    /// Lower-cased attribute name.
    Attribute(String),
    InlineStyle,
    StyleBody,
    /// The text of a `<script type=speculationrules>` (URLs a browser prefetches or prerenders).
    ScriptBody,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetRef {
    pub node: NodeId,
    pub source: AssetSource,
    /// Decoded reference text as the port saw it (or would have).
    pub reference: String,
    /// Bytes in the source holding the reference (the whole value when decoding changed offsets; the outermost `data:`
    /// URL for a reference found inside a `data:` payload).
    pub span: Range<usize>,
    pub outcome: AssetOutcome,
    /// How many `data:` payloads the reference is inside: 0 = in the document itself.
    pub data_depth: u32,
}

/// `data:` payloads nested deeper than this refuse the scan `LimitExceeded(Depth)` (test
/// `data_payloads_beyond_the_bounds_are_refused_typed`).
pub const MAX_DATA_NESTING: u32 = 8;

/// HTML "ASCII whitespace" (space, tab, LF, FF, CR): what the attribute rules strip (`rel`, `http-equiv`, `attributeName`
/// values, `srcdoc` emptiness). Never Unicode White_Space: U+00A0, U+3000, U+2028 and the rest are ordinary characters.
fn is_ascii_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{c}' | '\r')
}

/// A value stripped of leading and trailing ASCII whitespace (the HTML attribute rules).
fn ws_trim(s: &str) -> &str {
    s.trim_matches(is_ascii_ws)
}

/// A value with no character except ASCII whitespace (never Unicode White_Space, R12-F3).
fn is_blank(s: &str) -> bool {
    ws_trim(s).is_empty()
}

/// The input of the URL Standard basic URL parser as it reads it (steps 1-2): leading and trailing C0 control or space
/// removed, then every ASCII tab or newline removed. Never Unicode white space (R12-F3): `&nbsp;` is the relative reference
/// `%C2%A0`, not nothing.
fn url_clean(reference: &str) -> String {
    reference
        .trim_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect()
}

/// The reference resolves to the document itself: empty after `url_clean`, or only a fragment (`#...`). A browser that
/// loads it fetches the document's own bytes (R12-F2).
fn resolves_to_document(reference: &str) -> bool {
    let cleaned = url_clean(reference);
    cleaned.is_empty() || cleaned.starts_with('#')
}

/// Classify without any I/O. `None` = relative: the port decides.
fn classify(reference: &str) -> Option<AssetOutcome> {
    // URL parsing drops tab/newline/CR and strips leading/trailing C0 controls and spaces (`url_clean`); mirror that so
    // `java\nscript:` cannot hide a scheme, and treat `\` as `/` for the authority check.
    let cleaned: String = url_clean(reference)
        .chars()
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

/// The document has a base URL carrier (`carriers` rows with `Path::Base`: `<base href>`, `xml:base`) anywhere.
fn has_base(doc: &WebDoc, cancel: &CancellationToken) -> Result<bool, WebError> {
    for id in 0..doc.len() {
        if id & 0x3ff == 0 {
            cancel.check()?;
        }
        let Some(node) = doc.node(id).filter(|n| n.kind == NodeKind::Element) else {
            continue;
        };
        let tag = doc.local_name(id).unwrap_or_default();
        if node.attrs.iter().any(|a| {
            matches!(
                carriers::attribute_path(&tag, &doc.attr_local_name(a)),
                Some((Path::Base, _))
            )
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The `;`-separated pieces of an animation `values` list (Blink `SVGAnimationElement::ParseValues`: split on every `;`,
/// each piece stripped of HTML whitespace; Gecko `SMILParserUtils::ParseValues` strips SVG whitespace, which differs only
/// in U+000C, CSS whitespace and a C0 control the URL parser strips; empty pieces dropped): (offset in `text`, piece).
fn value_pieces(text: &str) -> Vec<(usize, &str)> {
    let html_space = |c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}');
    let mut out = Vec::new();
    let mut from = 0usize;
    for part in text.split(';') {
        let start = from;
        from = from.saturating_add(part.len()).saturating_add(1);
        let lead = part
            .len()
            .saturating_sub(part.trim_start_matches(html_space).len());
        let item = part.trim_matches(html_space);
        if !item.is_empty() {
            out.push((start.saturating_add(lead), item));
        }
    }
    out
}

struct Collector<'a> {
    doc: &'a WebDoc,
    port: &'a dyn AssetPort,
    base_blocks: bool,
    limit: usize,
    /// Encoded bytes of `data:` payloads that may still be scanned (aggregate bound, `Limits::max_bytes`).
    data_budget: usize,
    out: Vec<AssetRef>,
    /// Element ids of the document (`id` attribute value -> elements), built when the first animated URL needs it.
    ids: Option<HashMap<String, Vec<NodeId>>>,
}

/// The load contexts of a `link` element's `href` from its `rel` and `as` attributes (decoded, first of each wins).
fn link_loads_of(doc: &WebDoc, id: NodeId) -> Loads {
    let value = |name: &str| doc.attribute(id, name).and_then(|a| doc.attr_value(a));
    carriers::link_loads(
        value("rel").unwrap_or_default().as_str(),
        value("as").as_deref(),
    )
}

/// Percent-decoding (URL Standard "percent-decode" of a fragment, UTF-8 decoded without failure) for the `id` lookup of an
/// animation's `href` target.
fn percent_decoded(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0usize;
    while let Some(&c) = b.get(i) {
        let hex = |k: usize| {
            b.get(k)
                .and_then(|h| char::from(*h).to_digit(16))
                .and_then(|d| u8::try_from(d).ok())
        };
        match (c, hex(i.saturating_add(1)), hex(i.saturating_add(2))) {
            (b'%', Some(h), Some(l)) => {
                out.push(h.saturating_mul(16).saturating_add(l));
                i = i.saturating_add(3);
            }
            _ => {
                out.push(c);
                i = i.saturating_add(1);
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Collector<'_> {
    /// The load contexts of the URL an SVG animation writes into attribute `name` (`href`, `xlink:href`, `src`, ...; R12-P1):
    /// those of the attribute on the element the animation targets. The target is the element its `href`/`xlink:href`
    /// names (`#id` in this document; every element with that id, also under percent-decoding) or, without one, its
    /// parent. `use href` animated to another fragment stays a same-document reference; a `script` target is a script
    /// load. A target that cannot be found, an attribute without a URL row on its element, or a base is read as the
    /// strictest applicable contexts, script and resource document together (fail closed).
    fn animated_loads(&mut self, animation: NodeId, name: &str) -> Loads {
        let strict = Loads::of(Load::Script).and(Load::Resource);
        let doc = self.doc;
        let by_href = ["href", "xlink:href"]
            .iter()
            .find_map(|n| doc.attribute(animation, n))
            .and_then(|a| doc.attr_value(a));
        let targets: Vec<NodeId> = match by_href {
            None => doc
                .node(animation)
                .and_then(|n| n.parent)
                .into_iter()
                .collect(),
            Some(href) => {
                let cleaned = url_clean(&href);
                let Some(fragment) = cleaned.strip_prefix('#') else {
                    return strict;
                };
                let ids = self.ids.get_or_insert_with(|| {
                    let mut map: HashMap<String, Vec<NodeId>> = HashMap::new();
                    for id in 0..doc.len() {
                        let Some(node) = doc.node(id).filter(|n| n.kind == NodeKind::Element)
                        else {
                            continue;
                        };
                        for a in &node.attrs {
                            if doc.attr_local_name(a) == "id"
                                && let Some(value) = doc.attr_value(a)
                            {
                                map.entry(value).or_default().push(id);
                            }
                        }
                    }
                    map
                });
                let mut found: Vec<NodeId> = ids.get(fragment).cloned().unwrap_or_default();
                let decoded = percent_decoded(fragment);
                if decoded != fragment {
                    found.extend(ids.get(&decoded).into_iter().flatten().copied());
                }
                found
            }
        };
        if targets.is_empty() {
            return strict;
        }
        let mut loads = Loads::EMPTY;
        for target in targets {
            let tag = doc.local_name(target).unwrap_or_default();
            loads = loads.union(match carriers::attribute_path(&tag, name) {
                Some((Path::Url, _)) if tag == "link" && name == "href" => {
                    link_loads_of(doc, target)
                }
                Some((Path::Url, load)) => Loads::of(load),
                _ => strict,
            });
        }
        loads
    }

    /// `nested`: the reference is inside a `data:` payload. `loads`: what the carrier's browser does with the fetched
    /// resource, decided before the reference's scheme or a payload's type (`carriers::Load`).
    fn decide(&self, reference: &str, bad: bool, nested: bool, loads: Loads) -> AssetOutcome {
        // A reference to the document itself (empty after the URL parser's stripping, or only a fragment) is harmless in
        // a navigation, image, media or SVG reference carrier. Where the browser reads the document's own text as the
        // resource (style sheet: quirks mode assumes text/css; script: no MIME check; text track; manifest) it parses or
        // executes the document and loads whatever URLs that text spells (R12-F2). Inside a `data:` payload a
        // fragment-only reference is the payload itself.
        if !nested && !bad && resolves_to_document(reference) {
            return if loads.reads_text() {
                AssetOutcome::Denied(DenyReason::SelfReference)
            } else {
                AssetOutcome::SameDocument
            };
        }
        if let Some(done) = classify(reference) {
            return done;
        }
        if bad || nested || reference.chars().any(char::is_control) {
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
        source: &AssetSource,
        reference: String,
        span: Range<usize>,
        outcome: AssetOutcome,
        data_depth: u32,
    ) -> Result<(), WebError> {
        if self.out.len() >= self.limit {
            return Err(WebError::LimitExceeded {
                limit: LimitKind::Nodes,
                span,
            });
        }
        self.out.push(AssetRef {
            node,
            source: source.clone(),
            reference,
            span,
            outcome,
            data_depth,
        });
        Ok(())
    }

    /// One URL value of a carrier: a value that is the empty string fetches nothing in any carrier (HTML: no request for
    /// `src=""`, `href=""`); in a carrier that reads the document's text it is still denied as a self-reference (lead
    /// decision R12: fail closed). Any other value goes through `reference`, whitespace-only included (it resolves to the
    /// document itself).
    fn url_value(
        &mut self,
        node: NodeId,
        source: &AssetSource,
        text: &str,
        span: Range<usize>,
        loads: Loads,
    ) -> Result<(), WebError> {
        if text.is_empty() {
            if loads.reads_text() {
                self.push(
                    node,
                    source,
                    String::new(),
                    span,
                    AssetOutcome::Denied(DenyReason::SelfReference),
                    0,
                )?;
            }
            return Ok(());
        }
        self.reference(node, source, text, false, span, loads, 0)
    }

    /// A URL handed to a legacy plugin (`param value`, `Path::PluginParam`): nothing the gate can read loads it, so it is
    /// reported and denied. External, protocol-relative, `file:` and script schemes keep their typed reason, a `data:`
    /// payload is `InlineDocument`, a relative reference is `NotGranted` and never offered to the port.
    fn plugin_url(
        &mut self,
        node: NodeId,
        source: &AssetSource,
        text: &str,
        span: Range<usize>,
    ) -> Result<(), WebError> {
        if text.is_empty() {
            return Ok(());
        }
        let outcome = if resolves_to_document(text) {
            AssetOutcome::SameDocument
        } else {
            match classify(text) {
                Some(AssetOutcome::Inline) => AssetOutcome::Denied(DenyReason::InlineDocument),
                Some(done) => done,
                None => AssetOutcome::Denied(DenyReason::NotGranted),
            }
        };
        self.push(node, source, text.to_owned(), span, outcome, 0)
    }

    /// One reference read from a carrier whose load contexts are `loads`, `depth` `data:` payloads deep.
    #[allow(
        clippy::too_many_arguments,
        reason = "one reference: where it is, what it is, how it is loaded"
    )]
    fn reference(
        &mut self,
        node: NodeId,
        source: &AssetSource,
        reference: &str,
        bad: bool,
        span: Range<usize>,
        loads: Loads,
        depth: u32,
    ) -> Result<(), WebError> {
        let outcome = self.decide(reference, bad, depth > 0, loads);
        // A bad-url token or bad string spelling `data:` loads nothing and its payload is cut short: denied, never read
        // (found by the differential generator, gen:Css:70041).
        if bad && outcome == AssetOutcome::Inline {
            return self.push(
                node,
                source,
                reference.to_owned(),
                span,
                AssetOutcome::Denied(DenyReason::NotGranted),
                depth,
            );
        }
        if outcome == AssetOutcome::Inline
            && let Some(url) = datauri::parse(reference)
        {
            match datauri::decide_all(loads, &url) {
                Decision::Inline => {}
                Decision::Deny => {
                    return self.push(
                        node,
                        source,
                        reference.to_owned(),
                        span,
                        AssetOutcome::Denied(DenyReason::InlineDocument),
                        depth,
                    );
                }
                Decision::ScanCss => {
                    return self.data_css(node, source, reference, &url, span, depth);
                }
            }
        }
        self.push(node, source, reference.to_owned(), span, outcome, depth)
    }

    /// A `data:` payload read as CSS: bounded by nesting (`MAX_DATA_NESTING`) and by the aggregate encoded size of every
    /// scanned payload (`Limits::max_bytes`), both checked before anything is decoded and refused typed; a payload the
    /// crate cannot decode (failed base64, an encoding it does not implement) is denied.
    fn data_css(
        &mut self,
        node: NodeId,
        source: &AssetSource,
        reference: &str,
        url: &DataUrl<'_>,
        span: Range<usize>,
        depth: u32,
    ) -> Result<(), WebError> {
        let inner = depth.saturating_add(1);
        if inner > MAX_DATA_NESTING {
            return Err(WebError::LimitExceeded {
                limit: LimitKind::Depth,
                span,
            });
        }
        let cost = url.encoded_len();
        if cost > self.data_budget {
            return Err(WebError::LimitExceeded {
                limit: LimitKind::Bytes,
                span,
            });
        }
        self.data_budget = self.data_budget.saturating_sub(cost);
        let text = url
            .body()
            .and_then(|bytes| datauri::css_text(&bytes, url.charset.as_deref()));
        let Some(text) = text else {
            return self.push(
                node,
                source,
                reference.to_owned(),
                span,
                AssetOutcome::Denied(DenyReason::InlineDocument),
                depth,
            );
        };
        self.push(
            node,
            source,
            reference.to_owned(),
            span.clone(),
            AssetOutcome::Inline,
            depth,
        )?;
        self.css(node, source, &text, &|_| span.clone(), inner)
    }

    /// References in CSS `css` (the text a browser's CSS parser reads); `locate` maps a range of `css` to the source bytes
    /// it came from (location only). The `@import` URL is a style sheet, every other URL a CSS value (the most
    /// restrictive CSS load context, `Load::Resource`). Blocks nested deeper than `cssurl::MAX_NESTING` refuse the scan
    /// typed (`LimitExceeded(Depth)`; test `css_nesting_beyond_the_limit_is_refused_typed`), so no reference inside them
    /// goes unseen.
    fn css(
        &mut self,
        node: NodeId,
        source: &AssetSource,
        css: &str,
        locate: &dyn Fn(&Range<usize>) -> Range<usize>,
        depth: u32,
    ) -> Result<(), WebError> {
        let room = self.limit.saturating_sub(self.out.len()).saturating_add(1);
        let found = css_references(css, room).map_err(|span| WebError::LimitExceeded {
            limit: LimitKind::Depth,
            span: locate(&span),
        })?;
        for r in found {
            let loads = Loads::of(if r.import {
                Load::Stylesheet
            } else {
                Load::Resource
            });
            match r.kind {
                RefKind::Url => {
                    self.reference(node, source, &r.value, false, locate(&r.span), loads, depth)?
                }
                RefKind::Bad => {
                    self.reference(node, source, &r.value, true, locate(&r.span), loads, depth)?
                }
                RefKind::Computed => self.push(
                    node,
                    source,
                    r.value,
                    locate(&r.span),
                    AssetOutcome::Denied(DenyReason::ComputedUrl),
                    depth,
                )?,
            }
        }
        Ok(())
    }

    /// An attribute read by `route`: its path and load context from the carrier inventory, and for
    /// `Path::AnimationValue(s)` how the animation target reads values.
    fn attribute(
        &mut self,
        node: NodeId,
        name: &str,
        value_span: Range<usize>,
        route: Route,
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
        let Route {
            path,
            loads,
            target,
        } = route;
        match (path, target) {
            (Path::Url, _) | (Path::AnimationValue, Some(Path::Url)) => {
                self.url_value(node, &source, text, value_span.clone(), loads)?;
            }
            (Path::Base, _) => {
                // A base the URL parser reads as nothing (ASCII whitespace only) re-targets nothing; any other value,
                // Unicode white space included, is a relative URL against the document (R12-F3).
                if !url_clean(text).is_empty() {
                    let outcome = match classify(text) {
                        Some(AssetOutcome::Denied(reason)) => AssetOutcome::Denied(reason),
                        _ => AssetOutcome::Denied(DenyReason::BaseHref),
                    };
                    self.push(
                        node,
                        &source,
                        text.to_owned(),
                        value_span.clone(),
                        outcome,
                        0,
                    )?;
                }
            }
            (Path::AnimationValues, Some(Path::Url)) => {
                for (at, item) in value_pieces(text) {
                    let r = at..at.saturating_add(item.len());
                    self.url_value(node, &source, item, sub(&r), loads)?;
                }
            }
            // R11-F1: each piece of a values list is a separate CSS value for the browser (a comment, string or bad url
            // opened in one piece ends at the `;`).
            (Path::AnimationValues, Some(Path::Css)) => {
                for (at, item) in value_pieces(text) {
                    let piece = |r: &Range<usize>| {
                        sub(&(at.saturating_add(r.start)..at.saturating_add(r.end)))
                    };
                    self.css(node, &source, item, &piece, 0)?;
                }
            }
            (Path::Css, _) | (Path::AnimationValue, Some(Path::Css)) => {
                self.css(node, &source, text, &sub, 0)?
            }
            (Path::UrlList, _) => {
                for r in tokens(text) {
                    self.reference(node, &source, &text[r.clone()], false, sub(&r), loads, 0)?;
                }
            }
            (Path::Srcset, _) => {
                for r in srcset_urls(text) {
                    self.reference(node, &source, &text[r.clone()], false, sub(&r), loads, 0)?;
                }
            }
            (Path::Refresh, _) => {
                if let Some(r) = refresh_url(text) {
                    self.url_value(node, &source, &text[r.clone()], sub(&r), loads)?;
                }
            }
            (Path::PluginParam, _) => {
                self.plugin_url(node, &source, text, value_span.clone())?;
            }
            (Path::InlineDoc | Path::MetaLink, _) if !is_blank(text) => {
                self.push(
                    node,
                    &source,
                    text.to_owned(),
                    value_span.clone(),
                    AssetOutcome::Denied(DenyReason::InlineDocument),
                    0,
                )?;
            }
            // No animation target, nothing loaded, or a CSS-only path (never an attribute row).
            _ => {}
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
        self.css(
            id,
            &AssetSource::StyleBody,
            &sheet.text,
            &|r| sheet.span(r),
            0,
        )
    }

    /// `<script type=speculationrules>` (HTML 7.6): its JSON lists URLs a browser prefetches or prerenders; the rules are
    /// not parsed here, so the whole text is denied as an inline document.
    fn speculation_rules(&mut self, id: NodeId) -> Result<(), WebError> {
        let is_rules = self
            .doc
            .attribute(id, "type")
            .and_then(|a| self.doc.attr_value(a))
            .is_some_and(|t| {
                t.trim_matches(|c: char| c.is_ascii_whitespace())
                    .eq_ignore_ascii_case("speculationrules")
            });
        if !is_rules {
            return Ok(());
        }
        let Some(node) = self.doc.node(id) else {
            return Ok(());
        };
        // The script's child text: the script-data body of an HTML script (preprocessed, no references), the text nodes
        // and CDATA sections of a foreign one.
        let text: String = node
            .children
            .iter()
            .filter_map(|&c| {
                self.doc
                    .raw_text_data(c)
                    .map(|d| d.text)
                    .or_else(|| self.doc.text_value(c))
            })
            .collect();
        if is_blank(&text) {
            return Ok(());
        }
        let span = node.content_span().unwrap_or_else(|| node.span.clone());
        self.push(
            id,
            &AssetSource::ScriptBody,
            text,
            span,
            AssetOutcome::Denied(DenyReason::InlineDocument),
            0,
        )
    }
}

/// How one attribute is read.
#[derive(Clone, Copy)]
struct Route {
    path: Path,
    /// What the carrier's browser does with the fetched resource (`carriers::Load`; several for a `link` with several
    /// keywords or an animated URL with several candidate targets).
    loads: Loads,
    /// For `Path::AnimationValue(s)`: how the animation's target reads values.
    target: Option<Path>,
}

/// The `meta` pragma directive (`http-equiv`, ASCII case-insensitive, after the same character-reference decoding a
/// browser applies) the gate reads: `refresh` is the HTML Standard's, `link` the legacy engine-only one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HttpEquiv {
    Refresh,
    Link,
}

/// Every asset-bearing reference in the document, in document order, with the decision for each. The carriers are the
/// rows of the carrier inventory (`carriers::CARRIERS`, each with its specification source and a fixture proving its
/// path: `carriers::tests::every_row_has_a_path_and_a_fixture`): URL attributes (`href`/`xlink:href` on any element,
/// `src` on `img image source video audio iframe frame embed track input script`, `poster`, `object
/// data/codebase/classid/archive`, `form action`, `formaction`, `cite`, `ping`, `attributionsrc`,
/// `srcset`/`imagesrcset` candidates, obsolete `background longdesc lowsrc manifest profile`, `<meta
/// http-equiv=refresh>` URL), `<base href>` and `xml:base`, `iframe srcdoc` and `<script type=speculationrules>`
/// (reported denied), CSS text (`style` attributes, SVG presentation attributes `fill stroke clip-path cursor filter mask
/// marker-*`, `<style>` contents), and the `to from by values` of SVG `set animate animateMotion animateTransform
/// animateColor` whose `attributeName` is `href`, `xlink:href`, `src` or any CSS property of the inventory (incl. the
/// `marker` shorthand; `values` split on every `;` like the engines: tests `r11_f1_animation_values_are_split_like_the_engines`,
/// `r11_f2_every_css_property_is_an_animation_target`). In CSS text every URL a browser loads: url tokens, the strings
/// of the inventory's URL functions and `@import`, bad-url tokens (denied) and computed URLs (`Denied(ComputedUrl)`),
/// read by the CSS Syntax 3 tokenizer in `cssurl` (tests: `tests/css_url_gate.rs`); `@namespace` URLs are not loaded and
/// not reported. A `data:` URL is decided by its type and the carrier's load context (`datauri`): CSS payloads (and, in
/// style sheet carriers, every type a browser may read as CSS) are scanned recursively, at most `MAX_DATA_NESTING` deep
/// and `Limits::max_bytes` encoded bytes in total (refused typed beyond either); payloads that load further URLs as
/// documents, scripts, tracks or playlists are `Denied(InlineDocument)`; the rest are `Inline` (tests: `data_*` in
/// `tests/css_url_gate.rs`). `srcdoc` and speculation rules contents and script text are not scanned.
pub fn collect_assets(
    doc: &WebDoc,
    port: &dyn AssetPort,
    cancel: &CancellationToken,
) -> Result<Vec<AssetRef>, WebError> {
    let mut c = Collector {
        doc,
        port,
        base_blocks: has_base(doc, cancel)?,
        limit: doc.limits().max_nodes,
        data_budget: doc.limits().max_bytes,
        out: Vec::new(),
        ids: None,
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
        // The keyword is compared after the same character-reference decoding a browser applies (`&#114efresh`), with the
        // HTML attribute rules' ASCII whitespace stripping (a browser that does not strip reads fewer: over-reporting).
        let http_equiv = if tag == "meta" {
            doc.attribute(id, "http-equiv")
                .and_then(|a| doc.attr_value(a))
                .and_then(|v| match ws_trim(&v).to_ascii_lowercase().as_str() {
                    "refresh" => Some(HttpEquiv::Refresh),
                    "link" => Some(HttpEquiv::Link),
                    _ => None,
                })
        } else {
            None
        };
        // `param value` is a URL only for the names a legacy plugin loads (`carriers::PLUGIN_URL_PARAMS`).
        let plugin_param = tag == "param"
            && doc
                .attribute(id, "name")
                .and_then(|a| doc.attr_value(a))
                .is_some_and(|n| {
                    carriers::PLUGIN_URL_PARAMS.contains(&ws_trim(&n).to_ascii_lowercase().as_str())
                });
        // An SVG animation writes from/to/by/values into the attribute or property named by attributeName.
        let animated = if ANIMATION_ELEMENTS.contains(&tag.as_str()) {
            doc.attribute(id, "attributename")
                .and_then(|a| doc.attr_value(a))
                .map(|t| ws_trim(&t).to_ascii_lowercase())
                .and_then(|name| carriers::animation_target(&name).map(|t| (name, t)))
        } else {
            None
        };
        // The load contexts of an animated URL: those of the attribute on the element it targets.
        let animated_url_loads = match &animated {
            Some((name, Path::Url)) => Some(c.animated_loads(id, name)),
            _ => None,
        };
        for attr in &node.attrs {
            let name = doc.attr_local_name(attr);
            let Some(value_span) = attr.value_span.clone() else {
                continue;
            };
            if let Some(path) = carriers::animation_path(&tag, &name) {
                if let Some((_, target)) = &animated {
                    let loads = animated_url_loads.unwrap_or(Loads::of(Load::Resource));
                    let route = Route {
                        path,
                        loads,
                        target: Some(*target),
                    };
                    c.attribute(id, &name, value_span, route)?;
                }
                continue;
            }
            // The first row naming the element and attribute whose gate holds (specific rows come first; `meta content`
            // has a refresh row and a link row, `param value` is read only for the plugin URL names).
            let row = carriers::attribute_paths(&tag, &name).find(|(path, _)| match path {
                Path::Refresh => http_equiv == Some(HttpEquiv::Refresh),
                Path::MetaLink => http_equiv == Some(HttpEquiv::Link),
                Path::PluginParam => plugin_param,
                _ => true,
            });
            if let Some((path, load)) = row {
                // `link`: the `rel` keywords and `as` decide (R12-P3); every other carrier has its row's context.
                let loads = if tag == "link" && name == "href" {
                    link_loads_of(doc, id)
                } else {
                    Loads::of(load)
                };
                let route = Route {
                    path,
                    loads,
                    target: None,
                };
                c.attribute(id, &name, value_span, route)?;
            }
        }
        if tag == "style" {
            c.style_children(id)?;
        }
        if tag == "script" {
            c.speculation_rules(id)?;
        }
    }
    Ok(c.out)
}
