//! Typed block-document node + inline-mark model (WP-KERNEL-012 MT-011).
//!
//! This is the BRAIN of the E2 rich-text cluster: a ProseMirror/Tiptap-style typed
//! block tree. Every other E2 microtask (MT-012 rendering, MT-013 lists/tables,
//! MT-014 embeds, MT-015 wikilinks, MT-016 save-to-format, …) binds to the types
//! defined here.
//!
//! ## Why a typed block model (not a comrak/CommonMark AST)
//!
//! The existing `handshake_core` backend stores `knowledge/documents.content_json`
//! in the Tiptap [`JSONContent`](https://prosemirror.net/docs/ref/#model.Node.toJSON)
//! shape (`schema_version = "rich_document_v1"`). The editor MUST round-trip that
//! exact shape (createRichDocument / loadRichDocument / saveRichDocument), so the
//! AUTHORITATIVE in-memory model mirrors the Tiptap node/mark set, not CommonMark.
//! comrak/egui_commonmark are secondary tools used elsewhere (markdown export in
//! MT-016, reading-mode rendering in MT-055) — never a replacement for this model.
//!
//! ## Char-index discipline (red-team RISK-1)
//!
//! Text lives in [`TextLeaf`] which wraps `ropey::Rope`. Every text mutation goes
//! through [`crate::rope_text::RopeText`], which speaks
//! CHAR indices exclusively (`Rope::insert` / `Rope::remove` / `Rope::char`). A
//! byte index is NEVER used to address rope content here, because a byte offset
//! that lands inside a multi-byte UTF-8 char (CJK, emoji) silently corrupts the
//! document. The mandatory `emoji_char_index_is_not_byte_index` test proves it.

use std::borrow::Cow;
use std::collections::HashMap;

use serde_json::Value as JsonValue;

use super::rope_text::RopeText;

/// A block-level node kind. The variant set is exactly the MT-011 contract list:
/// `doc, paragraph, heading[1-3], blockquote, code_block, ordered_list,
/// bullet_list, list_item, table, table_row, table_cell, task_item, hard_break,
/// horizontal_rule`. Heading levels 1..=3 are distinct variants so the schema can
/// validate the level without a stringly attr lookup, but they all serialize to the
/// single Tiptap `"heading"` type with an `attrs.level` (see [`NodeKind::to_json_type`]).
///
/// `to_json_type` / `from_json_type` map each variant to the camelCase Tiptap
/// `type` string the backend `content_json` round-trips (the in-memory enum uses
/// idiomatic Rust names; the WIRE shape is the Tiptap shape — they are deliberately
/// kept distinct and the mapping is the single load-bearing compatibility seam).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// The document root. Exactly one per tree, never nested.
    Doc,
    /// A body paragraph.
    Paragraph,
    /// A heading. Levels 1..=3 (the React StarterKit `heading: { levels: [1,2,3] }`).
    Heading(HeadingLevel),
    /// A block quote (contains block children).
    Blockquote,
    /// A fenced code block (contains a single text leaf; `attrs.language` optional).
    CodeBlock,
    /// An ordered (numbered) list.
    OrderedList,
    /// A bulleted list.
    BulletList,
    /// A list item (child of a list; contains block children).
    ListItem,
    /// A table.
    Table,
    /// A table row.
    TableRow,
    /// A table cell (body cell).
    TableCell,
    /// A table HEADER cell. MT-020 amendment (carried from MT-013): the REAL backend
    /// `content_json` uses a DISTINCT `tableHeader` node type for header cells (verified against
    /// `app/src/lib/tiptap/*` — Tiptap's table extension emits `tableHeader`, NOT
    /// `tableCell{attrs.isHeader:true}`). MT-011 had only [`NodeKind::TableCell`] and MT-013
    /// stopgapped header-ness as an attr, which means a React-authored table containing header
    /// cells would deserialize its `tableHeader` nodes as [`DocJsonError::UnknownNodeType`]. This
    /// variant + its `to_json_type`/`from_json_type` mapping make a real backend table round-trip.
    /// A header cell holds block content exactly like a body cell.
    TableHeader,
    /// A task-list item (carries `attrs.checked: bool`).
    TaskItem,
    /// A hard line break (a leaf inline-ish atom; carries no children/text).
    HardBreak,
    /// A horizontal rule (a leaf block atom; carries no children/text).
    HorizontalRule,
    /// A persisted Notes reference rendered as a block atom. Reference and block
    /// identities remain in the block's attrs; loading its preview is host-owned.
    HsEmbed,
}

/// A heading level, constrained to the 1..=3 the schema allows. Constructed via
/// [`HeadingLevel::new`] so an out-of-range level can never be represented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HeadingLevel(u8);

impl HeadingLevel {
    /// Build a heading level, clamping into the valid `1..=3` window. A level of 0
    /// clamps to 1 and a level > 3 clamps to 3 so an out-of-range attr from a
    /// deserialized document degrades to the nearest valid heading rather than
    /// failing to represent the node.
    pub fn new(level: u8) -> Self {
        Self(level.clamp(1, 3))
    }

    /// The numeric level (always in `1..=3`).
    pub fn get(self) -> u8 {
        self.0
    }
}

impl NodeKind {
    /// The camelCase Tiptap `type` string this kind serializes to in
    /// `content_json`. Heading variants all map to `"heading"` (the level rides in
    /// `attrs.level`). This is the WIRE name — the in-memory enum uses Rust names.
    pub fn to_json_type(self) -> &'static str {
        match self {
            NodeKind::Doc => "doc",
            NodeKind::Paragraph => "paragraph",
            NodeKind::Heading(_) => "heading",
            NodeKind::Blockquote => "blockquote",
            NodeKind::CodeBlock => "codeBlock",
            NodeKind::OrderedList => "orderedList",
            NodeKind::BulletList => "bulletList",
            NodeKind::ListItem => "listItem",
            NodeKind::Table => "table",
            NodeKind::TableRow => "tableRow",
            NodeKind::TableCell => "tableCell",
            NodeKind::TableHeader => "tableHeader",
            NodeKind::TaskItem => "taskItem",
            NodeKind::HardBreak => "hardBreak",
            NodeKind::HorizontalRule => "horizontalRule",
            NodeKind::HsEmbed => "hsEmbed",
        }
    }

    /// Parse a Tiptap `type` string back into a [`NodeKind`]. `"heading"` resolves
    /// to `Heading(level)` using the `level` arg (caller reads `attrs.level`,
    /// defaulting to 1). Returns `None` for an unknown type so the deserializer can
    /// surface a typed error rather than silently inventing a node.
    pub fn from_json_type(ty: &str, heading_level: u8) -> Option<NodeKind> {
        Some(match ty {
            "doc" => NodeKind::Doc,
            "paragraph" => NodeKind::Paragraph,
            "heading" => NodeKind::Heading(HeadingLevel::new(heading_level)),
            "blockquote" => NodeKind::Blockquote,
            "codeBlock" => NodeKind::CodeBlock,
            "orderedList" => NodeKind::OrderedList,
            "bulletList" => NodeKind::BulletList,
            "listItem" => NodeKind::ListItem,
            "table" => NodeKind::Table,
            "tableRow" => NodeKind::TableRow,
            "tableCell" => NodeKind::TableCell,
            "tableHeader" => NodeKind::TableHeader,
            "taskItem" => NodeKind::TaskItem,
            "hardBreak" => NodeKind::HardBreak,
            "horizontalRule" => NodeKind::HorizontalRule,
            "hsEmbed" => NodeKind::HsEmbed,
            _ => return None,
        })
    }

    /// True when this kind is a leaf block atom that holds neither block children
    /// nor a text leaf (`hard_break`, `horizontal_rule`). Used by the schema and
    /// the transform layer to reject inserting children under an atom.
    pub fn is_atom(self) -> bool {
        matches!(
            self,
            NodeKind::HardBreak | NodeKind::HorizontalRule | NodeKind::HsEmbed
        )
    }

    /// True when this kind's direct children are text leaves rather than block
    /// nodes (the inline-content containers: `paragraph`, `heading`, `code_block`).
    /// Other non-atom kinds hold block children. This is the schema's content-model
    /// axis the transform layer consults before inserting a child.
    pub fn holds_inline_content(self) -> bool {
        matches!(
            self,
            NodeKind::Paragraph | NodeKind::Heading(_) | NodeKind::CodeBlock
        )
    }
}

/// An inline mark applied to a run of text. The variant set is exactly the MT-011
/// contract mark list: `bold, italic, underline, strike, code, link`.
///
/// `link` carries a typed `href` payload (the render layer in MT-012 needs it), per
/// the MT implementation note. The payload-free marks compare structurally so
/// `AddMark`/`RemoveMark` and dedup work without special-casing.
///
/// NOTE on wikilinks (BACKEND-SHAPE ANCHOR reconciliation): the MT scope text models
/// a "wikilink" as a mark, but the REAL backend `content_json` shape
/// (`app/src/lib/tiptap/hs_link_node.ts`) is an inline ATOM NODE named `hsLink` with
/// attrs `{refKind, refValue, label, resolved}`, NOT a mark. The mark-vs-node
/// instruction is reconciled in favour of the real backend node so a real
/// `loadRichDocument` response containing a wikilink deserializes (RISK-5). The
/// wikilink therefore lives as [`HsLinkNode`] in [`Child::HsLink`], not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    /// Bold (`<strong>` / `**`).
    Bold,
    /// Italic (`<em>` / `*`).
    Italic,
    /// Underline.
    Underline,
    /// Strikethrough.
    Strike,
    /// Inline code (`<code>` / backticks).
    Code,
    /// A plain hyperlink. `href` is the only attr the React `link` mark carries
    /// (`marks:[{type:"link",attrs:{href:"..."}}]`).
    Link { href: String },
}

impl Mark {
    /// The Tiptap mark `type` string this mark serializes to. `link` serializes as
    /// `{type:"link", attrs:{href}}`; the payload-free marks use their lowercase
    /// name. (Wikilinks are an `hsLink` NODE, not a mark — see [`HsLinkNode`].)
    pub fn json_type(&self) -> &'static str {
        match self {
            Mark::Bold => "bold",
            Mark::Italic => "italic",
            Mark::Underline => "underline",
            Mark::Strike => "strike",
            Mark::Code => "code",
            Mark::Link { .. } => "link",
        }
    }

    /// True when `self` and `other` are the SAME mark type (ignoring payload). Two
    /// links are the same type even with different hrefs; `RemoveMark` over a range
    /// removes every mark of the requested type regardless of payload, matching the
    /// ProseMirror "remove mark by type" semantics.
    pub fn same_type(&self, other: &Mark) -> bool {
        self.json_type() == other.json_type()
    }
}

/// A leaf of inline text plus the marks applied to the whole run. Text storage is a
/// rope ([`RopeText`]) so an insert/delete at an arbitrary char position is
/// O(log n) — mandatory for the IME + caret work in MT-012 (MT impl note 1).
///
/// `Clone + Debug + PartialEq` so the undo manager can cheaply clone the doc for the
/// undo stack (MT impl note 2). No `Arc<Mutex<>>` at this layer; the renderer takes
/// a read reference.
#[derive(Debug, Clone, PartialEq)]
pub struct TextLeaf {
    /// The run's text, rope-backed and char-addressed.
    pub text: RopeText,
    /// Marks applied to the whole run (dedup'd by type via [`TextLeaf::add_mark`]).
    pub marks: Vec<Mark>,
}

impl TextLeaf {
    /// Build a text leaf from a string with no marks.
    pub fn new(text: &str) -> Self {
        Self {
            text: RopeText::from_str(text),
            marks: Vec::new(),
        }
    }

    /// Build a text leaf with an explicit mark set.
    pub fn with_marks(text: &str, marks: Vec<Mark>) -> Self {
        Self {
            text: RopeText::from_str(text),
            marks,
        }
    }

    /// Add a mark, replacing any existing mark of the SAME type (so a run never
    /// carries two `Bold`s, and re-linking replaces the href). Returns the mark that
    /// was displaced, if any, so the transform layer can build the inverse step.
    pub fn add_mark(&mut self, mark: Mark) -> Option<Mark> {
        let displaced = self
            .marks
            .iter()
            .position(|m| m.same_type(&mark))
            .map(|i| self.marks.remove(i));
        self.marks.push(mark);
        displaced
    }

    /// Remove every mark of the given type. Returns the removed marks in order so
    /// the transform layer can build the inverse (re-add) step.
    pub fn remove_marks_of_type(&mut self, ty: &Mark) -> Vec<Mark> {
        let mut removed = Vec::new();
        self.marks.retain(|m| {
            if m.same_type(ty) {
                removed.push(m.clone());
                false
            } else {
                true
            }
        });
        removed
    }

    /// True when this leaf carries a mark of the given type.
    pub fn has_mark_type(&self, ty: &Mark) -> bool {
        self.marks.iter().any(|m| m.same_type(ty))
    }
}

/// A typed Handshake wikilink as an inline ATOM node (`hsLink`), matching the REAL
/// backend `content_json` shape in `app/src/lib/tiptap/hs_link_node.ts`.
///
/// It is an inline atom: a single indivisible inline token that lives inside a
/// paragraph/heading `content` array as a sibling of text leaves (NOT a mark on a
/// text run). The backend persists it as `{type:"hsLink", attrs:{refKind, refValue,
/// label, resolved}}`. `refKind` is the backend ref kind
/// (`RichDocBacklink.link_kind` / `RichDocEmbed.ref_kind`); an unrecognized prefix is
/// preserved as `refKind="unknown"` with `resolved=false` (never silently dropped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HsLinkNode {
    /// The backend ref kind (`"wp"`, `"file"`, …) or `"unknown"`. Defaults to
    /// `"unknown"` in the React node when absent.
    pub ref_kind: String,
    /// The target value after the prefix (e.g. `"WP-KERNEL-012"`, `"src/app.ts"`).
    pub ref_value: String,
    /// Display label; the React node defaults this to the empty string (rendered as
    /// `refKind:refValue` when blank).
    pub label: String,
    /// Whether the prefix matched a known wikilink kind. The React node defaults this
    /// to `true` and forces it to `false` for an unknown/spoofed kind.
    pub resolved: bool,
    /// Optional evidence provenance carried by embed-style links. Ordinary wikilinks leave this absent;
    /// Stage capture embeds persist the exact `{source,artifact_id,sha256,manifest_ref}` descriptor here.
    pub provenance: Option<serde_json::Value>,
    /// The persisted target class, independent of the note's `ref_value`.
    pub target_kind: Option<ReferenceTargetKind>,
    /// The persisted heading/block identity. Never derived from a tree path.
    pub target_block_id: Option<String>,
    /// Marked tags store an exact raw full label. Unmarked labels remain opaque.
    pub tag_ref_kind: Option<TagReferenceKind>,
}

/// Explicit meaning of a tag's `ref_value` and raw-label contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagReferenceKind {
    HubId,
    NormalizedName,
}

impl TagReferenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HubId => "hub_id",
            Self::NormalizedName => "normalized_name",
        }
    }
    pub fn from_json_type(value: &str) -> Option<Self> {
        match value {
            "hub_id" => Some(Self::HubId),
            "normalized_name" => Some(Self::NormalizedName),
            _ => None,
        }
    }
}

/// Host resolution must keep unclassified historical values distinct from IDs
/// and names. Neither value syntax nor lookup failure determines their meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagReference<'a> {
    HubId {
        id: &'a str,
        raw_label: &'a str,
    },
    NormalizedName {
        canonical_name: &'a str,
        raw_label: &'a str,
    },
    Unclassified {
        raw_value: &'a str,
        stored_label: &'a str,
    },
}

impl<'a> TagReference<'a> {
    /// Plain chip text. Marked labels gain one syntax prefix without stripping a
    /// literal leading '#'. Historical labels remain verbatim. Hosts own escaping,
    /// icons, navigation and Markdown export separately.
    pub fn display_label(self) -> Cow<'a, str> {
        match self {
            Self::HubId { raw_label, .. } | Self::NormalizedName { raw_label, .. } => {
                Cow::Owned(format!("#{raw_label}"))
            }
            Self::Unclassified {
                raw_value,
                stored_label,
            } => Cow::Borrowed(if stored_label.is_empty() {
                raw_value
            } else {
                stored_label
            }),
        }
    }
}

/// Notes reference destinations admitted by the canonical rich-document format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceTargetKind {
    Note,
    Heading,
    Block,
}

impl ReferenceTargetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Heading => "heading",
            Self::Block => "block",
        }
    }

    pub fn from_json_type(value: &str) -> Option<Self> {
        match value {
            "note" => Some(Self::Note),
            "heading" => Some(Self::Heading),
            "block" => Some(Self::Block),
            _ => None,
        }
    }
}

pub(crate) struct ReferencePayload<'a> {
    pub ref_value: &'a str,
    pub target_kind: Option<ReferenceTargetKind>,
    pub target_block_id: Option<&'a str>,
}

pub(crate) fn parse_reference_payload<'a>(
    attr: impl Fn(&str) -> Option<&'a JsonValue>,
) -> Result<ReferencePayload<'a>, &'static str> {
    let ref_value = attr("refValue")
        .and_then(JsonValue::as_str)
        .ok_or("refValue")?;
    let target_kind = match attr("target_kind") {
        None | Some(JsonValue::Null) => None,
        Some(value) => Some(
            value
                .as_str()
                .and_then(ReferenceTargetKind::from_json_type)
                .ok_or("target_kind")?,
        ),
    };
    let target_block_id = match attr("target_block_id") {
        None | Some(JsonValue::Null) => None,
        Some(value) => Some(value.as_str().ok_or("target_block_id")?),
    };
    Ok(ReferencePayload {
        ref_value,
        target_kind,
        target_block_id,
    })
}

impl HsLinkNode {
    /// Build an hsLink node, defaulting `resolved` to `true` (the React node default).
    pub fn new(
        ref_kind: impl Into<String>,
        ref_value: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            ref_kind: ref_kind.into(),
            ref_value: ref_value.into(),
            label: label.into(),
            resolved: true,
            provenance: None,
            target_kind: None,
            target_block_id: None,
            tag_ref_kind: None,
        }
    }

    pub fn tag_hub_id(id: impl Into<String>, raw_label: impl Into<String>) -> Self {
        let mut link = Self::new("tag", id, raw_label);
        link.tag_ref_kind = Some(TagReferenceKind::HubId);
        link
    }

    /// The host supplies the canonical identity from its proved legacy grammar.
    /// This constructor never parses or normalizes the full raw label.
    pub fn tag_normalized_name(name: impl Into<String>, raw_label: impl Into<String>) -> Self {
        let mut link = Self::new("tag", name, raw_label);
        link.tag_ref_kind = Some(TagReferenceKind::NormalizedName);
        link
    }

    pub fn tag_reference(&self) -> Option<TagReference<'_>> {
        if self.ref_kind != "tag" {
            return None;
        }
        Some(match self.tag_ref_kind {
            Some(TagReferenceKind::HubId) => TagReference::HubId {
                id: &self.ref_value,
                raw_label: &self.label,
            },
            Some(TagReferenceKind::NormalizedName) => TagReference::NormalizedName {
                canonical_name: &self.ref_value,
                raw_label: &self.label,
            },
            None => TagReference::Unclassified {
                raw_value: &self.ref_value,
                stored_label: &self.label,
            },
        })
    }
}

/// A note-transclusion reference as an inline ATOM node (`loomTransclusion`), matching the REAL
/// backend `content_json` shape used by `app/src/components/LoomTransclusionView.tsx` (the host
/// document stores ONLY `{type:"loomTransclusion", attrs:{refValue}}` — never the transcluded body;
/// the body is resolved at view time through `GET /workspaces/{ws}/loom/blocks/{refValue}/transclusion`).
///
/// MT-015 NODE-SHAPE RECONCILIATION (KERNEL_BUILDER gate): the MT contract describes a
/// `TransclusionNode` with a `{block_id}` attr, but MT-011's `NodeKind` had no transclusion variant
/// and the REAL React node carries `refValue` (NOT `block_id`). Rather than invent a `{block_id}`
/// shape that would not round-trip, this is added as the inline atom [`Child::Transclusion`] carrying
/// `ref_value` (the backend block id the read-through resolves), anchored to the verified backend
/// shape. The `loomTransclusion` node is selectable and atom-like exactly as the `hsLink` node is, so
/// it lives in [`Child`] alongside it (one unified inline-atom family), NOT as a block-level
/// [`NodeKind`] (block-level atoms are `hard_break`/`horizontal_rule`, which carry no attrs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransclusionNode {
    /// The transcluded LoomBlock id the read-through resolves (`attrs.refValue` in the backend
    /// `content_json`). The host never stores the body — only this reference.
    pub ref_value: String,
}

impl TransclusionNode {
    /// Build a transclusion atom referencing the LoomBlock `ref_value`.
    pub fn new(ref_value: impl Into<String>) -> Self {
        Self {
            ref_value: ref_value.into(),
        }
    }
}

/// A child of a [`BlockNode`]: a nested block, a run of inline text, or an inline
/// atom (`hsLink`).
///
/// A `paragraph`/`heading` holds `Text` and `HsLink` inline children; `code_block`
/// holds `Text` only; every other container kind holds `Block` children. The schema
/// ([`super::schema`]) enforces which is allowed where; this enum just makes them
/// representable in one `Vec`.
#[derive(Debug, Clone, PartialEq)]
pub enum Child {
    /// A nested block node.
    Block(BlockNode),
    /// A run of inline text with marks.
    Text(TextLeaf),
    /// An inline atom: a typed `hsLink` wikilink node.
    HsLink(HsLinkNode),
    /// An inline atom: a `loomTransclusion` note-transclusion reference (MT-015).
    Transclusion(TransclusionNode),
}

impl Child {
    /// Borrow the child as a block node, or `None` if it is not one.
    pub fn as_block(&self) -> Option<&BlockNode> {
        match self {
            Child::Block(b) => Some(b),
            Child::Text(_) | Child::HsLink(_) | Child::Transclusion(_) => None,
        }
    }

    /// Mutably borrow the child as a block node, or `None` if it is not one.
    pub fn as_block_mut(&mut self) -> Option<&mut BlockNode> {
        match self {
            Child::Block(b) => Some(b),
            Child::Text(_) | Child::HsLink(_) | Child::Transclusion(_) => None,
        }
    }

    /// Borrow the child as a text leaf, or `None` if it is not one.
    pub fn as_text(&self) -> Option<&TextLeaf> {
        match self {
            Child::Text(t) => Some(t),
            Child::Block(_) | Child::HsLink(_) | Child::Transclusion(_) => None,
        }
    }

    /// Mutably borrow the child as a text leaf, or `None` if it is not one.
    pub fn as_text_mut(&mut self) -> Option<&mut TextLeaf> {
        match self {
            Child::Text(t) => Some(t),
            Child::Block(_) | Child::HsLink(_) | Child::Transclusion(_) => None,
        }
    }

    /// Borrow the child as an hsLink atom, or `None` if it is not one.
    pub fn as_hs_link(&self) -> Option<&HsLinkNode> {
        match self {
            Child::HsLink(l) => Some(l),
            Child::Block(_) | Child::Text(_) | Child::Transclusion(_) => None,
        }
    }

    /// Borrow the child as a `loomTransclusion` atom, or `None` if it is not one (MT-015).
    pub fn as_transclusion(&self) -> Option<&TransclusionNode> {
        match self {
            Child::Transclusion(t) => Some(t),
            Child::Block(_) | Child::Text(_) | Child::HsLink(_) => None,
        }
    }

    /// The char length this child contributes to a flat document offset: a text
    /// leaf contributes its char count; a block contributes the sum of its
    /// descendants ([`BlockNode::char_len`]); an `hsLink` / `loomTransclusion` inline
    /// atom contributes 1 (a ProseMirror leaf/atom node has size 1 in document positions).
    pub fn char_len(&self) -> usize {
        match self {
            Child::Text(t) => t.text.len_chars(),
            Child::Block(b) => b.char_len(),
            Child::HsLink(_) | Child::Transclusion(_) => 1,
        }
    }
}

/// A block node in the document tree: a typed kind, free-form `attrs` (heading
/// level, code-block language, task-item checked, …), and an ordered list of
/// children.
///
/// `attrs` stores `serde_json::Value` so an attr that is not in the base schema
/// (e.g. a forward-compat key from a newer document) round-trips losslessly rather
/// than being dropped (red-team RISK-3). The well-known attrs (`level`, `checked`,
/// `language`) have typed accessors.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockNode {
    /// The typed node kind.
    pub kind: NodeKind,
    /// Free-form node attributes, serialized as a JSON object under the `attrs` key.
    pub attrs: HashMap<String, JsonValue>,
    /// Ordered children (blocks or text leaves per the schema).
    pub children: Vec<Child>,
}

impl BlockNode {
    /// Build a block node of `kind` with no attrs and no children.
    pub fn new(kind: NodeKind) -> Self {
        Self {
            kind,
            attrs: HashMap::new(),
            children: Vec::new(),
        }
    }

    /// Build a block node with explicit children.
    pub fn with_children(kind: NodeKind, children: Vec<Child>) -> Self {
        Self {
            kind,
            attrs: HashMap::new(),
            children,
        }
    }

    /// A `doc` root wrapping `blocks`.
    pub fn doc(blocks: Vec<BlockNode>) -> Self {
        Self::with_children(
            NodeKind::Doc,
            blocks.into_iter().map(Child::Block).collect(),
        )
    }

    /// A `paragraph` holding a single text leaf with the given text and no marks.
    pub fn paragraph(text: &str) -> Self {
        Self::with_children(NodeKind::Paragraph, vec![Child::Text(TextLeaf::new(text))])
    }

    /// A `heading` at `level` holding a single text leaf.
    ///
    /// The level is NOT stored in the free-form `attrs` map: the typed
    /// [`NodeKind::Heading`] variant owns it, and [`super::doc_json`] stamps
    /// `attrs.level` only on the WIRE during serialization (and strips it on
    /// deserialize). Keeping `attrs` free of the typed `level` makes the in-memory
    /// representation canonical, so a `heading(1, …)` round-trips through DocJson to
    /// an EQUAL node (the round-trip equality test depends on this).
    pub fn heading(level: u8, text: &str) -> Self {
        Self::with_children(
            NodeKind::Heading(HeadingLevel::new(level)),
            vec![Child::Text(TextLeaf::new(text))],
        )
    }

    /// The total char length of all text in this node's subtree, used to map a flat
    /// absolute offset to a tree path ([`super::position`]).
    pub fn char_len(&self) -> usize {
        self.children.iter().map(Child::char_len).sum()
    }

    /// The heading level if this is a heading, else `None`.
    pub fn heading_level(&self) -> Option<u8> {
        match self.kind {
            NodeKind::Heading(l) => Some(l.get()),
            _ => None,
        }
    }

    /// The `checked` attr of a task item (`false` if absent / not a task item).
    pub fn task_checked(&self) -> bool {
        self.attrs
            .get("checked")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false)
    }
}
