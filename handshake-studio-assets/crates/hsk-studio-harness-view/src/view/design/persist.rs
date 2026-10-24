//! Instance state saved with the document (lane SMB-FIG-EDIT-2; [STU-IO-006] row 35 edit-preserve,
//! [STU-DS-003/008/016/017]).
//!
//! Every design command ([`super::DesignCommand`]) writes the session document entry [`ENTRY`]
//! (`design`): the Folio `StudioDesignStateRecords` of the document, one `StudioInstanceState` per
//! instance layer and content (the layer's child ids after the redraw). The entry is written after
//! the redraw's chronicle step, which stays the command's ONE history step; the entry is saved with
//! the package and read back on open, so an edited instance reads `edited` after a save and reopen
//! and takes further commands.
//!
//! Undo: `Run::restructure` (the redraw) carries no document entry in its history step (only
//! `set_tiles_and_entry` and `set_vector_paths_and_entry` do), so undo cannot restore an earlier
//! entry. The entry therefore keeps a state per (layer, content) (Folio reading): undo and redo
//! return the layer to earlier children and the state recorded for those children applies again,
//! also after a reopen; the newest [`MAX_PERSISTED_STATES`] are kept.
//!
//! Reading: Folio validates the entry against the document and the receipt's design records. A
//! refusal that names one state (`instances/<i>/...`) drops that state and validates the rest
//! again (each dropped state's diagnostic is reported; an instance whose current content had such
//! a state reads `unknown` with that reason); any other refusal (junk, another document) reads as
//! the entry diagnostic, every instance without a state of this session reads `import` or
//! `unknown`, and commands are refused with the stored bytes unchanged. Without design records the
//! entry is validated against the document only (`design_records_absent`).
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use hsk_studio_folio::{
    self as folio, Budget, StudioComponentInstance, StudioDesignRecords, StudioDesignStateRecords, StudioInstanceState, StudioOverrideRecord,
    StudioStateOverride,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Session document entry holding the design state records (JSON).
pub const ENTRY: &str = "design";
/// Newest instance states one entry keeps (the oldest goes first).
pub const MAX_PERSISTED_STATES: usize = 256;

/// `Code::InvalidJson` -> `invalid_json`.
pub(super) fn snake(code: folio::Code) -> String {
    let text = format!("{code:?}");
    let mut out = String::with_capacity(text.len().saturating_add(4));
    for (i, ch) in text.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn diag(d: &folio::Diagnostic) -> Value {
    json!({"code": snake(d.code), "target": d.target})
}

/// The `design` entry as read (module docs).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EntryRead {
    /// Whether the session holds the entry.
    pub present: bool,
    /// The states Folio admits (after dropping refused states).
    pub records: Option<StudioDesignStateRecords>,
    /// A refusal of the whole entry (junk, another document): the entry diagnostic.
    pub error: Option<folio::Diagnostic>,
    /// States dropped with their diagnostic: (layer id, content, diagnostic).
    pub dropped: Vec<(String, Vec<String>, folio::Diagnostic)>,
    /// No design records were available to validate against (document only).
    pub design_records_absent: bool,
}

impl EntryRead {
    /// The diagnostic that explains why `layer_id` with children `content` has no admitted state.
    pub fn reason(&self, layer_id: &str, content: &[String]) -> Option<String> {
        if let Some((_, _, d)) = self.dropped.iter().find(|(l, c, _)| l == layer_id && c == content) {
            return Some(format!("design_state_refused: {} {}", snake(d.code), d.target));
        }
        self.error.as_ref().map(|d| format!("design_state_entry_invalid: {} {}", snake(d.code), d.target))
    }

    pub fn json(&self) -> Value {
        json!({
            "present": self.present,
            "states": self.records.as_ref().map(|r| r.instances.len()),
            "error": self.error.as_ref().map(diag),
            "dropped": self.dropped.iter().map(|(l, _, d)| json!({"layer_id": l, "error": diag(d)})).collect::<Vec<_>>(),
            "design_records_absent": self.design_records_absent,
        })
    }
}

/// The index of the state a diagnostic names (`instances/<i>/...`).
fn state_index(target: &str) -> Option<usize> {
    target.strip_prefix("instances/")?.split('/').next()?.parse().ok()
}

/// The instance layer's child ids in document order.
pub(super) fn content(doc: &folio::StudioDocument, layer_id: &str) -> Vec<String> {
    doc.layers.iter().filter(|l| matches!(&l.parent, folio::Parent::Node(p) if p.node_id == layer_id)).map(|l| l.layer_id.clone()).collect()
}

/// The record a state restores: the import record with the state's member, properties and overrides.
pub(super) fn record_of(import: &StudioComponentInstance, s: &StudioInstanceState) -> StudioComponentInstance {
    let mut r = import.clone();
    r.main_component_id = Some(s.component_id.clone());
    r.component_properties = s.properties.clone();
    r.overrides = Vec::new();
    for o in &s.overrides {
        let at = match r.overrides.iter().position(|x| x.overridden_node_id == o.node_id) {
            Some(i) => i,
            None => {
                r.overrides.push(StudioOverrideRecord { overridden_node_id: o.node_id.clone(), overridden_fields: Vec::new(), values: BTreeMap::new() });
                r.overrides.len().saturating_sub(1)
            }
        };
        if let Some(rec) = r.overrides.get_mut(at) {
            rec.overridden_fields.push(o.field.clone());
            rec.values.insert(o.field.clone(), o.value.clone());
        }
    }
    r
}

/// The state of `record` drawn by `layer_id` with children `content`. Override fields without a
/// value cannot be stored (a named loss: returned as `false`).
pub(super) fn state_of(
    record: &StudioComponentInstance,
    layer_id: &str,
    content: Vec<String>,
    variant: BTreeMap<String, String>,
    swap_target: Option<String>,
) -> Result<(StudioInstanceState, bool), String> {
    let component_id = record.main_component_id.clone().ok_or("design: the instance has no main component (deleted-main state is not stored)")?;
    let mut overrides = Vec::new();
    let mut complete = true;
    for o in &record.overrides {
        for f in &o.overridden_fields {
            match o.values.get(f) {
                Some(v) => overrides.push(StudioStateOverride { node_id: o.overridden_node_id.clone(), field: f.clone(), value: v.clone() }),
                None => complete = false,
            }
        }
    }
    Ok((
        StudioInstanceState {
            layer_id: layer_id.to_owned(),
            instance_id: record.instance_id.clone(),
            component_id,
            variant,
            properties: record.component_properties.clone(),
            overrides,
            swap_target,
            content_signature: content,
        },
        complete,
    ))
}

impl Viewer {
    /// Reads the `design` entry and validates it (module docs); `design` = the receipt's records.
    pub(super) fn read_design_state(&self, design: Option<&StudioDesignRecords>) -> EntryRead {
        let mut out = EntryRead { design_records_absent: design.is_none(), ..EntryRead::default() };
        let Some(snap) = self.run.snapshot() else { return out };
        let Some(bytes) = self.run.document_entry(ENTRY) else { return out };
        out.present = true;
        let doc = snap.document();
        let budget = Budget::default();
        let first = match folio::parse_design_state_records(bytes, doc, design, budget, self.cancel) {
            Ok(r) => {
                out.records = Some(r);
                return out;
            }
            Err(e) => e,
        };
        // A refusal naming one state comes after the bounded parse succeeded: drop it and retry.
        let (Some(_), Ok(mut records)) = (state_index(&first.target), serde_json::from_slice::<StudioDesignStateRecords>(bytes)) else {
            out.error = Some(first);
            return out;
        };
        let mut e = first;
        for _ in 0..=records.instances.len() {
            let Some(i) = state_index(&e.target).filter(|i| *i < records.instances.len()) else {
                out.error = Some(e);
                return out;
            };
            let s = records.instances.remove(i);
            out.dropped.push((s.layer_id, s.content_signature, e));
            match folio::validate_design_state_records(&records, doc, design, budget, self.cancel) {
                Ok(()) => {
                    out.records = Some(records);
                    return out;
                }
                Err(next) => e = next,
            }
        }
        out.error = Some(e);
        out
    }

    /// The design state records of the open document as Folio admits them (`Ok(None)`: none stored
    /// or nothing open); a refusal of the whole entry is its Folio diagnostic.
    pub fn design_state_records(&self) -> Result<Option<StudioDesignStateRecords>, folio::Diagnostic> {
        let receipt = self.design_receipt();
        let read = self.read_design_state(receipt.as_ref().map(|r| &r.records));
        match read.error {
            Some(e) => Err(e),
            None => Ok(read.records),
        }
    }

    /// Stores `state` in the `design` entry beside the admitted states (newest kept), validated by
    /// Folio first. Returns notes (states dropped as stale or over the bound).
    pub(super) fn write_design_state(&mut self, mut base: EntryRead, design: &StudioDesignRecords, state: StudioInstanceState) -> Result<Vec<String>, String> {
        let doc = self.run.snapshot().ok_or("nothing open")?.document().clone();
        let mut notes = Vec::new();
        if !base.dropped.is_empty() {
            notes.push("refused_design_states_dropped".to_owned());
        }
        let mut records = base.records.take().unwrap_or_else(|| StudioDesignStateRecords::new(doc.document_id.clone()));
        records.instances.retain(|s| !(s.layer_id == state.layer_id && s.content_signature == state.content_signature));
        records.instances.push(state);
        if records.instances.len() > MAX_PERSISTED_STATES {
            let extra = records.instances.len().saturating_sub(MAX_PERSISTED_STATES);
            records.instances.drain(..extra);
            notes.push("oldest_design_states_dropped".to_owned());
        }
        folio::validate_design_state_records(&records, &doc, Some(design), Budget::default(), self.cancel)
            .map_err(|d| format!("design: design_state {} {}", snake(d.code), d.target))?;
        let bytes = serde_json::to_vec(&records).map_err(|e| format!("design: design_state {e}"))?;
        self.run.set_document_entry(ENTRY, Some(bytes)).map_err(|code| format!("design: design entry {code}"))?;
        Ok(notes)
    }
}
