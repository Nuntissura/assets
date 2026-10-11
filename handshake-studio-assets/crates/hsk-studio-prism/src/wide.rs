//! Wide ICC profile admission and conversion (SMB-PRISM-ENG-3 and the engine lanes built on it).
//!
//! Strictly additive beside the legacy matrix-only path: `MAX_PROFILE_BYTES`, `Prism`, `ColorEngine`
//! and `AdmittedPrism` are untouched. This path admits ICC v2/v4 matrix/TRC, `mAB ` and `mft1/mft2`
//! profiles for Gray, RGB and CMYK devices with an XYZ or Lab PCS, up to `MAX_WIDE_PROFILE_BYTES`.
//!
//! Conversion is a Prism-evaluated source stage (device -> PCS XYZ / D50, f64, unclamped), the
//! Prism PCS stage (black point compensation or absolute media-white scaling, applied exactly once)
//! and a Prism-evaluated destination stage. moxcms 0.9.1 parses the profiles; it runs no transform on
//! this path (SMB-PRISM-ENG-6, `icc_eval.rs`, diagnostics/prism-eng6.md). Matrix/TRC and Gray pairs
//! fuse both matrices and the PCS map into one affine 3x3 between the curves.
use crate::icc_eval::{FromPcs, Fused, ToPcs};
use crate::{EvaluationOptions, Intent, ProfileInput, profile_hash};
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{Budget, FailureCode, Observation, Observe, Outcome, SinkPort};
use moxcms::{ColorProfile, DataColorSpace, ParsingOptions, ProfileClass};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Numeric engine of the wide path, carried by every `WideReceipt` (the legacy path keeps
/// `ENGINE_NAME`/`ENGINE_VERSION`, moxcms). Version bumps whenever the arithmetic changes.
pub const WIDE_ENGINE_NAME: &str = "hsk-studio-prism-icc";
pub const WIDE_ENGINE_VERSION: &str = "1";

/// Upper bound for the wide path: real CMYK profiles are 0.56-0.72 MB and Adobe's HLG video profile
/// is 0.97 MB; 2 MiB leaves headroom without admitting unbounded input. The legacy path keeps
/// `MAX_PROFILE_BYTES` (64 KiB).
pub const MAX_WIDE_PROFILE_BYTES: usize = 2 * 1024 * 1024;
/// Largest CLUT (grid points per input channel raised to the channel count) accepted at parse time.
pub const MAX_WIDE_CLUT_ENTRIES: usize = 1_048_576;
/// Largest `curv` table accepted at parse time.
pub const MAX_WIDE_TRC_ENTRIES: usize = 65_536;
pub const MAX_WIDE_TAGS: usize = 512;
/// Pixels per request: large enough to convert a frame in a few calls (24 MP = 12 calls), bounded so
/// one call holds at most about 25 MB of output.
pub const MAX_WIDE_PIXELS: usize = 1 << 21;
const CHUNK_PIXELS: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceSpace {
    Gray,
    Rgb,
    Cmyk,
}
impl DeviceSpace {
    pub const fn channels(self) -> usize {
        match self {
            Self::Gray => 1,
            Self::Rgb => 3,
            Self::Cmyk => 4,
        }
    }
    pub const fn code(self) -> &'static str {
        match self {
            Self::Gray => "gray",
            Self::Rgb => "rgb",
            Self::Cmyk => "cmyk",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcsKind {
    Xyz,
    Lab,
}
/// Admission order: v4 matrix, v2 `curv` matrix, `mAB `, then `mft1/mft2`; anything else is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileKind {
    /// RGB matrix/TRC; `TrcKind` tells whether the curves are `para` (v4) or `curv`.
    MatrixTrc,
    /// Gray `kTRC`.
    GrayTrc,
    /// ICC v4 multi-dimensional `mAB `/`mBA ` tables.
    MultiDimensional,
    /// Legacy `mft1`/`mft2` tables (CMYK printers, v2 RGB devices).
    LegacyLut,
}
impl ProfileKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::MatrixTrc => "matrix_trc",
            Self::GrayTrc => "gray_trc",
            Self::MultiDimensional => "multi_dimensional",
            Self::LegacyLut => "legacy_lut",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrcKind {
    Parametric,
    Curve,
}
/// Which intent tables a profile carries in one direction (A2B or B2A).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct IntentTables {
    pub perceptual: bool,
    pub colorimetric: bool,
    pub saturation: bool,
}
impl IntentTables {
    const ALL: Self = Self {
        perceptual: true,
        colorimetric: true,
        saturation: true,
    };
    pub const fn has_all(self) -> bool {
        self.perceptual && self.colorimetric && self.saturation
    }
    pub fn has(self, intent: Intent) -> bool {
        match intent {
            Intent::Perceptual => self.perceptual,
            Intent::Saturation => self.saturation,
            Intent::RelativeColorimetric | Intent::AbsoluteColorimetric => self.colorimetric,
        }
    }
}

/// Typed refusals of the wide path. Codes are stable snake_case strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WideError {
    InvalidProfileId,
    ProfileLimit,
    HashMismatch,
    MalformedProfile,
    UnsupportedVersion,
    UnsupportedClass,
    UnsupportedColorSpace,
    UnsupportedPcs,
    UnsupportedTagType,
    UnsupportedProfile,
    SingularMatrix,
    MissingIntentTable,
    MissingMediaWhite,
    UnsupportedBpc,
    UnsupportedIntent,
    PixelLimit,
    ChannelCount,
    NonfiniteChannel,
    ChannelRange,
    RevisionMismatch,
    ContextMismatch,
    CacheFull,
    InvalidBudget,
    Canceled,
    EngineFailure,
    NonfiniteOutput,
    DeliveryFailed,
}
impl WideError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidProfileId => "invalid_profile_id",
            Self::ProfileLimit => "profile_limit",
            Self::HashMismatch => "hash_mismatch",
            Self::MalformedProfile => "malformed_profile",
            Self::UnsupportedVersion => "unsupported_version",
            Self::UnsupportedClass => "unsupported_class",
            Self::UnsupportedColorSpace => "unsupported_color_space",
            Self::UnsupportedPcs => "unsupported_pcs",
            Self::UnsupportedTagType => "unsupported_tag_type",
            Self::UnsupportedProfile => "unsupported_profile",
            Self::SingularMatrix => "singular_matrix",
            Self::MissingIntentTable => "missing_intent_table",
            Self::MissingMediaWhite => "missing_media_white",
            Self::UnsupportedBpc => "unsupported_bpc",
            Self::UnsupportedIntent => "unsupported_intent",
            Self::PixelLimit => "pixel_limit",
            Self::ChannelCount => "channel_count",
            Self::NonfiniteChannel => "nonfinite_channel",
            Self::ChannelRange => "channel_range",
            Self::RevisionMismatch => "revision_mismatch",
            Self::ContextMismatch => "context_mismatch",
            Self::CacheFull => "cache_full",
            Self::InvalidBudget => "invalid_budget",
            Self::Canceled => "canceled",
            Self::EngineFailure => "engine_failure",
            Self::NonfiniteOutput => "nonfinite_output",
            Self::DeliveryFailed => "delivery_failed",
        }
    }
    fn outcome(self) -> Outcome {
        match self {
            Self::Canceled => Outcome::Canceled,
            Self::UnsupportedVersion
            | Self::UnsupportedClass
            | Self::UnsupportedColorSpace
            | Self::UnsupportedPcs
            | Self::UnsupportedTagType
            | Self::UnsupportedProfile
            | Self::MissingIntentTable
            | Self::UnsupportedBpc
            | Self::UnsupportedIntent => Outcome::Failure(FailureCode::Unsupported),
            Self::CacheFull | Self::EngineFailure => Outcome::Failure(FailureCode::Unavailable),
            _ => Outcome::Failure(FailureCode::Validation),
        }
    }
}
impl std::fmt::Display for WideError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for WideError {}

/// Result of pure wide-profile validation; only admission can construct it.
#[derive(Clone, Debug, PartialEq)]
pub struct WideProfileDescriptor {
    pub profile_id: DomainId,
    pub sha256: [u8; 32],
    pub bytes: usize,
    pub kind: ProfileKind,
    pub trc: Option<TrcKind>,
    pub device: DeviceSpace,
    pub pcs: PcsKind,
    pub version_major: u8,
    pub device_to_pcs: IntentTables,
    pub pcs_to_device: IntentTables,
    /// `wtpt` media white point (PCS XYZ), when the profile carries one.
    pub media_white: Option<[f64; 3]>,
}

struct Tag {
    sig: [u8; 4],
    kind: [u8; 4],
    offset: usize,
    size: usize,
}
fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn s15(b: &[u8], at: usize) -> f64 {
    f64::from(i32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])) / 65536.0
}

fn frame(b: &[u8]) -> Result<Vec<Tag>, WideError> {
    if b.len() < 132 || be32(b, 0) as usize != b.len() || &b[36..40] != b"acsp" {
        return Err(WideError::MalformedProfile);
    }
    let count = be32(b, 128) as usize;
    if count > MAX_WIDE_TAGS {
        return Err(WideError::MalformedProfile);
    }
    let table_end = count
        .checked_mul(12)
        .and_then(|n| n.checked_add(132))
        .filter(|end| *end <= b.len())
        .ok_or(WideError::MalformedProfile)?;
    let mut tags: Vec<Tag> = Vec::with_capacity(count);
    for entry in b[132..table_end].chunks_exact(12) {
        let sig = [entry[0], entry[1], entry[2], entry[3]];
        let (offset, size) = (be32(entry, 4) as usize, be32(entry, 8) as usize);
        let end = offset.checked_add(size).ok_or(WideError::MalformedProfile)?;
        if offset < table_end || size < 8 || end > b.len() || tags.iter().any(|t| t.sig == sig) {
            return Err(WideError::MalformedProfile);
        }
        tags.push(Tag {
            sig,
            kind: [b[offset], b[offset + 1], b[offset + 2], b[offset + 3]],
            offset,
            size,
        });
    }
    Ok(tags)
}

/// Pure admission: bounded framing checks, typed refusal of everything outside the supported
/// families, then moxcms parsing. No transform, cache entry or telemetry.
/// Identity, size and exact-bytes checks only (no parsing): everything a cache hit must re-prove.
pub(crate) fn verify(input: ProfileInput<'_>) -> Result<(), WideError> {
    if input.profile_id.prefix() != "SCPF" {
        return Err(WideError::InvalidProfileId);
    }
    if input.bytes.len() > MAX_WIDE_PROFILE_BYTES {
        return Err(WideError::ProfileLimit);
    }
    if profile_hash(input.bytes) != input.expected_sha256 {
        return Err(WideError::HashMismatch);
    }
    Ok(())
}
pub(crate) fn admit(
    input: ProfileInput<'_>,
) -> Result<(ColorProfile, WideProfileDescriptor), WideError> {
    verify(input)?;
    let b = input.bytes;
    let tags = frame(b)?;
    let major = b[8];
    if major != 2 && major != 4 {
        return Err(WideError::UnsupportedVersion);
    }
    if !matches!(&b[12..16], b"scnr" | b"mntr" | b"prtr" | b"spac") {
        return Err(WideError::UnsupportedClass);
    }
    let device = match &b[16..20] {
        b"GRAY" => DeviceSpace::Gray,
        b"RGB " => DeviceSpace::Rgb,
        b"CMYK" => DeviceSpace::Cmyk,
        _ => return Err(WideError::UnsupportedColorSpace),
    };
    let pcs = match &b[20..24] {
        b"XYZ " => PcsKind::Xyz,
        b"Lab " => PcsKind::Lab,
        _ => return Err(WideError::UnsupportedPcs),
    };
    let find = |sig: &[u8; 4]| tags.iter().find(|t| &t.sig == sig);
    // Multi-process elements (D2B/B2D) change colour meaning and are not interpreted: refuse.
    if tags
        .iter()
        .any(|t| matches!(&t.sig, b"D2B0" | b"D2B1" | b"D2B2" | b"D2B3" | b"B2D0" | b"B2D1" | b"B2D2" | b"B2D3"))
    {
        return Err(WideError::UnsupportedTagType);
    }
    let lut_sigs: [&[u8; 4]; 6] = [b"A2B0", b"A2B1", b"A2B2", b"B2A0", b"B2A1", b"B2A2"];
    let mut lut_kind: Option<ProfileKind> = None;
    for sig in lut_sigs {
        if let Some(t) = find(sig) {
            let kind = match &t.kind {
                b"mAB " | b"mBA " => ProfileKind::MultiDimensional,
                b"mft1" | b"mft2" => ProfileKind::LegacyLut,
                _ => return Err(WideError::UnsupportedTagType),
            };
            lut_kind.get_or_insert(kind);
        }
    }
    let tables = |a: [&[u8; 4]; 3]| IntentTables {
        perceptual: find(a[0]).is_some(),
        colorimetric: find(a[1]).is_some(),
        saturation: find(a[2]).is_some(),
    };
    let (kind, trc) = if let Some(kind) = lut_kind {
        (kind, None)
    } else if device == DeviceSpace::Gray && find(b"kTRC").is_some() {
        (ProfileKind::GrayTrc, None)
    } else if device == DeviceSpace::Rgb
        && [b"rXYZ", b"gXYZ", b"bXYZ", b"rTRC", b"gTRC", b"bTRC"]
            .iter()
            .all(|s| find(s).is_some())
    {
        let parametric = [b"rTRC", b"gTRC", b"bTRC"]
            .iter()
            .all(|s| find(s).is_some_and(|t| &t.kind == b"para"));
        let curves = [b"rTRC", b"gTRC", b"bTRC"]
            .iter()
            .all(|s| find(s).is_some_and(|t| &t.kind == b"curv"));
        if !parametric && !curves {
            return Err(WideError::UnsupportedTagType);
        }
        (
            ProfileKind::MatrixTrc,
            Some(if parametric {
                TrcKind::Parametric
            } else {
                TrcKind::Curve
            }),
        )
    } else {
        return Err(WideError::UnsupportedProfile);
    };
    if matches!(kind, ProfileKind::MatrixTrc | ProfileKind::GrayTrc) && pcs != PcsKind::Xyz {
        return Err(WideError::UnsupportedPcs);
    }
    if device == DeviceSpace::Cmyk && lut_kind.is_none() {
        return Err(WideError::UnsupportedProfile);
    }
    let media_white = find(b"wtpt")
        .filter(|t| t.size >= 20 && &t.kind == b"XYZ ")
        .map(|t| [s15(b, t.offset + 8), s15(b, t.offset + 12), s15(b, t.offset + 16)]);
    let profile = ColorProfile::new_from_slice_with_options(
        b,
        ParsingOptions {
            max_profile_size: MAX_WIDE_PROFILE_BYTES + 1,
            max_allowed_clut_size: MAX_WIDE_CLUT_ENTRIES,
            max_allowed_trc_size: MAX_WIDE_TRC_ENTRIES,
        },
    )
    .map_err(|_| WideError::MalformedProfile)?;
    let space_ok = match device {
        DeviceSpace::Gray => profile.color_space == DataColorSpace::Gray,
        DeviceSpace::Rgb => profile.color_space == DataColorSpace::Rgb,
        DeviceSpace::Cmyk => profile.color_space == DataColorSpace::Cmyk,
    };
    if !space_ok
        || profile.pcs != if pcs == PcsKind::Xyz { DataColorSpace::Xyz } else { DataColorSpace::Lab }
        || matches!(profile.profile_class, ProfileClass::DeviceLink | ProfileClass::Abstract | ProfileClass::Named)
    {
        return Err(WideError::MalformedProfile);
    }
    if kind == ProfileKind::MatrixTrc
        && profile
            .colorant_matrix()
            .determinant()
            .is_none_or(|d| !d.is_finite() || d.abs() < 1e-10)
    {
        return Err(WideError::SingularMatrix);
    }
    let (device_to_pcs, pcs_to_device) = if lut_kind.is_some() {
        (
            tables([b"A2B0", b"A2B1", b"A2B2"]),
            tables([b"B2A0", b"B2A1", b"B2A2"]),
        )
    } else {
        (IntentTables::ALL, IntentTables::ALL)
    };
    let descriptor = WideProfileDescriptor {
        profile_id: input.profile_id.clone(),
        sha256: input.expected_sha256,
        bytes: b.len(),
        kind,
        trc,
        device,
        pcs,
        version_major: major,
        device_to_pcs,
        pcs_to_device,
        media_white,
    };
    Ok((profile, descriptor))
}

/// Engine identity carried by every wide receipt: what was compiled and what the numeric path is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineBuild {
    /// moxcms features compiled in (moxcms parses the profiles; it runs no wide-path transform).
    pub features: &'static str,
    /// moxcms SIMD feature flags are all off (`default-features = false`).
    pub simd: bool,
    /// True when the wide-path arithmetic could contract to fused multiply-add. The Prism evaluator is
    /// plain f64 Rust (no `mul_add`, no fast-math), which never contracts: always false.
    pub fma_contraction: bool,
    /// CLUT interpolation: tetrahedral for 3 inputs, linear in the first input over 3D tetrahedral for 4.
    pub interpolation: &'static str,
    /// `exact_f64`: interpolation weights are the exact f64 fractions (no binned weights, no re-baked grid).
    pub barycentric_weights: &'static str,
    pub fixed_point: bool,
}
pub const fn engine_build() -> EngineBuild {
    EngineBuild {
        features: "extended_range+lut+options",
        simd: false,
        fma_contraction: false,
        interpolation: "tetrahedral",
        barycentric_weights: "exact_f64",
        fixed_point: false,
    }
}

/// Black point compensation outcome (filled by the BPC lane; `Default` means not requested).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct BpcReport {
    pub requested: bool,
    pub applied: bool,
    /// Why nothing was applied, or the rule that was.
    pub rule: &'static str,
    pub source_black_lstar: Option<f64>,
    pub destination_black_lstar: Option<f64>,
    pub scale: Option<f64>,
    /// moxcms would have applied its own hard-coded black-point scaling to this pair (RGB v4 tables,
    /// perceptual or saturation); Prism's legs bypass it, so it never double-applies.
    pub engine_builtin_suppressed: bool,
}
/// Absolute colorimetric media-white scaling outcome.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct AbsoluteReport {
    pub applied: bool,
    pub source_media_white: Option<[f64; 3]>,
    pub destination_media_white: Option<[f64; 3]>,
    pub scale: Option<[f64; 3]>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ClipMethod {
    /// Not computed for this conversion.
    #[default]
    NotComputed,
    /// Linear device values of a matrix/TRC destination checked against 0..1 before encoding.
    LinearDevice,
    /// The destination is table-based; its clamped output cannot reveal clipping.
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct ClipReport {
    pub method: ClipMethod,
    pub pixels_clipped: u64,
    pub channels_clipped: [u64; 4],
    pub max_overshoot: f64,
}

/// Immutable plan: the Prism source stage (`None` for a Lab source, whose pixels convert to PCS XYZ
/// directly), the Prism PCS map and the destination stage; `fused` replaces all three for matrix/TRC
/// and Gray pairs. Clip detection (SMB-PRISM-ENG-2) scans the linear destination values of matrix/Gray
/// destinations before their curves (exact, independent of any TRC clamping); table destinations
/// cannot reveal clipping from their clamped output.
pub(crate) struct Plan {
    pub(crate) source: Option<ToPcs>,
    pub(crate) dest: FromPcs,
    pub(crate) map: PcsMap,
    pub(crate) fused: Option<Fused>,
    pub(crate) bpc: BpcReport,
    pub(crate) absolute: AbsoluteReport,
}
impl Plan {
    fn clip_method(&self) -> ClipMethod {
        match self.dest {
            FromPcs::Table { .. } => ClipMethod::Unavailable,
            FromPcs::Matrix { .. } | FromPcs::Gray(_) => ClipMethod::LinearDevice,
        }
    }
    /// Count the linear destination values of one pixel outside 0..1.
    fn scan_clip(lin: [f64; 3], channels: usize, report: &mut ClipReport) {
        // Values within 1e-3 of 0 or 1 are rounding noise (16.16 colorants), not clipping.
        const EPS: f64 = 1e-3;
        let mut any = false;
        for (ch, v) in lin.iter().take(channels).enumerate() {
            let over = if *v < -EPS {
                -v
            } else if *v > 1.0 + EPS {
                v - 1.0
            } else {
                continue;
            };
            any = true;
            report.channels_clipped[ch] += 1;
            report.max_overshoot = report.max_overshoot.max(over);
        }
        report.pixels_clipped += u64::from(any);
    }
    /// Convert one pixel into `out`; returns the linear destination values when clip detection applies.
    fn pixel(&self, px: &[f32], lab: bool, out: &mut [f32]) -> Option<[f64; 3]> {
        if let Some(f) = &self.fused {
            let lin = f.linear(px);
            f.encode(lin, out);
            return Some(lin);
        }
        let n = match &self.source {
            Some(s) => s.eval(px),
            None => {
                debug_assert!(lab);
                let v = crate::icc_eval::lab_to_xyz(f64::from(px[0]), f64::from(px[1]), f64::from(px[2]));
                [v[0] / crate::icc_eval::D50[0], v[1] / crate::icc_eval::D50[1], v[2] / crate::icc_eval::D50[2]]
            }
        };
        let n = self.map.apply(n);
        match self.dest.linear(n) {
            Some(lin) => {
                self.dest.encode_linear(lin, out);
                Some(lin)
            }
            None => {
                self.dest.eval(n, out);
                None
            }
        }
    }
}
/// Prism-owned adjustment of PCS XYZ between the stages.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PcsMap {
    Identity,
    /// `n * scale + offset` on the D50-normalised PCS (BPC: uniform scale, offset = 1 - scale).
    Affine { scale: f64, offset: [f64; 3] },
    /// Component-wise scale (absolute colorimetric media white ratio).
    Scale([f64; 3]),
}
impl PcsMap {
    /// The map as `n' = k * n + o` (component-wise).
    pub(crate) fn affine(&self) -> ([f64; 3], [f64; 3]) {
        match *self {
            Self::Identity => ([1.0; 3], [0.0; 3]),
            Self::Affine { scale, offset } => ([scale; 3], offset),
            Self::Scale(k) => (k, [0.0; 3]),
        }
    }
    fn apply(&self, n: [f64; 3]) -> [f64; 3] {
        match *self {
            Self::Identity => n,
            Self::Affine { scale, offset } => [scale * n[0] + offset[0], scale * n[1] + offset[1], scale * n[2] + offset[2]],
            Self::Scale(k) => [n[0] * k[0], n[1] * k[1], n[2] * k[2]],
        }
    }
}

/// Where the pixels come from: an admitted device profile, or ICC PCS Lab (D50, floats) directly.
#[derive(Clone, Copy)]
pub enum WideSource<'a> {
    Profile(ProfileInput<'a>),
    /// `L*` 0..100, `a*`/`b*` -128..128, three interleaved channels; no profile involved.
    Lab,
}
/// What the source pixels were, as recorded in the receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceSpace {
    Device(DeviceSpace),
    Lab,
}

/// Caller-scoped request for the wide path. `pixels` are interleaved: device values in 0..1 with the
/// source profile's channel count (CMYK: 1.0 is full ink, ICC convention), or Lab triples.
pub struct WideTransformRequest<'a> {
    pub source: WideSource<'a>,
    pub destination: ProfileInput<'a>,
    pub pixels: &'a [f32],
    pub intent: Intent,
    pub black_point_compensation: bool,
    pub revision: u64,
    pub expected_revision: u64,
    pub correlation_id: u64,
    pub resource_id: &'a DomainId,
    pub actor: &'a ActorContext,
}
#[derive(Clone, Debug, PartialEq)]
pub struct WideReceipt {
    /// `None` for a Lab source.
    pub source_profile_id: Option<DomainId>,
    pub source_sha256: Option<[u8; 32]>,
    pub source_kind: Option<ProfileKind>,
    pub source_space: SourceSpace,
    pub destination_profile_id: DomainId,
    pub destination_sha256: [u8; 32],
    pub destination_kind: ProfileKind,
    pub destination_space: DeviceSpace,
    pub engine_name: &'static str,
    pub engine_version: &'static str,
    pub engine_build: EngineBuild,
    pub intent: Intent,
    pub options: EvaluationOptions,
    pub bpc: BpcReport,
    pub absolute: AbsoluteReport,
    pub clip: ClipReport,
    pub cache_hit: bool,
    pub revision: u64,
    pub correlation_id: u64,
    pub pixel_count: usize,
    /// SHA-256 of the output as little-endian f32 bits: the cross-host determinism handle.
    pub output_sha256: [u8; 32],
}
#[derive(Debug)]
pub struct WideTransformResult {
    /// Interleaved destination device values (unclamped for matrix/TRC destinations).
    pub pixels: Vec<f32>,
    pub receipt: WideReceipt,
    pub delivery: hsk_studio_observe::Receipt,
    pub diagnostic_state: hsk_studio_observe::State,
}
#[derive(Debug)]
pub struct WideFailure {
    pub error: WideError,
    pub operation_error: Option<WideError>,
    pub delivery: Result<hsk_studio_observe::Receipt, hsk_studio_observe::Error>,
    pub diagnostic_state: hsk_studio_observe::State,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WideCacheStats {
    pub entries: usize,
    pub hits: u64,
    pub misses: u64,
    pub rejected: u64,
    /// Profile parses performed (a cache hit parses nothing).
    pub parses: u64,
}
struct Entry {
    source: Option<[u8; 32]>,
    destination: [u8; 32],
    intent: Intent,
    bpc: bool,
    plan: Arc<Plan>,
    source_desc: Option<WideProfileDescriptor>,
    destination_desc: WideProfileDescriptor,
}

/// One cache per caller context, like `Prism`: no globals, no eviction, no disk cache.
pub struct WideEngine {
    actor: ActorContext,
    capacity: usize,
    cache: Vec<Entry>,
    stats: WideCacheStats,
    fusion: bool,
}
struct ForwardSink<'a>(&'a mut dyn SinkPort);
impl SinkPort for ForwardSink<'_> {
    fn try_send(
        &mut self,
        class: hsk_studio_observe::DeliveryClass,
        bytes: &[u8],
    ) -> Result<(), hsk_studio_observe::DeliveryError> {
        self.0.try_send(class, bytes)
    }
}
fn canceled(token: &CancellationToken) -> Result<(), WideError> {
    token.check().map_err(|_| WideError::Canceled)
}

type Planned = (
    Arc<Plan>,
    Option<WideProfileDescriptor>,
    WideProfileDescriptor,
    bool,
);

impl WideEngine {
    pub fn new(actor: ActorContext, cache_entries: usize) -> Result<Self, WideError> {
        if cache_entries == 0 || cache_entries > crate::MAX_CACHE_ENTRIES {
            return Err(WideError::InvalidBudget);
        }
        Ok(Self {
            actor,
            capacity: cache_entries,
            cache: Vec::with_capacity(cache_entries),
            stats: WideCacheStats {
                entries: 0,
                hits: 0,
                misses: 0,
                rejected: 0,
                parses: 0,
            },
            fusion: true,
        })
    }
    /// Matrix/TRC and Gray pairs run as one fused affine 3x3 between the curves (default on). Off
    /// keeps the staged source -> PCS map -> destination evaluation (same arithmetic up to f64
    /// rounding; used to prove the fusion). Changing the setting clears the plan cache.
    pub fn set_matrix_fusion(&mut self, enabled: bool) {
        if self.fusion != enabled {
            self.fusion = enabled;
            self.clear_cache();
        }
    }
    pub fn matrix_fusion(&self) -> bool {
        self.fusion
    }
    pub fn cache_stats(&self) -> WideCacheStats {
        self.stats
    }
    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.stats.entries = 0;
    }
    /// Pure bounded validation of a wide profile: no transform, cache admission or telemetry.
    pub fn inspect_profile(
        &self,
        input: ProfileInput<'_>,
        cancel: &CancellationToken,
    ) -> Result<WideProfileDescriptor, WideError> {
        canceled(cancel)?;
        let (_, descriptor) = admit(input)?;
        canceled(cancel)?;
        Ok(descriptor)
    }

    fn plan(
        &mut self,
        r: &WideTransformRequest<'_>,
        cancel: &CancellationToken,
    ) -> Result<Planned, WideError> {
        let source_hash = match r.source {
            WideSource::Profile(p) => Some(p.expected_sha256),
            WideSource::Lab => None,
        };
        let hit = self.cache.iter().position(|e| {
            e.source == source_hash
                && e.destination == r.destination.expected_sha256
                && e.intent == r.intent
                && e.bpc == r.black_point_compensation
        });
        if let Some(i) = hit {
            // A hit re-proves identity and exact bytes (hash) but parses nothing.
            if let WideSource::Profile(p) = r.source {
                verify(p)?;
            }
            canceled(cancel)?;
            verify(r.destination)?;
            self.stats.hits = self.stats.hits.saturating_add(1);
            let e = &self.cache[i];
            return Ok((
                e.plan.clone(),
                e.source_desc.clone(),
                e.destination_desc.clone(),
                true,
            ));
        }
        let source = match r.source {
            WideSource::Profile(p) => {
                self.stats.parses += 1;
                Some(admit(p)?)
            }
            WideSource::Lab => None,
        };
        canceled(cancel)?;
        self.stats.parses += 1;
        let (destination, destination_desc) = admit(r.destination)?;
        canceled(cancel)?;
        if self.cache.len() >= self.capacity {
            return Err(WideError::CacheFull);
        }
        let source_desc = source.as_ref().map(|(_, d)| d.clone());
        let plan = Arc::new(crate::wide_plan::build(
            source.as_ref().map(|(p, d)| (p, d)),
            (&destination, &destination_desc),
            r.intent,
            r.black_point_compensation,
            self.fusion,
            cancel,
        )?);
        canceled(cancel)?;
        self.cache.push(Entry {
            source: source_hash,
            destination: r.destination.expected_sha256,
            intent: r.intent,
            bpc: r.black_point_compensation,
            plan: plan.clone(),
            source_desc: source_desc.clone(),
            destination_desc: destination_desc.clone(),
        });
        self.stats.entries = self.cache.len();
        self.stats.misses = self.stats.misses.saturating_add(1);
        Ok((plan, source_desc, destination_desc, false))
    }

    fn compute(
        &mut self,
        r: &WideTransformRequest<'_>,
        cancel: &CancellationToken,
    ) -> Result<(Vec<f32>, WideReceipt), WideError> {
        canceled(cancel)?;
        if r.actor != &self.actor {
            return Err(WideError::ContextMismatch);
        }
        if r.revision != r.expected_revision {
            return Err(WideError::RevisionMismatch);
        }
        let (plan, source_desc, destination_desc, cache_hit) = self.plan(r, cancel)?;
        let in_ch = source_desc.as_ref().map_or(3, |d| d.device.channels());
        let out_ch = destination_desc.device.channels();
        if r.pixels.is_empty() || !r.pixels.len().is_multiple_of(in_ch) {
            return Err(WideError::ChannelCount);
        }
        let count = r.pixels.len() / in_ch;
        if count > MAX_WIDE_PIXELS {
            return Err(WideError::PixelLimit);
        }
        for (i, v) in r.pixels.iter().enumerate() {
            if !v.is_finite() {
                return Err(WideError::NonfiniteChannel);
            }
            let range = if source_desc.is_some() {
                0.0..=1.0
            } else if i % 3 == 0 {
                0.0..=100.0
            } else {
                -128.0..=128.0
            };
            if !range.contains(v) {
                return Err(WideError::ChannelRange);
            }
        }
        let mut output = vec![0.0f32; count * out_ch];
        let mut clip = ClipReport {
            method: plan.clip_method(),
            ..Default::default()
        };
        let lab = source_desc.is_none();
        let clip_channels = if clip.method == ClipMethod::LinearDevice { out_ch } else { 0 };
        let mut hasher = Sha256::new();
        let mut bytes = Vec::with_capacity(CHUNK_PIXELS * out_ch * 4);
        for (px_chunk, out_chunk) in r.pixels.chunks(CHUNK_PIXELS * in_ch).zip(output.chunks_mut(CHUNK_PIXELS * out_ch)) {
            canceled(cancel)?;
            for (px, out) in px_chunk.chunks_exact(in_ch).zip(out_chunk.chunks_exact_mut(out_ch)) {
                if let Some(lin) = plan.pixel(px, lab, out) {
                    Plan::scan_clip(lin, clip_channels, &mut clip);
                }
            }
            if out_chunk.iter().any(|v| !v.is_finite()) {
                return Err(WideError::NonfiniteOutput);
            }
            bytes.clear();
            for v in out_chunk.iter() {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            hasher.update(&bytes);
        }
        canceled(cancel)?;
        let (source_profile_id, source_sha256) = match r.source {
            WideSource::Profile(p) => (Some(p.profile_id.clone()), Some(p.expected_sha256)),
            WideSource::Lab => (None, None),
        };
        let receipt = WideReceipt {
            source_profile_id,
            source_sha256,
            source_kind: source_desc.as_ref().map(|d| d.kind),
            source_space: source_desc
                .as_ref()
                .map_or(SourceSpace::Lab, |d| SourceSpace::Device(d.device)),
            destination_profile_id: r.destination.profile_id.clone(),
            destination_sha256: r.destination.expected_sha256,
            destination_kind: destination_desc.kind,
            destination_space: destination_desc.device,
            engine_name: WIDE_ENGINE_NAME,
            engine_version: WIDE_ENGINE_VERSION,
            engine_build: engine_build(),
            intent: r.intent,
            options: EvaluationOptions {
                allow_use_cicp_transfer: false,
                prefer_fixed_point: false,
                allow_extended_range_rgb_xyz: true,
                precision_bits: 32,
                clamped_output: false,
            },
            bpc: plan.bpc,
            absolute: plan.absolute,
            clip,
            cache_hit,
            revision: r.revision,
            correlation_id: r.correlation_id,
            pixel_count: count,
            output_sha256: hasher.finalize().into(),
        };
        Ok((output, receipt))
    }

    /// Convert interleaved pixels (device values or Lab) through the two-leg plan and deliver one
    /// terminal outcome to the caller's sink. Numeric output is withheld when delivery fails.
    pub fn transform(
        &mut self,
        r: &WideTransformRequest<'_>,
        cancel: &CancellationToken,
        sink: &mut dyn SinkPort,
    ) -> Result<WideTransformResult, WideFailure> {
        let compute = self.compute(r, cancel);
        let operation_error = compute.as_ref().err().copied();
        let outcome = operation_error.map_or(Outcome::Success, WideError::outcome);
        let mut observe = Observe::new(
            r.correlation_id,
            r.revision,
            r.resource_id.clone(),
            self.actor.clone(),
            Budget::new(0, 0).expect("valid zero progress budget"),
        );
        let delivery = observe.emit(
            Observation {
                correlation_id: r.correlation_id,
                revision: r.revision,
                outcome,
                progress: None,
                private_project_text: None,
            },
            cancel,
            &mut ForwardSink(sink),
        );
        let diagnostic_state = observe.state();
        let delivery = match delivery {
            Ok(d) => d,
            Err(e) => {
                self.stats.rejected = self.stats.rejected.saturating_add(1);
                return Err(WideFailure {
                    error: WideError::DeliveryFailed,
                    operation_error,
                    delivery: Err(e),
                    diagnostic_state,
                });
            }
        };
        if let Some(error) = operation_error {
            self.stats.rejected = self.stats.rejected.saturating_add(1);
            return Err(WideFailure {
                error,
                operation_error: Some(error),
                delivery: Ok(delivery),
                diagnostic_state,
            });
        }
        if delivery.outcome == Outcome::Canceled {
            return Err(WideFailure {
                error: WideError::Canceled,
                operation_error: Some(WideError::Canceled),
                delivery: Ok(delivery),
                diagnostic_state,
            });
        }
        let (pixels, receipt) = compute.expect("checked compute");
        Ok(WideTransformResult {
            pixels,
            receipt,
            delivery,
            diagnostic_state,
        })
    }
}

impl crate::Prism {
    /// Wide-profile counterpart of `ColorEngine::inspect_profile`; the legacy method is unchanged.
    pub fn inspect_wide_profile(
        &self,
        input: ProfileInput<'_>,
        cancel: &CancellationToken,
    ) -> Result<WideProfileDescriptor, WideError> {
        canceled(cancel)?;
        let (_, descriptor) = admit(input)?;
        canceled(cancel)?;
        Ok(descriptor)
    }
}

/// Same descriptor for models, the human manual and Argus adapters; describes the wide path only
/// (`DESCRIPTOR` keeps describing the legacy matrix-only path).
pub const WIDE_DESCRIPTOR: &str = r##"{"module":"STUDIO-MODULE-PRISM","path":"wide","descriptor_version":1,"api":"WideEngine::new(actor, cache_entries); inspect_profile(ProfileInput, &CancellationToken) -> WideProfileDescriptor; transform(&WideTransformRequest, &CancellationToken, &mut dyn SinkPort) -> WideTransformResult | WideFailure. Prism::inspect_wide_profile is the same admission. Legacy Prism, ColorEngine and AdmittedPrism are unchanged","inputs":{"source":"WideSource::Profile(ProfileInput) with interleaved device pixels (Gray 1, RGB 3, CMYK 4 channels, 0..1, CMYK 1.0 = full ink) or WideSource::Lab (PCS Lab D50, L 0..100, a/b -128..128, 3 channels, no profile)","destination":"ProfileInput; Gray, RGB or CMYK device","intent":"relative_colorimetric | perceptual | saturation | absolute_colorimetric","black_point_compensation":"independent boolean; ignored for absolute colorimetric","pixels_per_request":2097152,"profile_bytes":2097152},"profiles":{"families":"ICC v2/v4 RGB matrix/TRC (para or curv), Gray kTRC, mAB/mBA multi-dimensional, mft1/mft2 legacy tables; XYZ or Lab PCS; classes scnr, mntr, prtr, spac","refused":"typed: version other than 2 or 4, link/abst/nmcl classes, device spaces other than Gray/RGB/CMYK, PCS other than XYZ/Lab, D2B/B2D multi-process tags, tags of unsupported type, CMYK without tables, missing intent table, absolute without wtpt, singular matrix, malformed framing","intent_tables":"ICC.1:2022 8.10.2 precedence: the intent's table (perceptual A2B0/B2A0, relative and absolute A2B1/B2A1, saturation A2B2/B2A2), else table 0 (A2B0/B2A0), else matrix/TRC; MissingIntentTable only when none exists; WideProfileDescriptor.device_to_pcs/pcs_to_device show which tables exist"},"model":{"stages":"Prism-evaluated source stage (device -> PCS XYZ/D50, f64, unclamped), Prism PCS stage, Prism-evaluated destination stage; moxcms 0.9.1 only parses (no moxcms transform, so its own black-point scaling never runs); curves: curv identity/gamma/table (linear interpolation), para types 0-4; mft1/mft2 matrix + input tables + CLUT + output tables; mAB A/CLUT/M/matrix+offset/B, mBA reverse; table curves and CLUT inputs clamp to 0..1, parametric curves unbounded (HDR PCS above 1 kept); destination matrix/Gray curves inverted exactly and extended sign-preserving outside 0..1","fusion":"matrix/TRC and Gray pairs: one affine 3x3 (destination inverse matrix x PCS map x source matrix) between the curves, identity curves skipped; WideEngine::set_matrix_fusion(false) keeps the staged path","interpolation":"CLUT: 1 input linear, 2 bilinear, 3 tetrahedral, 4 linear in the first input over 3D tetrahedral; exact f64 weights, no re-baked grid","bpc":"ISO 18619 per bpc.rs: source/destination black L* (darkest vertex, CMYK B2A local black, round-trip ramp for table destinations), XYZ' = scale*XYZ + (1-scale)*D50 with scale=(1-Yd)/(1-Ys); one application; receipt names the rule and whether moxcms's own scaling was suppressed","absolute":"PCS XYZ scaled by wtpt_src / wtpt_dst (Lab source = D50); BPC ignored","clip":"matrix/TRC destinations: linear values outside 0..1 counted exactly (tolerance 0.001), output left unclamped; table destinations: method unavailable, stated in the receipt","cache":"plans keyed by (source sha256, destination sha256, intent, bpc flag) within one engine; a hit re-proves id, size and sha256 and parses nothing (WideCacheStats.parses); capacity 1..8","determinism":"f64 from IEEE basic operations only (own ln/exp/pow, no libm, no fused multiply-add); receipt carries engine name/version (WIDE_ENGINE_NAME/WIDE_ENGINE_VERSION) and build (simd=false, fma_contraction=false, interpolation, weights=exact_f64) and output_sha256 over the f32 bits"},"output":"interleaved destination device values as f32 (unclamped for matrix/TRC destinations) plus WideReceipt (source/destination ids and hashes, kinds, spaces, engine build, bpc, absolute, clip, cache_hit, revision, correlation_id, pixel_count, output_sha256) and the Observe delivery receipt","recovery":"unsupported_*: choose a supported profile; missing_intent_table: the profile has neither the intent's table, table 0 nor matrix/TRC tags, choose another profile; missing_media_white: use another intent or a profile with wtpt; channel_count/channel_range/nonfinite_channel: fix the pixel buffer; pixel_limit: split into requests of at most 2097152 pixels; cache_full: clear_cache or use a larger engine; revision_mismatch/context_mismatch: reload; canceled: nothing was produced; delivery_failed: reconcile through the sink owner before retry","privacy":"profile bytes, pixels and ids never enter telemetry; the Observe frame carries only the outcome","not_in_scope":["OCIO","device-link and named-colour profiles","D2B/B2D multi-process elements","gamut check for table destinations","ink limit and black preservation","Lab as a device profile","caller memory-ledger admission of the wide path (bounded by profile size, pixel limit and cache capacity)"],"manual_route":"same descriptor","argus_route":"same descriptor plus WideReceipt, cache_stats and diagnostic_state"}"##;
