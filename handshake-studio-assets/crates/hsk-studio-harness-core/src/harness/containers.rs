//! `document.new` and the artboard and page commands of [`Run`] (lane SMB-DOC-NEW-1; [STU-SHL-023],
//! [STU-SHL-026], [STU-VEC-028], [STU-VEC-165]; rules in [`crate::document_new`]). Each command is one
//! op record and one Observe frame of codes and counters, carries the run's attribution, and each
//! edit is one named undo step: a [`ContainerStep`] (one revision) or, for the options, an entry-only
//! step (no revision, like every document entry). Not recordable: a recording reports them typed
//! ([STU-AUT-109]).
//!
//! What a delete refuses: an artboard a layer still names (`artboard_not_empty`), the
//! last page of a document (`last_page`) and the last artboard of a document with layers
//! (`last_artboard`), so a frame for the layers stays. A page delete takes the page's content with it
//! and updates the layout records that name the page or its content in the same step, or refuses
//! `page_bound_by_layout_records` ([`Run::delete_page`]). A page has no name of its own in Folio
//! (`StudioPage`): `page.rename` names the page's spread.
//!
//! The layout delete cascade (lane SMB-LAYOUT-DELETE-CASCADE-1; [STU-LAY-003] delete, [STU-LAY-122]
//! threads; [`cascade_layout_delete`]): the layers removed take every layer under them; a thread closes
//! over a removed frame (the frame before the removed run links to the frame after it); a story no
//! remaining layer shows goes with the records keyed to it, and so do the cell and note stories of
//! those records, closed over (review F3 of acd73bc7); the records that name a removed layer go
//! (graphic frames, frame options, vertical thresholds, applied object styles, anchored objects,
//! parent page items and the overrides of those items; a page-local replacement that goes becomes a
//! suppress override; a running text keeps its other frames). Records the cascade does not follow and
//! that would name something gone refuse typed (`page_bound_by_layout_records`,
//! `layer_bound_by_layout_records`), so a delete leaves no record Folio checks naming something gone
//! (tests/layout_delete_cascade.rs). Not closed: Folio checks no longdoc story id against the stored
//! stories, so a longdoc record keyed to a story the cascade does not reach would validate (next-lane
//! candidate). InDesign parity of the thread closing and of the replacement-to-suppress rule is
//! UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]

use super::{EntryRestore, HistoryStep, Run, StudioBusinessEventId, command_text};
use crate::document_new::{self as rules, ArtboardOptions, ArtboardOptionsEdit, NewDocument};
use hsk_studio_accord::{CancellationToken, CommandContext};
use hsk_studio_folio::{
    self as folio, Bounds, GeometryUnit, Length, PageSide, Parent, Payload, PortAddress, StudioArtboard, StudioItemOverride, StudioPage, StudioPageSpread, StudioXrefDestination,
};
use hsk_studio_session::{ContainerStep, LayoutRecords, Session, SessionConfig, SessionError};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

const ARTBOARD_SCHEMA: &str = "hsk.studio.artboard@1";

/// The two lists an edit changes, what it says back, and its history label.
struct ContainerEdit {
    label: String,
    detail: Value,
}

fn layer_names_node(document: &folio::StudioDocument, id: &str) -> bool {
    document.layers.iter().any(|l| matches!(&l.parent, Parent::Node(n) if n.node_id == id))
}

fn refit(spread: &mut StudioPageSpread, unit: GeometryUnit) {
    let width: f64 = spread.pages.iter().map(|p| p.width.value).sum();
    let height = spread.pages.iter().map(|p| p.height.value).fold(0.0, f64::max);
    spread.bounds.width = Length { value: rules::from_points(width, unit), unit };
    spread.bounds.height = Length { value: rules::from_points(height, unit), unit };
}

/// The page delete on the container lists (`Run::delete_page`).
fn delete_page_edit(document: &folio::StudioDocument, spreads: &mut Vec<StudioPageSpread>, page: &str) -> Result<ContainerEdit, &'static str> {
    let (si, pi) = locate(spreads, page).ok_or("unknown_page")?;
    if rules::page_count(document) <= 1 {
        return Err("last_page");
    }
    // A text frame sits on one page (lane SMB-R60-FOLLOWUPS-1 item 6: the document would name a page it no
    // longer holds). `Run::delete_page` removes the content first (`page_delete_plan`); this is the guard
    // of the plain container step.
    if document.layers.iter().any(|l| l.text_frame.as_ref().is_some_and(|f| f.page_id == page)) {
        return Err("page_not_empty");
    }
    if spreads[si].pages.len() == 1 {
        if layer_names_node(document, &spreads[si].page_spread_id) {
            return Err("page_not_empty");
        }
        spreads.remove(si);
    } else {
        spreads[si].pages.remove(pi);
        refit(&mut spreads[si], document.geometry_unit);
    }
    Ok(ContainerEdit { label: format!("delete_page {page}"), detail: json!({"page_id": page}) })
}

fn locate(spreads: &[StudioPageSpread], page: &str) -> Option<(usize, usize)> {
    spreads.iter().enumerate().find_map(|(si, s)| s.pages.iter().position(|p| p.page_id == page).map(|pi| (si, pi)))
}

fn margins_fit(page: &StudioPage) -> bool {
    let m = &page.margins;
    page.width.value - m.left.value - m.right.value > 0.0 && page.height.value - m.top.value - m.bottom.value > 0.0
}

/// The page-side after `side`, as a following page alternates.
const fn next_side(side: PageSide) -> PageSide {
    match side {
        PageSide::RightHand => PageSide::LeftHand,
        PageSide::LeftHand => PageSide::RightHand,
        PageSide::SingleSided => PageSide::SingleSided,
    }
}

/// N of a page key of the mint's form `page-N`, else `None`.
fn page_number(key: &str) -> Option<u64> {
    let digits = key.strip_prefix("page-")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Adds to `out` every `page-N` a document entry body names in an id field (lane SMB-R60-FOLLOWUPS-1 item
/// 4, lead ruling 2026-11-11): a JSON body's string values under `page_start` or a key ending `_id` or
/// `_ids` (the fields that hold page keys: `page_id` of a parent binding, a graphic frame or a guide,
/// `page_start` of a section), never free text, so a story naming `page-N` changes no mint. A body that
/// is not JSON (or nests past serde_json's recursion limit) names none.
fn page_id_fields(bytes: &[u8], out: &mut BTreeSet<u64>) {
    fn id_field(key: &str) -> bool {
        key == "page_start" || key.ends_with("_id") || key.ends_with("_ids")
    }
    fn walk(value: &Value, in_id_field: bool, out: &mut BTreeSet<u64>) {
        match value {
            Value::String(s) if in_id_field => out.extend(page_number(s)),
            Value::Array(items) => items.iter().for_each(|v| walk(v, in_id_field, out)),
            Value::Object(map) => map.iter().for_each(|(k, v)| walk(v, id_field(k), out)),
            _ => {}
        }
    }
    if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
        walk(&value, false, out);
    }
}

/// What [`cascade_layout_delete`] removed: opaque ids and counts, the op record's detail.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayoutCascade {
    /// The layers removed, in document order: the ones asked for and every layer under them.
    pub layers: Vec<String>,
    /// Kept frames whose thread link now skips the removed frames.
    pub relinked: Vec<String>,
    /// Stories no kept layer shows, and the note and cell stories of the records removed with them.
    pub stories: Vec<String>,
    /// Layout records removed or rewritten.
    pub records: u64,
}

impl LayoutCascade {
    /// The counts of the op record's detail.
    pub fn detail(&self) -> Value {
        json!({"layers_removed": self.layers.len(), "frames_relinked": self.relinked.len(), "stories_removed": self.stories.len(), "records_removed": self.records})
    }
}

/// A page delete with content as [`Run::delete_page_with_content`] plans it.
#[derive(Clone, Debug, PartialEq)]
pub struct PageDelete {
    /// The document without the page and its content.
    pub document: folio::StudioDocument,
    /// The updated records; `None` for a document without layout records (none is written).
    pub records: Option<LayoutRecords>,
    pub cascade: LayoutCascade,
}

/// Removes the `items` that are `gone`, adding the number removed to `count`.
fn drop_where<T>(items: &mut Vec<T>, count: &mut u64, mut gone: impl FnMut(&T) -> bool) {
    let before = items.len();
    items.retain(|t| !gone(t));
    *count = count.saturating_add(u64::try_from(before.saturating_sub(items.len())).unwrap_or(u64::MAX));
}

/// Removes `doomed` (and every layer under them) from `document` and the stored layout `records`, in
/// place (module docs, "The layout delete cascade"). Ids `document` does not hold are ignored. Bounded:
/// every layer and thread link is visited a bounded number of times; nothing here validates, so the
/// caller validates `records` against the successor (and the session the document) before committing.
pub fn cascade_layout_delete(document: &mut folio::StudioDocument, records: &mut LayoutRecords, doomed: &BTreeSet<String>) -> LayoutCascade {
    let mut out = LayoutCascade::default();
    // The layers under a removed layer go with it (a group's children, at any depth).
    let mut children: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for l in &document.layers {
        if let Parent::Node(n) = &l.parent {
            children.entry(n.node_id.as_str()).or_default().push(l.layer_id.as_str());
        }
    }
    let held: BTreeSet<&str> = document.layers.iter().map(|l| l.layer_id.as_str()).collect();
    let mut gone: BTreeSet<String> = BTreeSet::new();
    let mut stack: Vec<&str> = doomed.iter().map(String::as_str).filter(|id| held.contains(id)).collect();
    while let Some(id) = stack.pop() {
        if gone.insert(id.to_owned()) {
            stack.extend(children.get(id).into_iter().flatten().copied());
        }
    }
    if gone.is_empty() {
        return out;
    }
    // Threads close over every removed run: the kept frame before a run links to the kept frame after it.
    let links: BTreeMap<String, (Option<String>, Option<String>)> =
        document.layers.iter().filter_map(|l| l.text_frame.as_ref().map(|f| (l.layer_id.clone(), (f.previous_text_frame.clone(), f.next_text_frame.clone())))).collect();
    let mut next_of: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut previous_of: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut walked: BTreeSet<&str> = BTreeSet::new();
    for (id, (previous, _)) in &links {
        // A run starts at a removed frame whose previous frame is kept or absent.
        if !gone.contains(id) || previous.as_ref().is_some_and(|p| gone.contains(p) && links.contains_key(p)) {
            continue;
        }
        let before = previous.clone().filter(|p| links.contains_key(p));
        let mut at = Some(id.as_str());
        let mut after = None;
        while let Some(current) = at {
            if !walked.insert(current) {
                break;
            }
            let next = links.get(current).and_then(|(_, n)| n.as_deref()).filter(|n| links.contains_key(*n));
            match next {
                Some(n) if gone.contains(n) => at = Some(n),
                other => {
                    after = other.map(str::to_owned);
                    at = None;
                }
            }
        }
        if let Some(b) = &before {
            next_of.insert(b.clone(), after.clone());
        }
        if let Some(a) = &after {
            previous_of.insert(a.clone(), before.clone());
        }
    }
    // Stories the removed layers show.
    let shown_by_gone: BTreeSet<String> = document
        .layers
        .iter()
        .filter(|l| gone.contains(&l.layer_id))
        .filter_map(|l| match &l.payload {
            Payload::Text { story_id } => Some(story_id.clone()),
            _ => None,
        })
        .collect();
    // The layers and their graph presence (as chronicle's remove: edges out of and into them, their
    // operations, their outputs; the stacks they fed renumbered densely).
    let fed: BTreeSet<PortAddress> = document.graph.edges.iter().filter(|e| gone.contains(&e.source.layer_id) && !gone.contains(&e.target.layer_id)).map(|e| e.target.clone()).collect();
    document.graph.edges.retain(|e| !gone.contains(&e.source.layer_id) && !gone.contains(&e.target.layer_id));
    document.graph.outputs.retain(|o| !gone.contains(&o.layer_id));
    document.graph.operations.retain(|o| !gone.contains(&o.layer_id));
    for port in &fed {
        let mut ordinals: Vec<(u64, usize)> = document.graph.edges.iter().enumerate().filter(|(_, e)| &e.target == port).map(|(i, e)| (e.input_ordinal, i)).collect();
        ordinals.sort_unstable();
        for (n, (_, i)) in ordinals.into_iter().enumerate() {
            if let (Some(edge), Ok(n)) = (document.graph.edges.get_mut(i), u64::try_from(n)) {
                edge.input_ordinal = n;
            }
        }
    }
    out.layers = document.layers.iter().filter(|l| gone.contains(&l.layer_id)).map(|l| l.layer_id.clone()).collect();
    document.layers.retain(|l| !gone.contains(&l.layer_id));
    for l in &mut document.layers {
        let Some(f) = l.text_frame.as_mut() else { continue };
        let mut changed = false;
        if let Some(n) = next_of.get(&l.layer_id) {
            f.next_text_frame.clone_from(n);
            changed = true;
        }
        if let Some(p) = previous_of.get(&l.layer_id) {
            f.previous_text_frame.clone_from(p);
            changed = true;
        }
        if changed {
            out.relinked.push(l.layer_id.clone());
        }
    }
    let shown: BTreeSet<&str> = document
        .layers
        .iter()
        .filter_map(|l| match &l.payload {
            Payload::Text { story_id } => Some(story_id.as_str()),
            _ => None,
        })
        .collect();
    // The removed stories, closed over the records keyed to them (review F3 of acd73bc7): a removed
    // story's tables take their cell stories and its footnotes, endnotes and sidenotes their note
    // stories, and a story so taken takes its own records' stories in turn. Bounded: each pass adds at
    // least one story or stops, and every story added is named by a record.
    let mut stories: BTreeSet<String> = shown_by_gone.into_iter().filter(|s| !shown.contains(s.as_str())).collect();
    loop {
        let longdoc = &records.longdoc;
        let cells = longdoc.tables.iter().filter(|t| stories.contains(&t.story_id)).filter_map(|t| t.cell_story_id.clone());
        let notes = longdoc.footnotes.iter().filter(|x| stories.contains(&x.story_id)).map(|x| x.note_story_id.clone());
        let more = longdoc.endnotes.iter().chain(&longdoc.sidenotes).filter(|x| stories.contains(&x.story_id)).map(|x| x.note_story_id.clone());
        let next: Vec<String> = cells.chain(notes).chain(more).filter(|s| !shown.contains(s.as_str()) && !stories.contains(s)).collect();
        if next.is_empty() {
            break;
        }
        stories.extend(next);
    }
    let n = &mut out.records;
    let story_gone = |s: &str, stories: &BTreeSet<String>| stories.contains(s);
    // Long-document records: a removed graphic frame's record; every record keyed to a removed story.
    let longdoc = &mut records.longdoc;
    drop_where(&mut longdoc.graphic_frames, n, |g| gone.contains(&g.layer_id));
    drop_where(&mut longdoc.tables, n, |t| story_gone(&t.story_id, &stories));
    drop_where(&mut longdoc.footnotes, n, |x| story_gone(&x.story_id, &stories));
    for notes in [&mut longdoc.endnotes, &mut longdoc.sidenotes] {
        drop_where(notes, n, |x| story_gone(&x.story_id, &stories));
    }
    drop_where(&mut longdoc.index_marks, n, |m| story_gone(&m.story_id, &stories));
    for r in &mut longdoc.running_texts {
        let before = r.frame_ids.len();
        r.frame_ids.retain(|f| !gone.contains(f));
        if r.frame_ids.len() != before {
            *n = n.saturating_add(1);
        }
    }
    drop_where(&mut longdoc.running_texts, n, |r| r.frame_ids.is_empty());
    // Layout-flow records of a removed frame, layer or story.
    let flow = &mut records.flow;
    drop_where(&mut flow.frame_options, n, |f| gone.contains(&f.layer_id));
    drop_where(&mut flow.vertical_thresholds, n, |v| gone.contains(&v.layer_id));
    drop_where(&mut flow.applied_object_styles, n, |a| gone.contains(&a.layer_id));
    drop_where(&mut flow.anchored_objects, n, |a| gone.contains(&a.layer_id) || story_gone(&a.story_id, &stories));
    drop_where(&mut flow.paragraph_flows, n, |p| story_gone(&p.story_id, &stories));
    let anchors: BTreeSet<String> = flow.text_anchors.iter().filter(|a| story_gone(&a.story_id, &stories)).map(|a| a.anchor_id.clone()).collect();
    drop_where(&mut flow.text_anchors, n, |a| story_gone(&a.story_id, &stories));
    drop_where(&mut flow.cross_references, n, |x| {
        story_gone(&x.source_story_id, &stories)
            || match &x.destination {
                StudioXrefDestination::Paragraph { story_id, .. } => story_gone(story_id, &stories),
                StudioXrefDestination::TextAnchor { anchor_id } => anchors.contains(anchor_id),
            }
    });
    drop_where(&mut flow.conditional_ranges, n, |c| story_gone(&c.story_id, &stories));
    // Parent pages: a removed item leaves its parent, with the overrides of it; a removed page-local
    // replacement leaves the inherited item suppressed.
    let mut items_gone: BTreeSet<(String, String)> = BTreeSet::new();
    for p in &mut records.parents.parents {
        for i in p.parent_page_items.iter().filter(|i| gone.contains(&i.layer_id)) {
            items_gone.insert((p.parent_id.clone(), i.item_id.clone()));
        }
        drop_where(&mut p.parent_page_items, n, |i| gone.contains(&i.layer_id));
    }
    let item_ids: BTreeSet<&str> = items_gone.iter().map(|(_, i)| i.as_str()).collect();
    let kept_items: BTreeSet<&str> = records.parents.parents.iter().flat_map(|p| &p.parent_page_items).map(|i| i.item_id.as_str()).collect();
    for b in &mut records.parents.page_bindings {
        drop_where(&mut b.override_list, n, |o| item_ids.contains(o.parent_item_id()) && !kept_items.contains(o.parent_item_id()));
        for o in &mut b.override_list {
            if let StudioItemOverride::Replace { parent_item_id, replacement_layer_id } = o
                && gone.contains(replacement_layer_id)
            {
                *o = StudioItemOverride::Suppress { parent_item_id: parent_item_id.clone() };
                *n = n.saturating_add(1);
            }
        }
    }
    // Character records (lane SMB-TYPE-CHARACTER-ATTRIBUTES-1): a removed story's local overrides; node
    // hyperlinks to a removed text anchor (review F14).
    drop_where(&mut records.styles.character_overrides, n, |o| story_gone(&o.text_id, &stories));
    if !anchors.is_empty() {
        let unlinked = records.styles.drop_node_links(&|id: &str| !anchors.contains(id));
        *n = n.saturating_add(u64::try_from(unlinked).unwrap_or(u64::MAX));
    }
    drop_where(&mut records.stories, n, |s| stories.contains(&s.story_id));
    out.stories = stories.into_iter().collect();
    out
}

impl Run<'_> {
    /// Ids a new container must not take (review F1 of 368e1080): the containers whose chronicle
    /// `name` entry the session still holds (a deleted one keeps its entry for the redo) and the
    /// artboards the `artboard_options` entry names (an inert row would go live on a new artboard).
    fn remembered_container_ids(&self) -> BTreeSet<String> {
        let Some(session) = self.session.as_ref() else {
            return BTreeSet::new();
        };
        let mut ids: BTreeSet<String> = session.revisions().iter().map(|e| e.address.target.node_id.clone()).collect();
        if let Some(bytes) = session.document_entry(rules::ARTBOARD_OPTIONS_ENTRY)
            && let Ok(rows) = rules::parse_options(bytes, &session.snapshot().document().document_id)
        {
            ids.extend(rows.into_iter().map(|o| o.artboard_id));
        }
        for step in self.history_containers() {
            ids.extend(step.expect_artboards.iter().chain(&step.write_artboards).map(|a| a.artboard_id.clone()));
            ids.extend(step.expect_spreads.iter().chain(&step.write_spreads).map(|s| s.page_spread_id.clone()));
        }
        // A layout step may remove a spread too (`delete_page` with layout records).
        for document in self.history_layout_documents() {
            ids.extend(document.artboards.iter().map(|a| a.artboard_id.clone()));
            ids.extend(document.page_spreads.iter().map(|s| s.page_spread_id.clone()));
        }
        ids
    }

    /// The container steps of the undo and redo histories (a group's inner steps included).
    fn history_containers(&self) -> Vec<&ContainerStep> {
        fn walk<'s>(steps: &'s [HistoryStep], out: &mut Vec<&'s ContainerStep>) {
            for step in steps {
                out.extend(step.containers.as_ref());
                if let Some(inner) = &step.group {
                    walk(inner, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.undo_stack, &mut out);
        walk(&self.redo_stack, &mut out);
        out
    }

    /// The documents the layout steps of the undo and redo histories quote and write (a group's inner
    /// steps included): a `page.delete` of a page the layout records bind is one (lane SMB-R60-FOLLOWUPS-1).
    fn history_layout_documents(&self) -> Vec<&folio::StudioDocument> {
        fn walk<'s>(steps: &'s [HistoryStep], out: &mut Vec<&'s folio::StudioDocument>) {
            for step in steps {
                if let Some(l) = &step.layout {
                    out.push(&l.expect);
                    out.push(&l.write);
                }
                if let Some(inner) = &step.group {
                    walk(inner, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.undo_stack, &mut out);
        walk(&self.redo_stack, &mut out);
        out
    }

    /// The first page number past every `page-N` the document, an id field of a document entry (never
    /// story text) or the history names, so a new page never takes the key of a deleted one that layout
    /// records or an undo may still name (review F1). Past `u64::MAX` (a page key may be
    /// `page-18446744073709551615`) it is the lowest n >= 1 none of them names (lane SMB-R60-FOLLOWUPS-1,
    /// review F2 of c43f2d90); `None` only when no number is free. The mint never loops.
    fn next_page_number(&self) -> Option<u64> {
        let Some(session) = self.session.as_ref() else {
            return Some(1);
        };
        let document = session.snapshot().document();
        let history = self.history_containers();
        let layouts = self.history_layout_documents();
        let pages = document
            .page_spreads
            .iter()
            .chain(history.iter().flat_map(|s| s.expect_spreads.iter().chain(&s.write_spreads)))
            .chain(layouts.iter().flat_map(|d| &d.page_spreads))
            .flat_map(|s| &s.pages);
        let mut named: BTreeSet<u64> = pages.filter_map(|p| page_number(&p.page_id)).collect();
        for token in hsk_studio_session::DOCUMENT_ENTRY_TOKENS {
            if let Some(bytes) = session.document_entry(token) {
                page_id_fields(bytes, &mut named);
            }
        }
        match named.last() {
            None => Some(1),
            Some(highest) => highest.checked_add(1).or_else(|| (1..=u64::MAX).find(|n| !named.contains(n))),
        }
    }

    /// `document.new`: replaces the working session with a blank document of `spec`, revision 0, no
    /// origin, dirty until saved, history empty. A refusal changes nothing (`canceled`,
    /// [`NewDocument::validate`]'s codes, `folio_rejected`). One record `new_document`, detail
    /// `document_id`, `profile`.
    pub fn new_document(&mut self, spec: &NewDocument) -> bool {
        const OP: &str = "new_document";
        self.note_unrecordable("document.new");
        let before = self.frames.count();
        let ctx = self.begin_at("document.new", Some(0), None);
        let refused = |run: &mut Self, code: &'static str| if run.session.is_some() { run.own_op(OP, before, Err(code)) } else { run.push(OP, before, Err(code), None, None) };
        if self.cancel.check().is_err() {
            return refused(self, "canceled");
        }
        // The clock only keeps two documents apart: nothing reads an id as a time.
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let document_id = rules::mint_document_id(now_ms, ctx.correlation().value());
        let document = match rules::blank_document(spec, &document_id) {
            Ok(document) => document,
            Err(code) => return refused(self, code),
        };
        let opened = Session::create_new(document, SessionConfig::new(self.context.actor().clone()), self.ports(), self.cancel);
        let ok = self.finish_open(OP, before, opened, false);
        if ok {
            let revision = self.revision().unwrap_or(0);
            self.emit_business_event(StudioBusinessEventId::DocumentCreated, &ctx, revision, &[]);
            self.attach_events_to_last_record();
            if let Some(record) = self.ops.last_mut()
                && record["detail"].is_object()
            {
                record["detail"]["document_id"] = json!(document_id);
                record["detail"]["profile"] = json!(spec.profile.token());
            }
        }
        ok
    }

    /// Runs one edit of the container lists as one [`ContainerStep`]: the edit sees the live document
    /// and writes the lists, the session checks and commits, the inverse goes on the undo history.
    fn container_edit<F>(&mut self, op: &str, command: &'static str, edit: F) -> bool
    where
        F: FnOnce(&folio::StudioDocument, &mut Vec<StudioArtboard>, &mut Vec<StudioPageSpread>) -> Result<ContainerEdit, &'static str>,
    {
        self.note_unrecordable(command);
        let before = self.frames.count();
        if self.session.is_none() {
            return self.push(op, before, Err("not_open"), None, None);
        }
        let ctx = self.begin(command, None);
        if self.cancel.check().is_err() {
            return self.own_op(op, before, Err("canceled"));
        }
        let cancel = self.cancel;
        let Some(session) = self.session.as_mut() else {
            return self.push(op, before, Err("not_open"), None, None);
        };
        let document = session.snapshot().document();
        let mut artboards = document.artboards.clone();
        let mut spreads = document.page_spreads.clone();
        let made = edit(document, &mut artboards, &mut spreads);
        let (expect_artboards, expect_spreads) = (document.artboards.clone(), document.page_spreads.clone());
        let ContainerEdit { label, mut detail } = match made {
            Ok(made) => made,
            Err(code) => return self.own_op(op, before, Err(code)),
        };
        let step = ContainerStep {
            command_id: command_text(&ctx).to_string(),
            correlation_id: ctx.correlation().value(),
            expect_artboards,
            write_artboards: artboards,
            expect_spreads,
            write_spreads: spreads,
        };
        let revision = session.document_revision();
        match session.submit_containers_as(&ctx, &step, revision, cancel) {
            Ok(applied) => {
                if applied.changed {
                    let inverse = step.inverse(step.command_id.clone(), step.correlation_id);
                    self.undo_stack.push(HistoryStep { label, inverse: Value::Null, entry: None, timeline: None, group: None, sequences: None, containers: Some(inverse), layout: None, ctx, revision: applied.revision });
                    self.redo_stack.clear();
                }
                detail["previous_revision"] = json!(applied.previous_revision);
                detail["revision"] = json!(applied.revision);
                detail["correlation_id"] = json!(applied.correlation_id);
                detail["changed"] = json!(applied.changed);
                self.push(op, before, Ok(detail), Some(applied.report), Some(applied.revision))
            }
            Err(f) => {
                let revision = self.revision();
                self.push(op, before, Err(f.code()), Some(f.report), revision)
            }
        }
    }

    /// Undo or redo of a container step: `submit` under the fresh command `ctx`; its inverse goes on
    /// the other history. Called by `Run::history_step`.
    pub(super) fn container_history(&mut self, undo: bool, step: HistoryStep, mut submit: ContainerStep, ctx: CommandContext, before: usize) -> bool {
        let op = if undo { "undo" } else { "redo" };
        let cancel = self.cancel;
        submit.command_id = command_text(&ctx).to_string();
        submit.correlation_id = ctx.correlation().value();
        let Some(session) = self.session.as_mut() else {
            self.restore_step(undo, step);
            return self.push(op, before, Err("not_open"), None, None);
        };
        let revision = session.document_revision();
        match session.submit_containers_as(&ctx, &submit, revision, cancel) {
            Ok(applied) => {
                if applied.changed && !applied.replayed {
                    let event = if undo { StudioBusinessEventId::HistoryUndone } else { StudioBusinessEventId::HistoryRedone };
                    self.emit_business_event(event, &ctx, applied.revision, &[]);
                }
                let detail = json!({"step": step.label, "previous_revision": applied.previous_revision, "revision": applied.revision, "correlation_id": applied.correlation_id, "changed": applied.changed});
                let inverse = submit.inverse(submit.command_id.clone(), submit.correlation_id);
                let next = HistoryStep { label: step.label, inverse: Value::Null, entry: None, timeline: None, group: None, sequences: None, containers: Some(inverse), layout: None, ctx, revision: applied.revision };
                if undo {
                    self.redo_stack.push(next);
                } else {
                    self.undo_stack.push(next);
                }
                self.push(op, before, Ok(detail), Some(applied.report), Some(applied.revision))
            }
            Err(f) => {
                self.restore_step(undo, step);
                let revision = self.revision();
                self.push(op, before, Err(f.code()), Some(f.report), revision)
            }
        }
    }

    /// `artboard.add` ([STU-SHL-026] New Artboard): `width` by `height` in document units, at `at`, or
    /// to the right of the others at [`rules::ARTBOARD_GAP`]. Refusals: `dimension_out_of_range`,
    /// `size_not_whole_pixels`, `position_out_of_range`, `name_too_long`, `name_invalid`,
    /// `too_many_artboards`. Detail `artboard_id`.
    pub fn add_artboard(&mut self, name: &str, width: f64, height: f64, at: Option<[f64; 2]>) -> bool {
        let remembered = self.remembered_container_ids();
        self.container_edit("add_artboard", "artboard.add", |document, artboards, _| {
            let unit = document.geometry_unit;
            rules::check_name(name)?;
            rules::check_size(width, height, unit)?;
            if artboards.len() >= rules::MAX_ARTBOARDS {
                return Err("too_many_artboards");
            }
            let [x, y] = at.unwrap_or_else(|| {
                let right = artboards.iter().map(|a| a.bounds.x.value + a.bounds.width.value + rules::ARTBOARD_GAP).fold(0.0, f64::max);
                [right, artboards.first().map_or(0.0, |a| a.bounds.y.value)]
            });
            rules::check_position(x, unit)?;
            rules::check_position(y, unit)?;
            let id = rules::mint_child_id(&document.document_id, "SART", u64::try_from(artboards.len()).unwrap_or(0).saturating_add(1), |id| remembered.contains(id) || artboards.iter().any(|a| a.artboard_id == id));
            let len = |value: f64| Length { value, unit };
            let record = serde_json::from_value::<StudioArtboard>(json!({"schema_id": ARTBOARD_SCHEMA, "artboard_id": id, "name": name, "parent": null, "bounds": Bounds { x: len(x), y: len(y), width: len(width), height: len(height) }})).map_err(|_| "document_encode")?;
            artboards.push(record);
            Ok(ContainerEdit { label: format!("add_artboard {name}"), detail: json!({"artboard_id": id}) })
        })
    }

    /// `artboard.rename` ([STU-SHL-026] Rename Artboard). Refusals: `unknown_artboard`, `name_too_long`, `name_invalid`.
    pub fn rename_artboard(&mut self, artboard: &str, name: &str) -> bool {
        self.container_edit("rename_artboard", "artboard.rename", |_, artboards, _| {
            rules::check_name(name)?;
            let target = artboards.iter_mut().find(|a| a.artboard_id == artboard).ok_or("unknown_artboard")?;
            target.name = name.to_owned();
            Ok(ContainerEdit { label: format!("rename_artboard {name}"), detail: json!({"artboard_id": artboard}) })
        })
    }

    /// `artboard.resize`: the corner stays. Refusals: `unknown_artboard`, `dimension_out_of_range`, `size_not_whole_pixels`.
    pub fn resize_artboard(&mut self, artboard: &str, width: f64, height: f64) -> bool {
        self.container_edit("resize_artboard", "artboard.resize", |document, artboards, _| {
            let unit = document.geometry_unit;
            let target = artboards.iter_mut().find(|a| a.artboard_id == artboard).ok_or("unknown_artboard")?;
            rules::check_size(width, height, unit)?;
            target.bounds.width = Length { value: width, unit };
            target.bounds.height = Length { value: height, unit };
            Ok(ContainerEdit { label: format!("resize_artboard {}", target.name), detail: json!({"artboard_id": artboard}) })
        })
    }

    /// `artboard.move`. Refusals: `unknown_artboard`, `position_out_of_range`, `size_not_whole_pixels`.
    pub fn move_artboard(&mut self, artboard: &str, x: f64, y: f64) -> bool {
        self.container_edit("move_artboard", "artboard.move", |document, artboards, _| {
            let unit = document.geometry_unit;
            let target = artboards.iter_mut().find(|a| a.artboard_id == artboard).ok_or("unknown_artboard")?;
            rules::check_position(x, unit)?;
            rules::check_position(y, unit)?;
            target.bounds.x = Length { value: x, unit };
            target.bounds.y = Length { value: y, unit };
            Ok(ContainerEdit { label: format!("move_artboard {}", target.name), detail: json!({"artboard_id": artboard}) })
        })
    }

    /// `artboard.delete` ([STU-SHL-026] Delete Artboard). Refusals: `unknown_artboard`,
    /// `artboard_not_empty`, `last_artboard` (module docs).
    pub fn delete_artboard(&mut self, artboard: &str) -> bool {
        self.container_edit("delete_artboard", "artboard.delete", |document, artboards, _| {
            let at = artboards.iter().position(|a| a.artboard_id == artboard).ok_or("unknown_artboard")?;
            if layer_names_node(document, artboard) {
                return Err("artboard_not_empty");
            }
            if artboards.len() == 1 && !document.layers.is_empty() {
                return Err("last_artboard");
            }
            let gone = artboards.remove(at);
            Ok(ContainerEdit { label: format!("delete_artboard {}", gone.name), detail: json!({"artboard_id": artboard}) })
        })
    }

    /// `page.add` ([STU-SHL-026] Insert Pages): a page after `after` (the last page when absent) on its
    /// spread, or on a new spread with `new_spread`, named `name`. Size in points (STU-LAY-100), else the
    /// neighbour's; margins and bleed the neighbour's. The first page of a document needs a size and is
    /// on a new spread. Refusals: `unknown_page`, `size_required`, `dimension_out_of_range`,
    /// `margins_exceed_page`, `too_many_pages`, `name_too_long`, `name_invalid`, `name_needs_new_spread`,
    /// `page_number_exhausted` (no `page-N` key is left past the highest one the document, an entry or the
    /// history names: the mint never loops). Detail `page_id`, `page_spread_id`.
    pub fn add_page(&mut self, after: Option<&str>, new_spread: bool, size: Option<(f64, f64)>, name: Option<&str>) -> bool {
        let remembered = self.remembered_container_ids();
        let page_number = self.next_page_number();
        self.container_edit("add_page", "page.add", |document, _, spreads| {
            if let Some(name) = name {
                rules::check_name(name)?;
            }
            if rules::page_count(document) >= rules::MAX_PAGES {
                return Err("too_many_pages");
            }
            if let Some((w, h)) = size {
                rules::check_size(w, h, GeometryUnit::Pt)?;
            }
            let at = match after {
                Some(page) => Some(locate(spreads, page).ok_or("unknown_page")?),
                None => spreads.iter().enumerate().rev().find_map(|(si, s)| s.pages.len().checked_sub(1).map(|pi| (si, pi))),
            };
            let unit = document.geometry_unit;
            let page_id = page_number.and_then(|n| rules::mint_page_id(document, n)).ok_or("page_number_exhausted")?;
            let spread_id = rules::mint_child_id(&document.document_id, "SPGS", u64::try_from(spreads.len()).unwrap_or(0).saturating_add(1), |id| remembered.contains(id) || spreads.iter().any(|s| s.page_spread_id == id));
            let default_name = format!("spread {}", spreads.len().saturating_add(1));
            let Some((si, pi)) = at else {
                let (w, h) = size.ok_or("size_required")?;
                spreads.push(rules::one_page_spread(spread_id.clone(), name.map_or(default_name, str::to_owned), page_id.clone(), w, h, unit));
                return Ok(ContainerEdit { label: format!("add_page {page_id}"), detail: json!({"page_id": page_id, "page_spread_id": spread_id}) });
            };
            if name.is_some() && !new_spread {
                return Err("name_needs_new_spread");
            }
            let mut page = spreads[si].pages[pi].clone();
            page.page_id.clone_from(&page_id);
            page.side = next_side(page.side);
            if let Some((w, h)) = size {
                page.width.value = w;
                page.height.value = h;
                if !margins_fit(&page) {
                    return Err("margins_exceed_page");
                }
            }
            let spread_of_page = if new_spread {
                let template = &spreads[si];
                let spread = StudioPageSpread {
                    schema_id: template.schema_id.clone(),
                    page_spread_id: spread_id.clone(),
                    name: name.map_or(default_name, str::to_owned),
                    parent: template.parent.clone(),
                    bounds: template.bounds.clone(),
                    pages: vec![page],
                };
                let at = si.saturating_add(1);
                spreads.insert(at, spread);
                at
            } else {
                spreads[si].pages.insert(pi.saturating_add(1), page);
                si
            };
            refit(&mut spreads[spread_of_page], unit);
            let spread_id = spreads[spread_of_page].page_spread_id.clone();
            Ok(ContainerEdit { label: format!("add_page {page_id}"), detail: json!({"page_id": page_id, "page_spread_id": spread_id}) })
        })
    }

    /// `page.rename`: names the page's spread (module docs). Refusals: `unknown_page`, `name_too_long`, `name_invalid`.
    pub fn rename_page(&mut self, page: &str, name: &str) -> bool {
        self.container_edit("rename_page", "page.rename", |_, _, spreads| {
            rules::check_name(name)?;
            let (si, _) = locate(spreads, page).ok_or("unknown_page")?;
            spreads[si].name = name.to_owned();
            Ok(ContainerEdit { label: format!("rename_page {name}"), detail: json!({"page_id": page, "page_spread_id": spreads[si].page_spread_id}) })
        })
    }

    /// `page.resize` ([STU-SHL-026] Page Size): points (STU-LAY-100); the spread follows its pages.
    /// Refusals: `unknown_page`, `dimension_out_of_range`, `margins_exceed_page`.
    pub fn resize_page(&mut self, page: &str, width: f64, height: f64) -> bool {
        self.container_edit("resize_page", "page.resize", |document, _, spreads| {
            let (si, pi) = locate(spreads, page).ok_or("unknown_page")?;
            rules::check_size(width, height, GeometryUnit::Pt)?;
            let target = &mut spreads[si].pages[pi];
            target.width.value = width;
            target.height.value = height;
            if !margins_fit(target) {
                return Err("margins_exceed_page");
            }
            refit(&mut spreads[si], document.geometry_unit);
            Ok(ContainerEdit { label: format!("resize_page {page}"), detail: json!({"page_id": page}) })
        })
    }

    /// `page.delete` ([STU-SHL-026] Delete Pages, [STU-LAY-003] delete): the page with its content; a spread
    /// left with no page goes too, with every layer it holds. Refusals: `unknown_page`, `last_page` (module docs).
    ///
    /// Content and layout records (lane SMB-LAYOUT-DELETE-CASCADE-1, after lane SMB-R60-FOLLOWUPS-1 item 6): the
    /// page goes through [`Run::delete_page_with_content`], and the page, its content and the updated records
    /// commit as ONE layout step (`Run::submit_layout`, one revision, one undo step labelled `delete_page <page>`),
    /// so the entry still validates after save and reopen and undo restores the page, its content and its records
    /// together. Detail `page_id`, `layout_records` (`updated`, or `none` for a document without a layout records
    /// entry, which gets none: review F7 of acd73bc7) and the cascade's counts ([`LayoutCascade::detail`]).
    /// Refused typed, changing nothing: `canceled`, `layout_records_unreadable` and `layout_records_stale` (a page
    /// with content while the stored entry does not decode, or no longer validates: the records could not
    /// follow), and `page_bound_by_layout_records`, a fail-closed guard, when the updated set still would not
    /// validate against the document without the page, its op record detail naming the failing record
    /// (`record`: the Folio diagnostic target with the record kind and id; `folio_code`): a delete never leaves a
    /// stale entry the cascade knows of (tests/layout_delete_cascade.rs). An empty page whose records need no
    /// update, or none stored, keeps the plain container step (as an empty page under a stale entry:
    /// `submit_layout` refuses a stale entry and the next layout edit reports it).
    pub fn delete_page(&mut self, page: &str) -> bool {
        const OP: &str = "delete_page";
        const COMMAND: &str = "page.delete";
        match self.page_delete_plan(page) {
            Ok(None) => self.container_edit(OP, COMMAND, |document, _, spreads| delete_page_edit(document, spreads, page)),
            Ok(Some(plan)) => {
                let depth = self.undo_stack.len();
                let kept = if plan.records.is_some() { "updated" } else { "none" };
                if !self.submit_layout_records(OP, COMMAND, plan.document, plan.records) {
                    return false;
                }
                if self.undo_stack.len() > depth
                    && let Some(step) = self.undo_stack.last_mut()
                {
                    step.label = format!("{OP} {page}");
                }
                self.add_cascade_detail(&plan.cascade, &[("page_id", json!(page)), ("layout_records", json!(kept))]);
                true
            }
            Err((code, detail)) => self.refuse_with_detail(OP, COMMAND, code, detail),
        }
    }

    /// The page delete with content as one pure function (review F2 of acd73bc7: `page.delete`, the layout
    /// editor's `layout.page.delete_page` and smart reflow's `delete_empty_pages` share it): the text frames on
    /// `page`, the graphic frames the records place on it and, when its spread holds no other page, every layer
    /// the spread holds go through [`cascade_layout_delete`]; the page goes (and the spread with it); the page's
    /// parent-page binding is dropped; a section that starts on the page starts on the next page instead, or is
    /// dropped when no page of its own is left (the next page starts another section, or the page was the last);
    /// the updated records must validate against the result. `records` are the live, valid records, `None` for a
    /// document without them (then none come back). Refusals, nothing changed: `unknown_page`, `last_page`,
    /// `canceled`, `page_bound_by_layout_records` (detail `page_id`, `record`, `folio_code`).
    pub fn delete_page_with_content(document: &folio::StudioDocument, records: Option<&LayoutRecords>, page: &str, cancel: &CancellationToken) -> Result<PageDelete, (&'static str, Value)> {
        let at = document.locate_page(page).ok_or(("unknown_page", json!({"page_id": page})))?;
        let spread = (at.spread.pages.len() == 1).then(|| at.spread.page_spread_id.clone());
        let mut doomed: BTreeSet<String> = document
            .layers
            .iter()
            .filter(|l| l.text_frame.as_ref().is_some_and(|f| f.page_id == page) || spread.as_ref().is_some_and(|s| matches!(&l.parent, Parent::Node(n) if n.node_id == *s)))
            .map(|l| l.layer_id.clone())
            .collect();
        if let Some(r) = records {
            doomed.extend(r.longdoc.graphic_frames.iter().filter(|g| g.page_id == page).map(|g| g.layer_id.clone()));
        }
        let mut successor = document.clone();
        let mut updated = records.cloned();
        let mut scratch = LayoutRecords::new(&document.document_id);
        let mut cascade = cascade_layout_delete(&mut successor, updated.as_mut().unwrap_or(&mut scratch), &doomed);
        let mut spreads = successor.page_spreads.clone();
        delete_page_edit(&successor, &mut spreads, page).map_err(|code| (code, json!({"page_id": page})))?;
        successor.page_spreads = spreads;
        if let Some(updated) = updated.as_mut() {
            updated.parents.page_bindings.retain(|b| b.page_id != page);
            // Node hyperlinks to a spread the delete removed go in the same step (lane
            // SMB-TYPE-CHARACTER-ATTRIBUTES-1, review F14).
            let kept: BTreeSet<&str> = successor.page_spreads.iter().map(|s| s.page_spread_id.as_str()).collect();
            let removed: BTreeSet<&str> = document.page_spreads.iter().map(|s| s.page_spread_id.as_str()).filter(|s| !kept.contains(s)).collect();
            if !removed.is_empty() {
                let unlinked = updated.styles.drop_node_links(&|id: &str| !removed.contains(id));
                cascade.records = cascade.records.saturating_add(u64::try_from(unlinked).unwrap_or(u64::MAX));
            }
            // The page after `page` in document order (sections number pages in that order).
            let order: Vec<&str> = document.page_spreads.iter().flat_map(|s| &s.pages).map(|p| p.page_id.as_str()).collect();
            let next = order.iter().position(|p| *p == page).and_then(|i| i.checked_add(1)).and_then(|i| order.get(i)).map(|p| (*p).to_owned());
            let starts: BTreeSet<String> = updated.longdoc.sections.iter().map(|s| s.page_start.clone()).collect();
            updated.longdoc.sections.retain_mut(|s| {
                if s.page_start != page {
                    return true;
                }
                match &next {
                    Some(n) if !starts.contains(n) => {
                        s.page_start.clone_from(n);
                        true
                    }
                    _ => false,
                }
            });
            // The updated set must validate without the page even when nothing in it changed (a record this
            // update does not follow): a delete never leaves a stale entry.
            updated.validate(&successor, folio::Budget::default(), cancel).map_err(|d| {
                if d.code == folio::Code::Canceled {
                    ("canceled", Value::Null)
                } else {
                    ("page_bound_by_layout_records", json!({"page_id": page, "record": d.target, "folio_code": format!("{:?}", d.code)}))
                }
            })?;
        }
        Ok(PageDelete { document: successor, records: updated, cascade })
    }

    /// One refusal record `op` of `command` with `detail` (`Value::Null` for none); always false.
    fn refuse_with_detail(&mut self, op: &str, command: &'static str, code: &'static str, detail: Value) -> bool {
        self.note_unrecordable(command);
        let before = self.frames.count();
        let _ = self.begin(command, None);
        let ok = self.own_op(op, before, Err(code));
        if let Some(record) = self.ops.last_mut()
            && !detail.is_null()
        {
            record["detail"] = detail;
        }
        ok
    }

    /// Adds `fields` and the cascade's counts to the detail of the newest op record.
    fn add_cascade_detail(&mut self, cascade: &LayoutCascade, fields: &[(&str, Value)]) {
        if let Some(record) = self.ops.last_mut()
            && record["detail"].is_object()
        {
            for (k, v) in fields {
                record["detail"][*k] = v.clone();
            }
            if let Value::Object(counts) = cascade.detail() {
                for (k, v) in counts {
                    record["detail"][k] = v;
                }
            }
        }
    }

    /// The stored layout records, validated: `Ok(None)` when none are stored; `layout_records_unreadable` when
    /// the entry does not decode, `layout_records_stale` when it decodes and does not validate against the
    /// document, `canceled` (as `Run::submit_layout` tells them apart; review F6 of acd73bc7).
    fn readable_layout_records(&self) -> Result<Option<LayoutRecords>, &'static str> {
        let cancel = self.cancel;
        let session = self.session.as_ref().ok_or("not_open")?;
        let code = |e: SessionError, other: &'static str| if matches!(e, SessionError::Canceled) { "canceled" } else { other };
        match session.layout_records_shape(cancel) {
            Err(e) => Err(code(e, "layout_records_unreadable")),
            Ok(None) => Ok(None),
            Ok(Some(_)) => session.layout_records(cancel).map_err(|e| code(e, "layout_records_stale")),
        }
    }

    /// The page delete of `page` with its content (`Run::delete_page_with_content` on the live state), or `None`
    /// when the plain container step applies (no session, an unknown or last page, which that step refuses, or
    /// an empty page whose records need no update: none stored, or a stale or unreadable entry under an empty
    /// page). A refusal is its code and the op record's detail (`Value::Null` for none).
    fn page_delete_plan(&self, page: &str) -> Result<Option<PageDelete>, (&'static str, Value)> {
        let Some(session) = self.session.as_ref() else {
            return Ok(None);
        };
        let document = session.snapshot().document();
        let Some(at) = document.locate_page(page) else {
            return Ok(None);
        };
        if rules::page_count(document) <= 1 {
            return Ok(None);
        }
        let spread = (at.spread.pages.len() == 1).then(|| at.spread.page_spread_id.as_str());
        let content = document.layers.iter().any(|l| l.text_frame.as_ref().is_some_and(|f| f.page_id == page) || spread.is_some_and(|s| matches!(&l.parent, Parent::Node(n) if n.node_id == s)));
        let records = match self.readable_layout_records() {
            Ok(r) => r,
            Err(code) if code != "canceled" && !content => return Ok(None),
            Err(code) => return Err((code, if code == "canceled" { Value::Null } else { json!({"page_id": page}) })),
        };
        let plan = Self::delete_page_with_content(document, records.as_ref(), page, self.cancel)?;
        Ok(if plan.cascade.layers.is_empty() && plan.records == records { None } else { Some(plan) })
    }

    /// One layout step that writes `document` and `records` (`Run::submit_layout`), or, for `None`, the document
    /// alone with the entry left absent (a document without a layout records entry gets none: review F7 of
    /// acd73bc7). Same records, refusals and undo as `Run::submit_layout`.
    fn submit_layout_records(&mut self, op: &str, command: &'static str, document: folio::StudioDocument, records: Option<LayoutRecords>) -> bool {
        if let Some(records) = records {
            return self.submit_layout(op, command, document, records);
        }
        self.note_unrecordable(command);
        let before = self.frames.count();
        let cancel = self.cancel;
        if self.session.is_none() {
            return self.push(op, before, Err("not_open"), None, None);
        }
        let ctx = self.begin(command, None);
        let Some(session) = self.session.as_mut() else {
            return self.push(op, before, Err("not_open"), None, None);
        };
        let step = hsk_studio_session::LayoutStep {
            command_id: command_text(&ctx).to_string(),
            correlation_id: ctx.correlation().value(),
            expect: Box::new(session.snapshot().document().clone()),
            write: Box::new(document),
            records_expect: None,
            records_write: None,
        };
        let revision = session.document_revision();
        match session.submit_layout_as(&ctx, &step, revision, cancel) {
            Ok(applied) => {
                if applied.changed && !applied.replayed {
                    self.emit_business_event(StudioBusinessEventId::LayerMutated, &ctx, applied.revision, &[]);
                }
                if applied.layout_inverse.is_some() {
                    self.undo_stack.push(HistoryStep { label: op.to_string(), inverse: Value::Null, entry: None, timeline: None, group: None, sequences: None, containers: None, layout: applied.layout_inverse.clone(), ctx, revision: applied.revision });
                    self.redo_stack.clear();
                }
                let detail = json!({"provider": applied.provider, "command": applied.command_id, "previous_revision": applied.previous_revision, "revision": applied.revision, "correlation_id": applied.correlation_id, "changed": applied.changed});
                self.push(op, before, Ok(detail), Some(applied.report), Some(applied.revision))
            }
            Err(f) => {
                let revision = self.revision();
                self.push(op, before, Err(f.code()), Some(f.report), revision)
            }
        }
    }

    /// The Viewer's Layers > Delete of layers that layout records follow (lane SMB-LAYOUT-DELETE-CASCADE-1; a
    /// text frame, a graphic frame or a group holding one; row `layer.delete_layout_layers`, driven as
    /// `viewer.delete_layer`): `layers` and every layer under them go through [`cascade_layout_delete`] (a
    /// thread closes over a removed frame) and commit with the updated layout records as ONE layout step under
    /// `command` (`Run::submit_layout`; with `entry`, the session document entry `(token, bytes)` set in the same
    /// step, `Run::submit_layout_and_entry`), labelled `op`; undo restores the layers, the thread and the records
    /// together. A document without a layout records entry gets none (review F7 of acd73bc7). Detail: the step's
    /// and the cascade's counts ([`LayoutCascade::detail`]). Refusals change nothing: `not_open`, `unknown_layer`,
    /// `canceled`, `layout_records_unreadable`, `layout_records_stale` (the stored entry does not decode, or does
    /// not validate now), `layer_bound_by_layout_records`, fail closed, when the updated records would still not
    /// validate (detail `record`, `folio_code`), `layout_records_missing` for an `entry` in a document without a
    /// layout records entry (no step writes that pair), then `submit_layout`'s codes.
    pub fn delete_layout_layers(&mut self, op: &str, command: &'static str, layers: &[String], entry: Option<(&str, Vec<u8>)>) -> bool {
        let cancel = self.cancel;
        let planned = (|| {
            let session = self.session.as_ref().ok_or(("not_open", Value::Null))?;
            let document = session.snapshot().document();
            if let Some(missing) = layers.iter().find(|id| !document.layers.iter().any(|l| l.layer_id == **id)) {
                return Err(("unknown_layer", json!({"layer_id": missing})));
            }
            let records = self.readable_layout_records().map_err(|code| (code, Value::Null))?;
            if records.is_none() && entry.is_some() {
                return Err(("layout_records_missing", Value::Null));
            }
            let mut successor = document.clone();
            let mut updated = records;
            let mut scratch = LayoutRecords::new(&document.document_id);
            let doomed: BTreeSet<String> = layers.iter().cloned().collect();
            let cascade = cascade_layout_delete(&mut successor, updated.as_mut().unwrap_or(&mut scratch), &doomed);
            if let Some(updated) = &updated {
                updated.validate(&successor, folio::Budget::default(), cancel).map_err(|d| {
                    if d.code == folio::Code::Canceled {
                        ("canceled", Value::Null)
                    } else {
                        ("layer_bound_by_layout_records", json!({"record": d.target, "folio_code": format!("{:?}", d.code)}))
                    }
                })?;
            }
            Ok((successor, updated, cascade))
        })();
        let (document, records, cascade) = match planned {
            Ok(p) => p,
            Err((code, detail)) => return self.refuse_with_detail(op, command, code, detail),
        };
        let ok = match (records, entry) {
            (Some(records), Some((token, bytes))) => self.submit_layout_and_entry(op, command, document, records, token, bytes),
            (records, _) => self.submit_layout_records(op, command, document, records),
        };
        if ok {
            self.add_cascade_detail(&cascade, &[]);
        }
        ok
    }

    /// The options of the document's artboards in document order, the spec's defaults for one never
    /// set. `artboard_options_invalid` when the entry does not parse.
    pub fn artboard_options(&self) -> Result<Vec<ArtboardOptions>, &'static str> {
        let session = self.session.as_ref().ok_or("not_open")?;
        let document = session.snapshot().document();
        let stored = match session.document_entry(rules::ARTBOARD_OPTIONS_ENTRY) {
            Some(bytes) => rules::parse_options(bytes, &document.document_id)?,
            None => Vec::new(),
        };
        Ok(document.artboards.iter().map(|a| stored.iter().find(|o| o.artboard_id == a.artboard_id).cloned().unwrap_or_else(|| ArtboardOptions::new(&a.artboard_id))).collect())
    }

    /// `artboard.set_options` ([STU-SHL-026] Artboard Options, [STU-VEC-165]): one entry-only undo step;
    /// the fields `edit` leaves `None` stay. The same options again are no step. Refusals: `not_open`,
    /// `unknown_artboard`, `position_out_of_range`, `ruler_aspect_out_of_range`, `name_too_long`,
    /// `name_invalid`, `artboard_options_invalid` (a stored entry that does not parse is never replaced
    /// by accident). One record `set_artboard_options`.
    pub fn set_artboard_options(&mut self, artboard: &str, edit: &ArtboardOptionsEdit) -> bool {
        const OP: &str = "set_artboard_options";
        const COMMAND: &str = "artboard.set_options";
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
        let document_id = session.snapshot().document().document_id.clone();
        if !session.snapshot().document().artboards.iter().any(|a| a.artboard_id == artboard) {
            return self.own_op(OP, before, Err("unknown_artboard"));
        }
        let previous = session.document_entry(rules::ARTBOARD_OPTIONS_ENTRY).map(<[u8]>::to_vec);
        let written = (|| {
            let mut rows = match &previous {
                Some(bytes) => rules::parse_options(bytes, &document_id)?,
                None => Vec::new(),
            };
            let at = rows.iter().position(|o| o.artboard_id == artboard);
            let current = at.map_or_else(|| ArtboardOptions::new(artboard), |i| rows[i].clone());
            let next = edit.apply(&current)?;
            let changed = next != current;
            match at {
                Some(i) => rows[i] = next.clone(),
                None => rows.push(next.clone()),
            }
            Ok::<_, &'static str>((rules::options_bytes(&rows, &document_id)?, next, changed))
        })();
        let (bytes, next, changed) = match written {
            Ok(done) => done,
            Err(code) => return self.own_op(OP, before, Err(code)),
        };
        if changed {
            if let Err(e) = session.set_document_entry(rules::ARTBOARD_OPTIONS_ENTRY, Some(bytes), cancel) {
                return self.own_op(OP, before, Err(e.code()));
            }
            let revision = session.document_revision();
            let restore = EntryRestore { token: rules::ARTBOARD_OPTIONS_ENTRY.to_string(), bytes: previous };
            self.undo_stack.push(HistoryStep { label: format!("{OP} {artboard}"), inverse: Value::Null, entry: Some(restore), timeline: None, group: None, sequences: None, containers: None, layout: None, ctx, revision });
            self.redo_stack.clear();
        }
        self.own_op(OP, before, Ok(json!({"options": next.to_json(), "changed": changed})))
    }
}
