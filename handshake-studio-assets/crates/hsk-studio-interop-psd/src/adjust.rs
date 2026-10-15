//! PSD adjustment-layer data, Blend-If ranges, mask parameters and the advanced-blending toggles
//! mapped to Folio raster records (`hsk_studio_folio::StudioRasterRecords`, STU-RAS-041, 115, 124,
//! 151-152, 159; architecture decision work-folder `diagnostics/adjustments-advanced-blending.md`).
//!
//! These are RECORDS only: nothing here applies an adjustment. Every parser takes hostile bytes:
//! counts are bounded before any loop, every read is bounds-checked, integer arithmetic is denied
//! (workflow V4-01) and a value outside the documented range, NaN and infinities included, refuses the
//! whole record with a fixed reason code, so the caller keeps the layer as a typed unsupported layer and
//! the source bytes stay untouched in the document (proof: `tests/adjustment_records.rs`
//! `non_finite_descriptor_numbers_are_typed_losses_not_import_failures`, which feeds NaN, +inf, -inf and
//! out-of-range numbers to brightness/contrast, exposure, levels, hue/saturation and `lfx2` effects and
//! asserts the import still succeeds). Photoshop display parity of the mapped values is NOT_PROVEN
//! until the D5 oracle exists; the format facts below cite their source:
//!
//! - Adobe Photoshop File Formats Specification, "Adjustment layer" keys `levl`, `curv`, `hue2`,
//!   `brit`, `expA`, `nvrt`, `thrs`, `post`, "Layer blending ranges data", "Layer mask / adjustment
//!   layer data" (mask parameters), `knko`, `infx`, `clbl`, `tsly`.
//! - psd-tools `psd/adjustments.py` (read-only reference, MIT): `Curves` header `BHI`, `Levels` 29 x
//!   `5H`, `BrightnessContrast` `3HBx`, `Exposure` `H3f`, `HueSaturation` `HBx` + 3h + 3h + 6 x
//!   (4h + 3h) with a trailing tail Photoshop extends to 136 bytes.
//! - ag-psd `psdReader.ts` blending ranges (4 x u8 per range).
#![deny(clippy::arithmetic_side_effects)]

use hsk_studio_folio::{
    MAX_HUE_BANDS, MAX_MASK_FEATHER, StudioAdjustment, StudioAdjustmentChannel, StudioAdvancedBlending,
    StudioBlendChannel, StudioBlendRange, StudioChannelBlend, StudioCurve, StudioCurvePoint, StudioHslShift,
    StudioHueBand, StudioKnockout, StudioLevelsRecord, StudioMaskParameters,
};

use crate::descriptor;
use crate::error::Result;
use crate::header::ColorMode;
use crate::layers::{LayerMask, PsdLayer, TaggedBlock};
use crate::limits::Limits;
use crate::loss::LossKind;
use crate::reader::Reader;

/// Fixed reason code of a record that could not be mapped.
pub(crate) type Reason = &'static str;
/// A loss entry the mapping adds beside its record: kind and fixed code.
pub(crate) type Note = (LossKind, &'static str);

/// A mapped value and the loss entries that describe what it left out or assumed.
pub(crate) struct Mapped<T> {
    pub value: T,
    pub notes: Vec<Note>,
}

/// Adjustment keys this module maps. The second set (lane SMB-PS-ADJUST-STYLES): `blnc`, `selc`,
/// `mixr`, `phfl` (binary records, layouts confirmed on the psd-tools corpus files
/// `layers/color-balance.psd` 20 bytes, `selective-color.psd` 84, `channel-mixer.psd` 44,
/// `photo-filter.psd` 20) and `vibA`, `blwh` (version 16 + descriptor). `grdm` (gradient map) and
/// `clrL` (colour lookup) are not mapped yet: the layer stays a typed unsupported layer.
pub(crate) const MAPPED_ADJUSTMENT_KEYS: [&[u8; 4]; 15] = [
    b"levl", b"curv", b"hue ", b"hue2", b"brit", b"expA", b"nvrt", b"thrs", b"post", b"blnc", b"selc", b"mixr",
    b"phfl", b"vibA", b"blwh",
];

const CHANNELS: [StudioAdjustmentChannel; 4] = [
    StudioAdjustmentChannel::Composite,
    StudioAdjustmentChannel::Red,
    StudioAdjustmentChannel::Green,
    StudioAdjustmentChannel::Blue,
];
/// `levl`: 29 records of 5 u16.
const LEVELS_RECORDS: usize = 29;
const LEVELS_BYTES: usize = 290;
const LEVELS_IDENTITY: [u16; 5] = [0, 255, 0, 255, 100];
/// `curv`: points per curve (specification: 2 to 19).
const CURVE_POINTS: std::ops::RangeInclusive<usize> = 2..=19;
/// A curve takes at least a u16 count and two points.
const CURVE_MIN_BYTES: usize = 10;
/// Curves read from one block (a Folio curve set has at most four mapped channels).
const MAX_CURVES: usize = 64;
/// `hue2`: version + enable + pad + 2 x 3 i16 + 6 x (4 + 3) i16.
const HUE_BANDS: usize = 6;
const HUE_BYTES: usize = 100;

fn truncated<T>(result: Result<T>, reason: Reason) -> std::result::Result<T, Reason> {
    result.map_err(|_| reason)
}

fn mapped_channel_count(mode: ColorMode) -> usize {
    if mode == ColorMode::Rgb { CHANNELS.len() } else { 1 }
}

/// Maps the adjustment `block` of `layer`; `None` when its key is not one this module maps.
pub(crate) fn adjustment(
    layer: &PsdLayer,
    block: &TaggedBlock,
    mode: ColorMode,
    limits: &Limits,
) -> Option<std::result::Result<Mapped<StudioAdjustment>, Reason>> {
    if !MAPPED_ADJUSTMENT_KEYS.contains(&&block.key) {
        return None;
    }
    if !matches!(mode, ColorMode::Rgb | ColorMode::Grayscale) {
        return Some(Err("adjustment_color_mode_unmapped"));
    }
    let data = block.data.as_slice();
    Some(match &block.key {
        b"levl" => levels(data, mode, layer),
        b"curv" => curves(data, mode),
        b"hue " | b"hue2" => hue_saturation(data),
        b"brit" => brightness_contrast(data, layer, limits),
        b"expA" => exposure(data),
        b"thrs" => threshold(data),
        b"post" => posterize(data),
        b"blnc" => colour_balance(data),
        b"selc" => selective_colour(data),
        b"mixr" => channel_mixer(data, mode),
        b"phfl" => photo_filter(data),
        b"vibA" => vibrance(data, limits),
        b"blwh" => black_and_white(data, limits),
        _ => Ok(Mapped { value: StudioAdjustment::Invert, notes: Vec::new() }),
    })
}

// ---- second set (lane SMB-PS-ADJUST-STYLES) -------------------------------------------------------
//
// Byte layouts: Adobe Photoshop File Formats Specification "Adjustment layer" (`blnc`: three sets of
// three i16 then the preserve-luminosity byte; `selc`: version, method, ten CMYK i16 records of which the
// first is unused; `mixr`: version, monochrome, i16 rows of R, G, B, fourth channel, constant; `phfl`:
// version 2 = colour space + four u16 components, 3 = XYZ; density u32 percent; preserve-luminosity
// byte), checked against the psd-tools corpus files named on `MAPPED_ADJUSTMENT_KEYS`. Every value is
// range-checked; a value out of range refuses the record (typed loss, the layer stays unsupported).

fn i16s<const N: usize>(r: &mut Reader<'_>, reason: Reason) -> std::result::Result<[i16; N], Reason> {
    let mut out = [0i16; N];
    for v in &mut out {
        *v = truncated(r.i16("adjustment"), reason)?;
    }
    Ok(out)
}
fn in_range<const N: usize>(v: [i16; N], lo: i16, hi: i16, reason: Reason) -> std::result::Result<[f64; N], Reason> {
    if v.iter().all(|x| (lo..=hi).contains(x)) { Ok(v.map(f64::from)) } else { Err(reason) }
}

fn colour_balance(data: &[u8]) -> std::result::Result<Mapped<StudioAdjustment>, Reason> {
    const R: Reason = "colour_balance_truncated";
    let mut r = Reader::new(data);
    let mut sets = [[0f64; 3]; 3];
    for set in &mut sets {
        *set = in_range(i16s::<3>(&mut r, R)?, -100, 100, "colour_balance_range")?;
    }
    let preserve_luminosity = truncated(r.u8("blnc"), R)? != 0;
    let [shadows, midtones, highlights] = sets;
    Ok(Mapped {
        value: StudioAdjustment::ColourBalance { shadows, midtones, highlights, preserve_luminosity },
        notes: Vec::new(),
    })
}

fn selective_colour(data: &[u8]) -> std::result::Result<Mapped<StudioAdjustment>, Reason> {
    const R: Reason = "selective_colour_truncated";
    let mut r = Reader::new(data);
    if truncated(r.u16("selc"), R)? != 1 {
        return Err("selective_colour_version_unsupported");
    }
    let method = match truncated(r.u16("selc"), R)? {
        0 => hsk_studio_folio::StudioSelectiveColourMethod::Relative,
        1 => hsk_studio_folio::StudioSelectiveColourMethod::Absolute,
        _ => return Err("selective_colour_method"),
    };
    // The first record is unused by the format.
    i16s::<4>(&mut r, R)?;
    let mut q = [[0f64; 4]; 9];
    for slot in &mut q {
        *slot = in_range(i16s::<4>(&mut r, R)?, -100, 100, "selective_colour_range")?;
    }
    let [reds, yellows, greens, cyans, blues, magentas, whites, neutrals, blacks] = q;
    Ok(Mapped {
        value: StudioAdjustment::SelectiveColour {
            method,
            reds,
            yellows,
            greens,
            cyans,
            blues,
            magentas,
            whites,
            neutrals,
            blacks,
        },
        notes: Vec::new(),
    })
}

fn channel_mixer(data: &[u8], mode: ColorMode) -> std::result::Result<Mapped<StudioAdjustment>, Reason> {
    const R: Reason = "channel_mixer_truncated";
    if mode != ColorMode::Rgb {
        return Err("channel_mixer_color_mode_unmapped");
    }
    let mut r = Reader::new(data);
    if truncated(r.u16("mixr"), R)? != 1 {
        return Err("channel_mixer_version_unsupported");
    }
    let monochrome = truncated(r.u16("mixr"), R)? != 0;
    let mut notes = Vec::new();
    let mut outputs = Vec::with_capacity(3);
    for _ in 0..3 {
        let [red, green, blue, fourth, constant] = in_range(i16s::<5>(&mut r, R)?, -200, 200, "channel_mixer_range")?;
        if fourth != 0.0 {
            // The fourth input channel does not exist in an RGB document.
            notes.push((LossKind::Transformed, "channel_mixer_fourth_channel_dropped"));
        }
        outputs.push(hsk_studio_folio::StudioMixerRow { weights: vec![red, green, blue], constant });
    }
    Ok(Mapped { value: StudioAdjustment::ChannelMixer { outputs, monochrome }, notes })
}

fn photo_filter(data: &[u8]) -> std::result::Result<Mapped<StudioAdjustment>, Reason> {
    const R: Reason = "photo_filter_truncated";
    let mut r = Reader::new(data);
    match truncated(r.u16("phfl"), R)? {
        2 => {}
        3 => return Err("photo_filter_xyz_colour_unmapped"),
        _ => return Err("photo_filter_version_unsupported"),
    }
    let space = truncated(r.u16("phfl"), R)?;
    let mut c = [0u16; 4];
    for v in &mut c {
        *v = truncated(r.u16("phfl"), R)?;
    }
    if space != 0 {
        // Only the RGB colour space maps (as the effect colours do); Lab and others need a converter.
        return Err("photo_filter_colour_model_unmapped");
    }
    let density = truncated(r.u32("phfl"), R)?;
    if density > 100 {
        return Err("photo_filter_range");
    }
    let preserve_luminosity = truncated(r.u8("phfl"), R)? != 0;
    let unit = |v: u16| f64::from(v) / 65535.0;
    Ok(Mapped {
        value: StudioAdjustment::PhotoFilter {
            filter_colour: hsk_studio_folio::StudioColor {
                r: unit(c[0]),
                g: unit(c[1]),
                b: unit(c[2]),
                colour_profile_id: hsk_studio_folio::ProfileBinding::Unassigned(()),
            },
            density: f64::from(density),
            preserve_luminosity,
        },
        notes: Vec::new(),
    })
}

/// A numeric descriptor item in `lo..=hi`; absent = `default` (a Photoshop descriptor omits default items).
fn item(o: &descriptor::Object, key: &str, lo: f64, hi: f64, default: f64, reason: Reason) -> std::result::Result<f64, Reason> {
    match o.get(key) {
        None => Ok(default),
        Some(v) => v.finite_number().filter(|n| (lo..=hi).contains(n)).ok_or(reason),
    }
}

fn vibrance(data: &[u8], limits: &Limits) -> std::result::Result<Mapped<StudioAdjustment>, Reason> {
    let o = descriptor::parse_versioned(data, &[16], limits).map_err(|_| "vibrance_descriptor_unreadable")?;
    let vibrance = item(&o, "vibrance", -100.0, 100.0, 0.0, "vibrance_range")?;
    let saturation = item(&o, "Strt", -100.0, 100.0, 0.0, "vibrance_range")?;
    Ok(Mapped { value: StudioAdjustment::Vibrance { vibrance, saturation }, notes: Vec::new() })
}

fn black_and_white(data: &[u8], limits: &Limits) -> std::result::Result<Mapped<StudioAdjustment>, Reason> {
    const RANGE: Reason = "black_and_white_range";
    let o = descriptor::parse_versioned(data, &[16], limits).map_err(|_| "black_and_white_descriptor_unreadable")?;
    // Photoshop's defaults for absent weights (40, 60, 40, 60, 20, 80; the corpus file carries them).
    let w = |key: &str, default: f64| item(&o, key, -200.0, 300.0, default, RANGE);
    let (red, yellow, green, cyan, blue, magenta) =
        (w("Rd  ", 40.0)?, w("Yllw", 60.0)?, w("Grn ", 40.0)?, w("Cyn ", 60.0)?, w("Bl  ", 20.0)?, w("Mgnt", 80.0)?);
    let tint_enabled = o.get("useTint").and_then(descriptor::Value::boolean).unwrap_or(false);
    let mut notes = Vec::new();
    // The tint is stored as an RGB colour; Folio keeps hue and saturation (STU-RAS-151): the HSL hue
    // and saturation of that colour.
    let (mut tint_hue, mut tint_saturation) = (0.0, 0.0);
    if let Some(c) = o.get("tintColor").and_then(descriptor::Value::object) {
        let ch = |k: &str| c.get(k).and_then(descriptor::Value::finite_number).filter(|v| (0.0..=255.0).contains(v));
        let (Some(r), Some(g), Some(b)) = (ch("Rd  "), ch("Grn "), ch("Bl  ")) else {
            return Err("black_and_white_tint_colour_unmapped");
        };
        let [h, s] = hue_saturation_of([r / 255.0, g / 255.0, b / 255.0]);
        tint_hue = h;
        tint_saturation = s;
        notes.push((LossKind::Transformed, "black_and_white_tint_colour_as_hue_saturation"));
    }
    Ok(Mapped {
        value: StudioAdjustment::BlackAndWhite {
            red,
            yellow,
            green,
            cyan,
            blue,
            magenta,
            tint_enabled,
            tint_hue,
            tint_saturation,
        },
        notes,
    })
}

/// HSL hue (degrees `0..360`) and saturation (percent) of a colour in `[0, 1]`.
fn hue_saturation_of(c: [f64; 3]) -> [f64; 2] {
    let [r, g, b] = c;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    if d <= 0.0 {
        return [0.0, 0.0];
    }
    let l = (max + min) / 2.0;
    let s = if l <= 0.5 { d / (max + min) } else { d / (2.0 - max - min) };
    let h = if max == r {
        (g - b) / d
    } else if max == g {
        2.0 + (b - r) / d
    } else {
        4.0 + (r - g) / d
    };
    [(h * 60.0).rem_euclid(360.0), (s * 100.0).clamp(0.0, 100.0)]
}

fn levels(data: &[u8], mode: ColorMode, layer: &PsdLayer) -> std::result::Result<Mapped<StudioAdjustment>, Reason> {
    const R: Reason = "levels_truncated";
    let mut r = Reader::new(data);
    if truncated(r.u16("levl"), R)? != 2 {
        return Err("levels_version_unsupported");
    }
    if r.remaining() < LEVELS_BYTES {
        return Err(R);
    }
    let kept = mapped_channel_count(mode);
    let mut records = Vec::with_capacity(kept);
    let mut unmapped_data = false;
    for index in 0..LEVELS_RECORDS {
        let mut v = [0u16; 5];
        for slot in &mut v {
            *slot = truncated(r.u16("levl"), R)?;
        }
        let [input_floor, input_ceiling, output_floor, output_ceiling, gamma] = v;
        if let Some(channel) = CHANNELS.get(index).filter(|_| index < kept) {
            if input_floor >= input_ceiling
                || input_ceiling > 255
                || output_floor > 255
                || output_ceiling > 255
                || !(10..=999).contains(&gamma)
            {
                return Err("levels_record_invalid");
            }
            records.push(StudioLevelsRecord {
                channel: *channel,
                input_black: f64::from(input_floor) / 255.0,
                input_white: f64::from(input_ceiling) / 255.0,
                output_black: f64::from(output_floor) / 255.0,
                output_white: f64::from(output_ceiling) / 255.0,
                gamma: f64::from(gamma) / 100.0,
            });
        } else if v != LEVELS_IDENTITY {
            unmapped_data = true;
        }
    }
    let mut notes = Vec::new();
    if unmapped_data {
        notes.push((LossKind::PreservedOpaque, "levels_extra_records_preserved"));
    }
    if r.remaining() > 0 || layer.block(b"Lvls").is_some() {
        notes.push((LossKind::PreservedOpaque, "levels_extra_block_preserved"));
    }
    Ok(Mapped { value: StudioAdjustment::Levels { records }, notes })
}

fn bit_set(map: u32, bit: u32) -> bool {
    1u32.checked_shl(bit).is_some_and(|mask| map & mask != 0)
}

fn curves(data: &[u8], mode: ColorMode) -> std::result::Result<Mapped<StudioAdjustment>, Reason> {
    const R: Reason = "curves_truncated";
    let mut r = Reader::new(data);
    let is_map = truncated(r.u8("curv"), R)?;
    let version = truncated(r.u16("curv"), R)?;
    let map = truncated(r.u32("curv"), R)?;
    if is_map != 0 {
        return Err("curves_map_format_unsupported");
    }
    // Version 1 carries a channel bitmap (bit n = channel n, composite first); version 4 carries the
    // curve count and the curves follow in channel order (UNVERIFIED, recorded).
    let channel_indices: Vec<usize> = match version {
        1 => (0..32u32).filter(|bit| bit_set(map, *bit)).map(|bit| usize::try_from(bit).unwrap_or(usize::MAX)).collect(),
        4 => {
            let count = usize::try_from(map).map_err(|_| "curves_count_limit")?;
            if count > MAX_CURVES {
                return Err("curves_count_limit");
            }
            (0..count).collect()
        }
        _ => return Err("curves_version_unsupported"),
    };
    // Each curve takes at least CURVE_MIN_BYTES: refuse a count the block cannot hold before reading.
    if channel_indices.len() > r.remaining().checked_div(CURVE_MIN_BYTES).unwrap_or(0) {
        return Err(R);
    }
    let kept = mapped_channel_count(mode);
    let mut curves = Vec::new();
    let mut extra_channels = false;
    for index in channel_indices {
        let count = usize::from(truncated(r.u16("curv"), R)?);
        if !CURVE_POINTS.contains(&count) {
            return Err("curves_point_count");
        }
        let mut points: Vec<StudioCurvePoint> = Vec::with_capacity(count);
        for _ in 0..count {
            let output = truncated(r.u16("curv"), R)?;
            let input = truncated(r.u16("curv"), R)?;
            if output > 255 || input > 255 {
                return Err("curves_point_range");
            }
            if points.last().is_some_and(|p| p.input >= f64::from(input)) {
                return Err("curves_points_not_ascending");
            }
            points.push(StudioCurvePoint { input: f64::from(input), output: f64::from(output), corner: false });
        }
        match CHANNELS.get(index).filter(|_| index < kept) {
            Some(channel) => curves.push(StudioCurve { channel: *channel, points }),
            None => extra_channels = true,
        }
    }
    let mut notes = Vec::new();
    if version == 4 {
        notes.push((LossKind::Transformed, "curves_v4_channel_order_assumed"));
    }
    if extra_channels {
        notes.push((LossKind::PreservedOpaque, "curves_extra_channels_preserved"));
    }
    if r.remaining() > 0 {
        notes.push((LossKind::PreservedOpaque, "curves_extra_block_preserved"));
    }
    Ok(Mapped { value: StudioAdjustment::Curves { curves }, notes })
}

fn i16_triplet(r: &mut Reader<'_>, reason: Reason) -> std::result::Result<[i16; 3], Reason> {
    Ok([truncated(r.i16("hue2"), reason)?, truncated(r.i16("hue2"), reason)?, truncated(r.i16("hue2"), reason)?])
}

/// Hue/saturation/lightness ranges of the `hue2` record; colorization saturation is `0..=100`.
fn shift(v: [i16; 3], colorize: bool) -> std::result::Result<StudioHslShift, Reason> {
    let [hue, saturation, lightness] = v;
    let saturation_min = if colorize { 0 } else { -100 };
    if !(-180..=180).contains(&hue) || !(saturation_min..=100).contains(&saturation) || !(-100..=100).contains(&lightness)
    {
        return Err("hue_saturation_range");
    }
    Ok(StudioHslShift { hue: f64::from(hue), saturation: f64::from(saturation), lightness: f64::from(lightness) })
}

fn hue_saturation(data: &[u8]) -> std::result::Result<Mapped<StudioAdjustment>, Reason> {
    const R: Reason = "hue_saturation_truncated";
    let mut r = Reader::new(data);
    if truncated(r.u16("hue2"), R)? != 2 {
        return Err("hue_saturation_version_unsupported");
    }
    if data.len() < HUE_BYTES {
        return Err(R);
    }
    let colorize = truncated(r.u8("hue2"), R)? != 0;
    truncated(r.u8("hue2"), R)?;
    let colorization = shift(i16_triplet(&mut r, R)?, colorize)?;
    let master = shift(i16_triplet(&mut r, R)?, false)?;
    let mut bands = Vec::new();
    for _ in 0..HUE_BANDS {
        let mut range = [0.0f64; 4];
        for edge in &mut range {
            *edge = f64::from(truncated(r.i16("hue2"), R)?);
        }
        let settings = shift(i16_triplet(&mut r, R)?, false)?;
        // A band without a shift is the identity; only bands that do something are recorded.
        if settings.hue != 0.0 || settings.saturation != 0.0 || settings.lightness != 0.0 {
            bands.push(StudioHueBand { range, shift: settings });
        }
    }
    debug_assert!(bands.len() <= MAX_HUE_BANDS);
    let mut notes = Vec::new();
    if r.remaining() > 0 {
        notes.push((LossKind::PreservedOpaque, "hue_saturation_tail_preserved"));
    }
    Ok(Mapped { value: StudioAdjustment::HueSaturation { master, colorize, colorization, bands }, notes })
}

/// `brightness` and `contrast` bounds of STU-RAS-151: hard `unknown`, soft -150..150 `derived` from the
/// shipped control width. Folio's validator only requires finite values (hard bound unknown); the
/// importer holds the soft range so a value no control produces is a typed loss on the layer, never a
/// record (R7 review B; Photoshop's own stored range is UNVERIFIED, D5).
const BRIGHTNESS_CONTRAST_RANGE: std::ops::RangeInclusive<f64> = -150.0..=150.0;

/// A decoded brightness or contrast value inside [`BRIGHTNESS_CONTRAST_RANGE`]; `None` (not a finite
/// number) and out-of-range values are the same fixed refusal.
fn brightness_contrast_value(value: Option<f64>) -> std::result::Result<f64, Reason> {
    value.filter(|v| BRIGHTNESS_CONTRAST_RANGE.contains(v)).ok_or("brightness_contrast_range")
}

fn brightness_contrast(
    data: &[u8],
    layer: &PsdLayer,
    limits: &Limits,
) -> std::result::Result<Mapped<StudioAdjustment>, Reason> {
    const R: Reason = "brightness_contrast_truncated";
    let mut notes = Vec::new();
    // `CgEd` (descriptor) takes precedence over the legacy `brit` record when it can be read.
    if let Some(block) = layer.block(b"CgEd") {
        match descriptor::parse_versioned(&block.data, &[16], limits) {
            Ok(object) => {
                // A numeric item is always a candidate, whatever its value: a non-finite or out-of-range
                // number refuses the adjustment below (typed loss) and never falls back to `brit`.
                let item = |key: &str| object.get(key).filter(|v| v.is_number());
                if let (Some(brightness), Some(contrast)) = (item("Brgh"), item("Cntr")) {
                    let brightness = brightness_contrast_value(brightness.finite_number())?;
                    let contrast = brightness_contrast_value(contrast.finite_number())?;
                    let use_legacy = object.get("useLegacy").and_then(descriptor::Value::boolean);
                    if use_legacy.is_none() {
                        notes.push((LossKind::Transformed, "brightness_contrast_legacy_assumed"));
                    }
                    return Ok(Mapped {
                        value: StudioAdjustment::BrightnessContrast { brightness, contrast, use_legacy: use_legacy.unwrap_or(true) },
                        notes,
                    });
                }
                notes.push((LossKind::Transformed, "brightness_contrast_descriptor_incomplete"));
            }
            Err(_) => notes.push((LossKind::Transformed, "brightness_contrast_descriptor_unreadable")),
        }
    }
    let mut r = Reader::new(data);
    let brightness = brightness_contrast_value(Some(f64::from(truncated(r.i16("brit"), R)?)))?;
    let contrast = brightness_contrast_value(Some(f64::from(truncated(r.i16("brit"), R)?)))?;
    notes.push((LossKind::Transformed, "brightness_contrast_legacy_assumed"));
    Ok(Mapped { value: StudioAdjustment::BrightnessContrast { brightness, contrast, use_legacy: true }, notes })
}

fn exposure(data: &[u8]) -> std::result::Result<Mapped<StudioAdjustment>, Reason> {
    const R: Reason = "exposure_truncated";
    let mut r = Reader::new(data);
    if truncated(r.u16("expA"), R)? != 1 {
        return Err("exposure_version_unsupported");
    }
    let mut v = [0f32; 3];
    for slot in &mut v {
        *slot = f32::from_bits(truncated(r.u32("expA"), R)?);
    }
    let [exposure, offset, gamma] = v.map(f64::from);
    if !(exposure.is_finite() && offset.is_finite() && gamma.is_finite() && gamma > 0.0) {
        return Err("exposure_range");
    }
    Ok(Mapped {
        value: StudioAdjustment::Exposure { exposure, offset, gamma, use_legacy: false },
        notes: vec![(LossKind::Transformed, "exposure_legacy_flag_assumed_false")],
    })
}

fn threshold(data: &[u8]) -> std::result::Result<Mapped<StudioAdjustment>, Reason> {
    let level = truncated(Reader::new(data).u16("thrs"), "threshold_truncated")?;
    if level > 255 {
        return Err("threshold_range");
    }
    Ok(Mapped {
        value: StudioAdjustment::Threshold { level: f64::from(level), false_colour: None, true_colour: None },
        notes: Vec::new(),
    })
}

fn posterize(data: &[u8]) -> std::result::Result<Mapped<StudioAdjustment>, Reason> {
    let levels = truncated(Reader::new(data).u16("post"), "posterize_truncated")?;
    if !(2..=255).contains(&levels) {
        return Err("posterize_range");
    }
    Ok(Mapped { value: StudioAdjustment::Posterize { levels }, notes: Vec::new() })
}

// ---- layer record: blending ranges and the advanced-blending toggles -----------------------------

const FULL_RANGE: [u8; 4] = [0, 0, 255, 255];

fn blend_range(channel: StudioBlendChannel, source: [u8; 4], destination: [u8; 4]) -> std::result::Result<StudioBlendRange, Reason> {
    let pair = |a: u8, b: u8| -> std::result::Result<[f64; 2], Reason> {
        if a > b {
            return Err("blend_range_order");
        }
        Ok([f64::from(a), f64::from(b)])
    };
    Ok(StudioBlendRange {
        channel,
        source_black: pair(source[0], source[1])?,
        source_white: pair(source[2], source[3])?,
        destination_black: pair(destination[0], destination[1])?,
        destination_white: pair(destination[2], destination[3])?,
    })
}

/// Blend-If ranges (gray, red, green, blue), the knockout state and the three blend toggles of a
/// layer. `None` when the layer carries none of them. Source and destination ranges of the
/// composite gray come first, then one pair per channel in the layer record's channel order
/// (UNVERIFIED against a Photoshop-authored file).
pub(crate) fn advanced_blending(layer: &PsdLayer) -> std::result::Result<Option<Mapped<StudioAdvancedBlending>>, Reason> {
    let ranges = layer.blend_ranges();
    if ranges.is_none() && !layer.blending_ranges_raw.is_empty() {
        // Present but shorter than the composite gray source and destination (8 bytes).
        return Err("blend_ranges_malformed");
    }
    let knockout_raw = layer.block(b"knko").and_then(|b| b.data.first().copied());
    let interior = layer.blend_interior_elements();
    let clipped = layer.blend_clipped_elements();
    let shapes = layer.transparency_shapes_layer();
    let non_default_ranges = ranges.as_ref().is_some_and(|r| !r.is_default());
    if !non_default_ranges && knockout_raw.is_none() && interior.is_none() && clipped.is_none() && shapes.is_none() {
        return Ok(None);
    }
    let mut notes = Vec::new();
    let mut blend_ranges = Vec::new();
    if let Some(ranges) = ranges.filter(|_| non_default_ranges) {
        if (ranges.composite_source, ranges.composite_destination) != (FULL_RANGE, FULL_RANGE) {
            blend_ranges.push(blend_range(StudioBlendChannel::Gray, ranges.composite_source, ranges.composite_destination)?);
        }
        let mut extra = false;
        for (position, (source, destination)) in ranges.channels.iter().enumerate() {
            let channel = match layer.channels.get(position).map(|c| c.id) {
                Some(0) => Some(StudioBlendChannel::Red),
                Some(1) => Some(StudioBlendChannel::Green),
                Some(2) => Some(StudioBlendChannel::Blue),
                _ => None,
            };
            match channel {
                Some(channel) if (*source, *destination) != (FULL_RANGE, FULL_RANGE) => {
                    // A channel table that names one channel twice would give Folio two ranges for it,
                    // which Folio refuses (and the whole import with it): refuse the record here.
                    if blend_ranges.iter().any(|r| r.channel == channel) {
                        return Err("blend_ranges_duplicate_channel");
                    }
                    blend_ranges.push(blend_range(channel, *source, *destination)?);
                }
                Some(_) => {}
                None => extra |= (*source, *destination) != (FULL_RANGE, FULL_RANGE),
            }
        }
        if extra {
            notes.push((LossKind::PreservedOpaque, "blend_ranges_extra_channels_preserved"));
        }
    }
    // `knko` 1 = shallow, 2 = deep (psd-tools corpus transparency/knockout-shallow-*.psd and
    // knockout-deep-*.psd, bytes inspected 2026-10-15; lane SMB-PS-ADJUST-STYLES).
    let (knockout, knockout_depth) = match knockout_raw {
        None | Some(0) => (StudioKnockout::Disabled, None),
        Some(1) => (StudioKnockout::Enabled, Some(hsk_studio_folio::StudioKnockoutDepth::Shallow)),
        Some(2) => (StudioKnockout::Enabled, Some(hsk_studio_folio::StudioKnockoutDepth::Deep)),
        Some(_) => {
            notes.push((LossKind::Transformed, "knockout_value_unknown"));
            (StudioKnockout::Unknown, None)
        }
    };
    if knockout_raw.is_none() || interior.is_none() || clipped.is_none() || shapes.is_none() {
        // Photoshop's documented defaults, not stated by the Studio spec (UNVERIFIED, D5).
        notes.push((LossKind::Transformed, "advanced_blending_defaults_assumed"));
    }
    notes.push((LossKind::Transformed, "channel_blend_assumed_all_enabled"));
    Ok(Some(Mapped {
        value: StudioAdvancedBlending {
            blend_ranges,
            knockout,
            channel_blend: StudioChannelBlend { red: true, green: true, blue: true },
            blend_interior_effects: interior.unwrap_or(false),
            blend_clipped_effects: clipped.unwrap_or(true),
            transparency_shapes_layer: shapes.unwrap_or(true),
            knockout_depth,
        },
        notes,
    }))
}

// ---- layer mask parameters --------------------------------------------------------------------------

/// User mask density and feather (mask flags bit 4) and the invert state of the mask in use
/// (`selected_flags` bit 2). `None` when the mask has neither. The parameters sit after the first 18
/// bytes (specification order) or after the real-mask fields (Photoshop order, offset 36), the same
/// choice `LayerMask` makes for the real-mask rectangle; vector-mask parameters are preserved only.
pub(crate) fn mask_parameters(
    layer: &PsdLayer,
    mask: &LayerMask,
    selected_flags: u8,
) -> std::result::Result<Option<Mapped<StudioMaskParameters>>, Reason> {
    const R: Reason = "mask_parameters_truncated";
    let inverted = selected_flags & 0x04 != 0;
    let mut notes = Vec::new();
    let (mut density, mut feather) = (100.0, 0.0);
    if mask.flags & 0x10 != 0 {
        let at = match mask.real_flags_offset {
            Some(18) => 36,
            _ => 18,
        };
        let mut r = Reader::with_order(layer.mask_raw.get(at..).ok_or(R)?, layer.little_endian);
        let present = truncated(r.u8("mask"), R)?;
        if present & 0xF0 != 0 {
            notes.push((LossKind::PreservedOpaque, "mask_parameters_flags_unknown"));
        }
        if present & 1 != 0 {
            density = f64::from(truncated(r.u8("mask"), R)?) / 255.0 * 100.0;
        }
        if present & 2 != 0 {
            feather = f64::from_bits(truncated(r.u64("mask"), R)?);
            if !feather.is_finite() || !(0.0..=MAX_MASK_FEATHER).contains(&feather) {
                return Err("mask_feather_range");
            }
        }
        if present & 0x0C != 0 {
            notes.push((LossKind::PreservedOpaque, "mask_vector_parameters_preserved"));
        }
    } else if !inverted {
        return Ok(None);
    }
    Ok(Some(Mapped { value: StudioMaskParameters { density, feather, inverted }, notes }))
}
