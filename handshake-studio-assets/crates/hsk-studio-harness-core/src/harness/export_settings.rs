//! `export.set_settings` of [`Run`] (lane SMB-ARTBOARD-EXPORT-1; [STU-IO-111] to [STU-IO-114],
//! [STU-IO-117]): the node export settings of the open document (Folio `StudioExportSettings`) in the
//! session document entry [`EXPORT_SETTINGS_ENTRY`], saved with the package and restored on reopen.
//! A change is one entry-only undo step (no revision, like every document entry) with the caller's
//! attribution, one op record and one Observe frame of codes and counters; a refusal changes nothing.
//! Not recordable: a recording reports it typed ([STU-AUT-109]).
#![deny(clippy::arithmetic_side_effects)]

use super::{EntryRestore, HistoryStep, Run};
use hsk_studio_folio::{self as folio, Code, NodeKind, NodeRef, StudioExportSettings, StudioNodeRenderSetting};
use serde_json::{Value, json};

/// The session document entry holding the node export settings (`hsk_studio_session::DOCUMENT_ENTRY_TOKENS`).
pub const EXPORT_SETTINGS_ENTRY: &str = "export_settings";

/// The budget a stored record set is read and judged under: the record's own bounds.
pub fn budget() -> folio::Budget {
    folio::Budget {
        input_bytes: folio::MAX_EXPORT_SETTINGS_INPUT_BYTES,
        nodes: folio::MAX_EXPORT_NODES.saturating_mul(folio::MAX_SETTINGS_PER_NODE.saturating_add(1)),
        payload_bytes: folio::MAX_EXPORT_SETTINGS_INPUT_BYTES,
        ..folio::Budget::default()
    }
}

/// A Folio verdict on export settings as the command's stable code.
pub fn refusal_code(d: &folio::Diagnostic) -> &'static str {
    match (d.code, d.target.as_str()) {
        (_, "export_settings/constraint") => "export_constraint_out_of_range",
        (Code::DuplicateIdentity, "export_settings/suffix") => "export_suffix_collision",
        (_, "export_settings/suffix") => "export_suffix_invalid",
        (Code::InvalidField, "export_settings/settings") => "export_settings_empty",
        (Code::BudgetExceeded, _) => "export_settings_too_large",
        (_, "export_settings/node_kind" | "export_settings/node") => "export_node_invalid",
        _ => "export_settings_invalid",
    }
}

impl Run<'_> {
    /// The node export settings of the open document: an empty set when none is stored.
    /// `not_open`; `export_settings_invalid` when the stored entry does not parse.
    pub fn export_settings(&self) -> Result<StudioExportSettings, &'static str> {
        let session = self.session.as_ref().ok_or("not_open")?;
        let document = session.snapshot().document();
        match session.document_entry(EXPORT_SETTINGS_ENTRY) {
            None => Ok(StudioExportSettings::new(document.document_id.clone())),
            Some(bytes) => folio::parse_export_settings(bytes, document, budget(), self.cancel).map_err(|_| "export_settings_invalid"),
        }
    }

    /// `export.set_settings`: replaces the render settings of `node` (a layer or artboard the document
    /// holds); an empty list removes them. The same settings again are no step. Refusals: `not_open`,
    /// `canceled`, `unknown_node`, `export_settings_invalid` (a stored entry that does not parse is never
    /// replaced by accident), [`refusal_code`]'s codes. One record `set_export_settings`, detail
    /// `{node, settings, nodes, changed}`.
    pub fn set_export_settings(&mut self, node: &NodeRef, settings: Vec<StudioNodeRenderSetting>) -> bool {
        const OP: &str = "set_export_settings";
        const COMMAND: &str = "export.set_settings";
        self.note_unrecordable(COMMAND);
        let before = self.frames.count();
        if self.session.is_none() {
            return self.push(OP, before, Err("not_open"), None, None);
        }
        let ctx = self.begin(COMMAND, None);
        if self.cancel.check().is_err() {
            return self.own_op(OP, before, Err("canceled"));
        }
        let cancel = self.cancel;
        let Some(session) = self.session.as_mut() else {
            return self.push(OP, before, Err("not_open"), None, None);
        };
        let previous = session.document_entry(EXPORT_SETTINGS_ENTRY).map(<[u8]>::to_vec);
        let written = {
            let document = session.snapshot().document();
            let held = match node.node_kind {
                NodeKind::Layer => document.layers.iter().any(|l| l.layer_id == node.node_id),
                NodeKind::Artboard => document.artboards.iter().any(|a| a.artboard_id == node.node_id),
                NodeKind::PageSpread => false,
            };
            (|| {
                if !held {
                    return Err("unknown_node");
                }
                let current = match &previous {
                    Some(bytes) => folio::parse_export_settings(bytes, document, budget(), cancel).map_err(|_| "export_settings_invalid")?,
                    None => StudioExportSettings::new(document.document_id.clone()),
                };
                let count = settings.len();
                let next = current.with_node(node, settings);
                folio::validate_export_settings(&next, document, budget(), cancel).map_err(|d| refusal_code(&d))?;
                let changed = next != current;
                let bytes = if next.nodes.is_empty() { None } else { Some(serde_json::to_vec(&next).map_err(|_| "export_settings_encode")?) };
                Ok::<_, &'static str>((bytes, changed, count, next.nodes.len()))
            })()
        };
        let (bytes, changed, count, nodes) = match written {
            Ok(done) => done,
            Err(code) => return self.own_op(OP, before, Err(code)),
        };
        if changed {
            if let Err(e) = session.set_document_entry(EXPORT_SETTINGS_ENTRY, bytes, cancel) {
                return self.own_op(OP, before, Err(e.code()));
            }
            let revision = session.document_revision();
            let restore = EntryRestore { token: EXPORT_SETTINGS_ENTRY.to_string(), bytes: previous };
            self.undo_stack.push(HistoryStep { label: format!("{OP} {}", node.node_id), inverse: Value::Null, entry: Some(restore), timeline: None, group: None, sequences: None, containers: None, layout: None, ctx, revision });
            self.redo_stack.clear();
        }
        self.own_op(OP, before, Ok(json!({"node": node.node_id, "settings": count, "nodes": nodes, "changed": changed})))
    }
}
