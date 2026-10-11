//! Granted asset resolution. The crate never fetches, opens or resolves anything itself: every reference
//! found in the source is classified, and only relative references reach the caller's `AssetPort`, which
//! alone decides what is granted. External, protocol-relative, `file:`, script and other scheme URLs are
//! typed `Denied` without the port being consulted.

use crate::css::css_references;
use crate::dom::{ContentModel, NodeId, NodeKind, WebDoc};
use crate::entity::decode_entities;
use crate::error::WebError;
use hsk_studio_accord::CancellationToken;
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssetDecision {
    Granted { id: String },
    Denied,
    Unavailable,
}

/// Caller-owned grant lookup for relative references. Must not block, fetch or have side effects.
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
    /// Port answered `Denied`, or the reference carries control characters.
    NotGranted,
}
impl DenyReason {
    pub const fn code(self) -> &'static str {
        match self {
            Self::ExternalScheme => "external_scheme",
            Self::ProtocolRelative => "protocol_relative",
            Self::FileScheme => "file_scheme",
            Self::ScriptScheme => "script_scheme",
            Self::NotGranted => "not_granted",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssetOutcome {
    Granted { id: String },
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
    /// Bytes in the source holding the reference (the whole value when entity decoding changed offsets).
    pub span: Range<usize>,
    pub outcome: AssetOutcome,
}

const URL_ATTRIBUTES: &[(&str, &str)] = &[
    ("img", "src"),
    ("link", "href"),
    ("script", "src"),
    ("source", "src"),
    ("video", "src"),
    ("video", "poster"),
    ("audio", "src"),
    ("iframe", "src"),
    ("a", "href"),
    ("area", "href"),
    ("use", "href"),
    ("use", "xlink:href"),
    ("embed", "src"),
    ("object", "data"),
    ("track", "src"),
    ("input", "src"),
];

/// Classify without any I/O. `None` = relative: the port decides.
fn classify(reference: &str) -> Option<AssetOutcome> {
    // URL parsing drops tab/newline/CR and strips leading/trailing C0 controls and spaces; mirror that so
    // `java\nscript:` cannot hide a scheme, and treat `\` as `/` for the authority check.
    let cleaned: String = reference
        .trim_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .map(|c| if c == '\\' { '/' } else { c.to_ascii_lowercase() })
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
        let valid = scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
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

fn decide(reference: &str, port: &dyn AssetPort) -> AssetOutcome {
    if let Some(done) = classify(reference) {
        return done;
    }
    if reference.chars().any(|c| c.is_control()) {
        return AssetOutcome::Denied(DenyReason::NotGranted);
    }
    match port.resolve(reference) {
        AssetDecision::Granted { id } => AssetOutcome::Granted { id },
        AssetDecision::Denied => AssetOutcome::Denied(DenyReason::NotGranted),
        AssetDecision::Unavailable => AssetOutcome::Unavailable,
    }
}

/// Every asset-bearing reference in the document (`img/source/video/audio/iframe/embed/track/input[src]`,
/// `link/a/area/use[href]`, `object[data]`, `video[poster]`, CSS `url(...)` and `@import "..."` in `style`
/// attributes and `<style>` bodies), in document order, with the decision for each. `srcset`, `image-set()`
/// and `<meta http-equiv=refresh>` are not scanned.
pub fn collect_assets(doc: &WebDoc, port: &dyn AssetPort, cancel: &CancellationToken) -> Result<Vec<AssetRef>, WebError> {
    let limit = doc.limits().max_nodes;
    let mut out: Vec<AssetRef> = Vec::new();
    for id in 0..doc.len() {
        if id & 0x3ff == 0 {
            cancel.check()?;
        }
        let Some(node) = doc.node(id) else { continue };
        if node.kind != NodeKind::Element {
            continue;
        }
        let tag = doc.tag_name(id).unwrap_or("").to_ascii_lowercase();
        for attr in &node.attrs {
            let name = doc.attr_name(attr).to_ascii_lowercase();
            let Some(value_span) = attr.value_span.clone() else { continue };
            if out.len() >= limit {
                return Err(WebError::LimitExceeded { limit: crate::error::LimitKind::Nodes, span: value_span });
            }
            if URL_ATTRIBUTES.iter().any(|(t, a)| *t == tag && *a == name) {
                let reference = decode_entities(doc.slice(&value_span)).text;
                if reference.trim().is_empty() {
                    continue;
                }
                let outcome = decide(&reference, port);
                out.push(AssetRef { node: id, source: AssetSource::Attribute(name.clone()), reference, span: value_span, outcome });
            } else if name == "style" {
                let raw = doc.slice(&value_span);
                let decoded = decode_entities(raw).text;
                let exact = decoded == raw;
                for r in css_references(&decoded, limit.saturating_sub(out.len())) {
                    let reference = decoded[r.clone()].to_owned();
                    let span = if exact { value_span.start + r.start..value_span.start + r.end } else { value_span.clone() };
                    let outcome = decide(&reference, port);
                    out.push(AssetRef { node: id, source: AssetSource::InlineStyle, reference, span, outcome });
                }
            }
        }
        if node.content == ContentModel::RawText && tag == "style" {
            for &child in &node.children {
                let Some(raw) = doc.node(child).filter(|c| c.kind == NodeKind::Raw) else { continue };
                let css = doc.slice(&raw.span);
                for r in css_references(css, limit.saturating_sub(out.len())) {
                    let reference = css[r.clone()].to_owned();
                    let span = raw.span.start + r.start..raw.span.start + r.end;
                    let outcome = decide(&reference, port);
                    out.push(AssetRef { node: id, source: AssetSource::StyleBody, reference, span, outcome });
                }
            }
        }
    }
    Ok(out)
}
