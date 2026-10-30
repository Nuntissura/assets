//! Merge bracketed exposures to a 32-bit HDR radiance frame (lane SMB-HDR-MERGE-1).
//!
//! Spec basis (module 14): [STU-RAS-033] "a merge-bracketed-exposures-to-HDR operation (with ghost
//! removal and tone-mapping); tone-mapping between 32/16/8-bit" and "32-bit floating point MUST store
//! HDR luminance beyond display range"; [STU-RAS-149] every command declares its supported depth set
//! as inspectable capability metadata and a command at an unsupported depth fails typed, naming the
//! depth and the supported set, never silently promoting; [STU-RAS-044] HDR merge is native,
//! on-device and offline (this module is plain CPU code with no provider).
//!
//! Inputs are N >= 2 linear-light RGB frames ([`RgbaPlane`], straight alpha) of equal bounds, each
//! with an exposure ([`Exposure`]: EV offset in stops or exposure time in seconds; one unit per
//! bracket). Sample value `1.0` is sensor saturation. Output is a linear-light radiance frame in the
//! scale of the reference exposure ([`MergeOptions::reference`], default the median exposure):
//! where the reference is unclipped the output equals the reference sample, elsewhere it may exceed
//! `1.0` (32-bit storage beyond display range).
//!
//! Merge (Studio reading, declared): the weighted radiance average of Debevec and Malik,
//! "Recovering High Dynamic Range Radiance Maps from Photographs", SIGGRAPH 1997 (eq. 6, hat weight
//! `w(z)`), taken in linear light because the inputs are already linear (no response-curve recovery,
//! arithmetic mean of `z / relative_exposure` instead of the paper's log-domain mean). The hat runs
//! on the pixel's largest colour channel over `(black, clip)` ([`HDR_BLACK_DEFAULT`],
//! [`HDR_CLIP_DEFAULT`]): `t = (m - black) / (clip - black)`, `w = 1 - |2t - 1|`, zero at and
//! outside both ends, so near-black noise and near-clip samples carry no weight and all three
//! channels of a pixel share one weight (no hue shift from a single clipped channel). When every
//! sample of a pixel has zero weight: if any unclipped sample exists (all near black) the longest
//! unclipped exposure supplies the pixel; only when every sample is clipped does the shortest
//! exposure supply it (a lower bound of the radiance). Clipped samples therefore never contribute
//! when an unclipped sample exists. Output alpha is the reference frame's alpha.
//!
//! Photoshop's Merge to HDR Pro and Lightroom's HDR merge are UNVERIFIED (D5, no oracle): the
//! weights, thresholds and defaults here are Studio readings, not parity claims.
//!
//! Determinism: one thread, fixed frame and pixel order, binary64 accumulation and one rounding to
//! binary32 per channel. EV offsets become scales through `f64::exp2` (exact for whole stops);
//! seconds become scales by one IEEE division.
//! Hostile inputs (V4-01/02): frame count, bounds, lengths, finiteness, alpha range, exposures and
//! options are checked (never clamped); bytes and work are refused against [`HdrLimits`] before
//! allocation.
use crate::Rect;
use crate::retouch::RgbaPlane;
use hsk_studio_accord::CancellationToken;
use std::fmt;

/// Largest bracket size.
pub const HDR_MAX_FRAMES: usize = 16;
/// Default near-black threshold on the largest channel (half an 8-bit level of linear light).
pub const HDR_BLACK_DEFAULT: f32 = 1.0 / 512.0;
/// Default near-clip threshold on the largest channel.
pub const HDR_CLIP_DEFAULT: f32 = 0.98;
/// Largest exposure distance of any frame from the reference, in stops.
pub const HDR_MAX_EXPOSURE_STOPS: f64 = 30.0;
/// Depth of every merge result ([STU-RAS-033] 32-bit floating point).
pub const HDR_OUTPUT_DEPTH: u8 = 32;

/// Which depth a capability check refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepthRole {
    /// The depth of a source document or frame.
    Source,
    /// The depth the command produces.
    Target,
}
impl DepthRole {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Target => "target",
        }
    }
}

/// [STU-RAS-149] inspectable depth capability of one command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepthCapability {
    pub command: &'static str,
    /// Depths (bits per channel) the command accepts as input.
    pub source_depths: &'static [u8],
    /// Depths the command produces.
    pub target_depths: &'static [u8],
}
impl DepthCapability {
    /// Typed refusal naming the depth and the supported set ([STU-RAS-149]); never promotes.
    pub fn admit(&self, role: DepthRole, depth: u8) -> Result<(), HdrError> {
        let supported = match role {
            DepthRole::Source => self.source_depths,
            DepthRole::Target => self.target_depths,
        };
        if supported.contains(&depth) {
            Ok(())
        } else {
            Err(HdrError::UnsupportedDepth {
                command: self.command,
                role,
                depth,
                supported,
            })
        }
    }
}

/// [`merge_brackets`]: 8-, 16- or 32-bit sources (carried as linear binary32), 32-bit result.
pub const HDR_MERGE_CAPABILITY: DepthCapability = DepthCapability {
    command: "hdr_merge",
    source_depths: &[8, 16, 32],
    target_depths: &[32],
};

/// Exposure of one bracket frame. All frames of a bracket use the same unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Exposure {
    /// Exposure value offset in stops from any common origin; `+1` gathers twice the light.
    Ev(f64),
    /// Exposure time in seconds at equal aperture and gain (Studio reading).
    Seconds(f64),
}

/// One frame of a bracket.
#[derive(Clone, Copy, Debug)]
pub struct Bracket<'a> {
    /// Linear-light RGB, straight alpha; `1.0` = saturation.
    pub frame: &'a RgbaPlane,
    /// `None` is refused ([`HdrError::MissingExposure`]).
    pub exposure: Option<Exposure>,
    /// Bits per channel of the source ([`HDR_MERGE_CAPABILITY`]).
    pub depth: u8,
}

/// Merge options (defaults are Studio readings).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MergeOptions {
    /// Largest-channel value at or below which a sample carries no weight.
    pub black: f32,
    /// Largest-channel value at or above which a sample is clipped.
    pub clip: f32,
    /// Index of the reference frame; `None` = the median exposure (the darker middle for even N).
    pub reference: Option<usize>,
}
impl Default for MergeOptions {
    fn default() -> Self {
        Self {
            black: HDR_BLACK_DEFAULT,
            clip: HDR_CLIP_DEFAULT,
            reference: None,
        }
    }
}

/// Byte and work budget (work = sample visits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HdrLimits {
    pub max_bytes: u64,
    pub max_work: u64,
}
impl HdrLimits {
    /// 1 GiB, 2^36 sample visits.
    pub const DEFAULT: Self = Self {
        max_bytes: 1 << 30,
        max_work: 1 << 36,
    };
}
impl Default for HdrLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Merge result.
#[derive(Clone, Debug, PartialEq)]
pub struct HdrMerged {
    /// Linear-light radiance in the reference frame's scale; colour may exceed `1.0`.
    pub frame: RgbaPlane,
    /// Always [`HDR_OUTPUT_DEPTH`].
    pub depth: u8,
    pub reference: usize,
    /// Light gathered by each frame relative to the reference (`2^(ev - ev_ref)` or `t / t_ref`).
    pub relative_exposure: Vec<f64>,
    /// Pixels where every sample had zero weight (supplied by the fallback rule).
    pub fallback_pixels: u64,
}

/// Typed failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HdrError {
    TooFewFrames { count: usize },
    TooManyFrames { count: usize, limit: usize },
    MissingExposure { index: usize },
    /// Non-finite EV, non-positive or non-finite seconds, or an exposure further than
    /// [`HDR_MAX_EXPOSURE_STOPS`] from the reference.
    InvalidExposure { index: usize },
    /// The frame's exposure unit differs from frame 0's.
    MixedExposureUnits { index: usize },
    /// The frame's bounds differ from frame 0's.
    SizeMismatch { index: usize },
    /// The frame's sample count disagrees with its bounds, or the bounds are empty or overflow.
    Shape { index: usize },
    /// A colour sample is non-finite or an alpha is outside `[0, 1]`.
    NonFinite { index: usize },
    /// An option is outside its domain.
    Parameter,
    /// [STU-RAS-149]: `command` does not support `depth` in `role`; `supported` is the set.
    UnsupportedDepth {
        command: &'static str,
        role: DepthRole,
        depth: u8,
        supported: &'static [u8],
    },
    /// Bytes or work above the limits (refused before allocation).
    OverBudget { needed: u64, limit: u64 },
    Overflow,
    Canceled,
}
impl HdrError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::TooFewFrames { .. } => "hdr_too_few_frames",
            Self::TooManyFrames { .. } => "hdr_too_many_frames",
            Self::MissingExposure { .. } => "hdr_missing_exposure",
            Self::InvalidExposure { .. } => "hdr_invalid_exposure",
            Self::MixedExposureUnits { .. } => "hdr_mixed_exposure_units",
            Self::SizeMismatch { .. } => "hdr_size_mismatch",
            Self::Shape { .. } => "hdr_shape",
            Self::NonFinite { .. } => "hdr_nonfinite",
            Self::Parameter => "hdr_parameter",
            Self::UnsupportedDepth { .. } => "unsupported_depth",
            Self::OverBudget { .. } => "hdr_over_budget",
            Self::Overflow => "hdr_overflow",
            Self::Canceled => "canceled",
        }
    }
}
impl fmt::Display for HdrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedDepth {
                command,
                role,
                depth,
                supported,
            } => write!(
                f,
                "unsupported_depth: {command} does not support {} depth {depth}; supported {supported:?}",
                role.code()
            ),
            other => f.write_str(other.code()),
        }
    }
}
impl std::error::Error for HdrError {}

/// Checked shape of a frame: sample count of its bounds.
pub(crate) fn frame_area(frame: &RgbaPlane, index: usize) -> Result<usize, HdrError> {
    let r = frame.bounds;
    if r.width == 0 || r.height == 0 || r.end().is_err() {
        return Err(HdrError::Shape { index });
    }
    let n = usize::try_from(r.area()).map_err(|_| HdrError::Overflow)?;
    if frame.data.len() != n {
        return Err(HdrError::Shape { index });
    }
    Ok(n)
}

/// Every colour channel finite, alpha in `[0, 1]`.
pub(crate) fn check_frame_samples(frame: &RgbaPlane, index: usize) -> Result<(), HdrError> {
    for p in &frame.data {
        if !(p[0].is_finite() && p[1].is_finite() && p[2].is_finite() && (0.0..=1.0).contains(&p[3])) {
            return Err(HdrError::NonFinite { index });
        }
    }
    Ok(())
}

pub(crate) fn admit(needed_bytes: u64, needed_work: u64, limits: &HdrLimits) -> Result<(), HdrError> {
    if needed_bytes > limits.max_bytes {
        return Err(HdrError::OverBudget {
            needed: needed_bytes,
            limit: limits.max_bytes,
        });
    }
    if needed_work > limits.max_work {
        return Err(HdrError::OverBudget {
            needed: needed_work,
            limit: limits.max_work,
        });
    }
    Ok(())
}

pub(crate) fn check_cancel(cancel: &CancellationToken) -> Result<(), HdrError> {
    cancel.check().map_err(|_| HdrError::Canceled)
}

/// Debevec-Malik hat on the largest channel `m` (module docs).
fn hat(m: f64, black: f64, clip: f64) -> f64 {
    if !(m > black && m < clip) {
        return 0.0;
    }
    let t = (m - black) / (clip - black);
    1.0 - (2.0 * t - 1.0).abs()
}

fn largest(p: [f32; 4]) -> f64 {
    f64::from(p[0].max(p[1]).max(p[2]))
}

/// Validated bracket: bounds, per-frame relative exposure and reference index.
struct Admitted {
    bounds: Rect,
    area: usize,
    reference: usize,
    relative: Vec<f64>,
}

fn admit_bracket(brackets: &[Bracket<'_>], options: &MergeOptions) -> Result<Admitted, HdrError> {
    let n = brackets.len();
    if n < 2 {
        return Err(HdrError::TooFewFrames { count: n });
    }
    if n > HDR_MAX_FRAMES {
        return Err(HdrError::TooManyFrames {
            count: n,
            limit: HDR_MAX_FRAMES,
        });
    }
    let (black, clip) = (options.black, options.clip);
    if !(black.is_finite() && clip.is_finite() && black >= 0.0 && black < clip) {
        return Err(HdrError::Parameter);
    }
    if options.reference.is_some_and(|r| r >= n) {
        return Err(HdrError::Parameter);
    }
    let bounds = brackets[0].frame.bounds;
    let mut area = 0;
    for (i, b) in brackets.iter().enumerate() {
        HDR_MERGE_CAPABILITY.admit(DepthRole::Source, b.depth)?;
        if b.frame.bounds != bounds {
            return Err(HdrError::SizeMismatch { index: i });
        }
        area = frame_area(b.frame, i)?;
    }
    // Exposure keys in one unit: stops for EV, seconds for times.
    let mut keys = Vec::with_capacity(n);
    let seconds = matches!(brackets[0].exposure, Some(Exposure::Seconds(_)));
    for (i, b) in brackets.iter().enumerate() {
        let key = match b.exposure {
            None => return Err(HdrError::MissingExposure { index: i }),
            Some(Exposure::Ev(v)) if !seconds => v,
            Some(Exposure::Seconds(t)) if seconds => t,
            Some(_) => return Err(HdrError::MixedExposureUnits { index: i }),
        };
        if !key.is_finite() || (seconds && key <= 0.0) {
            return Err(HdrError::InvalidExposure { index: i });
        }
        keys.push(key);
    }
    let reference = match options.reference {
        Some(r) => r,
        None => {
            let mut order: Vec<usize> = (0..n).collect();
            order.sort_by(|&a, &b| keys[a].total_cmp(&keys[b]).then(a.cmp(&b)));
            order[(n - 1) / 2]
        }
    };
    let limit = HDR_MAX_EXPOSURE_STOPS.exp2();
    let mut relative = Vec::with_capacity(n);
    for (i, &k) in keys.iter().enumerate() {
        let s = if seconds {
            k / keys[reference]
        } else {
            let d = k - keys[reference];
            if d.abs() > HDR_MAX_EXPOSURE_STOPS {
                return Err(HdrError::InvalidExposure { index: i });
            }
            d.exp2()
        };
        if !(s.is_finite() && s > 0.0 && s <= limit && s >= 1.0 / limit) {
            return Err(HdrError::InvalidExposure { index: i });
        }
        relative.push(s);
    }
    for (i, b) in brackets.iter().enumerate() {
        check_frame_samples(b.frame, i)?;
    }
    Ok(Admitted {
        bounds,
        area,
        reference,
        relative,
    })
}

/// Merge a bracket into one 32-bit linear radiance frame (module docs).
pub fn merge_brackets(
    brackets: &[Bracket<'_>],
    options: &MergeOptions,
    limits: &HdrLimits,
    cancel: &CancellationToken,
) -> Result<HdrMerged, HdrError> {
    let a = admit_bracket(brackets, options)?;
    let area = a.area as u64;
    let n = brackets.len() as u64;
    let bytes = area.checked_mul(16).ok_or(HdrError::Overflow)?;
    let work = area.checked_mul(n).ok_or(HdrError::Overflow)?;
    admit(bytes, work, limits)?;
    check_cancel(cancel)?;

    let black = f64::from(options.black);
    let clip = f64::from(options.clip);
    let width = a.bounds.width as usize;
    let mut out = Vec::with_capacity(a.area);
    let mut fallback_pixels = 0u64;
    for (p, reference_sample) in brackets[a.reference].frame.data.iter().enumerate() {
        if p % width == 0 {
            check_cancel(cancel)?;
        }
        let mut weight_sum = 0.0f64;
        let mut acc = [0.0f64; 3];
        // Fallbacks: longest unclipped (near-black) exposure, else shortest clipped exposure.
        let mut dark: Option<usize> = None;
        let mut clipped: Option<usize> = None;
        for (i, b) in brackets.iter().enumerate() {
            let z = b.frame.data[p];
            let m = largest(z);
            let w = hat(m, black, clip);
            if w > 0.0 {
                let s = a.relative[i];
                weight_sum += w;
                for (sum, v) in acc.iter_mut().zip(z) {
                    *sum += w * (f64::from(v) / s);
                }
            } else if m >= clip {
                if clipped.is_none_or(|j| a.relative[i] < a.relative[j]) {
                    clipped = Some(i);
                }
            } else if dark.is_none_or(|j| a.relative[i] > a.relative[j]) {
                dark = Some(i);
            }
        }
        let rgb = if weight_sum > 0.0 {
            [acc[0] / weight_sum, acc[1] / weight_sum, acc[2] / weight_sum]
        } else {
            fallback_pixels += 1;
            // At least one of the two exists: every sample is either weighted, dark or clipped.
            let i = dark.or(clipped).ok_or(HdrError::Overflow)?;
            let z = brackets[i].frame.data[p];
            let s = a.relative[i];
            [f64::from(z[0]) / s, f64::from(z[1]) / s, f64::from(z[2]) / s]
        };
        let px = [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32, reference_sample[3]];
        if !px[..3].iter().all(|v| v.is_finite()) {
            return Err(HdrError::Overflow);
        }
        out.push(px);
    }
    Ok(HdrMerged {
        frame: RgbaPlane {
            bounds: a.bounds,
            data: out,
        },
        depth: HDR_OUTPUT_DEPTH,
        reference: a.reference,
        relative_exposure: a.relative,
        fallback_pixels,
    })
}
