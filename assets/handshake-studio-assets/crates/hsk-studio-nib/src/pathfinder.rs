//! The unified geometry-operation set (lane SMB-ILLUSTRATOR-PEN slice 3; STU-VEC-012, STU-VEC-130,
//! STU-VEC-133): one entry point, [`pathfinder`], over the thirteen captured operation tokens.
//!
//! Built here on top of [`crate::boolean`] (same flatten, one power-of-two integer grid, i_overlay
//! sweep, same budgets and receipt): `uniteCommand`, `intersectCommand`, `excludeCommand`,
//! `backMinusFrontCommand`, `frontMinusBackCommand` (one result piece, delegated to
//! [`crate::boolean::boolean`]), and the multi-piece operations
//! - `divideCommand`: every face of the overlap arrangement as a separate closed region, each piece
//!   listing the operands (bottom-first indices) that cover it;
//! - `trimCommand`: each operand minus every operand above it (hidden overlaps removed, one piece
//!   per visible operand, region boundaries kept);
//! - `mergeCommand`: trim, then the touching trimmed pieces that share a paint key are united
//!   (pieces that do not touch stay separate);
//! - `cropCommand`: every operand below the topmost intersected with the topmost, which is removed.
//!
//! `outlineCommand`, `hardCommand`, `softCommand` and `trapCommand` are refused
//! (`UnsupportedOperation`): outline produces open stroked borders and the other three composite
//! paint, not geometry, so they need the paint model (not built in this slice).
//!
//! Results are polygons (outer contours positive, holes negative, fill rule non-zero) like every
//! boolean result. An operation with no piece at all is the typed `EmptyPathfinderResult`
//! (STU-VEC-133 "empty pathfinder result"). Piece order is deterministic: divide lists faces in
//! operand-insertion order, then i_overlay's shape order. Adobe's own output (piece order, curve
//! refit, which piece keeps which appearance) is UNVERIFIED (D5).
//!
//! Hostile sizes (V4-01/02): the boolean budgets apply (flatten points, operand count,
//! candidate-pair guard before i_overlay allocates), and divide/merge stop at [`MAX_PIECES`].
#![deny(clippy::arithmetic_side_effects)]
use crate::boolean::{BooleanOp, BooleanOptions, GeometryReceipt, Grid, Region, binary, boolean, candidate_guard, count_points, normalise, union_all};
use crate::flatten::flatten;
use crate::owned_path::OwnedPath;
use crate::path_contract::Error;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::i_shape::int::shape::IntShapes;

/// Most pieces one divide or merge produces.
pub const MAX_PIECES: usize = 4096;

/// The thirteen STU-VEC-130 operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathfinderOp {
    Union,
    Intersect,
    Exclude,
    SubtractFront,
    SubtractBack,
    Divide,
    Trim,
    Merge,
    Crop,
    Outline,
    HardMix,
    SoftMix,
    Trap,
}

impl PathfinderOp {
    pub const ALL: [Self; 13] = [
        Self::Union,
        Self::Intersect,
        Self::Exclude,
        Self::SubtractFront,
        Self::SubtractBack,
        Self::Divide,
        Self::Trim,
        Self::Merge,
        Self::Crop,
        Self::Outline,
        Self::HardMix,
        Self::SoftMix,
        Self::Trap,
    ];
    /// Captured token (wire value).
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Union => "uniteCommand",
            Self::Intersect => "intersectCommand",
            Self::Exclude => "excludeCommand",
            Self::SubtractFront => "backMinusFrontCommand",
            Self::SubtractBack => "frontMinusBackCommand",
            Self::Divide => "divideCommand",
            Self::Trim => "trimCommand",
            Self::Merge => "mergeCommand",
            Self::Crop => "cropCommand",
            Self::Outline => "outlineCommand",
            Self::HardMix => "hardCommand",
            Self::SoftMix => "softCommand",
            Self::Trap => "trapCommand",
        }
    }
    /// Studio member name (`geom.*`).
    pub const fn member(self) -> &'static str {
        match self {
            Self::Union => "geom.union",
            Self::Intersect => "geom.intersect",
            Self::Exclude => "geom.exclude",
            Self::SubtractFront => "geom.subtract_front",
            Self::SubtractBack => "geom.subtract_back",
            Self::Divide => "geom.divide",
            Self::Trim => "geom.trim",
            Self::Merge => "geom.merge",
            Self::Crop => "geom.crop",
            Self::Outline => "geom.outline",
            Self::HardMix => "geom.hard_mix",
            Self::SoftMix => "geom.soft_mix",
            Self::Trap => "geom.trap",
        }
    }
    pub fn parse(text: &str) -> Result<Self, Error> {
        Self::ALL.into_iter().find(|o| o.wire() == text).ok_or(Error::UnsupportedOperation)
    }
    const fn single(self) -> Option<BooleanOp> {
        match self {
            Self::Union => Some(BooleanOp::Union),
            Self::Intersect => Some(BooleanOp::Intersect),
            Self::Exclude => Some(BooleanOp::Exclude),
            Self::SubtractFront => Some(BooleanOp::SubtractFront),
            Self::SubtractBack => Some(BooleanOp::SubtractBack),
            _ => None,
        }
    }
}

/// One result region.
#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    /// Polygon path, non-zero.
    pub path: OwnedPath,
    /// Operand indices (bottom first) covering the piece: every covering operand for divide; the
    /// one operand it came from for trim and crop; every merged operand for merge; all operands
    /// for the single-result operations.
    pub sources: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PathfinderResult {
    pub pieces: Vec<Piece>,
    pub receipt: GeometryReceipt,
}

struct Prepared {
    grid: Grid,
    sets: Vec<IntShapes>,
    regions: Vec<Region>,
    pairs: u64,
}

fn prepare(operands: &[OwnedPath], options: &BooleanOptions) -> Result<Prepared, Error> {
    if operands.len() < 2 {
        return Err(Error::InvalidRequest);
    }
    if operands.len() > options.max_operands {
        return Err(Error::BudgetExceeded);
    }
    let mut regions = Vec::with_capacity(operands.len());
    let mut flatten_options = options.flatten;
    for operand in operands {
        let lines = flatten(operand, &flatten_options)?;
        flatten_options.max_points = flatten_options.max_points.saturating_sub(count_points(&lines));
        regions.push(Region { lines, fill_rule: operand.fill_rule });
    }
    let grid = Grid::covering(regions.iter().flat_map(|r| r.lines.iter()))?;
    let pairs = candidate_guard(regions.iter().flat_map(|r| r.lines.iter()), &grid, options)?;
    let sets = regions.iter().map(|r| normalise(&grid.contours(&r.lines), r.fill_rule)).collect();
    Ok(Prepared { grid, sets, regions, pairs })
}

fn receipt(p: &Prepared, pieces: &[(IntShapes, Vec<usize>)], tolerance: f64) -> GeometryReceipt {
    let input_points = p.regions.iter().fold(0_u64, |n, r| n.saturating_add(u64::try_from(count_points(&r.lines)).unwrap_or(u64::MAX)));
    let (mut points, mut contours) = (0_u64, 0_u64);
    for contour in pieces.iter().flat_map(|(s, _)| s.iter().flatten()) {
        points = points.saturating_add(u64::try_from(contour.len()).unwrap_or(u64::MAX));
        contours = contours.saturating_add(1);
    }
    GeometryReceipt { flatten_tolerance: tolerance, grid_quantum: p.grid.quantum, input_points, candidate_pairs: p.pairs, output_points: points, output_contours: contours }
}

fn push_piece(out: &mut Vec<(IntShapes, Vec<usize>)>, shapes: IntShapes, sources: Vec<usize>) -> Result<(), Error> {
    if shapes.is_empty() {
        return Ok(());
    }
    if out.len() >= MAX_PIECES {
        return Err(Error::BudgetExceeded);
    }
    out.push((shapes, sources));
    Ok(())
}

/// Splits pieces into their connected shapes (one outer contour with its holes each).
fn components(pieces: Vec<(IntShapes, Vec<usize>)>) -> Result<Vec<(IntShapes, Vec<usize>)>, Error> {
    let mut out = Vec::new();
    for (shapes, sources) in pieces {
        for shape in shapes {
            push_piece(&mut out, vec![shape], sources.clone())?;
        }
    }
    Ok(out)
}

fn divide(sets: &[IntShapes]) -> Result<Vec<(IntShapes, Vec<usize>)>, Error> {
    let mut faces: Vec<(IntShapes, Vec<usize>)> = Vec::new();
    let mut below: IntShapes = Vec::new();
    for (i, set) in sets.iter().enumerate() {
        let mut next = Vec::with_capacity(faces.len().saturating_mul(2).saturating_add(1));
        for (face, sources) in faces {
            let mut covered = sources.clone();
            covered.push(i);
            push_piece(&mut next, binary(&face, set, OverlayRule::Intersect), covered)?;
            push_piece(&mut next, binary(&face, set, OverlayRule::Difference), sources)?;
        }
        push_piece(&mut next, binary(set, &below, OverlayRule::Difference), vec![i])?;
        below = union_all(&[below, set.clone()]);
        faces = next;
    }
    components(faces)
}

fn trim(sets: &[IntShapes]) -> Result<Vec<(IntShapes, Vec<usize>)>, Error> {
    let mut out = Vec::new();
    let mut above: IntShapes = Vec::new();
    let mut rev = Vec::new();
    for (i, set) in sets.iter().enumerate().rev() {
        rev.push((binary(set, &above, OverlayRule::Difference), vec![i]));
        above = union_all(&[above, set.clone()]);
    }
    for (shapes, sources) in rev.into_iter().rev() {
        push_piece(&mut out, shapes, sources)?;
    }
    Ok(out)
}

fn merge(sets: &[IntShapes], keys: &[u64]) -> Result<Vec<(IntShapes, Vec<usize>)>, Error> {
    if keys.len() != sets.len() {
        return Err(Error::InvalidRequest);
    }
    let trimmed = trim(sets)?;
    // Group by key in first-appearance order (bottom first).
    let mut groups: Vec<(u64, Vec<IntShapes>, Vec<usize>)> = Vec::new();
    for (shapes, sources) in trimmed {
        let key = sources.first().and_then(|i| keys.get(*i)).copied().ok_or(Error::InvalidRequest)?;
        match groups.iter_mut().find(|g| g.0 == key) {
            Some(g) => {
                g.1.push(shapes);
                g.2.extend(sources);
            }
            None => groups.push((key, vec![shapes], sources)),
        }
    }
    let mut out = Vec::new();
    for (_, members, sources) in groups {
        let united = union_all(&members);
        // Each connected component keeps only the operands that contributed to it.
        for shape in united {
            let component = vec![shape];
            let mut contributing: Vec<usize> = Vec::new();
            for (m, src) in members.iter().zip(&sources) {
                if !binary(&component, m, OverlayRule::Intersect).is_empty() {
                    contributing.push(*src);
                }
            }
            push_piece(&mut out, component, contributing)?;
        }
    }
    Ok(out)
}

fn crop(sets: &[IntShapes]) -> Result<Vec<(IntShapes, Vec<usize>)>, Error> {
    let (top, rest) = sets.split_last().ok_or(Error::InvalidRequest)?;
    let mut out = Vec::new();
    for (i, set) in rest.iter().enumerate() {
        push_piece(&mut out, binary(set, top, OverlayRule::Intersect), vec![i])?;
    }
    Ok(out)
}

/// Runs `op` over `operands` (bottom to top, at least two). `paint_keys` (one per operand) is
/// required by `mergeCommand` and ignored by the others.
pub fn pathfinder(op: PathfinderOp, operands: &[OwnedPath], paint_keys: Option<&[u64]>, options: &BooleanOptions) -> Result<PathfinderResult, Error> {
    if let Some(single) = op.single() {
        let r = boolean(operands, single, options)?;
        let sources = (0..operands.len()).collect();
        return Ok(PathfinderResult { pieces: vec![Piece { path: r.path, sources }], receipt: r.receipt });
    }
    if matches!(op, PathfinderOp::Outline | PathfinderOp::HardMix | PathfinderOp::SoftMix | PathfinderOp::Trap) {
        return Err(Error::UnsupportedOperation);
    }
    if op == PathfinderOp::Merge && paint_keys.is_none_or(|k| k.len() != operands.len()) {
        return Err(Error::InvalidRequest);
    }
    let prepared = prepare(operands, options)?;
    let pieces = match op {
        PathfinderOp::Divide => divide(&prepared.sets)?,
        PathfinderOp::Trim => trim(&prepared.sets)?,
        PathfinderOp::Merge => merge(&prepared.sets, paint_keys.unwrap_or(&[]))?,
        PathfinderOp::Crop => crop(&prepared.sets)?,
        _ => return Err(Error::UnsupportedOperation),
    };
    if pieces.is_empty() {
        return Err(Error::EmptyPathfinderResult);
    }
    let receipt = receipt(&prepared, &pieces, options.flatten.tolerance);
    let pieces = pieces
        .iter()
        .map(|(shapes, sources)| Ok(Piece { path: prepared.grid.path_of(shapes)?, sources: sources.clone() }))
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(PathfinderResult { pieces, receipt })
}
