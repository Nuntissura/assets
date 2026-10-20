//! Pages and parent-pages panel (lane SMB-LAYOUT-UI-2 slice 1; STU-LAY-001/006/007/110): the
//! document's spreads with their pages (insert after a page or as a new spread, delete, move), the
//! parent pages (create, based on another parent, make a frame a parent item) and per-page parent
//! application with item overrides (override = a page-local copy that replaces the inherited item,
//! remove override = restore inheritance, detach = keep the copy and sever the link). The parent
//! state is Folio's `hsk.studio.parent_page_records@1`; every edit is checked by Folio's
//! `validate_parent_page_records` and the page view shows the inherited items that layout's
//! `resolve_page_from_records` resolves (parent chain + overrides), so the panel holds no parent
//! logic of its own.
//!
//! Declared gaps: page sides are not re-paginated (a new page takes the side opposite its
//! reference page on facing documents); `allow_overrides`, `parent_transform` and page-local items
//! beyond overrides have no Folio record; parents are not loaded from another document. InDesign
//! page/parent behaviour is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use super::{LayoutDoc, LayoutEdit, LayoutEditor, LayoutRecords, LayoutUiError, mint, widget};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    Bounds, Code, Diagnostic, NodeKind, NodeRef, PageSide, PageSpreadSchema, Parent, ParentPageSchema, Payload,
    StudioItemOverride, StudioPageParentBinding, StudioPageSpread, StudioParentPage, StudioParentPageItem,
    StudioParentPageRecords, Unresolved, validate_parent_page_records,
};
use hsk_studio_layout::{ItemSource, LayerRects, resolve_page_from_records};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// One pages-panel command (one undo step through [`LayoutEdit::Page`]).
#[derive(Clone, Debug, PartialEq)]
pub enum PageEdit {
    /// A page after `after` (same size, margins and bleed), in its spread or as a new spread after
    /// it; `parent` is applied to the new page.
    InsertPage { after: String, new_spread: bool, parent: Option<String> },
    /// Deletes the page with its text frames; threads close over a deleted frame, a story losing
    /// its last frame is deleted, the page's parent binding goes with it.
    DeletePage { page: String },
    /// Moves the page to `index` of spread `spread` (positions after the page is taken out; a
    /// spread left empty is removed first; `spread == spreads` makes a new last spread).
    MovePage { page: String, spread: usize, index: usize },
    /// A new parent page (Folio parent key minted `P<n>`).
    NewParent { name: String, based_on: Option<String> },
    /// Applies `parent` (`None` = the "None" parent) to every page in `pages`; a changed parent
    /// clears the page's overrides.
    ApplyParent { pages: Vec<String>, parent: Option<String> },
    /// Makes a text frame an item of `parent` (it then shows on pages applying the parent).
    AddParentItem { parent: String, frame: String },
    /// Overrides an inherited item on `page`: a page-local copy (text frames copy their story).
    OverrideItem { page: String, item: String },
    /// Restores inheritance: the override and its page-local copy are removed.
    RemoveOverride { page: String, item: String },
    /// Severs the link: the copy stays as an ordinary page item, the inherited item stays hidden.
    DetachItem { page: String, item: String },
}

/// One row of the pages panel.
#[derive(Clone, Debug, PartialEq)]
pub struct PageRow {
    pub spread: usize,
    pub index: usize,
    /// 1-based page position in the document.
    pub ordinal: usize,
    pub page_id: String,
    pub parent: Option<String>,
    pub parent_name: Option<String>,
    pub overrides: usize,
    pub side: PageSide,
}

/// Draft fields of the pages panel.
#[derive(Clone, Debug, Default)]
pub(crate) struct PagesUi {
    pub new_parent_name: String,
    pub new_parent_based_on: Option<String>,
    pub apply_parent: Option<String>,
}

/// Layer ids that are parent items (drawn as inherited overlays, not as page frames).
pub(crate) fn parent_item_layers(r: &StudioParentPageRecords) -> BTreeSet<&str> {
    r.parents.iter().flat_map(|p| p.parent_page_items.iter().map(|i| i.layer_id.as_str())).collect()
}

fn folio_code(d: &Diagnostic) -> &'static str {
    match d.code {
        Code::DanglingReference => "folio_dangling_reference",
        Code::DuplicateIdentity => "folio_duplicate_identity",
        Code::RenderCycle => "folio_render_cycle",
        Code::InvalidField => "folio_invalid_field",
        Code::InvalidId => "folio_invalid_id",
        Code::BudgetExceeded => "folio_budget_exceeded",
        Code::Canceled => "folio_canceled",
        _ => "folio_invalid",
    }
}

fn validate(d: &LayoutDoc, r: &LayoutRecords) -> Result<(), LayoutUiError> {
    validate_parent_page_records(&r.parents, &d.document, hsk_studio_folio::Budget::default(), &CancellationToken::default(), &Unresolved)
        .map(|_| ())
        .map_err(|e| LayoutUiError::Folio(folio_code(&e), e.target))
}

/// Spread bounds follow their pages (width = sum, height = tallest).
fn refit_spreads(d: &mut LayoutDoc) {
    for s in &mut d.document.page_spreads {
        let w: f64 = s.pages.iter().map(|p| p.width.value).sum();
        let h = s.pages.iter().map(|p| p.height.value).fold(0.0, f64::max);
        s.bounds.width.value = w;
        s.bounds.height.value = h;
    }
}

fn binding_mut<'a>(r: &'a mut StudioParentPageRecords, page: &str) -> &'a mut StudioPageParentBinding {
    if let Some(i) = r.page_bindings.iter().position(|b| b.page_id == page) {
        &mut r.page_bindings[i]
    } else {
        r.page_bindings.push(StudioPageParentBinding { page_id: page.to_owned(), applied_parent: None, override_list: Vec::new() });
        let last = r.page_bindings.len().saturating_sub(1);
        &mut r.page_bindings[last]
    }
}

/// The parent item `item` inherited by `page` (applied parent chain): its layer id.
fn inherited_layer(r: &StudioParentPageRecords, page: &str, item: &str) -> Result<String, LayoutUiError> {
    let mut at = r.binding(page).and_then(|b| b.applied_parent.clone());
    let mut steps = 0usize;
    while let Some(pid) = at {
        steps = steps.saturating_add(1);
        if steps > r.parents.len() {
            break;
        }
        let p = r.parent(&pid).ok_or_else(|| LayoutUiError::UnknownParent(pid.clone()))?;
        if let Some(i) = p.parent_page_items.iter().find(|i| i.item_id == item) {
            return Ok(i.layer_id.clone());
        }
        at = p.based_on.clone();
    }
    Err(LayoutUiError::UnknownParentItem(item.to_owned()))
}

fn mint_spread(d: &LayoutDoc) -> Result<String, LayoutUiError> {
    for n in 1u64..=1_000_000 {
        let id = format!("SPGS-019a5a10-0000-7000-8000-{n:012x}");
        if !d.document.page_spreads.iter().any(|s| s.page_spread_id == id) {
            return Ok(id);
        }
    }
    Err(LayoutUiError::IdSpaceExhausted)
}

fn mint_page(d: &LayoutDoc) -> Result<String, LayoutUiError> {
    for n in 1u64..=1_000_000 {
        let id = format!("page-{n}");
        if d.document.locate_page(&id).is_none() {
            return Ok(id);
        }
    }
    Err(LayoutUiError::IdSpaceExhausted)
}

fn delete_layers(d: &mut LayoutDoc, doomed: &BTreeSet<String>) {
    // Close each chain over deleted frames: previous -> next of the deleted run.
    let links: Vec<(String, Option<String>, Option<String>)> = d
        .document
        .layers
        .iter()
        .filter(|l| doomed.contains(&l.layer_id))
        .filter_map(|l| l.text_frame.as_ref().map(|f| (l.layer_id.clone(), f.previous_text_frame.clone(), f.next_text_frame.clone())))
        .collect();
    let follow = |mut at: Option<String>, forward: bool| -> Option<String> {
        let mut steps = 0usize;
        while let Some(f) = at.clone() {
            if !doomed.contains(&f) {
                return Some(f);
            }
            steps = steps.saturating_add(1);
            if steps > links.len() {
                return None;
            }
            at = links.iter().find(|(id, ..)| *id == f).and_then(|(_, p, n)| if forward { n.clone() } else { p.clone() });
        }
        None
    };
    let mut relink = Vec::new();
    for (_, prev, next) in &links {
        let (p, n) = (follow(prev.clone(), false), follow(next.clone(), true));
        relink.push((p, n));
    }
    let stories: BTreeSet<String> = d
        .document
        .layers
        .iter()
        .filter(|l| doomed.contains(&l.layer_id))
        .filter_map(|l| match &l.payload {
            Payload::Text { story_id } => Some(story_id.clone()),
            _ => None,
        })
        .collect();
    d.document.layers.retain(|l| !doomed.contains(&l.layer_id));
    for (p, n) in relink {
        for l in &mut d.document.layers {
            if let Some(f) = l.text_frame.as_mut() {
                if Some(&l.layer_id) == p.as_ref() {
                    f.next_text_frame = n.clone();
                }
                if Some(&l.layer_id) == n.as_ref() {
                    f.previous_text_frame = p.clone();
                }
            }
        }
    }
    for s in stories {
        let shown = d.document.layers.iter().any(|l| matches!(&l.payload, Payload::Text { story_id } if *story_id == s));
        if !shown {
            d.stories.retain(|st| st.story_id != s);
        }
    }
}

/// The state after one pages-panel edit (pure; validated by Folio before it is accepted).
pub(crate) fn transform(doc: &LayoutDoc, rec: &LayoutRecords, edit: &PageEdit) -> Result<(LayoutDoc, LayoutRecords), LayoutUiError> {
    let mut d = doc.clone();
    let mut r = rec.clone();
    match edit {
        PageEdit::InsertPage { after, new_spread, parent } => {
            let loc = d.document.locate_page(after).ok_or_else(|| LayoutUiError::UnknownPage(after.clone()))?;
            let (si, pi) = (loc.spread_index, loc.page_index);
            let mut page = loc.page.clone();
            page.page_id = mint_page(&d)?;
            page.side = match page.side {
                PageSide::RightHand => PageSide::LeftHand,
                PageSide::LeftHand => PageSide::RightHand,
                PageSide::SingleSided => PageSide::SingleSided,
            };
            let new_id = page.page_id.clone();
            if *new_spread {
                let spread_id = mint_spread(&d)?;
                let template = &d.document.page_spreads[si];
                let spread = StudioPageSpread {
                    schema_id: PageSpreadSchema::V1,
                    page_spread_id: spread_id,
                    name: "spread".into(),
                    parent: template.parent.clone(),
                    bounds: Bounds { x: template.bounds.x.clone(), y: template.bounds.y.clone(), width: page.width.clone(), height: page.height.clone() },
                    pages: vec![page],
                };
                d.document.page_spreads.insert(si.saturating_add(1), spread);
            } else {
                d.document.page_spreads[si].pages.insert(pi.saturating_add(1), page);
            }
            refit_spreads(&mut d);
            if let Some(p) = parent {
                if r.parents.parent(p).is_none() {
                    return Err(LayoutUiError::UnknownParent(p.clone()));
                }
                binding_mut(&mut r.parents, &new_id).applied_parent = Some(p.clone());
            }
        }
        PageEdit::DeletePage { page } => {
            let loc = d.document.locate_page(page).ok_or_else(|| LayoutUiError::UnknownPage(page.clone()))?;
            let total: usize = d.document.page_spreads.iter().map(|s| s.pages.len()).sum();
            if total <= 1 {
                return Err(LayoutUiError::LastPage);
            }
            let (si, pi) = (loc.spread_index, loc.page_index);
            let doomed: BTreeSet<String> = d
                .document
                .layers
                .iter()
                .filter(|l| l.text_frame.as_ref().is_some_and(|f| f.page_id == *page))
                .map(|l| l.layer_id.clone())
                .collect();
            delete_layers(&mut d, &doomed);
            d.document.page_spreads[si].pages.remove(pi);
            if d.document.page_spreads[si].pages.is_empty() {
                let gone = d.document.page_spreads.remove(si).page_spread_id;
                if d.document.layers.iter().any(|l| matches!(&l.parent, Parent::Node(n) if n.node_id == gone)) {
                    return Err(LayoutUiError::Folio("folio_dangling_reference", gone));
                }
            }
            refit_spreads(&mut d);
            r.parents.page_bindings.retain(|b| b.page_id != *page);
        }
        PageEdit::MovePage { page, spread, index } => {
            let loc = d.document.locate_page(page).ok_or_else(|| LayoutUiError::UnknownPage(page.clone()))?;
            let (si, pi) = (loc.spread_index, loc.page_index);
            let moved = d.document.page_spreads[si].pages.remove(pi);
            let mut removed_spread = None;
            if d.document.page_spreads[si].pages.is_empty() {
                removed_spread = Some(d.document.page_spreads.remove(si));
            }
            let count = d.document.page_spreads.len();
            if *spread == count {
                let template = removed_spread.clone().unwrap_or_else(|| d.document.page_spreads[count.saturating_sub(1)].clone());
                let mut s = template;
                s.pages = vec![moved];
                if removed_spread.is_none() {
                    s.page_spread_id = mint_spread(&d)?;
                }
                d.document.page_spreads.push(s);
            } else {
                let target = d.document.page_spreads.get_mut(*spread).ok_or(LayoutUiError::InvalidGeometry)?;
                if *index > target.pages.len() {
                    return Err(LayoutUiError::InvalidGeometry);
                }
                target.pages.insert(*index, moved);
            }
            // Frames on the moved page now sit in another spread node.
            let new_spread_id = d.document.locate_page(page).map(|l| l.spread.page_spread_id.clone()).ok_or(LayoutUiError::InvalidGeometry)?;
            if let Some(gone) = &removed_spread
                && gone.page_spread_id != new_spread_id
                && d.document.layers.iter().any(|l| {
                    matches!(&l.parent, Parent::Node(n) if n.node_id == gone.page_spread_id)
                        && l.text_frame.as_ref().is_none_or(|f| f.page_id != *page)
                })
            {
                return Err(LayoutUiError::Folio("folio_dangling_reference", gone.page_spread_id.clone()));
            }
            for l in &mut d.document.layers {
                if l.text_frame.as_ref().is_some_and(|f| f.page_id == *page) {
                    l.parent = Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: new_spread_id.clone() });
                }
            }
            refit_spreads(&mut d);
        }
        PageEdit::NewParent { name, based_on } => {
            if name.trim().is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
                return Err(LayoutUiError::InvalidName);
            }
            if let Some(b) = based_on
                && r.parents.parent(b).is_none()
            {
                return Err(LayoutUiError::UnknownParent(b.clone()));
            }
            let mut key = None;
            for n in 1u64..=(hsk_studio_folio::MAX_PARENT_PAGES as u64) {
                let k = format!("P{n}");
                if r.parents.parent(&k).is_none() {
                    key = Some(k);
                    break;
                }
            }
            let parent_id = key.ok_or(LayoutUiError::IdSpaceExhausted)?;
            r.parents.parents.push(StudioParentPage {
                schema_id: ParentPageSchema::V1,
                parent_id,
                name: name.clone(),
                based_on: based_on.clone(),
                parent_page_items: Vec::new(),
            });
        }
        PageEdit::ApplyParent { pages, parent } => {
            if let Some(p) = parent
                && r.parents.parent(p).is_none()
            {
                return Err(LayoutUiError::UnknownParent(p.clone()));
            }
            for page in pages {
                if d.document.locate_page(page).is_none() {
                    return Err(LayoutUiError::UnknownPage(page.clone()));
                }
                let b = binding_mut(&mut r.parents, page);
                if b.applied_parent != *parent {
                    b.applied_parent = parent.clone();
                    b.override_list.clear();
                }
            }
            r.parents.page_bindings.retain(|b| b.applied_parent.is_some() || !b.override_list.is_empty());
        }
        PageEdit::AddParentItem { parent, frame } => {
            if !d.document.layers.iter().any(|l| l.layer_id == *frame && l.text_frame.is_some()) {
                return Err(LayoutUiError::UnknownFrame(frame.clone()));
            }
            let p = r
                .parents
                .parents
                .iter_mut()
                .find(|p| p.parent_id == *parent)
                .ok_or_else(|| LayoutUiError::UnknownParent(parent.clone()))?;
            let mut item_id = None;
            for n in 1u64..=(hsk_studio_folio::MAX_PARENT_PAGE_ITEMS as u64) {
                let k = format!("item-{n}");
                if !p.parent_page_items.iter().any(|i| i.item_id == k) {
                    item_id = Some(k);
                    break;
                }
            }
            let item_id = item_id.ok_or(LayoutUiError::IdSpaceExhausted)?;
            p.parent_page_items.push(StudioParentPageItem { item_id, layer_id: frame.clone() });
        }
        PageEdit::OverrideItem { page, item } => {
            if d.document.locate_page(page).is_none() {
                return Err(LayoutUiError::UnknownPage(page.clone()));
            }
            let layer_id = inherited_layer(&r.parents, page, item)?;
            if r.parents.binding(page).is_some_and(|b| b.override_list.iter().any(|o| o.parent_item_id() == item)) {
                return Err(LayoutUiError::AlreadyOverridden(item.clone()));
            }
            let source = d.document.layers.iter().find(|l| l.layer_id == layer_id).cloned().ok_or_else(|| LayoutUiError::UnknownFrame(layer_id.clone()))?;
            let spread_id = d.document.locate_page(page).map(|l| l.spread.page_spread_id.clone()).ok_or(LayoutUiError::InvalidGeometry)?;
            let mut copy = source.clone();
            copy.layer_id = mint(&d, "SLYR")?;
            copy.parent = Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: spread_id });
            if let Some(f) = copy.text_frame.as_mut() {
                f.page_id = page.clone();
                f.previous_text_frame = None;
                f.next_text_frame = None;
            }
            if let Payload::Text { story_id } = &source.payload {
                let mut st = d.stories.iter().find(|s| s.story_id == *story_id).cloned().ok_or_else(|| LayoutUiError::UnknownStory(story_id.clone()))?;
                st.story_id = mint(&d, "STXT")?;
                copy.payload = Payload::Text { story_id: st.story_id.clone() };
                d.stories.push(st);
            }
            let replacement = copy.layer_id.clone();
            d.document.layers.push(copy);
            binding_mut(&mut r.parents, page)
                .override_list
                .push(StudioItemOverride::Replace { parent_item_id: item.clone(), replacement_layer_id: replacement });
        }
        PageEdit::RemoveOverride { page, item } | PageEdit::DetachItem { page, item } => {
            let detach = matches!(edit, PageEdit::DetachItem { .. });
            let b = r
                .parents
                .page_bindings
                .iter_mut()
                .find(|b| b.page_id == *page)
                .ok_or_else(|| LayoutUiError::ItemNotOverridden(item.clone()))?;
            let at = b.override_list.iter().position(|o| o.parent_item_id() == item).ok_or_else(|| LayoutUiError::ItemNotOverridden(item.clone()))?;
            let removed = b.override_list.remove(at);
            match (&removed, detach) {
                (StudioItemOverride::Replace { replacement_layer_id, .. }, false) => {
                    let doomed = BTreeSet::from([replacement_layer_id.clone()]);
                    delete_layers(&mut d, &doomed);
                }
                (StudioItemOverride::Replace { .. }, true) => {
                    // The copy stays a plain page item; the inherited item stays hidden on this page.
                    b.override_list.insert(at, StudioItemOverride::Suppress { parent_item_id: item.clone() });
                }
                (StudioItemOverride::Suppress { .. }, false) => {}
                (StudioItemOverride::Suppress { .. }, true) => return Err(LayoutUiError::ItemNotOverridden(item.clone())),
            }
        }
    }
    validate(&d, &r)?;
    Ok((d, r))
}

/// Page-coordinate rectangle of an overlay.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OverlayRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl LayoutEditor {
    /// Pages in document order.
    pub fn page_rows(&self) -> Vec<PageRow> {
        let mut out = Vec::new();
        let mut ordinal = 0usize;
        for (si, s) in self.doc.document.page_spreads.iter().enumerate() {
            for (pi, p) in s.pages.iter().enumerate() {
                ordinal = ordinal.saturating_add(1);
                let b = self.rec.parents.binding(&p.page_id);
                let parent = b.and_then(|b| b.applied_parent.clone());
                out.push(PageRow {
                    spread: si,
                    index: pi,
                    ordinal,
                    page_id: p.page_id.clone(),
                    parent_name: parent.as_deref().and_then(|k| self.rec.parents.parent(k)).map(|p| p.name.clone()),
                    parent,
                    overrides: b.map_or(0, |b| b.override_list.len()),
                    side: p.side,
                });
            }
        }
        out
    }

    /// Inherited parent items per page, through layout's resolver (overridden copies are page frames).
    pub(crate) fn parent_overlays(&self) -> Vec<(String, Vec<OverlayRect>)> {
        let rects = LayerRects::new();
        self.page_rows()
            .into_iter()
            .filter(|r| r.parent.is_some())
            .filter_map(|r| {
                let items = resolve_page_from_records(&self.rec.parents, &self.doc.document, &rects, &r.page_id).ok()?;
                let v = items
                    .into_iter()
                    .filter(|i| matches!(i.source, ItemSource::Parent { .. }))
                    .map(|i| OverlayRect { x: i.item.rect.x0_pt, y: i.item.rect.y0_pt, w: i.item.rect.x1_pt - i.item.rect.x0_pt, h: i.item.rect.y1_pt - i.item.rect.y0_pt })
                    .collect();
                Some((r.page_id, v))
            })
            .collect()
    }

    /// Items shown on a page: `(item id, source)` with source `parent:<key>`, `override:<key>`, or
    /// the adapter's refusal code.
    pub fn page_items(&self, page: &str) -> Result<Vec<(String, String)>, LayoutUiError> {
        let items = resolve_page_from_records(&self.rec.parents, &self.doc.document, &LayerRects::new(), page).map_err(|e| LayoutUiError::Parent(e.code()))?;
        Ok(items
            .into_iter()
            .map(|i| {
                let src = match i.source {
                    ItemSource::Parent { parent_id } => format!("parent:{parent_id}"),
                    ItemSource::Override { parent_id } => format!("override:{parent_id}"),
                    ItemSource::Local => "local".to_owned(),
                };
                (i.item.id, src)
            })
            .collect())
    }

    pub(crate) fn pages_json(&self) -> Value {
        Value::Array(
            self.page_rows()
                .into_iter()
                .map(|r| {
                    json!({
                        "page_id": r.page_id, "spread": r.spread, "index": r.index, "ordinal": r.ordinal,
                        "parent": r.parent, "overrides": r.overrides,
                        "items": self.page_items(&r.page_id).map(|v| v.into_iter().map(|(i, s)| json!([i, s])).collect::<Vec<_>>()).map_err(|e| e.code()).ok(),
                    })
                })
                .collect(),
        )
    }

    pub(crate) fn parents_json(&self) -> Value {
        Value::Array(
            self.rec
                .parents
                .parents
                .iter()
                .map(|p| json!({"parent_id": p.parent_id, "name": p.name, "based_on": p.based_on, "items": p.parent_page_items.len()}))
                .collect(),
        )
    }

    /// The pages panel: spreads with their pages, page commands, parents and overrides.
    pub(crate) fn pages_panel(&mut self, ui: &mut egui::Ui, drawn: &mut Vec<&'static str>) {
        let rows = self.page_rows();
        ui.push_id(widget::PAGES_PANEL, |ui| {
            ui.label("Pages");
            let mut spread = usize::MAX;
            ui.horizontal_wrapped(|ui| {
                for r in &rows {
                    if r.spread != spread {
                        spread = r.spread;
                        ui.separator();
                    }
                    let prefix = r.parent.clone().unwrap_or_else(|| "-".into());
                    let label = format!("{} [{}]{}", r.ordinal, prefix, if r.overrides > 0 { "*" } else { "" });
                    if ui.selectable_label(self.selected_page.as_deref() == Some(r.page_id.as_str()), label).clicked() {
                        self.selected_page = Some(r.page_id.clone());
                    }
                }
            });
            let sel = self.selected_page.clone();
            ui.horizontal(|ui| {
                let enabled = sel.is_some();
                let pos = sel.as_ref().and_then(|p| rows.iter().position(|r| r.page_id == *p));
                if ui.push_id(widget::PAGE_INSERT, |ui| ui.add_enabled(enabled, egui::Button::new("Insert page"))).inner.clicked()
                    && let Some(p) = &sel
                {
                    let parent = self.rec.parents.binding(p).and_then(|b| b.applied_parent.clone());
                    self.act(LayoutEdit::Page(PageEdit::InsertPage { after: p.clone(), new_spread: false, parent }));
                }
                if ui.push_id(widget::PAGE_INSERT_SPREAD, |ui| ui.add_enabled(enabled, egui::Button::new("Insert spread"))).inner.clicked()
                    && let Some(p) = &sel
                {
                    self.act(LayoutEdit::Page(PageEdit::InsertPage { after: p.clone(), new_spread: true, parent: None }));
                }
                if ui.push_id(widget::PAGE_DELETE, |ui| ui.add_enabled(enabled, egui::Button::new("Delete page"))).inner.clicked()
                    && let Some(p) = &sel
                {
                    self.act(LayoutEdit::Page(PageEdit::DeletePage { page: p.clone() }));
                }
                if ui.push_id(widget::PAGE_MOVE_UP, |ui| ui.add_enabled(pos.is_some_and(|i| i > 0), egui::Button::new("Move up"))).inner.clicked()
                    && let (Some(p), Some(i)) = (&sel, pos)
                    && let Some(prev) = i.checked_sub(1).and_then(|k| rows.get(k))
                {
                    self.act(LayoutEdit::Page(PageEdit::MovePage { page: p.clone(), spread: prev.spread, index: prev.index }));
                }
                if ui.push_id(widget::PAGE_MOVE_DOWN, |ui| ui.add_enabled(pos.is_some_and(|i| i.saturating_add(1) < rows.len()), egui::Button::new("Move down"))).inner.clicked()
                    && let (Some(p), Some(i)) = (&sel, pos)
                    && let Some(next) = rows.get(i.saturating_add(1))
                {
                    // Positions are taken after the page is removed: same spread shifts by one.
                    let me = &rows[i];
                    let (spread, index) = if next.spread == me.spread { (next.spread, next.index) } else { (next.spread, next.index.saturating_add(1)) };
                    let spread = if next.spread != me.spread && rows.iter().filter(|r| r.spread == me.spread).count() == 1 { spread.saturating_sub(1) } else { spread };
                    self.act(LayoutEdit::Page(PageEdit::MovePage { page: p.clone(), spread, index }));
                }
            });
            drawn.extend([widget::PAGES_PANEL, widget::PAGE_INSERT, widget::PAGE_INSERT_SPREAD, widget::PAGE_DELETE, widget::PAGE_MOVE_UP, widget::PAGE_MOVE_DOWN]);
            ui.label("Parent pages");
            let parents: Vec<(String, String)> = self.rec.parents.parents.iter().map(|p| (p.parent_id.clone(), p.name.clone())).collect();
            ui.push_id(widget::PARENT_LIST, |ui| {
                for (k, n) in &parents {
                    ui.horizontal(|ui| {
                        ui.label(format!("{k} {n}"));
                        if let Some(frame) = self.selected.clone()
                            && ui.button("Add selected frame").clicked()
                        {
                            self.act(LayoutEdit::Page(PageEdit::AddParentItem { parent: k.clone(), frame }));
                        }
                    });
                }
            });
            ui.horizontal(|ui| {
                ui.push_id(widget::PARENT_NEW, |ui| {
                    ui.text_edit_singleline(&mut self.pages_ui.new_parent_name);
                    egui::ComboBox::from_id_salt("based_on")
                        .selected_text(self.pages_ui.new_parent_based_on.clone().unwrap_or_else(|| "[based on none]".into()))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.pages_ui.new_parent_based_on, None, "[based on none]");
                            for (k, n) in &parents {
                                ui.selectable_value(&mut self.pages_ui.new_parent_based_on, Some(k.clone()), n);
                            }
                        });
                    if ui.button("New parent").clicked() {
                        let (name, based_on) = (self.pages_ui.new_parent_name.clone(), self.pages_ui.new_parent_based_on.clone());
                        self.act(LayoutEdit::Page(PageEdit::NewParent { name, based_on }));
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.push_id(widget::PARENT_APPLY, |ui| {
                    egui::ComboBox::from_id_salt("apply")
                        .selected_text(self.pages_ui.apply_parent.clone().unwrap_or_else(|| "[None]".into()))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.pages_ui.apply_parent, None, "[None]");
                            for (k, n) in &parents {
                                ui.selectable_value(&mut self.pages_ui.apply_parent, Some(k.clone()), n);
                            }
                        });
                    if ui.add_enabled(sel.is_some(), egui::Button::new("Apply to page")).clicked()
                        && let Some(p) = &sel
                    {
                        self.act(LayoutEdit::Page(PageEdit::ApplyParent { pages: vec![p.clone()], parent: self.pages_ui.apply_parent.clone() }));
                    }
                    if ui.button("Apply to all pages").clicked() {
                        let pages = rows.iter().map(|r| r.page_id.clone()).collect();
                        self.act(LayoutEdit::Page(PageEdit::ApplyParent { pages, parent: self.pages_ui.apply_parent.clone() }));
                    }
                });
            });
            drawn.extend([widget::PARENT_LIST, widget::PARENT_NEW, widget::PARENT_APPLY]);
            if let Some(p) = sel {
                ui.push_id(widget::PARENT_ITEMS, |ui| {
                    let items = self.page_items(&p);
                    match items {
                        Ok(items) => {
                            for (item, src) in items {
                                ui.horizontal(|ui| {
                                    ui.label(format!("{item} ({src})"));
                                    if src.starts_with("parent:") {
                                        if ui.button("Override").clicked() {
                                            self.act(LayoutEdit::Page(PageEdit::OverrideItem { page: p.clone(), item: item.clone() }));
                                        }
                                    } else if src.starts_with("override:") {
                                        if ui.button("Remove override").clicked() {
                                            self.act(LayoutEdit::Page(PageEdit::RemoveOverride { page: p.clone(), item: item.clone() }));
                                        }
                                        if ui.button("Detach").clicked() {
                                            self.act(LayoutEdit::Page(PageEdit::DetachItem { page: p.clone(), item: item.clone() }));
                                        }
                                    }
                                });
                            }
                        }
                        Err(e) => {
                            ui.colored_label(egui::Color32::RED, e.code());
                        }
                    }
                });
                drawn.push(widget::PARENT_ITEMS);
            }
        });
    }
}
