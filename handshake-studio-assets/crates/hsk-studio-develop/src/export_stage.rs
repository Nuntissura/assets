//! Export stage (lane SMB-LR-EXPORT-SIZING-1, Lightroom Classic export parity row LR-33, engine level): the one
//! develop OUTPUT stage every export passes through, on the developed linear-light image ([`DevelopedImage`], in
//! the recipe's output profile) before [`crate::render_output`] applies the transfer curve, so the PNG, JPEG,
//! TIFF and DNG-preview paths share it. Order: image sizing, then output sharpening at the output size, then the
//! graphic watermark.
//!
//! Spec basis (module 14, studio creative suite): STU-IO-134 (per key: stored export setting or surface-only
//! field; never promoted silently, see [`EXPORT_STAGE_KEYS`]), STU-IO-135 (groups `image_sizing`,
//! `output_sharpening`, `watermarking`), STU-IO-136 (resize type, resolution unit, sharpening media and amount
//! domains), STU-IO-137 (max width/height 1000, percentage 100, resolution 240, hard minimum 1, unknown hard
//! maximum), STU-IO-138 (`constrain_size`, `do_not_enlarge`, `use_watermark`; a split preset spread gets a default
//! labelled `studio_declared`). STU-IO-103: a setting whose hard maximum is unknown is not clamped; what the
//! stage cannot produce is the stage's own typed refusal ([`ExportStageError::OutputTooLarge`]).
//!
//! Image sizing (Studio reading; Lightroom Classic behaviour UNVERIFIED, D5):
//! * `constrain_size` false keeps the source size whatever the resize type says (rule `unconstrained`).
//! * Boxes: `max_width` / `max_height` are in `size_unit`; `in` and `cm` become pixels through the resolution
//!   (`ppcm` x 2.54 = ppi), rounded half up to whole pixels before fitting.
//! * `WidthHeight`: scale = min(W / w, H / h), the box as given. `Dimensions`: the long box edge fits the long
//!   image edge and the short box edge the short image edge, so either orientation fits. `LongEdge` and
//!   `ShortEdge`: that image edge becomes `max_width` (the single edge field; `max_height` unused). `Megapixels`:
//!   scale = sqrt(megapixels x 10^6 / (w h)), aspect kept. `Percentage`: scale = percentage / 100.
//! * `do_not_enlarge` caps the scale at 1 (never upsamples). Rounding: each edge is `w x scale` rounded half up,
//!   at least 1; `Megapixels` floors both edges (a 1e-7 guard absorbs binary64 error) so the area never exceeds
//!   the target. An output equal to the source size is the source, copied, not resampled.
//! * Resampling ([`RESAMPLE_FILTER`]): separable, in linear light, binary64 accumulation. The kernel is Keys cubic
//!   convolution `a = -0.5` evaluated through `hsk_studio_pigment::transform_math::catmull_rom` (k(x) = weight 1 of
//!   `catmull_rom(|x|)` below 1, weight 0 of `catmull_rom(|x| - 1)` below 2). For a reduction by factor `f` the
//!   kernel is stretched by `f` (support 4f source pixels), so its cutoff follows the output Nyquist frequency and
//!   a reduction does not alias; enlargement uses the unstretched kernel (plain Catmull-Rom interpolation). Output
//!   pixel `o` is centred on source coordinate `(o + 0.5) f - 0.5`; taps outside the source replicate the edge;
//!   weights are normalised to sum 1 and accumulated as differences from the first tap, so a flat field stays
//!   exactly flat. Values are not clamped here: the cubic's lobes may leave 0..1 and [`crate::render_output`]
//!   clamps and counts them (receipt `out_of_range`).
//! * Resolution is recorded in the receipt (`resolution_ppi`); the TIFF path writes it as whole dpi.
#![deny(clippy::arithmetic_side_effects)]
use crate::DevelopError;
use crate::output::ClipReport;
use crate::pipeline::DevelopedImage;
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::transform_math::catmull_rom;

/// Largest output edge the stage produces (Studio stage limit, not a clamp of a setting: STU-IO-103).
pub const MAX_OUTPUT_SIDE: u32 = 65_535;
/// Largest output (and intermediate pass) area in pixels.
pub const MAX_OUTPUT_PIXELS: u64 = 1 << 28;
/// The reduction / enlargement filter named in receipts.
pub const RESAMPLE_FILTER: &str = "keys_cubic_a-0.5_stretched_by_reduction_factor";

/// Resize type (STU-IO-136, six members).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeType {
    WidthHeight,
    Dimensions,
    LongEdge,
    ShortEdge,
    Megapixels,
    Percentage,
}

impl ResizeType {
    /// The STU-IO-136 member name.
    pub const fn spec_member(self) -> &'static str {
        match self {
            Self::WidthHeight => "WidthHeight",
            Self::Dimensions => "Dimensions",
            Self::LongEdge => "LongEdge",
            Self::ShortEdge => "ShortEdge",
            Self::Megapixels => "Megapixels",
            Self::Percentage => "Percentage",
        }
    }

    /// The fit rule recorded in the receipt.
    pub const fn fit_rule(self) -> &'static str {
        match self {
            Self::WidthHeight => "width_height",
            Self::Dimensions => "dimensions",
            Self::LongEdge => "long_edge",
            Self::ShortEdge => "short_edge",
            Self::Megapixels => "megapixels",
            Self::Percentage => "percentage",
        }
    }
}

/// Unit of `max_width` / `max_height` (STU-IO-136 resolution unit members `pixels`, `in`, `cm`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeUnit {
    Pixels,
    Inches,
    Centimetres,
}

impl SizeUnit {
    pub const fn spec_member(self) -> &'static str {
        match self {
            Self::Pixels => "pixels",
            Self::Inches => "in",
            Self::Centimetres => "cm",
        }
    }
}

/// Unit of `resolution` (STU-IO-136 resolution unit members `ppinch`, `ppcm`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionUnit {
    PixelsPerInch,
    PixelsPerCm,
}

impl ResolutionUnit {
    pub const fn spec_member(self) -> &'static str {
        match self {
            Self::PixelsPerInch => "ppinch",
            Self::PixelsPerCm => "ppcm",
        }
    }
}

/// Image sizing settings (`image_sizing` group, STU-IO-135). Defaults: [`EXPORT_STAGE_KEYS`].
#[derive(Clone, Debug, PartialEq)]
pub struct OutputSizing {
    /// Resize at all (Lightroom "Resize to Fit" reading, UNVERIFIED); STU-IO-138, studio_declared true (5 of 9).
    pub constrain_size: bool,
    pub resize_type: ResizeType,
    /// STU-IO-137: hard minimum 1, default 1000, in `size_unit`. The single edge of `LongEdge` / `ShortEdge`.
    pub max_width: f64,
    /// STU-IO-137: hard minimum 1, default 1000, in `size_unit`.
    pub max_height: f64,
    pub size_unit: SizeUnit,
    /// STU-IO-137: hard minimum 1, default 100 (percent).
    pub percentage: f64,
    /// Target megapixels of `Megapixels` (Studio reading: the spec names no megapixel field; above 0).
    pub megapixels: f64,
    /// STU-IO-137: hard minimum 1, default 240, in `resolution_unit`.
    pub resolution: f64,
    pub resolution_unit: ResolutionUnit,
    /// STU-IO-138, studio_declared false (4 of 5).
    pub do_not_enlarge: bool,
}

impl Default for OutputSizing {
    fn default() -> Self {
        Self {
            constrain_size: true,
            resize_type: ResizeType::WidthHeight,
            max_width: 1000.0,
            max_height: 1000.0,
            size_unit: SizeUnit::Pixels,
            percentage: 100.0,
            megapixels: 2.0,
            resolution: 240.0,
            resolution_unit: ResolutionUnit::PixelsPerInch,
            do_not_enlarge: false,
        }
    }
}

/// Storage class of a stage key (STU-IO-134).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyStorage {
    /// A setting the spec names (STU-IO-136/137/138): a stored export setting.
    StoredSetting,
    /// A Studio reading the spec does not enumerate: its stored/surface-only class is unknown and it is NOT
    /// promoted to a stored setting (spec-delta request in the lane notes).
    Unclassified,
}

/// One key of the stage's settings record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StageKey {
    pub key: &'static str,
    /// STU-IO-135 group.
    pub group: &'static str,
    pub storage: KeyStorage,
    pub default: &'static str,
    /// The clause that declares the default, or `studio_declared`.
    pub default_source: &'static str,
}

const fn key(key: &'static str, group: &'static str, storage: KeyStorage, default: &'static str, source: &'static str) -> StageKey {
    StageKey { key, group, storage, default, default_source: source }
}

/// Every key the stage reads, with its STU-IO-134 storage class and default provenance.
pub const EXPORT_STAGE_KEYS: &[StageKey] = &[
    key("constrain_size", "image_sizing", KeyStorage::StoredSetting, "true", "studio_declared"),
    key("resize_type", "image_sizing", KeyStorage::StoredSetting, "WidthHeight", "studio_declared"),
    key("max_width", "image_sizing", KeyStorage::StoredSetting, "1000", "STU-IO-137"),
    key("max_height", "image_sizing", KeyStorage::StoredSetting, "1000", "STU-IO-137"),
    key("size_unit", "image_sizing", KeyStorage::StoredSetting, "pixels", "studio_declared"),
    key("resize_percentage", "image_sizing", KeyStorage::StoredSetting, "100", "STU-IO-137"),
    key("megapixels", "image_sizing", KeyStorage::Unclassified, "2", "studio_declared"),
    key("resolution", "image_sizing", KeyStorage::StoredSetting, "240", "STU-IO-137"),
    key("resolution_unit", "image_sizing", KeyStorage::StoredSetting, "ppinch", "studio_declared"),
    key("do_not_enlarge", "image_sizing", KeyStorage::StoredSetting, "false", "studio_declared"),
];

/// The whole export stage.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExportStage {
    pub sizing: OutputSizing,
}

/// A typed refusal of the stage; nothing is clamped, defaulted or skipped silently.
#[derive(Clone, Debug, PartialEq)]
pub enum ExportStageError {
    /// A setting that is NaN or infinite (key inside).
    NonFinite(&'static str),
    /// A setting outside its hard bounds (key inside).
    OutOfRange(&'static str),
    /// The output (or an intermediate pass) would exceed [`MAX_OUTPUT_SIDE`] / [`MAX_OUTPUT_PIXELS`].
    OutputTooLarge,
    Develop(DevelopError),
}

impl ExportStageError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NonFinite(_) => "export_setting_not_finite",
            Self::OutOfRange(_) => "export_setting_out_of_range",
            Self::OutputTooLarge => "export_output_too_large",
            Self::Develop(e) => e.code(),
        }
    }

    /// The key a setting refusal names; empty otherwise.
    pub fn detail(&self) -> &'static str {
        match self {
            Self::NonFinite(k) | Self::OutOfRange(k) => k,
            _ => "",
        }
    }
}

impl From<DevelopError> for ExportStageError {
    fn from(e: DevelopError) -> Self {
        Self::Develop(e)
    }
}

/// Output size chosen by [`fit_output_size`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SizeFit {
    pub width: u32,
    pub height: u32,
    /// [`ResizeType::fit_rule`] or `unconstrained`.
    pub rule: &'static str,
    /// The uniform scale before rounding (1 when unconstrained or capped by `do_not_enlarge`).
    pub scale: f64,
}

/// What the stage did, for the host to record beside the export.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportStageReceipt {
    pub source_size: (u32, u32),
    pub output_size: (u32, u32),
    pub fit_rule: &'static str,
    pub scale: f64,
    /// False when the output is the source size (copied, not resampled).
    pub resized: bool,
    pub filter: &'static str,
    /// Output resolution in pixels per inch (`ppcm` x 2.54).
    pub resolution_ppi: f64,
    /// Channel values of the staged linear image outside 0..1: exactly what the output transfer clamps and counts.
    pub out_of_range: ClipReport,
}

fn finite_min(value: f64, min: f64, key: &'static str) -> Result<(), ExportStageError> {
    if !value.is_finite() {
        return Err(ExportStageError::NonFinite(key));
    }
    if value < min {
        return Err(ExportStageError::OutOfRange(key));
    }
    Ok(())
}

fn round_half_up(v: f64) -> f64 {
    (v + 0.5).floor()
}

impl OutputSizing {
    fn validate(&self) -> Result<(), ExportStageError> {
        finite_min(self.max_width, 1.0, "max_width")?;
        finite_min(self.max_height, 1.0, "max_height")?;
        finite_min(self.percentage, 1.0, "resize_percentage")?;
        finite_min(self.resolution, 1.0, "resolution")?;
        if !self.megapixels.is_finite() {
            return Err(ExportStageError::NonFinite("megapixels"));
        }
        if self.megapixels <= 0.0 {
            return Err(ExportStageError::OutOfRange("megapixels"));
        }
        Ok(())
    }

    /// Output resolution in pixels per inch.
    pub fn pixels_per_inch(&self) -> f64 {
        match self.resolution_unit {
            ResolutionUnit::PixelsPerInch => self.resolution,
            ResolutionUnit::PixelsPerCm => self.resolution * 2.54,
        }
    }

    /// A box edge in whole pixels (rounded half up).
    fn box_pixels(&self, value: f64) -> f64 {
        let ppi = self.pixels_per_inch();
        round_half_up(match self.size_unit {
            SizeUnit::Pixels => value,
            SizeUnit::Inches => value * ppi,
            SizeUnit::Centimetres => value / 2.54 * ppi,
        })
    }
}

fn edge(source: f64, scale: f64, floor: bool) -> Result<u32, ExportStageError> {
    let v = if floor { (source * scale + 1e-7).floor() } else { round_half_up(source * scale) };
    let v = v.max(1.0);
    if !v.is_finite() || v > f64::from(MAX_OUTPUT_SIDE) {
        return Err(ExportStageError::OutputTooLarge);
    }
    Ok(v as u32)
}

/// The output size of a `width` x `height` source under `sizing` (settings checked first).
pub fn fit_output_size(sizing: &OutputSizing, width: u32, height: u32) -> Result<SizeFit, ExportStageError> {
    sizing.validate()?;
    if width == 0 || height == 0 {
        return Err(DevelopError::InvalidDimensions.into());
    }
    if !sizing.constrain_size {
        return Ok(SizeFit { width, height, rule: "unconstrained", scale: 1.0 });
    }
    let (w, h) = (f64::from(width), f64::from(height));
    let (long, short) = (w.max(h), w.min(h));
    let (bw, bh) = (sizing.box_pixels(sizing.max_width), sizing.box_pixels(sizing.max_height));
    let t = sizing.resize_type;
    let scale = match t {
        ResizeType::WidthHeight => (bw / w).min(bh / h),
        ResizeType::Dimensions => (bw.max(bh) / long).min(bw.min(bh) / short),
        ResizeType::LongEdge => bw / long,
        ResizeType::ShortEdge => bw / short,
        ResizeType::Megapixels => (sizing.megapixels * 1e6 / (w * h)).sqrt(),
        ResizeType::Percentage => sizing.percentage / 100.0,
    };
    let scale = if sizing.do_not_enlarge { scale.min(1.0) } else { scale };
    if !scale.is_finite() {
        return Err(ExportStageError::OutputTooLarge);
    }
    if scale == 1.0 {
        return Ok(SizeFit { width, height, rule: t.fit_rule(), scale });
    }
    let floor = t == ResizeType::Megapixels;
    let (ow, oh) = (edge(w, scale, floor)?, edge(h, scale, floor)?);
    if u64::from(ow).checked_mul(u64::from(oh)).is_none_or(|p| p > MAX_OUTPUT_PIXELS) {
        return Err(ExportStageError::OutputTooLarge);
    }
    Ok(SizeFit { width: ow, height: oh, rule: t.fit_rule(), scale })
}

/// Keys cubic convolution kernel (`a = -0.5`), read from pigment's Catmull-Rom weights.
fn keys_kernel(x: f64) -> f64 {
    let a = x.abs();
    if a < 1.0 {
        catmull_rom(a)[1]
    } else if a < 2.0 {
        catmull_rom(a - 1.0)[0]
    } else {
        0.0
    }
}

/// Taps (source index, normalised weight) of every output coordinate along one axis.
type Taps = Vec<Vec<(usize, f64)>>;

fn resample_taps(src: u32, dst: u32) -> Result<Taps, ExportStageError> {
    let last = i64::from(src.checked_sub(1).ok_or(DevelopError::InvalidDimensions)?);
    let ratio = f64::from(src) / f64::from(dst);
    let stretch = ratio.max(1.0);
    let mut all = Vec::with_capacity(usize::try_from(dst).map_err(|_| ExportStageError::OutputTooLarge)?);
    for o in 0..dst {
        let centre = (f64::from(o) + 0.5) * ratio - 0.5;
        let lo = (centre - 2.0 * stretch).ceil() as i64;
        let hi = (centre + 2.0 * stretch).floor() as i64;
        let mut taps = Vec::new();
        let mut sum = 0.0;
        for i in lo..=hi {
            let w = keys_kernel((i as f64 - centre) / stretch);
            if w != 0.0 {
                taps.push((i.clamp(0, last) as usize, w));
                sum += w;
            }
        }
        if !sum.is_finite() || sum <= 0.0 {
            return Err(DevelopError::InvalidDimensions.into());
        }
        for t in &mut taps {
            t.1 /= sum;
        }
        all.push(taps);
    }
    Ok(all)
}

/// Weighted sum as differences from the first tap (a constant input returns that constant exactly).
fn accumulate<const C: usize>(taps: &[(usize, f64)], read: impl Fn(usize) -> [f32; C]) -> [f32; C] {
    let Some(&(first, _)) = taps.first() else {
        return [0.0; C];
    };
    let base = read(first);
    let mut acc = [0.0f64; C];
    for &(i, w) in taps {
        for ((a, v), b) in acc.iter_mut().zip(read(i)).zip(base) {
            *a += w * (f64::from(v) - f64::from(b));
        }
    }
    let mut out = base;
    for (o, a) in out.iter_mut().zip(acc) {
        *o = (f64::from(*o) + a) as f32;
    }
    out
}

fn area(w: usize, h: usize) -> Result<usize, ExportStageError> {
    let n = w.checked_mul(h).ok_or(ExportStageError::OutputTooLarge)?;
    if u64::try_from(n).ok().is_none_or(|n| n > MAX_OUTPUT_PIXELS) {
        return Err(ExportStageError::OutputTooLarge);
    }
    Ok(n)
}

/// Separable filter: rows through `xt` (output width `xt.len()`), then columns through `yt`.
pub(crate) fn separable<const C: usize>(
    src: &[[f32; C]],
    src_width: usize,
    xt: &Taps,
    yt: &Taps,
    cancel: &CancellationToken,
) -> Result<Vec<[f32; C]>, ExportStageError> {
    let (dw, dh) = (xt.len(), yt.len());
    if src_width == 0 || dw == 0 || dh == 0 {
        return Err(DevelopError::InvalidDimensions.into());
    }
    let src_height = src.len().checked_div(src_width).ok_or(DevelopError::InvalidDimensions)?;
    let mut mid = Vec::with_capacity(area(dw, src_height)?);
    for row in src.chunks_exact(src_width) {
        cancel.check().map_err(DevelopError::from)?;
        for taps in xt {
            mid.push(accumulate(taps, |i| row[i]));
        }
    }
    let rows: Vec<&[[f32; C]]> = mid.chunks_exact(dw).collect();
    let mut out = Vec::with_capacity(area(dw, dh)?);
    for taps in yt {
        cancel.check().map_err(DevelopError::from)?;
        out.extend((0..dw).map(|x| accumulate(taps, |j| rows[j][x])));
    }
    Ok(out)
}

fn to_pixels(rgb: &[f32]) -> Vec<[f32; 3]> {
    rgb.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect()
}

fn count_out_of_range(rgb: &[f32]) -> Result<ClipReport, DevelopError> {
    let mut clip = ClipReport::default();
    for v in rgb {
        if *v < 0.0 {
            clip.below_zero = clip.below_zero.checked_add(1).ok_or(DevelopError::Overflow)?;
        } else if *v > 1.0 {
            clip.above_one = clip.above_one.checked_add(1).ok_or(DevelopError::Overflow)?;
        }
    }
    Ok(clip)
}

/// Run the export stage on a developed linear-light image. Settings are checked before any pixel work; the image
/// must be consistent and finite.
pub fn apply_export_stage(
    image: &DevelopedImage,
    stage: &ExportStage,
    cancel: &CancellationToken,
) -> Result<(DevelopedImage, ExportStageReceipt), ExportStageError> {
    stage.sizing.validate()?;
    let pixels = usize::try_from(u64::from(image.width).checked_mul(u64::from(image.height)).ok_or(DevelopError::Overflow)?)
        .map_err(|_| DevelopError::Overflow)?;
    if image.width == 0 || image.height == 0 || pixels.checked_mul(3) != Some(image.rgb.len()) {
        return Err(DevelopError::InvalidDimensions.into());
    }
    if image.rgb.iter().any(|v| !v.is_finite()) {
        return Err(DevelopError::NonFinite.into());
    }
    cancel.check().map_err(DevelopError::from)?;
    let fit = fit_output_size(&stage.sizing, image.width, image.height)?;
    let resized = (fit.width, fit.height) != (image.width, image.height);
    let out = if resized {
        let xt = resample_taps(image.width, fit.width)?;
        let yt = resample_taps(image.height, fit.height)?;
        let src = to_pixels(&image.rgb);
        let px = separable(&src, image.width as usize, &xt, &yt, cancel)?;
        DevelopedImage { width: fit.width, height: fit.height, rgb: px.into_iter().flatten().collect() }
    } else {
        image.clone()
    };
    let receipt = ExportStageReceipt {
        source_size: (image.width, image.height),
        output_size: (out.width, out.height),
        fit_rule: fit.rule,
        scale: fit.scale,
        resized,
        filter: RESAMPLE_FILTER,
        resolution_ppi: stage.sizing.pixels_per_inch(),
        out_of_range: count_out_of_range(&out.rgb)?,
    };
    Ok((out, receipt))
}
