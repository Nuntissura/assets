//! Edge-refinement brush set (STU-RAS-122 "modes `refine_edge`, `add_to_selection`,
//! `subtract_from_selection`, plus a matte/foreground/background mode set and a feather mode, each
//! driven by the one brush engine of §6"), lane SMB-PS-SELECT-LIQUIFY-2.
//!
//! [`RefineState`] is the editable state of the refinement surface: the selection plus painted
//! trimap marks. Every [`RefineState::apply_brush`] is one edit (clone the state for undo); the
//! refined result is always re-rendered from the state by [`RefineState::render`] through
//! [`super::refine::refine_with_marks`]. Strokes are placed by [`crate::brush::place_dabs`] and
//! rasterized by [`crate::brush::stroke_coverage`] (round tip, 25 % spacing).
//!
//! Mode readings (Studio; Photoshop UNVERIFIED, D5): `RefineEdge` and `Matte` add the footprint
//! to the unknown band; `Foreground` / `Background` pin it to 1 / 0; `AddToSelection` /
//! `SubtractFromSelection` combine the footprint into the selection (extend / diminish);
//! `Feather` marks pixels whose output is blended toward a Gaussian feather of radius
//! `feather_px` by the mark coverage. A later mark of another kind clears the footprint from the
//! earlier kinds. Decontaminated colours are not produced while feather-mode marks exist.
#![deny(clippy::arithmetic_side_effects)]
use super::refine::{RefineOptions, RefineOutput, TrimapMarks, refine_with_marks};
use super::{ToolError, ToolLimits};
use crate::brush::{BrushDynamics, BrushLimits, BrushTip, StrokeSample, place_dabs, stroke_coverage};
use crate::selection::{ColourView, CombineMode, CoveragePlane, SelectionContext, combine, feather};

/// STU-RAS-122 brush modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefineBrushMode {
    RefineEdge,
    AddToSelection,
    SubtractFromSelection,
    Matte,
    Foreground,
    Background,
    Feather,
}

/// One refinement-brush stroke.
#[derive(Clone, Copy, Debug)]
pub struct RefineBrush<'a> {
    pub samples: &'a [StrokeSample],
    pub diameter_px: f64,
    pub hardness_pct: f64,
    pub mode: RefineBrushMode,
    /// Gaussian radius of the feather mode (pixels, `> 0` in that mode).
    pub feather_px: f32,
}

/// Selection + painted marks of the refinement surface.
#[derive(Clone, Debug, PartialEq)]
pub struct RefineState {
    pub selection: CoveragePlane,
    pub unknown: Option<CoveragePlane>,
    pub foreground: Option<CoveragePlane>,
    pub background: Option<CoveragePlane>,
    pub feather: Option<CoveragePlane>,
    pub feather_px: f32,
}

fn add(into: &mut Option<CoveragePlane>, foot: &CoveragePlane, ctx: &SelectionContext) -> Result<(), ToolError> {
    *into = Some(combine(into.as_ref(), foot, CombineMode::Extend, &ctx.limits)?);
    Ok(())
}

fn remove(from: &mut Option<CoveragePlane>, foot: &CoveragePlane, ctx: &SelectionContext) -> Result<(), ToolError> {
    if let Some(p) = from.as_ref() {
        *from = Some(combine(Some(p), foot, CombineMode::Diminish, &ctx.limits)?);
    }
    Ok(())
}

impl RefineState {
    pub fn new(selection: CoveragePlane) -> Self {
        Self { selection, unknown: None, foreground: None, background: None, feather: None, feather_px: 0.0 }
    }

    /// Applies one stroke (all-or-nothing: the state changes only when every step succeeded).
    pub fn apply_brush(&mut self, brush: &RefineBrush<'_>, ctx: &SelectionContext) -> Result<(), ToolError> {
        let tip = BrushTip::round(brush.diameter_px, brush.hardness_pct, 25.0);
        let blim = BrushLimits { max_bytes: ctx.limits.max_bytes, ..BrushLimits::default() };
        let dabs = place_dabs(&tip, &BrushDynamics::default(), brush.samples, &blim)?;
        if dabs.is_empty() {
            return Ok(());
        }
        let foot = stroke_coverage(&dabs, brush.hardness_pct, ctx.canvas, &blim)?;
        let mut next = self.clone();
        match brush.mode {
            RefineBrushMode::AddToSelection => {
                next.selection = combine(Some(&next.selection), &foot, CombineMode::Extend, &ctx.limits)?;
            }
            RefineBrushMode::SubtractFromSelection => {
                next.selection = combine(Some(&next.selection), &foot, CombineMode::Diminish, &ctx.limits)?;
            }
            RefineBrushMode::RefineEdge | RefineBrushMode::Matte => {
                add(&mut next.unknown, &foot, ctx)?;
                remove(&mut next.foreground, &foot, ctx)?;
                remove(&mut next.background, &foot, ctx)?;
            }
            RefineBrushMode::Foreground => {
                add(&mut next.foreground, &foot, ctx)?;
                remove(&mut next.unknown, &foot, ctx)?;
                remove(&mut next.background, &foot, ctx)?;
            }
            RefineBrushMode::Background => {
                add(&mut next.background, &foot, ctx)?;
                remove(&mut next.unknown, &foot, ctx)?;
                remove(&mut next.foreground, &foot, ctx)?;
            }
            RefineBrushMode::Feather => {
                if !brush.feather_px.is_finite() || brush.feather_px <= 0.0 {
                    return Err(ToolError::OutOfRange);
                }
                crate::coverage_math::feather_kernel(brush.feather_px)?;
                add(&mut next.feather, &foot, ctx)?;
                next.feather_px = brush.feather_px;
            }
        }
        *self = next;
        Ok(())
    }

    /// The refined result of the current state.
    pub fn render(
        &self,
        layers: &[ColourView<'_>],
        active: usize,
        opts: &RefineOptions,
        ctx: &SelectionContext,
        limits: &ToolLimits,
    ) -> Result<RefineOutput, ToolError> {
        let marks = TrimapMarks {
            unknown: self.unknown.as_ref(),
            foreground: self.foreground.as_ref(),
            background: self.background.as_ref(),
        };
        let mut out = refine_with_marks(layers, active, &self.selection, &marks, opts, ctx, limits)?;
        if let Some(mark) = &self.feather
            && self.feather_px > 0.0
        {
            let soft = feather(&out.coverage, self.feather_px, ctx)?;
            let bounds = soft.bounds;
            let mut data = Vec::with_capacity(soft.data.len());
            let w = (bounds.width as usize).max(1);
            for (i, v) in soft.data.iter().enumerate() {
                let x = bounds.x.saturating_add(i.checked_rem(w).unwrap_or(0) as i64);
                let y = bounds.y.saturating_add(i.checked_div(w).unwrap_or(0) as i64);
                let t = f64::from(mark.at(x, y)) / 65535.0;
                let a = f64::from(out.coverage.at(x, y));
                data.push((a + (f64::from(*v) - a) * t).round().clamp(0.0, 65535.0) as u16);
            }
            out.coverage = super::tighten(CoveragePlane { bounds, data })?;
            // The feather grows the bounds past the decontaminated colours: they are not produced
            // together with feather-mode marks (re-render without them for colours).
            out.colours = None;
        }
        Ok(out)
    }
}
