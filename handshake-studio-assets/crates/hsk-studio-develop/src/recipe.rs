//! Typed, validated develop recipe (STU-CON-042/044). The recipe is an immutable value: develop
//! never rewrites it, original labels stay as authored, disabled groups stay intact.
use crate::DevelopError;
use crate::curves::{Curve, PreparedCurve};
use hsk_studio_accord::DomainId;

pub const PROCESS_VERSION: (u32, u32) = (1, 0);
pub const ENGINE_VERSION: (u32, u32) = (1, 0);
pub const MATH_TOKEN: &str = "native_cfa_bilinear_hermite_v1";
pub const DEMOSAIC_ALGORITHM: &str = "bilinear_reflect_native_v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WbMode {
    AsShot,
    Temperature,
    Auto,
    Custom,
}

/// The twelve STU-RAW-105 booleans, all required and all retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Enables {
    pub enable_calibration: bool,
    pub enable_colour_adjustments: bool,
    pub enable_detail: bool,
    pub enable_effects: bool,
    pub enable_grayscale_mix: bool,
    pub enable_lens_corrections: bool,
    pub enable_mask_group_based_corrections: bool,
    pub enable_red_eye: bool,
    pub enable_retouch: bool,
    pub enable_tone_curve: bool,
    pub enable_transform: bool,
    pub enable_distraction_removal: bool,
}

impl Enables {
    pub const fn all_disabled() -> Self {
        Self {
            enable_calibration: false,
            enable_colour_adjustments: false,
            enable_detail: false,
            enable_effects: false,
            enable_grayscale_mix: false,
            enable_lens_corrections: false,
            enable_mask_group_based_corrections: false,
            enable_red_eye: false,
            enable_retouch: false,
            enable_tone_curve: false,
            enable_transform: false,
            enable_distraction_removal: false,
        }
    }

    /// First enabled group that this slice cannot execute (spec order), if any.
    fn first_unsupported(&self) -> Option<&'static str> {
        [
            (self.enable_calibration, "calibration"),
            (self.enable_colour_adjustments, "colour_adjustments"),
            (self.enable_detail, "detail"),
            (self.enable_effects, "effects"),
            (self.enable_grayscale_mix, "grayscale_mix"),
            (self.enable_lens_corrections, "lens_corrections"),
            (
                self.enable_mask_group_based_corrections,
                "mask_group_based_corrections",
            ),
            (self.enable_red_eye, "red_eye"),
            (self.enable_retouch, "retouch"),
            (self.enable_transform, "transform"),
            (self.enable_distraction_removal, "distraction_removal"),
        ]
        .into_iter()
        .find_map(|(on, name)| on.then_some(name))
    }
}

/// Exactly one record per channel role (composite, red, green, blue); authored points retained
/// even while `enable_tone_curve` is false.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedCurves {
    pub composite: Curve,
    pub red: Curve,
    pub green: Curve,
    pub blue: Curve,
}

impl ResolvedCurves {
    pub fn identity() -> Self {
        Self {
            composite: Curve::identity(),
            red: Curve::identity(),
            green: Curve::identity(),
            blue: Curve::identity(),
        }
    }
}

/// Authored normalised crop edges (0..1) plus orientation state; only identity variants run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NormalizedCrop {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub angle: f64,
    /// TIFF/EXIF orientation value; 1 = identity.
    pub orientation: u8,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
    pub constrain_aspect_ratio: bool,
    pub constrain_to_warp: bool,
}

impl NormalizedCrop {
    pub const fn full() -> Self {
        Self {
            left: 0.0,
            top: 0.0,
            right: 1.0,
            bottom: 1.0,
            angle: 0.0,
            orientation: 1,
            flip_horizontal: false,
            flip_vertical: false,
            constrain_aspect_ratio: false,
            constrain_to_warp: false,
        }
    }

    fn validate(&self) -> Result<(), DevelopError> {
        let edges = [self.left, self.top, self.right, self.bottom, self.angle];
        if edges.iter().any(|v| !v.is_finite()) {
            return Err(DevelopError::NonFinite);
        }
        if self.angle != 0.0
            || self.orientation != 1
            || self.flip_horizontal
            || self.flip_vertical
            || self.constrain_to_warp
        {
            return Err(DevelopError::UnsupportedCrop);
        }
        let unit = 0.0..=1.0;
        if !unit.contains(&self.left)
            || !unit.contains(&self.top)
            || !unit.contains(&self.right)
            || !unit.contains(&self.bottom)
            || self.left >= self.right
            || self.top >= self.bottom
        {
            return Err(DevelopError::EmptyCrop);
        }
        Ok(())
    }

    /// Derived source bounds `(x0, y0, x1, y1)`: floor/ceil of the authored edges in written
    /// binary64 arithmetic; the authored normalised edges stay the authority.
    pub fn source_bounds(&self, width: u32, height: u32) -> Result<(u32, u32, u32, u32), DevelopError> {
        self.validate()?;
        let (w, h) = (f64::from(width), f64::from(height));
        let x0 = (self.left * w).floor();
        let y0 = (self.top * h).floor();
        let x1 = (self.right * w).ceil();
        let y1 = (self.bottom * h).ceil();
        let (x0, y0, x1, y1) = (x0 as u32, y0 as u32, x1 as u32, y1 as u32);
        if x1 > width || y1 > height || x0 >= x1 || y0 >= y1 {
            return Err(DevelopError::EmptyCrop);
        }
        Ok((x0, y0, x1, y1))
    }
}

/// Existing Prism profile identity (SCPF id + exact ICC byte hash).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileBinding {
    pub profile_id: DomainId,
    pub sha256: [u8; 32],
}

#[derive(Clone, Debug, PartialEq)]
pub struct DevelopRecipe {
    pub process_version: (u32, u32),
    pub engine_version: (u32, u32),
    pub math_token: String,
    pub demosaic_algorithm: String,
    pub white_balance_mode: WbMode,
    pub as_shot_neutral: [f64; 3],
    /// Row-major 3x3 camera -> working-profile RGB.
    pub camera_to_profile_rgb: [f64; 9],
    pub working_profile: ProfileBinding,
    pub output_profile: ProfileBinding,
    /// `stops_ev`; no invented clamp.
    pub exposure_stops: f64,
    pub enables: Enables,
    pub curves: ResolvedCurves,
    /// Exactly 100 is supported.
    pub curve_refine_saturation: u32,
    pub crop: NormalizedCrop,
}

/// Derived, validated numbers used by the pipeline.
pub(crate) struct PreparedRecipe {
    pub gain: [f64; 3],
    pub exposure_gain: f64,
    pub matrix: [f64; 9],
    /// composite, red, green, blue; `None` per role means "skip" (disabled or identity).
    pub curves: [Option<PreparedCurve>; 4],
}

impl DevelopRecipe {
    /// A native 1.0 recipe with identity colour (neutral 1, identity matrix, no exposure, no
    /// crop, all groups disabled). Callers set the real calibration and profile fields.
    pub fn native(
        as_shot_neutral: [f64; 3],
        camera_to_profile_rgb: [f64; 9],
        profile: ProfileBinding,
    ) -> Self {
        Self {
            process_version: PROCESS_VERSION,
            engine_version: ENGINE_VERSION,
            math_token: MATH_TOKEN.to_owned(),
            demosaic_algorithm: DEMOSAIC_ALGORITHM.to_owned(),
            white_balance_mode: WbMode::AsShot,
            as_shot_neutral,
            camera_to_profile_rgb,
            working_profile: profile.clone(),
            output_profile: profile,
            exposure_stops: 0.0,
            enables: Enables::all_disabled(),
            curves: ResolvedCurves::identity(),
            curve_refine_saturation: 100,
            crop: NormalizedCrop::full(),
        }
    }

    pub fn validate(&self) -> Result<(), DevelopError> {
        self.prepare().map(|_| ())
    }

    pub(crate) fn prepare(&self) -> Result<PreparedRecipe, DevelopError> {
        if self.process_version != PROCESS_VERSION || self.engine_version != ENGINE_VERSION {
            return Err(DevelopError::UnsupportedProcessVersion);
        }
        if self.math_token != MATH_TOKEN {
            return Err(DevelopError::UnsupportedMathToken);
        }
        if self.demosaic_algorithm != DEMOSAIC_ALGORITHM {
            return Err(DevelopError::UnsupportedDemosaic);
        }
        if self.white_balance_mode != WbMode::AsShot {
            return Err(DevelopError::UnsupportedWhiteBalance);
        }
        if let Some(name) = self.enables.first_unsupported() {
            return Err(DevelopError::UnsupportedContribution(name));
        }
        if self.curve_refine_saturation != 100 {
            return Err(DevelopError::UnsupportedCurve);
        }
        self.crop.validate()?;
        for profile in [&self.working_profile, &self.output_profile] {
            if profile.profile_id.prefix() != "SCPF" {
                return Err(DevelopError::InvalidProfile);
            }
        }
        if self.working_profile != self.output_profile {
            return Err(DevelopError::UnsupportedProfileTransform);
        }

        // As-shot gains: r = 1/neutral, m = min(r), gain = r/m.
        if self
            .as_shot_neutral
            .iter()
            .any(|n| !n.is_finite() || *n <= 0.0)
        {
            return Err(DevelopError::NonFinite);
        }
        let mut r = [0.0f64; 3];
        for (slot, n) in r.iter_mut().zip(self.as_shot_neutral) {
            *slot = 1.0 / n;
            if !slot.is_finite() || *slot == 0.0 {
                return Err(DevelopError::StageUnderflow);
            }
        }
        let m = r[0].min(r[1]).min(r[2]);
        let mut gain = [0.0f64; 3];
        for (slot, ri) in gain.iter_mut().zip(r) {
            *slot = ri / m;
            if !slot.is_finite() || *slot <= 0.0 {
                return Err(DevelopError::StageOverflow);
            }
        }

        let c = &self.camera_to_profile_rgb;
        if c.iter().any(|v| !v.is_finite()) {
            return Err(DevelopError::NonFinite);
        }
        let det = (c[0] * (c[4] * c[8] - c[5] * c[7]) - c[1] * (c[3] * c[8] - c[5] * c[6]))
            + c[2] * (c[3] * c[7] - c[4] * c[6]);
        if !det.is_finite() || det == 0.0 {
            return Err(DevelopError::NotInvertibleMatrix);
        }

        if !self.exposure_stops.is_finite() {
            return Err(DevelopError::NonFinite);
        }
        let exposure_gain = self.exposure_stops.exp2();
        if !exposure_gain.is_finite() || exposure_gain <= 0.0 {
            return Err(DevelopError::StageOverflow);
        }

        // Curves run only when the group is enabled and the individual curve is not identity.
        let mut curves: [Option<PreparedCurve>; 4] = [None, None, None, None];
        if self.enables.enable_tone_curve {
            let roles = [
                &self.curves.composite,
                &self.curves.red,
                &self.curves.green,
                &self.curves.blue,
            ];
            for (slot, curve) in curves.iter_mut().zip(roles) {
                if !curve.is_identity() {
                    *slot = Some(curve.prepare()?);
                }
            }
        }
        Ok(PreparedRecipe {
            gain,
            exposure_gain,
            matrix: *c,
            curves,
        })
    }
}
