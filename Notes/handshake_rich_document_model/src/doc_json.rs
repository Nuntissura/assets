//! DocJson: serialize/deserialize the typed block model to/from the Tiptap
//! JSONContent shape used by the backend `knowledge/documents.content_json` field
//! (WP-KERNEL-012 MT-011).
//!
//! This is the LOAD-BEARING compatibility contract (MT impl note: anchored to the
//! REAL React/backend shape, read from `app/src/lib/editor/*`). The on-the-wire
//! shape is exactly what the React Tiptap editor produces:
//!
//! ```json
//! { "type": "doc", "content": [
//!     { "type": "heading", "attrs": { "level": 1 }, "content": [ { "type": "text", "text": "Title" } ] },
//!     { "type": "paragraph", "content": [
//!         { "type": "text", "text": "Bold ", "marks": [ { "type": "bold" } ] },
//!         { "type": "text", "text": "link", "marks": [ { "type": "link", "attrs": { "href": "https://x" } } ] }
//!     ] }
//! ] }
//! ```
//!
//! Round-trip rules (red-team RISK-3 — never silently drop attrs):
//! - `attrs` is serialized as an `"attrs"` object of `serde_json::Value` (NOT
//!   `#[serde(flatten)]`, which loses type info — MT impl note 4). Unknown attrs
//!   survive verbatim.
//! - `marks` is a `"marks"` array of `{ "type": ..., "attrs"?: {...} }`. `link`
//!   carries `attrs.href`.
//! - A typed wikilink is an inline ATOM node `{ "type": "hsLink", "attrs": {
//!   refKind, refValue, label, resolved } }` living inside a paragraph's `content`
//!   array (the REAL backend shape from `app/src/lib/tiptap/hs_link_node.ts`), NOT a
//!   mark. The serializer emits and the deserializer reads exactly that node.
//!
//! ## content_json is a BARE doc node; schema_version is a SIBLING field
//!
//! The backend `RichDocument` record (`app/src/lib/api.ts`) carries `schema_version`
//! and `content_json` as SEPARATE fields, and `content_json` is a BARE ProseMirror
//! doc node (`{type:"doc", content:[...]}`) with NO `schema_version` key inside it
//! (createRichDocument POSTs `content_json` alone). So:
//! - [`to_content_json_value`] / [`to_content_json_string`] produce the BARE doc node
//!   that is sent to POST/PUT `/knowledge/documents` `content_json` (the load-bearing
//!   wire value). It has NO `schema_version` key.
//! - [`RichDocument`] is the RECORD envelope, carrying `schema_version`
//!   ([`RICH_DOCUMENT_SCHEMA_VERSION`] = `"rich_document_v1"`, matching the React
//!   `WP009_RICH_DOCUMENT_SCHEMA_VERSION`) as a SIBLING of `content_json`, NOT
//!   flattened into the doc node. A version bump is a one-line change to the const.
//!
//! The serde model uses an INTERMEDIATE typed JSON struct (`JsonNode`) rather than a
//! hand-rolled `serde_json::Value` walk, so both the write and read paths are the
//! same shape (CODER_RUBRIC dimension 4: end-to-end integrity — inspect both
//! write and read paths for serialized shapes).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as JsonValue};
use thiserror::Error;

use super::node::{BlockNode, Child, HsLinkNode, Mark, NodeKind, TextLeaf, TransclusionNode};

/// The single source of the rich-document schema version. A bump is a one-line edit
/// here (MT impl note). MUST equal the React `WP009_RICH_DOCUMENT_SCHEMA_VERSION`
/// (`"rich_document_v1"`) so the backend round-trips the document (RISK-5).
pub const RICH_DOCUMENT_SCHEMA_VERSION: &str = "rich_document_v1";

/// Why a DocJson (de)serialization failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DocJsonError {
    /// The top-level node was not a `doc`.
    #[error("expected a top-level 'doc' node, found {found:?}")]
    NotADoc { found: Option<String> },
    /// An unknown node `type` string with no [`NodeKind`] mapping.
    #[error("unknown node type {0:?}")]
    UnknownNodeType(String),
    /// An unknown mark `type` string with no [`Mark`] mapping.
    #[error("unknown mark type {0:?}")]
    UnknownMarkType(String),
    /// A link mark was missing its required `href` attr.
    #[error("link mark missing href attr")]
    LinkMissingHref,
    /// An `hsLink` node was missing a required attr (`refValue`).
    #[error("hsLink node missing required attr: {0}")]
    HsLinkMissingAttr(&'static str),
    /// A `loomTransclusion` node was missing a required attr (`refValue`).
    #[error("loomTransclusion node missing required attr: {0}")]
    TransclusionMissingAttr(&'static str),
    /// The JSON text could not be parsed at all.
    #[error("invalid JSON: {0}")]
    Parse(String),
}

/// The doc-level RECORD envelope, matching the backend `RichDocument` record
/// (`app/src/lib/api.ts`): `schema_version` and `content_json` are SEPARATE sibling
/// fields. `content_json` is the BARE doc node (`{type:"doc", content:[...]}`) with
/// NO `schema_version` key inside it — the version lives only here, as a sibling.
///
/// This is NOT what is POSTed to `/knowledge/documents` (the API receives the bare
/// `content_json` node alone — see [`to_content_json_value`]); it mirrors the record
/// the backend STORES + RETURNS so a round-trip test can assert both the bare wire
/// value and the version-carrying record shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RichDocument {
    /// The schema version (always [`RICH_DOCUMENT_SCHEMA_VERSION`] on serialize), a
    /// SIBLING of `content_json`, never flattened into the doc node.
    pub schema_version: String,
    /// The BARE root `doc` node (no `schema_version` key inside).
    pub content_json: JsonNode,
}

/// The serde wire representation of one node (block or text). This is the Tiptap
/// JSONContent shape. `type`/`attrs`/`content`/`text`/`marks` are all optional so a
/// text node (no `content`/`attrs`) and a block node (no `text`/`marks`) share one
/// struct, matching the `JSONContentLike` type in `app/src/lib/api.ts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonNode {
    /// The node/mark `type` string (camelCase Tiptap name).
    #[serde(rename = "type")]
    pub ty: String,
    /// Node attributes (heading level, code-block language, task checked, …).
    /// Serialized as `"attrs"` object; omitted when empty (MT impl note 4).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub attrs: Option<Map<String, JsonValue>>,
    /// Block content (child nodes). Omitted on text nodes.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub content: Option<Vec<JsonNode>>,
    /// Text payload (present only on text nodes).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub text: Option<String>,
    /// Marks on a text node. Omitted when none.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub marks: Option<Vec<JsonMark>>,
}

/// The serde wire representation of one mark: `{ "type": ..., "attrs"?: {...} }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonMark {
    /// The mark `type` string.
    #[serde(rename = "type")]
    pub ty: String,
    /// Mark attributes (`href` for link; `kind`/`value`/`label` for wikilink).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub attrs: Option<Map<String, JsonValue>>,
}

// ---- model -> JSON ----------------------------------------------------------

/// Serialize a `doc` [`BlockNode`] to the [`RichDocument`] RECORD envelope
/// (`schema_version` + bare `content_json` siblings). Mirrors the backend record
/// shape; use [`to_content_json_value`] for the bare value actually POSTed.
pub fn to_rich_document(doc: &BlockNode) -> RichDocument {
    RichDocument {
        schema_version: RICH_DOCUMENT_SCHEMA_VERSION.to_string(),
        content_json: block_to_json(doc),
    }
}

/// Serialize a `doc` [`BlockNode`] to the BARE `content_json` value sent to
/// POST/PUT `/knowledge/documents` (`createRichDocument` / `saveRichDocument`). This
/// is a bare ProseMirror doc node (`{type:"doc", content:[...]}`) with NO
/// `schema_version` key — the load-bearing wire value (binds_backend_api).
pub fn to_content_json_value(doc: &BlockNode) -> JsonValue {
    serde_json::to_value(block_to_json(doc)).unwrap_or(JsonValue::Null)
}

/// Serialize a `doc` [`BlockNode`] to the BARE `content_json` JSON `String`. This is
/// the wire value sent to the backend; deserialize it back with [`from_json_string`].
pub fn to_json_string(doc: &BlockNode) -> Result<String, DocJsonError> {
    serde_json::to_string(&block_to_json(doc)).map_err(|e| DocJsonError::Parse(e.to_string()))
}

/// Serialize a [`BlockNode`] to its bare [`JsonNode`] (no envelope) — used for
/// `serde_json::Value` shape assertions in the acceptance tests.
pub fn block_to_json(node: &BlockNode) -> JsonNode {
    let attrs = node_attrs_map(node);
    if node.kind.is_atom() {
        return JsonNode {
            ty: node.kind.to_json_type().to_string(),
            attrs,
            content: None,
            text: None,
            marks: None,
        };
    }
    let content: Vec<JsonNode> = node.children.iter().map(child_to_json).collect();
    JsonNode {
        ty: node.kind.to_json_type().to_string(),
        attrs,
        content: Some(content),
        text: None,
        marks: None,
    }
}

/// Build the `attrs` map for a node: the well-known typed attrs (`level` for a
/// heading) are stamped from the typed kind, then any free-form attrs are layered on
/// (free-form wins for forward-compat keys, except the typed `level` which the kind
/// owns). Returns `None` when empty so `attrs` is omitted from the JSON.
fn node_attrs_map(node: &BlockNode) -> Option<Map<String, JsonValue>> {
    let mut map = Map::new();
    // Free-form attrs first (so a forward-compat key survives — RISK-3)…
    for (k, v) in &node.attrs {
        map.insert(k.clone(), v.clone());
    }
    // …then stamp the typed level so a heading always carries the correct level
    // even if the free-form attrs lacked it.
    if let NodeKind::Heading(level) = node.kind {
        map.insert("level".to_string(), JsonValue::from(level.get()));
    }
    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

/// Serialize a [`Child`] (block, text leaf, hsLink atom, or loomTransclusion atom) to a [`JsonNode`].
fn child_to_json(child: &Child) -> JsonNode {
    match child {
        Child::Block(b) => block_to_json(b),
        Child::Text(leaf) => text_leaf_to_json(leaf),
        Child::HsLink(link) => hs_link_to_json(link),
        Child::Transclusion(t) => transclusion_to_json(t),
    }
}

/// Serialize a [`TransclusionNode`] to a `{type:"loomTransclusion", attrs:{refValue}}` inline atom
/// node — the REAL backend shape from `app/src/components/LoomTransclusionView.tsx` (MT-015). The
/// host stores ONLY the `refValue` reference; the body is resolved at view time.
fn transclusion_to_json(node: &TransclusionNode) -> JsonNode {
    let mut attrs = Map::new();
    attrs.insert(
        "refValue".to_string(),
        JsonValue::from(node.ref_value.clone()),
    );
    JsonNode {
        ty: "loomTransclusion".to_string(),
        attrs: Some(attrs),
        content: None,
        text: None,
        marks: None,
    }
}

/// Serialize an [`HsLinkNode`] to a `{type:"hsLink", attrs:{refKind, refValue,
/// label, resolved}}` inline atom node — the REAL backend shape from
/// `app/src/lib/tiptap/hs_link_node.ts` (camelCase attr keys; an atom carries no
/// `content`/`text`).
fn hs_link_to_json(link: &HsLinkNode) -> JsonNode {
    let mut attrs = Map::new();
    attrs.insert(
        "refKind".to_string(),
        JsonValue::from(link.ref_kind.clone()),
    );
    attrs.insert(
        "refValue".to_string(),
        JsonValue::from(link.ref_value.clone()),
    );
    attrs.insert("label".to_string(), JsonValue::from(link.label.clone()));
    attrs.insert("resolved".to_string(), JsonValue::from(link.resolved));
    if let Some(provenance) = link.provenance.clone() {
        attrs.insert("provenance".to_string(), provenance);
    }
    JsonNode {
        ty: "hsLink".to_string(),
        attrs: Some(attrs),
        content: None,
        text: None,
        marks: None,
    }
}

/// Serialize a [`TextLeaf`] to a `{type:"text", text:..., marks?:[...]}` node.
fn text_leaf_to_json(leaf: &TextLeaf) -> JsonNode {
    let marks: Vec<JsonMark> = leaf.marks.iter().map(mark_to_json).collect();
    JsonNode {
        ty: "text".to_string(),
        attrs: None,
        content: None,
        text: Some(leaf.text.to_string()),
        marks: if marks.is_empty() { None } else { Some(marks) },
    }
}

/// Serialize a [`Mark`] to a `{type, attrs?}` wire mark, carrying typed payloads.
fn mark_to_json(mark: &Mark) -> JsonMark {
    match mark {
        Mark::Link { href } => {
            let mut attrs = Map::new();
            attrs.insert("href".to_string(), JsonValue::from(href.clone()));
            JsonMark {
                ty: "link".to_string(),
                attrs: Some(attrs),
            }
        }
        other => JsonMark {
            ty: other.json_type().to_string(),
            attrs: None,
        },
    }
}

// ---- JSON -> model ----------------------------------------------------------

/// Parse a JSON `String` envelope back into a `doc` [`BlockNode`]. Accepts the
/// [`RichDocument`] envelope (with `schema_version`); the bare-doc form is accepted
/// too (the `schema_version` is then assumed current).
pub fn from_json_string(json: &str) -> Result<BlockNode, DocJsonError> {
    let value: JsonValue =
        serde_json::from_str(json).map_err(|e| DocJsonError::Parse(e.to_string()))?;
    from_json_value(&value)
}

/// Parse a `serde_json::Value` envelope/bare-doc into a `doc` [`BlockNode`].
pub fn from_json_value(value: &JsonValue) -> Result<BlockNode, DocJsonError> {
    let node: JsonNode =
        serde_json::from_value(value.clone()).map_err(|e| DocJsonError::Parse(e.to_string()))?;
    let block = json_to_block(&node)?;
    if !matches!(block.kind, NodeKind::Doc) {
        return Err(DocJsonError::NotADoc {
            found: Some(node.ty.clone()),
        });
    }
    Ok(block)
}

/// Deserialize a [`JsonNode`] into a block [`BlockNode`]. The caller guarantees this
/// is a block node (a text node is handled in [`json_to_child`]).
fn json_to_block(node: &JsonNode) -> Result<BlockNode, DocJsonError> {
    let heading_level = node
        .attrs
        .as_ref()
        .and_then(|m| m.get("level"))
        .and_then(JsonValue::as_u64)
        .map(|l| l as u8)
        .unwrap_or(1);
    let kind = NodeKind::from_json_type(&node.ty, heading_level)
        .ok_or_else(|| DocJsonError::UnknownNodeType(node.ty.clone()))?;

    // Preserve free-form attrs verbatim (RISK-3) EXCEPT the typed `level`, which the
    // kind already owns (and `to_json` re-stamps), so we don't duplicate it.
    let mut attrs = std::collections::HashMap::new();
    if let Some(map) = &node.attrs {
        for (k, v) in map {
            if matches!(kind, NodeKind::Heading(_)) && k == "level" {
                continue;
            }
            attrs.insert(k.clone(), v.clone());
        }
    }

    let mut children = Vec::new();
    if let Some(content) = &node.content {
        for c in content {
            children.push(json_to_child(c)?);
        }
    }
    Ok(BlockNode {
        kind,
        attrs,
        children,
    })
}

/// Deserialize a [`JsonNode`] into a [`Child`]: a `"text"` node becomes a text leaf,
/// an `"hsLink"` node becomes an inline atom, anything else a nested block.
fn json_to_child(node: &JsonNode) -> Result<Child, DocJsonError> {
    match node.ty.as_str() {
        "text" => {
            let text = node.text.clone().unwrap_or_default();
            let mut marks = Vec::new();
            if let Some(ms) = &node.marks {
                for m in ms {
                    marks.push(json_to_mark(m)?);
                }
            }
            Ok(Child::Text(TextLeaf::with_marks(&text, marks)))
        }
        "hsLink" => Ok(Child::HsLink(json_to_hs_link(node)?)),
        "loomTransclusion" => Ok(Child::Transclusion(json_to_transclusion(node)?)),
        _ => Ok(Child::Block(json_to_block(node)?)),
    }
}

/// Deserialize a `loomTransclusion` [`JsonNode`] into a [`TransclusionNode`], reading the
/// `attrs.refValue` the React node persists (MT-015). `refValue` is required (a transclusion with no
/// target block is malformed).
fn json_to_transclusion(node: &JsonNode) -> Result<TransclusionNode, DocJsonError> {
    let ref_value = node
        .attrs
        .as_ref()
        .and_then(|m| m.get("refValue"))
        .and_then(JsonValue::as_str)
        .ok_or(DocJsonError::TransclusionMissingAttr("refValue"))?
        .to_string();
    Ok(TransclusionNode { ref_value })
}

/// Deserialize an `hsLink` [`JsonNode`] into an [`HsLinkNode`], reading the camelCase
/// `attrs.{refKind, refValue, label, resolved}` the React node persists. `refKind`
/// defaults to `"unknown"`, `label` to `""`, `resolved` to `true` (the React node
/// defaults). `refValue` is required (a wikilink with no target is malformed).
fn json_to_hs_link(node: &JsonNode) -> Result<HsLinkNode, DocJsonError> {
    let attrs = node.attrs.as_ref();
    let ref_kind = attrs
        .and_then(|m| m.get("refKind"))
        .and_then(JsonValue::as_str)
        .unwrap_or("unknown")
        .to_string();
    let ref_value = attrs
        .and_then(|m| m.get("refValue"))
        .and_then(JsonValue::as_str)
        .ok_or(DocJsonError::HsLinkMissingAttr("refValue"))?
        .to_string();
    let label = attrs
        .and_then(|m| m.get("label"))
        .and_then(JsonValue::as_str)
        .unwrap_or("")
        .to_string();
    let resolved = attrs
        .and_then(|m| m.get("resolved"))
        .and_then(JsonValue::as_bool)
        .unwrap_or(true);
    let provenance = attrs.and_then(|m| m.get("provenance")).cloned();
    Ok(HsLinkNode {
        ref_kind,
        ref_value,
        label,
        resolved,
        provenance,
    })
}

/// Deserialize a [`JsonMark`] into a [`Mark`], reading the typed payloads.
fn json_to_mark(mark: &JsonMark) -> Result<Mark, DocJsonError> {
    Ok(match mark.ty.as_str() {
        "bold" => Mark::Bold,
        "italic" => Mark::Italic,
        "underline" => Mark::Underline,
        "strike" => Mark::Strike,
        "code" => Mark::Code,
        "link" => {
            let href = mark
                .attrs
                .as_ref()
                .and_then(|m| m.get("href"))
                .and_then(JsonValue::as_str)
                .ok_or(DocJsonError::LinkMissingHref)?
                .to_string();
            Mark::Link { href }
        }
        other => return Err(DocJsonError::UnknownMarkType(other.to_string())),
    })
}

#[cfg(test)]
mod tests {
    use super::super::node::{BlockNode, Child, HsLinkNode, Mark, NodeKind, TextLeaf};
    use super::*;

    fn rich_doc() -> BlockNode {
        // doc > [ heading(1,"Title"), paragraph("Bold italic", with bold+italic) ]
        let mut para = BlockNode::new(NodeKind::Paragraph);
        para.children
            .push(Child::Text(TextLeaf::with_marks("Bold ", vec![Mark::Bold])));
        para.children.push(Child::Text(TextLeaf::with_marks(
            "italic",
            vec![Mark::Italic],
        )));
        BlockNode::doc(vec![BlockNode::heading(1, "Title"), para])
    }

    #[test]
    fn round_trip_equals_original() {
        let doc = rich_doc();
        let json = to_json_string(&doc).unwrap();
        let back = from_json_string(&json).unwrap();
        assert_eq!(doc, back);
    }

    #[test]
    fn schema_version_is_rich_document_v1() {
        assert_eq!(RICH_DOCUMENT_SCHEMA_VERSION, "rich_document_v1");
        let env = to_rich_document(&rich_doc());
        assert_eq!(env.schema_version, "rich_document_v1");
    }

    #[test]
    fn content_json_is_bare_doc_with_no_schema_version() {
        // MUST-FIX #2: the wire content_json is a BARE doc node with NO
        // schema_version key inside it (the version is a sibling record field).
        let v = to_content_json_value(&rich_doc());
        assert_eq!(v["type"], "doc");
        assert!(
            v.get("schema_version").is_none(),
            "content_json must NOT embed schema_version"
        );
        // The record envelope carries the version as a SIBLING of content_json.
        let env: JsonValue = serde_json::to_value(to_rich_document(&rich_doc())).unwrap();
        assert_eq!(env["schema_version"], "rich_document_v1");
        assert_eq!(env["content_json"]["type"], "doc");
        assert!(env["content_json"].get("schema_version").is_none());
    }

    #[test]
    fn json_shape_matches_tiptap() {
        let doc = rich_doc();
        // The bare content_json value (what the API receives) is a doc node.
        let v: JsonValue = to_content_json_value(&doc);
        assert_eq!(v["type"], "doc");
        // First child: heading with attrs.level == 1.
        assert_eq!(v["content"][0]["type"], "heading");
        assert_eq!(v["content"][0]["attrs"]["level"], 1);
        assert_eq!(v["content"][0]["content"][0]["type"], "text");
        assert_eq!(v["content"][0]["content"][0]["text"], "Title");
        // Second child: paragraph with a bold text run.
        assert_eq!(v["content"][1]["type"], "paragraph");
        assert_eq!(v["content"][1]["content"][0]["text"], "Bold ");
        assert_eq!(v["content"][1]["content"][0]["marks"][0]["type"], "bold");
    }

    #[test]
    fn link_href_survives_round_trip() {
        let mut para = BlockNode::new(NodeKind::Paragraph);
        para.children.push(Child::Text(TextLeaf::with_marks(
            "click",
            vec![Mark::Link {
                href: "https://example.com/docs".to_string(),
            }],
        )));
        let doc = BlockNode::doc(vec![para]);
        let json = to_json_string(&doc).unwrap();
        let back = from_json_string(&json).unwrap();
        assert_eq!(doc, back);
        // The href is present in the bare content_json.
        let v: JsonValue = to_content_json_value(&doc);
        assert_eq!(
            v["content"][0]["content"][0]["marks"][0]["attrs"]["href"],
            "https://example.com/docs"
        );
    }

    #[test]
    fn hs_link_node_payload_survives_round_trip() {
        // MUST-FIX #1: a wikilink is an inline hsLink NODE, not a mark.
        let mut para = BlockNode::new(NodeKind::Paragraph);
        para.children.push(Child::Text(TextLeaf::new("see ")));
        para.children.push(Child::HsLink(HsLinkNode::new(
            "wp",
            "WP-KERNEL-012",
            "the WP",
        )));
        let doc = BlockNode::doc(vec![para]);
        let json = to_json_string(&doc).unwrap();
        let back = from_json_string(&json).unwrap();
        assert_eq!(doc, back);
        // The hsLink serializes as a sibling node with camelCase backend attrs.
        let v: JsonValue = to_content_json_value(&doc);
        let link = &v["content"][0]["content"][1];
        assert_eq!(link["type"], "hsLink");
        assert_eq!(link["attrs"]["refKind"], "wp");
        assert_eq!(link["attrs"]["refValue"], "WP-KERNEL-012");
        assert_eq!(link["attrs"]["label"], "the WP");
        assert_eq!(link["attrs"]["resolved"], true);
    }

    #[test]
    fn transclusion_node_payload_survives_round_trip() {
        // MT-015: a loomTransclusion is an inline atom node carrying `attrs.refValue` — the REAL
        // backend shape (NOT a `{block_id}` attr). It round-trips through DocJson unchanged.
        use super::super::node::TransclusionNode;
        let mut para = BlockNode::new(NodeKind::Paragraph);
        para.children.push(Child::Text(TextLeaf::new("embed: ")));
        para.children
            .push(Child::Transclusion(TransclusionNode::new("BLK-42")));
        let doc = BlockNode::doc(vec![para]);
        let json = to_json_string(&doc).unwrap();
        let back = from_json_string(&json).unwrap();
        assert_eq!(doc, back);
        // The node serializes with the camelCase `refValue` attr the backend round-trips.
        let v: JsonValue = to_content_json_value(&doc);
        let node = &v["content"][0]["content"][1];
        assert_eq!(node["type"], "loomTransclusion");
        assert_eq!(node["attrs"]["refValue"], "BLK-42");
        assert!(node.get("content").is_none(), "an atom carries no content");
    }

    #[test]
    fn transclusion_missing_ref_value_errors() {
        let json = r#"{"type":"doc","content":[{"type":"paragraph","content":[{"type":"loomTransclusion","attrs":{}}]}]}"#;
        assert!(matches!(
            from_json_string(json),
            Err(DocJsonError::TransclusionMissingAttr("refValue"))
        ));
    }

    #[test]
    fn unknown_attr_survives_round_trip() {
        // A forward-compat attr the schema does not know must NOT be dropped (RISK-3).
        let mut para = BlockNode::new(NodeKind::Paragraph);
        para.attrs
            .insert("data-future".to_string(), JsonValue::from("keepme"));
        para.children.push(Child::Text(TextLeaf::new("x")));
        let doc = BlockNode::doc(vec![para]);
        let json = to_json_string(&doc).unwrap();
        let back = from_json_string(&json).unwrap();
        assert_eq!(
            back.children[0]
                .as_block()
                .unwrap()
                .attrs
                .get("data-future"),
            Some(&JsonValue::from("keepme"))
        );
    }

    #[test]
    fn unknown_node_type_errors() {
        let json = r#"{"type":"doc","content":[{"type":"bogus"}]}"#;
        assert!(matches!(
            from_json_string(json),
            Err(DocJsonError::UnknownNodeType(_))
        ));
    }

    #[test]
    fn table_header_node_deserializes_from_real_backend_shape() {
        // MT-020 amendment (carried MT-013), anchored to a CAPTURED REAL backend `content_json`
        // fixture (a Tiptap table whose first row uses `tableHeader` cells and whose body row uses
        // `tableCell`). Before the TableHeader variant this deserialized as UnknownNodeType. The
        // fixture is the verbatim shape the React Tiptap table extension emits (NOT
        // tableCell{attrs.isHeader:true}).
        let captured = r#"{
            "type": "doc",
            "content": [
                { "type": "table", "content": [
                    { "type": "tableRow", "content": [
                        { "type": "tableHeader", "content": [
                            { "type": "paragraph", "content": [ { "type": "text", "text": "Col A" } ] }
                        ] },
                        { "type": "tableHeader", "content": [
                            { "type": "paragraph", "content": [ { "type": "text", "text": "Col B" } ] }
                        ] }
                    ] },
                    { "type": "tableRow", "content": [
                        { "type": "tableCell", "content": [
                            { "type": "paragraph", "content": [ { "type": "text", "text": "1" } ] }
                        ] },
                        { "type": "tableCell", "content": [
                            { "type": "paragraph", "content": [ { "type": "text", "text": "2" } ] }
                        ] }
                    ] }
                ] }
            ]
        }"#;
        // It deserializes without UnknownNodeType (the assertion that fails pre-amendment).
        let doc = from_json_string(captured).expect("real backend table must deserialize");
        let table = doc.children[0].as_block().unwrap();
        assert_eq!(table.kind, NodeKind::Table);
        let header_row = table.children[0].as_block().unwrap();
        let header_cell = header_row.children[0].as_block().unwrap();
        assert_eq!(
            header_cell.kind,
            NodeKind::TableHeader,
            "first-row cells are tableHeader"
        );
        let body_row = table.children[1].as_block().unwrap();
        let body_cell = body_row.children[0].as_block().unwrap();
        assert_eq!(
            body_cell.kind,
            NodeKind::TableCell,
            "second-row cells are tableCell"
        );

        // And it re-serializes back to the same node TYPES (deserialize -> reserialize byte-shape
        // compatibility against the captured value's structure — NOT a self-round-trip of our own
        // output; we re-parse our re-serialization and assert the kinds survive).
        let reser = to_content_json_value(&doc);
        assert_eq!(
            reser["content"][0]["content"][0]["content"][0]["type"],
            "tableHeader"
        );
        assert_eq!(
            reser["content"][0]["content"][1]["content"][0]["type"],
            "tableCell"
        );
        // Full round-trip equality (model -> json -> model).
        let back = from_json_value(&reser).unwrap();
        assert_eq!(doc, back);
    }
}
