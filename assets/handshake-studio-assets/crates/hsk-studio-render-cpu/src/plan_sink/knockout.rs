//! Knockout placements of lane SMB-PS-ADJUST-STYLES-2 (pigment `knockout_math`, the single owner of
//! the rules): shallow in a pass-through group reads the running result of the enclosing buffer just
//! before the group began (a per-worker snapshot), and deep knockouts clear every enclosing group's
//! backdrop by their accumulated shape (a per-worker shape plane per level of isolated-group
//! nesting). Pigment's admission only lets these two placements through a plain ancestry (every
//! enclosing group layer a single-use, unclipped, operator-free group layer), so the context stack
//! below is only pushed by the plain group paths of `Eval::layer`. All buffers are chunk-sized,
//! allocated once per worker and charged with the scratch before the first sink call
//! ([`KnockoutShape::bytes`]).
#![deny(clippy::arithmetic_side_effects)]
use super::RenderError;
use crate::px::overflow;
use hsk_studio_pigment::blend_math::Rgba;
use hsk_studio_pigment::knockout_math::{knockout_clear, knockout_union};

/// Buffer counts of one worker's knockout scratch (zero when the plan has no such knockout).
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct KnockoutShape {
    /// Snapshots: at most one per nested pass-through group that holds a shallow knockout layer.
    pub(super) snapshots: usize,
    /// Shape planes: one per level of isolated-group nesting when a deep knockout exists.
    pub(super) shapes: usize,
}
impl KnockoutShape {
    /// Bytes of one worker's buffers for `pixels` chunk pixels (16 per snapshot texel, 4 per shape
    /// sample).
    pub(super) fn bytes(&self, pixels: u64) -> Result<u64, RenderError> {
        let snaps = u64::try_from(self.snapshots).map_err(|_| overflow())?;
        let shapes = u64::try_from(self.shapes).map_err(|_| overflow())?;
        snaps
            .checked_mul(16)
            .and_then(|s| shapes.checked_mul(4).and_then(|k| s.checked_add(k)))
            .and_then(|per| per.checked_mul(pixels))
            .ok_or_else(overflow)
    }
}

/// The enclosing buffer contexts of the layer being evaluated, innermost last.
#[derive(Clone, Copy, Debug)]
enum Ctx {
    /// A pass-through group (plan node `group`) holding a shallow knockout: its start snapshot.
    Snapshot { level: usize, group: usize },
    /// An isolated group buffer: its deep-knockout shape plane (`None` when the plan has no deep
    /// knockout or this scratch carries no planes).
    Isolated(Option<usize>),
}

/// Per-worker knockout scratch.
#[derive(Debug, Default)]
pub(super) struct KnockoutScratch {
    snaps: Vec<Vec<[f32; 4]>>,
    shapes: Vec<Vec<f32>>,
    stack: Vec<Ctx>,
}
impl KnockoutScratch {
    pub(super) fn new(shape: KnockoutShape, pixels: usize) -> Self {
        Self {
            snaps: (0..shape.snapshots).map(|_| vec![[0.0; 4]; pixels]).collect(),
            shapes: (0..shape.shapes).map(|_| vec![0.0; pixels]).collect(),
            stack: Vec::new(),
        }
    }

    /// Enter the pass-through group `group` that holds a shallow knockout: snapshot `dst`.
    pub(super) fn push_snapshot(&mut self, dst: &[[f32; 4]], group: usize) -> Result<(), RenderError> {
        let level = self
            .stack
            .iter()
            .filter(|c| matches!(c, Ctx::Snapshot { .. }))
            .count();
        let buf = self
            .snaps
            .get_mut(level)
            .ok_or_else(|| super::unsupported("knockout_scratch"))?;
        let part = buf
            .get_mut(..dst.len())
            .ok_or_else(|| super::unsupported("knockout_scratch"))?;
        part.copy_from_slice(dst);
        self.stack.push(Ctx::Snapshot { level, group });
        Ok(())
    }

    /// Enter an isolated group buffer of `pixels` samples; with `deep`, a zeroed shape plane. A
    /// scratch without planes (an effect, blur or filter frame, where pigment's admission lets no
    /// deep knockout through: plain ancestry) records nothing.
    pub(super) fn push_isolated(&mut self, deep: bool, pixels: usize) {
        let level = self
            .stack
            .iter()
            .filter(|c| matches!(c, Ctx::Isolated(Some(_))))
            .count();
        let plane = match self.shapes.get_mut(level).and_then(|p| p.get_mut(..pixels)) {
            Some(plane) if deep => {
                plane.fill(0.0);
                Some(level)
            }
            _ => None,
        };
        self.stack.push(Ctx::Isolated(plane));
    }

    /// Leave the innermost context (a snapshot or an isolated buffer without deep handling).
    pub(super) fn pop(&mut self) {
        self.stack.pop();
    }

    /// Leave an isolated buffer: clear `dst` (the enclosing running result) by its accumulated deep
    /// shape (pigment `knockout_clear`), and carry the shape on to the next enclosing isolated
    /// buffer (pigment `knockout_union`); at the root there is nothing further to clear.
    pub(super) fn pop_isolated(&mut self, dst: &mut [[f32; 4]]) -> Result<(), RenderError> {
        let Some(Ctx::Isolated(Some(level))) = self.stack.pop() else {
            return Ok(());
        };
        let outer = self
            .stack
            .iter()
            .rev()
            .find_map(|c| match c {
                Ctx::Isolated(p) => Some(*p),
                Ctx::Snapshot { .. } => None,
            })
            .flatten();
        // levels are assigned outward-first, so an outer plane sits before `level`
        if level >= self.shapes.len() {
            return Err(super::unsupported("knockout_scratch"));
        }
        let (before, from) = self.shapes.split_at_mut(level);
        let k_plane = from.first().ok_or_else(overflow)?;
        for (at, px) in dst.iter_mut().enumerate() {
            let k = *k_plane.get(at).ok_or_else(overflow)?;
            if k <= 0.0 {
                continue;
            }
            let out = knockout_clear(Rgba::new([px[0], px[1], px[2]], px[3]), k)
                .map_err(super::blend_error)?;
            *px = [out.rgb[0], out.rgb[1], out.rgb[2], out.a];
        }
        if let Some(o) = outer {
            let target = before.get_mut(o).ok_or_else(overflow)?;
            for (t, k) in target.iter_mut().zip(k_plane.iter()) {
                *t = knockout_union(*t, *k).map_err(super::blend_error)?;
            }
        }
        Ok(())
    }

    /// The initial backdrop of a SHALLOW knockout layer of group `group` at chunk index `at`: the
    /// start snapshot of that group when it is a pass-through group being evaluated, otherwise
    /// transparent (an isolated group's start, or the plan root's transparent base).
    pub(super) fn shallow_initial(&self, group: usize, at: usize) -> Result<Rgba, RenderError> {
        let snapshot = self.stack.iter().rev().find_map(|c| match c {
            Ctx::Snapshot { level, group: g } if *g == group => Some(*level),
            _ => None,
        });
        match snapshot {
            Some(level) => {
                let p = self
                    .snaps
                    .get(level)
                    .and_then(|s| s.get(at))
                    .ok_or_else(overflow)?;
                Ok(Rgba::new([p[0], p[1], p[2]], p[3]))
            }
            _ => Ok(Rgba::TRANSPARENT),
        }
    }

    /// Record the shape of a DEEP knockout pixel into the nearest enclosing isolated buffer's plane
    /// (none at the root level).
    pub(super) fn record_deep(&mut self, at: usize, shape: f32) -> Result<(), RenderError> {
        let level = self.stack.iter().rev().find_map(|c| match c {
            Ctx::Isolated(p) => Some(*p),
            Ctx::Snapshot { .. } => None,
        });
        match level {
            None => Ok(()),
            Some(None) => Err(super::unsupported("knockout_scratch")),
            Some(Some(level)) => {
                let slot = self
                    .shapes
                    .get_mut(level)
                    .and_then(|p| p.get_mut(at))
                    .ok_or_else(overflow)?;
                *slot = knockout_union(*slot, shape).map_err(super::blend_error)?;
                Ok(())
            }
        }
    }
}
