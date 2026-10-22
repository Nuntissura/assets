//! Text animators (lane SMB-AE-MOTION-TOOLS slice 2; STU-MOT-090..092). Motion owns the animator
//! property subtree and its evaluation; Type shapes and outlines, Composite places the glyphs.
//!
//! Model (STU-MOT-090): an [`Animator`] holds PROPERTIES (what changes) and SELECTORS (which units
//! and by how much). Each selector yields a per-unit amount in `0..=1` per dimension (x, y, z);
//! several selectors of one animator compose MULTIPLICATIVELY (STU-MOT-091). A character takes
//! the amount of the unit it belongs to (characters, characters excluding spaces, words, lines).
//!
//! Range selector (Studio's stated maths; After Effects parity UNVERIFIED, D5):
//! * `n` units of the selector's basis; start `s`, end `e`, offset `o` in percent of `n` (or in unit
//!   indices with `RangeUnits::Index`); `lo = min(s, e) + o`, `hi = max(s, e) + o` in units.
//! * Unit `i` occupies `[i, i + 1]`, centre `c = i + 0.5`; with randomize order the unit's position
//!   is `perm[i]` (seeded Fisher-Yates over splitmix64, `random_seed`).
//! * `Square`: the overlap of the box `[c - w/2, c + w/2]` with `[lo, hi]`, divided by `w`, where
//!   `w = smoothness / 100` (100 % = the unit's own coverage `[i, i + 1]`, 0 % = a hard step at the
//!   centre).
//! * Other shapes use `t = (c - lo) / (hi - lo)`: `RampUp` = clamp(t), `RampDown` = 1 - clamp(t),
//!   and inside `0 <= t <= 1` only: `Triangle` = 1 - |2t - 1|, `Round` = sqrt(1 - (2t - 1)^2),
//!   `Smooth` = (1 - cos(2 pi t)) / 2 with cos from quarter-turn reduction and fixed Taylor
//!   polynomials (outside: 0).
//! * Ease low / ease high (percent -100..=100) remap the value `v` through the cubic Bezier
//!   `[0, (1 - low)/3, 1 - (1 - high)/3, 1]` (x(s) = s, so the curve is evaluated at s = v); 0 / 0
//!   is the identity.
//! * Amount (percent 0..=100) scales the result.
//!
//! Wiggly selector: per unit and dimension, smooth seeded value noise `N` in `-1..=1` (lattice
//! values from splitmix64, smoothstep interpolation) at `x = time_s * wiggles_per_second +
//! temporal_phase / 360 + i * spatial_phase / 360`; `correlation` (percent) mixes one stream common
//! to all units with each unit's own stream; `lock_dimensions` uses dimension 0's stream for all;
//! amount `= (min + (max - min) * (N + 1) / 2) / 100`, with `0 <= min <= max <= 100`.
//!
//! Applying an animator with amount `a` (per dimension) to a character: position offset `a * P`,
//! scale `100 + a (S - 100)` percent, rotation `a * R` degrees, opacity factor
//! `(100 + a (O - 100)) / 100`, fill `lerp(fill, F, a)` straight RGBA (a character with amount 0
//! keeps the layer fill: `CharTransform::fill` stays `None`), tracking `a * T` in 1/1000 em
//! (added after the character). Several animators apply in order: offsets, rotations and tracking
//! add, scale and opacity factors multiply, fills lerp from the running fill.
//!
//! The expression selector (STU-MOT-091) needs an expression engine (MotionJS port) and is not
//! built here: declared gap. Every input is bounded and checked before allocation (unit count,
//! animators, selectors) and every parameter is finite and in domain (typed `invalid_parameter`).
use crate::{error::MotionError, property::Property, time::EvalTick};
use hsk_studio_accord::CancellationToken;

/// Most text units (characters) one evaluation accepts.
pub const MAX_TEXT_UNITS: usize = 1 << 20;
/// Most animators of one text layer.
pub const MAX_ANIMATORS: usize = 1024;
/// Most selectors of one animator.
pub const MAX_SELECTORS: usize = 64;

/// One character of the text being animated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextChar {
    pub is_space: bool,
    /// Word index (0-based, words separated by spaces).
    pub word: u32,
    /// Line index (0-based).
    pub line: u32,
}

/// What a selector counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BasedOn {
    Characters,
    CharactersExcludingSpaces,
    Words,
    Lines,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeUnits {
    Percent,
    Index,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeShape {
    Square,
    RampUp,
    RampDown,
    Triangle,
    Round,
    Smooth,
}

/// STU-MOT-091 range selector (module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RangeSelector {
    pub start: f64,
    pub end: f64,
    pub offset: f64,
    pub units: RangeUnits,
    pub based_on: BasedOn,
    pub shape: RangeShape,
    /// Percent 0..=100 (Square only).
    pub smoothness: f64,
    /// Percent -100..=100.
    pub ease_high: f64,
    pub ease_low: f64,
    /// Percent 0..=100.
    pub amount: f64,
    pub randomize_order: bool,
    pub random_seed: u64,
}

impl Default for RangeSelector {
    /// After Effects' new-selector defaults as Studio reads them: 0..100 %, square, 100 % smoothness.
    fn default() -> Self {
        Self {
            start: 0.0,
            end: 100.0,
            offset: 0.0,
            units: RangeUnits::Percent,
            based_on: BasedOn::Characters,
            shape: RangeShape::Square,
            smoothness: 100.0,
            ease_high: 0.0,
            ease_low: 0.0,
            amount: 100.0,
            randomize_order: false,
            random_seed: 0,
        }
    }
}

/// STU-MOT-091 wiggly selector (module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WigglySelector {
    pub based_on: BasedOn,
    /// Percent, `0 <= min_amount <= max_amount <= 100`.
    pub max_amount: f64,
    pub min_amount: f64,
    pub wiggles_per_second: f64,
    /// Percent 0..=100.
    pub correlation: f64,
    /// Degrees.
    pub temporal_phase: f64,
    pub spatial_phase: f64,
    pub lock_dimensions: bool,
    pub random_seed: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Selector {
    Range(RangeSelector),
    Wiggly(WigglySelector),
}

/// The animator properties built here (STU-MOT-092 subset: transform, appearance, tracking). An
/// absent property leaves the character unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AnimatorProperties {
    /// Pixels (x right, y down).
    pub position: Option<[f64; 2]>,
    /// Percent per axis.
    pub scale: Option<[f64; 2]>,
    /// Degrees, clockwise on a y-down canvas.
    pub rotation: Option<f64>,
    /// Percent 0..=100 (the opacity the fully selected character reaches).
    pub opacity: Option<f64>,
    /// Straight RGBA, 0..=1.
    pub fill: Option<[f64; 4]>,
    /// 1/1000 em added after the character.
    pub tracking: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Animator {
    pub properties: AnimatorProperties,
    pub selectors: Vec<Selector>,
}

/// The resolved per-character result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharTransform {
    pub position: [f64; 2],
    /// Factors (1 = unscaled).
    pub scale: [f64; 2],
    pub rotation: f64,
    /// Factor 0..=1.
    pub opacity: f64,
    /// `None` = the layer's own fill.
    pub fill: Option<[f64; 4]>,
    /// 1/1000 em added after the character.
    pub tracking: f64,
}

impl CharTransform {
    pub const IDENTITY: Self = Self {
        position: [0.0, 0.0],
        scale: [1.0, 1.0],
        rotation: 0.0,
        opacity: 1.0,
        fill: None,
        tracking: 0.0,
    };
}

fn bad(parameter: &'static str) -> MotionError {
    MotionError::InvalidParameter { parameter }
}

fn finite_in(v: f64, lo: f64, hi: f64, name: &'static str) -> Result<(), MotionError> {
    if !v.is_finite() || v < lo || v > hi {
        return Err(bad(name));
    }
    Ok(())
}

fn finite(v: f64, name: &'static str) -> Result<(), MotionError> {
    if !v.is_finite() || v.abs() > 1.0e9 {
        return Err(bad(name));
    }
    Ok(())
}

impl RangeSelector {
    fn validate(&self) -> Result<(), MotionError> {
        finite(self.start, "range_start")?;
        finite(self.end, "range_end")?;
        finite(self.offset, "range_offset")?;
        finite_in(self.smoothness, 0.0, 100.0, "range_smoothness")?;
        finite_in(self.ease_high, -100.0, 100.0, "range_ease_high")?;
        finite_in(self.ease_low, -100.0, 100.0, "range_ease_low")?;
        finite_in(self.amount, 0.0, 100.0, "range_amount")
    }
}

impl WigglySelector {
    fn validate(&self) -> Result<(), MotionError> {
        finite_in(self.max_amount, 0.0, 100.0, "wiggly_max_amount")?;
        finite_in(self.min_amount, 0.0, self.max_amount, "wiggly_min_amount")?;
        finite_in(self.wiggles_per_second, 0.0, 1.0e6, "wiggly_frequency")?;
        finite_in(self.correlation, 0.0, 100.0, "wiggly_correlation")?;
        finite(self.temporal_phase, "wiggly_temporal_phase")?;
        finite(self.spatial_phase, "wiggly_spatial_phase")
    }
}

impl AnimatorProperties {
    fn validate(&self) -> Result<(), MotionError> {
        for v in self.position.iter().flatten() {
            finite(*v, "animator_position")?;
        }
        for v in self.scale.iter().flatten() {
            finite(*v, "animator_scale")?;
        }
        if let Some(r) = self.rotation {
            finite(r, "animator_rotation")?;
        }
        if let Some(o) = self.opacity {
            finite_in(o, 0.0, 100.0, "animator_opacity")?;
        }
        for v in self.fill.iter().flatten() {
            finite_in(*v, 0.0, 1.0, "animator_fill")?;
        }
        if let Some(t) = self.tracking {
            finite(t, "animator_tracking")?;
        }
        Ok(())
    }
}

/// splitmix64 (Steele, Lea, Flood 2014; public-domain reference constants).
pub fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn unit_f64(h: u64) -> f64 {
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// Seeded value noise in `-1..=1`: lattice values at integer `x` from splitmix64 of
/// `(seed, stream, floor(x))`, smoothstep interpolation between them (exact operations only).
pub fn value_noise(seed: u64, stream: u64, x: f64) -> f64 {
    let x0 = x.floor();
    let f = x - x0;
    let k = x0 as i64 as u64;
    let lattice = |k: u64| {
        let h = splitmix64(seed ^ splitmix64(stream ^ splitmix64(k)));
        unit_f64(h) * 2.0 - 1.0
    };
    let (a, b) = (lattice(k), lattice(k.wrapping_add(1)));
    let s = f * f * (3.0 - 2.0 * f);
    a + (b - a) * s
}

/// cos(2 pi t): reduction to the nearest quarter turn (exact `%` and `round`), then Taylor
/// polynomials of degree 18 / 19 on `|x| <= pi / 4` (error below 1e-17) and the quadrant rotation.
fn cos_turn(t: f64) -> f64 {
    let mut q = (t % 1.0) * 4.0;
    if q < 0.0 {
        q += 4.0;
    }
    let k = q.round();
    let x = (q - k) * std::f64::consts::FRAC_PI_2;
    let x2 = x * x;
    let (mut sin, mut cos) = (1.0, 1.0);
    for n in (1..=9_u32).rev() {
        let n = f64::from(n);
        sin = 1.0 - x2 / ((2.0 * n) * (2.0 * n + 1.0)) * sin;
        cos = 1.0 - x2 / ((2.0 * n - 1.0) * (2.0 * n)) * cos;
    }
    let sin = x * sin;
    match (k as i64).rem_euclid(4) {
        0 => cos,
        1 => -sin,
        2 => -cos,
        _ => sin,
    }
}

/// Unit index of every character for a basis, plus the unit count. Excluded spaces get `None`.
pub fn units_of(chars: &[TextChar], based_on: BasedOn) -> (Vec<Option<usize>>, usize) {
    let mut out = Vec::with_capacity(chars.len());
    let mut count = 0usize;
    match based_on {
        BasedOn::Characters => {
            for _ in chars {
                out.push(Some(count));
                count += 1;
            }
        }
        BasedOn::CharactersExcludingSpaces => {
            for c in chars {
                if c.is_space {
                    out.push(None);
                } else {
                    out.push(Some(count));
                    count += 1;
                }
            }
        }
        BasedOn::Words | BasedOn::Lines => {
            let mut last: Option<u32> = None;
            for c in chars {
                if based_on == BasedOn::Words && c.is_space {
                    out.push(None);
                    continue;
                }
                let key = if based_on == BasedOn::Words { c.word } else { c.line };
                if last != Some(key) {
                    last = Some(key);
                    count += 1;
                }
                out.push(Some(count - 1));
            }
        }
    }
    (out, count)
}

fn permutation(n: usize, seed: u64) -> Vec<usize> {
    let mut p: Vec<usize> = (0..n).collect();
    let mut state = seed;
    for i in (1..n).rev() {
        state = splitmix64(state);
        let j = (state % (i as u64 + 1)) as usize;
        p.swap(i, j);
    }
    p
}

fn ease_curve(v: f64, low: f64, high: f64) -> f64 {
    let y1 = (1.0 - low / 100.0) / 3.0;
    let y2 = 1.0 - (1.0 - high / 100.0) / 3.0;
    let m = 1.0 - v;
    3.0 * m * m * v * y1 + 3.0 * m * v * v * y2 + v * v * v
}

/// Range-selector value of unit position `pos` among `n` units (module docs).
pub fn range_value(sel: &RangeSelector, pos: usize, n: usize) -> f64 {
    let scale = match sel.units {
        RangeUnits::Percent => n as f64 / 100.0,
        RangeUnits::Index => 1.0,
    };
    let (a, b) = if sel.start <= sel.end { (sel.start, sel.end) } else { (sel.end, sel.start) };
    let lo = (a + sel.offset) * scale;
    let hi = (b + sel.offset) * scale;
    let c = pos as f64 + 0.5;
    let raw = match sel.shape {
        RangeShape::Square => {
            let w = sel.smoothness / 100.0;
            if w <= 0.0 {
                if c >= lo && c < hi { 1.0 } else { 0.0 }
            } else {
                let (x0, x1) = (c - w / 2.0, c + w / 2.0);
                ((x1.min(hi) - x0.max(lo)).max(0.0) / w).min(1.0)
            }
        }
        shape => {
            if hi <= lo {
                return 0.0;
            }
            let t = (c - lo) / (hi - lo);
            match shape {
                RangeShape::RampUp => t.clamp(0.0, 1.0),
                RangeShape::RampDown => 1.0 - t.clamp(0.0, 1.0),
                _ if !(0.0..=1.0).contains(&t) => 0.0,
                RangeShape::Triangle => 1.0 - (2.0 * t - 1.0).abs(),
                RangeShape::Round => (1.0 - (2.0 * t - 1.0) * (2.0 * t - 1.0)).max(0.0).sqrt(),
                _ => (1.0 - cos_turn(t)) / 2.0,
            }
        }
    };
    ease_curve(raw.clamp(0.0, 1.0), sel.ease_low, sel.ease_high) * sel.amount / 100.0
}

fn wiggly_value(sel: &WigglySelector, unit: usize, dim: usize, time_s: f64) -> f64 {
    let d = if sel.lock_dimensions { 0 } else { dim as u64 };
    let x = time_s * sel.wiggles_per_second + sel.temporal_phase / 360.0;
    let own_x = x + unit as f64 * sel.spatial_phase / 360.0;
    let common = value_noise(sel.random_seed, d, x);
    let own = value_noise(sel.random_seed, 3 + (unit as u64).wrapping_mul(3).wrapping_add(d), own_x);
    let c = sel.correlation / 100.0;
    let n = (c * common + (1.0 - c) * own).clamp(-1.0, 1.0);
    (sel.min_amount + (sel.max_amount - sel.min_amount) * (n + 1.0) / 2.0) / 100.0
}

/// Per-character amounts `[x, y, z]` of one animator's selectors (multiplicative; no selector = 1).
pub fn selector_amounts(
    chars: &[TextChar],
    selectors: &[Selector],
    time_s: f64,
) -> Result<Vec<[f64; 3]>, MotionError> {
    if chars.len() > MAX_TEXT_UNITS {
        return Err(MotionError::CountLimit { what: "text_units" });
    }
    if selectors.len() > MAX_SELECTORS {
        return Err(MotionError::CountLimit { what: "text_selectors" });
    }
    if !time_s.is_finite() {
        return Err(MotionError::NonFinite);
    }
    for s in selectors {
        match s {
            Selector::Range(r) => r.validate()?,
            Selector::Wiggly(w) => w.validate()?,
        }
    }
    let mut out = vec![[1.0; 3]; chars.len()];
    for s in selectors {
        match s {
            Selector::Range(r) => {
                let (units, n) = units_of(chars, r.based_on);
                let perm = r.randomize_order.then(|| permutation(n, r.random_seed));
                for (o, u) in out.iter_mut().zip(&units) {
                    let v = match u {
                        None => 0.0,
                        Some(i) => range_value(r, perm.as_ref().map_or(*i, |p| p[*i]), n),
                    };
                    for d in o.iter_mut() {
                        *d *= v;
                    }
                }
            }
            Selector::Wiggly(w) => {
                let (units, _) = units_of(chars, w.based_on);
                for (o, u) in out.iter_mut().zip(&units) {
                    for (dim, d) in o.iter_mut().enumerate() {
                        *d *= match u {
                            None => 0.0,
                            Some(i) => wiggly_value(w, *i, dim, time_s),
                        };
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Resolves every animator for every character at `time_s` seconds (module docs). `base_fill` is
/// the layer fill the first fill animator lerps from.
pub fn evaluate_text_animators(
    chars: &[TextChar],
    animators: &[Animator],
    base_fill: [f64; 4],
    time_s: f64,
) -> Result<Vec<CharTransform>, MotionError> {
    if chars.len() > MAX_TEXT_UNITS {
        return Err(MotionError::CountLimit { what: "text_units" });
    }
    if animators.len() > MAX_ANIMATORS {
        return Err(MotionError::CountLimit { what: "text_animators" });
    }
    for a in animators {
        a.properties.validate()?;
    }
    for v in base_fill {
        finite_in(v, 0.0, 1.0, "base_fill")?;
    }
    let mut out = vec![CharTransform::IDENTITY; chars.len()];
    for a in animators {
        let amounts = selector_amounts(chars, &a.selectors, time_s)?;
        let p = &a.properties;
        for (t, am) in out.iter_mut().zip(&amounts) {
            if let Some(pos) = p.position {
                t.position[0] += am[0] * pos[0];
                t.position[1] += am[1] * pos[1];
            }
            if let Some(s) = p.scale {
                t.scale[0] *= (100.0 + am[0] * (s[0] - 100.0)) / 100.0;
                t.scale[1] *= (100.0 + am[1] * (s[1] - 100.0)) / 100.0;
            }
            if let Some(r) = p.rotation {
                t.rotation += am[0] * r;
            }
            if let Some(o) = p.opacity {
                t.opacity *= (100.0 + am[0] * (o - 100.0)) / 100.0;
            }
            if let Some(f) = p.fill
                && am[0] > 0.0
            {
                let from = t.fill.unwrap_or(base_fill);
                let mut mixed = [0.0; 4];
                for i in 0..4 {
                    mixed[i] = from[i] + (f[i] - from[i]) * am[0];
                }
                t.fill = Some(mixed);
            }
            if let Some(tr) = p.tracking {
                t.tracking += am[0] * tr;
            }
        }
    }
    Ok(out)
}

/// Animatable parameters of an animator (each an ordinary keyframable `Property`, STU-MOT-092).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimatorParam {
    /// Selector `index` range start / end / offset / amount (1 component).
    RangeStart(usize),
    RangeEnd(usize),
    RangeOffset(usize),
    RangeAmount(usize),
    /// Selector `index` wiggly max / min amount, wiggles per second (1 component).
    WigglyMax(usize),
    WigglyMin(usize),
    WigglyFrequency(usize),
    Position,
    Scale,
    Rotation,
    Opacity,
    Fill,
    Tracking,
}

impl AnimatorParam {
    fn dimension(self) -> usize {
        match self {
            Self::Position | Self::Scale => 2,
            Self::Fill => 4,
            _ => 1,
        }
    }
}

/// An animator whose parameters are driven by motion properties.
#[derive(Clone, Debug, Default)]
pub struct AnimatorTrack {
    pub base: Animator,
    pub streams: Vec<(AnimatorParam, Property)>,
}

impl AnimatorTrack {
    /// The animator with every stream sampled at `tick` (`Property::sample`, keyframes only).
    pub fn at(&self, tick: EvalTick, cancel: &CancellationToken) -> Result<Animator, MotionError> {
        let mut a = self.base.clone();
        for (param, p) in &self.streams {
            if p.dimension() != param.dimension() {
                return Err(MotionError::ComponentMismatch);
            }
            let v = p.sample(tick, cancel)?;
            match *param {
                AnimatorParam::RangeStart(i)
                | AnimatorParam::RangeEnd(i)
                | AnimatorParam::RangeOffset(i)
                | AnimatorParam::RangeAmount(i) => {
                    let Some(Selector::Range(r)) = a.selectors.get_mut(i) else {
                        return Err(MotionError::SelectionOutOfRange);
                    };
                    match *param {
                        AnimatorParam::RangeStart(_) => r.start = v[0],
                        AnimatorParam::RangeEnd(_) => r.end = v[0],
                        AnimatorParam::RangeOffset(_) => r.offset = v[0],
                        _ => r.amount = v[0],
                    }
                }
                AnimatorParam::WigglyMax(i) | AnimatorParam::WigglyMin(i) | AnimatorParam::WigglyFrequency(i) => {
                    let Some(Selector::Wiggly(w)) = a.selectors.get_mut(i) else {
                        return Err(MotionError::SelectionOutOfRange);
                    };
                    match *param {
                        AnimatorParam::WigglyMax(_) => w.max_amount = v[0],
                        AnimatorParam::WigglyMin(_) => w.min_amount = v[0],
                        _ => w.wiggles_per_second = v[0],
                    }
                }
                AnimatorParam::Position => a.properties.position = Some([v[0], v[1]]),
                AnimatorParam::Scale => a.properties.scale = Some([v[0], v[1]]),
                AnimatorParam::Rotation => a.properties.rotation = Some(v[0]),
                AnimatorParam::Opacity => a.properties.opacity = Some(v[0]),
                AnimatorParam::Fill => a.properties.fill = Some([v[0], v[1], v[2], v[3]]),
                AnimatorParam::Tracking => a.properties.tracking = Some(v[0]),
            }
        }
        Ok(a)
    }
}
