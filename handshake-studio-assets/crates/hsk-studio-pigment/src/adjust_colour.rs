//! Per-pixel maths of the second STU-RAS-151 adjustment set (lane SMB-PS-ADJUST-STYLES): vibrance,
//! colour balance, photo filter, black & white, channel mixer, selective colour, gradient map,
//! colour lookup, shadows/highlights (global form), tone compression (HDR toning, global methods),
//! match colour and replace colour. [`crate::adjust_math`] stays the single owner of the
//! adjustment-layer rule and of [`crate::adjust_math::AdjustmentOp`]; this module holds the
//! parameter records and the colour functions those variants dispatch to (CX-CAP-001: one owner per
//! formula, the variants are not duplicated anywhere else).
//!
//! Same scope as `adjust_math`: STRAIGHT colours in `[0, 1]` in the plan's blend space, no profile
//! conversion (STU-COL-163); parameters on the STU-RAS-151 canonical scales (percent where the spec
//! says percent, degrees for hues). Every non-finite or out-of-range parameter is a typed refusal
//! (`AdjustError::InvalidParameter`), checked in [`validate`] BEFORE any table is read; the only
//! allocation-sized inputs (gradient-map stops, the colour-lookup cube, replace-colour samples) are
//! bounded by [`MAX_GRADIENT_MAP_STOPS`], [`MAX_LUT_SIDE`] and [`MAX_REPLACE_SAMPLES`]. Adobe does
//! not publish these formulas: every kind is class `Approximate` and Photoshop parity is
//! UNVERIFIED (D5 oracle list). Each formula below states its basis.
#![deny(clippy::arithmetic_side_effects)]
use crate::adjust_math::{AdjustError, HueSaturation, hsl_to_rgb, rgb_to_hsl};
use crate::blend_math::{lum, set_lum};
use serde::Serialize;

/// Gradient-map stops per gradient (a Pigment hard ceiling; the spec's `StudioGradient` declares no
/// stop count).
pub const MAX_GRADIENT_MAP_STOPS: usize = 256;
/// Largest colour-lookup cube side (65 is the largest common 3D-LUT grid; 65^3 = 274 625 entries,
/// 3.3 MB of f32 triples). A larger side is refused before the table is read.
pub const MAX_LUT_SIDE: u16 = 65;
/// Sampled colours of one replace-colour adjustment.
pub const MAX_REPLACE_SAMPLES: usize = 64;

fn unit(v: f32) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}
fn within(v: f32, lo: f32, hi: f32) -> bool {
    v.is_finite() && (lo..=hi).contains(&v)
}
fn param(ok: bool, name: &'static str) -> Result<(), AdjustError> {
    if ok {
        Ok(())
    } else {
        Err(AdjustError::InvalidParameter(name))
    }
}
fn clamp3(c: [f32; 3]) -> [f32; 3] {
    c.map(|v| v.clamp(0.0, 1.0))
}

/// Vibrance (STU-RAS-151 `vibrance`): `vibrance` and `saturation` in percent `[-100, 100]`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Vibrance {
    pub vibrance: f32,
    pub saturation: f32,
}

/// Colour balance (STU-RAS-151 `colour_balance`): per tonal range the triple `[cyan_red,
/// magenta_green, yellow_blue]` in `[-100, 100]`, plus `preserve_luminosity`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ColourBalance {
    pub shadows: [f32; 3],
    pub midtones: [f32; 3],
    pub highlights: [f32; 3],
    pub preserve_luminosity: bool,
}

/// Photo filter (STU-RAS-151 `photo_filter`): filter colour (straight RGB `[0, 1]`), density percent
/// `[0, 100]`, `preserve_luminosity`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct PhotoFilter {
    pub colour: [f32; 3],
    pub density: f32,
    pub preserve_luminosity: bool,
}

/// Black & white (STU-RAS-151 `black_and_white`): six weights in percent `[-200, 300]` in the order
/// red, yellow, green, cyan, blue, magenta; optional tint `[hue degrees 0..=360, saturation percent
/// 0..=100]`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct BlackAndWhite {
    pub weights: [f32; 6],
    pub tint: Option<[f32; 2]>,
}

/// Channel mixer (STU-RAS-151 `channel_mixer`): per output channel `[from red, from green, from
/// blue, constant]` in percent `[-200, 200]`; `monochrome` uses the red row for all three outputs.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ChannelMixer {
    pub red: [f32; 4],
    pub green: [f32; 4],
    pub blue: [f32; 4],
    pub monochrome: bool,
}

/// Selective colour method (STU-RAS-151 `method`: `relative = 1 | absolute = 2`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum SelectiveColourMethod {
    Relative,
    Absolute,
}

/// Selective colour (STU-RAS-151 `selective_colour`): nine ranges in the spec order reds, yellows,
/// greens, cyans, blues, magentas, whites, neutrals, blacks, each a `[c, m, y, k]` quadruple in
/// percent `[-100, 100]`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct SelectiveColour {
    pub method: SelectiveColourMethod,
    pub ranges: [[f32; 4]; 9],
}

/// One gradient-map stop: position `[0, 1]`, straight RGB `[0, 1]`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct GradientMapStop {
    pub position: f32,
    pub colour: [f32; 3],
}

/// Gradient map (STU-RAS-151 `gradient_map`): 2..=[`MAX_GRADIENT_MAP_STOPS`] stops with
/// non-decreasing positions, `reverse`. Interpolation is linear between stops (the gradient's
/// smoothness setting is not part of this op).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GradientMap {
    pub stops: Vec<GradientMapStop>,
    pub reverse: bool,
}

/// Colour lookup (STU-RAS-151 `colour_lookup`, kind `3dlut`): a resolved cube of `side^3` straight
/// RGB entries, red fastest (`index = r + side (g + side b)`, the .cube file order), side
/// `2..=`[`MAX_LUT_SIDE`]. Trilinear interpolation.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ColourLookup {
    pub side: u16,
    pub table: Vec<[f32; 3]>,
}

/// Shadows/highlights (STU-RAS-151 `shadows_highlights`, the ten-field shape). Amounts and tone
/// widths percent `[0, 100]`, radii pixels, colour correction and midtone contrast `[-100, 100]`,
/// clips percent `[0, 50)`. Only the GLOBAL form (both radii 0) is defined here: a radius above 0
/// needs a neighbourhood and is refused (`Unsupported("shadows_highlights.radius")`). The clips are
/// the histogram-stretch clip percentages of the local operator and are validated but not read.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ShadowsHighlights {
    pub shadow_amount: f32,
    pub shadow_tone_width: f32,
    pub shadow_radius: f32,
    pub highlight_amount: f32,
    pub highlight_tone_width: f32,
    pub highlight_radius: f32,
    pub colour_correction: f32,
    pub midtone_contrast: f32,
    pub black_clip: f32,
    pub white_clip: f32,
}

/// Tone-compression method (STU-RAS-151 `tone_compression`, the older "HDR toning").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ToneCompressionMethod {
    /// Global: `v = clamp(x 2^exposure)^(1 / gamma)`.
    ExposureGamma,
    /// Global: `v = x / (1 + x)` after the exposure gain (Reinhard 2002 global operator).
    HighlightCompression,
    /// Local (refused: needs a neighbourhood).
    LocalAdaptation,
    /// Histogram-based (refused: needs the image histogram).
    EqualizeHistogram,
}

/// Tone compression: method, exposure in stops `[-20, 20]`, gamma `[0.01, 9.99]`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ToneCompression {
    pub method: ToneCompressionMethod,
    pub exposure: f32,
    pub gamma: f32,
}

/// Match colour (STU-RAS-151 `match_colour`): `luminance` and `colour_intensity` percent `[1, 200]`
/// (100 = unchanged), `fade` percent `[0, 100]`, `neutralize`, and the resolved source selection as
/// per-channel statistics (mean `[0, 1]`, standard deviation `[0, 1]`) of the source and of the
/// target. The caller computes the statistics from the source selection; this op is per pixel.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct MatchColour {
    pub luminance: f32,
    pub colour_intensity: f32,
    pub fade: f32,
    pub neutralize: bool,
    pub source_mean: [f32; 3],
    pub source_std: [f32; 3],
    pub target_mean: [f32; 3],
    pub target_std: [f32; 3],
}

/// Replace colour (STU-RAS-151 `replace_colour`): sampled colours (1..=[`MAX_REPLACE_SAMPLES`],
/// straight RGB), fuzziness `[0, 200]` on the 0..255 scale, and the hue (degrees `[-180, 180]`),
/// saturation and lightness (percent `[-100, 100]`) shift.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReplaceColour {
    pub samples: Vec<[f32; 3]>,
    pub fuzziness: f32,
    pub hue: f32,
    pub saturation: f32,
    pub lightness: f32,
}

/// The second-set ops, one variant per STU-RAS-151 kind. Carried by
/// [`crate::adjust_math::AdjustmentOp`]'s variants of the same names.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ColourOp<'a> {
    Vibrance(&'a Vibrance),
    ColourBalance(&'a ColourBalance),
    PhotoFilter(&'a PhotoFilter),
    BlackAndWhite(&'a BlackAndWhite),
    ChannelMixer(&'a ChannelMixer),
    SelectiveColour(&'a SelectiveColour),
    GradientMap(&'a GradientMap),
    ColourLookup(&'a ColourLookup),
    ShadowsHighlights(&'a ShadowsHighlights),
    ToneCompression(&'a ToneCompression),
    MatchColour(&'a MatchColour),
    ReplaceColour(&'a ReplaceColour),
}

/// Number of entries of a cube of `side` (checked; `None` on overflow).
pub fn lut_entries(side: u16) -> Option<usize> {
    let s = usize::from(side);
    s.checked_mul(s)?.checked_mul(s)
}

/// Parameter validation of a second-set op. Bounds first, table contents after the size check.
pub(crate) fn validate(op: &ColourOp<'_>) -> Result<(), AdjustError> {
    match op {
        ColourOp::Vibrance(v) => {
            param(within(v.vibrance, -100.0, 100.0), "vibrance.vibrance")?;
            param(within(v.saturation, -100.0, 100.0), "vibrance.saturation")
        }
        ColourOp::ColourBalance(b) => param(
            [b.shadows, b.midtones, b.highlights]
                .iter()
                .flatten()
                .all(|v| within(*v, -100.0, 100.0)),
            "colour_balance.values",
        ),
        ColourOp::PhotoFilter(p) => {
            param(p.colour.iter().all(|v| unit(*v)), "photo_filter.colour")?;
            param(within(p.density, 0.0, 100.0), "photo_filter.density")
        }
        ColourOp::BlackAndWhite(b) => {
            param(
                b.weights.iter().all(|v| within(*v, -200.0, 300.0)),
                "black_and_white.weights",
            )?;
            if let Some([h, s]) = b.tint {
                param(within(h, 0.0, 360.0), "black_and_white.tint_hue")?;
                param(within(s, 0.0, 100.0), "black_and_white.tint_saturation")?;
            }
            Ok(())
        }
        ColourOp::ChannelMixer(m) => param(
            [m.red, m.green, m.blue]
                .iter()
                .flatten()
                .all(|v| within(*v, -200.0, 200.0)),
            "channel_mixer.values",
        ),
        ColourOp::SelectiveColour(s) => param(
            s.ranges.iter().flatten().all(|v| within(*v, -100.0, 100.0)),
            "selective_colour.ranges",
        ),
        ColourOp::GradientMap(g) => {
            param(
                (2..=MAX_GRADIENT_MAP_STOPS).contains(&g.stops.len()),
                "gradient_map.stop_count",
            )?;
            param(
                g.stops
                    .iter()
                    .all(|s| unit(s.position) && s.colour.iter().all(|c| unit(*c)))
                    && g.stops.windows(2).all(|w| match w {
                        [a, b] => a.position <= b.position,
                        _ => true,
                    }),
                "gradient_map.stops",
            )
        }
        ColourOp::ColourLookup(l) => {
            // Side first, so a hostile side is refused before the table is inspected.
            param(
                (2..=MAX_LUT_SIDE).contains(&l.side),
                "colour_lookup.side",
            )?;
            param(
                lut_entries(l.side) == Some(l.table.len()),
                "colour_lookup.table_length",
            )?;
            param(
                l.table.iter().flatten().all(|v| unit(*v)),
                "colour_lookup.table",
            )
        }
        ColourOp::ShadowsHighlights(s) => {
            for (v, name) in [
                (s.shadow_amount, "shadows_highlights.shadow_amount"),
                (s.shadow_tone_width, "shadows_highlights.shadow_tone_width"),
                (s.highlight_amount, "shadows_highlights.highlight_amount"),
                (
                    s.highlight_tone_width,
                    "shadows_highlights.highlight_tone_width",
                ),
            ] {
                param(within(v, 0.0, 100.0), name)?;
            }
            for (v, name) in [
                (s.colour_correction, "shadows_highlights.colour_correction"),
                (s.midtone_contrast, "shadows_highlights.midtone_contrast"),
            ] {
                param(within(v, -100.0, 100.0), name)?;
            }
            for (v, name) in [
                (s.black_clip, "shadows_highlights.black_clip"),
                (s.white_clip, "shadows_highlights.white_clip"),
            ] {
                param(v.is_finite() && (0.0..50.0).contains(&v), name)?;
            }
            param(
                s.shadow_radius.is_finite()
                    && s.shadow_radius >= 0.0
                    && s.highlight_radius.is_finite()
                    && s.highlight_radius >= 0.0,
                "shadows_highlights.radius",
            )?;
            if s.shadow_radius > 0.0 || s.highlight_radius > 0.0 {
                return Err(AdjustError::Unsupported("shadows_highlights.radius"));
            }
            Ok(())
        }
        ColourOp::ToneCompression(t) => {
            param(within(t.exposure, -20.0, 20.0), "tone_compression.exposure")?;
            param(within(t.gamma, 0.01, 9.99), "tone_compression.gamma")?;
            match t.method {
                ToneCompressionMethod::ExposureGamma
                | ToneCompressionMethod::HighlightCompression => Ok(()),
                ToneCompressionMethod::LocalAdaptation => Err(AdjustError::Unsupported(
                    "tone_compression.local_adaptation",
                )),
                ToneCompressionMethod::EqualizeHistogram => Err(AdjustError::Unsupported(
                    "tone_compression.equalize_histogram",
                )),
            }
        }
        ColourOp::MatchColour(m) => {
            param(within(m.luminance, 1.0, 200.0), "match_colour.luminance")?;
            param(
                within(m.colour_intensity, 1.0, 200.0),
                "match_colour.colour_intensity",
            )?;
            param(within(m.fade, 0.0, 100.0), "match_colour.fade")?;
            param(
                [m.source_mean, m.source_std, m.target_mean, m.target_std]
                    .iter()
                    .flatten()
                    .all(|v| unit(*v)),
                "match_colour.statistics",
            )
        }
        ColourOp::ReplaceColour(r) => {
            param(
                (1..=MAX_REPLACE_SAMPLES).contains(&r.samples.len())
                    && r.samples.iter().flatten().all(|v| unit(*v)),
                "replace_colour.samples",
            )?;
            param(within(r.fuzziness, 0.0, 200.0), "replace_colour.fuzziness")?;
            param(within(r.hue, -180.0, 180.0), "replace_colour.hue")?;
            param(
                within(r.saturation, -100.0, 100.0),
                "replace_colour.saturation",
            )?;
            param(
                within(r.lightness, -100.0, 100.0),
                "replace_colour.lightness",
            )
        }
    }
}

/// Apply a validated second-set op to one straight colour in `[0, 1]`; the result is clamped to
/// `[0, 1]` per channel.
pub(crate) fn apply(op: &ColourOp<'_>, c: [f32; 3]) -> Result<[f32; 3], AdjustError> {
    Ok(clamp3(match op {
        ColourOp::Vibrance(v) => vibrance(v, c),
        ColourOp::ColourBalance(b) => colour_balance(b, c),
        ColourOp::PhotoFilter(p) => photo_filter(p, c),
        ColourOp::BlackAndWhite(b) => black_and_white(b, c),
        ColourOp::ChannelMixer(m) => channel_mixer(m, c),
        ColourOp::SelectiveColour(s) => selective_colour(s, c),
        ColourOp::GradientMap(g) => gradient_map(g, c),
        ColourOp::ColourLookup(l) => colour_lookup(l, c)?,
        ColourOp::ShadowsHighlights(s) => shadows_highlights(s, c),
        ColourOp::ToneCompression(t) => tone_compression(t, c),
        ColourOp::MatchColour(m) => match_colour(m, c),
        ColourOp::ReplaceColour(r) => replace_colour(r, c)?,
    }))
}

/// `L + (c - L) k` per channel with `L = Lum(c)` (W3C Compositing 1 luminosity weights).
fn scale_chroma(c: [f32; 3], l: f32, k: f32) -> [f32; 3] {
    c.map(|v| l + (v - l) * k)
}

/// Vibrance: with `L = Lum(c)` and chroma `s = max - min`, first `c1 = L + (c - L)(1 + v (1 - s))`
/// (vibrance acts more on weakly saturated colours), then `c2 = L + (c1 - L)(1 + t)`, with `v`
/// and `t` the fractions. Studio formula (Photoshop's vibrance, including its skin-tone
/// protection, is unpublished): Approximate, UNVERIFIED (D5).
fn vibrance(p: &Vibrance, c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let s = crate::blend_math::sat(c);
    let v = p.vibrance / 100.0;
    let t = p.saturation / 100.0;
    let c1 = scale_chroma(c, l, 1.0 + v * (1.0 - s));
    scale_chroma(c1, l, 1.0 + t)
}

/// Colour balance. Per channel `x + (S w_s + M w_m + H w_h) / 100` with the HSL lightness `l` of
/// the pixel and `a = 0.25`, `b = 0.333`, `scale = 0.7`: `w_s = clamp((l - b) / -a + 0.5) scale`,
/// `w_m = clamp((l - b) / a + 0.5) clamp((l + b - 1) / -a + 0.5) scale`, `w_h = clamp((l + b - 1)
/// / a + 0.5) scale`; with `preserve_luminosity` the result's HSL lightness is reset to `l`.
/// Behaviour reference: GIMP `gimpoperationcolorbalance.c` transfer weights (GPL; formula
/// re-derived, no code copied). Photoshop's transfer is unpublished: Approximate, UNVERIFIED (D5).
fn colour_balance(p: &ColourBalance, c: [f32; 3]) -> [f32; 3] {
    let hsl = rgb_to_hsl(c);
    let l = hsl[2] as f32;
    let (a, b, scale) = (0.25f32, 0.333f32, 0.7f32);
    let ws = ((l - b) / -a + 0.5).clamp(0.0, 1.0) * scale;
    let wm = ((l - b) / a + 0.5).clamp(0.0, 1.0)
        * ((l + b - 1.0) / -a + 0.5).clamp(0.0, 1.0)
        * scale;
    let wh = ((l + b - 1.0) / a + 0.5).clamp(0.0, 1.0) * scale;
    let out = [0usize, 1, 2].map(|i| {
        let shift = p.shadows.get(i).copied().unwrap_or(0.0) * ws
            + p.midtones.get(i).copied().unwrap_or(0.0) * wm
            + p.highlights.get(i).copied().unwrap_or(0.0) * wh;
        (c.get(i).copied().unwrap_or(0.0) + shift / 100.0).clamp(0.0, 1.0)
    });
    if p.preserve_luminosity {
        let mut h = rgb_to_hsl(out);
        h[2] = hsl[2];
        hsl_to_rgb(h)
    } else {
        out
    }
}

/// Photo filter: `c1 = c + (c f - c) d` (a multiply of the filter colour faded by the density
/// fraction `d`), then with `preserve_luminosity` `SetLum(c1, Lum(c))` (W3C Compositing 1 s10.2).
/// Adobe describes the filter as a coloured lens; the multiply model is Studio's reading:
/// Approximate, UNVERIFIED (D5).
fn photo_filter(p: &PhotoFilter, c: [f32; 3]) -> [f32; 3] {
    let d = p.density / 100.0;
    let c1 = [0usize, 1, 2].map(|i| {
        let x = c.get(i).copied().unwrap_or(0.0);
        let f = p.colour.get(i).copied().unwrap_or(0.0);
        x + (x * f - x) * d
    });
    if p.preserve_luminosity {
        set_lum(c1, lum(c))
    } else {
        c1
    }
}

/// Black & white: with the channels sorted `max >= mid >= min` (ties keep red, green, blue order),
/// `gray = min + (max - mid) w_p + (mid - min) w_s`, where `w_p` is the weight of the primary of
/// the max channel (red, green or blue) and `w_s` the weight of the secondary mixed from the max
/// and mid channels (red+green = yellow, green+blue = cyan, red+blue = magenta), as fractions. With
/// a tint the output is `HSL(hue, saturation, gray)`. Studio reading of the six-weight shape
/// (STU-RAS-151); Photoshop's exact mix and tint model are unpublished: Approximate, UNVERIFIED (D5).
fn black_and_white(p: &BlackAndWhite, c: [f32; 3]) -> [f32; 3] {
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|a, b| {
        let va = c.get(*a).copied().unwrap_or(0.0);
        let vb = c.get(*b).copied().unwrap_or(0.0);
        vb.total_cmp(&va)
    });
    let [imax, imid, imin] = idx;
    let at = |i: usize| c.get(i).copied().unwrap_or(0.0);
    let w = |k: usize| p.weights.get(k).copied().unwrap_or(0.0) / 100.0;
    // Weight slots: 0 red, 1 yellow, 2 green, 3 cyan, 4 blue, 5 magenta.
    let primary = match imax {
        0 => w(0),
        1 => w(2),
        _ => w(4),
    };
    let secondary = match (imax.min(imid), imax.max(imid)) {
        (0, 1) => w(1),
        (1, 2) => w(3),
        _ => w(5),
    };
    let gray =
        (at(imin) + (at(imax) - at(imid)) * primary + (at(imid) - at(imin)) * secondary)
            .clamp(0.0, 1.0);
    match p.tint {
        Some([h, s]) => hsl_to_rgb([
            (f64::from(h) / 360.0).rem_euclid(1.0),
            f64::from(s) / 100.0,
            f64::from(gray),
        ]),
        None => [gray; 3],
    }
}

/// Channel mixer: `out_i = (w_r R + w_g G + w_b B + k) / 100` with the row of output `i` (percent);
/// monochrome uses the red row for all three. The constant is a percentage of full scale (Studio
/// reading; Photoshop's constant scale UNVERIFIED, D5). The weighted sum itself is the standard
/// channel-mixer definition.
fn channel_mixer(p: &ChannelMixer, c: [f32; 3]) -> [f32; 3] {
    let row = |r: &[f32; 4]| (r[0] * c[0] + r[1] * c[1] + r[2] * c[2] + r[3]) / 100.0;
    if p.monochrome {
        let v = row(&p.red);
        [v, v, v]
    } else {
        [row(&p.red), row(&p.green), row(&p.blue)]
    }
}

/// Selective colour. Membership amount of each range for the pixel (`max >= mid >= min`): reds
/// `max - mid` when red is the max, greens and blues likewise; yellows `mid - min` when blue is the
/// min, cyans when red is the min, magentas when green is the min; whites `2 (min - 0.5)` when
/// `min > 0.5`; blacks `2 (0.5 - max)` when `max < 0.5`; neutrals `1 - (|max - 0.5| + |min - 0.5|)`
/// unless the pixel is pure black or pure white. Each channel's ink is `1 - x` (red carries cyan,
/// green magenta, blue yellow); a range adds `(a + k) ink` (relative) or `a + k` (absolute) of ink,
/// with `a` the range's value for that ink and `k` its black, as fractions, scaled by the
/// membership amount; `x' = x - sum`. Relative/absolute follow the Adobe help example (50 %
/// magenta + 10 % gives 55 % relative, 60 % absolute); the range memberships are a Studio reading.
/// Approximate, UNVERIFIED (D5).
fn selective_colour(p: &SelectiveColour, c: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = c;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let mid = r + g + b - max - min;
    let mut amount = [0.0f32; 9];
    let hi = max - mid;
    let lo = mid - min;
    if r == max {
        amount[0] = hi;
    }
    if b == min {
        amount[1] = lo;
    }
    if g == max {
        amount[2] = hi;
    }
    if r == min {
        amount[3] = lo;
    }
    if b == max {
        amount[4] = hi;
    }
    if g == min {
        amount[5] = lo;
    }
    if min > 0.5 {
        amount[6] = (min - 0.5) * 2.0;
    }
    if max > 0.0 && min < 1.0 {
        amount[7] = 1.0 - ((max - 0.5).abs() + (min - 0.5).abs());
    }
    if max < 0.5 {
        amount[8] = (0.5 - max) * 2.0;
    }
    [0usize, 1, 2].map(|i| {
        let x = c.get(i).copied().unwrap_or(0.0);
        let ink = 1.0 - x;
        let mut add = 0.0f32;
        for (range, m) in p.ranges.iter().zip(amount) {
            if m <= 0.0 {
                continue;
            }
            let a = range.get(i).copied().unwrap_or(0.0) / 100.0;
            let k = range[3] / 100.0;
            let d = match p.method {
                SelectiveColourMethod::Relative => (a + k) * ink,
                SelectiveColourMethod::Absolute => a + k,
            };
            add += d * m;
        }
        x - add
    })
}

/// Gradient map: `t = Lum(c)` (W3C weights), `1 - t` when reversed, then the colour of the gradient
/// at `t`: the first stop's colour at or below its position, the last stop's at or above its
/// position, otherwise the linear interpolation between the enclosing stops (a zero-width segment
/// takes the later stop). Photoshop's luminance metric and gradient smoothness are UNVERIFIED (D5).
fn gradient_map(p: &GradientMap, c: [f32; 3]) -> [f32; 3] {
    let mut t = lum(c).clamp(0.0, 1.0);
    if p.reverse {
        t = 1.0 - t;
    }
    let (Some(first), Some(last)) = (p.stops.first(), p.stops.last()) else {
        return c;
    };
    if t <= first.position {
        return first.colour;
    }
    if t >= last.position {
        return last.colour;
    }
    for w in p.stops.windows(2) {
        let [a, b] = w else { continue };
        if t >= a.position && t < b.position {
            let span = b.position - a.position;
            if span <= 0.0 {
                return b.colour;
            }
            let f = (t - a.position) / span;
            return [0usize, 1, 2].map(|i| {
                let ca = a.colour.get(i).copied().unwrap_or(0.0);
                let cb = b.colour.get(i).copied().unwrap_or(0.0);
                ca + (cb - ca) * f
            });
        }
    }
    last.colour
}

/// Colour lookup: trilinear interpolation of the cube at `c (side - 1)` (red fastest). The
/// interpolation scheme of Photoshop's Color Lookup is UNVERIFIED (D5); trilinear is the .cube
/// reference reading.
fn colour_lookup(p: &ColourLookup, c: [f32; 3]) -> Result<[f32; 3], AdjustError> {
    let n = usize::from(p.side);
    let last = n.checked_sub(1).ok_or(AdjustError::InvalidParameter("colour_lookup.side"))?;
    let lastf = f32::from(p.side) - 1.0;
    let mut i0 = [0usize; 3];
    let mut i1 = [0usize; 3];
    let mut f = [0.0f32; 3];
    for (((v, lo_slot), hi_slot), frac) in c.iter().zip(&mut i0).zip(&mut i1).zip(&mut f) {
        let x = v * lastf;
        let fl = x.floor().clamp(0.0, lastf);
        // fl is an integer in [0, side - 1], side <= 65.
        let lo = fl as usize;
        *lo_slot = lo.min(last);
        *hi_slot = lo.saturating_add(1).min(last);
        *frac = x - fl;
    }
    let at = |r: usize, g: usize, b: usize| -> Result<[f32; 3], AdjustError> {
        let idx = g
            .checked_mul(n)
            .and_then(|v| b.checked_mul(n)?.checked_mul(n)?.checked_add(v))
            .and_then(|v| v.checked_add(r))
            .ok_or(AdjustError::InvalidParameter("colour_lookup.table_length"))?;
        p.table
            .get(idx)
            .copied()
            .ok_or(AdjustError::InvalidParameter("colour_lookup.table_length"))
    };
    let lerp = |a: [f32; 3], b: [f32; 3], t: f32| -> [f32; 3] {
        [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
    };
    let [r0, g0, b0] = i0;
    let [r1, g1, b1] = i1;
    let c00 = lerp(at(r0, g0, b0)?, at(r1, g0, b0)?, f[0]);
    let c10 = lerp(at(r0, g1, b0)?, at(r1, g1, b0)?, f[0]);
    let c01 = lerp(at(r0, g0, b1)?, at(r1, g0, b1)?, f[0]);
    let c11 = lerp(at(r0, g1, b1)?, at(r1, g1, b1)?, f[0]);
    let c0 = lerp(c00, c10, f[1]);
    let c1 = lerp(c01, c11, f[1]);
    Ok(lerp(c0, c1, f[2]))
}

/// Shadows/highlights, global form. With `L = Lum(c)`, shadow weight `ws = (1 - L / tw_s)^2` for
/// `L < tw_s` (else 0) and highlight weight `wh = (1 - (1 - L) / tw_h)^2` for `1 - L < tw_h`
/// (tone widths as fractions; width 0 = no weight); `L1 = L + sa ws (1 - L) - ha wh L` (amounts as
/// fractions), `L2 = 0.5 + (L1 - 0.5)(1 + mc / 100)` clamped; the colour moves to `SetLum(c, L2)`
/// and its chroma is scaled by `1 + cc / 100 (ws + wh)` about `L2`. Studio definition: Photoshop's
/// operator is local (radius) and unpublished; this is not a parity claim (UNVERIFIED, D5).
fn shadows_highlights(p: &ShadowsHighlights, c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let weight = |d: f32, width: f32| {
        let tw = width / 100.0;
        if tw > 0.0 && d < tw {
            let t = 1.0 - d / tw;
            t * t
        } else {
            0.0
        }
    };
    let ws = weight(l, p.shadow_tone_width);
    let wh = weight(1.0 - l, p.highlight_tone_width);
    let l1 = l + p.shadow_amount / 100.0 * ws * (1.0 - l) - p.highlight_amount / 100.0 * wh * l;
    let l2 = (0.5 + (l1 - 0.5) * (1.0 + p.midtone_contrast / 100.0)).clamp(0.0, 1.0);
    let moved = set_lum(c, l2);
    scale_chroma(moved, lum(moved), 1.0 + p.colour_correction / 100.0 * (ws + wh))
}

/// Tone compression, global methods. Exposure and gamma: `v = clamp(x 2^e)^(1 / gamma)`. Highlight
/// compression: `y = x 2^e`, `v = (y / (1 + y))^(1 / gamma)` (Reinhard et al. 2002 global operator,
/// on encoded values). Photoshop's HDR toning maths are unpublished: Approximate, UNVERIFIED (D5).
fn tone_compression(p: &ToneCompression, c: [f32; 3]) -> [f32; 3] {
    let gain = 2f32.powf(p.exposure);
    c.map(|x| {
        let y = x * gain;
        let v = match p.method {
            ToneCompressionMethod::HighlightCompression => y / (1.0 + y),
            _ => y.clamp(0.0, 1.0),
        };
        if p.gamma != 1.0 && v > 0.0 {
            v.powf(1.0 / p.gamma)
        } else {
            v
        }
    })
}

/// Match colour: per channel statistics transfer `y = (x - mt) st' / tt + ms` (Reinhard et al.
/// 2001 transfer, here per RGB channel rather than in l-alpha-beta; `st' = ss`, a target deviation
/// of 0 keeps the scale 1), where `neutralize` replaces the source means by their luminance (a
/// neutral cast); then colour intensity `y = L + (y - L) ci / 100` (`L = Lum(y)`), luminance `y =
/// y lu / 100`, and fade `out = y + (x - y) fade / 100`. Studio reading of the four controls;
/// Photoshop's colour space and statistics are UNVERIFIED (D5).
fn match_colour(p: &MatchColour, c: [f32; 3]) -> [f32; 3] {
    let source_mean = if p.neutralize {
        [lum(p.source_mean); 3]
    } else {
        p.source_mean
    };
    let y = [0usize, 1, 2].map(|i| {
        let x = c.get(i).copied().unwrap_or(0.0);
        let mt = p.target_mean.get(i).copied().unwrap_or(0.0);
        let tt = p.target_std.get(i).copied().unwrap_or(0.0);
        let ss = p.source_std.get(i).copied().unwrap_or(0.0);
        let ms = source_mean.get(i).copied().unwrap_or(0.0);
        let k = if tt > 0.0 { ss / tt } else { 1.0 };
        (x - mt) * k + ms
    });
    let l = lum(y);
    let y = scale_chroma(y, l, p.colour_intensity / 100.0).map(|v| v * p.luminance / 100.0);
    let fade = p.fade / 100.0;
    [0usize, 1, 2].map(|i| {
        let a = y.get(i).copied().unwrap_or(0.0);
        let b = c.get(i).copied().unwrap_or(0.0);
        a + (b - a) * fade
    })
}

/// Replace colour: membership `w = max over samples of clamp(1 - 255 d / fuzziness)` with `d` the
/// Euclidean distance in encoded RGB (`fuzziness 0` = exact matches only; the colour-range falloff
/// of `selection::colour_range`, fuzziness on the 0..255 scale of STU-RAS-151), then
/// `out = c + (h - c) w` with `h` the Hue/Saturation master shift of `c` ([`crate::adjust_math`]). Photoshop's
/// fuzziness falloff and colour space are UNVERIFIED (D5).
fn replace_colour(p: &ReplaceColour, c: [f32; 3]) -> Result<[f32; 3], AdjustError> {
    let mut w = 0.0f64;
    for s in &p.samples {
        let d2: f64 = (0..3)
            .map(|i| {
                let d = f64::from(c.get(i).copied().unwrap_or(0.0))
                    - f64::from(s.get(i).copied().unwrap_or(0.0));
                d * d
            })
            .sum();
        let m = if p.fuzziness == 0.0 {
            if d2 == 0.0 { 1.0 } else { 0.0 }
        } else {
            (1.0 - 255.0 * d2.sqrt() / f64::from(p.fuzziness)).clamp(0.0, 1.0)
        };
        w = w.max(m);
    }
    if w <= 0.0 {
        return Ok(c);
    }
    let shifted = crate::adjust_math::AdjustmentOp::HueSaturation(HueSaturation {
        hue: p.hue,
        saturation: p.saturation,
        lightness: p.lightness,
        colorize: false,
    })
    .apply(c)?;
    let w = w as f32;
    Ok([0usize, 1, 2].map(|i| {
        let a = c.get(i).copied().unwrap_or(0.0);
        let b = shifted.get(i).copied().unwrap_or(0.0);
        a + (b - a) * w
    }))
}
