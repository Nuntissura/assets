//! Host side of the extension-envelope operators (pigment `PlanExtensions`): the parameter block
//! the envelope compute entry points read, the Blend-If entries, the adjustment parameters and
//! curve look-up table, and the host-side mask feather blur. Every formula is pigment's
//! (`coverage_math`, `adjust_math`): the WGSL bodies in `kernel.rs` are transcriptions checked by
//! parity tests, and the pieces computed here call pigment's functions directly (the feather kernel,
//! the prepared curve splines) instead of redefining them (CX-CAP-001).
//!
//! The module decodes sizes and coordinates that come from the plan and from caller-owned mask
//! planes, so all integer arithmetic is checked (V4-01).
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::adjust_math::{AdjustmentKind, AdjustmentOp, PreparedCurve};
use hsk_studio_pigment::coverage_math::{
    BlendChannel, BlendRangeEntry, IDENTITY_RANGE, feather_kernel,
};

/// Size of the uniform block `Ext` in `kernel.wgsl`.
pub(crate) const EXT_BYTES: u64 = 288;
/// Samples per channel of the curve look-up table (`lut` in `kernel.wgsl`).
pub(crate) const LUT_POINTS: usize = 4096;
/// Largest sample index of one channel as a float: sample `i` holds the curve at `i / LUT_LAST`.
const LUT_LAST: f32 = 4095.0;
/// Channels of the look-up table: composite, red, green, blue.
const LUT_CHANNELS: usize = 4;
/// Bytes of a full look-up table binding.
pub(crate) const LUT_BYTES: u64 = 4 * 4096 * 4;
/// Bytes of the placeholder binding when no layer uses curves (a binding cannot be empty).
pub(crate) const MIN_LUT_BYTES: u64 = 16;
/// Most Blend-If entries one layer can carry (one per channel).
const MAX_BLEND_IF: usize = 4;
/// Largest `taps x pixels` product one feather blur may cost on the host.
pub(crate) const FEATHER_MAX_OPS: u64 = 1 << 31;

/// Blend-If entries of one layer in plan order (identity entries dropped: their weight is 1).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct BlendIfGpu {
    count: u32,
    channel: [u32; MAX_BLEND_IF],
    src: [[f32; 4]; MAX_BLEND_IF],
    dst: [[f32; 4]; MAX_BLEND_IF],
}

impl BlendIfGpu {
    /// `Ok(None)` when no entry changes anything. `Err(operator key)` for a channel or an entry
    /// count this backend has no encoding for (a typed refusal, not an approximation).
    pub(crate) fn from_entries(entries: &[BlendRangeEntry]) -> Result<Option<Self>, &'static str> {
        let mut out = Self::default();
        for entry in entries {
            if entry.source == IDENTITY_RANGE && entry.destination == IDENTITY_RANGE {
                continue;
            }
            let channel = match entry.channel {
                BlendChannel::Gray => 0,
                BlendChannel::Red => 1,
                BlendChannel::Green => 2,
                BlendChannel::Blue => 3,
                _ => return Err("blend_if_channel"),
            };
            let slot = usize::try_from(out.count).map_err(|_| "blend_if_entries")?;
            let (Some(c), Some(s), Some(d)) = (
                out.channel.get_mut(slot),
                out.src.get_mut(slot),
                out.dst.get_mut(slot),
            ) else {
                return Err("blend_if_entries");
            };
            *c = channel;
            *s = entry.source;
            *d = entry.destination;
            out.count = out.count.checked_add(1).ok_or("blend_if_entries")?;
        }
        Ok((out.count > 0).then_some(out))
    }
}

/// One adjustment compiled for the shader: opcode (position of its kind in
/// `AdjustmentKind::ALL`), flags, parameter block and, for curves, the look-up table.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AdjustGpu {
    pub(crate) opcode: u32,
    pub(crate) flags: u32,
    pub(crate) params: [[f32; 4]; 8],
    pub(crate) lut: Option<Vec<f32>>,
}

/// Compile an adjustment (already validated by pigment's `prepare`) for the shader. `opcode` is the
/// shader opcode of the op's kind. `Err(key)` = no GPU encoding (a typed refusal).
pub(crate) fn compile_adjustment(
    op: &AdjustmentOp,
    opcode: u32,
) -> Result<AdjustGpu, &'static str> {
    let mut out = AdjustGpu {
        opcode,
        flags: 0,
        params: [[0.0; 4]; 8],
        lut: None,
    };
    match op {
        AdjustmentOp::Invert => {}
        AdjustmentOp::Levels(l) => {
            // Record order composite, red, green, blue: (input black, input white, gamma, output
            // black) in vec4 2r, output white in the x of vec4 2r + 1.
            let [p0, p1, p2, p3, p4, p5, p6, p7] = &mut out.params;
            for (lo, hi, r) in [
                (p0, p1, &l.composite),
                (p2, p3, &l.red),
                (p4, p5, &l.green),
                (p6, p7, &l.blue),
            ] {
                *lo = [r.input_black, r.input_white, r.gamma, r.output_black];
                *hi = [r.output_white, 0.0, 0.0, 0.0];
            }
        }
        AdjustmentOp::Curves(c) => {
            let mut table = vec![0.0f32; LUT_CHANNELS.checked_mul(LUT_POINTS).ok_or("curves_lut")?];
            for (bit, (points, samples)) in [&c.composite, &c.red, &c.green, &c.blue]
                .into_iter()
                .zip(table.chunks_exact_mut(LUT_POINTS))
                .enumerate()
            {
                if points.is_empty() {
                    continue;
                }
                let curve = PreparedCurve::new(points).map_err(|_| "curves_points")?;
                for (i, slot) in samples.iter_mut().enumerate() {
                    *slot = curve.eval(i as f32 / LUT_LAST);
                }
                out.flags |= 1u32
                    .checked_shl(u32::try_from(bit).map_err(|_| "curves_lut")?)
                    .ok_or("curves_lut")?;
            }
            out.lut = Some(table);
        }
        AdjustmentOp::HueSaturation(h) => {
            // pigment: hue = (degrees / 360).rem_euclid(1), saturation and lightness in percent / 100
            let hue = (f64::from(h.hue) / 360.0).rem_euclid(1.0);
            let sat = f64::from(h.saturation) / 100.0;
            let light = f64::from(h.lightness) / 100.0;
            out.params[0] = [
                hue as f32,
                sat as f32,
                light as f32,
                if h.colorize { 1.0 } else { 0.0 },
            ];
        }
        AdjustmentOp::Exposure(e) => {
            out.params[0] = [2f32.powf(e.exposure), e.offset, e.gamma, e.linearize_gamma];
        }
        AdjustmentOp::Threshold { level } => out.params[0] = [*level, 0.0, 0.0, 0.0],
        AdjustmentOp::Posterize { levels } => out.params[0] = [f32::from(*levels), 0.0, 0.0, 0.0],
        _ => return Err("adjustment_kind"),
    }
    Ok(out)
}

/// Kinds the shader must know by opcode: the position of `kind` in `AdjustmentKind::ALL`.
pub(crate) fn opcode_of(kind: AdjustmentKind) -> Option<u32> {
    AdjustmentKind::ALL
        .iter()
        .position(|k| *k == kind)
        .and_then(|i| u32::try_from(i).ok())
}

/// The uniform block (`Ext` in `kernel.wgsl`) of one dispatch.
pub(crate) fn ext_bytes(blend_if: Option<&BlendIfGpu>, adjust: Option<&AdjustGpu>) -> [u8; 288] {
    let mut out = [0u8; 288];
    let mut put = |at: usize, bytes: [u8; 4]| {
        if let Some(slot) = out.get_mut(at..at.saturating_add(4)) {
            slot.copy_from_slice(&bytes);
        }
    };
    let head = [
        blend_if.map_or(0, |b| b.count),
        adjust.map_or(0, |a| a.opcode),
        adjust.map_or(0, |a| a.flags),
        0,
    ];
    for (i, w) in head.iter().enumerate() {
        put(i.saturating_mul(4), w.to_ne_bytes());
    }
    if let Some(b) = blend_if {
        for (i, w) in b.channel.iter().enumerate() {
            put(16usize.saturating_add(i.saturating_mul(4)), w.to_ne_bytes());
        }
        for (i, rows) in [b.src, b.dst].iter().enumerate() {
            for (r, row) in rows.iter().enumerate() {
                for (c, v) in row.iter().enumerate() {
                    let at = 32usize
                        .saturating_add(i.saturating_mul(64))
                        .saturating_add(r.saturating_mul(16))
                        .saturating_add(c.saturating_mul(4));
                    put(at, v.to_ne_bytes());
                }
            }
        }
    }
    if let Some(a) = adjust {
        for (r, row) in a.params.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let at = 160usize
                    .saturating_add(r.saturating_mul(16))
                    .saturating_add(c.saturating_mul(4));
                put(at, v.to_ne_bytes());
            }
        }
    }
    out
}

/// Why a feather blur was not computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FeatherError {
    /// The radius is outside pigment's range.
    Radius,
    /// The scratch the blur needs exceeds the working-byte limit.
    Budget { requested: u64 },
    /// `taps x pixels` exceeds [`FEATHER_MAX_OPS`].
    Cost,
    /// A size or coordinate overflowed.
    Overflow,
}

/// Blurred coverage of `region` (row-major `f32`), computed from `raw` (the plane's coverage
/// before inversion and density) with pigment's separable Gaussian taps; samples outside the plane
/// are clamped to its border, so the result equals blurring the whole plane.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FeatherPlane {
    pub(crate) region: Rect,
    data: Vec<f32>,
}

impl FeatherPlane {
    /// Bytes the blurred plane holds.
    pub(crate) fn bytes(&self) -> u64 {
        u64::try_from(self.data.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(4)
    }
    /// Blurred coverage at an absolute pixel of the region; a pixel beyond the region (frames of a
    /// plan with layer effects reach past the target) reads the nearest region texel.
    pub(crate) fn at(&self, x: i64, y: i64) -> f32 {
        let last_x = i64::from(self.region.width.saturating_sub(1));
        let last_y = i64::from(self.region.height.saturating_sub(1));
        let (Some(dx), Some(dy)) = (
            x.checked_sub(self.region.x).map(|v| v.clamp(0, last_x)),
            y.checked_sub(self.region.y).map(|v| v.clamp(0, last_y)),
        ) else {
            return 0.0;
        };
        let (Ok(dx), Ok(dy), Ok(w)) = (
            usize::try_from(dx),
            usize::try_from(dy),
            usize::try_from(self.region.width),
        ) else {
            return 0.0;
        };
        dy.checked_mul(w)
            .and_then(|row| row.checked_add(dx))
            .and_then(|i| self.data.get(i))
            .copied()
            .unwrap_or(0.0)
    }
}

fn clamp_at(base: i64, tap: usize, half: i64, lo: i64, hi: i64) -> Option<i64> {
    Some(
        base.checked_add(i64::try_from(tap).ok()?)?
            .checked_sub(half)?
            .clamp(lo, hi),
    )
}

/// Blur `region` of a mask plane covering `plane` (the region lies inside the plane). `limit` is the
/// working-byte limit the host scratch (the horizontal-pass rows plus the output) is charged to.
pub(crate) fn feather_plane(
    radius_px: f32,
    region: Rect,
    plane: Rect,
    limit: u64,
    raw: impl Fn(i64, i64) -> f32,
) -> Result<FeatherPlane, FeatherError> {
    let taps = feather_kernel(radius_px).map_err(|_| FeatherError::Radius)?;
    let tap_count = u64::try_from(taps.len()).map_err(|_| FeatherError::Overflow)?;
    let half =
        i64::try_from(taps.len().saturating_sub(1) / 2).map_err(|_| FeatherError::Overflow)?;
    let (x1, y1) = region.end().map_err(|_| FeatherError::Overflow)?;
    let (px1, py1) = plane.end().map_err(|_| FeatherError::Overflow)?;
    let (px_last, py_last) = (
        px1.checked_sub(1).ok_or(FeatherError::Overflow)?,
        py1.checked_sub(1).ok_or(FeatherError::Overflow)?,
    );
    // rows of the plane the vertical pass can reach
    let ya = region
        .y
        .checked_sub(half)
        .ok_or(FeatherError::Overflow)?
        .max(plane.y);
    let yb = y1
        .checked_sub(1)
        .and_then(|v| v.checked_add(half))
        .ok_or(FeatherError::Overflow)?
        .min(py_last);
    let rows = yb
        .checked_sub(ya)
        .and_then(|v| v.checked_add(1))
        .and_then(|v| u64::try_from(v).ok())
        .ok_or(FeatherError::Overflow)?;
    let (w, h) = (u64::from(region.width), u64::from(region.height));
    let tmp_len = rows.checked_mul(w).ok_or(FeatherError::Overflow)?;
    let out_len = w.checked_mul(h).ok_or(FeatherError::Overflow)?;
    let bytes = tmp_len
        .checked_add(out_len)
        .and_then(|n| n.checked_mul(4))
        .ok_or(FeatherError::Overflow)?;
    if bytes > limit {
        return Err(FeatherError::Budget { requested: bytes });
    }
    let ops = tmp_len
        .checked_add(out_len)
        .and_then(|n| n.checked_mul(tap_count))
        .ok_or(FeatherError::Overflow)?;
    if ops > FEATHER_MAX_OPS {
        return Err(FeatherError::Cost);
    }
    let tmp_len = usize::try_from(tmp_len).map_err(|_| FeatherError::Overflow)?;
    let out_len = usize::try_from(out_len).map_err(|_| FeatherError::Overflow)?;
    let width = usize::try_from(region.width).map_err(|_| FeatherError::Overflow)?;

    // horizontal pass over the reachable rows, only for the output columns
    let mut tmp = Vec::new();
    tmp.try_reserve_exact(tmp_len)
        .map_err(|_| FeatherError::Budget { requested: bytes })?;
    let mut y = ya;
    while y <= yb {
        let mut x = region.x;
        while x < x1 {
            let mut acc = 0.0f64;
            for (k, t) in taps.iter().enumerate() {
                let sx = clamp_at(x, k, half, plane.x, px_last).ok_or(FeatherError::Overflow)?;
                acc += f64::from(*t) * f64::from(raw(sx, y));
            }
            tmp.push(acc as f32);
            x = x.checked_add(1).ok_or(FeatherError::Overflow)?;
        }
        y = y.checked_add(1).ok_or(FeatherError::Overflow)?;
    }
    // vertical pass
    let mut data = Vec::new();
    data.try_reserve_exact(out_len)
        .map_err(|_| FeatherError::Budget { requested: bytes })?;
    let mut y = region.y;
    while y < y1 {
        for col in 0..width {
            let mut acc = 0.0f64;
            for (k, t) in taps.iter().enumerate() {
                let sy = clamp_at(y, k, half, plane.y, py_last).ok_or(FeatherError::Overflow)?;
                let row = usize::try_from(sy.checked_sub(ya).ok_or(FeatherError::Overflow)?)
                    .map_err(|_| FeatherError::Overflow)?;
                let at = row
                    .checked_mul(width)
                    .and_then(|v| v.checked_add(col))
                    .ok_or(FeatherError::Overflow)?;
                acc += f64::from(*t) * f64::from(*tmp.get(at).ok_or(FeatherError::Overflow)?);
            }
            data.push(acc as f32);
        }
        y = y.checked_add(1).ok_or(FeatherError::Overflow)?;
    }
    Ok(FeatherPlane { region, data })
}
