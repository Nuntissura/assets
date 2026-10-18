//! Block document model for the native Rust rich-text editor (WP-KERNEL-012 MT-011).
//!
//! This module is the BRAIN of the E2 rich-text cluster (MT-012..MT-020 all bind to
//! it). It is a ProseMirror/Tiptap-style typed block tree on `ropey`, with an atomic
//! typed transform/step system, a bounded undo/redo history, and DocJson
//! serialization to the exact backend `content_json` shape (`rich_document_v1`).
//!
//! ## Submodules
//!
//! - [`node`] — [`BlockNode`], [`Child`], [`TextLeaf`], [`NodeKind`], [`Mark`],
//!   [`HeadingLevel`]: the typed tree + inline-mark types.
//! - [`rope_text`] — [`RopeText`], the char-addressed `ropey::Rope` wrapper that
//!   makes byte-index corruption (RISK-1) unrepresentable.
//! - [`schema`] — the compile-time content-model + mark validation run on every
//!   transaction apply.
//! - [`transform`] — [`Step`], [`Transaction`], [`apply_transaction`]: atomic
//!   (clone-before-apply, rollback-on-error) document mutation with inverse capture.
//! - [`history`] — [`UndoManager`]: bounded (default 200) undo/redo over receipts.
//! - [`position`] — [`DocPosition`], [`resolve`] / [`absolute_offset`]: tree-path
//!   ↔ flat-char-offset mapping.
//! - [`selection`] — [`Selection`]: text-range + whole-node selection shapes.
//! - [`doc_json`] — [`RichDocument`] envelope + serde round-trip to the Tiptap
//!   JSONContent backend shape ([`doc_json::RICH_DOCUMENT_SCHEMA_VERSION`]).
//!
//! ## AccessKit note
//!
//! This layer has NO UI widgets, so it assigns NO AccessKit author_ids (MT impl
//! note). The renderer (MT-012) consumes this model and owns the AccessKit surface.

pub mod doc_json;
pub mod edit_state;
pub mod find_scan;
pub mod history;
pub mod node;
pub mod position;
pub mod rope_text;
pub mod schema;
pub mod selection;
pub mod transform;

pub use doc_json::{
    block_to_json, from_json_string, from_json_value, to_content_json_value, to_json_string,
    to_rich_document, DocJsonError, JsonMark, JsonNode, RichDocument, RICH_DOCUMENT_SCHEMA_VERSION,
};
pub use edit_state::{DocumentEditState, EditCommit, EditError};
pub use history::{UndoManager, DEFAULT_HISTORY_CAP};
pub use node::{BlockNode, Child, HeadingLevel, HsLinkNode, Mark, NodeKind, TextLeaf};
pub use position::{absolute_offset, resolve, DocPosition};
pub use rope_text::RopeText;
pub use schema::{
    block_child_allowed, mark_allowed, text_child_allowed, validate_node, validate_tree,
    SchemaError,
};
pub use selection::Selection;
pub use transform::{
    apply_transaction, ActorKind, Step, Transaction, TransactionReceipt, TransformError,
};
