//! Declared display transform for scene-referred sources (lane SMB-RENDER-SCENE-DISPLAY), the GPU
//! twin of render-cpu `plan_sink/display.rs` (same key, class and clip; render-gpu does not depend
//! on render-cpu).
//!
//! Without a declaration a tile colour sample outside `[0, 1]` stays the typed refusal
//! `sample_range_outside_unit`. With [`super::GpuBackend::with_display_transform`]`(Clip)` every
//! colour sample of a verified tile is clipped into `[0, 1]` with `f32::clamp` ON THE HOST, before
//! upload, so the device receives exactly the bytes render-cpu composites; `display_clip_sources`
//! (Approximate) is recorded in the receipt ledger when anything was clipped. The clip applies at
//! the source boundary because pigment's blend arithmetic is defined on `[0, 1]` only (see the
//! render-cpu module for the stage-6a reading). The clipped copies are host bytes, checked against
//! `max_working_bytes` before allocation; what remains of that limit goes to the host-side
//! feather scratch.
use super::{mismatch, overflow, plane_ok, verify_tile};
use crate::error::GpuError;
use hsk_studio_pigment::render_plan::{
    BackendError, ColourTag, Exactness, ExactnessLedger, PlanNode, PlanSources, RenderPlan,
    SourceKey, TilePlanes,
};

/// Ledger and refusal key of [`DisplayTransform::Clip`] (equal to render-cpu's).
pub const DISPLAY_CLIP_OPERATOR: &str = "display_clip_sources";
/// Class of [`DisplayTransform::Clip`] (equal to render-cpu's).
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

struct Clipped {
    key: SourceKey,
    colour: Vec<u8>,
}

/// Clipped colour planes of one execution; borrowed by `prepare`.
#[derive(Default)]
pub(crate) struct ClipStore {
    planes: Vec<Clipped>,
    bytes: u64,
}

impl ClipStore {
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Verify every reachable tile (`verify_tile`, as `prepare` does), check the clipped copies
    /// against `limit` before allocating them, then copy and clip. `ledger` gains the operator when
    /// anything was clipped; a request floor above its class is refused first.
    pub(crate) fn build<V>(
        plan: &RenderPlan<V>,
        sources: &dyn PlanSources,
        space: ColourTag,
        min_exactness: Exactness,
        ledger: &mut ExactnessLedger,
        limit: u64,
    ) -> Result<Self, GpuError> {
        let n = plan.nodes.len();
        let mut reach = vec![false; n];
        if let Some(r) = reach.get_mut(plan.root.0 as usize) {
            *r = true;
        }
        for i in (0..n).rev() {
            if reach[i]
                && let PlanNode::Group(g) = &plan.nodes[i]
            {
                for l in &g.layers {
                    if let Some(r) = reach.get_mut(l.input.0 as usize) {
                        *r = true;
                    }
                }
            }
        }
        let mut wanted: Vec<(SourceKey, u32, u32)> = Vec::new();
        let mut bytes = 0u64;
        for (i, node) in plan.nodes.iter().enumerate() {
            let PlanNode::Tile(t) = node else { continue };
            if !reach[i] || wanted.iter().any(|w| w.0 == t.key) {
                continue;
            }
            let planes = sources
                .tile(t.key)
                .ok_or(GpuError::Backend(BackendError::UnresolvedSource(t.key)))?;
            if verify_tile(t, &planes, space, true)? {
                bytes = u64::from(planes.width)
                    .checked_mul(u64::from(planes.height))
                    .and_then(|p| p.checked_mul(12))
                    .and_then(|b| bytes.checked_add(b))
                    .ok_or_else(overflow)?;
                wanted.push((t.key, planes.width, planes.height));
            }
        }
        if !wanted.is_empty() {
            if DISPLAY_CLIP_EXACTNESS.rank() > min_exactness.rank() {
                return Err(GpuError::Backend(BackendError::Unsupported {
                    operator: DISPLAY_CLIP_OPERATOR,
                    exactness: DISPLAY_CLIP_EXACTNESS,
                }));
            }
            ledger.record(DISPLAY_CLIP_OPERATOR, DISPLAY_CLIP_EXACTNESS);
        }
        if bytes > limit {
            return Err(GpuError::BudgetExceeded {
                requested: bytes,
                budget: limit,
            });
        }
        let mut planes = Vec::with_capacity(wanted.len());
        for (key, width, height) in wanted {
            let tile = sources
                .tile(key)
                .ok_or(GpuError::Backend(BackendError::UnresolvedSource(key)))?;
            if tile.width != width || tile.height != height {
                return Err(mismatch(key));
            }
            planes.push(Clipped {
                key,
                colour: clip_plane(&tile, key)?,
            });
        }
        Ok(Self { planes, bytes })
    }

    /// `original` with its colour plane replaced by the clipped copy of `key` (taken from bytes
    /// verified against the same declaration; the declared hashes stay the source's).
    pub(crate) fn substitute<'a>(
        &'a self,
        key: SourceKey,
        original: TilePlanes<'a>,
    ) -> Result<TilePlanes<'a>, GpuError> {
        let Some(c) = self.planes.iter().find(|c| c.key == key) else {
            return Err(mismatch(key));
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

/// Tightly packed copy of the colour plane with every sample clipped into `[0, 1]`.
fn clip_plane(planes: &TilePlanes<'_>, key: SourceKey) -> Result<Vec<u8>, GpuError> {
    if !plane_ok(
        planes.colour,
        planes.colour_stride_bytes,
        planes.width,
        planes.height,
        12,
    ) {
        return Err(mismatch(key));
    }
    let width = usize::try_from(planes.width).map_err(|_| overflow())?;
    let height = usize::try_from(planes.height).map_err(|_| overflow())?;
    let stride = usize::try_from(planes.colour_stride_bytes).map_err(|_| overflow())?;
    let row_bytes = width.checked_mul(12).ok_or_else(overflow)?;
    let len = row_bytes.checked_mul(height).ok_or_else(overflow)?;
    let mut out = Vec::with_capacity(len);
    for row in 0..height {
        let start = row.checked_mul(stride).ok_or_else(overflow)?;
        let end = start.checked_add(row_bytes).ok_or_else(overflow)?;
        let bytes = planes.colour.get(start..end).ok_or_else(|| mismatch(key))?;
        for word in bytes.chunks_exact(4) {
            let v = f32::from_le_bytes([word[0], word[1], word[2], word[3]]);
            out.extend_from_slice(&v.clamp(0.0, 1.0).to_le_bytes());
        }
    }
    Ok(out)
}
