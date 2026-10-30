//! HDR merge of bracketed raw sources (lane SMB-HDR-MERGE-1 slice 3; [STU-RAS-033] merge bracketed
//! exposures to HDR, [STU-RAS-044] native and offline; Lightroom's Photo Merge > HDR entry, parity
//! UNVERIFIED (D5)).
//!
//! Each bracket is a decoded raw ([`DngRaw`]) with the EXIF its container carries
//! ([`crate::dng_write::exif_subset`]: exposure time, f-number, ISO, make, model). The brackets must
//! come from one camera (UniqueCameraModel, EXIF make and model), have one size (frame, default
//! window) and one CFA layout, and differ only in exposure time (equal f-number and ISO); anything
//! else is refused typed before any pixel work.
//!
//! Pipeline (Studio reading, declared): every bracket's default window is cut and normalised by the
//! existing develop stages ([`DngRaw::cfa_plane`], [`crate::demosaic_normalized_threads`]: black
//! subtracted, `1.0` = the bracket's white level, demosaiced, no white balance, no matrix, no
//! exposure, no tone curve, no output transform), so a saturated sensor sample is exactly `1.0` and
//! pigment's clip test is exact; the frames merge in that linear camera space through
//! [`hsk_studio_pigment::hdr_merge::merge_brackets`] with the exposure times as
//! [`Exposure::Seconds`] (depth 32: the demosaiced frames are binary32). The merged camera radiance
//! then takes the reference bracket's develop stage A (its as-shot gain and camera -> working
//! matrix from [`DngRaw::recipe`] with BaselineExposure ignored, the same arithmetic as the tile
//! pipeline) into a linear scene-referred working-space frame that may exceed `1.0`. Merging after
//! white balance would hide sensor clipping behind per-channel gains, hence camera space first.
//! Real camera brackets are NOT_PROVEN (the tests synthesise brackets from one scene).
use crate::color::{self, Mat3};
use crate::dng::{BaselineExposure, DngRaw, PlaneWindow, RangePolicy};
use crate::dng_write::ExifSubset;
use crate::par::Threads;
use crate::pipeline::{Limits, demosaic_normalized_threads};
use crate::recipe::{DevelopRecipe, ProfileBinding, as_shot_gain};
use crate::DevelopError;
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::hdr_merge::{Bracket, Exposure, HDR_MAX_FRAMES, HdrError, HdrLimits, HdrMerged, MergeOptions, merge_brackets};
use hsk_studio_pigment::retouch::RgbaPlane;
use std::fmt;

/// One bracket: a decoded raw and its container's EXIF.
#[derive(Clone, Copy, Debug)]
pub struct RawBracket<'a> {
    pub raw: &'a DngRaw,
    pub exif: &'a ExifSubset,
}

/// Options of [`merge_raw_brackets`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RawHdrOptions {
    /// Pigment merge options (thresholds, reference, alignment, deghost).
    pub merge: MergeOptions,
    /// Samples outside the black..white range: refused or clamped and reported.
    pub range: RangePolicy,
    /// Plane admission limits of the develop stages.
    pub plane_limits: Limits,
    pub threads: Threads,
}
impl Default for RawHdrOptions {
    fn default() -> Self {
        Self {
            merge: MergeOptions::default(),
            range: RangePolicy::ClampReported,
            plane_limits: Limits::LARGE,
            threads: Threads::auto(),
        }
    }
}

/// Result of [`merge_raw_brackets`].
#[derive(Clone, Debug, PartialEq)]
pub struct RawHdrMerged {
    /// Merged demosaiced camera RGB (`1.0` = sensor saturation at the reference exposure) with the
    /// pigment merge facts (reference, relative exposures, shifts, fallback and ghost counts).
    pub camera: HdrMerged,
    /// The merged radiance through the reference bracket's as-shot gain and camera -> working
    /// matrix: linear scene-referred working-space RGB, 32-bit, may exceed `1.0`.
    pub working: RgbaPlane,
    /// The reference bracket's recipe that supplied the gain and matrix.
    pub recipe: DevelopRecipe,
    /// The window every bracket was cut to (raw coordinates).
    pub window: PlaneWindow,
}

/// Typed failure of [`merge_raw_brackets`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawHdrError {
    /// UniqueCameraModel or EXIF make/model differ from bracket 0's.
    CameraMismatch { index: usize },
    /// Frame size or default window differs from bracket 0's.
    SizeMismatch { index: usize },
    /// CFA layout differs from bracket 0's.
    CfaMismatch { index: usize },
    /// An exposure setting other than the exposure time differs (`f_number` or `iso`).
    SettingsDiffer { index: usize, field: &'static str },
    Develop(DevelopError),
    Merge(HdrError),
}
impl RawHdrError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::CameraMismatch { .. } => "raw_hdr_camera_mismatch",
            Self::SizeMismatch { .. } => "raw_hdr_size_mismatch",
            Self::CfaMismatch { .. } => "raw_hdr_cfa_mismatch",
            Self::SettingsDiffer { .. } => "raw_hdr_settings_differ",
            Self::Develop(e) => e.code(),
            Self::Merge(e) => e.code(),
        }
    }
}
impl fmt::Display for RawHdrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SettingsDiffer { index, field } => write!(f, "raw_hdr_settings_differ: bracket {index} {field}"),
            Self::Merge(e) => fmt::Display::fmt(e, f),
            other => f.write_str(other.code()),
        }
    }
}
impl std::error::Error for RawHdrError {}
impl From<DevelopError> for RawHdrError {
    fn from(e: DevelopError) -> Self {
        Self::Develop(e)
    }
}
impl From<HdrError> for RawHdrError {
    fn from(e: HdrError) -> Self {
        Self::Merge(e)
    }
}

/// Seconds of an EXIF ExposureTime rational (a zero denominator gives a non-finite value, which
/// pigment refuses as `hdr_invalid_exposure`).
fn seconds(r: (u32, u32)) -> f64 {
    f64::from(r.0) / f64::from(r.1)
}

/// Refuse brackets that are not one camera, size, CFA layout and aperture/ISO (module docs).
fn admit(brackets: &[RawBracket<'_>]) -> Result<(), RawHdrError> {
    let n = brackets.len();
    if n < 2 {
        return Err(HdrError::TooFewFrames { count: n }.into());
    }
    if n > HDR_MAX_FRAMES {
        return Err(HdrError::TooManyFrames {
            count: n,
            limit: HDR_MAX_FRAMES,
        }
        .into());
    }
    let first = brackets[0];
    for (index, b) in brackets.iter().enumerate() {
        if b.raw.unique_camera_model != first.raw.unique_camera_model
            || b.exif.make != first.exif.make
            || b.exif.model != first.exif.model
        {
            return Err(RawHdrError::CameraMismatch { index });
        }
        if (b.raw.width, b.raw.height) != (first.raw.width, first.raw.height)
            || b.raw.default_window() != first.raw.default_window()
        {
            return Err(RawHdrError::SizeMismatch { index });
        }
        if b.raw.cfa_pattern != first.raw.cfa_pattern {
            return Err(RawHdrError::CfaMismatch { index });
        }
        if b.exif.f_number != first.exif.f_number {
            return Err(RawHdrError::SettingsDiffer { index, field: "f_number" });
        }
        if b.exif.iso != first.exif.iso {
            return Err(RawHdrError::SettingsDiffer { index, field: "iso" });
        }
        if b.exif.exposure_time.is_none() {
            return Err(HdrError::MissingExposure { index }.into());
        }
    }
    Ok(())
}

/// Merge bracketed raws to one 32-bit linear HDR frame (module docs). `xyz_d50_to_profile` and
/// `profile` are those of [`DngRaw::recipe`] for the working space.
pub fn merge_raw_brackets(
    brackets: &[RawBracket<'_>],
    xyz_d50_to_profile: &Mat3,
    profile: ProfileBinding,
    options: &RawHdrOptions,
    limits: &HdrLimits,
    cancel: &CancellationToken,
) -> Result<RawHdrMerged, RawHdrError> {
    admit(brackets)?;
    let window = brackets[0].raw.default_window();
    let bounds = hsk_studio_pigment::Rect {
        x: 0,
        y: 0,
        width: window.width,
        height: window.height,
    };
    let area = u64::from(window.width) * u64::from(window.height);
    let frame_bytes = area.checked_mul(16).ok_or(HdrError::Overflow)?;
    let all_bytes = frame_bytes
        .checked_mul(brackets.len() as u64 + 2)
        .ok_or(HdrError::Overflow)?;
    if all_bytes > limits.max_bytes {
        return Err(HdrError::OverBudget {
            needed: all_bytes,
            limit: limits.max_bytes,
        }
        .into());
    }
    let mut frames = Vec::with_capacity(brackets.len());
    for (i, b) in brackets.iter().enumerate() {
        cancel.check().map_err(DevelopError::from)?;
        let (plane, _report) = b.raw.cfa_plane(window, options.range, i as u64, options.plane_limits)?;
        let rgb = demosaic_normalized_threads(&plane, options.threads, cancel)?;
        frames.push(RgbaPlane {
            bounds,
            data: rgb.into_iter().map(|p| [p[0], p[1], p[2], 1.0]).collect(),
        });
    }
    let pigment: Vec<Bracket<'_>> = frames
        .iter()
        .zip(brackets)
        .map(|(frame, b)| Bracket {
            frame,
            exposure: b.exif.exposure_time.map(|t| Exposure::Seconds(seconds(t))),
            depth: 32,
        })
        .collect();
    // The pigment budget covers the merge itself; the frames above are charged here.
    let merge_limits = HdrLimits {
        max_bytes: limits.max_bytes - (all_bytes - frame_bytes),
        max_work: limits.max_work,
    };
    let camera = merge_brackets(&pigment, &options.merge, &merge_limits, cancel)?;
    drop(pigment);
    drop(frames);

    let (recipe, _solution) = brackets[camera.reference]
        .raw
        .recipe(xyz_d50_to_profile, profile, BaselineExposure::Ignore)?;
    recipe.validate()?;
    let gain = as_shot_gain(recipe.as_shot_neutral)?;
    let matrix = recipe.camera_to_profile_rgb;
    let mut data = Vec::with_capacity(camera.frame.data.len());
    for (p, px) in camera.frame.data.iter().enumerate() {
        if p % window.width as usize == 0 {
            cancel.check().map_err(DevelopError::from)?;
        }
        let balanced = [
            gain[0] * f64::from(px[0]),
            gain[1] * f64::from(px[1]),
            gain[2] * f64::from(px[2]),
        ];
        let w = color::mul_vec(&matrix, &balanced);
        let out = [w[0] as f32, w[1] as f32, w[2] as f32, px[3]];
        if !out[..3].iter().all(|v| v.is_finite()) {
            return Err(DevelopError::StageOverflow.into());
        }
        data.push(out);
    }
    Ok(RawHdrMerged {
        working: RgbaPlane {
            bounds: camera.frame.bounds,
            data,
        },
        camera,
        recipe,
        window,
    })
}
