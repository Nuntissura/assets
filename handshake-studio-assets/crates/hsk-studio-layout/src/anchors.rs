//! Anchored objects (lane SMB-LAYOUT-UI; STU-LAY-025, STU-LAY-127). Conventions (Studio's,
//! InDesign parity UNVERIFIED, D5):
//!
//! - inline: the glyph at `anchor_byte` (the placeholder character the caller shaped) is given the
//!   advance `x_offset + width` before composition, so the object takes room in its line; the box
//!   sits at x = glyph x + `x_offset` with its bottom `y_offset` above the baseline. Line height
//!   does not grow; a box that leaves its frame (top above the frame top, right past the frame
//!   right) is `inline_beyond_container` (STU-LAY-127 constraint vocabulary).
//! - above line: the anchor must start a single-column paragraph (else `unsupported_composition`);
//!   the paragraph is pushed down by `space_above + height + y_offset` (a pushed paragraph that
//!   falls to the next column carries the space there); the box sits `y_offset` above the first
//!   line's top, horizontally aligned left/center/right in the line's column plus `x_offset`.
//! - custom: no effect on the flow; the reference point (column edge, text frame, page margins,
//!   page edge or anchor location; line baseline, column edge, text frame, page margins or page
//!   edge vertically) with its alignment, plus the offsets, receives the object's `anchor_point`.
//!   `spine_relative` mirrors alignment, x offset and anchor point on left-hand pages;
//!   `pin_position` keeps the box between the frame's top and bottom.
//!
//! An anchor whose byte is overset (or not composed) is listed in `unplaced`. A layer that is a
//! member of a group is `group_member` (it cannot be re-anchored).
#![deny(clippy::arithmetic_side_effects)]
use crate::fitting::PlacedGraphic;
use crate::folio_input::pt;
use crate::geometry::Rect;
use crate::paragraph::ShapedParagraph;
use crate::threaded_reflow::LayoutProposal;
use crate::LayoutError;
use hsk_studio_folio::{
    NodeKind, PageSide, Parent, StudioAnchorHAlign, StudioAnchorHRef, StudioAnchorPoint,
    StudioAnchorPosition, StudioAnchorVAlign, StudioAnchorVRef, StudioAnchoredObject,
    StudioDocument, StudioLayoutFlowRecords, StudioTextStory,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnchorError {
    Layout(LayoutError),
    /// STU-LAY-127: a member of a group cannot be re-anchored.
    GroupMember(String),
    /// STU-LAY-127: an inline item cannot be moved beyond its container boundary.
    InlineBeyondContainer(String),
}
impl AnchorError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Layout(e) => e.code(),
            Self::GroupMember(_) => "anchor_group_member",
            Self::InlineBeyondContainer(_) => "inline_beyond_container",
        }
    }
}
impl From<LayoutError> for AnchorError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}
impl std::fmt::Display for AnchorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for AnchorError {}

/// One placed anchored object, spread coordinates (pt, y down).
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedAnchor {
    pub layer_id: String,
    /// The text frame holding the anchor's line.
    pub frame_id: String,
    pub spread: usize,
    pub page: usize,
    pub rect: Rect,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct AnchorProposal {
    pub placed: Vec<PlacedAnchor>,
    /// Layers whose anchor byte is overset or not composed.
    pub unplaced: Vec<String>,
}

fn size(a: &StudioAnchoredObject) -> Result<(f64, f64), LayoutError> {
    let (w, h) = (pt(&a.width)?, pt(&a.height)?);
    if w > 0.0 && h > 0.0 { Ok((w, h)) } else { Err(LayoutError::InvalidGeometry) }
}

/// Anchors of `story` in record order.
fn of_story<'a>(records: &'a StudioLayoutFlowRecords, story: &StudioTextStory) -> impl Iterator<Item = &'a StudioAnchoredObject> {
    let id = story.story_id.clone();
    records.anchored_objects.iter().filter(move |a| a.story_id == id)
}

/// Flow effects of the anchors of `story`: paragraphs with inline anchor glyph advances set
/// (`None` = unchanged) and per paragraph the above-line space pushed before it.
pub(crate) fn flow_effects(
    records: &StudioLayoutFlowRecords,
    story: &StudioTextStory,
    paragraphs: &[ShapedParagraph],
) -> Result<(Option<Vec<ShapedParagraph>>, Vec<f64>), LayoutError> {
    let mut pre = vec![0.0f64; paragraphs.len()];
    let mut adjusted: Option<Vec<ShapedParagraph>> = None;
    for a in of_story(records, story) {
        let (w, h) = size(a)?;
        let s = &a.settings;
        let (xo, yo, above) = (pt(&s.anchor_x_offset)?, pt(&s.anchor_y_offset)?, pt(&s.anchor_space_above)?);
        match s.anchored_position {
            StudioAnchorPosition::Inline => {
                let advance = xo + w;
                if advance <= 0.0 || !advance.is_finite() {
                    return Err(LayoutError::InvalidGeometry);
                }
                let paras = adjusted.get_or_insert_with(|| paragraphs.to_vec());
                let g = paras
                    .iter_mut()
                    .flat_map(|p| p.glyphs.iter_mut())
                    .find(|g| g.cluster.start == a.anchor_byte)
                    .ok_or(LayoutError::InvalidInput)?;
                g.advance_pt = advance;
            }
            StudioAnchorPosition::AboveLine => {
                let i = paragraphs
                    .iter()
                    .position(|p| p.source.start == a.anchor_byte)
                    .ok_or(LayoutError::UnsupportedComposition)?;
                let space = above + h + yo;
                if !(space.is_finite() && space >= 0.0) {
                    return Err(LayoutError::InvalidGeometry);
                }
                pre[i] += space;
            }
            StudioAnchorPosition::Custom => {}
        }
    }
    Ok((adjusted, pre))
}

/// The paragraphs with the inline anchors' advances applied (the glyph runs `reflow_flow`
/// composed): pass these, not the shaped originals, to painters such as the PDF export, so the
/// glyphs after an inline object are drawn where layout placed them.
pub fn inline_adjusted_paragraphs(
    records: &StudioLayoutFlowRecords,
    story: &StudioTextStory,
    paragraphs: &[ShapedParagraph],
) -> Result<Vec<ShapedParagraph>, LayoutError> {
    let (adjusted, _) = flow_effects(records, story, paragraphs)?;
    Ok(adjusted.unwrap_or_else(|| paragraphs.to_vec()))
}

/// Frame-local geometry of a text-frame layer: (spread x, spread y, w, h, inner box, columns).
struct FrameGeo {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    inset: [f64; 4],
    columns: usize,
    gutter: f64,
    page_x: f64,
    page_w: f64,
    page_h: f64,
    margins: [f64; 4],
    side: PageSide,
}
impl FrameGeo {
    fn column(&self, c: usize) -> (f64, f64) {
        let lo = self.x + self.inset[3];
        let hi = self.x + self.w - self.inset[1];
        let n = self.columns.max(1) as f64;
        let cw = (hi - lo - (n - 1.0) * self.gutter) / n;
        let x0 = lo + c as f64 * (cw + self.gutter);
        (x0, x0 + cw)
    }
}

fn frame_geo(document: &StudioDocument, frame_id: &str) -> Result<FrameGeo, LayoutError> {
    let layer = document
        .layers
        .iter()
        .find(|l| l.layer_id == frame_id)
        .ok_or(LayoutError::InvalidInput)?;
    let f = layer.text_frame.as_ref().ok_or(LayoutError::InvalidInput)?;
    let loc = document.locate_page(&f.page_id).ok_or(LayoutError::InvalidInput)?;
    let page_x = loc.spread.pages[..loc.page_index]
        .iter()
        .map(|p| pt(&p.width))
        .sum::<Result<f64, LayoutError>>()?;
    let m = &loc.page.margins;
    let i = &f.inset_spacing;
    Ok(FrameGeo {
        x: page_x + pt(&f.bounds.x)?,
        y: pt(&f.bounds.y)?,
        w: pt(&f.bounds.width)?,
        h: pt(&f.bounds.height)?,
        inset: [pt(&i.top)?, pt(&i.right)?, pt(&i.bottom)?, pt(&i.left)?],
        columns: usize::try_from(f.text_column_count).map_err(|_| LayoutError::InvalidGeometry)?,
        gutter: pt(&f.text_column_gutter)?,
        page_x,
        page_w: pt(&loc.page.width)?,
        page_h: pt(&loc.page.height)?,
        margins: [pt(&m.top)?, pt(&m.right)?, pt(&m.bottom)?, pt(&m.left)?],
        side: loc.page.side,
    })
}

fn mirror_point(p: StudioAnchorPoint) -> StudioAnchorPoint {
    use StudioAnchorPoint as P;
    match p {
        P::TopLeft => P::TopRight,
        P::TopRight => P::TopLeft,
        P::LeftCenter => P::RightCenter,
        P::RightCenter => P::LeftCenter,
        P::BottomLeft => P::BottomRight,
        P::BottomRight => P::BottomLeft,
        other => other,
    }
}

fn point_offset(p: StudioAnchorPoint, w: f64, h: f64) -> (f64, f64) {
    use StudioAnchorPoint as P;
    let fx = match p {
        P::TopLeft | P::LeftCenter | P::BottomLeft => 0.0,
        P::TopCenter | P::Center | P::BottomCenter => 0.5,
        P::TopRight | P::RightCenter | P::BottomRight => 1.0,
    };
    let fy = match p {
        P::TopLeft | P::TopCenter | P::TopRight => 0.0,
        P::LeftCenter | P::Center | P::RightCenter => 0.5,
        P::BottomLeft | P::BottomCenter | P::BottomRight => 1.0,
    };
    (fx * w, fy * h)
}

fn align(range: (f64, f64), a: Option<StudioAnchorHAlign>, w: f64) -> f64 {
    match a.unwrap_or(StudioAnchorHAlign::Left) {
        StudioAnchorHAlign::Left => range.0,
        StudioAnchorHAlign::Center => (range.0 + range.1 - w) / 2.0,
        StudioAnchorHAlign::Right => range.1 - w,
    }
}

/// Places the anchored objects of `story` from its composition (module docs). `paragraphs` are the
/// ones passed to `reflow_flow` (the inline advances are re-applied here).
pub fn place_anchored_objects(
    document: &StudioDocument,
    records: &StudioLayoutFlowRecords,
    story: &StudioTextStory,
    paragraphs: &[ShapedParagraph],
    placement: &LayoutProposal,
) -> Result<AnchorProposal, AnchorError> {
    let (adjusted, _) = flow_effects(records, story, paragraphs)?;
    let paras = adjusted.as_deref().unwrap_or(paragraphs);
    let mut out = AnchorProposal::default();
    for a in of_story(records, story) {
        let layer = document
            .layers
            .iter()
            .find(|l| l.layer_id == a.layer_id)
            .ok_or(LayoutError::InvalidInput)?;
        if let Parent::Node(n) = &layer.parent
            && n.node_kind == NodeKind::Layer
        {
            return Err(AnchorError::GroupMember(a.layer_id.clone()));
        }
        let (w, h) = size(a)?;
        let s = &a.settings;
        let (mut xo, yo) = (pt(&s.anchor_x_offset)?, pt(&s.anchor_y_offset)?);
        // The line holding the anchor byte.
        let found = placement.frames.iter().find_map(|f| {
            f.lines
                .iter()
                .find(|l| l.source.start <= a.anchor_byte && a.anchor_byte < l.source.end)
                .map(|l| (f, l))
        });
        let Some((frame, line)) = found else {
            out.unplaced.push(a.layer_id.clone());
            continue;
        };
        let p = paras.get(line.paragraph).ok_or(LayoutError::InvalidInput)?;
        let before: f64 = p.glyphs[line.glyph_range.clone()]
            .iter()
            .take_while(|g| g.cluster.start < a.anchor_byte)
            .map(|g| g.advance_pt)
            .sum();
        let anchor_x = frame.origin_x_pt + line.x_start_pt + before;
        let baseline = frame.origin_y_pt + line.baseline_y_pt;
        let geo = frame_geo(document, &frame.frame_id)?;
        let column = geo.column(line.column);
        let rect = match s.anchored_position {
            StudioAnchorPosition::Inline => {
                let r = Rect { x0_pt: anchor_x + xo, y0_pt: baseline - yo - h, x1_pt: anchor_x + xo + w, y1_pt: baseline - yo };
                if r.y0_pt < geo.y - 1e-9 || r.x1_pt > geo.x + geo.w + 1e-9 || r.y1_pt > geo.y + geo.h + 1e-9 {
                    return Err(AnchorError::InlineBeyondContainer(a.layer_id.clone()));
                }
                r
            }
            StudioAnchorPosition::AboveLine => {
                let top = baseline - p.ascent_pt - yo - h;
                let x0 = align(column, s.horizontal_alignment, w) + xo;
                Rect { x0_pt: x0, y0_pt: top, x1_pt: x0 + w, y1_pt: top + h }
            }
            StudioAnchorPosition::Custom => {
                let mut ha = s.horizontal_alignment;
                let mut point = s.anchor_point;
                if s.spine_relative && geo.side == PageSide::LeftHand {
                    xo = -xo;
                    point = mirror_point(point);
                    ha = ha.map(|v| match v {
                        StudioAnchorHAlign::Left => StudioAnchorHAlign::Right,
                        StudioAnchorHAlign::Right => StudioAnchorHAlign::Left,
                        c => c,
                    });
                }
                let page = (geo.page_x, geo.page_x + geo.page_w);
                let hr = match s.horizontal_reference_point.unwrap_or(StudioAnchorHRef::TextFrame) {
                    StudioAnchorHRef::ColumnEdge => column,
                    StudioAnchorHRef::TextFrame => (geo.x, geo.x + geo.w),
                    StudioAnchorHRef::PageMargins => (page.0 + geo.margins[3], page.1 - geo.margins[1]),
                    StudioAnchorHRef::PageEdge => page,
                    StudioAnchorHRef::AnchorLocation => (anchor_x, anchor_x),
                };
                let vr = match s.vertical_reference_point.unwrap_or(StudioAnchorVRef::LineBaseline) {
                    StudioAnchorVRef::LineBaseline => (baseline, baseline),
                    StudioAnchorVRef::ColumnEdge => (geo.y + geo.inset[0], geo.y + geo.h - geo.inset[2]),
                    StudioAnchorVRef::TextFrame => (geo.y, geo.y + geo.h),
                    StudioAnchorVRef::PageMargins => (geo.margins[0], geo.page_h - geo.margins[2]),
                    StudioAnchorVRef::PageEdge => (0.0, geo.page_h),
                };
                let x_ref = match ha.unwrap_or(StudioAnchorHAlign::Left) {
                    StudioAnchorHAlign::Left => hr.0,
                    StudioAnchorHAlign::Center => (hr.0 + hr.1) / 2.0,
                    StudioAnchorHAlign::Right => hr.1,
                };
                let y_ref = match s.vertical_alignment.unwrap_or(StudioAnchorVAlign::Top) {
                    StudioAnchorVAlign::Top => vr.0,
                    StudioAnchorVAlign::Center => (vr.0 + vr.1) / 2.0,
                    StudioAnchorVAlign::Bottom => vr.1,
                };
                let (ox, oy) = point_offset(point, w, h);
                let x0 = x_ref + xo - ox;
                let mut y0 = y_ref + yo - oy;
                if s.pin_position {
                    y0 = y0.max(geo.y).min(geo.y + geo.h - h);
                }
                Rect { x0_pt: x0, y0_pt: y0, x1_pt: x0 + w, y1_pt: y0 + h }
            }
        };
        out.placed.push(PlacedAnchor {
            layer_id: a.layer_id.clone(),
            frame_id: frame.frame_id.clone(),
            spread: frame.spread,
            page: frame.page,
            rect,
        });
    }
    Ok(out)
}

/// A graphic frame moved to its anchor: the frame's top-left goes to the anchor box's top-left;
/// the fitted content moves with it (size and fit unchanged).
pub fn anchored_graphic(g: &PlacedGraphic, a: &PlacedAnchor) -> PlacedGraphic {
    let (dx, dy) = (a.rect.x0_pt - g.frame.x0_pt, a.rect.y0_pt - g.frame.y0_pt);
    let shift = |r: Rect| Rect { x0_pt: r.x0_pt + dx, y0_pt: r.y0_pt + dy, x1_pt: r.x1_pt + dx, y1_pt: r.y1_pt + dy };
    let mut t = g.transform;
    t[4] += dx;
    t[5] += dy;
    PlacedGraphic {
        spread: a.spread,
        page: a.page,
        frame: shift(g.frame),
        transform: t,
        content_box: shift(g.content_box),
        ..g.clone()
    }
}
