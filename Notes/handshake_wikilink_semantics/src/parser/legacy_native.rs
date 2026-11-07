//! Explicit compatibility with the native parser's historical first-match and
//! Unicode-word prefix grammar. Canonical parser defaults remain strict.

use regex::{Captures, Regex};
use std::sync::OnceLock;

pub use super::{
    classify_wikilink, open_wikilink_query, wikilink_kind_by_prefix, ParsedWikilink, WikilinkKind,
};

/// The historical native regex. The first prefix character stays ASCII; later
/// characters use regex's Unicode-aware word class. Unknown prefixes stay unknown.
pub fn wikilink_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\[\[([a-zA-Z_][\w]*):([^\]|]+)(?:\|([^\]]+))?\]\]")
            .expect("legacy native wikilink regex is a constant pattern")
    })
}

/// Return the first token in trimmed input, including surrounding prose or later
/// tokens. Production atom admission remains a separate host responsibility.
pub fn parse_wikilink(token: &str) -> Option<ParsedWikilink> {
    classify_capture(wikilink_regex().captures(token.trim())?)
}

/// Retain all historical native tokens in order, including unresolved Unicode
/// prefixes. Classification, labels and model materialization use the canonical
/// implementation and types, rather than a second native semantic implementation.
pub fn extract_wikilinks(text: &str) -> Vec<ParsedWikilink> {
    wikilink_regex()
        .captures_iter(text)
        .filter_map(classify_capture)
        .collect()
}

fn classify_capture(caps: Captures<'_>) -> Option<ParsedWikilink> {
    Some(classify_wikilink(
        caps.get(1)?.as_str(),
        caps.get(2)?.as_str(),
        caps.get(3).map(|value| value.as_str()),
    ))
}
