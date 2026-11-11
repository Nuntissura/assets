//! `document.new` and the artboard and page commands of [`Run`] (lane SMB-DOC-NEW-1; [STU-SHL-023],
//! [STU-SHL-026], [STU-VEC-028], [STU-VEC-165]; rules in [`crate::document_new`]). Each command is one
//! op record and one Observe frame of codes and counters, carries the run's attribution, and each
//! edit is one named undo step: a [`ContainerStep`] (one revision) or, for the options, an entry-only
//! step (no revision, like every document entry). Not recordable: a recording reports them typed
//! ([STU-AUT-109]).
//!
//! What a delete refuses: a container a layer still names (`artboard_not_empty`, `page_not_empty`), the
//! last page of a document (`last_page`) and the last artboard of a document with layers
//! (`last_artboard`), so a frame for the layers stays; a page delete updates the layout records that name
//! the page in the same step, or refuses `page_bound_by_layout_records` ([`Run::delete_page`]). A page has
//! no name of its own in Folio
//! (`StudioPage`): `page.rename` names the page's spread.
#![deny(clippy::arithmetic_side_effects)]

use super::{EntryRestore, HistoryStep, Run, StudioBusinessEventId, command_text};
use crate::document_new::{self as rules, ArtboardOptions, ArtboardOptionsEdit, NewDocument};
use hsk_studio_accord::CommandContext;
use hsk_studio_folio::{self as folio, Bounds, GeometryUnit, Length, PageSide, Parent, StudioArtboard, StudioPage, StudioPageSpread};
use hsk_studio_session::{ContainerStep, Session, SessionConfig};
use serde_json::{Value, json};
use std::collections::BTreeSet;
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

/// The highest `page-N` named anywhere in `bytes` (a bounded scan of a document entry body).
fn highest_page_number(bytes: &[u8], mut highest: u64) -> u64 {
    const KEY: &[u8] = b"page-";
    let mut at = 0usize;
    while let Some(found) = bytes.get(at..).and_then(|rest| rest.windows(KEY.len()).position(|w| w == KEY)) {
        let start = at.saturating_add(found).saturating_add(KEY.len());
        let digits = bytes.get(start..).map_or(0, |rest| rest.iter().take(20).take_while(|b| b.is_ascii_digit()).count());
        if let Some(n) = bytes.get(start..start.saturating_add(digits)).and_then(|d| std::str::from_utf8(d).ok()).and_then(|d| d.parse::<u64>().ok()) {
            highest = highest.max(n);
        }
        at = start;
    }
    highest
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

    /// The first page number past every `page-N` the document, a document entry or the history
    /// names, so a new page never takes the key of a deleted one that layout records or an undo may
    /// still name (review F1). `None` when that number would pass `u64::MAX` (lane SMB-R60-FOLLOWUPS-1
    /// item 4: a document entry may name `page-18446744073709551615`).
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
        let mut highest = pages.fold(0, |n, p| highest_page_number(p.page_id.as_bytes(), n));
        for token in hsk_studio_session::DOCUMENT_ENTRY_TOKENS {
            if let Some(bytes) = session.document_entry(token) {
                highest = highest_page_number(bytes, highest);
            }
        }
        highest.checked_add(1)
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

    /// `page.delete` ([STU-SHL-026] Delete Pages): a spread left with no page goes too. Refusals:
    /// `unknown_page`, `last_page`, `page_not_empty` (module docs).
    ///
    /// Layout records (lane SMB-R60-FOLLOWUPS-1 item 6, lead ruling 2026-11-08): when the document holds
    /// layout records that validate and name the page, the delete and the records updated for it commit
    /// as ONE layout step (`Run::submit_layout`, one revision, one undo step labelled `delete_page <page>`),
    /// so the entry still validates after save and reopen and undo restores the page and its records
    /// together. The update: the page's parent-page binding (applied parent and overrides) is dropped; a
    /// section that starts on the page starts on the next page instead, or is dropped when no page of its
    /// own is left (the next page starts another section, or the page was the last). Refused typed
    /// `page_bound_by_layout_records`, changing nothing, where a record cannot follow: a graphic frame
    /// placed on the page (its layer stays on the spread with no page to sit on), and any record the
    /// updated set still would not validate against the document without the page (a record this update does
    /// not follow): a delete never leaves a stale entry. Records that need no
    /// update and validate without the page, none stored, or an entry that is already stale or unreadable
    /// keep the plain container step (`submit_layout` refuses a stale entry; the next layout edit reports it).
    pub fn delete_page(&mut self, page: &str) -> bool {
        const OP: &str = "delete_page";
        const COMMAND: &str = "page.delete";
        match self.layout_records_without_page(page) {
            Ok(None) => self.container_edit(OP, COMMAND, |document, _, spreads| delete_page_edit(document, spreads, page)),
            Ok(Some((document, records))) => {
                let depth = self.undo_stack.len();
                if !self.submit_layout(OP, COMMAND, document, records) {
                    return false;
                }
                if self.undo_stack.len() > depth
                    && let Some(step) = self.undo_stack.last_mut()
                {
                    step.label = format!("{OP} {page}");
                }
                if let Some(record) = self.ops.last_mut()
                    && record["detail"].is_object()
                {
                    record["detail"]["page_id"] = json!(page);
                    record["detail"]["layout_records"] = json!("updated");
                }
                true
            }
            Err(code) => {
                self.note_unrecordable(COMMAND);
                let before = self.frames.count();
                let _ = self.begin(COMMAND, None);
                self.own_op(OP, before, Err(code))
            }
        }
    }

    /// The document without `page` and the stored layout records updated for it (`delete_page` docs), or
    /// `None` when the plain container step applies (no session, no records, records that do not validate
    /// now, records that need no update and still validate without the page, or a delete the container step
    /// refuses itself).
    fn layout_records_without_page(&self, page: &str) -> Result<Option<(folio::StudioDocument, hsk_studio_session::LayoutRecords)>, &'static str> {
        let cancel = self.cancel;
        let Some(session) = self.session.as_ref() else {
            return Ok(None);
        };
        let Ok(Some(records)) = session.layout_records(cancel) else {
            return Ok(None);
        };
        let document = session.snapshot().document();
        let mut spreads = document.page_spreads.clone();
        if delete_page_edit(document, &mut spreads, page).is_err() {
            return Ok(None);
        }
        let mut successor = document.clone();
        successor.page_spreads = spreads;
        let mut updated = records.clone();
        if updated.longdoc.graphic_frames.iter().any(|g| g.page_id == page) {
            return Err("page_bound_by_layout_records");
        }
        updated.parents.page_bindings.retain(|b| b.page_id != page);
        // The page after `page` in document order (sections number pages in that order).
        let order: Vec<&str> = document.page_spreads.iter().flat_map(|s| &s.pages).map(|p| p.page_id.as_str()).collect();
        let next = order.iter().position(|p| *p == page).and_then(|i| i.checked_add(1)).and_then(|i| order.get(i)).map(|p| (*p).to_owned());
        let starts: BTreeSet<String> = records.longdoc.sections.iter().map(|s| s.page_start.clone()).collect();
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
        updated.validate(&successor, folio::Budget::default(), cancel).map_err(|d| if d.code == folio::Code::Canceled { "canceled" } else { "page_bound_by_layout_records" })?;
        Ok(if updated == records { None } else { Some((successor, updated)) })
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
