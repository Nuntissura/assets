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
//! The prune joins the newest step only when this structure step pushed it (one more undo step than before; review
//! F6); otherwise nothing is written and the warning `prune_skipped:no_step` is returned.
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
//!   graphic styles and swatch companions, are the palette's definitions and stay; lead ruling Q3); the path records
//!   of removed layers (their payloads, a dropped boolean group's fill geometry, a dropped mesh's source path) that
//!   nothing references any more;
//! - `text`: the text layer records of removed layers; the stories that only removed layers or dropped bindings
//!   named (the bindings read before AND after the step, review F7: a step that removes a binding in its own write
//!   still takes the story), with their `paragraph_composition` and `character_overrides` rows; the own (unnamed)
//!   type styles of removed layers (`STYS-<layer uuid>` and its variant-nibble siblings, `text.rs` and
//!   `vector.rs` live text) that nothing references any more, with their `style_composition` and `style_character`
//!   rows. Named, imported (a design file's) and published type styles always stay (lead ruling F1).
//!
//! References (lead ruling Q1, 2026-11-13; review F4): a path, style or story id is kept while it occurs anywhere
//! else: the document after the step, every other session document entry (the design binding states and the layout
//! records among them), the import receipt and the rest of its own entry (a kept style's `based_on`, `next_style` and
//! GREP character style among them). An occurrence anywhere counts, also glued to other id bytes or as the prefix of a
//! longer id, which can only keep more. The document, the receipt and the entries the prune does not write are
//! scanned once per prune for every candidate together; only the two entries it writes are scanned again per stage.
//! Path-id consumers (grep at c8fca900): layer payloads (`Payload::Vector`), live shapes, width profiles (on the path
//! record), boolean groups' fill geometry, path texts, area texts (their container), gradient meshes (their source
//! path), layout graphic frames (layout records); the viewer persists no clipboard, symbol or library store. After the
//! prune every path id the document's payloads, the live shapes and the vector records name must still resolve, else
//! the vector part is not written (`prune_skipped:vector`). SVPT ids may be minted again after a prune (lead ruling
//! Q5: no live reference remains, and undo restores whole entries; SMB-LAYER-ID-STABILITY-1 owns id stability).
//!
//! Not removed here: the design binding states (entry `design`; lead ruling F1 form 2026-11-11: pruned at open only),
//! the layout records (`Run::delete_layout_layers` and the page deletes cascade them), the develop entry, connector
//! endpoints naming a deleted board node (the board reports them `broken_connectors`), placed sources (shared by their
//! instances). The open-time drops (`place_opened`, `board_opened`, `design_opened`) stay as the guard for packages
//! written before.
//!
//! When a part cannot be pruned (its entry does not decode, the pruned entry would not decode, a path would no longer
//! resolve, the reference scan is canceled or passes [`MAX_SCAN_BYTES`]), the step commits without that part, as
//! before this lane, and the warning `prune_skipped:<entry>` is returned with the step ([`Viewer::prune_warnings`]; a
//! driven row's result carries it as `warnings`; lead ruling Q3). Editing is never blocked by it; the open-time drops
//! remove such orphans later. Bounds: nothing is read unless the step removed a layer; every scan is one linear pass
//! over bytes the session already holds, with a cancel check between parts and the [`MAX_SCAN_BYTES`] budget.
#![deny(clippy::arithmetic_side_effects)]

use super::Viewer;
use super::text::{self, TextEntry};
use super::vector::{self, VectorEntry};
use super::{design, place};
use crate::harness::Run;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{self as folio, NodeKind, Payload, Snapshot, StudioDocument, StudioEnvelopeShape, StudioLayer};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

/// Publish key of a layer's own appearance style (`vector::style`).
const APPEARANCE_KEY: &str = "appearance";

/// Most bytes one prune's reference scans read in all (the document, the receipt and the entries; review F4).
pub const MAX_SCAN_BYTES: usize = 256 << 20;

/// Code of a reference scan past [`MAX_SCAN_BYTES`].
pub const SCAN_OVER_BUDGET: &str = "layer_records_scan_over_budget";

/// The state before a structure step: the snapshot (an `Arc` clone; `None` with nothing open), the undo depth (review
/// F6) and the vector entry bytes (review F7: the text bindings a step's own write removes).
pub(super) struct Before {
    snapshot: Option<Snapshot>,
    depth: usize,
    vector: Option<Vec<u8>>,
}

/// The ids of `candidates` that occur anywhere in `parts` (a substring match: an occurrence glued to other id bytes
/// counts). One pass over each part; `cancel` is checked between parts and `budget` bytes in all are read at most.
pub(super) fn occurring(parts: &[&[u8]], candidates: &BTreeSet<String>, cancel: &CancellationToken, budget: usize) -> Result<BTreeSet<String>, &'static str> {
    let mut found = BTreeSet::new();
    if candidates.is_empty() {
        return Ok(found);
    }
    let wanted: BTreeSet<&[u8]> = candidates.iter().map(|c| c.as_bytes()).filter(|c| !c.is_empty()).collect();
    // Candidates by their first (up to) five bytes, with the lengths that prefix takes.
    let mut prefixes: BTreeMap<&[u8], BTreeSet<usize>> = BTreeMap::new();
    for c in &wanted {
        let cut = c.len().min(5);
        prefixes.entry(c.get(..cut).unwrap_or(c)).or_default().insert(c.len());
    }
    let firsts: BTreeSet<u8> = prefixes.keys().filter_map(|p| p.first().copied()).collect();
    let mut read: usize = 0;
    for bytes in parts {
        cancel.check().map_err(|_| "canceled")?;
        read = read.checked_add(bytes.len()).filter(|n| *n <= budget).ok_or(SCAN_OVER_BUDGET)?;
        for (at, first) in bytes.iter().enumerate() {
            if !firsts.contains(first) {
                continue;
            }
            for (prefix, lengths) in &prefixes {
                if bytes.get(at..at.saturating_add(prefix.len())) != Some(*prefix) {
                    continue;
                }
                for len in lengths {
                    if let Some(slice) = bytes.get(at..at.saturating_add(*len))
                        && wanted.contains(slice)
                        && let Ok(id) = std::str::from_utf8(slice)
                    {
                        found.insert(id.to_owned());
                    }
                }
            }
        }
        if found.len() == wanted.len() {
            break;
        }
    }
    Ok(found)
}

/// True when any of `ids` occurs in `bytes` (no budget: one entry the session holds).
fn mentions(bytes: &[u8], ids: &BTreeSet<String>) -> bool {
    occurring(&[bytes], ids, &CancellationToken::default(), usize::MAX).is_ok_and(|f| !f.is_empty())
}

/// The own (unnamed) type style ids a layer's text takes (`text.rs` point text: `STYS-<uuid>` and the `-a000-`
/// variant; `vector.rs` live text: `STYS-<uuid>` and the flipped variant nibble).
fn own_type_styles(layer_id: &str) -> Vec<String> {
    let Some(uuid) = layer_id.strip_prefix("SLYR-") else { return Vec::new() };
    let mut out = vec![format!("STYS-{uuid}"), format!("STYS-{}", uuid.replacen("-9000-", "-a000-", 1))];
    if let (Some(head), Some(nibble), Some(tail)) = (uuid.get(..19), uuid.get(19..20), uuid.get(20..)) {
        out.push(format!("STYS-{head}{}{tail}", if nibble == "b" { "8" } else { "b" }));
    }
    out
}

/// Removes the stories `stories` with their `paragraph_composition` and `character_overrides` rows, and the type
/// styles `styles` with their `style_composition` and `style_character` rows.
fn remove_text(entry: &mut TextEntry, stories: &BTreeSet<String>, styles: &BTreeSet<String>) {
    entry.stories.retain(|s| !stories.contains(&s.story_id));
    let r = &mut entry.styles;
    r.paragraph_composition.retain(|c| !stories.contains(&c.text_id));
    r.character_overrides.retain(|o| !stories.contains(&o.text_id));
    r.styles.retain(|s| !styles.contains(&s.style_id));
    r.style_composition.retain(|c| !styles.contains(&c.style_id));
    r.style_character.retain(|c| !styles.contains(&c.style_id));
}

/// The board's own story drop (`board.node.delete`, review F2/F7 of SMB-BOARD-1): the story `story_id` leaves `entry`
/// with its rows and with the type styles it used that no remaining story, text layer record or kept style
/// (`based_on`, `next_style`, GREP character style) uses (board-minted styles of that story).
pub(super) fn drop_board_story(entry: &mut TextEntry, story_id: &str) {
    let refs = |s: &folio::StudioTextStory| -> Vec<String> { s.paragraphs.iter().filter_map(|p| p.style_ref.clone()).chain(s.character_ranges.iter().filter_map(|r| r.style_ref.clone())).collect() };
    let Some(story) = entry.stories.iter().find(|s| s.story_id == story_id) else { return };
    let candidates: BTreeSet<String> = refs(story).into_iter().collect();
    let stories = BTreeSet::from([story_id.to_owned()]);
    let mut tentative = entry.clone();
    remove_text(&mut tentative, &stories, &BTreeSet::new());
    let used: BTreeSet<String> = tentative
        .stories
        .iter()
        .flat_map(&refs)
        .chain(tentative.layers.iter().flat_map(|l| [l.paragraph_style.clone(), l.character_style.clone()]))
        .chain(tentative.styles.styles.iter().flat_map(|s| s.based_on.iter().chain(s.next_style.iter()).cloned().chain(s.grep_styles.iter().map(|g| g.character_style.clone()))))
        .collect();
    let styles: BTreeSet<String> = candidates.into_iter().filter(|c| !used.contains(c)).collect();
    remove_text(entry, &stories, &styles);
}

/// The path ids a layer's payload names.
fn payload_paths(layer: &StudioLayer) -> &[String] {
    match &layer.payload {
        Payload::Vector { path_ids } => path_ids,
        _ => &[],
    }
}

/// The path ids `doc` and `entry` name outside the path records themselves (payloads, live shapes, records).
fn named_paths(doc: &StudioDocument, entry: &VectorEntry) -> BTreeSet<String> {
    let r = &entry.records;
    doc.layers
        .iter()
        .flat_map(|l| payload_paths(l).iter().cloned())
        .chain(entry.live.iter().map(|s| s.path_id.clone()))
        .chain(r.boolean_groups.iter().flat_map(|g| g.fill_geometry_path_ids.iter().cloned()))
        .chain(r.gradient_meshes.iter().filter_map(|m| m.source_path_id.clone()))
        .chain(r.path_texts.iter().map(|p| p.path_id.clone()))
        .chain(r.area_texts.iter().map(|a| a.container_id.clone()))
        .collect()
}

/// What one prune decided for the vector and the text entry: the pruned bytes (`None`: unchanged) or the reason it
/// was skipped.
type Part = Result<Option<Vec<u8>>, String>;

impl Viewer {
    /// The warnings of the structure steps since the last driven row (`run_command` clears them): `prune_skipped:<entry>`
    /// for an entry whose records of removed layers could not be pruned in the step (module docs), `prune_skipped:no_step`
    /// when the step pushed no history step to join.
    pub fn prune_warnings(&self) -> &[String] {
        &self.prune_warnings
    }

    /// The state before a structure step, for [`Self::prune_removed`].
    pub(super) fn before_structure(&self) -> Before {
        let vector = if vector::entry_persistable() { self.run.document_entry(vector::ENTRY).map(<[u8]>::to_vec) } else { None };
        Before { snapshot: self.run.snapshot().cloned(), depth: self.undo_depth(), vector }
    }

    fn undo_depth(&self) -> usize {
        self.run.history()["undo"].as_array().map_or(0, Vec::len)
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

    fn prune_warning(&mut self, part: &str, why: &str) {
        let w = format!("prune_skipped:{part}");
        self.say(format!("layer_records_not_pruned: {part} ({why})"));
        if !self.prune_warnings.contains(&w) {
            self.prune_warnings.push(w);
        }
    }

    /// After a committed structure step: the records keyed to the layers of `before` the document no longer holds leave
    /// their entries inside the newest history step, when this step pushed it (module docs). `op` names the refusal
    /// record; a refused write rolls the step back and is returned. A part that cannot be pruned is a warning.
    pub(super) fn prune_removed(&mut self, op: &str, before: Before) -> Result<(), String> {
        let (Some(snapshot), Some(after)) = (before.snapshot.as_ref(), self.run.snapshot().cloned()) else { return Ok(()) };
        let present: BTreeSet<&str> = after.document().layers.iter().map(|l| l.layer_id.as_str()).collect();
        let removed: Vec<&StudioLayer> = snapshot.document().layers.iter().filter(|l| !present.contains(l.layer_id.as_str())).collect();
        if removed.is_empty() {
            return Ok(());
        }
        if self.undo_depth() != before.depth.saturating_add(1) {
            self.prune_warning("no_step", "the step pushed no history step of its own");
            return Ok(());
        }
        #[allow(unused_mut)]
        let mut entries = self.pruned_entries(after.document(), &removed, before.vector.as_deref());
        #[cfg(test)]
        if tests::REFUSE_WRITE.with(std::cell::Cell::get) {
            entries.push(("no_such_entry_token", Vec::new()));
        }
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

    /// The entries without the records of `removed`, each only when it changed (raster, vector, text order); a part
    /// that cannot be pruned becomes a warning.
    fn pruned_entries(&mut self, after: &StudioDocument, removed: &[&StudioLayer], vector_before: Option<&[u8]>) -> Vec<(&'static str, Vec<u8>)> {
        let ids: BTreeSet<String> = removed.iter().map(|l| l.layer_id.clone()).collect();
        let mut out = Vec::new();
        match self.raster_records_without(&ids) {
            Ok(Some(bytes)) => out.push((place::ENTRY, bytes)),
            Ok(None) => {}
            Err(why) => self.prune_warning(place::ENTRY, &why),
        }
        let (vector, text) = self.pruned_vector_and_text(after, removed, &ids, vector_before);
        match vector {
            Ok(Some(bytes)) => out.push((vector::ENTRY, bytes)),
            Ok(None) => {}
            Err(why) => self.prune_warning(vector::ENTRY, &why),
        }
        match text {
            Ok(Some(bytes)) => out.push((text::ENTRY, bytes)),
            Ok(None) => {}
            Err(why) => self.prune_warning(text::ENTRY, &why),
        }
        out
    }

    /// The bytes every reference scan of one prune reads besides the two entries it may write: every other session
    /// document entry, the document after the step (or the projection `doc` a caller passes) and the import receipt.
    fn fixed_parts<'a>(&'a self, doc: &StudioDocument, skip: &[&str]) -> Result<Vec<Cow<'a, [u8]>>, String> {
        let mut parts: Vec<Cow<'a, [u8]>> = hsk_studio_session::DOCUMENT_ENTRY_TOKENS.iter().filter(|t| !skip.contains(t)).filter_map(|t| self.run.document_entry(t)).map(Cow::Borrowed).collect();
        parts.push(Cow::Owned(serde_json::to_vec(doc).map_err(|e| format!("document: {e}"))?));
        if let Some(r) = &self.receipt {
            parts.push(Cow::Owned(serde_json::to_vec(r).map_err(|e| format!("receipt: {e}"))?));
        }
        Ok(parts)
    }

    /// Lead ruling Q6: the path ids of `candidates` that a projected document `doc`, any session document entry but the
    /// vector entry, the import receipt or `own` (the vector entry without the candidates) names. A scan that cannot run
    /// keeps every candidate (`Err`).
    pub(in crate::view) fn referenced_paths(&self, doc: &StudioDocument, candidates: &BTreeSet<String>, own: &[u8]) -> Result<BTreeSet<String>, String> {
        let mut parts = self.fixed_parts(doc, &[vector::ENTRY])?;
        parts.push(Cow::Borrowed(own));
        let views: Vec<&[u8]> = parts.iter().map(AsRef::as_ref).collect();
        occurring(&views, candidates, self.cancel, MAX_SCAN_BYTES).map_err(str::to_owned)
    }

    /// The pruned vector and text entries (module docs), each `Ok(None)` when unchanged and `Err` when skipped.
    fn pruned_vector_and_text(&self, after: &StudioDocument, removed: &[&StudioLayer], ids: &BTreeSet<String>, vector_before: Option<&[u8]>) -> (Part, Part) {
        let gone = |id: &str| ids.contains(id);
        // The vector entry: decoded against the document after the step plus the removed layers (its records still
        // name them).
        let mut union = after.clone();
        union.layers.extend(removed.iter().map(|l| (*l).clone()));
        let own_styles: BTreeSet<String> = removed.iter().filter_map(|l| vector::style::layer_style_id(&l.layer_id).ok()).collect();
        let removed_paths: BTreeSet<String> = removed.iter().flat_map(|l| payload_paths(l).iter().cloned()).collect();
        let vector_bytes = if vector::entry_persistable() { self.run.document_entry(vector::ENTRY) } else { None };
        let vector_named: BTreeSet<String> = ids.iter().chain(&removed_paths).chain(&own_styles).cloned().collect();
        let vector_original: Result<Option<VectorEntry>, String> = match vector_bytes {
            Some(bytes) if mentions(bytes, &vector_named) => match VectorEntry::decode(bytes, &union, self.cancel).or_else(|_| VectorEntry::decode(bytes, after, self.cancel)) {
                Ok(e) => Ok(Some(e)),
                Err(e) => Err(format!("does not decode: {e}")),
            },
            _ => Ok(None),
        };
        // Story candidates: removed text layers' stories, their text records' stories and the stories of the bindings
        // of removed layers, read before the step (review F7) and after it.
        let mut stories: BTreeSet<String> = removed
            .iter()
            .filter_map(|l| match &l.payload {
                Payload::Text { story_id } => Some(story_id.clone()),
                _ => None,
            })
            .collect();
        let bindings = |e: &VectorEntry, out: &mut BTreeSet<String>| {
            out.extend(e.records.area_texts.iter().filter(|a| gone(&a.container_layer_id)).map(|a| a.text_id.clone()));
            out.extend(e.records.path_texts.iter().filter(|p| gone(&p.path_layer_id)).map(|p| p.story_id.clone()));
        };
        if let Some(e) = vector_before.and_then(|b| VectorEntry::decode(b, &union, self.cancel).ok()) {
            bindings(&e, &mut stories);
        }
        if let Ok(Some(e)) = &vector_original {
            bindings(e, &mut stories);
        }
        let text_bytes = self.run.document_entry(text::ENTRY);
        let type_styles: BTreeSet<String> = removed.iter().flat_map(|l| own_type_styles(&l.layer_id)).collect();
        let text_named: BTreeSet<String> = ids.iter().chain(&stories).chain(&type_styles).cloned().collect();
        let text_original: Result<Option<TextEntry>, String> = match text_bytes {
            Some(bytes) if mentions(bytes, &text_named) => TextEntry::decode(bytes, after, self.cancel).map(Some).map_err(|e| format!("does not decode: {e}")),
            _ => Ok(None),
        };
        if let Ok(Some(t)) = &text_original {
            stories.extend(t.layers.iter().filter(|l| gone(&l.layer_id)).map(|l| l.story_id.clone()));
        }
        // Every candidate id, scanned once in the bytes the prune does not write (review F4).
        let mut candidates: BTreeSet<String> = removed_paths.iter().chain(&own_styles).chain(&stories).chain(&type_styles).cloned().collect();
        if let Ok(Some(e)) = &vector_original {
            candidates.extend(e.records.boolean_groups.iter().filter(|g| gone(&g.layer_id)).flat_map(|g| g.fill_geometry_path_ids.iter().cloned()));
            candidates.extend(e.records.gradient_meshes.iter().filter(|m| gone(&m.layer_id)).filter_map(|m| m.source_path_id.clone()));
            if let Some(d) = &e.design {
                candidates.extend(d.nodes.iter().filter(|n| n.node.node_kind == NodeKind::Layer && gone(&n.node.node_id)).flat_map(|n| n.style_refs.values().cloned()));
            }
        }
        let fixed = self.fixed_parts(after, &[vector::ENTRY, text::ENTRY]).and_then(|parts| {
            let views: Vec<&[u8]> = parts.iter().map(AsRef::as_ref).collect();
            occurring(&views, &candidates, self.cancel, MAX_SCAN_BYTES).map_err(str::to_owned)
        });
        let vector = match (vector_original, &fixed) {
            (Ok(None), _) => Ok(None),
            (Err(e), _) => Err(e),
            (Ok(Some(_)), Err(e)) => Err(format!("reference scan: {e}")),
            (Ok(Some(original)), Ok(fixed)) => self.pruned_vector(after, &union, original, ids, own_styles, &candidates, fixed, text_bytes.unwrap_or_default()),
        };
        let vector_now: &[u8] = match &vector {
            Ok(Some(b)) => b,
            _ => vector_bytes.unwrap_or_default(),
        };
        let text = match (text_original, &fixed) {
            (Ok(None), _) => Ok(None),
            (Err(e), _) => Err(e),
            (Ok(Some(_)), Err(e)) => Err(format!("reference scan: {e}")),
            (Ok(Some(original)), Ok(fixed)) => self.pruned_text(after, original, ids, &stories, &type_styles, fixed, vector_now),
        };
        (vector, text)
    }

    /// The vector entry without the records of the removed layers (module docs). `fixed` are the candidates the bytes
    /// the prune does not write name; `text` is the stored text entry.
    #[allow(clippy::too_many_arguments)]
    fn pruned_vector(&self, after: &StudioDocument, union: &StudioDocument, original: VectorEntry, ids: &BTreeSet<String>, own_styles: BTreeSet<String>, candidates: &BTreeSet<String>, fixed: &BTreeSet<String>, text: &[u8]) -> Part {
        let gone = |id: &str| ids.contains(id);
        let mut entry = original.clone();
        let r = &mut entry.records;
        r.board_nodes.retain(|n| !gone(&n.layer_id));
        r.area_texts.retain(|a| !gone(&a.container_layer_id));
        r.path_texts.retain(|p| !gone(&p.path_layer_id));
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
        r.boolean_groups.retain(|g| !gone(&g.layer_id));
        r.envelopes.retain(|e| !gone(&e.layer_id) && !matches!(&e.envelope, StudioEnvelopeShape::TopObject { source_layer_id } if gone(source_layer_id)));
        r.gradient_meshes.retain(|m| !gone(&m.layer_id));
        r.live_filters.retain(|s| !gone(&s.layer_id));
        entry.live.retain(|s| !gone(&s.layer_id));
        let style_candidates = self.prune_design(after, union, &original, &mut entry, ids, own_styles);
        // Path records of the removed layers that nothing references any more (the styles still in place).
        let paths: BTreeSet<String> = candidates.iter().filter(|c| entry.paths.iter().any(|r| &r.path_id == *c)).cloned().collect();
        if !paths.is_empty() {
            let mut tentative = entry.clone();
            tentative.paths.retain(|r| !paths.contains(&r.path_id));
            let own = tentative.to_bytes(&after.document_id)?;
            let found = occurring(&[&own, text], &paths, self.cancel, MAX_SCAN_BYTES).map_err(|e| format!("reference scan: {e}"))?;
            entry.paths.retain(|r| !paths.contains(&r.path_id) || fixed.contains(&r.path_id) || found.contains(&r.path_id));
        }
        // Own appearance styles nothing references any more leave with the bindings keyed to them.
        if !style_candidates.is_empty() {
            let mut tentative = entry.clone();
            drop_paint_styles(&mut tentative, &style_candidates);
            let own = tentative.to_bytes(&after.document_id)?;
            let found = occurring(&[&own, text], &style_candidates, self.cancel, MAX_SCAN_BYTES).map_err(|e| format!("reference scan: {e}"))?;
            let unreferenced: BTreeSet<String> = style_candidates.into_iter().filter(|c| !fixed.contains(c) && !found.contains(c)).collect();
            drop_paint_styles(&mut entry, &unreferenced);
        }
        // Integrity (review F4): every path the document and the records name, held before, still resolves.
        let held: BTreeSet<&str> = original.paths.iter().map(|p| p.path_id.as_str()).collect();
        if let Some(lost) = named_paths(after, &entry).into_iter().find(|p| held.contains(p.as_str()) && entry.path(p).is_none()) {
            return Err(format!("path {lost} would no longer resolve"));
        }
        if entry == original {
            return Ok(None);
        }
        let pruned = entry.to_bytes(&after.document_id)?;
        VectorEntry::decode(&pruned, after, self.cancel).map_err(|e| format!("the pruned entry does not decode: {e}"))?;
        Ok(Some(pruned))
    }

    /// Lead ruling Q3 (2026-11-13): the design records of the removed layers (module docs), on `entry`; returns the own
    /// appearance style ids that may leave if nothing references them. `original` is the entry before any prune (the
    /// instance bindings are read from it over `union`, the document holding the removed layers too).
    fn prune_design(&self, after: &StudioDocument, union: &StudioDocument, original: &VectorEntry, entry: &mut VectorEntry, ids: &BTreeSet<String>, own_styles: BTreeSet<String>) -> BTreeSet<String> {
        let Some(mut d) = entry.design.clone() else { return BTreeSet::new() };
        let gone = |id: &str| ids.contains(id);
        let names = d.components.iter().any(|k| gone(&k.layer_id))
            || d.component_sets.iter().any(|s| gone(&s.layer_id))
            || d.nodes.iter().any(|n| n.node.node_kind == NodeKind::Layer && gone(&n.node.node_id))
            || !d.component_instances.is_empty()
            || d.paint_styles.iter().any(|s| own_styles.contains(&s.style_id));
        if !names {
            return BTreeSet::new();
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
        candidates.retain(|c| d.paint_styles.iter().any(|s| &s.style_id == c && s.publishing.publish_key == APPEARANCE_KEY));
        entry.design = Some(d);
        candidates
    }

    /// The text entry without the records of the removed layers (module docs). `fixed` are the candidates the bytes the
    /// prune does not write name; `vector` is the vector entry as the prune leaves it.
    #[allow(clippy::too_many_arguments)]
    fn pruned_text(&self, after: &StudioDocument, original: TextEntry, ids: &BTreeSet<String>, stories: &BTreeSet<String>, type_styles: &BTreeSet<String>, fixed: &BTreeSet<String>, vector: &[u8]) -> Part {
        let mut entry = original.clone();
        entry.layers.retain(|l| !ids.contains(&l.layer_id));
        let stories: BTreeSet<String> = stories.iter().filter(|s| entry.stories.iter().any(|x| &x.story_id == *s)).cloned().collect();
        if !stories.is_empty() {
            let mut tentative = entry.clone();
            remove_text(&mut tentative, &stories, &BTreeSet::new());
            let found = occurring(&[&tentative.to_bytes()?, vector], &stories, self.cancel, MAX_SCAN_BYTES).map_err(|e| format!("reference scan: {e}"))?;
            let unreferenced: BTreeSet<String> = stories.into_iter().filter(|s| !fixed.contains(s) && !found.contains(s)).collect();
            remove_text(&mut entry, &unreferenced, &BTreeSet::new());
        }
        let styles: BTreeSet<String> = type_styles.iter().filter(|s| entry.styles.styles.iter().any(|x| &x.style_id == *s)).cloned().collect();
        if !styles.is_empty() {
            let mut tentative = entry.clone();
            remove_text(&mut tentative, &BTreeSet::new(), &styles);
            let found = occurring(&[&tentative.to_bytes()?, vector], &styles, self.cancel, MAX_SCAN_BYTES).map_err(|e| format!("reference scan: {e}"))?;
            let unreferenced: BTreeSet<String> = styles.into_iter().filter(|s| !fixed.contains(s) && !found.contains(s)).collect();
            remove_text(&mut entry, &BTreeSet::new(), &unreferenced);
        }
        if entry == original {
            return Ok(None);
        }
        let pruned = entry.to_bytes()?;
        TextEntry::decode(&pruned, after, self.cancel).map_err(|e| format!("the pruned entry does not decode: {e}"))?;
        Ok(Some(pruned))
    }

    /// Lead ruling Q2 (2026-11-13): a replacement (knife, shape builder, blob brush, pathfinder) of `layers` refuses
    /// typed `structure_would_drop_dependents` when one of them is the source of a clip or mask another layer carries
    /// or carries area or path text, naming those dependents; nothing changes. (Carrying them over is a next-lane
    /// candidate.) An unreadable vector entry refuses nothing here (the replacement reads it itself).
    pub(super) fn refuse_dropping_dependents(&self, op: &str, layers: &[String]) -> Result<(), String> {
        let Ok(entry) = self.vector_entry() else { return Ok(()) };
        let r = &entry.records;
        let mut named: Vec<String> = Vec::new();
        for l in layers {
            let sources: BTreeSet<&str> = r.clips.iter().filter(|c| &c.source_layer_id == l).map(|c| c.clip_id.as_str()).chain(r.masks.iter().filter(|m| &m.source_layer_id == l).map(|m| m.mask_id.as_str())).collect();
            for a in r.attachments.iter().filter(|a| a.clip_id.as_deref().is_some_and(|c| sources.contains(c)) || a.mask_id.as_deref().is_some_and(|m| sources.contains(m))) {
                named.push(format!("{l} clips or masks {}", a.layer_id));
            }
            for a in r.area_texts.iter().filter(|a| &a.container_layer_id == l) {
                named.push(format!("{l} carries area text {}", a.text_id));
            }
            for p in r.path_texts.iter().filter(|p| &p.path_layer_id == l) {
                named.push(format!("{l} carries path text {}", p.story_id));
            }
        }
        if named.is_empty() {
            return Ok(());
        }
        Err(format!("{op}: structure_would_drop_dependents ({})", named.join("; ")))
    }
}

/// Removes the paint styles `set` and the swatch, brush, marker, pattern and image bindings keyed to them.
fn drop_paint_styles(e: &mut VectorEntry, set: &BTreeSet<String>) {
    if set.is_empty() {
        return;
    }
    if let Some(d) = e.design.as_mut() {
        d.paint_styles.retain(|s| !set.contains(&s.style_id));
    }
    let r = &mut e.records;
    r.swatch_paints.retain(|p| !set.contains(&p.style_id));
    r.brush_strokes.retain(|p| !set.contains(&p.style_id));
    r.stroke_markers.retain(|m| !set.contains(&m.style_id));
    r.pattern_paints.retain(|p| !set.contains(&p.style_id));
    r.image_paints.retain(|p| !set.contains(&p.style_id));
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    thread_local! {
        /// Fault injection (V4-04, tests only): the prune's entry write is refused.
        pub(super) static REFUSE_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    fn set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_reference_glued_to_other_id_bytes_or_inside_a_longer_id_counts() {
        let id = "SVPT-01a1d557-eb59-7ede-a000-000000000001";
        let other = "SVPT-01a1d557-eb59-7ede-a000-000000000002";
        let glued = format!("{{\"k\":\"{id}_width\",\"x\":\"x-{id}\"}}");
        let found = occurring(&[glued.as_bytes(), b"nothing here"], &set(&[id, other]), &CancellationToken::default(), MAX_SCAN_BYTES).unwrap();
        assert_eq!(found, set(&[id]), "glued and prefixed occurrences keep the id; an absent one is not found");
    }

    #[test]
    fn a_scan_past_its_budget_or_canceled_refuses_typed() {
        let id = "STXT-01a1d557-eb59-7ede-9000-000000000001";
        let part = vec![b'x'; 64];
        assert_eq!(occurring(&[&part, &part], &set(&[id]), &CancellationToken::default(), 100), Err(SCAN_OVER_BUDGET));
        let cancel = CancellationToken::default();
        cancel.cancel();
        assert_eq!(occurring(&[&part], &set(&[id]), &cancel, MAX_SCAN_BYTES), Err("canceled"));
    }

    fn viewer() -> Viewer {
        use hsk_studio_accord::{ActorContext, ClientSessionId, TransientCorrelation};
        use hsk_studio_harness_core::document_new::{DocumentProfile, NewDocument};
        let actor = ActorContext::new("prune-unit", "principal", "owner", "owner-principal", "space", "session").unwrap();
        let context = hsk_studio_harness_core::RunContext::new(actor, ClientSessionId::parse("prune-unit").unwrap(), std::sync::Arc::new(TransientCorrelation::new()));
        let mut v = Viewer::with_context(super::super::render::Prefer::Cpu, hsk_studio_render_gpu::DeviceOptions::default(), context);
        v.new_document(&NewDocument::new(DocumentProfile::Design, None), None).unwrap();
        v
    }

    fn history(v: &Viewer) -> (usize, usize) {
        let h = v.run.history();
        (h["undo"].as_array().map_or(0, Vec::len), h["redo"].as_array().map_or(0, Vec::len))
    }

    /// Review F6: a step that removes a layer without pushing a history step of its own (here an undo) is not
    /// joined: nothing is written, the older step stays as it was, and the warning names it.
    #[test]
    fn a_step_that_pushed_no_history_step_is_not_joined() {
        let mut v = viewer();
        let id = v.new_raster_layer("a").unwrap();
        v.structure_step("probe", "probe".into(), |run| run.undo()).unwrap();
        assert!(!v.run.snapshot().unwrap().document().layers.iter().any(|l| l.layer_id == id), "the undo removed the layer");
        assert_eq!(v.prune_warnings(), ["prune_skipped:no_step"]);
        assert_eq!(history(&v), (0, 1), "the undone step stays redoable: nothing joined it");
    }

    /// Review F3: a refused prune write rolls the structure step back: the layer stays, the history is as before (no
    /// redo entry from the rollback) and the delete is refused.
    #[test]
    fn a_refused_prune_write_rolls_the_step_back() {
        let mut v = viewer();
        let id = v.new_raster_layer("a").unwrap();
        let before = (history(&v), v.run.snapshot().unwrap().document().layers.clone());
        REFUSE_WRITE.with(|c| c.set(true));
        let r = v.delete_layer(&id);
        REFUSE_WRITE.with(|c| c.set(false));
        assert!(r.is_err(), "the delete is refused: {r:?}");
        assert_eq!((history(&v), v.run.snapshot().unwrap().document().layers.clone()), before, "the step is rolled back");
    }

    #[test]
    fn own_type_styles_are_the_layer_uuid_and_its_variants() {
        let own = own_type_styles("SLYR-01a1d557-eb59-7ede-9000-000000000001");
        assert_eq!(own, ["STYS-01a1d557-eb59-7ede-9000-000000000001", "STYS-01a1d557-eb59-7ede-a000-000000000001", "STYS-01a1d557-eb59-7ede-b000-000000000001"]);
    }
}
