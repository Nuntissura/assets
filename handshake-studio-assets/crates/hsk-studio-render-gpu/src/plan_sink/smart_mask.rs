//! Smart-filter masks on the GPU backend (lane SMB-PS-ADJUST-STYLES-3; STU-RAS-037 per-filter
//! masking, STU-FX-004): the per-entry masks and the stack mask of an envelope
//! `LayerExtension::smart_filters` entry. Each mask source is checked like render-cpu checks it
//! (declared format `coverage_u8` / `coverage_u16le`, density 1, the plane's format, hash, bounds and
//! stride) and decoded once at prepare into pigment's coverage scale (`u8 * 257`, inverted
//! `65535 - c`, zero outside its bounds), charged to the working-byte limit. Every entry with a mask
//! (its own or the stack's) mixes through `filter_mix_masked` with the per-texel weight
//! `opacity * entry mask * stack mask` of pigment `SmartFilterStack::render`, computed on the host in
//! binary64 and rounded once into the `Exec::mask` plane of the frame; the mix itself is the
//! unmasked mix's binary32 arithmetic. Parity with pigment's stack is tested in
//! tests/gpu_smart_filter_masks.rs. Photoshop parity UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use super::{Exec, FilterBuf, overflow, unsupported};
use crate::error::GpuError;
use crate::kernel::{FilterOp, filter_params_bytes};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{BackendError, MaskFormat, MaskInput, PlanError, PlanSources};

/// One decoded smart-filter mask: pigment coverage (`0..=65535`) over `bounds`, row-major.
#[derive(Debug)]
pub(super) struct CovPlane {
    bounds: Rect,
    data: Vec<u16>,
}

impl CovPlane {
    /// Coverage at plan pixel `(x, y)` in `[0, 1]` (binary64, pigment's `c / 65535`), zero outside.
    fn at(&self, x: i64, y: i64) -> f64 {
        let (Some(dx), Some(dy)) = (x.checked_sub(self.bounds.x), y.checked_sub(self.bounds.y)) else {
            return 0.0;
        };
        let (Ok(dx), Ok(dy)) = (u32::try_from(dx), u32::try_from(dy)) else {
            return 0.0;
        };
        if dx >= self.bounds.width || dy >= self.bounds.height {
            return 0.0;
        }
        let at = u64::from(dy)
            .checked_mul(u64::from(self.bounds.width))
            .and_then(|r| r.checked_add(u64::from(dx)))
            .and_then(|a| usize::try_from(a).ok());
        at.and_then(|a| self.data.get(a)).map_or(0.0, |c| f64::from(*c) / 65535.0)
    }
}

fn parameter(node: u32) -> GpuError {
    GpuError::Backend(BackendError::InvalidPlan(PlanError::SmartFilterParameter { node }))
}

/// Decodes the mask source `m` (render-cpu's `mask_view` checks), charging its bytes to `budget`.
pub(super) fn resolve(sources: &dyn PlanSources, m: &MaskInput, node: u32, budget: &mut u64) -> Result<CovPlane, GpuError> {
    let wide = match m.format {
        MaskFormat::CoverageU8 => false,
        MaskFormat::CoverageU16Le => true,
        _ => return Err(parameter(node)),
    };
    if m.density != 1.0 {
        return Err(parameter(node));
    }
    let plane = sources.mask(m.key).ok_or(GpuError::Backend(BackendError::UnresolvedSource(m.key)))?;
    if plane.sample_format != m.format.key() || plane.sha256 != m.sha256 || plane.bounds != m.placement {
        return Err(GpuError::Backend(BackendError::SourceMismatch(m.key)));
    }
    plane.validate(plane.bounds).map_err(|_| GpuError::Backend(BackendError::SourceMismatch(m.key)))?;
    let (w, h) = (plane.bounds.width, plane.bounds.height);
    let count = u64::from(w).checked_mul(u64::from(h)).ok_or_else(overflow)?;
    let bytes = count.checked_mul(2).ok_or_else(overflow)?;
    *budget = budget.checked_sub(bytes).ok_or(GpuError::BudgetExceeded { requested: bytes, budget: *budget })?;
    let stride = usize::try_from(plane.stride_bytes).map_err(|_| overflow())?;
    let mut data = Vec::new();
    data.try_reserve_exact(usize::try_from(count).map_err(|_| overflow())?).map_err(|_| unsupported("smart_filter_mask_allocation"))?;
    let size: usize = if wide { 2 } else { 1 };
    for row in 0..usize::try_from(h).map_err(|_| overflow())? {
        let start = row.checked_mul(stride).ok_or_else(overflow)?;
        for col in 0..usize::try_from(w).map_err(|_| overflow())? {
            let at = col.checked_mul(size).and_then(|c| start.checked_add(c)).ok_or_else(overflow)?;
            let stored = if wide {
                let end = at.checked_add(2).ok_or_else(overflow)?;
                match plane.bytes.get(at..end) {
                    Some(&[lo, hi]) => u16::from_le_bytes([lo, hi]),
                    _ => return Err(overflow()),
                }
            } else {
                u16::from(*plane.bytes.get(at).ok_or_else(overflow)?).saturating_mul(257)
            };
            data.push(if m.inverted { u16::MAX.saturating_sub(stored) } else { stored });
        }
    }
    Ok(CovPlane { bounds: plane.bounds, data })
}

impl Exec<'_> {
    /// Uploads the weight `opacity * entry * stack` (pigment's `t`, binary64 rounded once) of every
    /// texel of the frame `chunk` into `Exec::mask`.
    pub(super) fn upload_weights(
        &mut self,
        chunk: Rect,
        opacity: f32,
        entry: Option<&CovPlane>,
        stack: Option<&CovPlane>,
    ) -> Result<(), GpuError> {
        self.mask_upload.clear();
        for y in 0..i64::from(chunk.height) {
            for x in 0..i64::from(chunk.width) {
                let (px, py) = (chunk.x.saturating_add(x), chunk.y.saturating_add(y));
                let e = entry.map_or(1.0, |p| p.at(px, py));
                let s = stack.map_or(1.0, |p| p.at(px, py));
                let t = f64::from(opacity) * e * s;
                self.mask_upload.extend_from_slice(&(t as f32).to_ne_bytes());
            }
        }
        self.ctx.queue.write_buffer(&self.mask, 0, &self.mask_upload);
        self.stats.bytes_uploaded = self.stats.bytes_uploaded.saturating_add(self.mask_upload.len() as u64);
        Ok(())
    }

    /// The masked mix (`filter_mix_masked`) over the frame `chunk`: `dst = mix(src, aux, weight)`
    /// inside the frame-relative `rect`, transparent outside, the weight read from `Exec::mask`.
    pub(super) fn filter_mix_masked(
        &mut self,
        chunk: Rect,
        rect: (i32, i32, u32, u32),
        src: FilterBuf,
        dst: FilterBuf,
        aux: FilterBuf,
    ) -> Result<(), GpuError> {
        let ctx = self.ctx;
        let Some(bb) = self.blur.as_ref() else {
            return Err(unsupported("plan_shape"));
        };
        let pick = |b: FilterBuf| match b {
            FilterBuf::Level(l) => self.levels.get(l).ok_or_else(|| unsupported("plan_shape")),
            FilterBuf::A => Ok(&bb.a),
            FilterBuf::B => Ok(&bb.b),
        };
        let (src, dst, aux) = (pick(src)?, pick(dst)?, pick(aux)?);
        let bytes = filter_params_bytes(FilterOp::Mix { t: 0.0, rect }, chunk.width, chunk.height, self.extended);
        ctx.queue.write_buffer(&bb.params, 0, &bytes);
        let bind = ctx.kernel.bind_filter_w(&ctx.device, &bb.params, [src, dst, aux, &self.mask]);
        let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("hsk-studio-render-gpu masked mix"),
        });
        ctx.kernel.dispatch_filter_w(&mut encoder, &bind, chunk.width, chunk.height);
        ctx.queue.submit(Some(encoder.finish()));
        self.stats.dispatches = self.stats.dispatches.saturating_add(1);
        Ok(())
    }
}
