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
//! also after a reopen. No later write prunes a state while the session lives (lead ruling
//! 2026-11-11 on review F1/F2 of lane SMB-DS-COMPONENTS-AUTHOR-1): a write past
//! [`MAX_PERSISTED_STATES`] (Folio's bound) is refused `design_state_over_cap` before anything is
//! written, never evicted. States whose layer is gone are dropped when a document is opened with no
//! history (`Viewer::design_opened`): nothing can return to them then.
//!
//! Native bindings (lane SMB-DS-COMPONENTS-AUTHOR-1): a state whose instance is not one of the design
//! file's is the binding of an authored instance (layer, instance, content). Its record is the vector
//! entry's, which the history restores, so a binding is never refused by Folio on read: an instance
//! whose main, layer or record is gone for now (a deleted definition, a deleted or detached instance,
//! an override of a deleted definition child) keeps its binding for the undo that brings it back. A
//! new binding is checked by Folio when it is written and its record names a main.
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
    self as folio, Budget, ComponentPropertyType, StudioComponentInstance, StudioDesignRecords, StudioDesignStateRecords, StudioInstanceState,
    StudioOverrideRecord, StudioPropertyValue, StudioStateOverride,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Session document entry holding the design state records (JSON).
pub const ENTRY: &str = "design";
/// Most instance states one entry holds (Folio's bound); a write past it is refused, never evicted.
pub const MAX_PERSISTED_STATES: usize = folio::MAX_DESIGN_STATES;

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
    /// The states Folio admits (after dropping refused states), followed by the native bindings
    /// (module docs: not refused on read).
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

/// The variant assignment a native state of `record` carries: its main's (a set member's) with the
/// record's VARIANT property values applied (Folio's expected assignment).
pub(super) fn native_variant(record: &StudioComponentInstance, design: &StudioDesignRecords) -> BTreeMap<String, String> {
    let mut variant = record.main_component_id.as_ref().and_then(|main| design.components.iter().find(|k| &k.component_id == main)).and_then(|k| k.variant_properties.clone()).unwrap_or_default();
    for (name, p) in &record.component_properties {
        if let (ComponentPropertyType::Variant, StudioPropertyValue::Text(v)) = (p.property_type, &p.value) {
            variant.insert(name.clone(), v.clone());
        }
    }
    variant
}

/// The entry's states parsed without Folio, bounded (`None`: over the input budget or Folio's state
/// bound, or not parseable: Folio's read then names the diagnostic).
pub(super) fn raw_states(bytes: &[u8]) -> Option<StudioDesignStateRecords> {
    if bytes.len() > Budget::default().input_bytes {
        return None;
    }
    serde_json::from_slice::<StudioDesignStateRecords>(bytes).ok().filter(|r| r.instances.len() <= folio::MAX_DESIGN_STATES)
}

/// Splits the native bindings off the stored entry (module docs): the bytes of the rest for Folio
/// (`None`: the stored bytes as they are) and the native states.
fn split_native(bytes: &[u8], design_file: &BTreeSet<String>) -> (Option<Vec<u8>>, Vec<StudioInstanceState>) {
    let Some(mut records) = raw_states(bytes) else { return (None, Vec::new()) };
    let (fig, native): (Vec<_>, Vec<_>) = records.instances.drain(..).partition(|s| design_file.contains(&s.instance_id));
    if native.is_empty() {
        return (None, Vec::new());
    }
    records.instances = fig;
    (serde_json::to_vec(&records).ok(), native)
}

/// Refused `design_state_over_cap` when `more` new states would pass [`MAX_PERSISTED_STATES`]
/// (checked before a step that writes them, F2).
pub(super) fn room_for(stored: Option<&[u8]>, more: usize) -> Result<(), String> {
    let held = stored.and_then(raw_states).map_or(0, |r| r.instances.len());
    if held.saturating_add(more) > MAX_PERSISTED_STATES {
        return Err(format!("design: design_state_over_cap (the document keeps at most {MAX_PERSISTED_STATES} instance states)"));
    }
    Ok(())
}

/// Appends `state` to `records` (replacing the state of the same layer, content and instance: a layer id
/// a delete gave up can be minted again for another instance, whose states never replace these), refused
/// `design_state_over_cap` when the entry would pass [`MAX_PERSISTED_STATES`] (F2: never an eviction).
pub(super) fn push_state(records: &mut StudioDesignStateRecords, state: StudioInstanceState) -> Result<(), String> {
    records.instances.retain(|s| !(s.layer_id == state.layer_id && s.content_signature == state.content_signature && s.instance_id == state.instance_id));
    if records.instances.len() >= MAX_PERSISTED_STATES {
        return Err(format!("design: design_state_over_cap (the document keeps at most {MAX_PERSISTED_STATES} instance states)"));
    }
    records.instances.push(state);
    Ok(())
}

impl Viewer {
    /// Reads the `design` entry and validates it (module docs) against the inspector's records (the
    /// receipt's and the authored ones); native states are first written from their records.
    pub(super) fn read_design_state(&self, receipt: Option<&super::instance::Receipt>) -> EntryRead {
        let design = receipt.map(|r| &r.records);
        let mut out = EntryRead { design_records_absent: design.is_none(), ..EntryRead::default() };
        let Some(snap) = self.run.snapshot() else { return out };
        let Some(stored) = self.run.document_entry(ENTRY) else { return out };
        out.present = true;
        let (fig, native) = receipt.map_or((None, Vec::new()), |r| split_native(stored, &r.design_file));
        let bytes = fig.as_deref().unwrap_or(stored);
        let doc = snap.document();
        let budget = Budget::default();
        let first = match folio::parse_design_state_records(bytes, doc, design, budget, self.cancel) {
            Ok(mut r) => {
                r.instances.extend(native);
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
                    records.instances.extend(native);
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
        let read = self.read_design_state(receipt.as_ref());
        match read.error {
            Some(e) => Err(e),
            None => Ok(read.records),
        }
    }

    /// Stores `state` in the `design` entry beside the admitted states and the native bindings
    /// (module docs), checked by Folio first with the design file's states (`checked`: `state` too).
    /// Refused `design_state_over_cap` at [`MAX_PERSISTED_STATES`] before anything is written.
    /// Returns notes (design file states dropped as refused).
    pub(super) fn write_design_state(&mut self, mut base: EntryRead, receipt: &super::instance::Receipt, state: StudioInstanceState, checked: bool) -> Result<Vec<String>, String> {
        let doc = self.run.snapshot().ok_or("nothing open")?.document().clone();
        let mut notes = Vec::new();
        if !base.dropped.is_empty() {
            notes.push("refused_design_states_dropped".to_owned());
        }
        let mut records = base.records.take().unwrap_or_else(|| StudioDesignStateRecords::new(doc.document_id.clone()));
        let key = (state.layer_id.clone(), state.content_signature.clone(), state.instance_id.clone());
        push_state(&mut records, state)?;
        let mut check = records.clone();
        check.instances.retain(|s| receipt.design_file.contains(&s.instance_id) || (checked && (&s.layer_id, &s.content_signature, &s.instance_id) == (&key.0, &key.1, &key.2)));
        folio::validate_design_state_records(&check, &doc, Some(&receipt.records), Budget::default(), self.cancel)
            .map_err(|d| format!("design: design_state {} {}", snake(d.code), d.target))?;
        let bytes = serde_json::to_vec(&records).map_err(|e| format!("design: design_state {e}"))?;
        self.run.set_document_entry(ENTRY, Some(bytes)).map_err(|code| format!("design: design entry {code}"))?;
        Ok(notes)
    }

    /// Run when a document is opened (view `open`): with no undo or redo step nothing can return to
    /// a layer that is gone or to a layer's earlier children, so the `design` states of absent layers
    /// (lead ruling 2026-11-11, F1 (b)), of superseded contents and of authored instances whose record
    /// the document no longer holds (ruling R2, 2026-11-12: none of them can bind again) are dropped,
    /// and the `text` records of redraw copies the document no longer draws are compacted (ruling R1).
    /// Each entry is written only when something was dropped.
    pub(in crate::view) fn design_opened(&mut self) {
        let history = self.run.history();
        let empty = |k: &str| history[k].as_array().is_some_and(Vec::is_empty);
        if !(empty("undo") && empty("redo")) {
            return;
        }
        let Some(doc) = self.run.snapshot().map(|s| s.document().clone()) else { return };
        let (design_file, native) = self.design_receipt().map_or_else(Default::default, |r| (r.design_file, r.native.into_keys().collect::<BTreeSet<_>>()));
        if let Some(mut records) = self.run.document_entry(ENTRY).and_then(raw_states) {
            let before = records.instances.len();
            records.instances.retain(|s| {
                (design_file.contains(&s.instance_id) || native.contains(&s.instance_id))
                    && doc.layers.iter().any(|l| l.layer_id == s.layer_id)
                    && s.content_signature == content(&doc, &s.layer_id)
            });
            if records.instances.len() != before {
                let written = serde_json::to_vec(&records).map_err(|e| e.to_string()).and_then(|b| self.run.set_document_entry(ENTRY, Some(b)).map_err(str::to_owned));
                if let Err(e) = written {
                    self.say(format!("design states of deleted layers not pruned: {e}"));
                }
            }
        }
        if self.run.document_entry(super::super::text::ENTRY).is_some()
            && let Ok(mut tentry) = self.text_entry()
            && super::instance::compact_copy_text(&mut tentry, &doc)
        {
            let written = super::super::text::TextEntry::to_bytes(&tentry).and_then(|b| self.run.set_document_entry(super::super::text::ENTRY, Some(b)).map_err(str::to_owned));
            if let Err(e) = written {
                self.say(format!("text records of instance copies not compacted: {e}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_PERSISTED_STATES, push_state, room_for};
    use hsk_studio_folio::{StudioDesignStateRecords, StudioInstanceState};
    use std::collections::BTreeMap;

    fn state(i: usize) -> StudioInstanceState {
        StudioInstanceState {
            layer_id: format!("SLYR-019abcde-0000-7000-8000-{i:012x}"),
            instance_id: format!("SCIN-019abcde-0000-7000-8000-{i:012x}"),
            component_id: "SCMP-019abcde-0000-7000-8000-000000000001".into(),
            variant: BTreeMap::new(),
            properties: BTreeMap::new(),
            overrides: Vec::new(),
            swap_target: None,
            content_signature: Vec::new(),
        }
    }

    #[test]
    fn a_state_past_the_bound_is_refused_and_none_is_evicted() {
        let mut r = StudioDesignStateRecords::new("SDOC-019abcde-0000-7000-8000-000000000001".to_owned());
        for i in 0..MAX_PERSISTED_STATES {
            push_state(&mut r, state(i)).unwrap();
        }
        let first = r.instances[0].clone();
        let e = push_state(&mut r, state(MAX_PERSISTED_STATES)).unwrap_err();
        assert!(e.starts_with("design: design_state_over_cap"), "{e}");
        assert_eq!(r.instances.len(), MAX_PERSISTED_STATES);
        assert_eq!(r.instances[0], first, "nothing is evicted");
        // The same layer and content replaces its state (no growth).
        push_state(&mut r, state(0)).unwrap();
        assert_eq!(r.instances.len(), MAX_PERSISTED_STATES);
        // A step that would add one more state is refused before it runs.
        let bytes = serde_json::to_vec(&r).unwrap();
        assert!(room_for(Some(&bytes), 1).unwrap_err().starts_with("design: design_state_over_cap"));
        assert!(room_for(Some(&bytes), 0).is_ok());
    }
}
