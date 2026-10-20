//! Visual/code/DOM/template/responsive workflow over one source revision and shared editor/preview
//! mechanics. No separate app/server/job or persistence authority.
#![forbid(unsafe_code)]
//!
//! Crate layout (SMB-CRATE-SEAM-SPLITS-2): this crate is the module's public face and re-exports,
//! item by item, its two halves: `hsk-studio-web-core` (DOM, parse, character references, CSS,
//! responsive, preview, refresh, source patches, errors, report, DESCRIPTOR) and
//! `hsk-studio-web-assets` (asset reference gate: carriers, CSS URLs, data: URLs). Both halves
//! belong to STUDIO-MODULE-WEB; not a multi-engine facade.

pub use hsk_studio_web_core::{responsive, source_dom_patch};

pub use hsk_studio_web_assets::{
    AssetDecision, AssetOutcome, AssetPort, AssetRef, AssetSource, DenyReason, MAX_DATA_NESTING,
    collect_assets,
};
pub use hsk_studio_web_assets::{Load, Loads};
pub use hsk_studio_web_core::{Declaration, RuleRefusal};
pub use hsk_studio_web_core::{
    Attr, ContentModel, Descendants, Limits, Namespace, Node, NodeId, NodeKey, NodeKind, Quote,
    ViewRow, WebDoc, parse, sha256_hex,
};
pub use hsk_studio_web_core::{Decoded, decode_entities, escape_attr, escape_text};
pub use hsk_studio_web_core::{
    EditRefusal, LimitKind, LossEntry, LossKind, LossReport, MAX_LOSS_ENTRIES, MalformedKind,
    UnsupportedKind, WebError,
};
pub use hsk_studio_web_core::{
    ActiveRule, HiddenReason, PreviewNode, PreviewTree, UnsupportedRule, UnsupportedRuleReason,
    preview_tree,
};
pub use hsk_studio_web_core::{KeyMap, MAX_REPORTED_ROOTS, key_map, refresh};
pub use hsk_studio_web_core::{outcome_of, report};
pub use hsk_studio_web_core::{
    Applied, SourcePatch, TextEdit, apply, insert_element, insert_fragment, remove_attribute,
    remove_node, set_attribute, set_text,
};
pub use hsk_studio_web_core::DESCRIPTOR;
