//! Content-aware fill, patch, move, inpaint and scale ([`scale`]) (lanes SMB-PIGMENT-CONTENT-AWARE
//! and SMB-PIGMENT-CONTENT-AWARE-2, Photoshop parity gap PS-18;
//! spec basis STU-RAS-143 "content-aware fill ... MUST have a native on-device implementation",
//! STU-RAS-030, STU-RAS-026 rows Patch and Content-aware move). Pure compute over straight-RGBA
//! planes ([`RgbaPlane`]); the hole is a selection plane ([`CoveragePlane`], pixel in the hole when
//! its coverage is above 0). No model, no provider: an exemplar-based patch synthesizer written
//! from the published algorithms (Wexler, Shechtman, Irani 2007 "Space-Time Completion of Video",
//! global patch-coherence EM with weighted voting; Barnes, Shechtman, Finkelstein, Goldman 2009
//! "PatchMatch", randomized nearest-neighbour field with propagation and random search). No code
//! from any implementation was read or copied.
//!
//! Synthesis ([`fill`]):
//! 1. Pyramid. Level 0 is the target (premultiplied RGBA, binary32). Level `l + 1` halves level
//!    `l` (ceil): a coarse pixel is in the hole when any of its children is, may be sampled when
//!    all of its children may, and its colour is the mean of its non-hole children. Levels are
//!    added while the hole's bounding box at the current level is wider or taller than one patch,
//!    the next level is at least two patches wide and tall, it has at least as many valid source
//!    centres as domain centres (step 2; a coarse level with too few exemplars would steer the
//!    finer levels into a wrong texture), and fewer than `max_levels` exist.
//! 2. Patches are `(2 r + 1)^2` squares (`patch_radius = r`). A SOURCE patch centre is valid when
//!    the whole patch lies inside the plane and every pixel of it may be sampled (outside the
//!    hole, inside the sampling area, outside any exclusion). The DOMAIN is every centre whose
//!    patch overlaps the hole (the hole dilated by `r`, Chebyshev); its nearest-neighbour field
//!    (NNF) maps each domain centre to a valid source centre. Patch distance = sum of squared
//!    premultiplied RGBA differences over the patch pixels inside the plane (binary64).
//!    Search adaptations (lane SMB-PIGMENT-CONTENT-AWARE-2, after Darabi, Shechtman, Barnes,
//!    Goldman, Sen 2012 "Image Melding" and Barnes, Shechtman, Goldman, Finkelstein 2010 "The
//!    Generalized PatchMatch Correspondence Algorithm"): a match also carries a transform
//!    `M = s R(theta) diag(mirror ? -1 : 1, 1)` (`theta` in [`ANGLE_STEP_DEG`] steps up to
//!    [`RotationAdaptation::max_degrees`], `s = 2^(k/8)` for `|k| <= `[`SCALE_STEPS`] when
//!    `scale`, horizontal mirror when `mirror`); a transformed source patch samples the level
//!    bilinearly at `c + M o` and is admissible only when every tap of non-zero weight may be
//!    sampled. `colour_adaptation` high / very_high fit a per-channel gain and bias
//!    ([`ColourAdaptation::gain_bias`]: `g = clamp(sd_t / sd_s)`, `b = clamp(mean_t - g mean_s)`
//!    over the patch) and compare `g s + b` (clamped to `[0, alpha]`); the vote applies the same.
//!    Propagation moves the neighbour's source by its transform applied to the step (same
//!    transform); random search tries each new place with the best transform and with a perturbed
//!    one (rotation within `max >> step`, scale within `max >> step`, random mirror); transform
//!    draws use their own SplitMix64 streams, so with no adaptation the search is the plain one.
//! 3. Coarsest level: hole pixels start from an onion peel (each pass, every unknown hole pixel
//!    with a known 4-neighbour takes the mean of its known 4-neighbours as they were before the
//!    pass); the NNF starts at seeded random valid sources. Finer levels: hole pixels start from
//!    the parent's estimate (nearest) and the NNF from the parent's match doubled (plus the child
//!    parity through the match's transform) when that is still admissible, else a seeded random
//!    valid source.
//! 4. Each level runs `em_iterations` of: recompute distances; `search_iterations` PatchMatch
//!    sweeps; vote. A sweep is JACOBI (every centre reads only the previous sweep's field):
//!    candidates are the current match, the propagated matches of the 4-neighbours at distance 1
//!    and at the jump distance `2^min(4, S-1-s)` (sweep `s` of `S`), and random search around the
//!    best match at radii `max(w, h)` (guided: the structure window), halved down to 1, with
//!    offsets drawn from SplitMix64 of
//!    `(seed, level, EM iteration, sweep, centre, draw)`; a candidate replaces the match only when
//!    strictly closer. Vote: each hole pixel becomes the weighted mean, over every domain patch
//!    covering it, of the pixel its match puts there, weight `exp(-e / (2 sigma^2))` with `e` the
//!    patch's mean squared difference per pixel channel and `sigma` = [`VOTE_SIGMA`] (uniform mean
//!    if the weights vanish); the LAST vote of level 0 instead takes the pixel proposed by the
//!    covering patch of least mean error (first in scan order on ties), so the final texture is a
//!    copy of exemplar pixels, not an average that blurs it. Votes read only source pixels, never
//!    hole pixels (test `fill_continues_stripes_and_checker_with_a_quality_receipt`: the hole's
//!    own red content never reaches the fill).
//! 5. Blend (`colour_adaptation`): `none` writes `d + c (f - d)` (premultiplied, `c` the hole
//!    coverage, `f` the synthesized value); `default` runs the healing solver of
//!    [`crate::retouch::heal`] (membrane Poisson, Perez/Gangnet/Blake 2003) with the guide = the
//!    synthesized pixels in the hole and the votes of the NNF on the hole's 4-neighbour ring, so
//!    the fill keeps its synthesized texture and takes the surrounding colour at the seam.
//!
//! Determinism: every per-centre and per-pixel update is a pure function of the previous state and
//! the seed, so the work split over `threads` scoped threads (contiguous chunks) never changes the
//! bytes (test `content_aware.rs` `fill_is_thread_count_invariant`); the onion peel, the pyramid
//! and the Poisson solve are single-threaded. Same binary, same inputs, same seed: same bytes.
//!
//! Bounds (V4-01/02): option ranges are checked (never clamped); the hole pixel count is checked
//! against `max_hole_pixels`; the working bytes of the whole pyramid (upper bound: every level's
//! pixels at [`LEVEL_BYTES_PER_PIXEL`] plus at most `min(pixels, hole pixels x patch area)` domain
//! centres at [`CENTRE_BYTES`], plus the output plane) are charged against
//! [`RetouchLimits::max_bytes`] before any allocation; the work (patch-pixel visits of every
//! sweep, recompute and vote, times the adaptations' visit factor) is computed with checked
//! arithmetic and charged against [`RetouchLimits::max_work`] before synthesis starts. Typed
//! refusals, never partial results (tests `hostile_fill_inputs_are_typed_refusals`,
//! `hostile_patch_and_move_inputs_are_typed_refusals`, `content_aware_adapt.rs`
//! `hostile_adaptation_inputs_are_typed_refusals`).
//!
//! Spec controls (STU-RAS-143) implemented: `sampling_area_mode` auto | rectangular | custom,
//! `colour_adaptation` none | default | high | very_high, `rotation_adaptation` none | low |
//! medium | high | full, `scale`, `mirror` (tests in `content_aware_adapt.rs`; the healing
//! [`PatchMode::Normal`] has no search and refuses the search adaptations
//! `content_aware_option_unsupported`). Not modelled here (caller or later lanes): the overlay,
//! `sample_all_layers` (the caller flattens), `additional_document_source`, output brightness /
//! contrast / colour balance, `output_to`. Every reading is a Studio reading, class Approximate;
//! Photoshop's content-aware algorithms and results are UNVERIFIED (oracle list D5).
#![deny(clippy::arithmetic_side_effects)]
use crate::render_plan::SampleRange;
use crate::retouch::straight_in;
use crate::blend_math::Exactness;
use crate::filter_math::splitmix;
use crate::retouch::{
    CloneMap, HealReceipt, RGBA_SAMPLE_BYTES, RetouchError, RetouchLimits, RetouchResult, RgbaPlane,
    copy_plane, heal_with_coverage, heal_with_guide, paint_over, premul, straight,
};
use crate::transform_math::ResampleFilter;
use crate::selection::CoveragePlane;
use crate::{Error, Rect};
use serde::Serialize;

pub mod scale;
pub use scale::{ScaleReceipt, ScaleResult, content_aware_scale};

/// Class of every content-aware operator (Studio readings; Photoshop parity UNVERIFIED, D5).
pub const CONTENT_AWARE_EXACTNESS: Exactness = Exactness::Approximate;
/// Voting weight scale: `exp(-e / (2 VOTE_SIGMA^2))` with `e` the mean squared difference per
/// patch pixel channel (premultiplied units).
pub const VOTE_SIGMA: f64 = 0.1;
/// A custom sampling plane marks a pixel as sampled when its coverage is at least this (one half).
pub const SAMPLING_THRESHOLD: u16 = 32768;
/// Conservative working bytes per pyramid pixel (image 16, three masks, source list, slot map,
/// domain and hole lists, integral image, domain scratch, onion-peel state), charged before
/// allocation.
pub const LEVEL_BYTES_PER_PIXEL: u64 = 64;
/// Conservative working bytes per possible domain centre (nearest-neighbour field entry 32 and
/// distance 8, sweep output 40, vote 16), charged before allocation.
pub const CENTRE_BYTES: u64 = 96;
/// Largest jump exponent of a Jacobi sweep's long-range propagation (jump `2^4 = 16`).
const MAX_JUMP_SHIFT: u32 = 4;
const NONE: u32 = u32::MAX;

/// STU-RAS-143 `colour_adaptation` (closed set).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ColourAdaptation {
    None,
    #[default]
    Default,
    High,
    VeryHigh,
}
impl ColourAdaptation {
    pub const fn key(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Default => "default",
            Self::High => "high",
            Self::VeryHigh => "very_high",
        }
    }
    /// The gain/bias-adapted patch distance of `high` and `very_high` (Darabi, Shechtman, Barnes,
    /// Goldman, Sen 2012 "Image Melding", per-channel gain and bias): `(gain_min, gain_max,
    /// bias_max)`; `high` = gain `0.9 ..= 1/0.9`, bias `|b| <= 0.1`; `very_high` = gain
    /// `0.75 ..= 1/0.75`, bias `|b| <= 0.25` (Studio readings); `None` for `none` and `default`.
    pub const fn gain_bias(self) -> Option<(f64, f64, f64)> {
        match self {
            Self::None | Self::Default => None,
            Self::High => Some((0.9, 1.0 / 0.9, 0.1)),
            Self::VeryHigh => Some((0.75, 1.0 / 0.75, 0.25)),
        }
    }
}

/// Rotation adaptation step of the transformed-patch search (degrees).
pub const ANGLE_STEP_DEG: f64 = 5.0;
/// Scale adaptation: patch scale factors `2^(k / 8)` for `k` in `-SCALE_STEPS ..= SCALE_STEPS`
/// (0.771 ..= 1.297).
pub const SCALE_STEPS: i8 = 3;
/// Largest patch radius of [`ContentAwareOptions::patch_radius`] (patch `17 x 17`).
pub const MAX_PATCH_RADIUS: u32 = 8;

/// STU-RAS-143 `rotation_adaptation` (closed set).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RotationAdaptation {
    #[default]
    None,
    Low,
    Medium,
    High,
    Full,
}
impl RotationAdaptation {
    pub const fn key(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Full => "full",
        }
    }
    /// Largest rotation the search may give a source patch (degrees, either sign): none 0, low
    /// 15, medium 45, high 90, full 180 (Studio readings), in steps of [`ANGLE_STEP_DEG`].
    pub const fn max_degrees(self) -> u32 {
        match self {
            Self::None => 0,
            Self::Low => 15,
            Self::Medium => 45,
            Self::High => 90,
            Self::Full => 180,
        }
    }
}

/// STU-RAS-143 `sampling_area_mode`: `auto` samples every pixel outside the hole; `rectangular`
/// only pixels inside the rectangle; `custom` only pixels whose coverage is at least
/// [`SAMPLING_THRESHOLD`]. The hole is never sampled (test
/// `fill_continues_stripes_and_checker_with_a_quality_receipt`).
#[derive(Clone, Copy, Debug, Default)]
pub enum SamplingArea<'a> {
    #[default]
    Auto,
    Rectangular(Rect),
    Custom(&'a CoveragePlane),
}
impl SamplingArea<'_> {
    pub const fn key(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Rectangular(_) => "rectangular",
            Self::Custom(_) => "custom",
        }
    }
    fn allows(&self, x: i64, y: i64) -> bool {
        match self {
            Self::Auto => true,
            Self::Rectangular(r) => {
                r.end().is_ok_and(|(ex, ey)| x >= r.x && y >= r.y && x < ex && y < ey)
            }
            Self::Custom(c) => c.at(x, y) >= SAMPLING_THRESHOLD,
        }
    }
}

/// Synthesis options (ranges checked by [`ContentAwareOptions::validate`], never clamped).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ContentAwareOptions {
    /// Patch radius `r` (patch `(2r+1)^2`), `1 ..= 8`; default 3 (7 x 7).
    pub patch_radius: u32,
    /// EM iterations per pyramid level, `1 ..= 64`; default 4.
    pub em_iterations: u32,
    /// PatchMatch sweeps per EM iteration, `1 ..= 32`; default 4.
    pub search_iterations: u32,
    /// Pyramid levels at most, `1 ..= 16`; default 8.
    pub max_levels: u32,
    /// Random seed (fixed seed = fixed bytes).
    pub seed: u64,
    /// Worker threads, `1 ..= 64`; the bytes do not depend on it, so it is not serialized (a plan
    /// digest does not change with the thread count).
    #[serde(skip)]
    pub threads: u32,
    /// Hole pixels at most (else `content_aware_hole_too_large`); default `1 << 20`.
    pub max_hole_pixels: u64,
    pub colour_adaptation: ColourAdaptation,
    pub rotation_adaptation: RotationAdaptation,
    pub scale: bool,
    pub mirror: bool,
}
impl Default for ContentAwareOptions {
    fn default() -> Self {
        Self {
            patch_radius: 3,
            em_iterations: 4,
            search_iterations: 4,
            max_levels: 8,
            seed: 0,
            threads: 1,
            max_hole_pixels: 1 << 20,
            colour_adaptation: ColourAdaptation::Default,
            rotation_adaptation: RotationAdaptation::None,
            scale: false,
            mirror: false,
        }
    }
}
impl ContentAwareOptions {
    /// Range check (never clamped). Every STU-RAS-143 adaptation value is implemented by the
    /// synthesis; [`patch`] in `PatchMode::Normal` (no search) refuses the search adaptations.
    pub fn validate(&self) -> Result<(), RetouchError> {
        let ok = (1..=MAX_PATCH_RADIUS).contains(&self.patch_radius)
            && (1..=64).contains(&self.em_iterations)
            && (1..=32).contains(&self.search_iterations)
            && (1..=16).contains(&self.max_levels)
            && (1..=64).contains(&self.threads);
        if !ok {
            return Err(RetouchError::OutOfRange);
        }
        Ok(())
    }
    /// The search uses an adaptation beyond the plain patch distance (gain/bias, rotation, scale
    /// or mirror).
    pub fn adapts_search(&self) -> bool {
        self.colour_adaptation.gain_bias().is_some()
            || self.rotation_adaptation != RotationAdaptation::None
            || self.scale
            || self.mirror
    }
}

/// The search adaptations of one synthesis (from [`ContentAwareOptions`]).
#[derive(Clone, Copy, Debug)]
struct Adapt {
    /// `(gain_min, gain_max, bias_max)` of the gain/bias-adapted distance.
    colour: Option<(f64, f64, f64)>,
    /// Largest rotation in [`ANGLE_STEP_DEG`] steps.
    max_angle: i8,
    /// Largest scale step (0 or [`SCALE_STEPS`]).
    max_scale: i8,
    mirror: bool,
}
impl Adapt {
    fn new(o: &ContentAwareOptions) -> Self {
        let deg = o.rotation_adaptation.max_degrees();
        Self {
            colour: o.colour_adaptation.gain_bias(),
            max_angle: i8::try_from(deg.checked_div(ANGLE_STEP_DEG as u32).unwrap_or(0)).unwrap_or(0),
            max_scale: if o.scale { SCALE_STEPS } else { 0 },
            mirror: o.mirror,
        }
    }
    fn transforms(&self) -> bool {
        self.max_angle > 0 || self.max_scale > 0 || self.mirror
    }
    /// Patch-pixel visit multiplier of one candidate evaluation (statistics pass, bilinear taps).
    fn cost_factor(&self) -> u64 {
        match (self.colour.is_some(), self.transforms()) {
            (false, false) => 1,
            (true, false) => 2,
            (false, true) => 4,
            (true, true) => 8,
        }
    }
}

/// A nearest-neighbour field entry: source centre, transform (rotation `angle` x
/// [`ANGLE_STEP_DEG`], scale `2^(scale / 8)`, horizontal mirror before rotation) and the
/// per-channel gain and bias of the adapted distance (1 and 0 when not adapted).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Match {
    src: u32,
    angle: i8,
    scale: i8,
    mirror: bool,
    gain: [f32; 3],
    bias: [f32; 3],
}
impl Match {
    const fn at(src: u32) -> Self {
        Self { src, angle: 0, scale: 0, mirror: false, gain: [1.0; 3], bias: [0.0; 3] }
    }
    fn identity(&self) -> bool {
        self.angle == 0 && self.scale == 0 && !self.mirror
    }
    fn same_place(&self, o: &Self) -> bool {
        self.src == o.src && self.angle == o.angle && self.scale == o.scale && self.mirror == o.mirror
    }
    /// Offset map `o -> M o` (row-major `[a, b, c, d]`): `M = s R(theta) diag(m, 1)`.
    fn matrix(&self) -> [f64; 4] {
        let th = f64::from(self.angle) * ANGLE_STEP_DEG * std::f64::consts::PI / 180.0;
        let s = (f64::from(self.scale) / 8.0).exp2();
        let (sn, cs) = th.sin_cos();
        let m = if self.mirror { -1.0 } else { 1.0 };
        [s * cs * m, -s * sn, s * sn * m, s * cs]
    }
    /// The stored gain and bias applied to a premultiplied source sample (colour channels
    /// clamped to `[0, alpha]`).
    fn adjust(&self, v: [f64; 4]) -> [f64; 4] {
        let mut o = v;
        for k in 0..3 {
            o[k] = (f64::from(self.gain[k]) * v[k] + f64::from(self.bias[k])).clamp(0.0, v[3].max(0.0));
        }
        o
    }
}

/// One pyramid level of a synthesis (`energies[i]` = mean squared difference per patch pixel
/// channel over the domain after the searches of EM iteration `i`).
#[derive(Clone, Debug, PartialEq)]
pub struct LevelReceipt {
    pub width: u32,
    pub height: u32,
    pub hole_pixels: u64,
    pub domain_centres: u64,
    pub source_centres: u64,
    pub em_iterations: u32,
    pub search_iterations: u32,
    pub energies: Vec<f64>,
}

/// Quality receipt of a synthesis (no silent partial result).
#[derive(Clone, Debug, PartialEq)]
pub struct ContentAwareReceipt {
    pub seed: u64,
    pub patch_radius: u32,
    /// Coarsest first, level 0 last; empty when nothing was synthesized.
    pub levels: Vec<LevelReceipt>,
    /// EM iterations run over all levels.
    pub iterations: u32,
    /// Level 0's last energy (0 when nothing was synthesized).
    pub final_energy: f64,
    /// Work charged against [`RetouchLimits::max_work`] (patch-pixel visits).
    pub work: u64,
    /// Patch guidance was used (content-aware patch).
    pub guided: bool,
    /// The healing blend's receipt (`colour_adaptation = default`, patch normal, move paste).
    pub blend: Option<HealReceipt>,
}
impl ContentAwareReceipt {
    fn empty(options: &ContentAwareOptions, guided: bool) -> Self {
        Self {
            seed: options.seed,
            patch_radius: options.patch_radius,
            levels: Vec::new(),
            iterations: 0,
            final_energy: 0.0,
            work: 0,
            guided,
            blend: None,
        }
    }
}

/// Result of a content-aware operator.
#[derive(Clone, Debug, PartialEq)]
pub struct ContentAwareResult {
    pub result: RetouchResult,
    pub receipt: ContentAwareReceipt,
}

/// Patch guidance: domain centre `q` prefers the source `q + offset`; random search stays within
/// `window` pixels (level 0) of the best match.
#[derive(Clone, Copy, Debug)]
struct Guide {
    offset: (i64, i64),
    window: i64,
}

struct Level {
    w: usize,
    h: usize,
    img: Vec<[f32; 4]>,
    hole: Vec<bool>,
    allowed: Vec<bool>,
    valid: Vec<bool>,
    sources: Vec<u32>,
    slot: Vec<u32>,
    domain: Vec<u32>,
    holes: Vec<u32>,
}

#[inline]
fn ix(w: usize, x: usize, y: usize) -> usize {
    // Coordinates are inside an allocated level, so the index is below its length.
    y.wrapping_mul(w).wrapping_add(x)
}

fn coverage_validate(c: &CoveragePlane) -> Result<(), RetouchError> {
    c.bounds.end()?;
    let n = usize::try_from(c.bounds.area()).map_err(|_| Error::Overflow)?;
    if c.data.len() != n {
        return Err(RetouchError::Shape);
    }
    Ok(())
}

/// Dimensions of the pyramid levels at most `max_levels` deep (no allocation).
fn pyramid_dims(w: usize, h: usize, max_levels: u32) -> Vec<(usize, usize)> {
    let mut dims = vec![(w, h)];
    let (mut cw, mut ch) = (w, h);
    while u32::try_from(dims.len()).is_ok_and(|n| n < max_levels) && (cw > 1 || ch > 1) {
        cw = cw.div_ceil(2);
        ch = ch.div_ceil(2);
        dims.push((cw, ch));
    }
    dims
}

impl Level {
    fn compute_valid(&mut self, r: usize) -> Result<(), Error> {
        let (w, h) = (self.w, self.h);
        let w1 = w.checked_add(1).ok_or(Error::Overflow)?;
        let h1 = h.checked_add(1).ok_or(Error::Overflow)?;
        let mut sat = vec![0u64; w1.checked_mul(h1).ok_or(Error::Overflow)?];
        for y in 0..h {
            let mut row = 0u64;
            for x in 0..w {
                if !self.allowed.get(ix(w, x, y)).copied().unwrap_or(false) {
                    row = row.wrapping_add(1);
                }
                let above = sat.get(ix(w1, x.wrapping_add(1), y)).copied().unwrap_or(0);
                if let Some(v) = sat.get_mut(ix(w1, x.wrapping_add(1), y.wrapping_add(1))) {
                    *v = above.wrapping_add(row);
                }
            }
        }
        self.valid = vec![false; self.img.len()];
        self.sources.clear();
        let pw = r.checked_mul(2).and_then(|v| v.checked_add(1)).ok_or(Error::Overflow)?;
        if w < pw || h < pw {
            return Ok(());
        }
        for y in r..h.wrapping_sub(r) {
            for x in r..w.wrapping_sub(r) {
                let (x0, y0) = (x.wrapping_sub(r), y.wrapping_sub(r));
                let (x1, y1) = (x.wrapping_add(r).wrapping_add(1), y.wrapping_add(r).wrapping_add(1));
                let s = |xx: usize, yy: usize| sat.get(ix(w1, xx, yy)).copied().unwrap_or(0);
                let bad = s(x1, y1).wrapping_sub(s(x1, y0)).wrapping_sub(s(x0, y1)).wrapping_add(s(x0, y0));
                if bad == 0 {
                    let i = ix(w, x, y);
                    if let Some(v) = self.valid.get_mut(i) {
                        *v = true;
                    }
                    self.sources.push(u32::try_from(i).map_err(|_| Error::Overflow)?);
                }
            }
        }
        Ok(())
    }

    fn compute_domain(&mut self, r: usize) -> Result<(), Error> {
        let (w, h) = (self.w, self.h);
        let n = self.img.len();
        let mut rows = vec![false; n];
        let mut dom = vec![false; n];
        self.holes.clear();
        for y in 0..h {
            for x in 0..w {
                if self.hole.get(ix(w, x, y)).copied().unwrap_or(false) {
                    self.holes.push(u32::try_from(ix(w, x, y)).map_err(|_| Error::Overflow)?);
                    let hi = x.saturating_add(r).min(w.saturating_sub(1));
                    for xx in x.saturating_sub(r)..=hi {
                        if let Some(v) = rows.get_mut(ix(w, xx, y)) {
                            *v = true;
                        }
                    }
                }
            }
        }
        for y in 0..h {
            for x in 0..w {
                if rows.get(ix(w, x, y)).copied().unwrap_or(false) {
                    let hi = y.saturating_add(r).min(h.saturating_sub(1));
                    for yy in y.saturating_sub(r)..=hi {
                        if let Some(v) = dom.get_mut(ix(w, x, yy)) {
                            *v = true;
                        }
                    }
                }
            }
        }
        self.slot = vec![NONE; n];
        self.domain.clear();
        for (i, d) in dom.iter().enumerate() {
            if *d {
                let k = u32::try_from(self.domain.len()).map_err(|_| Error::Overflow)?;
                if let Some(s) = self.slot.get_mut(i) {
                    *s = k;
                }
                self.domain.push(u32::try_from(i).map_err(|_| Error::Overflow)?);
            }
        }
        Ok(())
    }

    fn downsample(&self) -> Result<Level, Error> {
        let (nw, nh) = (self.w.div_ceil(2), self.h.div_ceil(2));
        let n = nw.checked_mul(nh).ok_or(Error::Overflow)?;
        let mut img = Vec::with_capacity(n);
        let mut hole = Vec::with_capacity(n);
        let mut allowed = Vec::with_capacity(n);
        for yc in 0..nh {
            for xc in 0..nw {
                let (mut any_hole, mut all_allowed) = (false, true);
                let mut sum = [0.0f64; 4];
                let mut cnt = 0u32;
                for (dx, dy) in [(0usize, 0usize), (1, 0), (0, 1), (1, 1)] {
                    let (x, y) = (xc.wrapping_mul(2).wrapping_add(dx), yc.wrapping_mul(2).wrapping_add(dy));
                    if x >= self.w || y >= self.h {
                        continue;
                    }
                    let i = ix(self.w, x, y);
                    let hl = self.hole.get(i).copied().unwrap_or(true);
                    any_hole |= hl;
                    all_allowed &= self.allowed.get(i).copied().unwrap_or(false);
                    if !hl {
                        let p = self.img.get(i).copied().unwrap_or([0.0; 4]);
                        for (s, v) in sum.iter_mut().zip(p) {
                            *s += f64::from(v);
                        }
                        cnt = cnt.saturating_add(1);
                    }
                }
                let mut px = [0.0f32; 4];
                if cnt > 0 {
                    for (o, s) in px.iter_mut().zip(sum) {
                        *o = (s / f64::from(cnt)) as f32;
                    }
                }
                img.push(px);
                hole.push(any_hole);
                allowed.push(all_allowed && !any_hole);
            }
        }
        Ok(Level {
            w: nw,
            h: nh,
            img,
            hole,
            allowed,
            valid: Vec::new(),
            sources: Vec::new(),
            slot: Vec::new(),
            domain: Vec::new(),
            holes: Vec::new(),
        })
    }

    fn hole_extent(&self) -> usize {
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
        for &i in &self.holes {
            let i = i as usize;
            let (x, y) = (i.checked_rem(self.w).unwrap_or(0), i.checked_div(self.w).unwrap_or(0));
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        if self.holes.is_empty() {
            return 0;
        }
        x1.wrapping_sub(x0).max(y1.wrapping_sub(y0)).wrapping_add(1)
    }

    #[inline]
    fn xy(&self, i: u32) -> (i64, i64) {
        let i = i as usize;
        let x = i.checked_rem(self.w).unwrap_or(0);
        let y = i.checked_div(self.w).unwrap_or(0);
        (x as i64, y as i64)
    }

    #[inline]
    fn index(&self, x: i64, y: i64) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 {
            return None;
        }
        Some(ix(self.w, x as usize, y as usize))
    }

    #[inline]
    fn valid_at(&self, x: i64, y: i64) -> Option<u32> {
        let i = self.index(x, y)?;
        if self.valid.get(i).copied().unwrap_or(false) { u32::try_from(i).ok() } else { None }
    }

    #[inline]
    fn px(&self, i: usize) -> [f32; 4] {
        self.img.get(i).copied().unwrap_or([0.0; 4])
    }

    /// Patch pixels of centre `q` inside the plane.
    fn patch_count(&self, r: i64, q: (i64, i64)) -> f64 {
        let span = |c: i64, n: usize| -> i64 {
            let lo = c.wrapping_sub(r).max(0);
            let hi = c.wrapping_add(r).min((n as i64).wrapping_sub(1));
            hi.wrapping_sub(lo).wrapping_add(1).max(0)
        };
        (span(q.0, self.w) as f64) * (span(q.1, self.h) as f64)
    }

    /// Squared premultiplied RGBA difference between the patch at `q` (pixels inside the plane)
    /// and the patch at source `s` (a valid source centre, so its patch is inside the plane);
    /// stops early once at least `cap`.
    fn dist(&self, r: i64, q: (i64, i64), s: (i64, i64), cap: f64) -> f64 {
        let mut sum = 0.0f64;
        for oy in r.wrapping_neg()..=r {
            let ty = q.1.wrapping_add(oy);
            if ty < 0 || ty >= self.h as i64 {
                continue;
            }
            let sy = s.1.wrapping_add(oy);
            for ox in r.wrapping_neg()..=r {
                let tx = q.0.wrapping_add(ox);
                if tx < 0 || tx >= self.w as i64 {
                    continue;
                }
                let a = self.px(ix(self.w, tx as usize, ty as usize));
                let b = self.px(ix(self.w, s.0.wrapping_add(ox) as usize, sy as usize));
                for c in 0..4 {
                    let d = f64::from(a[c]) - f64::from(b[c]);
                    sum += d * d;
                }
            }
            if sum >= cap {
                return sum;
            }
        }
        sum
    }

    /// Bilinear premultiplied sample at `(px, py)` (pixel centres at integers); `None` when a tap
    /// of non-zero weight leaves the plane or may not be sampled (so the hole is never read).
    fn sample(&self, px: f64, py: f64) -> Option<[f64; 4]> {
        if !(px.is_finite() && py.is_finite()) {
            return None;
        }
        let (x0, y0) = (px.floor(), py.floor());
        let (fx, fy) = (px - x0, py - y0);
        let (xi, yi) = (x0 as i64, y0 as i64);
        let mut out = [0.0f64; 4];
        for (dx, dy, wt) in [
            (0i64, 0i64, (1.0 - fx) * (1.0 - fy)),
            (1, 0, fx * (1.0 - fy)),
            (0, 1, (1.0 - fx) * fy),
            (1, 1, fx * fy),
        ] {
            if wt == 0.0 {
                continue;
            }
            let i = self.index(xi.wrapping_add(dx), yi.wrapping_add(dy))?;
            if !self.allowed.get(i).copied().unwrap_or(false) {
                return None;
            }
            let p = self.px(i);
            for c in 0..4 {
                out[c] += wt * f64::from(p[c]);
            }
        }
        Some(out)
    }

    /// The (unadjusted) source sample of match `m` at patch offset `(ox, oy)`; `mat` =
    /// `m.matrix()`. Identity matches read the exemplar pixel (their centre is a valid source).
    fn source_px(&self, m: &Match, mat: &[f64; 4], ox: i64, oy: i64) -> Option<[f64; 4]> {
        let (cx, cy) = self.xy(m.src);
        if m.identity() {
            let i = self.index(cx.wrapping_add(ox), cy.wrapping_add(oy))?;
            return Some(f32x4_to_f64(self.px(i)));
        }
        let (u, v) = (ox as f64, oy as f64);
        self.sample(cx as f64 + mat[0] * u + mat[1] * v, cy as f64 + mat[2] * u + mat[3] * v)
    }

    /// Distance of domain centre `q` to candidate `m` with the search adaptations (the gain and
    /// bias are fitted here and returned in the match); `None` when the candidate may not be
    /// sampled (an identity centre that is not a valid source, or a transformed tap outside the
    /// sampling area). Stops early once at least `cap`.
    fn cost(&self, r: i64, q: (i64, i64), m: Match, ad: &Adapt, cap: f64) -> Option<(f64, Match)> {
        if m.identity() && !self.valid.get(m.src as usize).copied().unwrap_or(false) {
            return None;
        }
        let plain = Match { gain: [1.0; 3], bias: [0.0; 3], ..m };
        if m.identity() && ad.colour.is_none() {
            return Some((self.dist(r, q, self.xy(m.src), cap), plain));
        }
        let mat = m.matrix();
        // Pass 1 (gain/bias): per-channel means and variances of the target and source patches
        // over the patch pixels inside the plane (Image Melding: g = clamp(sd_t / sd_s), b =
        // clamp(mean_t - g mean_s)); any unsampleable tap refuses the candidate here.
        let fitted = match ad.colour {
            None => plain,
            Some((gmin, gmax, bmax)) => {
                let mut acc = [[0.0f64; 4]; 3];
                let mut n = 0.0f64;
                self.each_pair(r, q, &m, &mat, |s, t| {
                    for (a, c) in acc.iter_mut().zip(0..3) {
                        a[0] += s[c];
                        a[1] += s[c] * s[c];
                        a[2] += t[c];
                        a[3] += t[c] * t[c];
                    }
                    n += 1.0;
                    true
                })?;
                let cnt = n.max(1.0);
                let (mut gain, mut bias) = ([1.0f32; 3], [0.0f32; 3]);
                for (c, a) in acc.iter().enumerate() {
                    let (ms, mt) = (a[0] / cnt, a[2] / cnt);
                    let vs = (a[1] / cnt - ms * ms).max(0.0);
                    let vt = (a[3] / cnt - mt * mt).max(0.0);
                    let g = if vs > 1e-12 { (vt / vs).sqrt() } else { 1.0 };
                    let g = g.clamp(gmin, gmax) as f32;
                    gain[c] = g;
                    bias[c] = (mt - f64::from(g) * ms).clamp(-bmax, bmax) as f32;
                }
                Match { gain, bias, ..m }
            }
        };
        // Pass 2: squared difference of the (adjusted) source and the target, early stop at cap.
        let mut sum = 0.0f64;
        let colour = ad.colour.is_some();
        self.each_pair(r, q, &m, &mat, |s, t| {
            let s = if colour { fitted.adjust(s) } else { s };
            for c in 0..4 {
                let d = s[c] - t[c];
                sum += d * d;
            }
            sum < cap
        })?;
        Some((sum, fitted))
    }

    /// Calls `f(source, target)` for every patch offset of `q` inside the plane (row-major) until it
    /// returns false; `None` when a source tap may not be sampled.
    fn each_pair(
        &self,
        r: i64,
        q: (i64, i64),
        m: &Match,
        mat: &[f64; 4],
        mut f: impl FnMut([f64; 4], [f64; 4]) -> bool,
    ) -> Option<()> {
        for oy in r.wrapping_neg()..=r {
            let ty = q.1.wrapping_add(oy);
            if ty < 0 || ty >= self.h as i64 {
                continue;
            }
            for ox in r.wrapping_neg()..=r {
                let tx = q.0.wrapping_add(ox);
                if tx < 0 || tx >= self.w as i64 {
                    continue;
                }
                let s = self.source_px(m, mat, ox, oy)?;
                let t = f32x4_to_f64(self.px(ix(self.w, tx as usize, ty as usize)));
                if !f(s, t) {
                    return Some(());
                }
            }
        }
        Some(())
    }

    /// Vote of pixel `p` from every domain patch covering it: the weighted mean, or with `best` the
    /// pixel proposed by the covering patch of least mean error (first in scan order on ties).
    fn vote(&self, r: i64, nnf: &[Match], dist: &[f64], ad: &Adapt, p: (i64, i64), best: bool) -> [f32; 4] {
        let mut sum = [0.0f64; 4];
        let mut best_v: Option<(f64, [f32; 4])> = None;
        let mut usum = [0.0f64; 4];
        let (mut wsum, mut cnt) = (0.0f64, 0.0f64);
        let two_s2 = 2.0 * VOTE_SIGMA * VOTE_SIGMA;
        for oy in r.wrapping_neg()..=r {
            for ox in r.wrapping_neg()..=r {
                let q = (p.0.wrapping_sub(ox), p.1.wrapping_sub(oy));
                let Some(qi) = self.index(q.0, q.1) else { continue };
                let k = self.slot.get(qi).copied().unwrap_or(NONE);
                if k == NONE {
                    continue;
                }
                let k = k as usize;
                let Some(&m) = nnf.get(k) else { continue };
                let v = if m.identity() && ad.colour.is_none() {
                    let (sx, sy) = self.xy(m.src);
                    let Some(si) = self.index(sx.wrapping_add(ox), sy.wrapping_add(oy)) else { continue };
                    self.px(si)
                } else {
                    let Some(s) = self.source_px(&m, &m.matrix(), ox, oy) else { continue };
                    let s = if ad.colour.is_some() { m.adjust(s) } else { s };
                    [s[0] as f32, s[1] as f32, s[2] as f32, s[3] as f32]
                };
                let e = dist.get(k).copied().unwrap_or(0.0) / (4.0 * self.patch_count(r, q)).max(1.0);
                if best_v.is_none_or(|(be, _)| e < be) {
                    best_v = Some((e, v));
                }
                let wt = (-e / two_s2).exp();
                for c in 0..4 {
                    sum[c] += wt * f64::from(v[c]);
                    usum[c] += f64::from(v[c]);
                }
                wsum += wt;
                cnt += 1.0;
            }
        }
        let mut out = [0.0f32; 4];
        if best && let Some((_, v)) = best_v {
            return v;
        }
        if wsum > 0.0 && wsum.is_finite() {
            for c in 0..4 {
                out[c] = (sum[c] / wsum) as f32;
            }
        } else if cnt > 0.0 {
            for c in 0..4 {
                out[c] = (usum[c] / cnt) as f32;
            }
        }
        out
    }
}

fn rng(seed: u64, level: usize, em: u32, sweep: u32, q: u32, draw: u32) -> u64 {
    let a = splitmix(seed ^ splitmix(level as u64));
    let b = splitmix(a ^ (u64::from(em) << 32 | u64::from(sweep)));
    splitmix(b ^ splitmix(u64::from(q) << 32 | u64::from(draw)))
}

fn random_source(l: &Level, seed: u64, level: usize, q: u32) -> u32 {
    let n = l.sources.len() as u64;
    let h = rng(seed, level, u32::MAX, u32::MAX, q, 0);
    let k = h.checked_rem(n).unwrap_or(0) as usize;
    l.sources.get(k).copied().unwrap_or(0)
}

/// Compute `out[k] = f(k)` for every `k`, split into contiguous chunks over `threads` scoped
/// threads; `f` is pure, so the result does not depend on `threads`.
fn par_fill<T: Send, F: Fn(usize) -> T + Sync>(out: &mut [T], threads: usize, f: F) {
    if threads <= 1 || out.len() < 2 {
        for (k, o) in out.iter_mut().enumerate() {
            *o = f(k);
        }
        return;
    }
    let chunk = out.len().div_ceil(threads).max(1);
    let f = &f;
    std::thread::scope(|scope| {
        for (c, part) in out.chunks_mut(chunk).enumerate() {
            let base = c.wrapping_mul(chunk);
            scope.spawn(move || {
                for (k, o) in part.iter_mut().enumerate() {
                    *o = f(base.wrapping_add(k));
                }
            });
        }
    });
}

struct Sweep<'a> {
    l: &'a Level,
    r: i64,
    nnf: &'a [Match],
    dist: &'a [f64],
    seed: u64,
    level: usize,
    em: u32,
    sweep: u32,
    jump: i64,
    window: i64,
    guide: Option<(i64, i64)>,
    adapt: Adapt,
}

/// Bit set on the draw index of the transform draws (separate SplitMix64 streams from the spatial
/// draws, so the identity search is unchanged by the adaptations).
const TRANSFORM_DRAW: u32 = 1 << 31;

impl Sweep<'_> {
    fn step(&self, k: usize) -> (Match, f64) {
        let l = self.l;
        let Some(&qi) = l.domain.get(k) else { return (Match::at(0), f64::INFINITY) };
        let q = l.xy(qi);
        let mut best = self.nnf.get(k).copied().unwrap_or(Match::at(0));
        let mut bd = self.dist.get(k).copied().unwrap_or(f64::INFINITY);
        let ad = self.adapt;
        // Guided (content-aware patch): a candidate must lie within the structure window of the
        // dragged source `q + offset` (Chebyshev), so propagation cannot pull the patch away.
        let in_window = |c: u32| match self.guide {
            None => true,
            Some((ox, oy)) => {
                let (cx, cy) = l.xy(c);
                let gx = q.0.wrapping_add(ox);
                let gy = q.1.wrapping_add(oy);
                cx.wrapping_sub(gx).unsigned_abs() <= self.window.unsigned_abs()
                    && cy.wrapping_sub(gy).unsigned_abs() <= self.window.unsigned_abs()
            }
        };
        let try_cand = |cand: Option<Match>, best: &mut Match, bd: &mut f64| {
            if let Some(c) = cand
                && !c.same_place(best)
                && in_window(c.src)
                && let Some((d, m)) = l.cost(self.r, q, c, &ad, *bd)
                && d < *bd
            {
                *best = m;
                *bd = d;
            }
        };
        let place = |x: i64, y: i64, like: &Match| -> Option<Match> {
            let i = u32::try_from(l.index(x, y)?).ok()?;
            Some(Match { src: i, ..*like })
        };
        let ident = Match::at(0);
        if let Some((ox, oy)) = self.guide {
            try_cand(place(q.0.wrapping_add(ox), q.1.wrapping_add(oy), &ident), &mut best, &mut bd);
        }
        let mut jumps = [1i64, 0];
        if self.jump > 1 {
            jumps[1] = self.jump;
        }
        for j in jumps {
            if j == 0 {
                continue;
            }
            for (dx, dy) in [(j.wrapping_neg(), 0), (j, 0), (0, j.wrapping_neg()), (0, j)] {
                let Some(ni) = l.index(q.0.wrapping_add(dx), q.1.wrapping_add(dy)) else { continue };
                let ns = l.slot.get(ni).copied().unwrap_or(NONE);
                if ns == NONE {
                    continue;
                }
                let Some(&mn) = self.nnf.get(ns as usize) else { continue };
                let (sx, sy) = l.xy(mn.src);
                // Generalized PatchMatch propagation (Barnes et al. 2010): the neighbour's source
                // moved back by its transform applied to the step, same transform.
                let cand = if mn.identity() {
                    place(sx.wrapping_sub(dx), sy.wrapping_sub(dy), &mn)
                } else {
                    let m = mn.matrix();
                    let (fx, fy) = (dx as f64, dy as f64);
                    let cx = (sx as f64 - (m[0] * fx + m[1] * fy)).round();
                    let cy = (sy as f64 - (m[2] * fx + m[3] * fy)).round();
                    place(cx as i64, cy as i64, &mn)
                };
                try_cand(cand, &mut best, &mut bd);
            }
        }
        let mut radius = self.window;
        let mut draw = 0u32;
        let mut step = 0u32;
        while radius >= 1 {
            let (bx, by) = l.xy(best.src);
            let span = (radius as u64).wrapping_mul(2).wrapping_add(1);
            let h1 = rng(self.seed, self.level, self.em, self.sweep, qi, draw);
            let h2 = rng(self.seed, self.level, self.em, self.sweep, qi, draw.wrapping_add(1));
            let dx = (h1.checked_rem(span).unwrap_or(0) as i64).wrapping_sub(radius);
            let dy = (h2.checked_rem(span).unwrap_or(0) as i64).wrapping_sub(radius);
            let (nx, ny) = (bx.wrapping_add(dx), by.wrapping_add(dy));
            if ad.transforms() {
                // Joint search: the new place with the best transform, then with a perturbed one.
                let keep = best;
                try_cand(place(nx, ny, &keep), &mut best, &mut bd);
                try_cand(place(nx, ny, &self.perturb(&keep, qi, draw, step)), &mut best, &mut bd);
            } else {
                try_cand(place(nx, ny, &ident), &mut best, &mut bd);
            }
            radius = radius.checked_div(2).unwrap_or(0);
            draw = draw.wrapping_add(2);
            step = step.saturating_add(1);
        }
        (best, bd)
    }

    /// Random transform around `best` for random-search step `step`: rotation by up to
    /// `max(1, max_angle >> step)` steps, scale by up to `max(1, max_scale >> step)` steps (each
    /// clamped to its adaptation range), mirror drawn uniformly when enabled.
    fn perturb(&self, best: &Match, qi: u32, draw: u32, step: u32) -> Match {
        let ad = self.adapt;
        let h = rng(self.seed, self.level, self.em, self.sweep, qi, draw | TRANSFORM_DRAW);
        let around = |v: i8, max: i8, bits: u64| -> i8 {
            if max == 0 {
                return 0;
            }
            let reach = i64::from(max).checked_shr(step).unwrap_or(0).max(1);
            let span = reach.wrapping_mul(2).wrapping_add(1) as u64;
            let d = (bits.checked_rem(span).unwrap_or(0) as i64).wrapping_sub(reach);
            let m = i64::from(max);
            i8::try_from(i64::from(v).wrapping_add(d).clamp(m.wrapping_neg(), m)).unwrap_or(0)
        };
        Match {
            src: best.src,
            angle: around(best.angle, ad.max_angle, h & 0xFFFF),
            scale: around(best.scale, ad.max_scale, (h >> 16) & 0xFFFF),
            mirror: if ad.mirror { (h >> 32) & 1 == 1 } else { false },
            gain: [1.0; 3],
            bias: [0.0; 3],
        }
    }
}

fn level_work(l: &Level, r: usize, options: &ContentAwareOptions, guided: bool, ad: &Adapt) -> Option<u64> {
    let a = (r as u64).checked_mul(2)?.checked_add(1)?;
    let a = a.checked_mul(a)?;
    // One candidate evaluation visits the patch `cost_factor` times (statistics pass, bilinear
    // taps); the transformed search tries two candidates per random draw.
    let ca = a.checked_mul(ad.cost_factor())?;
    let d = l.domain.len() as u64;
    let hcount = l.holes.len() as u64;
    let bits = u64::from(usize::BITS.checked_sub(l.w.max(l.h).leading_zeros())?);
    let draws = if ad.transforms() { bits.checked_mul(2)? } else { bits };
    let cands = 8u64.checked_add(draws)?.checked_add(u64::from(guided))?;
    let per_sweep = d.checked_mul(cands)?.checked_mul(ca)?;
    let per_em = d
        .checked_mul(ca)?
        .checked_add(per_sweep.checked_mul(u64::from(options.search_iterations))?)?
        .checked_add(hcount.checked_mul(ca)?)?;
    per_em.checked_mul(u64::from(options.em_iterations))
}

struct Synthesis {
    level0: Level,
    nnf: Vec<Match>,
    dist: Vec<f64>,
    receipt: ContentAwareReceipt,
}

#[allow(clippy::too_many_arguments)]
fn synthesize(
    target: &RgbaPlane,
    hole: &CoveragePlane,
    sampling: &SamplingArea<'_>,
    exclude: Option<&CoveragePlane>,
    guide: Option<Guide>,
    options: &ContentAwareOptions,
    limits: &RetouchLimits,
) -> Result<Option<Synthesis>, RetouchError> {
    let b = target.bounds;
    let w = b.width as usize;
    let h = b.height as usize;
    // Hole size first (no allocation).
    let mut hole_pixels = 0u64;
    for v in &hole.data {
        if *v > 0 {
            hole_pixels = hole_pixels.checked_add(1).ok_or(Error::Overflow)?;
        }
    }
    if hole_pixels > options.max_hole_pixels {
        return Err(RetouchError::ContentAwareHoleTooLarge);
    }
    if hole_pixels == 0 || w == 0 || h == 0 {
        return Ok(None);
    }
    // Working bytes of the whole pyramid (upper bound) plus the output plane, before allocation:
    // per level, every pixel and every possible domain centre (a coarse hole pixel has a distinct
    // fine hole child, so a level's domain is at most min(pixels, level-0 hole pixels x patch area)).
    let ar = u64::from(options.patch_radius).checked_mul(2).and_then(|v| v.checked_add(1)).ok_or(Error::Overflow)?;
    let patch_area = ar.checked_mul(ar).ok_or(Error::Overflow)?;
    let max_centres = hole_pixels.checked_mul(patch_area).ok_or(Error::Overflow)?;
    let mut bytes = b.area().checked_mul(RGBA_SAMPLE_BYTES).ok_or(Error::Overflow)?;
    for (lw, lh) in pyramid_dims(w, h, options.max_levels) {
        let px = (lw as u64).checked_mul(lh as u64).ok_or(Error::Overflow)?;
        let centres = px.min(max_centres);
        bytes = bytes
            .checked_add(px.checked_mul(LEVEL_BYTES_PER_PIXEL).ok_or(Error::Overflow)?)
            .and_then(|v| v.checked_add(centres.checked_mul(CENTRE_BYTES)?))
            .ok_or(Error::Overflow)?;
    }
    if bytes > limits.max_bytes {
        return Err(Error::BudgetExceeded.into());
    }
    if u32::try_from(b.area()).is_err() {
        return Err(Error::Overflow.into());
    }
    let r = options.patch_radius as usize;
    let ri = i64::from(options.patch_radius);
    let pw = r.wrapping_mul(2).wrapping_add(1);
    let threads = options.threads as usize;
    // Level 0.
    let n = w.wrapping_mul(h);
    let mut img = Vec::with_capacity(n);
    let mut hl = Vec::with_capacity(n);
    let mut allowed = Vec::with_capacity(n);
    let (ex, ey) = b.end()?;
    for y in b.y..ey {
        for x in b.x..ex {
            let p = premul(target.at(x, y));
            img.push([p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32]);
            let in_hole = hole.at(x, y) > 0;
            hl.push(in_hole);
            let excluded = exclude.is_some_and(|e| e.at(x, y) > 0);
            allowed.push(!in_hole && !excluded && sampling.allows(x, y));
        }
    }
    let mut l0 = Level {
        w,
        h,
        img,
        hole: hl,
        allowed,
        valid: Vec::new(),
        sources: Vec::new(),
        slot: Vec::new(),
        domain: Vec::new(),
        holes: Vec::new(),
    };
    l0.compute_valid(r)?;
    if l0.sources.is_empty() {
        return Err(RetouchError::ContentAwareNoSource);
    }
    l0.compute_domain(r)?;
    let mut levels = vec![l0];
    while let Some(cur) = levels.last() {
        let depth_ok = u32::try_from(levels.len()).is_ok_and(|v| v < options.max_levels);
        let (nw, nh) = (cur.w.div_ceil(2), cur.h.div_ceil(2));
        if !depth_ok || cur.hole_extent() <= pw || nw.min(nh) < pw.saturating_mul(2) {
            break;
        }
        let mut next = cur.downsample()?;
        next.compute_valid(r)?;
        next.compute_domain(r)?;
        if next.sources.len() < next.domain.len() {
            break;
        }
        levels.push(next);
    }
    // Work budget (checked before synthesis).
    let ad = Adapt::new(options);
    let mut work = 0u64;
    for l in &levels {
        let lw = level_work(l, r, options, guide.is_some(), &ad).ok_or(Error::Overflow)?;
        work = work.checked_add(lw).ok_or(Error::Overflow)?;
    }
    limits.admit_work(work)?;

    let top = levels.len().saturating_sub(1);
    let mut receipts: Vec<LevelReceipt> = Vec::new();
    let mut prev: Option<(Level, Vec<Match>)> = None;
    let mut total_iters = 0u32;
    let mut final_nnf = Vec::new();
    let mut final_dist = Vec::new();
    for li in (0..=top).rev() {
        let Some(mut l) = levels.pop() else { break };
        let shift = u32::try_from(li).map_err(|_| Error::Overflow)?;
        let goff = guide.map(|g| {
            (
                g.offset.0.checked_shr(shift).unwrap_or(0),
                g.offset.1.checked_shr(shift).unwrap_or(0),
            )
        });
        // Initial hole estimate and field.
        let mut nnf = vec![Match::at(0); l.domain.len()];
        match &prev {
            None => {
                onion_peel(&mut l);
                if let Some((ox, oy)) = goff {
                    for &p in &l.holes.clone() {
                        let (x, y) = l.xy(p);
                        if let Some(si) = l.index(x.wrapping_add(ox), y.wrapping_add(oy))
                            && !l.hole.get(si).copied().unwrap_or(true)
                        {
                            let v = l.px(si);
                            if let Some(t) = l.img.get_mut(p as usize) {
                                *t = v;
                            }
                        }
                    }
                }
                for (k, slot) in nnf.iter_mut().enumerate() {
                    let qi = l.domain.get(k).copied().unwrap_or(0);
                    let (qx, qy) = l.xy(qi);
                    *slot = Match::at(
                        goff.and_then(|(ox, oy)| l.valid_at(qx.wrapping_add(ox), qy.wrapping_add(oy)))
                            .unwrap_or_else(|| random_source(&l, options.seed, li, qi)),
                    );
                }
            }
            Some((coarse, cnnf)) => {
                for &p in &l.holes.clone() {
                    let (x, y) = l.xy(p);
                    let ci = coarse.index(x.checked_div(2).unwrap_or(0), y.checked_div(2).unwrap_or(0));
                    let v = ci.map(|c| coarse.px(c)).unwrap_or([0.0; 4]);
                    if let Some(t) = l.img.get_mut(p as usize) {
                        *t = v;
                    }
                }
                let lr = &l;
                let mut init = vec![Match::at(0); lr.domain.len()];
                par_fill(&mut init, threads, |k| {
                    let qi = lr.domain.get(k).copied().unwrap_or(0);
                    let (qx, qy) = lr.xy(qi);
                    // The parent's match doubled (plus the child parity), same transform; a
                    // transformed match must still be sampleable at this level.
                    let from_parent = coarse
                        .index(qx.checked_div(2).unwrap_or(0), qy.checked_div(2).unwrap_or(0))
                        .and_then(|c| coarse.slot.get(c).copied())
                        .filter(|s| *s != NONE)
                        .and_then(|s| cnnf.get(s as usize).copied())
                        .and_then(|cm| {
                            let (sx, sy) = coarse.xy(cm.src);
                            let (px, py) = (qx.checked_rem(2).unwrap_or(0), qy.checked_rem(2).unwrap_or(0));
                            if cm.identity() {
                                lr.valid_at(sx.wrapping_mul(2).wrapping_add(px), sy.wrapping_mul(2).wrapping_add(py))
                                    .map(Match::at)
                            } else {
                                // The child parity offset goes through the transform too.
                                let m = cm.matrix();
                                let (fx, fy) = (px as f64, py as f64);
                                let cx = sx.wrapping_mul(2).wrapping_add((m[0] * fx + m[1] * fy).round() as i64);
                                let cy = sy.wrapping_mul(2).wrapping_add((m[2] * fx + m[3] * fy).round() as i64);
                                let i = u32::try_from(lr.index(cx, cy)?).ok()?;
                                let m = Match { src: i, ..cm };
                                lr.cost(ri, (qx, qy), m, &ad, f64::INFINITY).map(|(_, m)| m)
                            }
                        });
                    from_parent.unwrap_or_else(|| {
                        Match::at(
                            goff.and_then(|(ox, oy)| lr.valid_at(qx.wrapping_add(ox), qy.wrapping_add(oy)))
                                .unwrap_or_else(|| random_source(lr, options.seed, li, qi)),
                        )
                    })
                });
                nnf = init;
            }
        }
        let window = match guide {
            Some(g) => g.window.checked_shr(shift).unwrap_or(0).max(1),
            None => i64::try_from(l.w.max(l.h)).map_err(|_| Error::Overflow)?,
        };
        let mut dist = vec![0.0f64; nnf.len()];
        let mut energies = Vec::new();
        let norm: f64 = l.domain.iter().map(|&q| 4.0 * l.patch_count(ri, l.xy(q))).sum::<f64>().max(1.0);
        for em in 0..options.em_iterations {
            {
                // Recompute every distance (and refit gain/bias) against the current estimate.
                let lr = &l;
                let nf = &nnf;
                let mut fresh = vec![(Match::at(0), 0.0f64); nnf.len()];
                par_fill(&mut fresh, threads, |k| {
                    let q = lr.xy(lr.domain.get(k).copied().unwrap_or(0));
                    let m = nf.get(k).copied().unwrap_or(Match::at(0));
                    lr.cost(ri, q, m, &ad, f64::INFINITY).map_or((m, f64::INFINITY), |(d, m)| (m, d))
                });
                for ((o, d), (no, nd)) in nnf.iter_mut().zip(dist.iter_mut()).zip(fresh) {
                    *o = no;
                    *d = nd;
                }
            }
            for sw in 0..options.search_iterations {
                let exp = options.search_iterations.wrapping_sub(1).wrapping_sub(sw).min(MAX_JUMP_SHIFT);
                let ctx = Sweep {
                    l: &l,
                    r: ri,
                    nnf: &nnf,
                    dist: &dist,
                    seed: options.seed,
                    level: li,
                    em,
                    sweep: sw,
                    jump: 1i64.checked_shl(exp).unwrap_or(1),
                    window,
                    guide: goff,
                    adapt: ad,
                };
                let mut out = vec![(Match::at(0), 0.0f64); nnf.len()];
                par_fill(&mut out, threads, |k| ctx.step(k));
                for ((o, d), (no, nd)) in nnf.iter_mut().zip(dist.iter_mut()).zip(out) {
                    *o = no;
                    *d = nd;
                }
            }
            energies.push(dist.iter().sum::<f64>() / norm);
            // Vote (reads source pixels only); the last EM iteration of level 0 takes the best
            // covering patch's pixel (no averaging blur of the final texture).
            let best_vote = li == 0 && em.wrapping_add(1) == options.em_iterations;
            let mut votes = vec![[0.0f32; 4]; l.holes.len()];
            {
                let lr = &l;
                let (nf, df) = (&nnf, &dist);
                par_fill(&mut votes, threads, |k| {
                    lr.vote(ri, nf, df, &ad, lr.xy(lr.holes.get(k).copied().unwrap_or(0)), best_vote)
                });
            }
            for (&p, v) in l.holes.iter().zip(votes) {
                if let Some(t) = l.img.get_mut(p as usize) {
                    *t = v;
                }
            }
            total_iters = total_iters.saturating_add(1);
        }
        receipts.push(LevelReceipt {
            width: u32::try_from(l.w).map_err(|_| Error::Overflow)?,
            height: u32::try_from(l.h).map_err(|_| Error::Overflow)?,
            hole_pixels: l.holes.len() as u64,
            domain_centres: l.domain.len() as u64,
            source_centres: l.sources.len() as u64,
            em_iterations: options.em_iterations,
            search_iterations: options.search_iterations,
            energies,
        });
        if li == 0 {
            final_nnf = nnf;
            final_dist = dist;
            prev = Some((l, Vec::new()));
        } else {
            prev = Some((l, nnf));
        }
    }
    let Some((level0, _)) = prev else { return Ok(None) };
    let final_energy = receipts.last().and_then(|r| r.energies.last().copied()).unwrap_or(0.0);
    Ok(Some(Synthesis {
        level0,
        nnf: final_nnf,
        dist: final_dist,
        receipt: ContentAwareReceipt {
            seed: options.seed,
            patch_radius: options.patch_radius,
            levels: receipts,
            iterations: total_iters,
            final_energy,
            work,
            guided: guide.is_some(),
            blend: None,
        },
    }))
}

/// Coarsest-level hole initialisation (single-threaded, deterministic).
fn onion_peel(l: &mut Level) {
    let mut known: Vec<bool> = l.hole.iter().map(|h| !h).collect();
    let mut pending: Vec<u32> = l.holes.clone();
    let mut passes = 0usize;
    let cap = l.w.saturating_add(l.h).saturating_add(2);
    while !pending.is_empty() && passes < cap {
        let mut updates = Vec::new();
        for &p in &pending {
            let (x, y) = l.xy(p);
            let mut sum = [0.0f64; 4];
            let mut cnt = 0u32;
            for (dx, dy) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)] {
                if let Some(ni) = l.index(x.wrapping_add(dx), y.wrapping_add(dy))
                    && known.get(ni).copied().unwrap_or(false)
                {
                    let v = l.px(ni);
                    for (s, c) in sum.iter_mut().zip(v) {
                        *s += f64::from(c);
                    }
                    cnt = cnt.saturating_add(1);
                }
            }
            if cnt > 0 {
                let mut px = [0.0f32; 4];
                for (o, s) in px.iter_mut().zip(sum) {
                    *o = (s / f64::from(cnt)) as f32;
                }
                updates.push((p, px));
            }
        }
        if updates.is_empty() {
            break;
        }
        for (p, v) in updates {
            if let Some(t) = l.img.get_mut(p as usize) {
                *t = v;
            }
            if let Some(k) = known.get_mut(p as usize) {
                *k = true;
            }
        }
        pending.retain(|p| !known.get(*p as usize).copied().unwrap_or(true));
        passes = passes.saturating_add(1);
    }
}

fn f32x4_to_f64(p: [f32; 4]) -> [f64; 4] {
    [f64::from(p[0]), f64::from(p[1]), f64::from(p[2]), f64::from(p[3])]
}

/// The hole clipped to the target bounds (budget checked).
fn clip_hole(
    target: &RgbaPlane,
    hole: &CoveragePlane,
    limits: &RetouchLimits,
) -> Result<Option<CoveragePlane>, RetouchError> {
    coverage_validate(hole)?;
    let Some(bounds) = hole.bounds.intersect(target.bounds)? else { return Ok(None) };
    let n = limits.admit_area(bounds, 2)?;
    let mut data = Vec::with_capacity(n);
    let (ex, ey) = bounds.end()?;
    for y in bounds.y..ey {
        for x in bounds.x..ex {
            data.push(hole.at(x, y));
        }
    }
    Ok(Some(CoveragePlane { bounds, data }))
}

fn validate_sampling(sampling: &SamplingArea<'_>) -> Result<(), RetouchError> {
    match sampling {
        SamplingArea::Auto => Ok(()),
        SamplingArea::Rectangular(r) => {
            r.end()?;
            Ok(())
        }
        SamplingArea::Custom(c) => coverage_validate(c),
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    range: SampleRange,
    target: &RgbaPlane,
    hole: &CoveragePlane,
    sampling: SamplingArea<'_>,
    exclude: Option<&CoveragePlane>,
    guide: Option<Guide>,
    options: &ContentAwareOptions,
    limits: &RetouchLimits,
) -> Result<ContentAwareResult, RetouchError> {
    options.validate()?;
    target.validate_in(range)?;
    validate_sampling(&sampling)?;
    if let Some(e) = exclude {
        coverage_validate(e)?;
    }
    let unchanged = |plane: RgbaPlane| ContentAwareResult {
        result: RetouchResult { plane, damage: None },
        receipt: ContentAwareReceipt::empty(options, guide.is_some()),
    };
    let Some(cov) = clip_hole(target, hole, limits)? else {
        return Ok(unchanged(crate::retouch::copy_plane_in(target, limits, range)?));
    };
    let Some(syn) = synthesize(target, &cov, &sampling, exclude, guide, options, limits)? else {
        return Ok(unchanged(crate::retouch::copy_plane_in(target, limits, range)?));
    };
    let b = target.bounds;
    let l = &syn.level0;
    let local = |x: i64, y: i64| -> Option<usize> { l.index(x.wrapping_sub(b.x), y.wrapping_sub(b.y)) };
    let mut receipt = syn.receipt;
    let plane = match options.colour_adaptation {
        ColourAdaptation::None => {
            let mut out = crate::retouch::copy_plane_in(target, limits, range)?;
            let (ex, ey) = cov.bounds.end()?;
            for y in cov.bounds.y..ey {
                for x in cov.bounds.x..ex {
                    let c = f64::from(cov.at(x, y)) / f64::from(u16::MAX);
                    if c <= 0.0 {
                        continue;
                    }
                    let Some(li) = local(x, y) else { continue };
                    let f = f32x4_to_f64(l.px(li));
                    let d = premul(target.at(x, y));
                    let mut v = [0.0f64; 4];
                    for k in 0..4 {
                        v[k] = d[k] + c * (f[k] - d[k]);
                    }
                    if let Some(oi) = out.index(x, y)
                        && let Some(px) = out.data.get_mut(oi)
                    {
                        *px = straight_in(v, range);
                    }
                }
            }
            out
        }
        _ => {
            let ri = i64::from(options.patch_radius);
            let healed = heal_with_guide(target, &cov, None, limits, |region| {
                let n = limits.admit_area(region, RGBA_SAMPLE_BYTES)?;
                let mut data = Vec::with_capacity(n);
                let (ex, ey) = region.end()?;
                for y in region.y..ey {
                    for x in region.x..ex {
                        let Some(li) = local(x, y) else {
                            data.push(target.at(x, y));
                            continue;
                        };
                        let in_hole = l.hole.get(li).copied().unwrap_or(false);
                        let ring = !in_hole
                            && [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)].iter().any(|(dx, dy)| {
                                local(x.wrapping_add(*dx), y.wrapping_add(*dy))
                                    .is_some_and(|ni| l.hole.get(ni).copied().unwrap_or(false))
                            });
                        let p = if in_hole {
                            straight(f32x4_to_f64(l.px(li)))
                        } else if ring {
                            let (lx, ly) = l.xy(u32::try_from(li).map_err(|_| Error::Overflow)?);
                            straight(f32x4_to_f64(l.vote(ri, &syn.nnf, &syn.dist, &Adapt::new(options), (lx, ly), true)))
                        } else {
                            target.at(x, y)
                        };
                        data.push(p);
                    }
                }
                Ok(RgbaPlane { bounds: region, data })
            })?;
            receipt.blend = Some(healed.receipt);
            healed.result.plane
        }
    };
    Ok(ContentAwareResult {
        result: RetouchResult { plane, damage: Some(cov.bounds) },
        receipt,
    })
}

/// Content-aware fill (STU-RAS-143): synthesize the pixels of `hole` (coverage > 0, clipped to the
/// target) from the sampling area of `target` (module documentation: pyramid, PatchMatch NNF,
/// weighted voting, optional healing blend). Returns the whole new plane, the damage rectangle
/// (the clipped hole bounds) and the quality receipt. An empty hole returns the target unchanged.
pub fn fill(
    target: &RgbaPlane,
    hole: &CoveragePlane,
    sampling: SamplingArea<'_>,
    options: &ContentAwareOptions,
    limits: &RetouchLimits,
) -> Result<ContentAwareResult, RetouchError> {
    run(SampleRange::Unit, target, hole, sampling, None, None, options, limits)
}

/// Is a fill with `options` defined on extended-range (scene-referred) samples? Only without
/// colour adaptation: PatchMatch distances, EM voting and the pyramid are weighted averages and the
/// `none` blend `d + c (f - d)` is linear, none of them refers to the unit white point; the Image
/// Melding gain / bias (bias bounded by 0.1 / 0.25 of the unit scale, adjusted samples clamped to
/// `[0, alpha]`) and the membrane (Poisson) heal of the other adaptations do (STU-COL-120,
/// STU-FX-006a).
pub fn fill_extended_defined(options: &ContentAwareOptions) -> bool {
    options.colour_adaptation == ColourAdaptation::None
}

/// [`fill`] in `range` (lane SMB-PIGMENT-EXTENDED-RANGE-2). Extended: the target may hold any
/// finite colour and the filled colour is not clamped; options that are not
/// [`fill_extended_defined`], and a non-finite result, are `RetouchError::OutOfDomain`.
pub fn fill_in(
    range: SampleRange,
    target: &RgbaPlane,
    hole: &CoveragePlane,
    sampling: SamplingArea<'_>,
    options: &ContentAwareOptions,
    limits: &RetouchLimits,
) -> Result<ContentAwareResult, RetouchError> {
    if range == SampleRange::Extended && !fill_extended_defined(options) {
        return Err(RetouchError::OutOfDomain);
    }
    let out = run(range, target, hole, sampling, None, None, options, limits)?;
    if range == SampleRange::Extended
        && !out.result.plane.data.iter().all(|p| p.iter().all(|v| v.is_finite()))
    {
        return Err(RetouchError::OutOfDomain);
    }
    Ok(out)
}

/// Inpaint / remove (STU-RAS-143 "inpaint/remove", the on-device baseline of distraction removal
/// in module 14 §15): the pixels of `mask` (coverage > 0) are removed and synthesized from every
/// other pixel of `target`. It is [`fill`] with `SamplingArea::Auto` (same bytes, receipt and
/// refusals; test `content_aware_scale.rs` `inpaint_is_the_native_fill_over_the_whole_plane`); a
/// caller that restricts the sources calls [`fill`].
pub fn inpaint(
    target: &RgbaPlane,
    mask: &CoveragePlane,
    options: &ContentAwareOptions,
    limits: &RetouchLimits,
) -> Result<ContentAwareResult, RetouchError> {
    fill(target, mask, SamplingArea::Auto, options, limits)
}

/// `structure` (1 ..= 7) of a content-aware patch: the level-0 random search radius around the best
/// match is `2^(PATCH_WINDOW_LOG2 - structure)` pixels (structure 7 = 8 px, 1 = 512 px), halved per
/// pyramid level.
pub const PATCH_WINDOW_LOG2: u32 = 10;

/// STU-RAS-026 Patch mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatchMode {
    /// Healing transfer of the dragged source along the offset (the retouch heal solver).
    Normal,
    /// Synthesis guided by the dragged source: every domain centre `q` starts at (and keeps trying)
    /// the source `q + offset`, the coarsest hole starts from the dragged pixels, and every later
    /// candidate (propagated or random) must lie within the structure window of `q + offset`
    /// (Chebyshev; higher structure = closer to the dragged source).
    ContentAware { structure: u8 },
}

/// The rectangle `r` moved by `(dx, dy)` (checked).
fn shifted(r: Rect, dx: i64, dy: i64) -> Result<Rect, Error> {
    let moved = Rect {
        x: r.x.checked_add(dx).ok_or(Error::Overflow)?,
        y: r.y.checked_add(dy).ok_or(Error::Overflow)?,
        width: r.width,
        height: r.height,
    };
    moved.end()?;
    Ok(moved)
}

/// Content-aware patch (STU-RAS-026 Patch row): repair the pixels of `region` (coverage > 0,
/// clipped to the target) from the source region `region + source_offset` (destination pixel
/// `p` reads `p + source_offset`). The source rectangle (clipped region bounds moved by the
/// offset) must lie inside the target (else `retouch_out_of_range`). `Normal` heals the source in
/// (receipt: no levels, `blend` = the healing receipt with `source_offset`); `ContentAware`
/// synthesizes like [`fill`] with the patch guidance (receipt `guided`). Studio readings of the
/// Photoshop Patch tool, class Approximate, UNVERIFIED (D5).
pub fn patch(
    target: &RgbaPlane,
    region: &CoveragePlane,
    source_offset: (i64, i64),
    mode: PatchMode,
    sampling: SamplingArea<'_>,
    options: &ContentAwareOptions,
    limits: &RetouchLimits,
) -> Result<ContentAwareResult, RetouchError> {
    options.validate()?;
    target.validate()?;
    let window = match mode {
        // The healing transfer has no search to adapt: refused, never ignored.
        PatchMode::Normal if options.adapts_search() => {
            return Err(RetouchError::ContentAwareOptionUnsupported);
        }
        PatchMode::Normal => None,
        PatchMode::ContentAware { structure } => {
            if !(1..=7).contains(&structure) {
                return Err(RetouchError::OutOfRange);
            }
            let shift = PATCH_WINDOW_LOG2.checked_sub(u32::from(structure)).ok_or(Error::Overflow)?;
            Some(1i64.checked_shl(shift).ok_or(Error::Overflow)?)
        }
    };
    let Some(cov) = clip_hole(target, region, limits)? else {
        return Ok(ContentAwareResult {
            result: RetouchResult { plane: copy_plane(target, limits)?, damage: None },
            receipt: ContentAwareReceipt::empty(options, true),
        });
    };
    let (ox, oy) = source_offset;
    if !target.bounds.contains(shifted(cov.bounds, ox, oy)?)? {
        return Err(RetouchError::OutOfRange);
    }
    match window {
        None => {
            // Destination p reads p + offset: destination = sample - offset (offsets are bounded
            // by the plane size here, so the binary64 conversion is exact).
            let map = CloneMap::offset(-(ox as f64), -(oy as f64), ResampleFilter::NearestNeighbour)?;
            let healed = heal_with_coverage(target, target, &map, &cov, Some(source_offset), limits)?;
            let mut receipt = ContentAwareReceipt::empty(options, true);
            receipt.blend = Some(healed.receipt);
            Ok(ContentAwareResult { result: healed.result, receipt })
        }
        Some(window) => run(
            SampleRange::Unit,
            target,
            &cov,
            sampling,
            None,
            Some(Guide { offset: source_offset, window }),
            options,
            limits,
        ),
    }
}

/// STU-RAS-026 Content-aware move mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveMode {
    /// The object leaves its place: the vacated selection is content-aware filled.
    Move,
    /// The object is duplicated at the destination; its original stays.
    Extend,
}

/// Result of a content-aware move.
#[derive(Clone, Debug, PartialEq)]
pub struct MoveResult {
    pub result: RetouchResult,
    /// The fill of the vacated area (`Move`; `None` for `Extend` or an empty selection).
    pub fill: Option<ContentAwareReceipt>,
    /// The healing blend of the placed object (`colour_adaptation = default`).
    pub paste: Option<HealReceipt>,
}

/// Content-aware move (STU-RAS-026 row): move (or extend) the selected object by `offset`
/// (destination pixel `p` shows the original pixel `p - offset`). `Move` first fills the vacated
/// selection like [`fill`] with the destination footprint excluded from the sampling area (test
/// `hostile_patch_and_move_inputs_are_typed_refusals`); then the object is placed:
/// `colour_adaptation = none` paints it source-over with its selection coverage, `default` heals it
/// in (membrane Poisson, guide = the moved original, so it keeps its texture and takes the new
/// surroundings' colour at its edge). A destination outside the target places nothing. Integer
/// offsets only (transform-on-drop is not modelled). Studio readings, class Approximate,
/// Photoshop UNVERIFIED (D5).
pub fn content_aware_move(
    target: &RgbaPlane,
    selection: &CoveragePlane,
    offset: (i64, i64),
    mode: MoveMode,
    sampling: SamplingArea<'_>,
    options: &ContentAwareOptions,
    limits: &RetouchLimits,
) -> Result<MoveResult, RetouchError> {
    options.validate()?;
    target.validate()?;
    let Some(sel) = clip_hole(target, selection, limits)? else {
        return Ok(MoveResult {
            result: RetouchResult { plane: copy_plane(target, limits)?, damage: None },
            fill: None,
            paste: None,
        });
    };
    let (ox, oy) = offset;
    let dest = CoveragePlane { bounds: shifted(sel.bounds, ox, oy)?, data: sel.data.clone() };
    let (mut plane, fill_receipt, mut damage) = match mode {
        MoveMode::Move => {
            let r = run(SampleRange::Unit, target, &sel, sampling, Some(&dest), None, options, limits)?;
            (r.result.plane, Some(r.receipt), r.result.damage)
        }
        MoveMode::Extend => (copy_plane(target, limits)?, None, None),
    };
    let mut paste = None;
    if let Some(visible) = dest.bounds.intersect(target.bounds)? {
        match options.colour_adaptation {
            ColourAdaptation::None => {
                let (ex, ey) = visible.end()?;
                for y in visible.y..ey {
                    for x in visible.x..ex {
                        let c = f64::from(dest.at(x, y)) / f64::from(u16::MAX);
                        let sx = x.checked_sub(ox).ok_or(Error::Overflow)?;
                        let sy = y.checked_sub(oy).ok_or(Error::Overflow)?;
                        let src = target.at(sx, sy);
                        if let Some(i) = plane.index(x, y)
                            && let Some(px) = plane.data.get_mut(i)
                        {
                            *px = paint_over(*px, src, c);
                        }
                    }
                }
            }
            _ => {
                // The destination intersects the target, so the offset is bounded by the plane
                // sizes and the binary64 conversion is exact.
                let map = CloneMap::offset(ox as f64, oy as f64, ResampleFilter::NearestNeighbour)?;
                let back = (ox.checked_neg().ok_or(Error::Overflow)?, oy.checked_neg().ok_or(Error::Overflow)?);
                let healed = heal_with_coverage(&plane, target, &map, &dest, Some(back), limits)?;
                plane = healed.result.plane;
                paste = Some(healed.receipt);
            }
        }
        damage = Some(match damage {
            None => visible,
            Some(d) => union(d, visible)?,
        });
    }
    Ok(MoveResult { result: RetouchResult { plane, damage }, fill: fill_receipt, paste })
}

/// Smallest rectangle containing `a` and `b` (checked).
fn union(a: Rect, b: Rect) -> Result<Rect, Error> {
    let (ae, af) = a.end()?;
    let (be, bf) = b.end()?;
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    let (e, f) = (ae.max(be), af.max(bf));
    Ok(Rect {
        x,
        y,
        width: u32::try_from(e.checked_sub(x).ok_or(Error::Overflow)?).map_err(|_| Error::Overflow)?,
        height: u32::try_from(f.checked_sub(y).ok_or(Error::Overflow)?).map_err(|_| Error::Overflow)?,
    })
}

/// Content-aware surface description (Studio readings, parity NOT_PROVEN).
pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PIGMENT","surface":"content_aware","version":2,"spec":"STU-RAS-143 native on-device content-aware fill (STU-RAS-030); STU-RAS-026 patch and content-aware move rows","fill":"fill(target, hole CoveragePlane coverage>0, SamplingArea auto|rectangular|custom(coverage>=32768), options, limits): pyramid (halve while hole bbox > patch, next level >= 2 patches, sources exist, <= max_levels), PatchMatch NNF (Jacobi sweeps: propagation at distance 1 and 2^min(4,S-1-s), random search radii max(w,h)/2^k, SplitMix64(seed, level, em, sweep, centre, draw)), EM with weighted voting exp(-e/(2*0.1^2)) and a best-patch final vote at level 0, levels need source centres >= domain centres, coarsest init onion peel, finer init from parent; blend colour_adaptation none = d + c(f - d), default = membrane Poisson (retouch heal solver) with guide = synthesized hole + NNF votes on the 4-neighbour ring","options":{"patch_radius":"1..8 default 3","em_iterations":"1..64 default 4","search_iterations":"1..32 default 4","max_levels":"1..16 default 8","threads":"1..64 (bytes independent)","max_hole_pixels":"default 1048576","colour_adaptation":"none|default|high (gain 0.9..1/0.9, |bias|<=0.1)|very_high (gain 0.75..1/0.75, |bias|<=0.25)","rotation_adaptation":"none 0|low 15|medium 45|high 90|full 180 degrees, 5 degree steps","scale":"patch scale 2^(k/8), |k|<=3","mirror":"horizontal mirror before rotation"},"adaptations":"Image Melding gain/bias per channel (g = clamp(sd_t/sd_s), b = clamp(mean_t - g mean_s)); Generalized PatchMatch transformed patches c + s R(theta) diag(+-1,1) o, bilinear taps all sampleable; propagation moves the source by the transformed step; random search tries the best transform and a perturbed one; patch normal refuses adaptations content_aware_option_unsupported","receipt":"ContentAwareReceipt{seed, patch_radius, levels[{width,height,hole_pixels,domain_centres,source_centres,em_iterations,search_iterations,energies}], iterations, final_energy, work, guided, blend HealReceipt}","refusals":["retouch_out_of_range","retouch_shape","retouch_out_of_domain","budget_exceeded","overflow","content_aware_hole_too_large","content_aware_no_source","content_aware_option_unsupported"],"determinism":"pure per-centre/per-pixel updates: bytes independent of threads; fixed seed = fixed bytes","patch":"patch(target, region, source_offset (p reads p + offset), PatchMode normal = retouch heal along the offset | content_aware{structure 1..7} = fill guided by q + offset (coarsest hole starts from the dragged pixels, every candidate within the window 2^(10 - structure) px of q + offset at level 0, halved per level); source rect must lie inside the target (retouch_out_of_range)","inpaint":"inpaint(target, mask, options, limits) = fill(target, mask, auto, options, limits) (STU-RAS-143 inpaint/remove)","scale":"content_aware_scale(plane, new_width, new_height, protect CoveragePlane?, limits): seam carving (Avidan-Shamir 2007), energy = L1 gradient of premultiplied RGBA + protect coverage x 8 x (max(w,h)+1), least-energy 8-connected seams by DP (first minimum, back-track x-1,x,x+1), narrowing removes seams one at a time, widening duplicates the seams a narrowing would remove (mean with the right neighbour) in steps of at most half the width, width then height; receipt removed/inserted columns/rows, protected_crossings, work; amount and protect-skin-tones not modelled","move":"content_aware_move(target, selection, offset (p shows p - offset), MoveMode move|extend): move fills the vacated selection (destination footprint never sampled), then places the object: colour_adaptation none = source-over with selection coverage, default = retouch heal with the moved original as guide; integer offsets only","not_modelled":["transform-on-drop","sampling overlay","sample_all_layers (caller flattens)","additional_document_source","output brightness/contrast/colour balance","output_to"],"class":"approximate","parity":"NOT_PROVEN (Photoshop content-aware fill/patch/move UNVERIFIED, D5)"}"#;
