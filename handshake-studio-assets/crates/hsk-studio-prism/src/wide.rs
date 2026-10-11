//! Wide ICC profile admission and conversion (SMB-PRISM-ENG-3 and the engine lanes built on it).
//!
//! Strictly additive beside the legacy matrix-only path: `MAX_PROFILE_BYTES`, `Prism`, `ColorEngine`
//! and `AdmittedPrism` are untouched. This path admits ICC v2/v4 matrix/TRC, `mAB ` and `mft1/mft2`
//! profiles for Gray, RGB and CMYK devices with an XYZ or Lab PCS, up to `MAX_WIDE_PROFILE_BYTES`.
//!
//! Conversion is two moxcms legs joined by a Prism-owned PCS stage: `source -> PCS-XYZ companion`
//! then `PCS-XYZ companion -> destination`. The companion is an identity-colourant, linear,
//! version-2.1 matrix profile (rgb = XYZ-D50); because moxcms only applies its hard-coded
//! black-point scaling to profiles of version 4.0 or later, no leg can apply it, so every PCS
//! adjustment (black point compensation, absolute media-white scaling) is applied exactly once, by
//! Prism, between the legs.
use crate::{
    EvaluationOptions, Intent, ProfileInput, ENGINE_NAME, ENGINE_VERSION, profile_hash,
};
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{Budget, FailureCode, Observation, Observe, Outcome, SinkPort};
use moxcms::{
    BarycentricWeightScale, ColorProfile, DataColorSpace, InterpolationMethod, Layout,
    ParsingOptions, ProfileClass, RenderingIntent, TransformF32Executor, TransformOptions,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;

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
    const fn layout(self) -> Layout {
        match self {
            Self::Gray => Layout::Gray,
            Self::Rgb => Layout::Rgb,
            Self::Cmyk => Layout::Rgba,
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

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn push_s15(out: &mut Vec<u8>, v: f64) {
    out.extend_from_slice(&((v * 65536.0).round() as i32).to_be_bytes());
}
/// Linear version-2.1 matrix profile whose colourants are the diagonal of the D50 white, so
/// `rgb = XYZ / D50` (white = 1,1,1; a gray source maps to equal channels = a D50 neutral). Version
/// 2.1 keeps moxcms's built-in black-point scaling (version 4.0+ only) from ever applying in a leg.
pub(crate) fn pcs_companion_bytes() -> Vec<u8> {
    let xyz = |x: f64, y: f64, z: f64| {
        let mut t = b"XYZ \0\0\0\0".to_vec();
        push_s15(&mut t, x);
        push_s15(&mut t, y);
        push_s15(&mut t, z);
        t
    };
    let mut curve = b"para\0\0\0\0\0\0\0\0".to_vec();
    push_s15(&mut curve, 1.0);
    let datas: [(&[u8; 4], Vec<u8>); 7] = [
        (b"wtpt", xyz(0.9642, 1.0, 0.8249)),
        (b"rXYZ", xyz(0.9642, 0.0, 0.0)),
        (b"gXYZ", xyz(0.0, 1.0, 0.0)),
        (b"bXYZ", xyz(0.0, 0.0, 0.8249)),
        (b"rTRC", curve.clone()),
        (b"gTRC", curve.clone()),
        (b"bTRC", curve),
    ];
    let mut out = vec![0u8; 128];
    out[8..12].copy_from_slice(&0x0210_0000u32.to_be_bytes());
    out[12..16].copy_from_slice(b"mntr");
    out[16..20].copy_from_slice(b"RGB ");
    out[20..24].copy_from_slice(b"XYZ ");
    out[36..40].copy_from_slice(b"acsp");
    for (i, v) in [0.9642, 1.0, 0.8249].into_iter().enumerate() {
        out[68 + i * 4..72 + i * 4].copy_from_slice(&((v * 65536.0_f64).round() as i32).to_be_bytes());
    }
    push_u32(&mut out, datas.len() as u32);
    let mut offset = 132 + datas.len() * 12;
    for (sig, data) in &datas {
        out.extend_from_slice(*sig);
        push_u32(&mut out, offset as u32);
        push_u32(&mut out, data.len() as u32);
        offset += data.len();
    }
    for (_, data) in &datas {
        out.extend_from_slice(data);
    }
    let len = out.len() as u32;
    out[..4].copy_from_slice(&len.to_be_bytes());
    out
}

/// Engine identity carried by every wide receipt: what was compiled and what the numeric path is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineBuild {
    pub features: &'static str,
    /// moxcms SIMD feature flags are all off (`default-features = false`).
    pub simd: bool,
    /// moxcms switches to fused multiply-add when compiled with fma/neon target features, which
    /// changes low result bits between builds; true marks the numeric path as build-dependent.
    pub fma_contraction: bool,
    pub interpolation: &'static str,
    pub barycentric_weights: &'static str,
    pub fixed_point: bool,
}
pub const fn engine_build() -> EngineBuild {
    EngineBuild {
        features: "extended_range+lut+options",
        simd: false,
        fma_contraction: cfg!(any(target_feature = "fma", target_feature = "neon")),
        interpolation: "tetrahedral",
        barycentric_weights: "binned_65536",
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

/// How the destination's out-of-range values are detected before encoding (SMB-PRISM-ENG-2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ClipPlan {
    /// Table-based destination: its clamped output cannot reveal clipping.
    Unavailable,
    /// Matrix/TRC destination: linear device value = (inverse colorant matrix * diag(D50)) times the
    /// normalised PCS, checked against 0..1 (exact, independent of any TRC clamping).
    Rgb([[f64; 3]; 3]),
    /// Gray TRC destination: relative luminance Y checked against 0..1.
    Gray,
}

/// Immutable plan: two moxcms legs and the Prism PCS stage between them. `to_pcs` is `None` for a
/// Lab source, whose pixels are converted to PCS XYZ directly.
pub(crate) struct Plan {
    pub(crate) to_pcs: Option<Arc<TransformF32Executor>>,
    pub(crate) from_pcs: Arc<TransformF32Executor>,
    pub(crate) map: PcsMap,
    pub(crate) bpc: BpcReport,
    pub(crate) absolute: AbsoluteReport,
    pub(crate) clip: ClipPlan,
}
impl Plan {
    fn clip_method(&self) -> ClipMethod {
        match self.clip {
            ClipPlan::Unavailable => ClipMethod::Unavailable,
            ClipPlan::Rgb(_) | ClipPlan::Gray => ClipMethod::LinearDevice,
        }
    }
    /// Count linear destination values outside 0..1 for the PCS XYZ pixels of one chunk.
    fn scan_clip(&self, xyz: &[f32], report: &mut ClipReport) {
        // Values within 1e-3 of 0 or 1 are rounding noise (16.16 colorants, f32), not clipping.
        const EPS: f64 = 1e-3;
        let mut note = |ch: usize, v: f64, any: &mut bool| {
            let over = if v < -EPS {
                -v
            } else if v > 1.0 + EPS {
                v - 1.0
            } else {
                return;
            };
            *any = true;
            report.channels_clipped[ch] += 1;
            report.max_overshoot = report.max_overshoot.max(over);
        };
        for p in xyz.chunks_exact(3) {
            let mut any = false;
            match self.clip {
                ClipPlan::Unavailable => return,
                ClipPlan::Gray => note(0, f64::from(p[1]), &mut any),
                ClipPlan::Rgb(inv) => {
                    let v = [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])];
                    for (ch, row) in inv.iter().enumerate() {
                        note(ch, row[0] * v[0] + row[1] * v[1] + row[2] * v[2], &mut any);
                    }
                }
            }
            report.pixels_clipped += u64::from(any);
        }
    }
}
/// Prism-owned adjustment of PCS XYZ between the legs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PcsMap {
    Identity,
    /// `n * scale + offset` on the D50-normalised PCS (BPC: uniform scale, offset = 1 - scale).
    Affine { scale: f64, offset: [f64; 3] },
    /// Component-wise scale (absolute colorimetric media white ratio).
    Scale([f64; 3]),
}
impl PcsMap {
    fn apply(&self, xyz: &mut [f32]) {
        match *self {
            Self::Identity => {}
            Self::Affine { scale, offset } => {
                for p in xyz.chunks_exact_mut(3) {
                    for (v, o) in p.iter_mut().zip(offset) {
                        *v = (scale * f64::from(*v) + o) as f32;
                    }
                }
            }
            Self::Scale(s) => {
                for p in xyz.chunks_exact_mut(3) {
                    for (v, k) in p.iter_mut().zip(s) {
                        *v = (f64::from(*v) * k) as f32;
                    }
                }
            }
        }
    }
}

pub(crate) fn mox_intent(intent: Intent) -> RenderingIntent {
    match intent {
        Intent::RelativeColorimetric | Intent::AbsoluteColorimetric => {
            RenderingIntent::RelativeColorimetric
        }
        Intent::Perceptual => RenderingIntent::Perceptual,
        Intent::Saturation => RenderingIntent::Saturation,
    }
}
pub(crate) fn options(intent: Intent) -> TransformOptions {
    TransformOptions {
        rendering_intent: mox_intent(intent),
        allow_use_cicp_transfer: false,
        prefer_fixed_point: false,
        allow_extended_range_rgb_xyz: true,
        // moxcms's default (`Linear` with `Low` weights) quantises float LUT inputs to 256 bins; the
        // declared path is tetrahedral with 65536-bin weights.
        interpolation_method: InterpolationMethod::Tetrahedral,
        barycentric_weight_scale: BarycentricWeightScale::High,
    }
}
pub(crate) fn leg(
    from: &ColorProfile,
    from_space: DeviceSpace,
    to: &ColorProfile,
    to_space: DeviceSpace,
    intent: Intent,
) -> Result<Arc<TransformF32Executor>, WideError> {
    from.create_transform_f32(from_space.layout(), to, to_space.layout(), options(intent))
        .map_err(|_| WideError::EngineFailure)
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
    companion: ColorProfile,
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
        let companion = ColorProfile::new_from_slice(&pcs_companion_bytes())
            .map_err(|_| WideError::EngineFailure)?;
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
            companion,
        })
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
            &self.companion,
            source.as_ref().map(|(p, d)| (p, d)),
            (&destination, &destination_desc),
            r.intent,
            r.black_point_compensation,
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
        let mut output = Vec::with_capacity(count * out_ch);
        let mut clip = ClipReport {
            method: plan.clip_method(),
            ..Default::default()
        };
        let mut hasher = Sha256::new();
        for chunk in r.pixels.chunks(CHUNK_PIXELS * in_ch) {
            canceled(cancel)?;
            let n = chunk.len() / in_ch;
            let mut xyz = vec![0.0f32; n * 3];
            match &plan.to_pcs {
                Some(to_pcs) => to_pcs
                    .transform(chunk, &mut xyz)
                    .map_err(|_| WideError::EngineFailure)?,
                None => {
                    for (lab, out) in chunk.chunks_exact(3).zip(xyz.chunks_exact_mut(3)) {
                        let v = crate::bpc::lab_to_xyz(
                            f64::from(lab[0]),
                            f64::from(lab[1]),
                            f64::from(lab[2]),
                        );
                        for ((o, c), white) in out.iter_mut().zip(v).zip(crate::bpc::D50) {
                            *o = (c / white) as f32;
                        }
                    }
                }
            }
            plan.map.apply(&mut xyz);
            plan.scan_clip(&xyz, &mut clip);
            let mut out = vec![0.0f32; n * out_ch];
            plan.from_pcs
                .transform(&xyz, &mut out)
                .map_err(|_| WideError::EngineFailure)?;
            if out.iter().any(|v| !v.is_finite()) {
                return Err(WideError::NonfiniteOutput);
            }
            for v in &out {
                hasher.update(v.to_le_bytes());
            }
            output.extend_from_slice(&out);
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
            engine_name: ENGINE_NAME,
            engine_version: ENGINE_VERSION,
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

pub(crate) fn intent_table_check(
    intent: Intent,
    source: Option<&WideProfileDescriptor>,
    destination: &WideProfileDescriptor,
) -> Result<(), WideError> {
    if source.is_some_and(|s| !s.device_to_pcs.has(intent)) || !destination.pcs_to_device.has(intent)
    {
        return Err(WideError::MissingIntentTable);
    }
    Ok(())
}
