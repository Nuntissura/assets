//! Records never outlive their layer (lane SMB-FIX-LAYER-RECORD-PRUNE-1; common lead ruling 5 and the lead direction of
//! 2026-11-13: a deleted object's records are removed in the same undo step as the object, and undo restores both).
//!
//! Every structure step runs through [`Viewer::structure_step`] (or [`Viewer::before_structure`] and
//! [`Viewer::prune_removed`] around a step that keeps its own refusal form). After the step commits, the records keyed
//! to the layers it removed (present before the step, absent after it; a layer the step makes again under the same id
//! keeps its records) leave the `raster_records`, `vector` and `text` entries INSIDE that same history step
//! (`Run::entries_into_newest_step`: one group step under the step's own label, command id and context). Undo restores
//! the layers and their records together, redo removes both, and a refused entry write rolls the step back. A layer
//! made afterwards that takes a removed id (ids are reused until SMB-LAYER-ID-STABILITY-1) therefore inherits nothing.
//!
//! Records removed:
//! - `raster_records`: the rows and the placed instances of removed layers ([`Viewer::raster_records_without`]);
//! - `vector`: board node records; area- and path-text bindings; clips and masks whose source was removed (with their
//!   mask parameters; their ids leave the attachments, and an attachment left with neither goes); the attachments,
//!   boolean groups, envelopes (their layer or top-object source), gradient meshes, live filter stacks and live shapes
//!   of removed layers; the design node entries of removed layers, the component and set records whose group is gone
//!   and the instance records bound to a removed layer (settled as every authoring commit is); the own appearance
//!   styles (`SSTY-<layer uuid>`, publish key `appearance`) of removed layers and of their dropped nodes that nothing
//!   references any more, with the swatch, brush, marker, pattern and image bindings keyed to them (published styles,
//!   graphic styles and swatch companions, are the palette's definitions and stay); the path records of removed layers
//!   (their payloads, a dropped boolean group's fill geometry, a dropped mesh's source path) that nothing references
//!   any more;
//! - `text`: the text layer records of removed layers; the stories only removed layers or dropped bindings named, with
//!   the type styles only those stories used ([`drop_stories`], which the board's node delete uses too).
//!
//! References (lead ruling Q1, 2026-11-13): a style, path or story id is kept while it occurs anywhere else: the
//! document after the step, every other session document entry (the design binding states and the layout records
//! among them), the import receipt and the rest of its own entry. Path-id consumers (grep at c8fca900): layer payloads
//! (`Payload::Vector`), live shapes, width profiles (on the path record), boolean groups' fill geometry, path texts,
//! area texts (their container), gradient meshes (their source path); the viewer persists no clipboard, symbol or
//! library store.
//!
//! Not removed here: the design binding states (entry `design`; lead ruling F1 form 2026-11-11: pruned at open only),
//! the layout records (`Run::delete_layout_layers` cascades them), the develop entry, connector endpoints naming a
//! deleted board node (the board reports them `broken_connectors`), placed sources (shared by their instances). The
//! open-time drops (`place_opened`, `board_opened`, `design_opened`) stay as the guard for packages written before.
//!
//! Bounds: nothing is read unless the step removed a layer; an entry is decoded only when its bytes name a removed
//! layer, path, story or style id; every scan is one linear pass over bytes the session already holds. An entry that
//! does not decode is left as it is and never blocks the step; a pruned entry that would not decode against the
//! document after the step is not written (status line `layer_records_not_pruned`).
#![deny(clippy::arithmetic_side_effects)]

use super::Viewer;
use super::text::{self, TextEntry};
use super::vector::{self, VectorEntry};
use super::{design, place};
use crate::harness::Run;
use hsk_studio_folio::{self as folio, NodeKind, Payload, Snapshot, StudioDocument, StudioEnvelopeShape, StudioLayer};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

/// Publish key of a layer's own appearance style (`vector::style`).
const APPEARANCE_KEY: &str = "appearance";

/// A pruned vector entry and the story ids of the text bindings it dropped (`None`: unchanged or not read).
type PrunedVector = Option<(Vec<u8>, BTreeSet<String>)>;

/// The document before a structure step (an `Arc` clone of the snapshot; `None` with nothing open).
pub(super) struct Before(Option<Snapshot>);

/// One byte of an id token (`SLYR-...`, `SVPT-...`, `STXT-...`, `SSTY-...`).
const fn id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// Every id-shaped token of `parts` (four ASCII capitals and `-`, then id bytes), one pass over each part.
fn id_tokens(parts: &[&[u8]]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for bytes in parts {
        for run in bytes.split(|b| !id_byte(*b)) {
            let shaped = run.len() > 5 && run.get(..4).is_some_and(|p| p.iter().all(u8::is_ascii_uppercase)) && run.get(4) == Some(&b'-');
            if shaped && let Ok(t) = std::str::from_utf8(run) {
                out.insert(t.to_owned());
            }
        }
    }
    out
}

/// The stories `gone` leave `entry`, with the type styles they used that no remaining story or text layer record uses.
pub(super) fn drop_stories(entry: &mut TextEntry, gone: &BTreeSet<String>) {
    if gone.is_empty() {
        return;
    }
    let refs = |s: &folio::StudioTextStory| -> Vec<String> { s.paragraphs.iter().filter_map(|p| p.style_ref.clone()).chain(s.character_ranges.iter().filter_map(|r| r.style_ref.clone())).collect() };
    let dropped: Vec<String> = entry.stories.iter().filter(|s| gone.contains(&s.story_id)).flat_map(&refs).collect();
    entry.stories.retain(|s| !gone.contains(&s.story_id));
    let used: BTreeSet<String> = entry.stories.iter().flat_map(&refs).chain(entry.layers.iter().flat_map(|l| [l.paragraph_style.clone(), l.character_style.clone()])).collect();
    let own: BTreeSet<String> = dropped.into_iter().filter(|r| !used.contains(r)).collect();
    entry.styles.styles.retain(|st| !own.contains(&st.style_id));
}

/// The path ids a layer's payload names.
fn payload_paths(layer: &StudioLayer) -> &[String] {
    match &layer.payload {
        Payload::Vector { path_ids } => path_ids,
        _ => &[],
    }
}

impl Viewer {
    /// The document before a structure step, for [`Self::prune_removed`].
    pub(super) fn before_structure(&self) -> Before {
        Before(self.run.snapshot().cloned())
    }

    /// Runs the structure step `step` (true: committed) as [`Self::after_edit`] reports it, then removes the records of
    /// the layers it removed inside that same history step ([`Self::prune_removed`]). Returns the step's revision.
    pub(super) fn structure_step(&mut self, op: &str, what: String, step: impl FnOnce(&mut Run<'static>) -> bool) -> Result<u64, String> {
        let before = self.before_structure();
        let ok = step(&mut self.run);
        let revision = self.after_edit(ok, what)?;
        self.prune_removed(op, before)?;
        Ok(revision)
    }

    /// After a committed structure step: the records keyed to the layers of `before` the document no longer holds leave
    /// their entries inside the newest history step (module docs). `op` names the refusal record; a refused write rolls
    /// the step back and is returned.
    pub(super) fn prune_removed(&mut self, op: &str, before: Before) -> Result<(), String> {
        let (Some(before), Some(after)) = (before.0, self.run.snapshot().cloned()) else { return Ok(()) };
        let present: BTreeSet<&str> = after.document().layers.iter().map(|l| l.layer_id.as_str()).collect();
        let removed: Vec<&StudioLayer> = before.document().layers.iter().filter(|l| !present.contains(l.layer_id.as_str())).collect();
        if removed.is_empty() {
            return Ok(());
        }
        let entries = self.pruned_entries(after.document(), &removed);
        if entries.is_empty() {
            return Ok(());
        }
        if self.run.entries_into_newest_step(op, entries) {
            self.place_sync();
            Ok(())
        } else {
            let e = format!("{op}: the records of the removed layers were refused, the step is rolled back: {}", self.op_error());
            self.say(e.clone());
            Err(e)
        }
    }

    /// The entries without the records of `removed`, each only when it changed (raster, vector, text order).
    fn pruned_entries(&mut self, after: &StudioDocument, removed: &[&StudioLayer]) -> Vec<(&'static str, Vec<u8>)> {
        let ids: BTreeSet<String> = removed.iter().map(|l| l.layer_id.clone()).collect();
        let mut out = Vec::new();
        if let Some(bytes) = self.raster_records_without(&ids) {
            out.push((place::ENTRY, bytes));
        }
        let (vector_bytes, stories) = match self.pruned_vector(after, removed, &ids) {
            Ok(Some((bytes, stories))) => (Some(bytes), stories),
            Ok(None) => (None, BTreeSet::new()),
            Err(e) => {
                self.say(format!("layer_records_not_pruned: vector ({e})"));
                (None, BTreeSet::new())
            }
        };
        match self.pruned_text(after, removed, &ids, stories, vector_bytes.as_deref()) {
            Ok(text_bytes) => {
                if let Some(bytes) = vector_bytes {
                    out.push((vector::ENTRY, bytes));
                }
                if let Some(bytes) = text_bytes {
                    out.push((text::ENTRY, bytes));
                }
            }
            Err(e) => {
                self.say(format!("layer_records_not_pruned: text ({e})"));
                if let Some(bytes) = vector_bytes {
                    out.push((vector::ENTRY, bytes));
                }
            }
        }
        out
    }

    /// The bytes of every session document entry but `skip`, `substitute` standing in for the entry of its token, the
    /// document after the step and the import receipt: where a reference to a pruned id may occur.
    fn elsewhere<'a>(&'a self, after: &StudioDocument, skip: &str, substitute: Option<(&str, &'a [u8])>) -> Result<Vec<Cow<'a, [u8]>>, String> {
        let mut parts: Vec<Cow<'a, [u8]>> = Vec::new();
        for token in hsk_studio_session::DOCUMENT_ENTRY_TOKENS {
            if *token == skip {
                continue;
            }
            match substitute {
                Some((t, bytes)) if t == *token => parts.push(Cow::Borrowed(bytes)),
                _ => parts.extend(self.run.document_entry(token).map(Cow::Borrowed)),
            }
        }
        parts.push(Cow::Owned(serde_json::to_vec(after).map_err(|e| format!("document: {e}"))?));
        if let Some(r) = &self.receipt {
            parts.push(Cow::Owned(serde_json::to_vec(r).map_err(|e| format!("receipt: {e}"))?));
        }
        Ok(parts)
    }

    /// The ids of `candidates` that occur in [`Self::elsewhere`] or in `own` (the entry without the candidates' records).
    fn referenced(&self, candidates: &BTreeSet<String>, after: &StudioDocument, skip: &str, substitute: Option<(&str, &[u8])>, own: &[u8]) -> Result<BTreeSet<String>, String> {
        let mut parts = self.elsewhere(after, skip, substitute)?;
        parts.push(Cow::Borrowed(own));
        let views: Vec<&[u8]> = parts.iter().map(AsRef::as_ref).collect();
        let found = id_tokens(&views);
        Ok(candidates.iter().filter(|c| found.contains(*c)).cloned().collect())
    }

    /// The vector entry without the records of the removed layers (module docs) and the story ids of the bindings it
    /// dropped; `Ok(None)` when nothing changes or the entry is not read here (none, not persistable, names no removed
    /// id, does not decode); `Err` when the pruned entry would not decode against `after` (it is not written).
    fn pruned_vector(&self, after: &StudioDocument, removed: &[&StudioLayer], ids: &BTreeSet<String>) -> Result<PrunedVector, String> {
        if !vector::entry_persistable() {
            return Ok(None);
        }
        let Some(bytes) = self.run.document_entry(vector::ENTRY) else { return Ok(None) };
        let own_styles: BTreeSet<String> = removed.iter().filter_map(|l| vector::style::layer_style_id(&l.layer_id).ok()).collect();
        let named = id_tokens(&[bytes]);
        let mentions = |set: &BTreeSet<String>| set.iter().any(|id| named.contains(id));
        let paths: BTreeSet<String> = removed.iter().flat_map(|l| payload_paths(l).iter().cloned()).collect();
        if !(mentions(ids) || mentions(&paths) || mentions(&own_styles)) {
            return Ok(None);
        }
        // The records still name the removed layers: decode against the document after the step plus those layers.
        let mut union = after.clone();
        union.layers.extend(removed.iter().map(|l| (*l).clone()));
        let Ok(original) = VectorEntry::decode(bytes, &union, self.cancel).or_else(|_| VectorEntry::decode(bytes, after, self.cancel)) else { return Ok(None) };
        let mut entry = original.clone();
        let gone = |id: &str| ids.contains(id);
        let mut stories = BTreeSet::new();
        let mut path_candidates = paths;
        let r = &mut entry.records;
        r.board_nodes.retain(|n| !gone(&n.layer_id));
        r.area_texts.retain(|a| {
            let keep = !gone(&a.container_layer_id);
            if !keep {
                stories.insert(a.text_id.clone());
            }
            keep
        });
        r.path_texts.retain(|p| {
            let keep = !gone(&p.path_layer_id);
            if !keep {
                stories.insert(p.story_id.clone());
            }
            keep
        });
        let clips: BTreeSet<String> = r.clips.iter().filter(|c| gone(&c.source_layer_id)).map(|c| c.clip_id.clone()).collect();
        let masks: BTreeSet<String> = r.masks.iter().filter(|m| gone(&m.source_layer_id)).map(|m| m.mask_id.clone()).collect();
        r.clips.retain(|c| !clips.contains(&c.clip_id));
        r.masks.retain(|m| !masks.contains(&m.mask_id));
        r.mask_parameters.retain(|p| !masks.contains(&p.mask_id));
        r.attachments.retain_mut(|a| {
            if a.clip_id.as_ref().is_some_and(|c| clips.contains(c)) {
                a.clip_id = None;
            }
            if a.mask_id.as_ref().is_some_and(|m| masks.contains(m)) {
                a.mask_id = None;
            }
            !gone(&a.layer_id) && (a.clip_id.is_some() || a.mask_id.is_some())
        });
        r.boolean_groups.retain(|g| {
            let keep = !gone(&g.layer_id);
            if !keep {
                path_candidates.extend(g.fill_geometry_path_ids.iter().cloned());
            }
            keep
        });
        r.envelopes.retain(|e| !gone(&e.layer_id) && !matches!(&e.envelope, StudioEnvelopeShape::TopObject { source_layer_id } if gone(source_layer_id)));
        r.gradient_meshes.retain(|m| {
            let keep = !gone(&m.layer_id);
            if !keep {
                path_candidates.extend(m.source_path_id.iter().cloned());
            }
            keep
        });
        r.live_filters.retain(|s| !gone(&s.layer_id));
        entry.live.retain(|s| !gone(&s.layer_id));
        self.prune_design(after, &union, &original, &mut entry, ids, own_styles)?;
        // Path records of the removed layers that nothing references any more.
        path_candidates.retain(|p| entry.paths.iter().any(|r| &r.path_id == p));
        if !path_candidates.is_empty() {
            let mut tentative = entry.clone();
            tentative.paths.retain(|r| !path_candidates.contains(&r.path_id));
            let referenced = self.referenced(&path_candidates, after, vector::ENTRY, None, &tentative.to_bytes(&after.document_id)?)?;
            entry.paths.retain(|r| !path_candidates.contains(&r.path_id) || referenced.contains(&r.path_id));
        }
        if entry == original {
            return Ok(None);
        }
        let pruned = entry.to_bytes(&after.document_id)?;
        VectorEntry::decode(&pruned, after, self.cancel)?;
        Ok(Some((pruned, stories)))
    }

    /// Lead ruling Q3 (2026-11-13): the design records of the removed layers (module docs), on `entry`. `original` is the
    /// entry before any prune (the instance bindings are read from it over `union`, the document holding the removed
    /// layers too).
    fn prune_design(&self, after: &StudioDocument, union: &StudioDocument, original: &VectorEntry, entry: &mut VectorEntry, ids: &BTreeSet<String>, own_styles: BTreeSet<String>) -> Result<(), String> {
        let Some(mut d) = entry.design.clone() else { return Ok(()) };
        let gone = |id: &str| ids.contains(id);
        let names = d.components.iter().any(|k| gone(&k.layer_id))
            || d.component_sets.iter().any(|s| gone(&s.layer_id))
            || d.nodes.iter().any(|n| n.node.node_kind == NodeKind::Layer && gone(&n.node.node_id))
            || !d.component_instances.is_empty()
            || d.paint_styles.iter().any(|s| own_styles.contains(&s.style_id));
        if !names {
            return Ok(());
        }
        let bound: BTreeMap<String, String> = self.native_bindings_in(union, original.clone());
        let mut candidates = own_styles;
        d.nodes.retain(|n| {
            let keep = !(n.node.node_kind == NodeKind::Layer && gone(&n.node.node_id));
            if !keep {
                candidates.extend(n.style_refs.values().cloned());
            }
            keep
        });
        d.component_instances.retain(|i| bound.get(&i.instance_id).is_none_or(|l| !gone(l)));
        design::instance::settle(&mut d, after);
        design::component::variant_definitions(&mut d, after);
        // Own appearance styles nothing references any more leave with the bindings keyed to them.
        candidates.retain(|c| d.paint_styles.iter().any(|s| &s.style_id == c && s.publishing.publish_key == APPEARANCE_KEY));
        if !candidates.is_empty() {
            let drop_styles = |e: &mut VectorEntry, set: &BTreeSet<String>| {
                if let Some(d) = e.design.as_mut() {
                    d.paint_styles.retain(|s| !set.contains(&s.style_id));
                }
                let r = &mut e.records;
                r.swatch_paints.retain(|p| !set.contains(&p.style_id));
                r.brush_strokes.retain(|p| !set.contains(&p.style_id));
                r.stroke_markers.retain(|m| !set.contains(&m.style_id));
                r.pattern_paints.retain(|p| !set.contains(&p.style_id));
                r.image_paints.retain(|p| !set.contains(&p.style_id));
            };
            let mut tentative = entry.clone();
            tentative.design = Some(d.clone());
            drop_styles(&mut tentative, &candidates);
            let referenced = self.referenced(&candidates, after, vector::ENTRY, None, &tentative.to_bytes(&after.document_id)?)?;
            candidates.retain(|c| !referenced.contains(c));
            entry.design = Some(d);
            drop_styles(entry, &candidates);
        } else {
            entry.design = Some(d);
        }
        Ok(())
    }

    /// The text entry without the records of the removed layers and the stories nothing references any more (module
    /// docs); `stories` are the story ids of the vector bindings the prune dropped, `vector` the pruned vector entry when
    /// there is one. `Ok(None)` when nothing changes or the entry is not read here; `Err` when the pruned entry would not
    /// decode against `after`.
    fn pruned_text(&self, after: &StudioDocument, removed: &[&StudioLayer], ids: &BTreeSet<String>, mut stories: BTreeSet<String>, vector: Option<&[u8]>) -> Result<Option<Vec<u8>>, String> {
        let Some(bytes) = self.run.document_entry(text::ENTRY) else { return Ok(None) };
        stories.extend(removed.iter().filter_map(|l| match &l.payload {
            Payload::Text { story_id } => Some(story_id.clone()),
            _ => None,
        }));
        let named = id_tokens(&[bytes]);
        if !(ids.iter().any(|id| named.contains(id)) || stories.iter().any(|s| named.contains(s))) {
            return Ok(None);
        }
        let Ok(original) = TextEntry::decode(bytes, after, self.cancel) else { return Ok(None) };
        let mut entry = original.clone();
        entry.layers.retain(|l| {
            let keep = !ids.contains(&l.layer_id);
            if !keep {
                stories.insert(l.story_id.clone());
            }
            keep
        });
        stories.retain(|s| entry.stories.iter().any(|x| &x.story_id == s));
        if !stories.is_empty() {
            let mut tentative = entry.clone();
            tentative.stories.retain(|s| !stories.contains(&s.story_id));
            let referenced = self.referenced(&stories, after, text::ENTRY, vector.map(|b| (vector::ENTRY, b)), &tentative.to_bytes()?)?;
            stories.retain(|s| !referenced.contains(s));
            drop_stories(&mut entry, &stories);
        }
        if entry == original {
            return Ok(None);
        }
        let pruned = entry.to_bytes()?;
        TextEntry::decode(&pruned, after, self.cancel)?;
        Ok(Some(pruned))
    }
}
