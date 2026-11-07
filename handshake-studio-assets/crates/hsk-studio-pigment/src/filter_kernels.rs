//! Raster filter kernels of the live filter stack (lane SMB-PIGMENT-FILTER-KERNELS-1; spec basis
//! STU-RAS-037, STU-FX-013, STU-FX-015, STU-FX-016, STU-FX-019, STU-FX-020, STU-FX-022). Every kind
//! maps a whole straight-RGBA plane to a straight-RGBA plane of the same bounds; they reach documents
//! as [`crate::smart_filter::StackFilter::Kernel`] and carry no window contract, so they stay out of
//! [`crate::filter_math::LayerFilter`] and its GPU parity. render-gpu refuses them with a typed
//! Unsupported and the host falls back to render-cpu; no promotion equivalence is claimed
//! (STU-FX-130).
//!
//! Formulas are Studio readings, class Approximate, vendor parity UNVERIFIED (D5); [`DESCRIPTOR`]
//! lists each one, where its bound comes from and what is not modelled. Unit range only
//! (STU-FX-006a): the stack refuses extended range already.
//!
//! Plane dependence: a kernel sees the plane it is given, and render-cpu gives it the content bounds
//! cut to the plan extent. So the mosaic grid moves with the extent and any neighbourhood kind differs
//! near a cropped edge (test `tests/filter_kernels.rs`
//! `item3_a_cropped_plane_moves_the_mosaic_grid_and_the_replicated_border`). Extent independence is
//! not claimed.
//!
//! Determinism: binary64 sums in a fixed order. `bilateral_blur` calls `exp` and every angled kind
//! calls `sin_cos` from the platform libm, so their bytes are stable per platform only.
//!
//! Hostile inputs (V4-01/02): parameters are checked, never clamped (STU-FX-104); where the source
//! declares no bound none is invented (STU-FX-106) and the work budget stops the kernel. Bytes and
//! work are computed with checked arithmetic and refused against [`FilterLimits`] before allocating;
//! no cost walks more than `O(radius)` for a radius with a hard bound, and the unbounded radii cost
//! `O(1)` (test `item5_unbounded_radii_are_refused_quickly`).
#![deny(clippy::arithmetic_side_effects)]
use crate::Rect;
use crate::blur_math::{
    BLUR_SAMPLE_BYTES, BlurAxis, BlurLimits, MAX_BOX_KERNEL_TAPS, RadialBlurKind, bilinear_add, blur_window_axis, blur_window_axis_cost,
    blur_window_cost, blur_window_in, box_kernel, cos_sin_deg, directional_blur_window, directional_blur_window_cost, radial_blur_window,
    radial_blur_window_cost, unpremultiply_in,
};
use crate::filter_math::{FilterError, FilterLimits, check, disc, disc_len, find_edges_window, find_edges_window_cost, sobel_magnitude};
use crate::render_plan::SampleRange;
use crate::retouch::RgbaPlane;
use serde::Serialize;

const UNIT: SampleRange = SampleRange::Unit;
/// Bytes of one premultiplied binary64 sample.
const PM64_BYTES: u64 = 32;
/// Convolution tap reads per pixel.
const CONV_READS: u64 = 25;
/// Sobel reads per pixel.
const SOBEL_READS: u64 = 9;
/// Line reads and writes per sample of the separable morphology: padded line up to 3 long, 3 buffers.
const MORPH_READS: u64 = 9;
/// Sobel magnitude that counts as an edge for `sharpen_edges` and `despeckle` (the largest of the
/// three colour channels; a Studio reading, not a source value).
pub const EDGE_GATE: f64 = 0.1;
/// Rec. 709 luma weights of `emboss`.
const LUMA: [f64; 3] = [0.2126, 0.7152, 0.0722];

const SHARPEN: Convolution = Convolution {
    weights: [0, 0, 0, 0, 0, 0, 0, -1, 0, 0, 0, -1, 5, -1, 0, 0, 0, -1, 0, 0, 0, 0, 0, 0, 0],
    scale: 1,
    offset: 0,
};
const SHARPEN_MORE: Convolution = Convolution {
    weights: [0, 0, 0, 0, 0, 0, -1, -1, -1, 0, 0, -1, 9, -1, 0, 0, -1, -1, -1, 0, 0, 0, 0, 0, 0],
    scale: 1,
    offset: 0,
};
const BASIC_BLUR: Convolution = Convolution {
    weights: [0, 0, 0, 0, 0, 0, 1, 2, 1, 0, 0, 2, 4, 2, 0, 0, 1, 2, 1, 0, 0, 0, 0, 0, 0],
    scale: 16,
    offset: 0,
};
const BLUR_MORE: Convolution = Convolution {
    weights: [1, 2, 2, 2, 1, 2, 4, 4, 4, 2, 2, 4, 4, 4, 2, 2, 4, 4, 4, 2, 1, 2, 2, 2, 1],
    scale: 64,
    offset: 0,
};

/// Surface description (Studio readings; vendor parity NOT_PROVEN, D5).
pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PIGMENT","surface":"filter_kernels","version":1,"lane":"SMB-PIGMENT-FILTER-KERNELS-1","spec_basis":["STU-RAS-037","STU-FX-013","STU-FX-015","STU-FX-016","STU-FX-019","STU-FX-020","STU-FX-022"],"kinds":{"filter_box_blur":"radius 0..30000 and iterations 1..50 (Fast Box Blur table); half = floor(radius + 0.5), blur_math box_kernel then blur_window_in or blur_window_axis; above MAX_BOX_KERNEL_TAPS is filter_over_budget","filter_motion_blur":"length 0..1000 (Directional Blur table, wins over the group row under STU-FX-101), angle any finite; blur_math directional_blur_window","filter_radial_blur":"amount 0..1000 (Radial Blur table), centre any finite plan coordinates; blur_math radial_blur_window","filter_find_edges":"filter_math find_edges_window","filter_custom_convolution":"5x5 weights, scale != 0 (division domain), offset; replicate border, per premultiplied colour channel sum(w q) / scale, un-premultiplied by the source alpha, plus offset / 255, clamped; alpha is the source's","filter_sharpen":"3x3 [0,-1,0;-1,5,-1;0,-1,0]","filter_sharpen_more":"3x3 [-1,-1,-1;-1,9,-1;-1,-1,-1]","filter_basic_blur":"binomial [1 2 1] squared / 16, impulse variance per axis 0.5","filter_blur_more":"[1 2 2 2 1] squared / 64, impulse variance per axis 1.5, three times basic blur","filter_sharpen_edges":"sharpen where the largest colour channel Sobel magnitude >= 0.1, else source","filter_despeckle":"basic blur where the largest colour channel Sobel magnitude < 0.1, else source","filter_emboss":"gray 0.5 + amount/100 (L(p + h d) - L(p - h d)), d = (cos, -sin), bilinear premultiplied, Rec. 709 L; alpha is the source's; height and amount >= 0, any finite angle","filter_solarize":"threshold 0..254 (Solarize table); per colour channel v * 255 > threshold gives 1 - v; alpha kept","filter_mosaic":"blocks 1..4000 each (Mosaic table rows 1, 2); plane split at floor(i W / n); each cell the premultiplied mean; grid anchored to the plane","filter_maximum":"radius 0..32000 (Minimax table); per premultiplied channel, transparent beyond the source; squareness separable van Herk / Gil-Werman, roundness disc; radius 0 identity","filter_minimum":"as filter_maximum, the minimum","filter_dust_and_scratches":"radius 0..255, threshold 0..1 (table); per premultiplied channel median m over the disc, |o - m| > threshold gives m else o; threshold 0 equals filter_median where radius >= 1","filter_surface_blur":"radius any, threshold > 0 (domain); square window of samples inside the plane; per premultiplied channel weight max(0, 1 - |q - o| * 255 / (2.5 t)), normalised; this weight is folklore, public sources did not confirm it","filter_bilateral_blur":"radius any, range sigma > 0 (domain); same engine; spatial Gaussian sigma = radius / 2 times Gaussian range; exp is platform libm, bytes stable per platform"},"plan":"StackFilter::Kernel of the smart filter stack; whole plane, no window reach; render-cpu runs them through SmartFilterStack::render, render-gpu refuses each by its key before any device work","gaps":["STU-FX-128: Emboss catalogue row has no typed parameters; names come from its group row","Emboss color-amount of its group row is not modelled: the result is gray","Mosaic table row 3 is unnamed and not implemented","Minimax Operation, Channel and Direction rows are a later slice","STU-FX-016 tessellation core unmet: Mosaic is a plain grid, Crystallize and Voronoi are not here","Motion Blur: the group row's intensity 1..999 yields to the table's length 0..1000, recorded for the spec owner","SmartFilterStack::render takes no cancel token: one stack can run up to the work budget uncancelled","GPU kernels: none; STU-FX-130 promotion equivalence is not claimed"],"parity":"NOT_PROVEN"}"#;

/// Blur dimensions of [`FilterKernel::BoxBlur`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum BoxDimensions {
    HorizontalAndVertical,
    Horizontal,
    Vertical,
}

/// Neighbourhood of [`FilterKernel::Maximum`] and [`FilterKernel::Minimum`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum MorphPreserve {
    Squareness,
    Roundness,
}

/// One raster filter kernel with its parameters.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum FilterKernel {
    BoxBlur { radius_px: f32, iterations: u32, dimensions: BoxDimensions },
    MotionBlur { angle_deg: f32, length_px: f32 },
    RadialBlur { centre_x_px: f32, centre_y_px: f32, amount: f32, kind: RadialBlurKind },
    FindEdges { invert: bool },
    CustomConvolution { weights: [i32; 25], scale: i32, offset: i32 },
    Sharpen,
    SharpenMore,
    BasicBlur,
    BlurMore,
    SharpenEdges,
    Despeckle,
    Emboss { angle_deg: f32, height_px: f32, amount_pct: f32 },
    Solarize { threshold_levels: u8 },
    Mosaic { horizontal_blocks: u32, vertical_blocks: u32 },
    Maximum { radius_px: u32, preserve: MorphPreserve },
    Minimum { radius_px: u32, preserve: MorphPreserve },
    DustAndScratches { radius_px: u32, threshold: f32 },
    SurfaceBlur { radius_px: u32, threshold_levels: f32 },
    BilateralBlur { radius_px: u32, range_sigma: f32 },
}

fn range(v: f32, lo: f32, hi: f32) -> bool {
    v.is_finite() && v >= lo && v <= hi
}

fn to_usize(v: u32) -> Result<usize, FilterError> {
    usize::try_from(v).map_err(|_| FilterError::Overflow)
}

fn blur_limits(l: FilterLimits) -> BlurLimits {
    BlurLimits { max_bytes: l.max_bytes, max_work: l.max_work }
}

/// `half = floor(radius + 0.5)` (hard bound checked) and the taps of `iterations` passes.
fn box_half_and_taps(radius_px: f32, iterations: u32) -> Result<(u32, u64), FilterError> {
    let half = (f64::from(radius_px) + 0.5).floor() as u32;
    let taps = u64::from(half)
        .checked_mul(2)
        .and_then(|v| v.checked_mul(u64::from(iterations)))
        .and_then(|v| v.checked_add(1))
        .ok_or(FilterError::Overflow)?;
    Ok((half, taps))
}

/// `bytes` output, premultiplied copy and any `scratch`; `reads` per output pixel.
fn plane_cost(area: u64, keep_per_px: u64, scratch: u64, reads: u64) -> Result<(u64, u64), FilterError> {
    let bytes = area
        .checked_mul(BLUR_SAMPLE_BYTES.checked_add(keep_per_px).ok_or(FilterError::Overflow)?)
        .and_then(|v| v.checked_add(scratch))
        .ok_or(FilterError::Overflow)?;
    let work = area.checked_mul(reads).ok_or(FilterError::Overflow)?;
    Ok((bytes, work))
}

impl FilterKernel {
    /// Operator key.
    pub const fn key(&self) -> &'static str {
        match self {
            Self::BoxBlur { .. } => "filter_box_blur",
            Self::MotionBlur { .. } => "filter_motion_blur",
            Self::RadialBlur { .. } => "filter_radial_blur",
            Self::FindEdges { .. } => "filter_find_edges",
            Self::CustomConvolution { .. } => "filter_custom_convolution",
            Self::Sharpen => "filter_sharpen",
            Self::SharpenMore => "filter_sharpen_more",
            Self::BasicBlur => "filter_basic_blur",
            Self::BlurMore => "filter_blur_more",
            Self::SharpenEdges => "filter_sharpen_edges",
            Self::Despeckle => "filter_despeckle",
            Self::Emboss { .. } => "filter_emboss",
            Self::Solarize { .. } => "filter_solarize",
            Self::Mosaic { .. } => "filter_mosaic",
            Self::Maximum { .. } => "filter_maximum",
            Self::Minimum { .. } => "filter_minimum",
            Self::DustAndScratches { .. } => "filter_dust_and_scratches",
            Self::SurfaceBlur { .. } => "filter_surface_blur",
            Self::BilateralBlur { .. } => "filter_bilateral_blur",
        }
    }

    /// Hard bounds and mathematical domains; outside is [`FilterError::Parameter`], never a clamp.
    pub fn validate(&self) -> Result<(), FilterError> {
        let ok = match *self {
            Self::BoxBlur { radius_px, iterations, .. } => range(radius_px, 0.0, 30_000.0) && (1..=50).contains(&iterations),
            Self::MotionBlur { angle_deg, length_px } => angle_deg.is_finite() && range(length_px, 0.0, 1_000.0),
            Self::RadialBlur { centre_x_px, centre_y_px, amount, .. } => {
                centre_x_px.is_finite() && centre_y_px.is_finite() && range(amount, 0.0, 1_000.0)
            }
            Self::CustomConvolution { scale, .. } => scale != 0,
            Self::FindEdges { .. } | Self::Sharpen | Self::SharpenMore | Self::BasicBlur | Self::BlurMore | Self::SharpenEdges | Self::Despeckle => true,
            Self::Emboss { angle_deg, height_px, amount_pct } => {
                angle_deg.is_finite() && height_px.is_finite() && height_px >= 0.0 && amount_pct.is_finite() && amount_pct >= 0.0
            }
            Self::Solarize { threshold_levels } => threshold_levels <= 254,
            Self::Mosaic { horizontal_blocks, vertical_blocks } => (1..=4_000).contains(&horizontal_blocks) && (1..=4_000).contains(&vertical_blocks),
            Self::Maximum { radius_px, .. } | Self::Minimum { radius_px, .. } => radius_px <= 32_000,
            Self::DustAndScratches { radius_px, threshold } => radius_px <= 255 && range(threshold, 0.0, 1.0),
            Self::SurfaceBlur { threshold_levels, .. } => threshold_levels.is_finite() && threshold_levels > 0.0,
            Self::BilateralBlur { range_sigma, .. } => range_sigma.is_finite() && range_sigma > 0.0,
        };
        if ok { Ok(()) } else { Err(FilterError::Parameter) }
    }

    /// Predicted peak bytes and work (sample visits) over `out`; a parameter or arithmetic refusal
    /// is typed. A box blur above [`MAX_BOX_KERNEL_TAPS`] and an unbounded radius whose window does
    /// not fit in `u64` are [`FilterError::OverBudget`]: inside their bounds, over the budget.
    pub fn cost(&self, out: Rect, limits: FilterLimits) -> Result<(u64, u64), FilterError> {
        self.validate()?;
        let area = u64::from(out.width).checked_mul(u64::from(out.height)).ok_or(FilterError::Overflow)?;
        match *self {
            Self::BoxBlur { radius_px, iterations, dimensions } => {
                let (_, taps) = box_half_and_taps(radius_px, iterations)?;
                let max = u64::try_from(MAX_BOX_KERNEL_TAPS).map_err(|_| FilterError::Overflow)?;
                if taps > max {
                    return Err(FilterError::OverBudget { needed: taps, limit: max });
                }
                let taps = usize::try_from(taps).map_err(|_| FilterError::Overflow)?;
                Ok(match dimensions {
                    BoxDimensions::HorizontalAndVertical => blur_window_cost(out, taps)?,
                    BoxDimensions::Horizontal | BoxDimensions::Vertical => blur_window_axis_cost(out, taps)?,
                })
            }
            Self::MotionBlur { length_px, .. } => Ok(directional_blur_window_cost(out, length_px)?),
            Self::RadialBlur { centre_x_px, centre_y_px, amount, kind } => {
                Ok(radial_blur_window_cost(out, [centre_x_px, centre_y_px], amount, kind, blur_limits(limits))?)
            }
            Self::FindEdges { .. } => find_edges_window_cost(out),
            Self::CustomConvolution { .. } | Self::Sharpen | Self::SharpenMore | Self::BasicBlur | Self::BlurMore => {
                plane_cost(area, PM64_BYTES, 0, CONV_READS)
            }
            Self::SharpenEdges | Self::Despeckle => {
                plane_cost(area, PM64_BYTES, 0, CONV_READS.checked_add(SOBEL_READS).ok_or(FilterError::Overflow)?)
            }
            Self::Emboss { .. } => plane_cost(area, 0, 0, 8),
            Self::Solarize { .. } => plane_cost(area, 0, 0, 4),
            Self::Mosaic { horizontal_blocks, vertical_blocks } => {
                let spans = u64::from(horizontal_blocks.min(out.width))
                    .checked_add(u64::from(vertical_blocks.min(out.height)))
                    .and_then(|v| v.checked_mul(16))
                    .ok_or(FilterError::Overflow)?;
                plane_cost(area, 0, spans, 2)
            }
            Self::Maximum { radius_px, preserve } | Self::Minimum { radius_px, preserve } => {
                if radius_px == 0 {
                    return plane_cost(area, 0, 0, 4);
                }
                match preserve {
                    MorphPreserve::Squareness => {
                        let scratch = u64::from(out.width.max(out.height)).checked_mul(40).ok_or(FilterError::Overflow)?;
                        // Rows then columns, four channels: independent of the radius.
                        let reads = MORPH_READS.checked_mul(8).ok_or(FilterError::Overflow)?;
                        plane_cost(area, BLUR_SAMPLE_BYTES, scratch, reads)
                    }
                    MorphPreserve::Roundness => {
                        let n = disc_len(radius_px)?;
                        let scratch = n.checked_mul(16).ok_or(FilterError::Overflow)?;
                        plane_cost(area, BLUR_SAMPLE_BYTES, scratch, n.checked_mul(4).ok_or(FilterError::Overflow)?)
                    }
                }
            }
            Self::DustAndScratches { radius_px, .. } => {
                let n = disc_len(radius_px)?;
                let scratch = n.checked_mul(32).ok_or(FilterError::Overflow)?;
                plane_cost(area, BLUR_SAMPLE_BYTES, scratch, n.checked_mul(4).ok_or(FilterError::Overflow)?)
            }
            Self::SurfaceBlur { radius_px, .. } | Self::BilateralBlur { radius_px, .. } => {
                let over = FilterError::OverBudget { needed: u64::MAX, limit: limits.max_work };
                let half = u64::from(radius_px).checked_add(1).ok_or(over)?;
                let side = u64::from(radius_px).checked_mul(2).and_then(|v| v.checked_add(1)).ok_or(over)?;
                let reads = side.checked_mul(side).and_then(|v| v.checked_mul(4)).ok_or(over)?;
                let scratch = half.checked_mul(8).ok_or(over)?;
                let bytes = area.checked_mul(BLUR_SAMPLE_BYTES.checked_add(PM64_BYTES).ok_or(FilterError::Overflow)?).and_then(|v| v.checked_add(scratch)).ok_or(over)?;
                Ok((bytes, area.checked_mul(reads).ok_or(over)?))
            }
        }
    }

    /// [`Self::cost`] over `out` checked against `limits`.
    pub fn admit(&self, out: Rect, limits: FilterLimits) -> Result<(), FilterError> {
        let (bytes, work) = self.cost(out, limits)?;
        if bytes > limits.max_bytes {
            return Err(FilterError::OverBudget { needed: bytes, limit: limits.max_bytes });
        }
        if work > limits.max_work {
            return Err(FilterError::OverBudget { needed: work, limit: limits.max_work });
        }
        Ok(())
    }

    /// The kernel over the whole plane. Parameters, then the budget, then the shape and every sample
    /// are checked before any kernel allocation; a non-finite result is `filter_out_of_domain`.
    pub fn render_plane(&self, plane: &RgbaPlane, limits: FilterLimits) -> Result<RgbaPlane, FilterError> {
        self.admit(plane.bounds, limits)?;
        let b = plane.bounds;
        b.end().map_err(|_| FilterError::Overflow)?;
        let n = usize::try_from(u64::from(b.width).checked_mul(u64::from(b.height)).ok_or(FilterError::Overflow)?).map_err(|_| FilterError::Overflow)?;
        if plane.data.len() != n {
            return Err(FilterError::Shape);
        }
        for p in &plane.data {
            check(*p, UNIT)?;
        }
        let data = match self {
            Self::BoxBlur { radius_px, iterations, dimensions } => box_blur(plane, *radius_px, *iterations, *dimensions, limits)?,
            Self::MotionBlur { angle_deg, length_px } => directional_blur_window(UNIT, &plane.data, b, b, *angle_deg, *length_px, blur_limits(limits))?,
            Self::RadialBlur { centre_x_px, centre_y_px, amount, kind } => {
                radial_blur_window(UNIT, &plane.data, b, b, [*centre_x_px, *centre_y_px], *amount, *kind, blur_limits(limits))?
            }
            Self::FindEdges { invert } => find_edges_window(&plane.data, b, b, *invert, limits)?,
            Self::CustomConvolution { weights, scale, offset } => convolve(plane, &Convolution { weights: *weights, scale: *scale, offset: *offset }, Gate::Everywhere, limits)?,
            Self::Sharpen => convolve(plane, &SHARPEN, Gate::Everywhere, limits)?,
            Self::SharpenMore => convolve(plane, &SHARPEN_MORE, Gate::Everywhere, limits)?,
            Self::BasicBlur => convolve(plane, &BASIC_BLUR, Gate::Everywhere, limits)?,
            Self::BlurMore => convolve(plane, &BLUR_MORE, Gate::Everywhere, limits)?,
            Self::SharpenEdges => convolve(plane, &SHARPEN, Gate::OnEdges, limits)?,
            Self::Despeckle => convolve(plane, &BASIC_BLUR, Gate::OffEdges, limits)?,
            Self::Emboss { angle_deg, height_px, amount_pct } => emboss(plane, *angle_deg, *height_px, *amount_pct, limits)?,
            Self::Solarize { threshold_levels } => solarize(plane, *threshold_levels, limits)?,
            Self::Mosaic { horizontal_blocks, vertical_blocks } => mosaic(plane, *horizontal_blocks, *vertical_blocks, limits)?,
            Self::Maximum { radius_px, preserve } => morphology(plane, *radius_px, *preserve, f32::max, limits)?,
            Self::Minimum { radius_px, preserve } => morphology(plane, *radius_px, *preserve, f32::min, limits)?,
            Self::DustAndScratches { radius_px, threshold } => dust_and_scratches(plane, *radius_px, *threshold, limits)?,
            Self::SurfaceBlur { radius_px, threshold_levels } => {
                let denom = 2.5 * f64::from(*threshold_levels);
                edge_preserving(plane, *radius_px, false, |d| (1.0 - d * 255.0 / denom).max(0.0), limits)?
            }
            Self::BilateralBlur { radius_px, range_sigma } => {
                let s = f64::from(*range_sigma);
                edge_preserving(plane, *radius_px, true, |d| (-(d * d) / (2.0 * s * s)).exp(), limits)?
            }
        };
        Ok(RgbaPlane { bounds: b, data })
    }
}

fn reserve<T>(n: usize, limits: FilterLimits) -> Result<Vec<T>, FilterError> {
    let mut v = Vec::new();
    v.try_reserve_exact(n)
        .map_err(|_| FilterError::OverBudget { needed: u64::try_from(n).unwrap_or(u64::MAX), limit: limits.max_bytes })?;
    Ok(v)
}

fn identity(plane: &RgbaPlane, limits: FilterLimits) -> Result<Vec<[f32; 4]>, FilterError> {
    let mut v = reserve(plane.data.len(), limits)?;
    v.extend_from_slice(&plane.data);
    Ok(v)
}

fn finite(px: [f32; 4]) -> Result<[f32; 4], FilterError> {
    if px.iter().all(|v| v.is_finite()) { Ok(px) } else { Err(FilterError::OutOfDomain) }
}

fn pm64(plane: &RgbaPlane, limits: FilterLimits) -> Result<Vec<[f64; 4]>, FilterError> {
    let mut v = reserve(plane.data.len(), limits)?;
    v.extend(plane.data.iter().map(|p| {
        let a = f64::from(p[3]);
        [f64::from(p[0]) * a, f64::from(p[1]) * a, f64::from(p[2]) * a, a]
    }));
    Ok(v)
}

/// Premultiplied binary32, the arithmetic of `filter_math`'s median.
fn pm32(plane: &RgbaPlane, limits: FilterLimits) -> Result<Vec<[f32; 4]>, FilterError> {
    let mut v = reserve(plane.data.len(), limits)?;
    v.extend(plane.data.iter().map(|p| [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]]));
    Ok(v)
}

/// Row-major index; `x < w` and `y` inside a plane whose shape was checked, so it stays in range.
fn index(w: usize, x: usize, y: usize) -> usize {
    y.saturating_mul(w).saturating_add(x)
}

fn box_blur(plane: &RgbaPlane, radius_px: f32, iterations: u32, dimensions: BoxDimensions, limits: FilterLimits) -> Result<Vec<[f32; 4]>, FilterError> {
    let (half, _) = box_half_and_taps(radius_px, iterations)?;
    if half == 0 {
        return identity(plane, limits);
    }
    let taps = box_kernel(half, iterations)?;
    let b = plane.bounds;
    let wide = blur_limits(limits);
    Ok(match dimensions {
        BoxDimensions::HorizontalAndVertical => blur_window_in(UNIT, &plane.data, b, b, &taps, wide)?,
        BoxDimensions::Horizontal => blur_window_axis(UNIT, &plane.data, b, b, &taps, BlurAxis::Horizontal, wide)?,
        BoxDimensions::Vertical => blur_window_axis(UNIT, &plane.data, b, b, &taps, BlurAxis::Vertical, wide)?,
    })
}

/// The one 5x5 replicate-border convolution primitive.
struct Convolution {
    weights: [i32; 25],
    scale: i32,
    offset: i32,
}

/// Which pixels take the convolution; the rest keep the source bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Gate {
    Everywhere,
    OnEdges,
    OffEdges,
}

fn convolve(plane: &RgbaPlane, k: &Convolution, gate: Gate, limits: FilterLimits) -> Result<Vec<[f32; 4]>, FilterError> {
    let b = plane.bounds;
    let (w, h) = (to_usize(b.width)?, to_usize(b.height)?);
    let pm = pm64(plane, limits)?;
    let mut out = reserve(plane.data.len(), limits)?;
    let (scale, offset) = (f64::from(k.scale), f64::from(k.offset) / 255.0);
    for y in 0..h {
        for x in 0..w {
            let o = plane.data[index(w, x, y)];
            if o[3] == 0.0 {
                out.push([0.0; 4]);
                continue;
            }
            if gate != Gate::Everywhere {
                let m = sobel_magnitude(&plane.data, b, b.x.saturating_add(x as i64), b.y.saturating_add(y as i64))?;
                let edge = m[0].max(m[1]).max(m[2]) >= EDGE_GATE;
                if edge != (gate == Gate::OnEdges) {
                    out.push(o);
                    continue;
                }
            }
            let mut sum = [0.0_f64; 3];
            let mut tap = k.weights.iter();
            for dy in 0..5usize {
                // Replicate border: the nearest source pixel.
                let ny = y.saturating_add(dy).saturating_sub(2).min(h.saturating_sub(1));
                for dx in 0..5usize {
                    let wt = f64::from(tap.next().copied().unwrap_or(0));
                    if wt == 0.0 {
                        continue;
                    }
                    let nx = x.saturating_add(dx).saturating_sub(2).min(w.saturating_sub(1));
                    let q = pm[index(w, nx, ny)];
                    for (s, v) in sum.iter_mut().zip(q) {
                        *s += wt * v;
                    }
                }
            }
            let a = f64::from(o[3]);
            let c = |s: f64| (s / scale / a + offset).clamp(0.0, 1.0) as f32;
            out.push(finite([c(sum[0]), c(sum[1]), c(sum[2]), o[3]])?);
        }
    }
    Ok(out)
}

fn emboss(plane: &RgbaPlane, angle_deg: f32, height_px: f32, amount_pct: f32, limits: FilterLimits) -> Result<Vec<[f32; 4]>, FilterError> {
    let b = plane.bounds;
    let (ex, ey) = b.end().map_err(|_| FilterError::Overflow)?;
    let (c, s) = cos_sin_deg(f64::from(angle_deg));
    let h = f64::from(height_px);
    let (hx, hy) = (h * c, h * -s);
    let amount = f64::from(amount_pct) / 100.0;
    let mut out = reserve(plane.data.len(), limits)?;
    let mut it = plane.data.iter();
    let luma = |u: f64, v: f64| -> Result<f64, FilterError> {
        let mut acc = [0.0_f64; 4];
        bilinear_add(UNIT, &plane.data, b, u, v, &mut acc)?;
        Ok(LUMA[0] * acc[0] + LUMA[1] * acc[1] + LUMA[2] * acc[2])
    };
    for y in b.y..ey {
        for x in b.x..ex {
            let o = it.next().copied().ok_or(FilterError::Shape)?;
            if o[3] == 0.0 {
                out.push([0.0; 4]);
                continue;
            }
            let (px, py) = (x as f64, y as f64);
            let d = luma(px + hx, py + hy)? - luma(px - hx, py - hy)?;
            let g = (0.5 + amount * d).clamp(0.0, 1.0) as f32;
            out.push(finite([g, g, g, o[3]])?);
        }
    }
    Ok(out)
}

fn solarize(plane: &RgbaPlane, threshold_levels: u8, limits: FilterLimits) -> Result<Vec<[f32; 4]>, FilterError> {
    let t = f64::from(threshold_levels);
    let mut out = reserve(plane.data.len(), limits)?;
    out.extend(plane.data.iter().map(|o| {
        if o[3] == 0.0 {
            return [0.0; 4];
        }
        let c = |v: f32| if f64::from(v) * 255.0 > t { (1.0 - f64::from(v)) as f32 } else { v };
        [c(o[0]), c(o[1]), c(o[2]), o[3]]
    }));
    Ok(out)
}

/// Non-empty cells of a split of `len` at `floor(i * len / n)`.
fn spans(n: u32, len: u32, limits: FilterLimits) -> Result<Vec<(usize, usize)>, FilterError> {
    let mut v = reserve(to_usize(n.min(len))?, limits)?;
    let mut start = 0u64;
    for i in 1..=n {
        let end = u64::from(i)
            .checked_mul(u64::from(len))
            .and_then(|m| m.checked_div(u64::from(n)))
            .ok_or(FilterError::Overflow)?;
        if end > start {
            let pair = (usize::try_from(start).map_err(|_| FilterError::Overflow)?, usize::try_from(end).map_err(|_| FilterError::Overflow)?);
            v.push(pair);
            start = end;
        }
    }
    Ok(v)
}

fn mosaic(plane: &RgbaPlane, horizontal_blocks: u32, vertical_blocks: u32, limits: FilterLimits) -> Result<Vec<[f32; 4]>, FilterError> {
    let b = plane.bounds;
    let w = to_usize(b.width)?;
    let cols = spans(horizontal_blocks, b.width, limits)?;
    let rows = spans(vertical_blocks, b.height, limits)?;
    let mut out: Vec<[f32; 4]> = reserve(plane.data.len(), limits)?;
    out.resize(plane.data.len(), [0.0; 4]);
    for &(y0, y1) in &rows {
        for &(x0, x1) in &cols {
            let mut sum = [0.0_f64; 4];
            let mut n = 0.0_f64;
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = plane.data[index(w, x, y)];
                    let a = f64::from(p[3]);
                    let pm = [f64::from(p[0]) * a, f64::from(p[1]) * a, f64::from(p[2]) * a, a];
                    for (s, v) in sum.iter_mut().zip(pm) {
                        *s += v;
                    }
                    n += 1.0;
                }
            }
            let mean = finite(unpremultiply_in(UNIT, sum.map(|v| (v / n) as f32)))?;
            for y in y0..y1 {
                for x in x0..x1 {
                    out[index(w, x, y)] = mean;
                }
            }
        }
    }
    Ok(out)
}

struct LineScratch {
    a: Vec<f32>,
    g: Vec<f32>,
    h: Vec<f32>,
}

/// One van Herk / Gil-Werman pass over `line`, transparent (0) beyond it. A radius of at least the
/// line's length covers the whole line and the padding from every position, so the padding is at
/// most the line's length on each side and the work does not depend on the radius.
fn morph_line(line: &mut [f32], radius: usize, op: fn(f32, f32) -> f32, s: &mut LineScratch) {
    let n = line.len();
    let r = radius.min(n);
    let span = r.saturating_mul(2);
    let win = span.saturating_add(1);
    let len = n.saturating_add(span);
    for buf in [&mut s.a, &mut s.g, &mut s.h] {
        buf.clear();
        buf.resize(len, 0.0);
    }
    s.a[r..r.saturating_add(n)].copy_from_slice(line);
    let mut acc = 0.0_f32;
    for (p, (g, a)) in s.g.iter_mut().zip(&s.a).enumerate() {
        acc = if p.checked_rem(win) == Some(0) { *a } else { op(acc, *a) };
        *g = acc;
    }
    for p in (0..len).rev() {
        let next = p.saturating_add(1);
        let a = s.a[p];
        acc = if next == len || next.checked_rem(win) == Some(0) { a } else { op(acc, a) };
        s.h[p] = acc;
    }
    for (i, v) in line.iter_mut().enumerate() {
        *v = op(s.h[i], s.g[i.saturating_add(span)]);
    }
}

fn morphology(plane: &RgbaPlane, radius_px: u32, preserve: MorphPreserve, op: fn(f32, f32) -> f32, limits: FilterLimits) -> Result<Vec<[f32; 4]>, FilterError> {
    if radius_px == 0 {
        return identity(plane, limits);
    }
    let b = plane.bounds;
    let (w, h) = (to_usize(b.width)?, to_usize(b.height)?);
    let pm = pm32(plane, limits)?;
    let mut cur = match preserve {
        MorphPreserve::Squareness => morph_square(pm, w, h, to_usize(radius_px)?, op, limits)?,
        MorphPreserve::Roundness => {
            let offsets = disc(radius_px)?;
            let (wi, hi) = (i64::from(b.width), i64::from(b.height));
            let mut cur: Vec<[f32; 4]> = reserve(pm.len(), limits)?;
            for y in 0..hi {
                for x in 0..wi {
                    let mut acc = [0.0_f32; 4];
                    for (i, &(dx, dy)) in offsets.iter().enumerate() {
                        let q = beyond_is_zero(&pm, w, wi, hi, x.saturating_add(dx), y.saturating_add(dy));
                        for (s, v) in acc.iter_mut().zip(q) {
                            *s = if i == 0 { v } else { op(*s, v) };
                        }
                    }
                    cur.push(acc);
                }
            }
            cur
        }
    };
    // In place, so the peak is the premultiplied copy plus `cur`: the 32 bytes a pixel of the cost.
    for p in cur.iter_mut() {
        *p = finite(unpremultiply_in(UNIT, *p))?;
    }
    Ok(cur)
}

fn morph_square(mut cur: Vec<[f32; 4]>, w: usize, h: usize, radius: usize, op: fn(f32, f32) -> f32, limits: FilterLimits) -> Result<Vec<[f32; 4]>, FilterError> {
    let longest = w.max(h);
    let cap = longest.saturating_mul(3).saturating_add(1);
    let mut s = LineScratch { a: reserve(cap, limits)?, g: reserve(cap, limits)?, h: reserve(cap, limits)? };
    let mut line: Vec<f32> = reserve(longest, limits)?;
    for c in 0..4 {
        for y in 0..h {
            line.clear();
            line.extend((0..w).map(|x| cur[index(w, x, y)][c]));
            morph_line(&mut line, radius, op, &mut s);
            for (x, v) in line.iter().enumerate() {
                cur[index(w, x, y)][c] = *v;
            }
        }
        for x in 0..w {
            line.clear();
            line.extend((0..h).map(|y| cur[index(w, x, y)][c]));
            morph_line(&mut line, radius, op, &mut s);
            for (y, v) in line.iter().enumerate() {
                cur[index(w, x, y)][c] = *v;
            }
        }
    }
    Ok(cur)
}

fn beyond_is_zero(pm: &[[f32; 4]], w: usize, wi: i64, hi: i64, x: i64, y: i64) -> [f32; 4] {
    if x < 0 || y < 0 || x >= wi || y >= hi {
        return [0.0; 4];
    }
    match (usize::try_from(x), usize::try_from(y)) {
        (Ok(x), Ok(y)) => pm.get(index(w, x, y)).copied().unwrap_or([0.0; 4]),
        _ => [0.0; 4],
    }
}

fn dust_and_scratches(plane: &RgbaPlane, radius_px: u32, threshold: f32, limits: FilterLimits) -> Result<Vec<[f32; 4]>, FilterError> {
    if radius_px == 0 {
        return identity(plane, limits);
    }
    let b = plane.bounds;
    let (w, wi, hi) = (to_usize(b.width)?, i64::from(b.width), i64::from(b.height));
    let pm = pm32(plane, limits)?;
    let offsets = disc(radius_px)?;
    let mid = offsets.len().checked_div(2).ok_or(FilterError::Overflow)?;
    let t = f64::from(threshold);
    let mut scratch: [Vec<f32>; 4] = [reserve(offsets.len(), limits)?, reserve(offsets.len(), limits)?, reserve(offsets.len(), limits)?, reserve(offsets.len(), limits)?];
    let mut out = reserve(pm.len(), limits)?;
    let mut it = pm.iter();
    for y in 0..hi {
        for x in 0..wi {
            let o = it.next().copied().ok_or(FilterError::Shape)?;
            for s in scratch.iter_mut() {
                s.clear();
            }
            for &(dx, dy) in &offsets {
                let q = beyond_is_zero(&pm, w, wi, hi, x.saturating_add(dx), y.saturating_add(dy));
                for (s, v) in scratch.iter_mut().zip(q) {
                    s.push(v);
                }
            }
            let mut r = [0.0_f32; 4];
            for ((rc, s), oc) in r.iter_mut().zip(scratch.iter_mut()).zip(o) {
                let (_, m, _) = s.select_nth_unstable_by(mid, f32::total_cmp);
                let m = *m;
                *rc = if (f64::from(oc) - f64::from(m)).abs() > t { m } else { oc };
            }
            out.push(finite(unpremultiply_in(UNIT, r))?);
        }
    }
    Ok(out)
}

/// Surface and Bilateral Blur: one engine, two weights. Per premultiplied channel the normalised mean
/// over the square window's samples inside the plane (samples beyond it take no part, so an opaque
/// layer keeps its edge), weighted by `range(|q - o|)` and, for `spatial`, the Gaussian of sigma
/// `radius / 2` along each axis. The pixel itself has weight 1, so the normaliser is at least 1.
fn edge_preserving(plane: &RgbaPlane, radius_px: u32, spatial: bool, range: impl Fn(f64) -> f64, limits: FilterLimits) -> Result<Vec<[f32; 4]>, FilterError> {
    if radius_px == 0 {
        return identity(plane, limits);
    }
    let b = plane.bounds;
    let (w, h, r) = (to_usize(b.width)?, to_usize(b.height)?, to_usize(radius_px)?);
    let pm = pm64(plane, limits)?;
    let mut table: Vec<f64> = reserve(if spatial { r.saturating_add(1) } else { 0 }, limits)?;
    if spatial {
        let sigma = f64::from(radius_px) / 2.0;
        table.extend((0..=radius_px).map(|d| {
            let d = f64::from(d);
            (-(d * d) / (2.0 * sigma * sigma)).exp()
        }));
    }
    let weight = |d: usize| table.get(d).copied().unwrap_or(1.0);
    let mut out = reserve(pm.len(), limits)?;
    for y in 0..h {
        let (y0, y1) = (y.saturating_sub(r), y.saturating_add(r).min(h.saturating_sub(1)));
        for x in 0..w {
            let (x0, x1) = (x.saturating_sub(r), x.saturating_add(r).min(w.saturating_sub(1)));
            let o = pm[index(w, x, y)];
            let (mut num, mut den) = ([0.0_f64; 4], [0.0_f64; 4]);
            for ny in y0..=y1 {
                let wy = weight(ny.abs_diff(y));
                for nx in x0..=x1 {
                    let s = wy * weight(nx.abs_diff(x));
                    let q = pm[index(w, nx, ny)];
                    for c in 0..4 {
                        let wt = s * range((q[c] - o[c]).abs());
                        num[c] += wt * q[c];
                        den[c] += wt;
                    }
                }
            }
            let mut px = [0.0_f32; 4];
            for c in 0..4 {
                px[c] = (num[c] / den[c]) as f32;
            }
            out.push(finite(unpremultiply_in(UNIT, px))?);
        }
    }
    Ok(out)
}
