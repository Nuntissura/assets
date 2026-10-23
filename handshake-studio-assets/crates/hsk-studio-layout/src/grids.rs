//! Grids, guides, snapping and measurement units (lane SMB-ID-PREPRESS; STU-LAY-050..053,
//! STU-LAY-101, STU-LAY-114, STU-LAY-115). Reads the Folio `hsk.studio.layout_grids@1` records.
//!
//! - Baseline grid: lines at `zero + baseline_start + k * baseline_division` (k >= 0) down the page,
//!   zero = page top or top margin (`baseline_grid_relative_option`). A frame with a custom
//!   baseline grid uses its own start, increment and zero (page top, top margin, frame top or the
//!   frame's top inset); other frames show the document grid. Frame lines are reported in frame
//!   coordinates inside the frame's inset area.
//! - Document grid: major lines every gridline division from the page origin, minor lines at
//!   division / subdivision, per axis.
//! - Snapping ([`snap_point`]): to guides (when `guides_snap_to`) and to document grid lines (when
//!   `document_grid_snap_to`), per axis, nearest within the caller's zone in points (the UI converts
//!   `guide_snap_to_zone` pixels at its zoom); a guide wins a tie. Spread guides (no page) are
//!   mapped into the page's coordinates. Locked guides still snap.
//! - Baseline alignment in composition: `reflow_grid` (threaded reflow) aligns paragraphs marked
//!   `all_lines` / `first_line_only` to the frame's baseline grid.
//! - Units ([`to_points`], [`parse_measure`]): the twelve ruler systems of STU-LAY-101 and the
//!   ten input suffixes (`cm mm in pt % px vh vw vmin vmax`); an unknown suffix is
//!   `unit_suffix_unknown`. Conversions: 72 pt per inch, pica 12 pt, cicero 12 Didot points of
//!   0.376065 mm, agate 1/14 inch, Q and Ha 0.25 mm, American point 1/72.27 inch, custom = caller
//!   points per unit; InDesign's exact cicero and custom-unit definitions are UNVERIFIED (D5).
//!
//! Line counts are bounded ([`MAX_GRID_LINES`], `budget_exceeded`). InDesign parity UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use crate::LayoutError;
use crate::folio_input::pt;
use hsk_studio_folio::{
    StudioBaselineRelative, StudioDocument, StudioFrameBaselineRelative, StudioGuideOrientation, StudioLayoutGrids,
};

/// Most lines one grid query returns.
pub const MAX_GRID_LINES: usize = 100_000;
const EPS: f64 = 1e-9;

/// One document-grid line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridLine {
    pub position_pt: f64,
    pub major: bool,
}

/// Lines `origin + k * step` (k >= 0) inside `[lo, hi]`.
fn ladder(origin: f64, step: f64, lo: f64, hi: f64) -> Result<Vec<f64>, LayoutError> {
    if !(origin.is_finite() && step.is_finite() && step > 0.0 && lo.is_finite() && hi.is_finite()) {
        return Err(LayoutError::InvalidGeometry);
    }
    let first = if origin >= lo { 0.0 } else { ((lo - origin) / step - EPS).ceil() };
    let span = (hi - origin) / step;
    if span - first > MAX_GRID_LINES as f64 {
        return Err(LayoutError::BudgetExceeded);
    }
    let mut out = Vec::new();
    let mut k = first;
    loop {
        let y = origin + k * step;
        if y > hi + EPS {
            break;
        }
        out.push(y);
        k += 1.0;
    }
    Ok(out)
}

fn page_of<'a>(document: &'a StudioDocument, page_id: &str) -> Result<hsk_studio_folio::PageLocation<'a>, LayoutError> {
    document.locate_page(page_id).ok_or(LayoutError::InvalidInput)
}

/// Document baseline grid lines of a page, y in page coordinates (pt, from the page top).
pub fn baseline_grid_lines(document: &StudioDocument, grids: &StudioLayoutGrids, page_id: &str) -> Result<Vec<f64>, LayoutError> {
    let g = grids.document_grid.as_ref().ok_or(LayoutError::InvalidInput)?;
    let page = page_of(document, page_id)?.page;
    let zero = match g.baseline_grid_relative_option {
        StudioBaselineRelative::TopOfPage => 0.0,
        StudioBaselineRelative::TopMargin => pt(&page.margins.top)?,
    };
    ladder(zero + pt(&g.baseline_start)?, pt(&g.baseline_division)?, 0.0, pt(&page.height)?)
}

/// `(origin, step)` of the baseline grid a text frame shows, in frame coordinates.
pub(crate) fn frame_grid(document: &StudioDocument, grids: &StudioLayoutGrids, layer_id: &str) -> Result<(f64, f64), LayoutError> {
    let layer = document.layers.iter().find(|l| l.layer_id == layer_id).ok_or(LayoutError::InvalidInput)?;
    let f = layer.text_frame.as_ref().ok_or(LayoutError::InvalidInput)?;
    let page = page_of(document, &f.page_id)?.page;
    let frame_y = pt(&f.bounds.y)?;
    if let Some(c) = grids.frame_baseline_grid(layer_id).filter(|c| c.use_custom_baseline_frame_grid) {
        let zero = match c.baseline_frame_grid_relative_option {
            StudioFrameBaselineRelative::TopOfPage => -frame_y,
            StudioFrameBaselineRelative::TopMargin => pt(&page.margins.top)? - frame_y,
            StudioFrameBaselineRelative::TopOfFrame => 0.0,
            StudioFrameBaselineRelative::FrameInset => pt(&f.inset_spacing.top)?,
        };
        return Ok((zero + pt(&c.starting_offset_for_baseline_frame_grid)?, pt(&c.baseline_frame_grid_increment)?));
    }
    let g = grids.document_grid.as_ref().ok_or(LayoutError::InvalidInput)?;
    let zero = match g.baseline_grid_relative_option {
        StudioBaselineRelative::TopOfPage => 0.0,
        StudioBaselineRelative::TopMargin => pt(&page.margins.top)?,
    };
    Ok((zero + pt(&g.baseline_start)? - frame_y, pt(&g.baseline_division)?))
}

/// Baseline grid lines a text frame shows, in frame coordinates, inside its inset area.
pub fn frame_baseline_lines(document: &StudioDocument, grids: &StudioLayoutGrids, layer_id: &str) -> Result<Vec<f64>, LayoutError> {
    let (origin, step) = frame_grid(document, grids, layer_id)?;
    let f = document
        .layers
        .iter()
        .find(|l| l.layer_id == layer_id)
        .and_then(|l| l.text_frame.as_ref())
        .ok_or(LayoutError::InvalidInput)?;
    let (top, bottom) = (pt(&f.inset_spacing.top)?, pt(&f.bounds.height)? - pt(&f.inset_spacing.bottom)?);
    let mut out = ladder(origin, step, top, bottom)?;
    out.retain(|y| *y >= top - EPS);
    Ok(out)
}

/// Document grid of a page: (vertical lines by x, horizontal lines by y), page coordinates.
pub fn document_grid_lines(
    document: &StudioDocument,
    grids: &StudioLayoutGrids,
    page_id: &str,
) -> Result<(Vec<GridLine>, Vec<GridLine>), LayoutError> {
    let g = grids.document_grid.as_ref().ok_or(LayoutError::InvalidInput)?;
    let page = page_of(document, page_id)?.page;
    let axis = |division: f64, sub: u32, extent: f64| -> Result<Vec<GridLine>, LayoutError> {
        if sub == 0 {
            return Err(LayoutError::InvalidGeometry);
        }
        let step = division / f64::from(sub);
        let lines = ladder(0.0, step, 0.0, extent)?;
        Ok(lines
            .into_iter()
            .enumerate()
            .map(|(i, p)| GridLine { position_pt: p, major: (i as u64).is_multiple_of(u64::from(sub)) })
            .collect())
    };
    Ok((
        axis(pt(&g.vertical_gridline_division)?, g.vertical_grid_subdivision, pt(&page.width)?)?,
        axis(pt(&g.horizontal_gridline_division)?, g.horizontal_grid_subdivision, pt(&page.height)?)?,
    ))
}

/// What a coordinate snapped to.
#[derive(Clone, Debug, PartialEq)]
pub enum SnapSource {
    Guide(String),
    DocumentGrid,
}

/// A snapped coordinate.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapHit {
    pub value_pt: f64,
    pub source: SnapSource,
}

/// Snaps a page point per axis (module docs). Returns `(x, y)`, `None` = no target in the zone.
pub fn snap_point(
    document: &StudioDocument,
    grids: &StudioLayoutGrids,
    page_id: &str,
    x_pt: f64,
    y_pt: f64,
    zone_pt: f64,
) -> Result<(Option<SnapHit>, Option<SnapHit>), LayoutError> {
    if !(x_pt.is_finite() && y_pt.is_finite() && zone_pt.is_finite() && zone_pt >= 0.0) {
        return Err(LayoutError::InvalidGeometry);
    }
    let loc = page_of(document, page_id)?;
    let page_x = loc.spread.pages[..loc.page_index].iter().map(|p| pt(&p.width)).sum::<Result<f64, LayoutError>>()?;
    let mut best: [Option<(f64, SnapHit)>; 2] = [None, None];
    let mut offer = |axis: usize, value: f64, source: SnapSource, at: f64| {
        let d = (value - at).abs();
        if d <= zone_pt + EPS && best[axis].as_ref().is_none_or(|(bd, _)| d < *bd - EPS) {
            best[axis] = Some((d, SnapHit { value_pt: value, source }));
        }
    };
    if grids.guide_preferences.as_ref().is_some_and(|p| p.guides_snap_to) {
        for g in &grids.guides {
            if g.page_spread_id != loc.spread.page_spread_id || g.page_id.as_ref().is_some_and(|p| *p != page_id) {
                continue;
            }
            let v = pt(&g.location)?;
            match g.orientation {
                StudioGuideOrientation::Vertical => {
                    let v = if g.page_id.is_none() { v - page_x } else { v };
                    offer(0, v, SnapSource::Guide(g.guide_id.clone()), x_pt);
                }
                StudioGuideOrientation::Horizontal => offer(1, v, SnapSource::Guide(g.guide_id.clone()), y_pt),
            }
        }
    }
    if grids.document_grid.as_ref().is_some_and(|g| g.document_grid_snap_to) {
        let (vs, hs) = document_grid_lines(document, grids, page_id)?;
        let near = |lines: &[GridLine], at: f64| lines.iter().map(|l| l.position_pt).min_by(|a, b| (a - at).abs().total_cmp(&(b - at).abs()));
        if let Some(v) = near(&vs, x_pt) {
            offer(0, v, SnapSource::DocumentGrid, x_pt);
        }
        if let Some(v) = near(&hs, y_pt) {
            offer(1, v, SnapSource::DocumentGrid, y_pt);
        }
    }
    let [x, y] = best;
    Ok((x.map(|b| b.1), y.map(|b| b.1)))
}

/// STU-LAY-101 ruler measurement systems.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MeasureSystem {
    Points,
    Picas,
    Inches,
    InchesDecimal,
    Millimeters,
    Centimeters,
    Ciceros,
    Agates,
    Q,
    Ha,
    AmericanPoints,
    /// Points per custom unit.
    Custom(f64),
}

/// Points in one unit of `system`.
pub fn to_points(system: MeasureSystem) -> f64 {
    const MM: f64 = 72.0 / 25.4;
    match system {
        MeasureSystem::Points => 1.0,
        MeasureSystem::Picas => 12.0,
        MeasureSystem::Inches | MeasureSystem::InchesDecimal => 72.0,
        MeasureSystem::Millimeters => MM,
        MeasureSystem::Centimeters => 10.0 * MM,
        MeasureSystem::Ciceros => 12.0 * 0.376065 * MM,
        MeasureSystem::Agates => 72.0 / 14.0,
        MeasureSystem::Q | MeasureSystem::Ha => 0.25 * MM,
        MeasureSystem::AmericanPoints => 72.0 / 72.27,
        MeasureSystem::Custom(p) => p,
    }
}

/// What relative input units resolve against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeasureContext {
    /// `%` of this length, pt.
    pub percent_of_pt: f64,
    /// `px` per inch.
    pub pixels_per_inch: f64,
    /// Viewport size for `vw vh vmin vmax`, pt.
    pub viewport_pt: [f64; 2],
}

/// Typed measurement-input refusals (STU-LAY-166).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MeasureError {
    NotANumber,
    UnitSuffixUnknown(String),
}
impl MeasureError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotANumber => "measure_not_a_number",
            Self::UnitSuffixUnknown(_) => "unit_suffix_unknown",
        }
    }
}

/// Parses a typed measurement (`12pt`, `3.5 mm`, `50%`, `2vw`, a bare number = `default` system)
/// to points. Suffixes are the ten of STU-LAY-101, case-insensitive; anything else is refused.
pub fn parse_measure(input: &str, default: MeasureSystem, ctx: &MeasureContext) -> Result<f64, MeasureError> {
    let s = input.trim();
    if s.len() > 64 {
        return Err(MeasureError::NotANumber);
    }
    let split = s
        .char_indices()
        .find(|(i, c)| !(c.is_ascii_digit() || *c == '.' || ((*c == '-' || *c == '+') && *i == 0)))
        .map_or(s.len(), |(i, _)| i);
    let (num, unit) = s.split_at(split);
    let v: f64 = num.parse().map_err(|_| MeasureError::NotANumber)?;
    if !v.is_finite() {
        return Err(MeasureError::NotANumber);
    }
    let unit = unit.trim().to_ascii_lowercase();
    let [vw, vh] = ctx.viewport_pt;
    let per = match unit.as_str() {
        "" => to_points(default),
        "pt" => 1.0,
        "in" => 72.0,
        "mm" => to_points(MeasureSystem::Millimeters),
        "cm" => to_points(MeasureSystem::Centimeters),
        "px" => 72.0 / ctx.pixels_per_inch,
        "%" => ctx.percent_of_pt / 100.0,
        "vw" => vw / 100.0,
        "vh" => vh / 100.0,
        "vmin" => vw.min(vh) / 100.0,
        "vmax" => vw.max(vh) / 100.0,
        other => return Err(MeasureError::UnitSuffixUnknown(other.to_owned())),
    };
    let out = v * per;
    if out.is_finite() { Ok(out) } else { Err(MeasureError::NotANumber) }
}
