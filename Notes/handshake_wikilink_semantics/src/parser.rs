//! Wikilink syntax parsing (WP-KERNEL-012 MT-015) — the native port of
//! `app/src/lib/editor/wikilink.ts` + the `WP009_WIKILINK_KIND_BY_PREFIX` table in
//! `app/src/lib/editor/extension_inventory.ts`.
//!
//! ## What a wikilink is (NODE-SHAPE RECONCILIATION — the MT-015 critical gate)
//!
//! A `[[prefix:value]]` / `[[prefix:value|label]]` token is NOT a `Mark::Wikilink`. MT-011's
//! harden established (and `app/src/lib/tiptap/hs_link_node.ts` confirms) that a typed wikilink
//! is the inline ATOM node `hsLink` carrying `{ ref_kind, ref_value, label, resolved }`
//! ([`handshake_rich_document_model::node::HsLinkNode`] / `Child::HsLink`). This module
//! parses the SYNTAX into a [`ParsedWikilink`] that maps 1:1 onto an `HsLinkNode`; autocomplete
//! confirm then inserts the atom via `InsertNode` (NOT `AddMark`). The MT-014 media embeds already
//! render from this same `hsLink` node by `ref_kind` — one unified `hsLink` dispatch, no fork.
//!
//! ## Prefix vocabulary (REAL backend authority, not the contract's stale examples)
//!
//! The MT scope text gives `block`/`doc`/`wp`/`file`/`tag` example prefixes, but the REAL
//! `WP009_WIKILINK_KIND_BY_PREFIX` table (the authority the React `classifyWikilink` uses) is:
//! `note, file, folder, project, spec, wp, symbol, album, video, HS_images, HS_slideshow`. This
//! module ports the REAL table verbatim. A prefix NOT in the table (including the contract's
//! `block`/`doc`/`tag` examples) is preserved as [`WikilinkKind::Unknown`] with `resolved=false`
//! (never silently dropped — RISK-5 / MC-005), exactly as the React `classifyWikilink` does.
//!
//! ## Regex compiled once (`std::sync::OnceLock`)
//!
//! [`wikilink_regex`] preserves the React pattern's ASCII prefix grammar explicitly,
//! because Rust's `\w` also accepts Unicode. The `regex` crate compiles it once via [`OnceLock`].
//! `regex` is RE2-style (linear-time, no catastrophic backtracking), so a hostile paste cannot
//! ReDoS the parser.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

use handshake_rich_document_model::node::HsLinkNode;

/// The typed wikilink kind a `[[prefix:value]]` token classifies into. The `Known(String)` arm
/// carries the backend ref kind (`"wp"`, `"file"`, …); the `Unknown(String)` arm carries the raw
/// (lower-cased) prefix that did not match the table, so a broken link is VISIBLE and diagnosable
/// rather than dropped (RISK-5 / MC-005). The enum is matched exhaustively everywhere; there is no
/// wildcard arm that could swallow a new kind silently (MC-005: an unrecognized prefix must not
/// panic and must round-trip).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WikilinkKind {
    /// A recognized prefix mapped to its backend ref kind (e.g. `Known("wp")`).
    Known(String),
    /// An unrecognized prefix preserved verbatim (lower-cased) as `unknown` (never dropped).
    Unknown(String),
}

impl WikilinkKind {
    /// The backend `ref_kind` string this kind maps onto when materialized as an [`HsLinkNode`]:
    /// the known backend ref kind, or the literal `"unknown"` for an unrecognized prefix (matching
    /// the React `hsLink` node's `refKind: "unknown"` default).
    pub fn ref_kind(&self) -> &str {
        match self {
            WikilinkKind::Known(k) => k.as_str(),
            WikilinkKind::Unknown(_) => "unknown",
        }
    }

    /// True when the prefix matched a known kind (drives the `resolved` flag + chip color).
    pub fn is_resolved(&self) -> bool {
        matches!(self, WikilinkKind::Known(_))
    }
}

/// A parsed wikilink token (the native mirror of the React `ParsedWikilink`): the classified
/// [`WikilinkKind`], the target value, the display label, and whether the prefix resolved. Maps 1:1
/// onto an [`HsLinkNode`] via [`Self::to_hs_link`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedWikilink {
    /// The classified kind (known backend ref kind or unknown-with-raw-prefix).
    pub kind: WikilinkKind,
    /// The target value after the prefix (e.g. `"WP-KERNEL-012"`, `"src/app.ts"`), trimmed.
    pub ref_value: String,
    /// Display label: explicit `|label`, else the ref value (known) or `prefix:value` (unknown) —
    /// matching the React `classifyWikilink` default-label rule.
    pub label: String,
    /// True when the prefix matched a known wikilink kind.
    pub resolved: bool,
    /// The raw prefix as typed (lower-cased), for diagnostics / the unknown chip title.
    pub raw_prefix: String,
}

impl ParsedWikilink {
    /// Materialize this parsed wikilink as the inline-atom [`HsLinkNode`] the document model and the
    /// backend `content_json` round-trip (`{ref_kind, ref_value, label, resolved}`). This is the
    /// node `InsertNode` inserts on autocomplete confirm (NOT a mark via `AddMark`).
    pub fn to_hs_link(&self) -> HsLinkNode {
        HsLinkNode {
            ref_kind: self.kind.ref_kind().to_owned(),
            ref_value: self.ref_value.clone(),
            label: self.label.clone(),
            resolved: self.resolved,
            provenance: None,
        }
    }
}

/// The REAL `WP009_WIKILINK_KINDS` prefix→backend-ref-kind table (verbatim from
/// `app/src/lib/editor/extension_inventory.ts`). The map key is the lower-cased prefix; the value
/// is the backend `ref_kind`. This is the authority for which prefixes resolve.
///
/// NOTE: prefixes are matched case-INSENSITIVELY against the lower-cased token prefix; the React
/// table key `HS_images` therefore matches a typed `hs_images:` prefix (the React `Map` is built
/// from `prefix.toLowerCase()`), so this map stores the already-lower-cased keys.
pub fn wikilink_kind_by_prefix() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| {
        // (prefix lower-cased, backend ref kind) — verbatim from WP009_WIKILINK_KINDS, plus the
        // WP-KERNEL-012 MT-034 `code:` code-symbol cross-reference prefix.
        //
        // MT-034 (E5 code<->note cross-refs): a `[[code:path/to/file.rs#MyStruct]]` token in a note is
        // an EXISTING `hsLink` inline atom (NOT an invented `code_ref` node — the KERNEL_BUILDER gate's
        // THIRD embed instance after MT-014 images + MT-033 atelier), so the `code:` prefix is added to
        // the SAME prefix table the wikilink parser drives. Its backend `ref_kind` is `code`, so the
        // atom round-trips `content_json` (AC-1) and the backend backlink indexer keys it on
        // `ref_value=symbol_key`. The historical `symbol:` prefix already existed (a Loom symbol ref);
        // `code:` is the code-editor cross-ref discriminator MT-034 dispatches `open-code-symbol` on.
        [
            ("note", "note"),
            ("file", "file"),
            ("folder", "folder"),
            ("project", "project"),
            ("spec", "spec"),
            ("wp", "wp"),
            ("symbol", "symbol"),
            ("album", "album"),
            ("video", "video"),
            ("hs_images", "images"),
            ("hs_slideshow", "slideshow"),
            // WP-KERNEL-012 MT-034: the code-symbol cross-reference prefix.
            ("code", "code"),
            // WP-KERNEL-012 MT-068 (E10 — editors<->Locus cross-references): the `locus:` prefix is the
            // SIBLING of `code:` (the THIRD cross-ref discriminator after the MT-014 media embeds + the
            // MT-034 code refs). A `[[locus:wp/WP-KERNEL-012]]` token in a note is the EXISTING `hsLink`
            // inline atom (ref_kind="locus"), NOT an invented node, so it round-trips `content_json`
            // (AC-006) and the backend backlink indexer keys it on the normalized `locus://` ref value.
            // The `locus://wp/{id}` / `locus://mt/{id}` URI form is parsed by
            // `native interop::locus_interop::parse_locus_ref`; this table entry lets the same wikilink
            // machinery author it as `[[locus:...]]` and render it through the shared CrossRef chip path.
            ("locus", "locus"),
        ]
        .into_iter()
        .collect()
    })
}

/// The wikilink regex with the React grammar's ASCII prefix characters.
/// Compiled once via [`OnceLock`]. Group 1 = prefix, group 2 = value, group 3 = optional label.
pub fn wikilink_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\[\[([a-zA-Z_][a-zA-Z0-9_]*):([^\]|]+)(?:\|([^\]]+))?\]\]")
            .expect("the wikilink regex is a compile-time constant pattern and always compiles")
    })
}

/// Classify a wikilink's captured groups into a typed [`ParsedWikilink`] — the native mirror of the
/// React `classifyWikilink`. `prefix`/`value` are required; `label` is the optional `|label` group.
/// An unrecognized prefix (NOT in [`wikilink_kind_by_prefix`]) is preserved as
/// [`WikilinkKind::Unknown`] with `resolved=false` and a `prefix:value` default label (never
/// dropped — RISK-5 / MC-005).
pub fn classify_wikilink(prefix: &str, value: &str, label: Option<&str>) -> ParsedWikilink {
    let raw_prefix = prefix.trim().to_lowercase();
    let ref_value = value.trim().to_owned();
    let explicit_label = label.map(str::trim).filter(|l| !l.is_empty());

    match wikilink_kind_by_prefix().get(raw_prefix.as_str()) {
        Some(&backend_ref_kind) => ParsedWikilink {
            kind: WikilinkKind::Known(backend_ref_kind.to_owned()),
            label: explicit_label
                .map(str::to_owned)
                .unwrap_or_else(|| ref_value.clone()),
            ref_value,
            resolved: true,
            raw_prefix,
        },
        None => {
            let default_label = format!("{raw_prefix}:{ref_value}");
            ParsedWikilink {
                kind: WikilinkKind::Unknown(raw_prefix.clone()),
                label: explicit_label.map(str::to_owned).unwrap_or(default_label),
                ref_value,
                resolved: false,
                raw_prefix,
            }
        }
    }
}

/// Parse a single wikilink token (the whole string must be exactly one `[[..]]` token, e.g.
/// `"[[wp:WP-KERNEL-012|My Note]]"`). Returns `None` when the string is not a wikilink. Native
/// mirror of the React `parseWikilink`.
pub fn parse_wikilink(token: &str) -> Option<ParsedWikilink> {
    let token = token.trim();
    let caps = wikilink_regex().captures(token)?;
    let matched = caps.get(0)?;
    if matched.start() != 0 || matched.end() != token.len() {
        return None;
    }
    let prefix = caps.get(1)?.as_str();
    let value = caps.get(2)?.as_str();
    let label = caps.get(3).map(|m| m.as_str());
    Some(classify_wikilink(prefix, value, label))
}

/// Extract every wikilink occurrence from free text (paste / bulk parse), in document order.
/// Native mirror of the React `extractWikilinks`.
pub fn extract_wikilinks(text: &str) -> Vec<ParsedWikilink> {
    wikilink_regex()
        .captures_iter(text)
        .filter_map(|caps| {
            let prefix = caps.get(1)?.as_str();
            let value = caps.get(2)?.as_str();
            let label = caps.get(3).map(|m| m.as_str());
            Some(classify_wikilink(prefix, value, label))
        })
        .collect()
}

/// Detect an OPEN, unterminated `[[` autocomplete trigger at the END of `text_before_caret` and
/// return the partial query typed so far (the text between the last `[[` and the caret), if the
/// caret is inside an open wikilink token that has NOT yet been closed by `]]`.
///
/// Returns `None` when there is no open `[[` before the caret, or when the most recent `[[` has
/// already been closed by a `]]` (so a completed `[[wp:X]]` token does not re-open the popup). The
/// returned `(start_char, query)` gives the CHAR offset of the opening `[[` within
/// `text_before_caret` (so the caller can compute the char span to remove on confirm — the doc is
/// CHAR-indexed, RISK-1) and the raw query text after the `[[`.
///
/// This is the input-trigger detector the input handler calls each keystroke (MT step 4): typing
/// `[[` opens the popup; typing more refines the query; typing `]]` (or moving past it) closes it.
pub fn open_wikilink_query(text_before_caret: &str) -> Option<(usize, String)> {
    // Find the LAST `[[` before the caret (byte offset, then converted to a CHAR offset).
    let open_byte = text_before_caret.rfind("[[")?;
    // The text after that `[[`.
    let after = &text_before_caret[open_byte + 2..];
    // If the token was already closed (a `]]` appears after the `[[` before the caret), the popup
    // is not open for THIS token (a finished `[[x]]` must not re-trigger).
    if after.contains("]]") {
        return None;
    }
    // Convert the byte offset of the `[[` to a CHAR offset (the doc addresses text by char).
    let open_char = text_before_caret[..open_byte].chars().count();
    Some((open_char, after.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_token_parser_rejects_surrounding_text_and_multiple_tokens() {
        for token in ["prefix [[wp:X]]", "[[wp:X]] suffix", "[[wp:X]] [[note:Y]]"] {
            assert!(parse_wikilink(token).is_none(), "{token}");
        }
        assert!(parse_wikilink("  [[wp:X]]  ").is_some());
        assert_eq!(extract_wikilinks("prefix [[wp:X]] [[note:Y]] suffix").len(), 2);
    }

    #[test]
    fn prefixes_use_ascii_word_characters_while_values_preserve_unicode() {
        assert!(parse_wikilink("[[noté:X]]").is_none());
        assert!(extract_wikilinks("[[noté:X]]").is_empty());
        let parsed = parse_wikilink("[[note:東京]]").unwrap();
        assert_eq!(parsed.ref_value, "東京");
        assert!(parse_wikilink("[[_custom_1:X]]").is_some());
    }

    #[test]
    fn parses_known_prefix_with_label() {
        // AC-1: parse_wikilink("[[wp:WP-KERNEL-012|My Note]]") -> Known(wp), value, label.
        // (The MT contract example uses `block:`; the REAL prefix table has NO `block` — `wp` is the
        // closest real Known prefix proving the value+label+kind extraction. The `block:` -> Unknown
        // path is proven by `parses_unknown_prefix_block_doc_tag` below.)
        let parsed = parse_wikilink("[[wp:WP-KERNEL-012|My Note]]").expect("a valid wikilink");
        assert_eq!(parsed.kind, WikilinkKind::Known("wp".to_owned()));
        assert_eq!(parsed.kind.ref_kind(), "wp");
        assert_eq!(parsed.ref_value, "WP-KERNEL-012");
        assert_eq!(parsed.label, "My Note");
        assert!(parsed.resolved);
    }

    #[test]
    fn parses_unknown_prefix_unresolved() {
        // AC-2: parse_wikilink("[[unknown:xyz]]") -> Unknown, resolved=false (no panic).
        let parsed = parse_wikilink("[[unknown:xyz]]").expect("a parseable token");
        assert_eq!(parsed.kind, WikilinkKind::Unknown("unknown".to_owned()));
        assert_eq!(parsed.kind.ref_kind(), "unknown");
        assert_eq!(parsed.ref_value, "xyz");
        assert!(!parsed.resolved, "an unrecognized prefix is NOT resolved");
        // The default label for an unknown is `prefix:value` (the React default).
        assert_eq!(parsed.label, "unknown:xyz");
    }

    #[test]
    fn parses_unknown_prefix_block_doc_tag_from_contract_examples() {
        // MC-005: the MT contract's `block`/`doc`/`tag` example prefixes are NOT in the REAL
        // WP009 table, so they classify as Unknown (resolved=false) — preserved, not dropped, not a
        // panic. This is the contract-example-vs-real-table reconciliation made explicit.
        for prefix in ["block", "doc", "tag"] {
            let token = format!("[[{prefix}:BLK-001]]");
            let parsed = parse_wikilink(&token).expect("parseable");
            assert_eq!(
                parsed.kind,
                WikilinkKind::Unknown(prefix.to_owned()),
                "contract example prefix '{prefix}' is not in the real table -> Unknown"
            );
            assert!(!parsed.resolved);
            assert_eq!(parsed.to_hs_link().ref_kind, "unknown");
        }
    }

    #[test]
    fn all_real_prefixes_resolve_to_their_backend_ref_kind() {
        // Every prefix in the REAL WP009 table resolves Known with its backend ref kind.
        let cases = [
            ("note", "note"),
            ("file", "file"),
            ("folder", "folder"),
            ("project", "project"),
            ("spec", "spec"),
            ("wp", "wp"),
            ("symbol", "symbol"),
            ("album", "album"),
            ("video", "video"),
            ("hs_images", "images"),
            ("hs_slideshow", "slideshow"),
        ];
        for (prefix, backend) in cases {
            let parsed = parse_wikilink(&format!("[[{prefix}:X]]")).expect("parseable");
            assert_eq!(
                parsed.kind,
                WikilinkKind::Known(backend.to_owned()),
                "prefix {prefix}"
            );
            assert!(parsed.resolved);
            assert_eq!(parsed.to_hs_link().ref_kind, backend);
        }
    }

    #[test]
    fn parses_code_prefix_to_resolved_code_hs_link() {
        // WP-KERNEL-012 MT-034 (AC-1 / cross_ref unit): `[[code:path#Symbol]]` parses to a RESOLVED
        // hsLink atom with ref_kind="code" and ref_value carrying the `path#Symbol` symbol key — the
        // node that round-trips content_json and that the backend backlink indexer keys on.
        let parsed =
            parse_wikilink("[[code:src/main.rs#MyStruct]]").expect("a valid code wikilink");
        assert_eq!(parsed.kind, WikilinkKind::Known("code".to_owned()));
        assert_eq!(parsed.kind.ref_kind(), "code");
        assert_eq!(parsed.ref_value, "src/main.rs#MyStruct");
        assert!(parsed.resolved, "the code: prefix is a known kind");
        let link = parsed.to_hs_link();
        assert_eq!(link.ref_kind, "code");
        assert_eq!(link.ref_value, "src/main.rs#MyStruct");
        assert!(link.resolved);
    }

    #[test]
    fn prefix_is_case_insensitive() {
        // The React map keys are lower-cased; an upper-cased typed prefix still resolves.
        let parsed = parse_wikilink("[[WP:WP-1]]").expect("parseable");
        assert_eq!(parsed.kind, WikilinkKind::Known("wp".to_owned()));
        assert_eq!(parsed.raw_prefix, "wp");
    }

    #[test]
    fn known_without_label_defaults_to_ref_value() {
        let parsed = parse_wikilink("[[file:src/app.ts]]").expect("parseable");
        assert_eq!(
            parsed.label, "src/app.ts",
            "known kind defaults label to the value"
        );
        assert_eq!(parsed.ref_value, "src/app.ts");
    }

    #[test]
    fn non_wikilink_string_is_none() {
        assert!(parse_wikilink("not a link").is_none());
        assert!(parse_wikilink("[single bracket]").is_none());
        assert!(
            parse_wikilink("[[no-colon]]").is_none(),
            "a token with no `:` is not a wikilink"
        );
        assert!(parse_wikilink("").is_none());
    }

    #[test]
    fn to_hs_link_maps_one_to_one() {
        let parsed = parse_wikilink("[[wp:WP-7|Seven]]").unwrap();
        let link = parsed.to_hs_link();
        assert_eq!(link.ref_kind, "wp");
        assert_eq!(link.ref_value, "WP-7");
        assert_eq!(link.label, "Seven");
        assert!(link.resolved);
        // An unknown maps to a ref_kind="unknown", resolved=false link.
        let unk = parse_wikilink("[[zzz:Q]]").unwrap().to_hs_link();
        assert_eq!(unk.ref_kind, "unknown");
        assert!(!unk.resolved);
        assert_eq!(unk.ref_value, "Q");
    }

    #[test]
    fn extract_finds_all_in_order() {
        let text = "see [[wp:A]] and [[file:b.rs|B]] then [[zzz:c]]";
        let links = extract_wikilinks(text);
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].kind, WikilinkKind::Known("wp".to_owned()));
        assert_eq!(links[1].label, "B");
        assert_eq!(links[2].kind, WikilinkKind::Unknown("zzz".to_owned()));
    }

    #[test]
    fn open_query_detects_unterminated_trigger() {
        // step 4: typing `[[` then a partial query opens the popup with the query so far.
        assert_eq!(
            open_wikilink_query("hello [[wp:WP-"),
            Some((6, "wp:WP-".to_owned()))
        );
        assert_eq!(open_wikilink_query("[["), Some((0, "".to_owned())));
        assert_eq!(open_wikilink_query("text [[no"), Some((5, "no".to_owned())));
    }

    #[test]
    fn open_query_closed_token_does_not_retrigger() {
        // A finished `[[wp:X]]` token must NOT re-open the popup.
        assert_eq!(open_wikilink_query("see [[wp:X]]"), None);
        assert_eq!(open_wikilink_query("plain text"), None);
        assert_eq!(open_wikilink_query(""), None);
        // An OPEN trigger AFTER a closed one re-opens for the new token.
        assert_eq!(
            open_wikilink_query("[[wp:X]] and [[fi"),
            Some((13, "fi".to_owned()))
        );
    }

    #[test]
    fn regex_compiles_once_and_is_reused() {
        // The OnceLock returns the SAME compiled regex instance across calls (compiled once).
        let a = wikilink_regex() as *const Regex;
        let b = wikilink_regex() as *const Regex;
        assert_eq!(a, b, "the regex is compiled once and reused");
    }
}
