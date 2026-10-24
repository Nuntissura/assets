//! Liquid layout rules (lane SMB-ID-LIQUID-LAYOUT-1; STU-LAY-009, STU-LAY-111). Reads the Folio
//! `hsk.studio.layout_rules@1` records (page rules, parent rules, object pins and resize
//! constraints), the parent-page records (`liquid.use_parent`), the grid records (liquid guides,
//! `StudioGuideType::Liquid`) and the longdoc graphic frames, and turns one page resize into one
//! revision-bound proposal: the new document, the moved objects, the refitted images and the reflow
//! of every supplied story through the existing threaded reflow ([`crate::reflow_flow`]). The
//! proposal document is validated by Folio before it is returned.
//!
//! Geometry per object (page coordinates, pt; old page `W x H`, new page `W' x H'`):
//! - `off`: unchanged. `recenter`: translated by `((W'-W)/2, (H'-H)/2)`, size kept.
//! - `scale`: uniform factor `min(W'/W, H'/H)` about the page origin (top-left); text frames are
//!   scaled and their stories reflowed at unchanged type size.
//! - `object_based`, per axis: both edges pinned = each edge keeps its distance to its page edge;
//!   one edge pinned = that distance kept, size fixed or relative (`size * W'/W`); unpinned = the
//!   object's centre scales with the page, size fixed or relative. An object without a constraint
//!   record is unpinned with fixed sizes.
//! - `guide_based`, per axis: an object a liquid guide of the other orientation crosses strictly
//!   (a vertical guide for x) stretches with the page (`edge * W'/W`); any other object keeps its
//!   size and its centre scales.
//! - `use_parent`: the parent's rule ([`hsk_studio_folio::resolve_liquid_rule`]).
//! - Images: a graphic frame whose size changed is refitted with its stored fitting through
//!   [`crate::fit_content`]; a distorting `content_to_frame` fitting is refitted `proportionally`
//!   so images never distort; `frame_to_content` keeps the content size and the frame follows it.
//! - Margins, insets, columns, gutters and guides are not changed by a liquid rule; overset after
//!   reflow is reported per story on its last frame, text is never dropped.
//!
//! Declared readings, InDesign parity UNVERIFIED (D5, no oracle case): the `scale` anchor and
//! factor, unpinned object movement, guide-based stretch distribution and image refit.
#![deny(clippy::arithmetic_side_effects)]
use crate::fitting::{Crop, FitError, Fitted, fit_content};
use crate::folio_input::pt;
use crate::geometry::Rect;
use crate::grids::MeasureError;
use crate::paragraph::{ShapedParagraph, SpaceParams};
use crate::threaded_reflow::{LayoutProposal, Limits, ReflowRequest, reflow_flow};
use crate::{Composer, LayoutError, SourceSpan, TypesetOptions};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    Bounds, Budget, Code, Diagnostic, GeometryUnit, Length, Resolver, StudioDocument, StudioFit, StudioGuideOrientation,
    StudioGuideType, StudioLayoutFlowRecords, StudioLayoutGrids, StudioLayoutRuleRecords, StudioLiquidRule,
    StudioLongDocRecords, StudioObjectLiquidConstraint, StudioParentPageRecords, StudioResizeConstraint,
    StudioTextStory, resolve_liquid_rule, validate_document, validate_layout_rule_records, validate_longdoc_records,
};

const EPS: f64 = 1e-6;

/// Typed refusals of the liquid, adjust-layout and alternate-layout operations. A refusal never
/// carries a partial proposal and never alters its inputs.
#[derive(Clone, Debug, PartialEq)]
pub enum LayoutRuleError {
    Layout(LayoutError),
    Fit(FitError),
    /// Folio refused the proposed document or records (code and target of its diagnostic).
    Folio { code: Code, target: String },
    PageMissing(String),
    /// An adjust-layout key outside the ten of STU-LAY-112.
    AdjustKeyUnknown(String),
    AdjustKeyRepeated(String),
    /// A value that is not finite, a non-positive size or a negative margin or bleed.
    AdjustValueInvalid(String),
    /// A measurement string the STU-LAY-101 parser refused.
    Measure { key: String, error: MeasureError },
    /// `adjust_font_size` without both caller bounds (STU-LAY-010 SPEC GAP: no default exists).
    FontSizeBoundsMissing,
    FontSizeBoundsInvalid,
    ScopeEmpty,
    SectionMissing(String),
    AlternateLayoutName(&'static str),
    /// A text thread links a frame inside the copied pages to one outside them.
    ThreadLeavesLayout(String),
    /// A copied layer whose parent is neither the root nor its page spread.
    UnsupportedParent(String),
    StoryMissing(String),
    LinkMissing(String),
}
impl LayoutRuleError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Layout(e) => e.code(),
            Self::Fit(e) => e.code(),
            Self::Folio { .. } => "folio_rejected",
            Self::PageMissing(_) => "page_missing",
            Self::AdjustKeyUnknown(_) => "adjust_key_unknown",
            Self::AdjustKeyRepeated(_) => "adjust_key_repeated",
            Self::AdjustValueInvalid(_) => "adjust_value_invalid",
            Self::Measure { error, .. } => error.code(),
            Self::FontSizeBoundsMissing => "font_size_bounds_missing",
            Self::FontSizeBoundsInvalid => "font_size_bounds_invalid",
            Self::ScopeEmpty => "scope_empty",
            Self::SectionMissing(_) => "section_missing",
            Self::AlternateLayoutName(rule) => rule,
            Self::ThreadLeavesLayout(_) => "thread_leaves_layout",
            Self::UnsupportedParent(_) => "unsupported_parent",
            Self::StoryMissing(_) => "story_missing",
            Self::LinkMissing(_) => "link_missing",
        }
    }
}
impl std::fmt::Display for LayoutRuleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for LayoutRuleError {}
impl From<LayoutError> for LayoutRuleError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}
impl From<FitError> for LayoutRuleError {
    fn from(e: FitError) -> Self {
        Self::Fit(e)
    }
}
impl From<Diagnostic> for LayoutRuleError {
    fn from(d: Diagnostic) -> Self {
        Self::Folio { code: d.code, target: d.target }
    }
}

/// One story to reflow after a geometry change: the Folio story, the bytes of stored content and
/// its Type-shaped paragraphs.
#[derive(Clone, Copy, Debug)]
pub struct StoryInput<'a> {
    pub story: &'a StudioTextStory,
    pub resolved_text: Option<&'a str>,
    pub paragraphs: &'a [ShapedParagraph],
}

/// What the rule operations reflow and how Folio checks their result.
#[derive(Clone, Copy)]
pub struct RuleContext<'a> {
    /// Stories reflowed against the proposal document (every story whose frames are affected).
    pub stories: &'a [StoryInput<'a>],
    pub flow: Option<&'a StudioLayoutFlowRecords>,
    pub typeset: TypesetOptions<'a>,
    pub composer: Composer,
    pub space: SpaceParams,
    pub limits: Limits,
    /// Budget and resolver of the Folio validation of the proposal.
    pub budget: Budget,
    pub resolver: &'a dyn Resolver,
}

/// Which record a moved object lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageObjectKind {
    /// `StudioLayer::text_frame` bounds.
    TextFrame,
    /// Longdoc `StudioGraphicFrame` bounds.
    GraphicFrame,
}

/// One object geometry change, page coordinates in pt.
#[derive(Clone, Debug, PartialEq)]
pub struct PageObjectChange {
    pub layer_id: String,
    pub page_id: String,
    pub kind: PageObjectKind,
    pub before: Rect,
    pub after: Rect,
}

/// The refit of a graphic frame whose size changed (frame coordinates of the new frame).
#[derive(Clone, Debug, PartialEq)]
pub struct GraphicRefit {
    pub layer_id: String,
    /// The fitting applied (`content_to_frame` is replaced by `proportionally`).
    pub fit: Option<StudioFit>,
    pub fitted: Fitted,
}

/// The reflow of one story against the proposal document.
#[derive(Clone, Debug, PartialEq)]
pub struct StoryReflow {
    pub story_id: String,
    pub proposal: LayoutProposal,
    /// Overset after reflow: the last frame of the chain and the span that does not fit.
    pub overset: Option<(String, SourceSpan)>,
}

/// One page resize under the page's liquid rule.
#[derive(Clone, Copy)]
pub struct LiquidRequest<'a> {
    pub document: &'a StudioDocument,
    pub records: &'a StudioLayoutRuleRecords,
    pub parents: Option<&'a StudioParentPageRecords>,
    pub grids: Option<&'a StudioLayoutGrids>,
    pub graphics: Option<&'a StudioLongDocRecords>,
    pub page_id: &'a str,
    /// New trim size, pt.
    pub width_pt: f64,
    pub height_pt: f64,
    pub expected_revision: u64,
    pub context: RuleContext<'a>,
}

/// The proposal of one resize: nothing is written until the caller accepts it.
#[derive(Clone, Debug, PartialEq)]
pub struct LiquidProposal {
    pub base_revision: u64,
    pub page_id: String,
    /// The effective rule applied (never `use_parent` or `preserve_existing`).
    pub rule: StudioLiquidRule,
    pub document: StudioDocument,
    pub graphics: Option<StudioLongDocRecords>,
    pub objects: Vec<PageObjectChange>,
    pub refits: Vec<GraphicRefit>,
    pub reflows: Vec<StoryReflow>,
}

pub(crate) fn len(v: f64) -> Length {
    Length { value: v, unit: GeometryUnit::Pt }
}
pub(crate) fn rect_of(b: &Bounds) -> Result<Rect, LayoutError> {
    let (x, y, w, h) = (pt(&b.x)?, pt(&b.y)?, pt(&b.width)?, pt(&b.height)?);
    Ok(Rect { x0_pt: x, y0_pt: y, x1_pt: x + w, y1_pt: y + h })
}
pub(crate) fn bounds_of(r: Rect) -> Bounds {
    Bounds { x: len(r.x0_pt), y: len(r.y0_pt), width: len(r.x1_pt - r.x0_pt), height: len(r.y1_pt - r.y0_pt) }
}
fn positive(r: Rect) -> Result<Rect, LayoutError> {
    let ok = [r.x0_pt, r.y0_pt, r.x1_pt, r.y1_pt].iter().all(|v| v.is_finite()) && r.x1_pt - r.x0_pt > EPS && r.y1_pt - r.y0_pt > EPS;
    if ok { Ok(r) } else { Err(LayoutError::InvalidGeometry) }
}

/// A page object: kind, index in its list (document layers or longdoc graphic frames), layer id.
#[derive(Clone, Debug)]
pub(crate) struct PageObject {
    pub kind: PageObjectKind,
    pub index: usize,
    pub layer_id: String,
    pub rect: Rect,
}

/// The text frames and graphic frames of a page, document order.
pub(crate) fn page_objects(
    document: &StudioDocument,
    graphics: Option<&StudioLongDocRecords>,
    page_id: &str,
) -> Result<Vec<PageObject>, LayoutError> {
    let mut out = Vec::new();
    for (index, l) in document.layers.iter().enumerate() {
        if let Some(f) = l.text_frame.as_ref().filter(|f| f.page_id == page_id) {
            out.push(PageObject { kind: PageObjectKind::TextFrame, index, layer_id: l.layer_id.clone(), rect: rect_of(&f.bounds)? });
        }
    }
    for (index, g) in graphics.map_or(&[][..], |g| g.graphic_frames.as_slice()).iter().enumerate() {
        if g.page_id == page_id {
            out.push(PageObject { kind: PageObjectKind::GraphicFrame, index, layer_id: g.layer_id.clone(), rect: rect_of(&g.bounds)? });
        }
    }
    Ok(out)
}

/// The page trim size.
pub(crate) fn page_size(document: &StudioDocument, page_id: &str) -> Result<(f64, f64), LayoutRuleError> {
    let loc = document.locate_page(page_id).ok_or_else(|| LayoutRuleError::PageMissing(page_id.to_owned()))?;
    let (w, h) = (pt(&loc.page.width)?, pt(&loc.page.height)?);
    if w > 0.0 && h > 0.0 { Ok((w, h)) } else { Err(LayoutError::InvalidGeometry.into()) }
}

/// Liquid guide positions of a page: (vertical guides by x, horizontal guides by y), page coordinates.
pub(crate) fn liquid_guides(
    document: &StudioDocument,
    grids: Option<&StudioLayoutGrids>,
    page_id: &str,
) -> Result<(Vec<f64>, Vec<f64>), LayoutError> {
    let (mut xs, mut ys) = (Vec::new(), Vec::new());
    let Some(grids) = grids else {
        return Ok((xs, ys));
    };
    let loc = document.locate_page(page_id).ok_or(LayoutError::InvalidInput)?;
    let page_x = loc.spread.pages[..loc.page_index].iter().map(|p| pt(&p.width)).sum::<Result<f64, LayoutError>>()?;
    for g in &grids.guides {
        if g.guide_type != StudioGuideType::Liquid
            || g.page_spread_id != loc.spread.page_spread_id
            || g.page_id.as_ref().is_some_and(|p| p != page_id)
        {
            continue;
        }
        let v = pt(&g.location)?;
        match g.orientation {
            StudioGuideOrientation::Vertical => xs.push(if g.page_id.is_none() { v - page_x } else { v }),
            StudioGuideOrientation::Horizontal => ys.push(v),
        }
    }
    Ok((xs, ys))
}

/// One axis of an object under pins and a resize constraint (module docs).
fn pinned_axis(lo: f64, hi: f64, old: f64, new: f64, pin_lo: bool, pin_hi: bool, relative: bool) -> (f64, f64) {
    let s = new / old;
    let size = if relative { (hi - lo) * s } else { hi - lo };
    match (pin_lo, pin_hi) {
        (true, true) => (lo, new - (old - hi)),
        (true, false) => (lo, lo + size),
        (false, true) => {
            let h = new - (old - hi);
            (h - size, h)
        }
        (false, false) => {
            let c = (lo + hi) / 2.0 * s;
            (c - size / 2.0, c + size / 2.0)
        }
    }
}

/// The page-size part of a liquid rule: the new rect of one object.
pub(crate) fn liquid_rect(
    rule: StudioLiquidRule,
    constraint: Option<&StudioObjectLiquidConstraint>,
    guides: &(Vec<f64>, Vec<f64>),
    old: (f64, f64),
    new: (f64, f64),
    r: Rect,
) -> Result<Rect, LayoutError> {
    let ((w, h), (w2, h2)) = (old, new);
    let out = match rule {
        StudioLiquidRule::Off | StudioLiquidRule::UseParent | StudioLiquidRule::PreserveExisting => r,
        StudioLiquidRule::Scale => {
            let s = (w2 / w).min(h2 / h);
            Rect { x0_pt: r.x0_pt * s, y0_pt: r.y0_pt * s, x1_pt: r.x1_pt * s, y1_pt: r.y1_pt * s }
        }
        StudioLiquidRule::Recenter => {
            let (dx, dy) = ((w2 - w) / 2.0, (h2 - h) / 2.0);
            Rect { x0_pt: r.x0_pt + dx, y0_pt: r.y0_pt + dy, x1_pt: r.x1_pt + dx, y1_pt: r.y1_pt + dy }
        }
        StudioLiquidRule::ObjectBased => {
            let rel = |c: StudioResizeConstraint| c == StudioResizeConstraint::Relative;
            let (pl, pt_, pr, pb, rw, rh) = constraint.map_or((false, false, false, false, false, false), |c| {
                (c.pin_left, c.pin_top, c.pin_right, c.pin_bottom, rel(c.width), rel(c.height))
            });
            let (x0, x1) = pinned_axis(r.x0_pt, r.x1_pt, w, w2, pl, pr, rw);
            let (y0, y1) = pinned_axis(r.y0_pt, r.y1_pt, h, h2, pt_, pb, rh);
            Rect { x0_pt: x0, y0_pt: y0, x1_pt: x1, y1_pt: y1 }
        }
        StudioLiquidRule::GuideBased => {
            let crossed = |gs: &[f64], lo: f64, hi: f64| gs.iter().any(|g| *g > lo + EPS && *g < hi - EPS);
            let (sx, sy) = (w2 / w, h2 / h);
            let (x0, x1) = if crossed(&guides.0, r.x0_pt, r.x1_pt) {
                (r.x0_pt * sx, r.x1_pt * sx)
            } else {
                pinned_axis(r.x0_pt, r.x1_pt, w, w2, false, false, false)
            };
            let (y0, y1) = if crossed(&guides.1, r.y0_pt, r.y1_pt) {
                (r.y0_pt * sy, r.y1_pt * sy)
            } else {
                pinned_axis(r.y0_pt, r.y1_pt, h, h2, false, false, false)
            };
            Rect { x0_pt: x0, y0_pt: y0, x1_pt: x1, y1_pt: y1 }
        }
    };
    positive(out)
}

/// Mutable copies the operations write into.
pub(crate) struct Work {
    pub document: StudioDocument,
    pub graphics: Option<StudioLongDocRecords>,
    pub objects: Vec<PageObjectChange>,
    pub refits: Vec<GraphicRefit>,
}
impl Work {
    pub(crate) fn new(document: &StudioDocument, graphics: Option<&StudioLongDocRecords>) -> Self {
        Self { document: document.clone(), graphics: graphics.cloned(), objects: Vec::new(), refits: Vec::new() }
    }
    /// Writes the new rect of an object (refitting a resized graphic frame) and records the change.
    pub(crate) fn place(&mut self, page_id: &str, o: &PageObject, after: Rect) -> Result<(), LayoutRuleError> {
        let after = positive(after)?;
        if after == o.rect {
            return Ok(());
        }
        let mut written = after;
        match o.kind {
            PageObjectKind::TextFrame => {
                let f = self.document.layers.get_mut(o.index).and_then(|l| l.text_frame.as_mut()).ok_or(LayoutError::InvalidInput)?;
                f.bounds = bounds_of(after);
            }
            PageObjectKind::GraphicFrame => {
                let g = self.graphics.as_mut().and_then(|g| g.graphic_frames.get_mut(o.index)).ok_or(LayoutError::InvalidInput)?;
                let resized = ((after.x1_pt - after.x0_pt) - (o.rect.x1_pt - o.rect.x0_pt)).abs() > EPS
                    || ((after.y1_pt - after.y0_pt) - (o.rect.y1_pt - o.rect.y0_pt)).abs() > EPS;
                if resized {
                    let fit = match g.fit {
                        Some(StudioFit::ContentToFrame) => Some(StudioFit::Proportionally),
                        f => f,
                    };
                    let crop = match &g.crop {
                        Some(c) => Crop { top: pt(&c.top)?, left: pt(&c.left)?, bottom: pt(&c.bottom)?, right: pt(&c.right)? },
                        None => Crop { top: 0.0, left: 0.0, bottom: 0.0, right: 0.0 },
                    };
                    let frame = Rect { x0_pt: 0.0, y0_pt: 0.0, x1_pt: after.x1_pt - after.x0_pt, y1_pt: after.y1_pt - after.y0_pt };
                    let fitted = fit_content(frame, pt(&g.content_width)?, pt(&g.content_height)?, fit, g.frame_fitting, g.anchor, crop)?;
                    if fitted.frame != frame {
                        written = positive(Rect {
                            x0_pt: after.x0_pt + fitted.frame.x0_pt,
                            y0_pt: after.y0_pt + fitted.frame.y0_pt,
                            x1_pt: after.x0_pt + fitted.frame.x1_pt,
                            y1_pt: after.y0_pt + fitted.frame.y1_pt,
                        })?;
                    }
                    g.fit = fit;
                    self.refits.push(GraphicRefit { layer_id: o.layer_id.clone(), fit, fitted });
                }
                g.bounds = bounds_of(written);
            }
        }
        self.objects.push(PageObjectChange { layer_id: o.layer_id.clone(), page_id: page_id.to_owned(), kind: o.kind, before: o.rect, after: written });
        Ok(())
    }
    /// Sets a page's trim size.
    pub(crate) fn resize_page(&mut self, page_id: &str, w: f64, h: f64) -> Result<(), LayoutRuleError> {
        let page = self
            .document
            .page_spreads
            .iter_mut()
            .flat_map(|s| s.pages.iter_mut())
            .find(|p| p.page_id == page_id)
            .ok_or_else(|| LayoutRuleError::PageMissing(page_id.to_owned()))?;
        page.width = len(w);
        page.height = len(h);
        Ok(())
    }
}

/// Folio validation of the proposal document (and graphic and rule records when present), then the
/// reflow of every context story against it.
pub(crate) fn finish(
    document: &StudioDocument,
    graphics: Option<&StudioLongDocRecords>,
    rules: Option<(&StudioLayoutRuleRecords, Option<&StudioParentPageRecords>)>,
    context: &RuleContext<'_>,
    extra_stories: &[StoryInput<'_>],
    cancel: &CancellationToken,
) -> Result<Vec<StoryReflow>, LayoutRuleError> {
    validate_document(document.clone(), context.budget, cancel, context.resolver)?;
    if let Some(g) = graphics {
        validate_longdoc_records(g, document, context.budget, cancel, context.resolver)?;
    }
    if let Some((r, parents)) = rules {
        validate_layout_rule_records(r, document, parents, context.budget, cancel, context.resolver)?;
    }
    let none = StudioLayoutFlowRecords::new(document.document_id.clone());
    let flow = context.flow.unwrap_or(&none);
    let mut out = Vec::new();
    for s in context.stories.iter().chain(extra_stories) {
        let req = ReflowRequest {
            document,
            story: s.story,
            resolved_text: s.resolved_text,
            paragraphs: s.paragraphs,
            obstacles: &[],
            composer: context.composer,
            space: context.space,
            expected_revision: document.revision,
            limits: context.limits,
        };
        let proposal = reflow_flow(&req, &context.typeset, flow, cancel)?;
        let overset = proposal.overset.and_then(|span| proposal.frames.last().map(|f| (f.frame_id.clone(), span)));
        out.push(StoryReflow { story_id: s.story.story_id.clone(), proposal, overset });
    }
    Ok(out)
}

pub(crate) fn check_revision(document: &StudioDocument, expected: u64) -> Result<(), LayoutRuleError> {
    if document.revision == expected {
        Ok(())
    } else {
        Err(LayoutError::StaleRevision { expected, current: document.revision }.into())
    }
}

/// Applies the page's liquid rule to a resize of `page_id` to `width_pt x height_pt` (module docs).
pub fn apply_liquid(req: &LiquidRequest<'_>, cancel: &CancellationToken) -> Result<LiquidProposal, LayoutRuleError> {
    cancel.check().map_err(|_| LayoutError::Canceled)?;
    check_revision(req.document, req.expected_revision)?;
    if req.records.document_id != req.document.document_id {
        return Err(LayoutError::InvalidInput.into());
    }
    let (w2, h2) = (req.width_pt, req.height_pt);
    if !(w2.is_finite() && h2.is_finite() && w2 > 0.0 && h2 > 0.0) {
        return Err(LayoutError::InvalidGeometry.into());
    }
    let old = page_size(req.document, req.page_id)?;
    let rule = resolve_liquid_rule(req.records, req.parents, req.page_id);
    let guides = liquid_guides(req.document, req.grids, req.page_id)?;
    let mut work = Work::new(req.document, req.graphics);
    work.resize_page(req.page_id, w2, h2)?;
    for o in page_objects(req.document, req.graphics, req.page_id)? {
        cancel.check().map_err(|_| LayoutError::Canceled)?;
        let after = liquid_rect(rule, req.records.object_constraint(&o.layer_id), &guides, old, (w2, h2), o.rect)?;
        work.place(req.page_id, &o, after)?;
    }
    let reflows = finish(&work.document, work.graphics.as_ref(), None, &req.context, &[], cancel)?;
    Ok(LiquidProposal {
        base_revision: req.document.revision,
        page_id: req.page_id.to_owned(),
        rule,
        document: work.document,
        graphics: work.graphics,
        objects: work.objects,
        refits: work.refits,
        reflows,
    })
}
