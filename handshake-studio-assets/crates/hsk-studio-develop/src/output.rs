//! Develop-to-output (lane SMB-DEVELOP-CONTROLS slice 4): render a developed image to an interleaved
//! RGB sample buffer plus the ICC profile bytes that describe it, so a host (the harness) can hand
//! both to an encoder and write a JPEG or PNG with the profile embedded.
//!
//! Contract:
//! * the input is [`DevelopedImage`]: linear-light RGB in the recipe's OUTPUT profile (develop has
//!   already converted working -> output when a converter ran);
//! * the output transfer is the sRGB OETF (IEC 61966-2-1 piecewise curve). It is also the transfer
//!   curve of Display P3, so the same function serves both; the caller must pass the ICC bytes of a
//!   profile whose curve is that one. Develop does not parse the curve: it checks the ICC header
//!   (size field, `acsp` signature, `RGB ` colour space, not a link/abstract profile) and, when the
//!   caller gives the recipe's `ProfileBinding::sha256`, that the bytes hash to exactly that value;
//! * values below 0 and above 1 are clamped and COUNTED in [`ClipReport`] (never silent); a non-finite
//!   value refuses; rounding is round-half-up on `v * max`, with no dithering, so the buffer is a pure
//!   function of the image;
//! * 8-bit samples are one byte each; 16-bit samples are two bytes, big-endian (PNG wire order).
//!
//! Harness wiring (for the lead): call [`develop_to_output`] (or [`render_output`] on an existing
//! [`DevelopedImage`]) with the ICC bytes the Prism/ArtifactService owner resolved for
//! `recipe.output_profile`, then pass `width`, `height`, `depth_bits`, `data` and `icc` to the
//! JPEG (8-bit only) or PNG encoder. The JPEG/PNG writers themselves are not in this crate.
use crate::DevelopError;
use crate::adjust::enc;
use crate::par::Threads;
use crate::pipeline::{DevelopReceipt, DevelopedImage, develop_to_image_threads};
use crate::recipe::DevelopRecipe;
use crate::CfaPlane;
use hsk_studio_accord::CancellationToken;
use sha2::{Digest, Sha256};

/// Largest ICC profile accepted (bytes).
pub const MAX_ICC_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputDepth {
    Eight,
    Sixteen,
}

impl OutputDepth {
    pub const fn bits(self) -> u8 {
        match self {
            Self::Eight => 8,
            Self::Sixteen => 16,
        }
    }
}

/// Channel values that left 0..1 and were clamped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClipReport {
    pub below_zero: u64,
    pub above_one: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputImage {
    pub width: u32,
    pub height: u32,
    pub depth_bits: u8,
    /// Always `"srgb_trc"` (the sRGB / Display P3 transfer curve).
    pub transfer: &'static str,
    /// Interleaved R, G, B; `width * height * 3` samples of `depth_bits / 8` bytes.
    pub data: Vec<u8>,
    /// The profile bytes describing `data` (copied from the caller, header-checked).
    pub icc: Vec<u8>,
    pub icc_sha256: [u8; 32],
    pub clip: ClipReport,
}

/// ICC header checks (ICC.1 clause 7.2): size, signature, RGB data colour space, profile class.
pub fn check_icc_header(icc: &[u8]) -> Result<(), DevelopError> {
    if icc.len() < 128 || icc.len() > MAX_ICC_BYTES {
        return Err(DevelopError::InvalidProfile);
    }
    let size = u32::from_be_bytes([icc[0], icc[1], icc[2], icc[3]]) as usize;
    let class = &icc[12..16];
    if size != icc.len()
        || &icc[36..40] != b"acsp"
        || &icc[16..20] != b"RGB "
        || class == b"link"
        || class == b"abst"
        || icc[8] < 2
    {
        return Err(DevelopError::InvalidProfile);
    }
    Ok(())
}

fn encode_sample(value: f32, max: f64, clip: &mut ClipReport) -> Result<u32, DevelopError> {
    if !value.is_finite() {
        return Err(DevelopError::NonFinite);
    }
    let linear = f64::from(value);
    let clamped = if linear < 0.0 {
        clip.below_zero += 1;
        0.0
    } else if linear > 1.0 {
        clip.above_one += 1;
        1.0
    } else {
        linear
    };
    // `enc` is the signed sRGB OETF; on 0..1 it stays in 0..1 up to rounding, which the clamp absorbs.
    let encoded = enc(clamped).clamp(0.0, 1.0);
    Ok((encoded * max + 0.5).floor() as u32)
}

/// Render `image` (linear, output-profile) to `depth` samples with `icc` attached. When
/// `expected_sha256` is given (the recipe's output `ProfileBinding::sha256`), `icc` must hash to it.
pub fn render_output(
    image: &DevelopedImage,
    depth: OutputDepth,
    icc: &[u8],
    expected_sha256: Option<[u8; 32]>,
) -> Result<OutputImage, DevelopError> {
    check_icc_header(icc)?;
    let icc_sha256: [u8; 32] = Sha256::digest(icc).into();
    if expected_sha256.is_some_and(|expected| expected != icc_sha256) {
        return Err(DevelopError::InvalidProfile);
    }
    let pixels = (image.width as usize)
        .checked_mul(image.height as usize)
        .ok_or(DevelopError::Overflow)?;
    if image.width == 0 || image.height == 0 || pixels.checked_mul(3) != Some(image.rgb.len()) {
        return Err(DevelopError::InvalidDimensions);
    }
    let bytes_per_sample = usize::from(depth.bits() / 8);
    let mut data = Vec::with_capacity(image.rgb.len().checked_mul(bytes_per_sample).ok_or(DevelopError::Overflow)?);
    let mut clip = ClipReport::default();
    let max = f64::from((1u32 << depth.bits()) - 1);
    for value in &image.rgb {
        let sample = encode_sample(*value, max, &mut clip)?;
        match depth {
            OutputDepth::Eight => data.push(sample as u8),
            OutputDepth::Sixteen => data.extend_from_slice(&(sample as u16).to_be_bytes()),
        }
    }
    Ok(OutputImage {
        width: image.width,
        height: image.height,
        depth_bits: depth.bits(),
        transfer: "srgb_trc",
        data,
        icc: icc.to_vec(),
        icc_sha256,
        clip,
    })
}

/// Develop a plane with `recipe` (including every adjustment and whole-frame stage) and render it
/// for export in one call. `icc` must be the bytes of the recipe's output profile: its hash is
/// checked against `recipe.output_profile.sha256`.
pub fn develop_to_output(
    plane: &CfaPlane,
    recipe: &DevelopRecipe,
    recipe_revision: u64,
    depth: OutputDepth,
    icc: &[u8],
    threads: Threads,
    cancel: &CancellationToken,
) -> Result<(OutputImage, DevelopReceipt), DevelopError> {
    check_icc_header(icc)?;
    if Sha256::digest(icc).as_slice() != recipe.output_profile.sha256 {
        return Err(DevelopError::InvalidProfile);
    }
    let (image, receipt) = develop_to_image_threads(plane, recipe, recipe_revision, threads, cancel)?;
    let output = render_output(&image, depth, icc, Some(recipe.output_profile.sha256))?;
    Ok((output, receipt))
}
