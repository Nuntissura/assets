//! nib-backed vector drawing (lane SMB-COMPOSITE-VECTOR-IN-GROUP slice 2; nib owns vector geometry,
//! coverage and paint, CX-CAP-001, lane SMB-NIB-VECTOR-RENDER). Composite only routes:
//!
//! - Exact coverage ([`crate::VectorCoverage::Exact`]): the layer's outline (one path, or nib
//!   `boolean` Union of its paths, or the nib `boolean` of a live boolean group's operands, bottom
//!   first; SUBTRACT = nib `SubtractFront`, back minus front) rasterised by `nib::raster::rasterize`
//!   (exact signed-area coverage, byte-identical on every host).
//! - Strokes: every path (a boolean group: its outline) outlined by `nib::stroke::outline_stroke`,
//!   the outlines united by nib `boolean` Union, rasterised by nib over the layer plane.
//! - Gradient and pattern paint: `nib::raster::paint_coverage` over full coverage on the layer plane
//!   (premultiplied), stored straight as an `rgb_f32le` + `f32le` tile the plan reads; the layer's
//!   coverage mask then cuts it, like a solid row.
//!
//! nib errors map to typed refusal codes; budgets (pixels, work, pieces) are nib's own, checked
//! before allocation (V4-02).
#![deny(clippy::arithmetic_side_effects)]

/// After Effects-style shape layers (lane SMB-AE-MOTION-TOOLS): ordered generators, paints and
/// nib path operators.
pub mod shape_layer;

use crate::vector::{BooleanOp, VectorGeometry};
use hsk_studio_nib::boolean::{BooleanOp as NibOp, BooleanOptions, boolean};
use hsk_studio_nib::raster::{CoveragePlane, Paint, PixelFrame, RasterOptions, paint_coverage, rasterize};
use hsk_studio_nib::stroke::{StrokeOptions, StrokeStyle, outline_stroke};
use hsk_studio_nib::{Error as NibError, OwnedPath, PathElement};
use hsk_studio_pigment::Rect;

pub const REFUSE_STROKE_STYLE: &str = "vector_stroke_style";
pub const REFUSE_PAINT: &str = "vector_paint";
pub const REFUSE_NIB_BUDGET: &str = "vector_nib_budget";

fn geometry_code(e: NibError) -> &'static str {
    match e {
        NibError::BudgetExceeded | NibError::Overflow => REFUSE_NIB_BUDGET,
        _ => crate::vector::REFUSE_VECTOR_GEOMETRY,
    }
}

/// nib boolean of `paths` (two or more); an empty result is `None`.
fn combine(paths: Vec<OwnedPath>, op: NibOp) -> Result<Option<OwnedPath>, &'static str> {
    match boolean(&paths, op, &BooleanOptions::default()) {
        Ok(r) => Ok(Some(r.path)),
        Err(NibError::EmptyPathfinderResult) => Ok(None),
        Err(e) => Err(geometry_code(e)),
    }
}

/// The filled outline of a geometry (`None` = empty), module docs.
pub(crate) fn outline(g: &VectorGeometry, depth: usize) -> Result<Option<OwnedPath>, &'static str> {
    if depth > crate::vector::MAX_BOOLEAN_DEPTH {
        return Err(crate::vector::REFUSE_VECTOR_GEOMETRY);
    }
    match g {
        VectorGeometry::Paths(paths) => match paths.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(one.clone())),
            many => combine(many.to_vec(), NibOp::Union),
        },
        VectorGeometry::Boolean { op, operands } => {
            let next = depth.checked_add(1).ok_or(crate::vector::REFUSE_VECTOR_GEOMETRY)?;
            let mut outs = Vec::with_capacity(operands.len());
            for o in operands {
                outs.push(outline(o, next)?);
            }
            let nib_op = match op {
                BooleanOp::Union => NibOp::Union,
                BooleanOp::Intersect => NibOp::Intersect,
                BooleanOp::Subtract => NibOp::SubtractFront,
                BooleanOp::Exclude => NibOp::Exclude,
            };
            let present: Vec<OwnedPath> = match op {
                // An empty operand empties an intersection; an empty first operand empties a
                // subtraction; empty later operands subtract nothing.
                BooleanOp::Intersect if outs.iter().any(Option::is_none) => return Ok(None),
                BooleanOp::Subtract if outs.first().is_none_or(Option::is_none) => {
                    return Ok(None);
                }
                _ => outs.into_iter().flatten().collect(),
            };
            match present.len() {
                0 => Ok(None),
                1 => Ok(present.into_iter().next()),
                _ => combine(present, nib_op),
            }
        }
    }
}

/// The painted stroke area of a geometry (`None` = empty), module docs.
pub(crate) fn stroke_outline(
    g: &VectorGeometry,
    style: &StrokeStyle,
) -> Result<Option<OwnedPath>, &'static str> {
    let sources: Vec<OwnedPath> = match g {
        VectorGeometry::Paths(paths) => paths.clone(),
        VectorGeometry::Boolean { .. } => outline(g, 0)?.into_iter().collect(),
    };
    let mut outs = Vec::with_capacity(sources.len());
    for p in &sources {
        match outline_stroke(p, style, &StrokeOptions::default()) {
            Ok(r) => outs.push(r.path),
            Err(NibError::EmptyPathfinderResult) => {}
            Err(
                NibError::UnsupportedOptions
                | NibError::UnsupportedStyle
                | NibError::InvalidRequest
                | NibError::UnavailableStyle,
            ) => return Err(REFUSE_STROKE_STYLE),
            Err(e) => return Err(geometry_code(e)),
        }
    }
    match outs.len() {
        0 => Ok(None),
        1 => Ok(outs.into_iter().next()),
        _ => combine(outs, NibOp::Union),
    }
}

/// Conservative bounds of a path (every point, control points included); `None` = no points.
pub(crate) fn path_bounds(p: &OwnedPath) -> Option<[f64; 4]> {
    let mut b: Option<[f64; 4]> = None;
    let mut add = |x: f64, y: f64| {
        b = Some(match b {
            None => [x, y, x, y],
            Some([a, c, d, e]) => [a.min(x), c.min(y), d.max(x), e.max(y)],
        });
    };
    for e in &p.elements {
        match e {
            PathElement::MoveTo(q) | PathElement::LineTo(q) => add(q.x, q.y),
            PathElement::QuadTo(c, q) => {
                add(c.x, c.y);
                add(q.x, q.y);
            }
            PathElement::CubicTo(c1, c2, q) => {
                add(c1.x, c1.y);
                add(c2.x, c2.y);
                add(q.x, q.y);
            }
            PathElement::Close => {}
        }
    }
    b
}

fn frame_of(rect: Rect) -> Result<PixelFrame, &'static str> {
    Ok(PixelFrame {
        x: i32::try_from(rect.x).map_err(|_| crate::vector::REFUSE_VECTOR_OVER_LIMIT)?,
        y: i32::try_from(rect.y).map_err(|_| crate::vector::REFUSE_VECTOR_OVER_LIMIT)?,
        width: rect.width,
        height: rect.height,
    })
}

/// nib's exact coverage of `path` over `rect` (`None` path = zero coverage).
pub(crate) fn coverage(
    path: Option<&OwnedPath>,
    rect: Rect,
    max_pixels: u64,
) -> Result<Vec<u8>, &'static str> {
    let frame = frame_of(rect)?;
    let pixels = u64::from(rect.width)
        .checked_mul(u64::from(rect.height))
        .filter(|p| *p <= max_pixels)
        .ok_or(crate::vector::REFUSE_VECTOR_OVER_LIMIT)?;
    let Some(path) = path else {
        return Ok(vec![0; usize::try_from(pixels).map_err(|_| crate::vector::REFUSE_VECTOR_OVER_LIMIT)?]);
    };
    let options = RasterOptions {
        max_pixels,
        ..RasterOptions::default()
    };
    rasterize(path, frame, &options)
        .map(|(plane, _)| plane.data)
        .map_err(geometry_code)
}

/// A gradient or pattern row evaluated over `rect`: straight RGBA planes and their SHA-256.
#[derive(Clone, Debug, PartialEq)]
pub struct PaintedTile {
    pub placement: Rect,
    /// `rgb_f32le`, row-major.
    pub colour: Vec<u8>,
    /// `f32le` straight alpha.
    pub alpha: Vec<u8>,
    pub colour_sha256: [u8; 32],
    pub alpha_sha256: [u8; 32],
}

/// nib paint at every pixel centre of `rect` (full coverage), stored straight.
pub(crate) fn painted(paint: &Paint, rect: Rect, max_pixels: u64) -> Result<PaintedTile, &'static str> {
    let frame = frame_of(rect)?;
    let pixels = u64::from(rect.width)
        .checked_mul(u64::from(rect.height))
        .filter(|p| *p <= max_pixels)
        .ok_or(crate::vector::REFUSE_VECTOR_OVER_LIMIT)?;
    let n = usize::try_from(pixels).map_err(|_| crate::vector::REFUSE_VECTOR_OVER_LIMIT)?;
    let full = CoveragePlane {
        frame,
        data: vec![255; n],
    };
    let plane = paint_coverage(&full, paint).map_err(|e| match e {
        NibError::BudgetExceeded | NibError::Overflow => REFUSE_NIB_BUDGET,
        _ => REFUSE_PAINT,
    })?;
    let mut colour = Vec::with_capacity(n.checked_mul(12).ok_or(REFUSE_NIB_BUDGET)?);
    let mut alpha = Vec::with_capacity(n.checked_mul(4).ok_or(REFUSE_NIB_BUDGET)?);
    for p in &plane.data {
        let a = p[3].clamp(0.0, 1.0);
        for c in &p[..3] {
            let straight = if a > 0.0 { (c / a).clamp(0.0, 1.0) } else { 0.0 };
            colour.extend_from_slice(&straight.to_le_bytes());
        }
        alpha.extend_from_slice(&a.to_le_bytes());
    }
    Ok(PaintedTile {
        placement: rect,
        colour_sha256: hsk_studio_prism::profile_hash(&colour),
        alpha_sha256: hsk_studio_prism::profile_hash(&alpha),
        colour,
        alpha,
    })
}
