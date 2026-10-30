//! TIFF export of a developed image (lane SMB-DEVELOP-EXPORT-TIFF, Lightroom Classic export parity, engine
//! level). The pixel rendering is [`crate::output`] (sRGB OETF, clamp counted); this module chooses the
//! container: 8/16-bit RGB, None/LZW/ZIP compression with the optional horizontal predictor, little- or
//! big-endian, classic or BigTIFF, the encoded ICC profile embedded as tag 34675, resolution and ASCII
//! metadata, all written by `hsk-studio-interop-tiff` (the single TIFF container owner).
//!
//! Spec basis (module 14, studio creative suite): STU-IO-136 (TIFF compression domain None/LZW/ZIP) and the
//! STU-COL embedded-profile clauses (the exported pixels are tagged with the profile they were rendered in).
//!
//! Colour space contract: [`ExportColourSpace`] names the space the caller rendered in. The embedded profile
//! is the caller's encoded ICC bytes; this module reads its colorant tags and TRC and refuses a profile that
//! is not the named space ([`ExportError::ProfileMismatch`]), so a TIFF is never tagged with a profile that
//! disagrees with its pixels. Output transfer is the sRGB OETF (also Display P3's), so only sRGB and Display
//! P3 are renderable. Adobe RGB (1998) and ProPhoto RGB need their own transfer curves and have no ICC
//! profile in the test corpus: they are the typed refusal [`ExportError::ProfileMissing`] naming the profile.
//!
//! Output resize, output sharpening and the graphic watermark are the export stage ([`crate::export_stage`],
//! lane SMB-LR-EXPORT-SIZING-1): [`develop_to_tiff_staged`] / [`tiff_from_staged_output`] write the staged image
//! with the stage's resolution as dpi; the unstaged entries below are unchanged.
//!
//! Not here (recorded gaps, not silently dropped): DNG export
//! (develop reads DNG; it has no DNG writer, only a test-side TIFF/DNG builder), CMYK/Lab/greyscale export
//! colour modes. Lightroom Classic export equivalence is UNVERIFIED (no oracle, D5).
#![deny(clippy::arithmetic_side_effects)]
use crate::output::{check_icc_header, develop_to_output, ClipReport, OutputDepth, OutputImage};
use crate::par::Threads;
use crate::pipeline::DevelopReceipt;
use crate::recipe::DevelopRecipe;
use crate::export_stage::{ExportStage, ExportStageError, ExportStageReceipt, validate_export_stage};
use crate::output::develop_to_output_staged;
use crate::{CfaPlane, DevelopError};
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_tiff::{
    encode_with, Alpha, ColorModel, Compression, EncodeOptions, EncodeRequest, Predictor, Resolution, Samples,
    TiffError, MAX_ENCODE_BYTES,
};
pub use hsk_studio_interop_tiff::{ByteOrder, Metadata};
use sha2::{Digest, Sha256};

/// Largest pixel-per-inch value accepted.
pub const MAX_DPI: u32 = 65_535;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportColourSpace {
    Srgb,
    DisplayP3,
    /// Refused: no Adobe RGB (1998) profile in the corpus and no matching transfer stage.
    AdobeRgb,
    /// Refused: no ProPhoto RGB profile in the corpus and no matching transfer stage.
    ProPhoto,
}

impl ExportColourSpace {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Srgb => "srgb",
            Self::DisplayP3 => "display_p3",
            Self::AdobeRgb => "adobe_rgb_1998",
            Self::ProPhoto => "prophoto_rgb",
        }
    }

    /// ICC D50-adapted colorants (rXYZ, gXYZ, bXYZ) of the space.
    const fn colorants(self) -> Option<[[f64; 3]; 3]> {
        match self {
            Self::Srgb => Some([[0.4360, 0.2225, 0.0139], [0.3851, 0.7169, 0.0971], [0.1431, 0.0606, 0.7141]]),
            Self::DisplayP3 => Some([[0.5151, 0.2412, -0.0011], [0.2920, 0.6922, 0.0419], [0.1571, 0.0666, 0.7841]]),
            Self::AdobeRgb | Self::ProPhoto => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TiffCompression {
    None,
    Lzw,
    /// ZIP (TIFF Deflate, tag 8).
    Deflate,
}

/// Export settings record (Lightroom export panel: file settings and colour space; resize, output sharpening
/// and watermark are the export stage, see the module notes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TiffExportSettings {
    pub depth: OutputDepth,
    pub compression: TiffCompression,
    /// Horizontal differencing predictor (only with LZW or ZIP; refused with `None`).
    pub predictor: bool,
    pub colour_space: ExportColourSpace,
    pub byte_order: ByteOrder,
    pub big_tiff: bool,
    /// Pixels per inch written as XResolution/YResolution (unit inch); `None` writes no resolution tags.
    pub dpi: Option<u32>,
    pub metadata: Metadata,
}

impl TiffExportSettings {
    pub fn new(depth: OutputDepth, colour_space: ExportColourSpace) -> Self {
        Self {
            depth,
            compression: TiffCompression::None,
            predictor: false,
            colour_space,
            byte_order: ByteOrder::Little,
            big_tiff: false,
            dpi: None,
            metadata: Metadata::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExportError {
    Develop(DevelopError),
    Tiff(TiffError),
    /// The named colour space cannot be exported: its profile is not available (name inside).
    ProfileMissing(&'static str),
    /// The embedded profile is not the profile of the named colour space (what differed inside).
    ProfileMismatch(&'static str),
    /// Settings or image inconsistent (stable code inside).
    Invalid(&'static str),
    /// An export-stage refusal (sizing, sharpening, watermark; lane SMB-LR-EXPORT-SIZING-1).
    Stage(ExportStageError),
}

impl ExportError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Develop(e) => e.code(),
            Self::Tiff(e) => e.code(),
            Self::ProfileMissing(_) => "colour_space_profile_missing",
            Self::ProfileMismatch(_) => "profile_colour_space_mismatch",
            Self::Invalid(c) => c,
            Self::Stage(e) => e.code(),
        }
    }

    /// Names the missing profile or the mismatching property; empty when the code says it all.
    pub fn detail(&self) -> &'static str {
        match self {
            Self::ProfileMissing(d) | Self::ProfileMismatch(d) => d,
            Self::Stage(e) => e.detail(),
            _ => "",
        }
    }
}

impl From<DevelopError> for ExportError {
    fn from(e: DevelopError) -> Self {
        Self::Develop(e)
    }
}

impl From<ExportStageError> for ExportError {
    fn from(e: ExportStageError) -> Self {
        match e {
            ExportStageError::Develop(d) => Self::Develop(d),
            other => Self::Stage(other),
        }
    }
}

impl From<TiffError> for ExportError {
    fn from(e: TiffError) -> Self {
        Self::Tiff(e)
    }
}

/// An encoded TIFF with the facts a host records beside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TiffExport {
    pub bytes: Vec<u8>,
    pub tiff_sha256: [u8; 32],
    pub width: u32,
    pub height: u32,
    pub depth_bits: u8,
    pub colour_space: ExportColourSpace,
    pub icc_sha256: [u8; 32],
    pub clip: ClipReport,
}

fn be32(icc: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_be_bytes(icc.get(at..end)?.try_into().ok()?))
}

/// Bytes of the tag `sig`, bounds-checked against the profile.
fn icc_tag<'a>(icc: &'a [u8], sig: &[u8; 4]) -> Option<&'a [u8]> {
    let count = usize::try_from(be32(icc, 128)?).ok()?;
    if count > 1024 {
        return None;
    }
    for i in 0..count {
        let at = 132_usize.checked_add(i.checked_mul(12)?)?;
        if icc.get(at..at.checked_add(4)?)? == sig {
            let off = usize::try_from(be32(icc, at.checked_add(4)?)?).ok()?;
            let len = usize::try_from(be32(icc, at.checked_add(8)?)?).ok()?;
            return icc.get(off..off.checked_add(len)?);
        }
    }
    None
}

fn s15(tag: &[u8], at: usize) -> Option<f64> {
    let end = at.checked_add(4)?;
    Some(f64::from(i32::from_be_bytes(tag.get(at..end)?.try_into().ok()?)) / 65_536.0)
}

/// The profile's colorants equal the named space's and its TRC is the sRGB piecewise curve (ICC `para`
/// function 3, gamma 2.4), which is the transfer [`crate::output`] applies.
fn check_profile_is(icc: &[u8], space: ExportColourSpace) -> Result<(), ExportError> {
    let want = space.colorants().ok_or(ExportError::ProfileMissing(space.name()))?;
    for (sig, want) in [b"rXYZ", b"gXYZ", b"bXYZ"].into_iter().zip(want) {
        let tag = icc_tag(icc, sig).ok_or(ExportError::ProfileMismatch("colorant_tag_missing"))?;
        if tag.get(..4) != Some(b"XYZ ") {
            return Err(ExportError::ProfileMismatch("colorant_tag_type"));
        }
        for (k, w) in want.iter().enumerate() {
            let at = 8_usize.checked_add(k.checked_mul(4).ok_or(ExportError::Invalid("overflow"))?);
            let got = at.and_then(|at| s15(tag, at)).ok_or(ExportError::ProfileMismatch("colorant_tag_truncated"))?;
            if (got - w).abs() > 0.002 {
                return Err(ExportError::ProfileMismatch("colorants_differ"));
            }
        }
    }
    for sig in [b"rTRC", b"gTRC", b"bTRC"] {
        let tag = icc_tag(icc, sig).ok_or(ExportError::ProfileMismatch("trc_tag_missing"))?;
        let function = tag.get(8..10).map(|b| u16::from_be_bytes([b[0], b[1]]));
        let gamma = s15(tag, 12);
        if tag.get(..4) != Some(b"para") || function != Some(3) || gamma.is_none_or(|g| (g - 2.4).abs() > 0.01) {
            return Err(ExportError::ProfileMismatch("transfer_is_not_srgb_curve"));
        }
    }
    Ok(())
}

fn validate(settings: &TiffExportSettings) -> Result<(), ExportError> {
    if settings.colour_space.colorants().is_none() {
        return Err(ExportError::ProfileMissing(settings.colour_space.name()));
    }
    if settings.predictor && settings.compression == TiffCompression::None {
        return Err(ExportError::Invalid("predictor_requires_compression"));
    }
    if settings.dpi.is_some_and(|d| d == 0 || d > MAX_DPI) {
        return Err(ExportError::Invalid("invalid_resolution"));
    }
    Ok(())
}

/// Encode an already rendered [`OutputImage`] (the buffer and the profile that describes it). Admission is
/// checked before any sample buffer is built: dimensions against the data length with checked arithmetic and
/// the TIFF encoder's byte budget.
pub fn tiff_from_output(out: &OutputImage, settings: &TiffExportSettings) -> Result<TiffExport, ExportError> {
    validate(settings)?;
    if out.depth_bits != settings.depth.bits() {
        return Err(ExportError::Invalid("depth_mismatch"));
    }
    check_icc_header(&out.icc)?;
    check_profile_is(&out.icc, settings.colour_space)?;
    let samples_n = u64::from(out.width)
        .checked_mul(u64::from(out.height))
        .and_then(|p| p.checked_mul(3))
        .ok_or(ExportError::Invalid("dims_overflow"))?;
    let bytes_per_sample = u64::from(out.depth_bits / 8);
    let data_n = samples_n.checked_mul(bytes_per_sample).ok_or(ExportError::Invalid("dims_overflow"))?;
    if out.width == 0 || out.height == 0 || u64::try_from(out.data.len()).ok() != Some(data_n) {
        return Err(ExportError::Develop(DevelopError::InvalidDimensions));
    }
    if data_n > MAX_ENCODE_BYTES {
        return Err(ExportError::Tiff(TiffError::Limit("limit_encode_bytes")));
    }
    let samples = match settings.depth {
        OutputDepth::Eight => Samples::U8(out.data.clone()),
        OutputDepth::Sixteen => {
            Samples::U16(out.data.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect())
        }
    };
    let request = EncodeRequest {
        width: out.width,
        height: out.height,
        color: ColorModel::Rgb,
        alpha: Alpha::None,
        bit_depth: out.depth_bits,
        samples: &samples,
        icc: Some(&out.icc),
        compression: match settings.compression {
            TiffCompression::None => Compression::None,
            TiffCompression::Lzw => Compression::Lzw,
            TiffCompression::Deflate => Compression::Deflate,
        },
        predictor: if settings.predictor { Predictor::Horizontal } else { Predictor::None },
        big_tiff: settings.big_tiff,
        resolution: settings.dpi.map(|d| Resolution { x: (d, 1), y: (d, 1), unit: 2 }),
        orientation: Some(1),
    };
    let options = EncodeOptions { byte_order: settings.byte_order, metadata: settings.metadata.clone() };
    let bytes = encode_with(&request, &options)?;
    // Read the file back before handing it out: size, depth, samples and profile must be what was encoded.
    let back = hsk_studio_interop_tiff::decode(&bytes, &hsk_studio_interop_tiff::Limits::default(), &CancellationToken::default())?;
    let page = back.pages.first().ok_or(ExportError::Invalid("export_verify_failed"))?;
    if back.pages.len() != 1
        || (page.width, page.height) != (out.width, out.height)
        || page.pixels() != &samples
        || page.icc.as_deref() != Some(out.icc.as_slice())
    {
        return Err(ExportError::Invalid("export_verify_failed"));
    }
    Ok(TiffExport {
        tiff_sha256: Sha256::digest(&bytes).into(),
        bytes,
        width: out.width,
        height: out.height,
        depth_bits: out.depth_bits,
        colour_space: settings.colour_space,
        icc_sha256: out.icc_sha256,
        clip: out.clip,
    })
}

/// Develop `plane` with `recipe` and export it as TIFF in one call. `icc` must be the bytes of the recipe's
/// output profile (hash-checked against `recipe.output_profile.sha256`, as [`crate::develop_to_output`]) and
/// must be the profile of `settings.colour_space`. Settings are checked before any development work.
#[allow(clippy::too_many_arguments)]
pub fn develop_to_tiff(
    plane: &CfaPlane,
    recipe: &DevelopRecipe,
    recipe_revision: u64,
    settings: &TiffExportSettings,
    icc: &[u8],
    threads: Threads,
    cancel: &CancellationToken,
) -> Result<(TiffExport, DevelopReceipt), ExportError> {
    validate(settings)?;
    check_icc_header(icc)?;
    check_profile_is(icc, settings.colour_space)?;
    let (out, receipt) = develop_to_output(plane, recipe, recipe_revision, settings.depth, icc, threads, cancel)?;
    Ok((tiff_from_output(&out, settings)?, receipt))
}

/// Whole dpi of a stage resolution (rounded half up), refused outside 1..=[`MAX_DPI`].
fn stage_dpi(ppi: f64) -> Result<u32, ExportError> {
    let dpi = (ppi + 0.5).floor();
    if !dpi.is_finite() || dpi < 1.0 || dpi > f64::from(MAX_DPI) {
        return Err(ExportError::Invalid("invalid_resolution"));
    }
    Ok(dpi as u32)
}

/// Encode an output rendered from a staged image, writing the stage's resolution as dpi (`receipt.resolution_ppi`
/// rounded half up). `settings.dpi` must be `None` or that same value (`resolution_conflict`); the output size must
/// be the receipt's (`stage_receipt_mismatch`). Otherwise exactly [`tiff_from_output`].
pub fn tiff_from_staged_output(
    out: &OutputImage,
    settings: &TiffExportSettings,
    receipt: &ExportStageReceipt,
) -> Result<TiffExport, ExportError> {
    if (out.width, out.height) != receipt.output_size {
        return Err(ExportError::Invalid("stage_receipt_mismatch"));
    }
    let dpi = stage_dpi(receipt.resolution_ppi)?;
    if settings.dpi.is_some_and(|d| d != dpi) {
        return Err(ExportError::Invalid("resolution_conflict"));
    }
    let mut staged = settings.clone();
    staged.dpi = Some(dpi);
    tiff_from_output(out, &staged)
}

/// [`develop_to_tiff`] with the export stage: develop, stage, render, encode with the stage's dpi. TIFF settings,
/// the profile and every stage setting are checked before any development work.
#[allow(clippy::too_many_arguments)]
pub fn develop_to_tiff_staged(
    plane: &CfaPlane,
    recipe: &DevelopRecipe,
    recipe_revision: u64,
    settings: &TiffExportSettings,
    stage: &ExportStage,
    icc: &[u8],
    threads: Threads,
    cancel: &CancellationToken,
) -> Result<(TiffExport, ExportStageReceipt, DevelopReceipt), ExportError> {
    validate(settings)?;
    check_icc_header(icc)?;
    check_profile_is(icc, settings.colour_space)?;
    validate_export_stage(stage)?;
    let dpi = stage_dpi(stage.sizing.pixels_per_inch())?;
    if settings.dpi.is_some_and(|d| d != dpi) {
        return Err(ExportError::Invalid("resolution_conflict"));
    }
    let (out, stage_receipt, receipt) =
        develop_to_output_staged(plane, recipe, recipe_revision, settings.depth, icc, stage, threads, cancel)?;
    Ok((tiff_from_staged_output(&out, settings, &stage_receipt)?, stage_receipt, receipt))
}
