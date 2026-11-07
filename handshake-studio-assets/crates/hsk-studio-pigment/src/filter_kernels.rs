//! Raster filter kernels of the live filter stack (lane SMB-PIGMENT-FILTER-KERNELS-1; spec basis
//! STU-RAS-037, STU-FX-013, STU-FX-015, STU-FX-016, STU-FX-019, STU-FX-020, STU-FX-022). Every kind
//! maps a whole straight-RGBA plane to a straight-RGBA plane of the same bounds; they reach documents
//! as [`crate::smart_filter::StackFilter::Kernel`] and carry no window contract.
//!
//! Stub of the plumbing commit: every kind is refused at render until its item lands.
#![deny(clippy::arithmetic_side_effects)]
use crate::Rect;
use crate::blur_math::RadialBlurKind;
use crate::filter_math::{FilterError, FilterLimits};
use crate::retouch::RgbaPlane;
use serde::Serialize;

/// Filled in by the kernels.
pub const DESCRIPTOR: &str = "{}";

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

    /// Predicted peak bytes and work over `out`.
    pub fn cost(&self, _out: Rect, _limits: FilterLimits) -> Result<(u64, u64), FilterError> {
        self.validate()?;
        Err(FilterError::Overflow)
    }

    /// The cost of `out` checked against `limits`.
    pub fn admit(&self, out: Rect, limits: FilterLimits) -> Result<(), FilterError> {
        self.cost(out, limits).map(|_| ())
    }

    /// The kernel over the whole plane.
    pub fn render_plane(&self, plane: &RgbaPlane, limits: FilterLimits) -> Result<RgbaPlane, FilterError> {
        self.admit(plane.bounds, limits)?;
        Ok(plane.clone())
    }
}
