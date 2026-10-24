//! Adjust layout (lane SMB-ID-LIQUID-LAYOUT-1; STU-LAY-010, STU-LAY-112): a discrete, undoable
//! command, distinct from the continuous liquid rules, that changes page size, margins or bleed and
//! recomputes object positions and sizes.
//!
//! - Property bag: exactly the ten keys `width`, `height`, `bleedInside`, `bleedTop`,
//!   `bleedOutside`, `bleedBottom`, `leftMargin`, `topMargin`, `rightMargin`, `bottomMargin`; a
//!   partial bag changes only the named values; an unknown or repeated key is refused. A value is a
//!   number in `pt` or a measurement string with any STU-LAY-101 suffix, parsed by
//!   [`crate::parse_measure`] (a bare number in a string is `pt`; `%` is a percentage of the current
//!   value of that key; `px`, `vw`, `vh`, `vmin`, `vmax` resolve against the caller's
//!   [`MeasureContext`]); an unknown suffix is `unit_suffix_unknown`.
//! - Scope: the document, a page collection or one page. Scoped to pages (collection or single page)
//!   the BLEED keys have no effect (STU-LAY-112 exception) and are listed in `ignored_keys`.
//! - Geometry per axis (declared reading, InDesign UNVERIFIED D5): an object edge on a page edge
//!   stays on that page edge, an edge on the bleed edge follows the new bleed edge, any other edge
//!   maps linearly from the old margin box to the new margin box (positions and sizes scale with
//!   the margin box). `move_ruler_guides` moves the page's guides by the same map. Locked objects
//!   (Folio stores no object lock, so the caller names them) and locked guides move only with
//!   `include_locked`.
//! - Font size (`adjust_font_size`): type sizes belong to Type, so layout returns the factor
//!   `min(margin-box x ratio, y ratio)` per page with the caller's minimum and maximum; the bounds,
//!   unit and default are a declared SPEC GAP in STU-LAY-010, so both bounds are required
//!   (`font_size_bounds_missing`) and nothing defaults.
//! - Undo: the proposal carries [`AdjustUndo`]; [`undo_adjust`] restores the pages, frames,
//!   graphic frames and guides it changed.
#![deny(clippy::arithmetic_side_effects)]
use crate::folio_input::pt;
use crate::geometry::Rect;
use crate::grids::{MeasureContext, MeasureSystem, parse_measure};
use crate::liquid::{
    GraphicRefit, LayoutRuleError, PageObjectChange, PageObjectKind, RuleContext, StoryReflow, Work, bounds_of, check_revision, finish,
    len, page_objects,
};
use crate::LayoutError;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    Length, PageBleed, PageSide, StudioDocument, StudioGraphicFrame, StudioLayoutGrids, StudioLongDocRecords, StudioPage, validate_layout_grids,
};
use std::collections::BTreeSet;

const EPS: f64 = 1e-6;

/// The ten STU-LAY-112 keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AdjustKey {
    Width,
    Height,
    BleedInside,
    BleedTop,
    BleedOutside,
    BleedBottom,
    LeftMargin,
    TopMargin,
    RightMargin,
    BottomMargin,
}
impl AdjustKey {
    pub const ALL: [Self; 10] = [
        Self::Width,
        Self::Height,
        Self::BleedInside,
        Self::BleedTop,
        Self::BleedOutside,
        Self::BleedBottom,
        Self::LeftMargin,
        Self::TopMargin,
        Self::RightMargin,
        Self::BottomMargin,
    ];
    /// The captured key spelling.
    pub fn wire(self) -> &'static str {
        match self {
            Self::Width => "width",
            Self::Height => "height",
            Self::BleedInside => "bleedInside",
            Self::BleedTop => "bleedTop",
            Self::BleedOutside => "bleedOutside",
            Self::BleedBottom => "bleedBottom",
            Self::LeftMargin => "leftMargin",
            Self::TopMargin => "topMargin",
            Self::RightMargin => "rightMargin",
            Self::BottomMargin => "bottomMargin",
        }
    }
    /// Exact, case-sensitive key match.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.wire() == s)
    }
    pub fn is_bleed(self) -> bool {
        matches!(self, Self::BleedInside | Self::BleedTop | Self::BleedOutside | Self::BleedBottom)
    }
}

/// A bag value: a number in `pt` or a measurement string.
#[derive(Clone, Debug, PartialEq)]
pub enum AdjustValue {
    Pt(f64),
    Text(String),
}

/// A partial property bag (only the values that change).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct AdjustBag {
    entries: Vec<(AdjustKey, AdjustValue)>,
}
impl AdjustBag {
    /// Most entries a bag takes (the ten keys, each once).
    pub const MAX_ENTRIES: usize = 10;
    /// Builds a bag from captured key spellings; an unknown key is `adjust_key_unknown`, a key given
    /// twice is `adjust_key_repeated`.
    pub fn from_entries<K: AsRef<str>>(entries: impl IntoIterator<Item = (K, AdjustValue)>) -> Result<Self, LayoutRuleError> {
        let mut out: Vec<(AdjustKey, AdjustValue)> = Vec::new();
        for (k, v) in entries {
            let k = k.as_ref();
            let key = AdjustKey::parse(k).ok_or_else(|| LayoutRuleError::AdjustKeyUnknown(k.chars().take(64).collect()))?;
            if out.iter().any(|(e, _)| *e == key) {
                return Err(LayoutRuleError::AdjustKeyRepeated(key.wire().to_owned()));
            }
            out.push((key, v));
        }
        Ok(Self { entries: out })
    }
    pub fn keys(&self) -> impl Iterator<Item = AdjustKey> + '_ {
        self.entries.iter().map(|(k, _)| *k)
    }
}

/// Where the operation applies.
#[derive(Clone, Debug, PartialEq)]
pub enum AdjustScope {
    Document,
    /// A page collection (page keys).
    Pages(Vec<String>),
    Page(String),
}

/// The three required STU-LAY-010 booleans and the caller's font-size bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdjustOptions {
    pub adjust_font_size: bool,
    /// Required with `adjust_font_size` (SPEC GAP: no default).
    pub font_size_min_pt: Option<f64>,
    pub font_size_max_pt: Option<f64>,
    pub include_locked: bool,
    pub move_ruler_guides: bool,
}

/// One adjust-layout command.
#[derive(Clone, Copy)]
pub struct AdjustRequest<'a> {
    pub document: &'a StudioDocument,
    pub graphics: Option<&'a StudioLongDocRecords>,
    pub grids: Option<&'a StudioLayoutGrids>,
    pub bag: &'a AdjustBag,
    pub scope: &'a AdjustScope,
    pub options: AdjustOptions,
    /// Layer ids of locked objects (Folio stores no object lock).
    pub locked: &'a [String],
    /// `percent_of_pt` is replaced per key by the key's current value.
    pub measure: MeasureContext,
    pub expected_revision: u64,
    pub context: RuleContext<'a>,
}

/// The font-size factor of one page with the caller's bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontSizeAdjust {
    pub factor: f64,
    pub min_pt: f64,
    pub max_pt: f64,
}
impl FontSizeAdjust {
    /// `size * factor` clamped to the bounds.
    pub fn apply(&self, size_pt: f64) -> f64 {
        (size_pt * self.factor).clamp(self.min_pt, self.max_pt)
    }
}

/// One page before and after.
#[derive(Clone, Debug, PartialEq)]
pub struct PageAdjust {
    pub page_id: String,
    pub before: StudioPage,
    pub after: StudioPage,
    pub font_size: Option<FontSizeAdjust>,
}

/// What [`undo_adjust`] restores.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct AdjustUndo {
    pub pages: Vec<StudioPage>,
    pub objects: Vec<PageObjectChange>,
    /// Graphic frames before (bounds and fitting).
    pub graphic_frames: Vec<StudioGraphicFrame>,
    /// `(guide_id, location before)`.
    pub guides: Vec<(String, Length)>,
}

/// The adjust-layout proposal; nothing is written until the caller accepts it.
#[derive(Clone, Debug, PartialEq)]
pub struct AdjustProposal {
    pub base_revision: u64,
    pub document: StudioDocument,
    pub graphics: Option<StudioLongDocRecords>,
    pub grids: Option<StudioLayoutGrids>,
    pub pages: Vec<PageAdjust>,
    pub objects: Vec<PageObjectChange>,
    /// Locked objects left in place (no `include_locked`).
    pub locked_skipped: Vec<String>,
    pub guides_moved: Vec<String>,
    /// Bag keys with no effect in this scope (bleed keys at page scope).
    pub ignored_keys: Vec<AdjustKey>,
    pub refits: Vec<GraphicRefit>,
    pub reflows: Vec<StoryReflow>,
    pub undo: AdjustUndo,
}

/// The edge map of one axis (module docs).
struct AxisMap {
    old: f64,
    new: f64,
    lo_old: f64,
    lo_new: f64,
    box_ratio: f64,
    bleed_lo: (f64, f64),
    bleed_hi: (f64, f64),
}
impl AxisMap {
    fn map(&self, e: f64) -> f64 {
        let near = |a: f64, b: f64| (a - b).abs() <= EPS;
        if near(e, 0.0) {
            0.0
        } else if near(e, self.old) {
            self.new
        } else if near(e, -self.bleed_lo.0) {
            -self.bleed_lo.1
        } else if near(e, self.old + self.bleed_hi.0) {
            self.new + self.bleed_hi.1
        } else {
            self.lo_new + (e - self.lo_old) * self.box_ratio
        }
    }
}

fn bleed_sides(p: &StudioPage) -> Result<[f64; 4], LayoutError> {
    // [left, top, right, bottom], physical sides.
    let Some(b) = &p.bleed else {
        return Ok([0.0; 4]);
    };
    let (inside, outside) = (pt(&b.inside_or_left)?, pt(&b.outside_or_right)?);
    let (left, right) = if p.side == PageSide::LeftHand { (outside, inside) } else { (inside, outside) };
    Ok([left, pt(&b.top)?, right, pt(&b.bottom)?])
}

fn resolve_value(key: AdjustKey, v: &AdjustValue, current: f64, measure: &MeasureContext) -> Result<f64, LayoutRuleError> {
    let out = match v {
        AdjustValue::Pt(x) => *x,
        AdjustValue::Text(s) => {
            let ctx = MeasureContext { percent_of_pt: current, ..*measure };
            parse_measure(s, MeasureSystem::Points, &ctx).map_err(|error| LayoutRuleError::Measure { key: key.wire().to_owned(), error })?
        }
    };
    let ok = out.is_finite() && if matches!(key, AdjustKey::Width | AdjustKey::Height) { out > 0.0 } else { out >= 0.0 };
    if ok { Ok(out) } else { Err(LayoutRuleError::AdjustValueInvalid(key.wire().to_owned())) }
}

/// The page after applying the bag (ignored keys skipped).
fn adjusted_page(p: &StudioPage, bag: &AdjustBag, ignore_bleed: bool, measure: &MeasureContext) -> Result<StudioPage, LayoutRuleError> {
    let mut out = p.clone();
    for (key, v) in &bag.entries {
        if key.is_bleed() && ignore_bleed {
            continue;
        }
        let bleed = out.bleed.get_or_insert_with(|| PageBleed { top: len(0.0), bottom: len(0.0), inside_or_left: len(0.0), outside_or_right: len(0.0) });
        let slot: &mut Length = match key {
            AdjustKey::Width => &mut out.width,
            AdjustKey::Height => &mut out.height,
            AdjustKey::LeftMargin => &mut out.margins.left,
            AdjustKey::TopMargin => &mut out.margins.top,
            AdjustKey::RightMargin => &mut out.margins.right,
            AdjustKey::BottomMargin => &mut out.margins.bottom,
            AdjustKey::BleedInside => &mut bleed.inside_or_left,
            AdjustKey::BleedOutside => &mut bleed.outside_or_right,
            AdjustKey::BleedTop => &mut bleed.top,
            AdjustKey::BleedBottom => &mut bleed.bottom,
        };
        let value = resolve_value(*key, v, pt(slot)?, measure)?;
        *slot = len(value);
    }
    if p.bleed.is_none() && !bag.entries.iter().any(|(k, _)| k.is_bleed() && !ignore_bleed) {
        out.bleed = None;
    }
    Ok(out)
}

fn axis_maps(before: &StudioPage, after: &StudioPage) -> Result<(AxisMap, AxisMap), LayoutRuleError> {
    let (bo, ao) = (bleed_sides(before)?, bleed_sides(after)?);
    let (mb, ma) = (&before.margins, &after.margins);
    let axis = |old: f64, new: f64, lo: (f64, f64), hi: (f64, f64), bl: (f64, f64), bh: (f64, f64), key: &str| {
        let (box_old, box_new) = (old - lo.0 - hi.0, new - lo.1 - hi.1);
        if box_old <= EPS || box_new <= EPS {
            return Err(LayoutRuleError::AdjustValueInvalid(key.to_owned()));
        }
        Ok(AxisMap { old, new, lo_old: lo.0, lo_new: lo.1, box_ratio: box_new / box_old, bleed_lo: bl, bleed_hi: bh })
    };
    let x = axis(
        pt(&before.width)?,
        pt(&after.width)?,
        (pt(&mb.left)?, pt(&ma.left)?),
        (pt(&mb.right)?, pt(&ma.right)?),
        (bo[0], ao[0]),
        (bo[2], ao[2]),
        "leftMargin/rightMargin",
    )?;
    let y = axis(
        pt(&before.height)?,
        pt(&after.height)?,
        (pt(&mb.top)?, pt(&ma.top)?),
        (pt(&mb.bottom)?, pt(&ma.bottom)?),
        (bo[1], ao[1]),
        (bo[3], ao[3]),
        "topMargin/bottomMargin",
    )?;
    Ok((x, y))
}

fn scope_pages(document: &StudioDocument, scope: &AdjustScope) -> Result<Vec<String>, LayoutRuleError> {
    let all = || document.page_spreads.iter().flat_map(|s| s.pages.iter()).map(|p| p.page_id.clone()).collect::<Vec<_>>();
    let pages = match scope {
        AdjustScope::Document => all(),
        AdjustScope::Pages(list) => {
            let mut seen = BTreeSet::new();
            list.iter().filter(|p| seen.insert(p.as_str())).cloned().collect()
        }
        AdjustScope::Page(p) => vec![p.clone()],
    };
    if pages.is_empty() {
        return Err(LayoutRuleError::ScopeEmpty);
    }
    for p in &pages {
        if document.locate_page(p).is_none() {
            return Err(LayoutRuleError::PageMissing(p.chars().take(128).collect()));
        }
    }
    Ok(pages)
}

/// Runs adjust layout (module docs).
pub fn adjust_layout(req: &AdjustRequest<'_>, cancel: &CancellationToken) -> Result<AdjustProposal, LayoutRuleError> {
    cancel.check().map_err(|_| LayoutError::Canceled)?;
    check_revision(req.document, req.expected_revision)?;
    let o = req.options;
    let bounds = if o.adjust_font_size {
        let (Some(min), Some(max)) = (o.font_size_min_pt, o.font_size_max_pt) else {
            return Err(LayoutRuleError::FontSizeBoundsMissing);
        };
        if !(min.is_finite() && max.is_finite() && min > 0.0 && min <= max) {
            return Err(LayoutRuleError::FontSizeBoundsInvalid);
        }
        Some((min, max))
    } else {
        None
    };
    let pages = scope_pages(req.document, req.scope)?;
    let page_scope = !matches!(req.scope, AdjustScope::Document);
    let ignored_keys: Vec<AdjustKey> = if page_scope { req.bag.keys().filter(|k| k.is_bleed()).collect() } else { Vec::new() };
    let locked: BTreeSet<&str> = req.locked.iter().map(String::as_str).collect();
    let mut work = Work::new(req.document, req.graphics);
    let mut grids = req.grids.cloned();
    let (mut out_pages, mut locked_skipped, mut guides_moved, mut undo_guides) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for page_id in &pages {
        cancel.check().map_err(|_| LayoutError::Canceled)?;
        let before = req.document.locate_page(page_id).ok_or_else(|| LayoutRuleError::PageMissing(page_id.clone()))?.page.clone();
        let after = adjusted_page(&before, req.bag, page_scope, &req.measure)?;
        let (mx, my) = axis_maps(&before, &after)?;
        for obj in page_objects(req.document, req.graphics, page_id)? {
            if locked.contains(obj.layer_id.as_str()) && !o.include_locked {
                locked_skipped.push(obj.layer_id.clone());
                continue;
            }
            let r = obj.rect;
            let to = Rect { x0_pt: mx.map(r.x0_pt), y0_pt: my.map(r.y0_pt), x1_pt: mx.map(r.x1_pt), y1_pt: my.map(r.y1_pt) };
            work.place(page_id, &obj, to)?;
        }
        if o.move_ruler_guides
            && let Some(g) = grids.as_mut()
        {
            for guide in g.guides.iter_mut().filter(|q| q.page_id.as_deref() == Some(page_id.as_str())) {
                if guide.locked && !o.include_locked {
                    continue;
                }
                let v = pt(&guide.location)?;
                let moved = match guide.orientation {
                    hsk_studio_folio::StudioGuideOrientation::Vertical => mx.map(v),
                    hsk_studio_folio::StudioGuideOrientation::Horizontal => my.map(v),
                };
                if (moved - v).abs() > EPS {
                    undo_guides.push((guide.guide_id.clone(), guide.location.clone()));
                    guide.location = len(moved);
                    guides_moved.push(guide.guide_id.clone());
                }
            }
        }
        let slot = work
            .document
            .page_spreads
            .iter_mut()
            .flat_map(|s| s.pages.iter_mut())
            .find(|p| p.page_id == *page_id)
            .ok_or_else(|| LayoutRuleError::PageMissing(page_id.clone()))?;
        *slot = after.clone();
        let font_size = bounds.map(|(min_pt, max_pt)| FontSizeAdjust { factor: mx.box_ratio.min(my.box_ratio), min_pt, max_pt });
        out_pages.push(PageAdjust { page_id: page_id.clone(), before, after, font_size });
    }
    if let Some(g) = &grids {
        validate_layout_grids(g, &work.document, cancel)?;
    }
    let reflows = finish(&work.document, work.graphics.as_ref(), None, &req.context, &[], cancel)?;
    let undo = AdjustUndo {
        pages: out_pages.iter().map(|p| p.before.clone()).collect(),
        objects: work.objects.clone(),
        graphic_frames: work
            .objects
            .iter()
            .filter(|o| o.kind == PageObjectKind::GraphicFrame)
            .filter_map(|o| req.graphics.and_then(|g| g.graphic_frames.iter().find(|f| f.layer_id == o.layer_id)).cloned())
            .collect(),
        guides: undo_guides,
    };
    Ok(AdjustProposal {
        base_revision: req.document.revision,
        document: work.document,
        graphics: work.graphics,
        grids,
        pages: out_pages,
        objects: work.objects,
        locked_skipped,
        guides_moved,
        ignored_keys,
        refits: work.refits,
        reflows,
        undo,
    })
}

/// Restores what an accepted adjust-layout proposal changed: pages, text-frame bounds, graphic
/// frames (bounds and fitting) and guide locations.
pub fn undo_adjust(
    document: &StudioDocument,
    graphics: Option<&StudioLongDocRecords>,
    grids: Option<&StudioLayoutGrids>,
    undo: &AdjustUndo,
) -> (StudioDocument, Option<StudioLongDocRecords>, Option<StudioLayoutGrids>) {
    let mut d = document.clone();
    for before in &undo.pages {
        if let Some(p) = d.page_spreads.iter_mut().flat_map(|s| s.pages.iter_mut()).find(|p| p.page_id == before.page_id) {
            *p = before.clone();
        }
    }
    let mut g = graphics.cloned();
    for o in &undo.objects {
        match o.kind {
            PageObjectKind::TextFrame => {
                if let Some(f) = d.layers.iter_mut().find(|l| l.layer_id == o.layer_id).and_then(|l| l.text_frame.as_mut()) {
                    f.bounds = bounds_of(o.before);
                }
            }
            PageObjectKind::GraphicFrame => {}
        }
    }
    for before in &undo.graphic_frames {
        if let Some(f) = g.as_mut().and_then(|g| g.graphic_frames.iter_mut().find(|f| f.layer_id == before.layer_id)) {
            *f = before.clone();
        }
    }
    let mut grids = grids.cloned();
    if let Some(gr) = grids.as_mut() {
        for (id, loc) in &undo.guides {
            if let Some(q) = gr.guides.iter_mut().find(|q| q.guide_id == *id) {
                q.location = loc.clone();
            }
        }
    }
    (d, g, grids)
}
