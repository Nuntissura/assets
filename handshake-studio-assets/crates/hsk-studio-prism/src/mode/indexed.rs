//! RGB -> Indexed (STU-COL-119 indexed option record; MT-31175, MT-31177).
//!
//! Input: an 8-bit RGB or RGBA image, borrowed and never written. Output: one index byte per
//! pixel and a [`ColorTable`] of at most 256 entries. Same input, options and environment give the
//! same bytes (single thread; maps are used for lookup only, never iterated for output order).
//!
//! Pipeline (Studio readings unless the spec is cited; Photoshop parity UNVERIFIED, D5):
//! 1. Refusals before any allocation: `master_*` (`master_palette_needs_several_images`),
//!    `mac_os` / `windows` / `previous` (`palette_table_not_captured`: their entries are not in the
//!    spec), `uniform` / `local_*` without `indexed.colors` (`parameter_required`; its default is
//!    UNKNOWN) or with 0 (`empty_palette`), diffusion without `indexed.dither_amount`, noise without
//!    a seed, a foreground/background matte without the caller's colour.
//! 2. Effective colour: with transparency on, a pixel with alpha below 128 is transparent; any
//!    other pixel with alpha below 255 is blended over the matte when one is set
//!    (`c a + m (255 - a)` over 255, rounded); otherwise alpha is dropped. Mattes: white
//!    (255,255,255), black (0,0,0), semi_gray (128,128,128), netscape_gray (192,192,192),
//!    foreground/background = the caller's colours.
//! 3. Palette, forced colours first (black_white: black, white; primaries: black, white, red, green,
//!    blue, cyan, magenta, yellow; web: the 216 web colours), duplicates dropped:
//!    `exact` = forced + distinct colours in first-occurrence raster order, over 256 refused
//!    (`exact_palette_over_256`); `web` = forced + the 216 colours of levels 0, 51, ..., 255 (red
//!    outermost); `uniform` = forced + the largest `L^3` cube (L >= 2) fitting the remaining count,
//!    else a gray ramp; `local_*` = forced + the distinct colours when they fit, else a median cut
//!    of a 5-bit-per-channel histogram (32768 bins, mean colour per box): `local_adaptive` splits
//!    along the widest RGB axis, `local_perceptual` along the widest CIELab axis
//!    ([`crate::srgb_to_lab`]), `local_selective` the same with each bin weighted by
//!    `1 + chroma / 64`. The count includes the forced colours (`forced_colors_exceed_count`
//!    otherwise) and, when transparent pixels exist, one entry is kept for transparency, appended
//!    last as `transparency_index` (its RGB is the matte colour, else black).
//! 4. Mapping: the nearest entry (squared RGB distance; CIE76 distance in CIELab
//!    ([`crate::srgb_to_lab`], the ordering of [`crate::delta_e`]) for `local_perceptual` and
//!    `local_selective`; ties to the lower index) of the colour rounded to
//!    8 bits after dithering: `diffusion` = Floyd-Steinberg in raster order with the error scaled
//!    by `dither_amount / 100`; `pattern` = 8 x 8 Bayer offset of +-16 per channel; `noise` =
//!    SplitMix64 from the caller's seed, +-16 per channel. `preserve_exact_colors`: a pixel whose
//!    effective colour is a palette entry takes that entry and diffuses no error.
#![deny(clippy::arithmetic_side_effects)]

use std::collections::{HashMap, HashSet};

use super::bitmap::bayer8;
use super::color_table::{ColorTable, MAX_TABLE_ENTRIES};
use super::{check_plane, token_enum, ModeError, INDEXED_COLORS, INDEXED_DITHER_AMOUNT};
use crate::srgb_to_lab;

/// Most memoised nearest-colour lookups per conversion (bounds memory; results do not depend on it).
const MAX_MEMO: usize = 1 << 20;
/// Pattern and noise dither spread, per channel, in 8-bit steps (Studio reading).
const DITHER_SPREAD: f64 = 32.0;
/// Alpha below which a pixel is transparent when transparency is on (Studio reading).
const TRANSPARENT_BELOW: u8 = 128;

token_enum! {
    /// STU-COL-119 `palette` (twelve values).
    Palette {
        Exact => "exact",
        MacOs => "mac_os",
        Windows => "windows",
        Web => "web",
        Uniform => "uniform",
        LocalPerceptual => "local_perceptual",
        LocalSelective => "local_selective",
        LocalAdaptive => "local_adaptive",
        MasterPerceptual => "master_perceptual",
        MasterSelective => "master_selective",
        MasterAdaptive => "master_adaptive",
        Previous => "previous",
    }
}

impl Default for Palette {
    /// STU-COL-119: default `exact`.
    fn default() -> Self {
        Self::Exact
    }
}

impl Palette {
    /// STU-COL-119: `indexed.colors` is read-only and derived for these palettes.
    pub const fn colors_read_only(self) -> bool {
        matches!(self, Self::Exact | Self::MacOs | Self::Windows | Self::Web | Self::Previous)
    }
}

token_enum! {
    /// STU-COL-119 `forced`.
    ForcedColors {
        None => "none",
        BlackWhite => "black_white",
        Primaries => "primaries",
        Web => "web",
    }
}

token_enum! {
    /// STU-COL-119 `dither`.
    IndexedDither {
        None => "none",
        Diffusion => "diffusion",
        Pattern => "pattern",
        Noise => "noise",
    }
}

token_enum! {
    /// STU-COL-119 `matte` (seven values).
    Matte {
        None => "none",
        ForegroundColor => "foreground_color",
        BackgroundColor => "background_color",
        White => "white",
        Black => "black",
        SemiGray => "semi_gray",
        NetscapeGray => "netscape_gray",
    }
}

/// STU-COL-119 indexed option record (eight fields). `colors` and `dither_amount` are set through
/// checked setters; `None` = UNKNOWN / unset. The spec declares no default for `forced`,
/// `transparency`, `dither`, `preserve_exact_colors` or `matte`; [`IndexedOptions::new`] sets them
/// to none / false (Studio construction values, not vendor defaults).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedOptions {
    palette: Palette,
    colors: Option<u16>,
    pub forced: ForcedColors,
    pub transparency: bool,
    pub dither: IndexedDither,
    dither_amount: Option<u8>,
    pub preserve_exact_colors: bool,
    pub matte: Matte,
}

impl Default for IndexedOptions {
    fn default() -> Self {
        Self::new(Palette::default())
    }
}

impl IndexedOptions {
    pub const fn new(palette: Palette) -> Self {
        Self {
            palette,
            colors: None,
            forced: ForcedColors::None,
            transparency: false,
            dither: IndexedDither::None,
            dither_amount: None,
            preserve_exact_colors: false,
            matte: Matte::None,
        }
    }
    pub const fn palette(&self) -> Palette {
        self.palette
    }
    /// Change the palette; a stored count stays (inert while the palette derives its count).
    pub fn set_palette(&mut self, palette: Palette) {
        self.palette = palette;
    }
    /// The stored `indexed.colors` (unset = UNKNOWN). Derived palettes report their count on the
    /// result ([`IndexedImage::colors_used`]).
    pub const fn colors(&self) -> Option<u16> {
        self.colors
    }
    /// Set `indexed.colors`: `indexed_colors_read_only` for a derived palette, over 256
    /// `parameter_out_of_range`. No minimum is declared (0 is stored; a conversion refuses it as
    /// `empty_palette`).
    pub fn set_colors(&mut self, colors: u16) -> Result<(), ModeError> {
        if self.palette.colors_read_only() {
            return Err(ModeError::IndexedColorsReadOnly);
        }
        INDEXED_COLORS.check(f64::from(colors))?;
        self.colors = Some(colors);
        Ok(())
    }
    pub const fn dither_amount(&self) -> Option<u8> {
        self.dither_amount
    }
    /// Set `indexed.dither_amount` (hard 1..=100, percent). Inert unless `dither` is diffusion.
    pub fn set_dither_amount(&mut self, amount: u8) -> Result<(), ModeError> {
        INDEXED_DITHER_AMOUNT.check(f64::from(amount))?;
        self.dither_amount = Some(amount);
        Ok(())
    }
}

/// Caller-supplied colours and seed a conversion may need.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IndexedEnv {
    pub foreground_color: Option<[u8; 3]>,
    pub background_color: Option<[u8; 3]>,
    /// Required by noise dither.
    pub noise_seed: Option<u64>,
}

/// A borrowed 8-bit image, row-major, no padding.
#[derive(Clone, Copy, Debug)]
pub enum RgbImage<'a> {
    Rgb8 { width: u32, height: u32, data: &'a [u8] },
    Rgba8 { width: u32, height: u32, data: &'a [u8] },
}

/// The result: indices, the table and the options exactly as given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedImage {
    pub width: u32,
    pub height: u32,
    pub indices: Vec<u8>,
    pub table: ColorTable,
    pub options: IndexedOptions,
}

impl IndexedImage {
    /// The derived (read-only) colour count: the table length.
    pub fn colors_used(&self) -> usize {
        self.table.len()
    }
}

/// The 216 web colours, red outermost.
pub fn web_palette() -> Vec<[u8; 3]> {
    const L: [u8; 6] = [0, 51, 102, 153, 204, 255];
    let mut out = Vec::with_capacity(216);
    for r in L {
        for g in L {
            for b in L {
                out.push([r, g, b]);
            }
        }
    }
    out
}

fn forced_colours(f: ForcedColors) -> Vec<[u8; 3]> {
    match f {
        ForcedColors::None => Vec::new(),
        ForcedColors::BlackWhite => vec![[0, 0, 0], [255, 255, 255]],
        ForcedColors::Primaries => vec![
            [0, 0, 0],
            [255, 255, 255],
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [0, 255, 255],
            [255, 0, 255],
            [255, 255, 0],
        ],
        ForcedColors::Web => web_palette(),
    }
}

fn push_unique(pal: &mut Vec<[u8; 3]>, c: [u8; 3]) {
    if !pal.contains(&c) {
        pal.push(c);
    }
}

fn level(i: usize, n: usize) -> u8 {
    // round(255 i / (n - 1)) for n >= 2.
    let d = n.saturating_sub(1).max(1) as f64;
    (255.0 * i as f64 / d).round().clamp(0.0, 255.0) as u8
}

fn uniform_grid(k: usize) -> Vec<[u8; 3]> {
    let mut l = 1_usize;
    while l.saturating_add(1).checked_pow(3).is_some_and(|c| c <= k) {
        l = l.saturating_add(1);
    }
    let mut out = Vec::new();
    if l >= 2 {
        for r in 0..l {
            for g in 0..l {
                for b in 0..l {
                    out.push([level(r, l), level(g, l), level(b, l)]);
                }
            }
        }
    } else if k == 1 {
        out.push([128, 128, 128]);
    } else {
        for i in 0..k {
            let v = level(i, k);
            out.push([v, v, v]);
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Metric {
    Rgb,
    Lab,
}

/// Median cut of the histogram to at most `k` mean colours (module docs).
fn median_cut(counts: &[u64], sums: &[[u64; 3]], k: usize, palette: Palette) -> Vec<[u8; 3]> {
    let mean = |b: usize| -> [f64; 3] {
        let n = counts[b] as f64;
        [sums[b][0] as f64 / n, sums[b][1] as f64 / n, sums[b][2] as f64 / n]
    };
    let bins: Vec<usize> = (0..counts.len()).filter(|b| counts[*b] != 0).collect();
    if bins.is_empty() || k == 0 {
        return Vec::new();
    }
    let lab_space = matches!(palette, Palette::LocalPerceptual | Palette::LocalSelective);
    // Per-bin coordinates and weights.
    let mut coord = vec![[0.0_f64; 3]; counts.len()];
    let mut weight = vec![0.0_f64; counts.len()];
    for &b in &bins {
        let m = mean(b);
        coord[b] = if lab_space { srgb_to_lab([m[0] / 255.0, m[1] / 255.0, m[2] / 255.0]) } else { m };
        let w = counts[b] as f64;
        weight[b] = if palette == Palette::LocalSelective {
            let c = coord[b];
            w * (1.0 + (c[1] * c[1] + c[2] * c[2]).sqrt() / 64.0)
        } else {
            w
        };
    }
    let extent = |bx: &[usize]| -> (usize, f64) {
        let mut best = (0_usize, -1.0_f64);
        for axis in 0..3 {
            let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
            for &b in bx {
                lo = lo.min(coord[b][axis]);
                hi = hi.max(coord[b][axis]);
            }
            if hi - lo > best.1 {
                best = (axis, hi - lo);
            }
        }
        best
    };
    let mut boxes: Vec<Vec<usize>> = vec![bins];
    while boxes.len() < k {
        // The splittable box (two or more bins) with the widest extent; ties to the lower index.
        let mut pick: Option<(usize, usize, f64)> = None;
        for (i, bx) in boxes.iter().enumerate() {
            if bx.len() < 2 {
                continue;
            }
            let (axis, e) = extent(bx);
            if pick.is_none_or(|(_, _, pe)| e > pe) {
                pick = Some((i, axis, e));
            }
        }
        let Some((i, axis, _)) = pick else { break };
        let mut bx = std::mem::take(&mut boxes[i]);
        bx.sort_by(|a, b| coord[*a][axis].total_cmp(&coord[*b][axis]).then(a.cmp(b)));
        let total: f64 = bx.iter().map(|b| weight[*b]).sum();
        let mut acc = 0.0;
        let mut cut = 1_usize;
        for (j, b) in bx.iter().enumerate() {
            acc += weight[*b];
            if acc >= total / 2.0 {
                cut = j.saturating_add(1);
                break;
            }
        }
        let cut = cut.clamp(1, bx.len().saturating_sub(1));
        let hi = bx.split_off(cut);
        boxes[i] = bx;
        boxes.push(hi);
    }
    let mut out = Vec::with_capacity(boxes.len());
    for bx in &boxes {
        let (mut n, mut s) = (0_u64, [0_u64; 3]);
        for &b in bx {
            n = n.saturating_add(counts[b]);
            for (acc, v) in s.iter_mut().zip(sums[b]) {
                *acc = acc.saturating_add(v);
            }
        }
        let nf = n.max(1) as f64;
        out.push([0, 1, 2].map(|c| (s[c] as f64 / nf).round().clamp(0.0, 255.0) as u8));
    }
    out
}

struct Nearest<'a> {
    pal: &'a [[u8; 3]],
    lab: Vec<[f64; 3]>,
    metric: Metric,
    exact: HashMap<[u8; 3], u8>,
    memo: HashMap<[u8; 3], u8>,
}

impl<'a> Nearest<'a> {
    fn new(pal: &'a [[u8; 3]], metric: Metric) -> Self {
        let lab = if metric == Metric::Lab { pal.iter().map(|c| srgb_to_lab(unit(*c))).collect() } else { Vec::new() };
        let mut exact = HashMap::new();
        for (i, c) in pal.iter().enumerate() {
            exact.entry(*c).or_insert(u8::try_from(i).unwrap_or(u8::MAX));
        }
        Self { pal, lab, metric, exact, memo: HashMap::new() }
    }
    fn get(&mut self, c: [u8; 3]) -> u8 {
        if let Some(i) = self.exact.get(&c) {
            return *i;
        }
        if let Some(i) = self.memo.get(&c) {
            return *i;
        }
        let q = match self.metric {
            Metric::Rgb => c.map(f64::from),
            Metric::Lab => srgb_to_lab(unit(c)),
        };
        let mut best = (0_usize, f64::INFINITY);
        for (i, p) in self.pal.iter().enumerate() {
            let r = match self.metric {
                Metric::Rgb => p.map(f64::from),
                Metric::Lab => self.lab[i],
            };
            let d = (q[0] - r[0]).powi(2) + (q[1] - r[1]).powi(2) + (q[2] - r[2]).powi(2);
            if d < best.1 {
                best = (i, d);
            }
        }
        let i = u8::try_from(best.0).unwrap_or(u8::MAX);
        if self.memo.len() < MAX_MEMO {
            self.memo.insert(c, i);
        }
        i
    }
}

fn unit(c: [u8; 3]) -> [f64; 3] {
    [f64::from(c[0]) / 255.0, f64::from(c[1]) / 255.0, f64::from(c[2]) / 255.0]
}

fn round8(v: f64) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

struct SplitMix64(u64);
impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ z.wrapping_shr(30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ z.wrapping_shr(27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ z.wrapping_shr(31)
    }
}

/// Convert `source` to indexed colour (module docs).
pub fn rgb_to_indexed(source: &RgbImage<'_>, options: &IndexedOptions, env: &IndexedEnv) -> Result<IndexedImage, ModeError> {
    let (width, height, data, samples) = match *source {
        RgbImage::Rgb8 { width, height, data } => (width, height, data, 3_usize),
        RgbImage::Rgba8 { width, height, data } => (width, height, data, 4_usize),
    };
    let pixels = check_plane(width, height, samples as u64, data.len())?;
    let palette = options.palette;
    match palette {
        Palette::MasterPerceptual | Palette::MasterSelective | Palette::MasterAdaptive => {
            return Err(ModeError::MasterPaletteNeedsSeveralImages);
        }
        Palette::MacOs | Palette::Windows | Palette::Previous => return Err(ModeError::PaletteTableNotCaptured),
        _ => {}
    }
    let requested = if palette.colors_read_only() {
        None
    } else {
        let n = options.colors.ok_or(ModeError::ParameterRequired(INDEXED_COLORS.key))?;
        if n == 0 {
            return Err(ModeError::EmptyPalette);
        }
        Some(usize::from(n))
    };
    let amount = match options.dither {
        IndexedDither::Diffusion => {
            f64::from(options.dither_amount.ok_or(ModeError::ParameterRequired(INDEXED_DITHER_AMOUNT.key))?) / 100.0
        }
        _ => 0.0,
    };
    let seed = match options.dither {
        IndexedDither::Noise => Some(env.noise_seed.ok_or(ModeError::ParameterRequired("indexed.noise_seed"))?),
        _ => None,
    };
    let matte = match options.matte {
        Matte::None => None,
        Matte::ForegroundColor => Some(env.foreground_color.ok_or(ModeError::MatteColorRequired)?),
        Matte::BackgroundColor => Some(env.background_color.ok_or(ModeError::MatteColorRequired)?),
        Matte::White => Some([255, 255, 255]),
        Matte::Black => Some([0, 0, 0]),
        Matte::SemiGray => Some([128, 128, 128]),
        Matte::NetscapeGray => Some([192, 192, 192]),
    };
    let forced = forced_colours(options.forced);
    if let Some(n) = requested {
        if forced.len() > n.min(MAX_TABLE_ENTRIES) {
            return Err(ModeError::ForcedColorsExceedCount);
        }
    }
    // Effective colour of pixel i (None = transparent); reads the source only.
    let effective = |i: usize| -> Option<[u8; 3]> {
        let o = i.wrapping_mul(samples);
        let rgb = [data[o], data[o.wrapping_add(1)], data[o.wrapping_add(2)]];
        let a = if samples == 4 { data[o.wrapping_add(3)] } else { 255 };
        if options.transparency && a < TRANSPARENT_BELOW {
            return None;
        }
        match matte {
            Some(m) if a < 255 => {
                let af = f64::from(a);
                Some([0, 1, 2].map(|k| round8((f64::from(rgb[k]) * af + f64::from(m[k]) * (255.0 - af)) / 255.0)))
            }
            _ => Some(rgb),
        }
    };
    // Scan: transparency present, distinct colours (first occurrence) up to 257.
    let mut any_transparent = false;
    let mut distinct: Vec<[u8; 3]> = Vec::new();
    let mut seen: HashSet<[u8; 3]> = HashSet::new();
    let mut over = false;
    for i in 0..pixels {
        match effective(i) {
            None => any_transparent = true,
            Some(c) => {
                if !over && !seen.contains(&c) {
                    if distinct.len() > MAX_TABLE_ENTRIES {
                        over = true;
                    } else {
                        seen.insert(c);
                        distinct.push(c);
                    }
                }
            }
        }
    }
    drop(seen);
    let reserve = usize::from(any_transparent);
    let cap = MAX_TABLE_ENTRIES.saturating_sub(reserve);
    let mut pal: Vec<[u8; 3]> = Vec::new();
    for c in &forced {
        push_unique(&mut pal, *c);
    }
    match palette {
        Palette::Exact => {
            for c in &distinct {
                push_unique(&mut pal, *c);
            }
            if over || pal.len() > cap {
                return Err(ModeError::ExactPaletteOver256);
            }
        }
        Palette::Web => {
            for c in web_palette() {
                push_unique(&mut pal, c);
            }
        }
        Palette::Uniform => {
            let budget = requested.unwrap_or(0).min(cap);
            if pal.len() > budget {
                return Err(ModeError::ForcedColorsExceedCount);
            }
            for c in uniform_grid(budget.saturating_sub(pal.len())) {
                push_unique(&mut pal, c);
            }
        }
        _ => {
            let budget = requested.unwrap_or(0).min(cap);
            if pal.len() > budget {
                return Err(ModeError::ForcedColorsExceedCount);
            }
            let k = budget.saturating_sub(pal.len());
            let rest: Vec<[u8; 3]> = distinct.iter().copied().filter(|c| !pal.contains(c)).collect();
            if !over && rest.len() <= k {
                pal.extend(rest);
            } else {
                let mut counts = vec![0_u64; 32_768];
                let mut sums = vec![[0_u64; 3]; 32_768];
                for i in 0..pixels {
                    if let Some(c) = effective(i) {
                        let b = usize::from(c[0].wrapping_shr(3)).wrapping_shl(10)
                            | usize::from(c[1].wrapping_shr(3)).wrapping_shl(5)
                            | usize::from(c[2].wrapping_shr(3));
                        counts[b] = counts[b].saturating_add(1);
                        for (acc, v) in sums[b].iter_mut().zip(c) {
                            *acc = acc.saturating_add(u64::from(v));
                        }
                    }
                }
                for c in median_cut(&counts, &sums, k, palette) {
                    if pal.len() < budget {
                        push_unique(&mut pal, c);
                    }
                }
            }
        }
    }
    let transparency_index = if any_transparent {
        let t = u8::try_from(pal.len()).map_err(|_| ModeError::ColorTableOver256)?;
        Some(t)
    } else {
        None
    };
    let mut table_entries = pal.clone();
    if any_transparent {
        table_entries.push(matte.unwrap_or([0, 0, 0]));
    }
    if table_entries.is_empty() {
        return Err(ModeError::EmptyPalette);
    }
    let table = ColorTable::new(&table_entries, transparency_index)?;
    let metric = if matches!(palette, Palette::LocalPerceptual | Palette::LocalSelective) { Metric::Lab } else { Metric::Rgb };
    let mut nearest = Nearest::new(&pal, metric);
    let w = usize::try_from(width).map_err(|_| ModeError::PixelBudgetExceeded)?;
    let mut indices = vec![0_u8; pixels];
    let row_len = w.checked_add(2).and_then(|v| v.checked_mul(3)).ok_or(ModeError::PixelBudgetExceeded)?;
    let (mut cur, mut next) = if options.dither == IndexedDither::Diffusion {
        (vec![0.0_f64; row_len], vec![0.0_f64; row_len])
    } else {
        (Vec::new(), Vec::new())
    };
    let mut rng = seed.map(SplitMix64);
    for (i, out) in indices.iter_mut().enumerate() {
        let x = i.wrapping_rem(w);
        let y = i.wrapping_div(w);
        if x == 0 && y != 0 && options.dither == IndexedDither::Diffusion {
            std::mem::swap(&mut cur, &mut next);
            next.iter_mut().for_each(|e| *e = 0.0);
        }
        let noise = rng.as_mut().map(SplitMix64::next);
        let Some(c) = effective(i) else {
            *out = transparency_index.unwrap_or(0);
            continue;
        };
        if pal.is_empty() {
            *out = transparency_index.unwrap_or(0);
            continue;
        }
        if options.preserve_exact_colors {
            if let Some(e) = nearest.exact.get(&c) {
                *out = *e;
                continue;
            }
        }
        let target: [f64; 3] = match options.dither {
            IndexedDither::None => c.map(f64::from),
            IndexedDither::Diffusion => {
                let k = x.wrapping_add(1).wrapping_mul(3);
                [0, 1, 2].map(|ch| f64::from(c[ch]) + cur[k.wrapping_add(ch)])
            }
            IndexedDither::Pattern => {
                let off = ((f64::from(bayer8(x, y)) + 0.5) / 64.0 - 0.5) * DITHER_SPREAD;
                c.map(|v| f64::from(v) + off)
            }
            IndexedDither::Noise => {
                let r = noise.unwrap_or(0);
                let off = |shift: u32| ((r.wrapping_shr(shift) & 0xFFFF) as f64 / 65_535.0 - 0.5) * DITHER_SPREAD;
                [f64::from(c[0]) + off(0), f64::from(c[1]) + off(16), f64::from(c[2]) + off(32)]
            }
        };
        let idx = nearest.get(target.map(round8));
        *out = idx;
        if options.dither == IndexedDither::Diffusion {
            let p = pal[usize::from(idx)];
            let k = x.wrapping_add(1).wrapping_mul(3);
            for (ch, (t, pv)) in target.iter().zip(p).enumerate() {
                let e = (t - f64::from(pv)) * amount;
                cur[k.wrapping_add(3).wrapping_add(ch)] += e * 7.0 / 16.0;
                next[k.wrapping_sub(3).wrapping_add(ch)] += e * 3.0 / 16.0;
                next[k.wrapping_add(ch)] += e * 5.0 / 16.0;
                next[k.wrapping_add(3).wrapping_add(ch)] += e / 16.0;
            }
        }
    }
    Ok(IndexedImage { width, height, indices, table, options: options.clone() })
}
