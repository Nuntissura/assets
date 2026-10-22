//! DNG export of a decoded raw document (lane SMB-DEVELOP-DNG-WRITER, Lightroom Classic export parity, engine
//! level). Writes a DNG 1.4-compatible file (DNGVersion 1.4.0.0, DNGBackwardVersion 1.4.0.0) from a [`DngRaw`]
//! through `hsk-studio-interop-tiff`'s container writer.
//!
//! Spec basis (module 14, studio creative suite): [STU-RAW-017] the raw-side export facts: "a DNG output path
//! with a compatibility version, a compression flag, a conversion method that may preserve the original raw
//! payload, and an embedded-preview cache option"; [STU-RAW-103] export to an interchange container is an
//! export step that reverses the import decode (develop settings travel as the embedded XMP packet).
//!
//! Conversion method (CFA-preserving, lossless): the file stores the decoded CFA frame exactly as develop holds
//! it (`DngRaw::samples`, after the optional LinearizationTable, before black subtraction): photometric
//! 32803 (CFA), the CFA pattern, black levels (repeat pattern plus BlackLevelDeltaH/V), WhiteLevel, ActiveArea
//! and DefaultCrop. Nothing is re-mosaiced, demosaiced or scaled, so the raw data of the file equals the
//! source document sample for sample. A linear-raw (demosaiced) DNG is not written: develop's decode pipeline
//! only holds CFA frames and a linear-raw output would need a lossy demosaic.
//!
//! Profile written: IFD0 (NewSubFileType 1 reduced-resolution RGB preview, 8-bit, uncompressed, sRGB:
//! PreviewColorSpace 1) with the DNG tags (DNGVersion, DNGBackwardVersion, UniqueCameraModel, ColorMatrix1/2,
//! CameraCalibration1/2, ForwardMatrix1/2, CalibrationIlluminant1/2, AnalogBalance, AsShotNeutral or
//! AsShotWhiteXY, BaselineExposure, RawDataUniqueID), Make/Model/Software/DateTime, Orientation, the develop
//! recipe as the XMP packet (tag 700, supplied by the caller from the lrcat XMP codec) and an Exif IFD
//! subset (ExposureTime, FNumber, ISOSpeedRatings, DateTimeOriginal, FocalLength, LensModel); a SubIFD with
//! the raw frame (NewSubFileType 0), uncompressed 16-bit or lossless JPEG (compression 7, SOF3 predictor 1,
//! one stream per strip). With no preview the raw IFD is IFD0 itself.
//!
//! Refusals are typed: non-CFA or inconsistent frames, missing colour data, single-illuminant colour without
//! the caller's opt-in (`DngRaw::allow_approximate_colour`), unapplied source features this file cannot carry
//! (opcode lists, masked areas, ...) unless the caller chose [`UnappliedPolicy::Record`], hostile sizes before
//! allocation. The written bytes are decoded back by develop's own reader and compared before they are handed
//! out. Adobe DNG Converter / Lightroom equivalence is UNVERIFIED (no Lightroom oracle).
#![deny(clippy::arithmetic_side_effects)]
use crate::dng::{self, ActiveArea, DngRaw, MAX_DNG_PIXELS};
use crate::export::ExportError;
use crate::ljpeg_enc;
use crate::output::{check_icc_header, develop_to_output, OutputDepth, OutputImage};
use crate::par::Threads;
use crate::pipeline::{CfaPattern, DevelopReceipt, Limits, XTRANS};
use crate::recipe::DevelopRecipe;
use crate::{DevelopError, RangePolicy};
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_tiff::{write_container, ChildIfds, Container, FieldValue, IfdSpec, Limits as TiffLimits, TiffError};
use sha2::{Digest, Sha256};

/// DNGVersion and DNGBackwardVersion written (STU-RAW-017 compatibility version: DNG 1.4).
pub const DNG_VERSION: [u8; 4] = [1, 4, 0, 0];
/// Largest XMP packet embedded (bytes).
pub const MAX_XMP_BYTES: usize = 4 << 20;
/// Largest written file (bytes), the same budget as the TIFF encoder.
pub const MAX_DNG_BYTES: u64 = 1 << 30;
/// Longest text field (bytes) written to a DNG ASCII tag.
pub const MAX_TEXT_BYTES: usize = 1024;
/// Long edge of [`DngPreview::Medium`].
pub const MEDIUM_PREVIEW_EDGE: u32 = 1024;
/// Largest preview (pixels per side and total RGB bytes).
pub const MAX_PREVIEW_EDGE: u32 = 16_384;
pub const MAX_PREVIEW_BYTES: u64 = 256 << 20;
/// Target size of one strip (bytes of samples).
const STRIP_BYTES: usize = 256 * 1024;

const SUBFILE_PREVIEW: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DngCompression {
    /// 16-bit little-endian samples, TIFF compression 1.
    None,
    /// Lossless JPEG (TIFF compression 7), bit depth = the larger of the source depth and the data's own.
    LosslessJpeg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DngPreview {
    /// No preview: the raw IFD is IFD0.
    None,
    /// Reduced-resolution preview with a long edge of [`MEDIUM_PREVIEW_EDGE`] (never upscaled).
    Medium,
    /// Preview at the full default-window size.
    FullSize,
}

/// What to do with source features this writer cannot carry ([`DngRaw::unapplied`]: opcode lists, masked
/// areas, profile tables, default scale/user crop, and facts a vendor decoder did not read). Orientation is
/// carried and is not part of this policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnappliedPolicy {
    /// Refuse with `unsupported_contribution` naming the first feature.
    Refuse,
    /// Write without them and list them in [`DngExport::dropped`].
    Record,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DngExportSettings {
    pub compression: DngCompression,
    pub preview: DngPreview,
    pub unapplied: UnappliedPolicy,
    /// IFD0 Software (ASCII, at most [`MAX_TEXT_BYTES`]).
    pub software: Option<String>,
    /// UniqueCameraModel override; default is the source document's.
    pub unique_camera_model: Option<String>,
}

impl Default for DngExportSettings {
    fn default() -> Self {
        Self {
            compression: DngCompression::None,
            preview: DngPreview::Medium,
            unapplied: UnappliedPolicy::Refuse,
            software: None,
            unique_camera_model: None,
        }
    }
}

/// The EXIF subset of the original that travels into the DNG. Rationals are (numerator, denominator) as stored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExifSubset {
    pub make: Option<String>,
    pub model: Option<String>,
    /// IFD0 DateTime (306), `YYYY:MM:DD HH:MM:SS`.
    pub date_time: Option<String>,
    pub date_time_original: Option<String>,
    pub exposure_time: Option<(u32, u32)>,
    pub f_number: Option<(u32, u32)>,
    pub iso: Option<u16>,
    pub focal_length: Option<(u32, u32)>,
    pub lens_model: Option<String>,
    /// Why nothing (or less) was read: `not_tiff`, `exif_unreadable`. Extraction never fails the export.
    pub skipped: Option<&'static str>,
}

/// An 8-bit sRGB preview image, interleaved RGB.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewImage {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

/// Everything besides the raw frame that goes into the file.
#[derive(Clone, Copy, Debug, Default)]
pub struct DngParts<'a> {
    /// The XMP packet (tag 700), e.g. from the lrcat XMP codec; validated, never rewritten here.
    pub xmp: Option<&'a [u8]>,
    pub exif: Option<&'a ExifSubset>,
    /// Required when `settings.preview` is not `None`.
    pub preview: Option<&'a PreviewImage>,
}

/// A written DNG with the facts a host records beside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DngExport {
    pub bytes: Vec<u8>,
    pub dng_sha256: [u8; 32],
    pub width: u32,
    pub height: u32,
    /// BitsPerSample of the raw IFD (16 uncompressed; the lossless JPEG precision otherwise).
    pub bits_per_sample: u16,
    pub compression: DngCompression,
    /// SHA-256 of the raw frame as little-endian u16 samples (equals the source document's frame).
    pub samples_sha256: [u8; 32],
    pub raw_payload_bytes: u64,
    pub preview: Option<(u32, u32)>,
    pub xmp_bytes: usize,
    pub exif_fields: Vec<&'static str>,
    /// Source features not written ([`UnappliedPolicy::Record`] only; orientation is written).
    pub dropped: Vec<&'static str>,
    /// Loss note of an opted-in approximate colour route.
    pub colour_loss: Option<&'static str>,
}

fn bad(code: &'static str) -> ExportError {
    ExportError::Invalid(code)
}

fn tiff(e: TiffError) -> ExportError {
    ExportError::Tiff(e)
}

fn ascii_field(text: &str, code: &'static str) -> Result<String, ExportError> {
    if text.is_empty() || text.len() > MAX_TEXT_BYTES || !text.is_ascii() || text.as_bytes().contains(&0) {
        return Err(bad(code));
    }
    Ok(text.to_owned())
}

/// Smallest decimal denominator that reproduces `v` exactly as `n / den` (f64 division), else the finest that
/// fits; `None` when |v| does not fit at all.
fn ratio(v: f64, max_numerator: f64) -> Option<(f64, f64)> {
    if !v.is_finite() {
        return None;
    }
    let mut finest = None;
    for den in [1.0, 10.0, 100.0, 1000.0, 10_000.0, 100_000.0, 1_000_000.0] {
        let n = (v * den).round();
        if n.abs() > max_numerator {
            break;
        }
        finest = Some((n, den));
        if n / den == v {
            return finest;
        }
    }
    finest
}

fn urational(v: f64) -> Result<(u32, u32), ExportError> {
    if v < 0.0 {
        return Err(bad("rational_negative"));
    }
    let (n, d) = ratio(v, f64::from(u32::MAX)).ok_or(bad("rational_out_of_range"))?;
    Ok((n as u32, d as u32))
}

fn srational(v: f64) -> Result<(i32, i32), ExportError> {
    let (n, d) = ratio(v, f64::from(i32::MAX)).ok_or(bad("rational_out_of_range"))?;
    Ok((n as i32, d as i32))
}

fn srationals(values: &[f64]) -> Result<FieldValue, ExportError> {
    Ok(FieldValue::SRational(values.iter().map(|v| srational(*v)).collect::<Result<_, _>>()?))
}

fn urationals(values: &[f64]) -> Result<FieldValue, ExportError> {
    Ok(FieldValue::Rational(values.iter().map(|v| urational(*v)).collect::<Result<_, _>>()?))
}

fn long(v: u32) -> FieldValue {
    FieldValue::Long(vec![v])
}

fn short(v: u16) -> FieldValue {
    FieldValue::Short(vec![v])
}

fn cfa_entries(pattern: CfaPattern) -> Vec<(u16, FieldValue)> {
    let (dim, codes): (u16, Vec<u8>) = match pattern {
        CfaPattern::Bayer(b) => (2, b.cell().iter().flatten().copied().collect()),
        CfaPattern::XTrans => (6, XTRANS.iter().flatten().copied().collect()),
    };
    vec![
        (33421, FieldValue::Short(vec![dim, dim])),
        (33422, FieldValue::Byte(codes)),
        (50710, FieldValue::Byte(vec![0, 1, 2])),
        (50711, short(1)),
    ]
}

/// (rows per strip, strip count) for rows of `row_bytes` bytes.
fn strip_rows(row_bytes: usize, height: usize) -> Result<usize, ExportError> {
    if row_bytes == 0 || height == 0 {
        return Err(bad("invalid_dimensions"));
    }
    Ok(STRIP_BYTES.checked_div(row_bytes).unwrap_or(1).clamp(1, height))
}

struct RawPayload {
    strips: Vec<Vec<u8>>,
    rows_per_strip: u32,
    bits: u16,
    bytes: u64,
}

fn raw_payload(raw: &DngRaw, compression: DngCompression) -> Result<RawPayload, ExportError> {
    let (w, h) = (raw.width as usize, raw.height as usize);
    let samples = raw.samples();
    let expected = w.checked_mul(h).ok_or(bad("dims_overflow"))?;
    if samples.len() != expected {
        return Err(bad("frame_length_mismatch"));
    }
    let max = samples.iter().copied().max().unwrap_or(0);
    let mut strips = Vec::new();
    let (rows, bits) = match compression {
        DngCompression::None => {
            let rows = strip_rows(w.checked_mul(2).ok_or(bad("dims_overflow"))?, h)?;
            for chunk in samples.chunks(rows.checked_mul(w).ok_or(bad("dims_overflow"))?) {
                strips.push(chunk.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>());
            }
            (rows, 16u16)
        }
        DngCompression::LosslessJpeg => {
            let needed = u16::try_from(32u32.saturating_sub(u32::from(max).leading_zeros())).unwrap_or(16).max(2);
            let bits = raw.bits_per_sample.clamp(2, 16).max(needed);
            let rows = strip_rows(w.checked_mul(2).ok_or(bad("dims_overflow"))?, h)?;
            for chunk in samples.chunks(rows.checked_mul(w).ok_or(bad("dims_overflow"))?) {
                let lines = u32::try_from(chunk.len().checked_div(w).ok_or(bad("dims_overflow"))?).map_err(|_| bad("dims_overflow"))?;
                let stream = ljpeg_enc::encode(chunk, raw.width, lines, u8::try_from(bits).map_err(|_| bad("dims_overflow"))?)?;
                strips.push(stream);
            }
            (rows, bits)
        }
    };
    let bytes = strips.iter().fold(0u64, |a, s| a.saturating_add(s.len() as u64));
    Ok(RawPayload { strips, rows_per_strip: u32::try_from(rows).map_err(|_| bad("dims_overflow"))?, bits, bytes })
}

fn black_entries(raw: &DngRaw) -> Result<Vec<(u16, FieldValue)>, ExportError> {
    let (rr, rc) = raw.black_repeat;
    let mut out = vec![(50713, FieldValue::Short(vec![u16::try_from(rr).map_err(|_| bad("invalid_levels"))?, u16::try_from(rc).map_err(|_| bad("invalid_levels"))?]))];
    if raw.black_level.iter().all(|v| v.fract() == 0.0 && (0.0..=65_535.0).contains(v)) {
        out.push((50714, FieldValue::Short(raw.black_level.iter().map(|v| *v as u16).collect())));
    } else if raw.black_level.iter().all(|v| *v >= 0.0) {
        out.push((50714, urationals(&raw.black_level)?));
    } else {
        return Err(ExportError::Develop(DevelopError::InvalidLevels));
    }
    if !raw.black_delta_h.is_empty() {
        out.push((50715, srationals(&raw.black_delta_h)?));
    }
    if !raw.black_delta_v.is_empty() {
        out.push((50716, srationals(&raw.black_delta_v)?));
    }
    Ok(out)
}

fn colour_entries(raw: &DngRaw) -> Result<Vec<(u16, FieldValue)>, ExportError> {
    let c = &raw.color;
    if c.color_matrix.iter().all(Option::is_none) {
        return Err(ExportError::Develop(DevelopError::MissingColourData));
    }
    if c.as_shot_neutral.is_none() && c.as_shot_white_xy.is_none() {
        return Err(bad("as_shot_white_missing"));
    }
    // The develop colour solve is the gate and the validity check (single-illuminant routes refuse unless the
    // caller opted in; a matrix develop cannot solve is not written).
    raw.solve_colour().map_err(ExportError::Develop)?;
    let mut out = Vec::new();
    for i in 0..2usize {
        let (cm, fm, cc, ill) = (c.color_matrix[i], c.forward_matrix[i], c.camera_calibration[i], c.calibration_illuminant[i]);
        if cm.is_none() && (fm.is_some() || cc.is_some() || ill.is_some()) && i == 0 {
            return Err(bad("colour_slot_inconsistent"));
        }
        if cm.is_none() && i == 1 && (fm.is_some() || cc.is_some() || ill.is_some()) {
            return Err(bad("colour_slot_inconsistent"));
        }
        if cm.is_some() && ill.is_none() {
            return Err(bad("calibration_illuminant_missing"));
        }
        let (t_cm, t_cc, t_fm, t_ill) = if i == 0 { (50721u16, 50723u16, 50964u16, 50778u16) } else { (50722, 50724, 50965, 50779) };
        if let Some(m) = cm {
            out.push((t_cm, srationals(&m)?));
        }
        if let Some(m) = cc {
            out.push((t_cc, srationals(&m)?));
        }
        if let Some(m) = fm {
            out.push((t_fm, srationals(&m)?));
        }
        if let Some(code) = ill {
            out.push((t_ill, short(code)));
        }
    }
    if c.analog_balance != [1.0; 3] {
        out.push((50727, urationals(&c.analog_balance)?));
    }
    match (c.as_shot_neutral, c.as_shot_white_xy) {
        (Some(n), _) => out.push((50728, urationals(&n)?)),
        (None, Some(xy)) => out.push((50729, urationals(&xy)?)),
        (None, None) => return Err(bad("as_shot_white_missing")),
    }
    if c.baseline_exposure != 0.0 {
        out.push((50730, srationals(&[c.baseline_exposure])?));
    }
    Ok(out)
}

fn validate_xmp(xmp: &[u8]) -> Result<(), ExportError> {
    if xmp.is_empty() || xmp.len() > MAX_XMP_BYTES {
        return Err(bad("limit_xmp_bytes"));
    }
    let text = std::str::from_utf8(xmp).map_err(|_| bad("xmp_invalid"))?;
    if text.contains('\0') || !text.contains("<x:xmpmeta") {
        return Err(bad("xmp_invalid"));
    }
    Ok(())
}

fn exif_ifd(exif: &ExifSubset, fields: &mut Vec<&'static str>) -> Result<Option<IfdSpec>, ExportError> {
    let mut entries: Vec<(u16, FieldValue)> = Vec::new();
    let rat = |r: (u32, u32)| -> Result<FieldValue, ExportError> {
        if r.1 == 0 {
            return Err(bad("exif_zero_denominator"));
        }
        Ok(FieldValue::Rational(vec![r]))
    };
    if let Some(r) = exif.exposure_time {
        entries.push((33434, rat(r)?));
        fields.push("exposure_time");
    }
    if let Some(r) = exif.f_number {
        entries.push((33437, rat(r)?));
        fields.push("f_number");
    }
    if let Some(v) = exif.iso {
        entries.push((34855, short(v)));
        fields.push("iso");
    }
    if let Some(s) = &exif.date_time_original {
        entries.push((36867, FieldValue::Ascii(ascii_field(s, "exif_text_invalid")?)));
        fields.push("date_time_original");
    }
    if let Some(r) = exif.focal_length {
        entries.push((37386, rat(r)?));
        fields.push("focal_length");
    }
    if let Some(s) = &exif.lens_model {
        entries.push((42036, FieldValue::Ascii(ascii_field(s, "exif_text_invalid")?)));
        fields.push("lens_model");
    }
    if entries.is_empty() {
        return Ok(None);
    }
    entries.push((36864, FieldValue::Undefined(b"0231".to_vec())));
    Ok(Some(IfdSpec { entries, ..IfdSpec::default() }))
}

/// Write `raw` as a DNG. Settings, frame consistency and every size are checked before the first large
/// buffer is built; the result is decoded back and compared before it is returned.
pub fn dng_from_raw(raw: &DngRaw, settings: &DngExportSettings, parts: &DngParts<'_>) -> Result<DngExport, ExportError> {
    // --- admission ---
    let pixels = u64::from(raw.width).saturating_mul(u64::from(raw.height));
    if raw.width == 0 || raw.height == 0 || raw.width > 65_535 || raw.height > 65_535 || pixels > MAX_DNG_PIXELS {
        return Err(ExportError::Develop(DevelopError::InvalidDimensions));
    }
    if pixels.saturating_mul(2) > MAX_DNG_BYTES {
        return Err(tiff(TiffError::Limit("limit_container_bytes")));
    }
    if u64::try_from(raw.samples().len()).ok() != Some(pixels) {
        return Err(bad("frame_length_mismatch"));
    }
    let period = match raw.cfa_pattern {
        CfaPattern::Bayer(_) => 2,
        CfaPattern::XTrans => 6,
    };
    let area: ActiveArea = raw.active_area;
    if area.top >= area.bottom
        || area.left >= area.right
        || area.bottom > raw.height
        || area.right > raw.width
        || area.top.checked_rem(period) != Some(0)
        || area.left.checked_rem(period) != Some(0)
    {
        return Err(ExportError::Develop(DevelopError::UnsupportedActiveArea));
    }
    let white = raw.white_level;
    if white == 0 || white > u32::from(u16::MAX) {
        return Err(ExportError::Develop(DevelopError::InvalidLevels));
    }
    let dropped: Vec<&'static str> = raw.unapplied.iter().copied().filter(|n| *n != "orientation").collect();
    if let (UnappliedPolicy::Refuse, Some(first)) = (settings.unapplied, dropped.first()) {
        return Err(ExportError::Develop(DevelopError::UnsupportedContribution(first)));
    }
    if !(1..=8).contains(&raw.orientation) {
        return Err(bad("invalid_orientation"));
    }
    let model = match (&settings.unique_camera_model, &raw.unique_camera_model) {
        (Some(m), _) | (None, Some(m)) => ascii_field(m, "unique_camera_model_invalid")?,
        (None, None) => return Err(bad("unique_camera_model_missing")),
    };
    let software = settings.software.as_deref().map(|s| ascii_field(s, "software_invalid")).transpose()?;
    if let Some(x) = parts.xmp {
        validate_xmp(x)?;
    }
    let preview = match (settings.preview, parts.preview) {
        (DngPreview::None, _) => None,
        (_, None) => return Err(bad("preview_missing")),
        (_, Some(p)) => {
            let bytes = u64::from(p.width).saturating_mul(u64::from(p.height)).saturating_mul(3);
            if p.width == 0 || p.height == 0 || p.width > MAX_PREVIEW_EDGE || p.height > MAX_PREVIEW_EDGE || bytes > MAX_PREVIEW_BYTES {
                return Err(bad("limit_preview_bytes"));
            }
            if u64::try_from(p.rgb.len()).ok() != Some(bytes) {
                return Err(bad("preview_length_mismatch"));
            }
            Some(p)
        }
    };
    let colour = colour_entries(raw)?;
    let colour_loss = raw.colour_loss();
    let window = raw.default_window();
    let origin_x = window.x.checked_sub(area.left).ok_or(bad("crop_outside_active_area"))?;
    let origin_y = window.y.checked_sub(area.top).ok_or(bad("crop_outside_active_area"))?;
    if window.width == 0
        || window.height == 0
        || window.x.checked_add(window.width).is_none_or(|e| e > area.right)
        || window.y.checked_add(window.height).is_none_or(|e| e > area.bottom)
    {
        return Err(bad("crop_outside_active_area"));
    }

    // --- raw payload ---
    let payload = raw_payload(raw, settings.compression)?;
    let mut samples_hash = Sha256::new();
    let mut le = Vec::with_capacity(64 * 1024);
    for chunk in raw.samples().chunks(32 * 1024) {
        le.clear();
        le.extend(chunk.iter().flat_map(|v| v.to_le_bytes()));
        samples_hash.update(&le);
    }
    let samples_sha256: [u8; 32] = samples_hash.finalize().into();

    // --- IFDs ---
    let raw_tags = |subfile: u32| -> Result<Vec<(u16, FieldValue)>, ExportError> {
        let mut e = vec![
            (254, long(subfile)),
            (256, long(raw.width)),
            (257, long(raw.height)),
            (258, short(payload.bits)),
            (259, short(match settings.compression {
                DngCompression::None => 1,
                DngCompression::LosslessJpeg => 7,
            })),
            (262, short(32803)),
            (277, short(1)),
            (278, long(payload.rows_per_strip)),
            (284, short(1)),
            (50717, long(white)),
            (50719, FieldValue::Long(vec![origin_x, origin_y])),
            (50720, FieldValue::Long(vec![window.width, window.height])),
            (50829, FieldValue::Long(vec![area.top, area.left, area.bottom, area.right])),
        ];
        e.extend(cfa_entries(raw.cfa_pattern));
        e.extend(black_entries(raw)?);
        Ok(e)
    };
    let mut ifd0_tags: Vec<(u16, FieldValue)> = vec![
        (274, short(raw.orientation)),
        (50706, FieldValue::Byte(DNG_VERSION.to_vec())),
        (50707, FieldValue::Byte(DNG_VERSION.to_vec())),
        (50708, FieldValue::Ascii(model)),
        (50781, FieldValue::Byte(samples_sha256.get(..16).unwrap_or(&[]).to_vec())),
    ];
    ifd0_tags.extend(colour);
    let mut exif_fields = Vec::new();
    let mut exif_child = None;
    if let Some(x) = parts.exif {
        if let Some(s) = &x.make {
            ifd0_tags.push((271, FieldValue::Ascii(ascii_field(s, "exif_text_invalid")?)));
            exif_fields.push("make");
        }
        if let Some(s) = &x.model {
            ifd0_tags.push((272, FieldValue::Ascii(ascii_field(s, "exif_text_invalid")?)));
            exif_fields.push("model");
        }
        if let Some(s) = &x.date_time {
            ifd0_tags.push((306, FieldValue::Ascii(ascii_field(s, "exif_text_invalid")?)));
            exif_fields.push("date_time");
        }
        exif_child = exif_ifd(x, &mut exif_fields)?;
    }
    if let Some(s) = software {
        ifd0_tags.push((305, FieldValue::Ascii(s)));
    }
    if let Some(x) = parts.xmp {
        ifd0_tags.push((700, FieldValue::Byte(x.to_vec())));
    }
    let mut children = Vec::new();
    if let Some(e) = exif_child {
        children.push(ChildIfds { tag: 34665, ifds: vec![e] });
    }
    let root = match preview {
        Some(p) => {
            let rows = strip_rows(usize::try_from(p.width).map_err(|_| bad("dims_overflow"))?.checked_mul(3).ok_or(bad("dims_overflow"))?, p.height as usize)?;
            let row_bytes = (p.width as usize).checked_mul(3).ok_or(bad("dims_overflow"))?;
            let strips: Vec<Vec<u8>> = p.rgb.chunks(rows.checked_mul(row_bytes).ok_or(bad("dims_overflow"))?).map(<[u8]>::to_vec).collect();
            let mut e = vec![
                (254, long(SUBFILE_PREVIEW)),
                (256, long(p.width)),
                (257, long(p.height)),
                (258, FieldValue::Short(vec![8, 8, 8])),
                (259, short(1)),
                (262, short(2)),
                (277, short(3)),
                (278, long(u32::try_from(rows).map_err(|_| bad("dims_overflow"))?)),
                (284, short(1)),
                (50970, short(1)),
            ];
            e.extend(ifd0_tags);
            children.insert(0, ChildIfds { tag: 330, ifds: vec![IfdSpec { entries: raw_tags(0)?, strips: payload.strips, children: Vec::new() }] });
            IfdSpec { entries: e, strips, children }
        }
        None => {
            let mut e = raw_tags(0)?;
            e.extend(ifd0_tags);
            IfdSpec { entries: e, strips: payload.strips, children }
        }
    };
    let bytes = write_container(&root, MAX_DNG_BYTES).map_err(tiff)?;
    drop(root);

    // --- read back ---
    let back = dng::decode(&bytes).map_err(ExportError::Develop)?;
    let same_levels = back.black_repeat == raw.black_repeat
        && back.black_level.len() == raw.black_level.len()
        && back.black_level.iter().zip(&raw.black_level).all(|(a, b)| (a - b).abs() <= 1e-6)
        && back.black_delta_h.len() == raw.black_delta_h.len()
        && back.black_delta_v.len() == raw.black_delta_v.len();
    if (back.width, back.height) != (raw.width, raw.height)
        || back.cfa_pattern != raw.cfa_pattern
        || back.active_area != raw.active_area
        || back.white_level != raw.white_level
        || back.orientation != raw.orientation
        || !same_levels
        || back.default_window() != raw.default_window()
        || back.samples() != raw.samples()
    {
        return Err(bad("export_verify_failed"));
    }
    Ok(DngExport {
        dng_sha256: Sha256::digest(&bytes).into(),
        bytes,
        width: raw.width,
        height: raw.height,
        bits_per_sample: payload.bits,
        compression: settings.compression,
        samples_sha256,
        raw_payload_bytes: payload.bytes,
        preview: preview.map(|p| (p.width, p.height)),
        xmp_bytes: parts.xmp.map_or(0, <[u8]>::len),
        exif_fields,
        dropped,
        colour_loss,
    })
}

/// Area-average an 8-bit RGB output down so its long edge is at most `long_edge` (never upscaled). Averages in
/// the encoded (sRGB) domain: a preview, not a measurement.
pub fn reduce_preview(out: &OutputImage, long_edge: Option<u32>) -> Result<PreviewImage, ExportError> {
    if out.depth_bits != 8 {
        return Err(bad("depth_mismatch"));
    }
    let (sw, sh) = (u64::from(out.width), u64::from(out.height));
    let expected = sw.saturating_mul(sh).saturating_mul(3);
    if sw == 0 || sh == 0 || u64::try_from(out.data.len()).ok() != Some(expected) {
        return Err(ExportError::Develop(DevelopError::InvalidDimensions));
    }
    // a * b / c without overflow (inputs are bounded by the 16-bit-per-side checks above and below).
    let mul_div = |a: u64, b: u64, c: u64| a.saturating_mul(b).checked_div(c).unwrap_or(0);
    let (dw, dh) = match long_edge {
        Some(edge) if edge > 0 && sw.max(sh) > u64::from(edge) => {
            let long = sw.max(sh);
            (mul_div(sw, u64::from(edge), long).max(1), mul_div(sh, u64::from(edge), long).max(1))
        }
        _ => (sw, sh),
    };
    if dw > u64::from(MAX_PREVIEW_EDGE) || dh > u64::from(MAX_PREVIEW_EDGE) || dw.saturating_mul(dh).saturating_mul(3) > MAX_PREVIEW_BYTES {
        return Err(bad("limit_preview_bytes"));
    }
    let src_w = usize::try_from(sw).map_err(|_| bad("dims_overflow"))?;
    let mut rgb = Vec::with_capacity(usize::try_from(dw.saturating_mul(dh).saturating_mul(3)).map_err(|_| bad("dims_overflow"))?);
    // Source span [lo, hi) of destination cell `i` of `n` over `len` source units (at least one unit).
    let span = |i: u64, n: u64, len: u64| {
        let lo = mul_div(i, len, n);
        let hi = mul_div(i.saturating_add(1), len, n).max(lo.saturating_add(1)).min(len);
        (lo.min(hi.saturating_sub(1)), hi)
    };
    for y in 0..dh {
        let (y0, y1) = span(y, dh, sh);
        for x in 0..dw {
            let (x0, x1) = span(x, dw, sw);
            let mut sum = [0u64; 3];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let at = usize::try_from(sy)
                        .unwrap_or(0)
                        .saturating_mul(src_w)
                        .saturating_add(usize::try_from(sx).unwrap_or(0))
                        .saturating_mul(3);
                    for (c, s) in sum.iter_mut().enumerate() {
                        *s = s.saturating_add(u64::from(out.data.get(at.saturating_add(c)).copied().unwrap_or(0)));
                    }
                }
            }
            let n = y1.saturating_sub(y0).saturating_mul(x1.saturating_sub(x0)).max(1);
            rgb.extend(sum.iter().map(|s| u8::try_from(s.saturating_add(n.checked_div(2).unwrap_or(0)).checked_div(n).unwrap_or(0)).unwrap_or(u8::MAX)));
        }
    }
    Ok(PreviewImage { width: u32::try_from(dw).map_err(|_| bad("dims_overflow"))?, height: u32::try_from(dh).map_err(|_| bad("dims_overflow"))?, rgb })
}

/// Develop `raw` with `recipe` for the embedded preview (8-bit, default window, `icc` = the recipe's output
/// profile, which must be sRGB: the preview is tagged PreviewColorSpace sRGB) and write the DNG. The raw frame
/// is written untouched; the recipe only shapes the preview (the recipe itself travels as `xmp`). Returns the
/// develop receipt of the preview render (`None` when no preview was requested).
#[allow(clippy::too_many_arguments)]
pub fn develop_to_dng(
    raw: &DngRaw,
    recipe: &DevelopRecipe,
    recipe_revision: u64,
    settings: &DngExportSettings,
    xmp: Option<&[u8]>,
    exif: Option<&ExifSubset>,
    icc: &[u8],
    threads: Threads,
    cancel: &CancellationToken,
) -> Result<(DngExport, Option<DevelopReceipt>), ExportError> {
    if settings.preview == DngPreview::None {
        cancel.check().map_err(|_| ExportError::Develop(DevelopError::Canceled))?;
        let export = dng_from_raw(raw, settings, &DngParts { xmp, exif, preview: None })?;
        return Ok((export, None));
    }
    check_icc_header(icc)?;
    // Refuse what the writer would refuse before the (expensive) preview render.
    dng_from_raw_precheck(raw, settings, xmp)?;
    let window = raw.default_window();
    let (plane, _) = raw.cfa_plane(window, RangePolicy::ClampReported, 1, Limits::LARGE)?;
    let (out, receipt) = develop_to_output(&plane, recipe, recipe_revision, OutputDepth::Eight, icc, threads, cancel)?;
    drop(plane);
    let edge = match settings.preview {
        DngPreview::Medium => Some(MEDIUM_PREVIEW_EDGE),
        _ => None,
    };
    let preview = reduce_preview(&out, edge)?;
    drop(out);
    cancel.check().map_err(|_| ExportError::Develop(DevelopError::Canceled))?;
    let export = dng_from_raw(raw, settings, &DngParts { xmp, exif, preview: Some(&preview) })?;
    Ok((export, Some(receipt)))
}

/// The cheap admission checks of [`dng_from_raw`] that do not need the preview (colour gate, unapplied policy,
/// model, XMP), so a refusal comes before the preview render.
fn dng_from_raw_precheck(raw: &DngRaw, settings: &DngExportSettings, xmp: Option<&[u8]>) -> Result<(), ExportError> {
    if let (UnappliedPolicy::Refuse, Some(first)) = (settings.unapplied, raw.unapplied.iter().find(|n| **n != "orientation")) {
        return Err(ExportError::Develop(DevelopError::UnsupportedContribution(first)));
    }
    colour_entries(raw)?;
    if settings.unique_camera_model.is_none() && raw.unique_camera_model.is_none() {
        return Err(bad("unique_camera_model_missing"));
    }
    if let Some(x) = xmp {
        validate_xmp(x)?;
    }
    Ok(())
}

fn rational_at(bytes: &[u8], at: usize, le: bool) -> Option<(u32, u32)> {
    let word = |o: usize| -> Option<u32> {
        let b: [u8; 4] = bytes.get(o..o.checked_add(4)?)?.try_into().ok()?;
        Some(if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    };
    let (n, d) = (word(at)?, word(at.checked_add(4)?)?);
    (d != 0).then_some((n, d))
}

/// Read the EXIF subset a DNG export carries from the original container bytes (a TIFF-based raw or DNG).
/// Never fails: a source that is not TIFF-based (CR3, RAF, ...) or whose EXIF is unreadable yields what could
/// be read and `skipped` names why.
pub fn exif_subset(source: &[u8]) -> ExifSubset {
    let mut out = ExifSubset::default();
    let limits = TiffLimits::default();
    let c = match Container::parse(source, &limits) {
        Ok(c) => c,
        Err(_) => {
            out.skipped = Some("not_tiff");
            return out;
        }
    };
    let le = c.byte_order() == hsk_studio_interop_tiff::ByteOrder::Little;
    let text = |e: &hsk_studio_interop_tiff::IfdEntry| -> Option<String> {
        let s = c.ascii(e).ok()?;
        let s = s.trim().to_owned();
        (!s.is_empty() && s.len() <= MAX_TEXT_BYTES && s.is_ascii()).then_some(s)
    };
    let rational = |e: &hsk_studio_interop_tiff::IfdEntry| -> Option<(u32, u32)> {
        if e.field_type != 5 || e.count == 0 {
            return None;
        }
        let raw = c.region(e.value_offset, e.value_len.min(8)).ok()?;
        rational_at(raw, 0, le)
    };
    let first = match c.ifd(c.first_ifd_offset()) {
        Ok(i) => i,
        Err(_) => {
            out.skipped = Some("exif_unreadable");
            return out;
        }
    };
    out.make = first.find(271).and_then(&text);
    out.model = first.find(272).and_then(&text);
    out.date_time = first.find(306).and_then(&text);
    if let Some(e) = first.find(34665) {
        match c.uints(e).ok().and_then(|v| v.first().copied()).map(|off| c.ifd(off)) {
            Some(Ok(exif)) => {
                out.exposure_time = exif.find(33434).and_then(&rational);
                out.f_number = exif.find(33437).and_then(&rational);
                out.iso = exif.find(34855).and_then(|e| c.uints(e).ok()).and_then(|v| v.first().copied()).and_then(|v| u16::try_from(v).ok());
                out.date_time_original = exif.find(36867).and_then(&text);
                out.focal_length = exif.find(37386).and_then(&rational);
                out.lens_model = exif.find(42036).and_then(&text);
            }
            _ => out.skipped = Some("exif_unreadable"),
        }
    }
    out
}
