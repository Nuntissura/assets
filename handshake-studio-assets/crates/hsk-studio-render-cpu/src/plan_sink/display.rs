//! Declared display transform for scene-referred sources (lane SMB-RENDER-SCENE-DISPLAY).
//!
//! Without a declaration a tile colour sample outside `[0, 1]` stays the typed refusal
//! `sample_range_outside_unit` (pigment's `SAMPLE_RANGE_OPERATOR`), exactly as before. A renderer
//! built with [`super::Renderer::with_display_transform`]`(DisplayTransform::Clip)` instead clips
//! every colour sample of a verified tile into `[0, 1]` (`f32::clamp`, alpha untouched) and
//! records [`DISPLAY_CLIP_OPERATOR`] in the receipt's exactness ledger whenever a sample was
//! clipped.
//!
//! WHERE the clip applies: at the source boundary, not after compositing. The spec places the
//! display transform after compositing (STU-COL-161 stage 6a), but pigment's blend arithmetic,
//! the single owner (CX-CAP-001), is defined on `[0, 1]` only and clamps every result, so an
//! extended-range composite does not exist to be clipped. Clipping the sources equals a stage-6a
//! clip of an extended-range composite when every clipped sample reaches the output through
//! opaque Normal source-over (a developed raw layer shown alone, the viewer case) and differs
//! otherwise; hence class Approximate, and a request whose `min_exactness` is above Approximate
//! is refused with this key. No tone-map curve is offered: Prism, the colour-maths owner, has
//! none (lane notes), so render-cpu does not invent one.
//!
//! Non-finite samples and alpha outside `[0, 1]` keep their source refusals (`source_nonfinite`,
//! `source_alpha_range`); `Solid` colours outside `[0, 1]` keep pigment's admission refusal.
use super::{
    RenderError, ScratchLease, f32_at, reachable, source, verify_tile,
};
use crate::px::{add_u, mul_u, overflow};
use hsk_studio_pigment::render_plan::{
    BackendError, ColourTag, Exactness, ExactnessLedger, PlanNode, PlanSources, RenderPlan,
    SourceKey, TilePlanes,
};

/// Ledger and refusal key of [`DisplayTransform::Clip`] (source-side clip, see the module docs).
pub const DISPLAY_CLIP_OPERATOR: &str = "display_clip_sources";
/// Class of [`DisplayTransform::Clip`] against a stage-6a clip of an extended-range composite.
pub const DISPLAY_CLIP_EXACTNESS: Exactness = Exactness::Approximate;

/// A display transform a host declares for scene-referred values outside `[0, 1]`.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DisplayTransform {
    /// Clip every tile colour sample into `[0, 1]` before compositing.
    Clip,
}
impl DisplayTransform {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Clip => DISPLAY_CLIP_OPERATOR,
        }
    }
}

/// One tile whose colour plane holds a sample outside `[0, 1]`, verified against the plan.
struct Wanted {
    key: SourceKey,
    width: u32,
    height: u32,
}
/// A clipped, tightly packed copy of a verified colour plane.
struct Clipped {
    key: SourceKey,
    colour: Vec<u8>,
}

/// Clipped colour planes of a run; borrowed by `prepare` so the clipped planes outlive it.
#[derive(Default)]
pub(crate) struct ClipStore {
    planes: Vec<Clipped>,
    bytes: u64,
}

impl ClipStore {
    /// Bytes held by the clipped planes (charged to `max_working_bytes` before allocation).
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Verify every reachable tile (the same `verify_tile` `prepare` runs, so a bad source keeps
    /// its key), charge the clipped copies with `charge` before allocating them, then copy and
    /// clip. Returns the store and the lease of its bytes. `ledger` gains the operator when
    /// anything was clipped; a request floor above its class is refused before any allocation.
    pub(crate) fn build<'r, V>(
        plan: &RenderPlan<V>,
        sources: &dyn PlanSources,
        space: ColourTag,
        min_exactness: Exactness,
        ledger: &mut ExactnessLedger,
        charge: impl FnOnce(u64) -> Result<ScratchLease<'r>, RenderError>,
    ) -> Result<(Self, ScratchLease<'r>), RenderError> {
        let reach = reachable(plan);
        let mut wanted: Vec<Wanted> = Vec::new();
        let mut bytes = 0u64;
        for (i, node) in plan.nodes.iter().enumerate() {
            let PlanNode::Tile(t) = node else { continue };
            if !reach.get(i).copied().unwrap_or(false) || wanted.iter().any(|w| w.key == t.key) {
                continue;
            }
            let planes = sources
                .tile(t.key)
                .ok_or(BackendError::UnresolvedSource(t.key))?;
            if verify_tile(t, &planes, space, true)? {
                bytes = u64::from(planes.width)
                    .checked_mul(u64::from(planes.height))
                    .and_then(|p| p.checked_mul(12))
                    .and_then(|b| bytes.checked_add(b))
                    .ok_or_else(overflow)?;
                wanted.push(Wanted {
                    key: t.key,
                    width: planes.width,
                    height: planes.height,
                });
            }
        }
        if !wanted.is_empty() {
            if DISPLAY_CLIP_EXACTNESS.rank() > min_exactness.rank() {
                return Err(RenderError::Backend(
                    BackendError::Unsupported {
                        operator: DISPLAY_CLIP_OPERATOR,
                        exactness: DISPLAY_CLIP_EXACTNESS,
                    },
                ));
            }
            ledger.record(DISPLAY_CLIP_OPERATOR, DISPLAY_CLIP_EXACTNESS);
        }
        let lease = charge(bytes)?;
        let mut planes = Vec::with_capacity(wanted.len());
        for w in wanted {
            let tile = sources
                .tile(w.key)
                .ok_or(BackendError::UnresolvedSource(w.key))?;
            if tile.width != w.width || tile.height != w.height {
                return Err(source(w.key, super::SourceFault::Declared));
            }
            planes.push(Clipped {
                key: w.key,
                colour: clip_plane(&tile)?,
            });
        }
        Ok((Self { planes, bytes }, lease))
    }

    /// `original` with its colour plane replaced by the clipped copy of `key`. The copy was taken
    /// from bytes verified against the same declaration, so the declared hashes stay the source's.
    pub(crate) fn substitute<'a>(
        &'a self,
        key: SourceKey,
        original: TilePlanes<'a>,
    ) -> Result<TilePlanes<'a>, RenderError> {
        let Some(c) = self.planes.iter().find(|c| c.key == key) else {
            // The source answered in range for the store and out of range for `prepare`.
            return Err(source(key, super::SourceFault::HashMismatch));
        };
        let stride = u64::from(original.width)
            .checked_mul(12)
            .ok_or_else(overflow)?;
        Ok(TilePlanes {
            colour: &c.colour,
            colour_stride_bytes: stride,
            ..original
        })
    }
}

/// Tightly packed copy of `planes`' colour with every sample clipped into `[0, 1]`.
fn clip_plane(planes: &TilePlanes<'_>) -> Result<Vec<u8>, RenderError> {
    let (w, h) = (
        crate::px::to_usize(u64::from(planes.width))?,
        crate::px::to_usize(u64::from(planes.height))?,
    );
    let len = mul_u(mul_u(w, h)?, 12)?;
    let mut out = Vec::with_capacity(len);
    let stride = crate::px::to_usize(planes.colour_stride_bytes)?;
    for row in 0..h {
        let start = mul_u(row, stride)?;
        for column in 0..w {
            let at = add_u(start, mul_u(column, 12)?)?;
            for channel in 0..3usize {
                let v = f32_at(planes.colour, add_u(at, mul_u(channel, 4)?)?)?;
                out.extend_from_slice(&v.clamp(0.0, 1.0).to_le_bytes());
            }
        }
    }
    Ok(out)
}
