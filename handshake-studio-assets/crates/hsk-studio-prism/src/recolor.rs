//! Deterministic colour harmony, recolour and colour reduction (lane SMB-AI-TYPE-PATH-RECOLOR;
//! STU-COL-235, STU-COL-236, STU-VEC-142, STU-VEC-052). Prism owns this maths; the vector domain
//! (the harness viewer) extracts the artwork's colours, calls these pure functions and writes the
//! result back as one command.
//!
//! Colours are encoded RGB triples in `0..=1` of the caller's working space. Hue, saturation and
//! brightness are HSB (HSV) of those components, the model colour-guide surfaces work in;
//! distances are CIE76 delta E in CIELab computed from the triple read as sRGB (D65), a Studio rule
//! for ordering and nearest-colour decisions only (no profile conversion happens here).
//!
//! * [`harmony`] (STU-COL-235): a pure function of (base, rule, count, step, library). The rule
//!   gives hue anchors (complementary 0/180, split complementary 0/150/210, analogous 0/30/-30,
//!   triad 0/120/240, tetrad 0/90/180/270, compound 0/30/180/150) or a one-axis ramp
//!   (monochromatic brightness, shades brightness down, tints saturation down, warm/cool hue
//!   toward 30/210 degrees, vivid saturation up, muted saturation down). Anchors cycle when `count`
//!   exceeds them, each further cycle stepped by `step` percent of brightness; `limit_to_library`
//!   snaps every output to its nearest library member.
//! * [`RecolorTable`] (STU-COL-236 ASSIGN): rows of current colours (merged rows hold several) with
//!   a new colour, or none (excluded). The row's KEY is its first current colour (callers order
//!   rows and members by prominence, [`extract_colours`]). `colorize_method`: `exact` (every member
//!   becomes the new colour), `scale_tints` (the darkest member becomes the new colour, lighter
//!   members proportional tints of it), `preserve_tints` (each member keeps its tint ratio to the
//!   key), `tints_and_shades` (lighter than the key -> tint, darker -> shade), `hue_shift` (each
//!   member rotated by the key's hue change, its own saturation and brightness kept). Tint ratio:
//!   `t = (1 - Y(c)) / (1 - Y(ref))` on relative luminance, tint `1 - t (1 - new)`; shade
//!   `new * Y(c) / Y(ref)`. Preserve toggles keep white, black and greys unchanged.
//! * [`RecolorAdjust`] (STU-VEC-142 EDIT sliders, plus the linked wheel rotation `hue_shift_deg`):
//!   saturation and brightness shifts move HSB S / B toward 1 (positive) or 0 (negative) by the
//!   percent; temperature shifts R up and B down by `0.1 * percent / 100`; luminosity scales HSL
//!   lightness like brightness. Applied after the table.
//! * [`reduce_colours`] (STU-COL-236 COLOUR REDUCTION): agglomerative merging of the closest pair
//!   (delta E, ties by lower index) until `target_count` rows remain; a merged row's key and new
//!   colour are its heaviest member. [`reduce_to_library`] groups colours by their nearest library
//!   member instead.
//!
//! Bounds (V4-01/02): colour counts are checked against the `MAX_*` constants before allocation;
//! non-finite or out-of-range components are refused. Illustrator Recolor Artwork / Color Guide
//! parity (key choice, tint maths, slider curves, the compound and warm/cool rules) is UNVERIFIED
//! (D5).
#![deny(clippy::arithmetic_side_effects)]

/// An encoded RGB colour, components `0..=1`.
pub type Rgb = [f64; 3];

/// Most colours one harmony returns.
pub const MAX_HARMONY_COLOURS: usize = 256;
/// Most distinct colours a recolour table, extraction or library holds.
pub const MAX_RECOLOR_COLOURS: usize = 65_536;
/// Most distinct colours [`reduce_colours`] merges (its work is cubic in this count).
pub const MAX_REDUCE_COLOURS: usize = 512;

/// Typed refusals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecolorError {
    InvalidInput(&'static str),
    OverLimit(&'static str),
}
impl RecolorError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "recolor_invalid_input",
            Self::OverLimit(_) => "recolor_over_limit",
        }
    }
}

/// STU-COL-235 rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HarmonyRule {
    Complementary,
    SplitComplementary,
    Analogous,
    Monochromatic,
    Triad,
    Tetrad,
    Compound,
    Shades,
    Tints,
    Warm,
    Cool,
    Vivid,
    Muted,
}

/// STU-COL-236 `colorize_method`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorizeMethod {
    #[default]
    Exact,
    ScaleTints,
    PreserveTints,
    TintsAndShades,
    HueShift,
}

/// One ASSIGN row: current colours (key first) and the new colour, `None` = excluded.
#[derive(Clone, Debug, PartialEq)]
pub struct RecolorRow {
    pub current: Vec<Rgb>,
    pub new: Option<Rgb>,
}

/// The ASSIGN table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecolorTable {
    pub rows: Vec<RecolorRow>,
    pub method: ColorizeMethod,
    pub preserve_white: bool,
    pub preserve_black: bool,
    pub preserve_grays: bool,
}

/// STU-VEC-142 sliders (percent, soft -100..=100) and the linked wheel rotation (degrees).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RecolorAdjust {
    pub hue_shift_deg: f64,
    pub saturation_shift: f64,
    pub brightness_shift: f64,
    pub temperature_shift: f64,
    pub luminosity_shift: f64,
}

/// A colour of the artwork with its prominence weight (area or use count).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WeightedColour {
    pub rgb: Rgb,
    pub weight: f64,
}

fn valid(c: Rgb) -> bool {
    c.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
}
fn need(c: Rgb, what: &'static str) -> Result<Rgb, RecolorError> {
    if valid(c) { Ok(c) } else { Err(RecolorError::InvalidInput(what)) }
}
fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

/// HSB of an encoded RGB colour: hue degrees `0..360`, saturation and brightness `0..=1`.
pub fn rgb_to_hsb(c: Rgb) -> [f64; 3] {
    let [r, g, b] = c;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d <= 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let s = if max <= 0.0 { 0.0 } else { d / max };
    [h.rem_euclid(360.0), s, max]
}

/// Encoded RGB of an HSB colour (hue wraps; saturation and brightness clamped).
pub fn hsb_to_rgb(hsb: [f64; 3]) -> Rgb {
    let h = hsb[0].rem_euclid(360.0) / 60.0;
    let (s, v) = (clamp01(hsb[1]), clamp01(hsb[2]));
    let c = v * s;
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [clamp01(r + m), clamp01(g + m), clamp01(b + m)]
}

fn linear(v: f64) -> f64 {
    if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// Relative luminance of the triple read as sRGB.
pub fn luminance(c: Rgb) -> f64 {
    0.212_6 * linear(c[0]) + 0.715_2 * linear(c[1]) + 0.072_2 * linear(c[2])
}

/// CIELab (D65) of the triple read as sRGB.
pub fn srgb_to_lab(c: Rgb) -> [f64; 3] {
    let [r, g, b] = [linear(c[0]), linear(c[1]), linear(c[2])];
    let x = (0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b) / 0.950_47;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175 * b;
    let z = (0.019_333_9 * r + 0.119_192 * g + 0.950_304_1 * b) / 1.088_83;
    let f = |t: f64| if t > 216.0 / 24_389.0 { t.cbrt() } else { (24_389.0 / 27.0 * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIE76 delta E between two triples read as sRGB.
pub fn delta_e(a: Rgb, b: Rgb) -> f64 {
    let (p, q) = (srgb_to_lab(a), srgb_to_lab(b));
    ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()
}

fn nearest(c: Rgb, library: &[Rgb]) -> Option<(usize, Rgb)> {
    let mut best: Option<(usize, f64)> = None;
    for (i, l) in library.iter().enumerate() {
        let d = delta_e(c, *l);
        if best.is_none_or(|(_, b)| d < b) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| (i, library[i]))
}

fn check_library(library: Option<&[Rgb]>) -> Result<(), RecolorError> {
    if let Some(l) = library {
        if l.len() > MAX_RECOLOR_COLOURS {
            return Err(RecolorError::OverLimit("library"));
        }
        if l.is_empty() || !l.iter().all(|c| valid(*c)) {
            return Err(RecolorError::InvalidInput("library"));
        }
    }
    Ok(())
}

/// STU-COL-235 harmony (module docs). `step` is percent (`0..=100`), `count` `1..=MAX_HARMONY_COLOURS`.
pub fn harmony(base: Rgb, rule: HarmonyRule, count: usize, step: f64, library: Option<&[Rgb]>) -> Result<Vec<Rgb>, RecolorError> {
    let base = need(base, "base")?;
    if count == 0 {
        return Err(RecolorError::InvalidInput("count"));
    }
    if count > MAX_HARMONY_COLOURS {
        return Err(RecolorError::OverLimit("count"));
    }
    if !(step.is_finite() && (0.0..=100.0).contains(&step)) {
        return Err(RecolorError::InvalidInput("step"));
    }
    check_library(library)?;
    let [h, s, b] = rgb_to_hsb(base);
    let k = step / 100.0;
    let anchors: &[f64] = match rule {
        HarmonyRule::Complementary => &[0.0, 180.0],
        HarmonyRule::SplitComplementary => &[0.0, 150.0, 210.0],
        HarmonyRule::Analogous => &[0.0, 30.0, -30.0],
        HarmonyRule::Triad => &[0.0, 120.0, 240.0],
        HarmonyRule::Tetrad => &[0.0, 90.0, 180.0, 270.0],
        HarmonyRule::Compound => &[0.0, 30.0, 180.0, 150.0],
        _ => &[],
    };
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let n = i as f64;
        let c = if anchors.is_empty() {
            // One-axis ramps: member i moves i steps from the base.
            let d = n * k;
            match rule {
                HarmonyRule::Monochromatic => hsb_to_rgb([h, s, if i.is_multiple_of(2) { b - d / 2.0 } else { b + d / 2.0 }]),
                HarmonyRule::Shades => hsb_to_rgb([h, s, b * (1.0 - d)]),
                HarmonyRule::Tints => hsb_to_rgb([h, s * (1.0 - d), b + (1.0 - b) * d]),
                HarmonyRule::Warm | HarmonyRule::Cool => {
                    let target = if rule == HarmonyRule::Warm { 30.0 } else { 210.0 };
                    let diff = (target - h + 540.0).rem_euclid(360.0) - 180.0;
                    hsb_to_rgb([h + diff * d.min(1.0), s, b])
                }
                HarmonyRule::Vivid => hsb_to_rgb([h, s + (1.0 - s) * d, b]),
                _ => hsb_to_rgb([h, s * (1.0 - d), b]),
            }
        } else {
            let len = anchors.len();
            let (cycle, slot) = (i.checked_div(len).unwrap_or(0), i.checked_rem(len).unwrap_or(0));
            let dim = cycle as f64 * k;
            hsb_to_rgb([h + anchors[slot], s, b * (1.0 - dim)])
        };
        let c = if i == 0 && !matches!(rule, HarmonyRule::Monochromatic) { base } else { c };
        out.push(match library {
            Some(l) => nearest(c, l).map_or(c, |(_, m)| m),
            None => c,
        });
    }
    Ok(out)
}

/// Distinct colours of the artwork ordered by prominence: equal triples merged (weights summed),
/// then weight descending, ties by the triple (lexicographic) so the order is deterministic.
pub fn extract_colours(colours: &[WeightedColour]) -> Result<Vec<WeightedColour>, RecolorError> {
    if colours.len() > MAX_RECOLOR_COLOURS {
        return Err(RecolorError::OverLimit("colours"));
    }
    let mut out: Vec<WeightedColour> = Vec::new();
    for c in colours {
        need(c.rgb, "colour")?;
        if !(c.weight.is_finite() && c.weight >= 0.0) {
            return Err(RecolorError::InvalidInput("weight"));
        }
        match out.iter_mut().find(|o| o.rgb == c.rgb) {
            Some(o) => o.weight += c.weight,
            None => out.push(*c),
        }
    }
    out.sort_by(|a, b| b.weight.total_cmp(&a.weight).then_with(|| a.rgb.iter().zip(b.rgb).map(|(x, y)| x.total_cmp(&y)).find(|o| o.is_ne()).unwrap_or(std::cmp::Ordering::Equal)));
    Ok(out)
}

fn is_white(c: Rgb) -> bool {
    c.iter().all(|v| *v >= 1.0 - 1e-9)
}
fn is_black(c: Rgb) -> bool {
    c.iter().all(|v| *v <= 1e-9)
}
fn is_gray(c: Rgb) -> bool {
    (c[0] - c[1]).abs() <= 1e-9 && (c[1] - c[2]).abs() <= 1e-9
}

/// True when `c` is kept by the table's preserve toggles.
fn preserved(t: &RecolorTable, c: Rgb) -> bool {
    (t.preserve_white && is_white(c)) || (t.preserve_black && is_black(c)) || (t.preserve_grays && is_gray(c) && !is_white(c) && !is_black(c))
}

fn tint(new: Rgb, t: f64) -> Rgb {
    let t = clamp01(t);
    [1.0 - t * (1.0 - new[0]), 1.0 - t * (1.0 - new[1]), 1.0 - t * (1.0 - new[2])]
}
fn tint_ratio(c: Rgb, reference: Rgb) -> f64 {
    let r = 1.0 - luminance(reference);
    if r <= 1e-12 { 0.0 } else { (1.0 - luminance(c)) / r }
}

impl RecolorTable {
    /// Checks the table (row count, members, colours).
    pub fn validate(&self) -> Result<(), RecolorError> {
        let members = self.rows.iter().try_fold(0_usize, |n, r| n.checked_add(r.current.len())).ok_or(RecolorError::OverLimit("rows"))?;
        if members > MAX_RECOLOR_COLOURS {
            return Err(RecolorError::OverLimit("rows"));
        }
        for r in &self.rows {
            if r.current.is_empty() {
                return Err(RecolorError::InvalidInput("empty_row"));
            }
            for c in &r.current {
                need(*c, "row_colour")?;
            }
            if let Some(n) = r.new {
                need(n, "new_colour")?;
            }
        }
        Ok(())
    }

    /// The colour `c` maps to (module docs); colours in no row, excluded rows and preserved
    /// colours are returned unchanged.
    pub fn map(&self, c: Rgb) -> Rgb {
        if preserved(self, c) {
            return c;
        }
        let Some(row) = self.rows.iter().find(|r| r.current.contains(&c)) else { return c };
        let Some(new) = row.new else { return c };
        let key = row.current[0];
        match self.method {
            ColorizeMethod::Exact => new,
            ColorizeMethod::ScaleTints => {
                let darkest = row.current.iter().copied().fold(key, |d, x| if luminance(x) < luminance(d) { x } else { d });
                tint(new, tint_ratio(c, darkest))
            }
            ColorizeMethod::PreserveTints => tint(new, tint_ratio(c, key)),
            ColorizeMethod::TintsAndShades => {
                let (yc, yk) = (luminance(c), luminance(key));
                if yc >= yk {
                    tint(new, tint_ratio(c, key))
                } else {
                    let s = if yk <= 1e-12 { 0.0 } else { yc / yk };
                    [new[0] * s, new[1] * s, new[2] * s]
                }
            }
            ColorizeMethod::HueShift => {
                let ([hk, ..], [hn, ..], [hc, sc, bc]) = (rgb_to_hsb(key), rgb_to_hsb(new), rgb_to_hsb(c));
                hsb_to_rgb([hc + (hn - hk), sc, bc])
            }
        }
    }
}

fn shift(v: f64, percent: f64) -> f64 {
    let p = (percent / 100.0).clamp(-1.0, 1.0);
    clamp01(if p >= 0.0 { v + (1.0 - v) * p } else { v * (1.0 + p) })
}

impl RecolorAdjust {
    /// Checks the sliders are finite (their soft range is a UI concern; values are clamped to
    /// -100..=100 percent when applied).
    pub fn validate(&self) -> Result<(), RecolorError> {
        let v = [self.hue_shift_deg, self.saturation_shift, self.brightness_shift, self.temperature_shift, self.luminosity_shift];
        if v.iter().all(|x| x.is_finite()) { Ok(()) } else { Err(RecolorError::InvalidInput("adjust")) }
    }
    /// True when every slider is zero.
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }
    /// Applies the sliders to `c` (module docs).
    pub fn apply(&self, c: Rgb) -> Rgb {
        if self.is_identity() {
            return c;
        }
        let [h, s, b] = rgb_to_hsb(c);
        let mut out = hsb_to_rgb([h + self.hue_shift_deg, shift(s, self.saturation_shift), shift(b, self.brightness_shift)]);
        let t = 0.1 * (self.temperature_shift / 100.0).clamp(-1.0, 1.0);
        out = [clamp01(out[0] + t), out[1], clamp01(out[2] - t)];
        if self.luminosity_shift != 0.0 {
            // HSL lightness scaled like brightness, hue and HSL saturation kept.
            let (max, min) = (out[0].max(out[1]).max(out[2]), out[0].min(out[1]).min(out[2]));
            let l = (max + min) / 2.0;
            let l2 = shift(l, self.luminosity_shift);
            let d = l2 - l;
            let scale = if d >= 0.0 { if l >= 1.0 { 0.0 } else { (1.0 - l2) / (1.0 - l) } } else if l <= 0.0 { 0.0 } else { l2 / l };
            out = out.map(|v| clamp01(l2 + (v - l) * scale));
        }
        out
    }
}

/// The full mapping of one colour: the table, then the sliders.
pub fn recolor(table: &RecolorTable, adjust: &RecolorAdjust, c: Rgb) -> Rgb {
    if preserved(table, c) {
        return c;
    }
    adjust.apply(table.map(c))
}

/// STU-COL-236 colour reduction to `target_count` rows (module docs). Preserved colours (per the
/// toggles) stay out of every row. Each row's new colour is its key (the caller assigns new ones).
pub fn reduce_colours(colours: &[WeightedColour], target_count: usize, preserve_white: bool, preserve_black: bool, preserve_grays: bool) -> Result<Vec<RecolorRow>, RecolorError> {
    if target_count == 0 {
        return Err(RecolorError::InvalidInput("target_count"));
    }
    let probe = RecolorTable { preserve_white, preserve_black, preserve_grays, ..Default::default() };
    let distinct = extract_colours(colours)?;
    let distinct: Vec<WeightedColour> = distinct.into_iter().filter(|c| !preserved(&probe, c.rgb)).collect();
    if distinct.len() > MAX_REDUCE_COLOURS {
        return Err(RecolorError::OverLimit("reduce_colours"));
    }
    // Groups of members (indices into `distinct`, heaviest first) with their summed weight.
    let mut groups: Vec<(Vec<usize>, f64)> = distinct.iter().enumerate().map(|(i, c)| (vec![i], c.weight)).collect();
    let lab: Vec<[f64; 3]> = distinct.iter().map(|c| srgb_to_lab(c.rgb)).collect();
    let dist = |a: &[usize], b: &[usize]| -> f64 {
        // Single linkage on delta E: the closest member pair.
        let mut best = f64::INFINITY;
        for i in a {
            for j in b {
                let (p, q) = (lab[*i], lab[*j]);
                best = best.min(((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt());
            }
        }
        best
    };
    while groups.len() > target_count {
        let mut pick = (0_usize, 1_usize, f64::INFINITY);
        for i in 0..groups.len() {
            for j in i.saturating_add(1)..groups.len() {
                let d = dist(&groups[i].0, &groups[j].0);
                if d < pick.2 {
                    pick = (i, j, d);
                }
            }
        }
        let (i, j, _) = pick;
        let (members, w) = groups.remove(j);
        groups[i].0.extend(members);
        groups[i].1 += w;
        groups[i].0.sort_by(|a, b| distinct[*b].weight.total_cmp(&distinct[*a].weight).then(a.cmp(b)));
    }
    groups.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0[0].cmp(&b.0[0])));
    Ok(groups
        .into_iter()
        .map(|(members, _)| {
            let current: Vec<Rgb> = members.iter().map(|m| distinct[*m].rgb).collect();
            RecolorRow { new: Some(current[0]), current }
        })
        .collect())
}

/// STU-COL-236 reduction to a library: every colour joins the row of its nearest library member
/// (rows in library order, empty ones omitted); the row's new colour is that member.
pub fn reduce_to_library(colours: &[WeightedColour], library: &[Rgb], preserve_white: bool, preserve_black: bool, preserve_grays: bool) -> Result<Vec<RecolorRow>, RecolorError> {
    check_library(Some(library))?;
    let probe = RecolorTable { preserve_white, preserve_black, preserve_grays, ..Default::default() };
    let distinct = extract_colours(colours)?;
    let mut rows: Vec<Vec<Rgb>> = vec![Vec::new(); library.len()];
    for c in distinct.iter().filter(|c| !preserved(&probe, c.rgb)) {
        if let Some((i, _)) = nearest(c.rgb, library) {
            rows[i].push(c.rgb);
        }
    }
    Ok(rows.into_iter().zip(library).filter(|(r, _)| !r.is_empty()).map(|(current, l)| RecolorRow { current, new: Some(*l) }).collect())
}
