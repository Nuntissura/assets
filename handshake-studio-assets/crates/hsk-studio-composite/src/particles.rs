//! Particle world layer (lane SMB-AE-PARTICLES-AUDIO-PRESETS slice 1; STU-CMP-075 "Particle world:
//! a full particle system with birth rate, longevity, producer position and radius, velocity,
//! gravity, resistance, per-particle type, and size, opacity and colour over life, under a physics
//! model"; STU-CMP-075a statefulness; STU-FX-011 explicit integer seed).
//!
//! **Model.** A [`ParticleWorld`] has a deterministic start tick, an [`Emitter`] (point, box or a
//! layer's alpha), a birth-rate [`Track`] (particles per second), longevity with a random share,
//! direction / spread / speed, gravity, air resistance, an optional seeded [`Turbulence`] field
//! (curl of a seeded value-noise potential, so it stirs without collapsing particles into sinks),
//! size / opacity over life ([`LifeCurve`]), a birth -> death colour, and a [`ParticleKind`] (an
//! antialiased dot or a sprite drawn from a layer's pixels).
//!
//! **Time grid and births.** Local time `T` is seconds since the start tick. The simulation grid is
//! `g_s = s dt`, `dt = ticks_per_frame / substeps` (in seconds). Step `s` births
//! `floor(I(g_{s+1})) - floor(I(g_s))` particles, `I` the exact integral of the piecewise-linear
//! birth rate from the start; particle `i` (a global birth index) is born at `g_s + u dt`, `u` its
//! own seeded draw. Every per-particle attribute comes from a splitmix64 stream keyed by
//! `(seed, i)`, so a particle is the same particle whatever frame asks for it.
//!
//! **Random access (STU-CMP-075a).** Particles never interact, so a particle's state at `T` depends
//! only on its own birth and on the force field (a function of position and time). A frame is
//! therefore computed by simulating ONLY the particles alive at `T` (births in `(T - max life, T]`)
//! from their own birth, on the shared grid (first a partial step to the next grid point, whole
//! grid steps, then a partial step to `T`). Grid states do not depend on the frame requested, so a
//! scrub backwards, a jump and forward play produce bit-identical frames, and no frame depends on
//! simulating from frame 0; a checkpoint cache is unnecessary (the cost of a frame is bounded by
//! alive particles x steps of the longest life, checked before any work). Integration is
//! semi-implicit Euler: `v' = (v + a h) exp(-k h)`, `p' = p + v' h`, `a` = gravity + turbulence at
//! the step start.
//!
//! **Raster.** Particles draw in ascending birth index (older under newer) with premultiplied
//! source-over into an `f32` RGBA plane. A dot of diameter `d` has coverage
//! `clamp(d/2 + 0.5 - dist, 0, 1)` at each pixel centre (a dot under 1 px is a 1 px dot with alpha
//! scaled by `d^2`); a sprite is scaled so its longer side is `d`, sampled bilinearly with a
//! transparent border and tinted by the particle colour. Motion blur goes through the composition
//! shutter ([`ParticleShutter::from_settings`] over Folio `StudioCompositionMotionSettings`, the same
//! sample ticks `t + round(tpf (phase + angle (k + 0.5) / n) / 360)` as the composition adapter's
//! 3D motion blur), averaging the premultiplied sample frames.
//!
//! **Output.** [`ParticleFrame::content`] is a [`StaticContent`] (one owned straight tile under a
//! full-coverage mask through `text::painted`), so a composition scene layer draws it through the
//! existing pigment routes on render-cpu and render-gpu alike; the simulation and raster are CPU.
//!
//! **Bounds (V4-01/02).** Every parameter must be finite and in its stated range; the alive count,
//! the window steps, the integration work and the raster work are estimated and refused BEFORE any
//! allocation ([`ParticleError`]); a state that becomes non-finite is refused, never dropped
//! silently. After Effects parity (CC Particle World / Particular behaviour, direction convention,
//! draw order, dot falloff) is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use crate::composition::StaticContent;
use crate::text::painted::{DEFAULT_MAX_PAINTED_BYTES, PaintedPlane, painted_content};
use crate::vector_nib::PaintedTile;
use crate::CompositeError;
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND};
use hsk_studio_nib::raster::{CoveragePlane, PixelFrame};
use hsk_studio_pigment::render_plan::ColourTag;
use hsk_studio_pigment::Rect;
use std::fmt;

/// Most keys of one [`Track`].
pub const MAX_TRACK_KEYS: usize = 4096;
/// Most points of one [`LifeCurve`].
pub const MAX_CURVE_POINTS: usize = 256;
/// Most substeps per frame.
pub const MAX_SUBSTEPS: u32 = 16;
/// Longest particle life in seconds.
pub const MAX_LIFE_SECONDS: f64 = 600.0;
/// Largest birth rate (particles per second).
pub const MAX_BIRTH_RATE: f64 = 1.0e9;
/// Largest dot or sprite size in pixels.
pub const MAX_PARTICLE_SIZE: f64 = 4096.0;
/// Largest emitter-layer or sprite pixel count.
pub const MAX_SOURCE_PIXELS: u64 = 1 << 24;
/// Hard ceiling on [`ParticleLimits::max_alive`].
pub const HARD_MAX_ALIVE: u64 = 1 << 20;
/// Most shutter samples (STU-MOT-004 `samples_per_frame` hard max 64).
pub const MAX_SHUTTER_SAMPLES: u32 = 64;

/// A typed refusal; nothing was allocated for the frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParticleError {
    /// A parameter is non-finite or outside its range (names the field).
    Parameter(&'static str),
    /// The layer emitter has no positive alpha.
    EmitterEmpty,
    /// The sprite's pixel count is not `width * height` or is over the bound.
    Sprite,
    /// The estimated alive count is over [`ParticleLimits::max_alive`].
    Count,
    /// The birth window spans more grid steps than [`ParticleLimits::max_window_steps`].
    Window,
    /// Integration or raster work over its budget.
    Work,
    /// The raster is empty or over [`ParticleLimits::max_pixels`].
    Raster,
    /// A particle state became non-finite.
    NonFinite,
    Canceled,
}

impl ParticleError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Parameter(_) => "particle_parameter",
            Self::EmitterEmpty => "particle_emitter_empty",
            Self::Sprite => "particle_sprite",
            Self::Count => "particle_count",
            Self::Window => "particle_window",
            Self::Work => "particle_work",
            Self::Raster => "particle_raster",
            Self::NonFinite => "particle_non_finite",
            Self::Canceled => "canceled",
        }
    }
}

impl fmt::Display for ParticleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parameter(p) => write!(f, "{}:{p}", self.code()),
            _ => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for ParticleError {}

impl From<ParticleError> for CompositeError {
    fn from(e: ParticleError) -> Self {
        match e {
            ParticleError::Canceled => Self::Canceled,
            other => Self::Unavailable(other.to_string()),
        }
    }
}

type Result<T> = std::result::Result<T, ParticleError>;

fn finite_in(v: f64, lo: f64, hi: f64, field: &'static str) -> Result<()> {
    if v.is_finite() && (lo..=hi).contains(&v) { Ok(()) } else { Err(ParticleError::Parameter(field)) }
}

/// Linear between keys, constant outside; `keys` non-empty with strictly increasing times.
fn piecewise(k: &[(f64, f64)], t: f64) -> f64 {
    let Some(&(t0, v0)) = k.first() else { return 1.0 };
    if t <= t0 {
        return v0;
    }
    let i = k.partition_point(|(kt, _)| *kt <= t);
    if i >= k.len() {
        return k[k.len().saturating_sub(1)].1;
    }
    let (ta, va) = k[i.saturating_sub(1)];
    let (tb, vb) = k[i];
    va + (vb - va) * (t - ta) / (tb - ta)
}

/// A piecewise-linear value over local seconds (constant before the first and after the last key).
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    /// `(seconds since the world start, value)`, strictly increasing times, at least one key.
    pub keys: Vec<(f64, f64)>,
}

impl Track {
    pub fn constant(v: f64) -> Self {
        Self { keys: vec![(0.0, v)] }
    }

    fn validate(&self, lo: f64, hi: f64, field: &'static str) -> Result<()> {
        if self.keys.is_empty() || self.keys.len() > MAX_TRACK_KEYS {
            return Err(ParticleError::Parameter(field));
        }
        let mut prev = f64::NEG_INFINITY;
        for &(t, v) in &self.keys {
            if !t.is_finite() || t.abs() > 1.0e9 || t <= prev {
                return Err(ParticleError::Parameter(field));
            }
            finite_in(v, lo, hi, field)?;
            prev = t;
        }
        Ok(())
    }

    pub fn value(&self, t: f64) -> f64 {
        piecewise(&self.keys, t)
    }

    /// Antiderivative with `F(first key) = 0` (constant extrapolation on both sides).
    fn antiderivative(&self, t: f64) -> f64 {
        let k = &self.keys;
        let (t0, v0) = k[0];
        if t <= t0 {
            return v0 * (t - t0);
        }
        let mut acc = 0.0;
        for w in k.windows(2) {
            let ((ta, va), (tb, vb)) = (w[0], w[1]);
            if t >= tb {
                acc += 0.5 * (va + vb) * (tb - ta);
            } else {
                let vt = va + (vb - va) * (t - ta) / (tb - ta);
                return acc + 0.5 * (va + vt) * (t - ta);
            }
        }
        let (tl, vl) = k[k.len().saturating_sub(1)];
        acc + vl * (t - tl)
    }

    /// `\int_0^t` of the track (`t >= 0`).
    fn integral(&self, t: f64) -> f64 {
        self.antiderivative(t) - self.antiderivative(0.0)
    }
}

/// A piecewise-linear multiplier over normalised age `0..=1`; no points = constant 1.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LifeCurve {
    /// `(age in [0, 1], value)`, strictly increasing ages.
    pub points: Vec<(f64, f64)>,
}

impl LifeCurve {
    fn validate(&self, hi: f64, field: &'static str) -> Result<()> {
        if self.points.len() > MAX_CURVE_POINTS {
            return Err(ParticleError::Parameter(field));
        }
        let mut prev = f64::NEG_INFINITY;
        for &(a, v) in &self.points {
            if !a.is_finite() || !(0.0..=1.0).contains(&a) || a <= prev {
                return Err(ParticleError::Parameter(field));
            }
            finite_in(v, 0.0, hi, field)?;
            prev = a;
        }
        Ok(())
    }

    pub fn value(&self, age: f64) -> f64 {
        if self.points.is_empty() {
            return 1.0;
        }
        piecewise(&self.points, age)
    }
}

/// An emitter layer's alpha in layer pixels, its top-left at the emitter position.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerEmitter {
    pub width: u32,
    pub height: u32,
    /// Row-major alpha in `[0, 1]`; births are distributed in proportion to it.
    pub alpha: Vec<f32>,
}

/// Where particles are born.
#[derive(Clone, Debug, PartialEq)]
pub enum EmitterShape {
    /// At the emitter position.
    Point,
    /// Uniformly in a box of `size` centred on the emitter position.
    Box { size: [f64; 2] },
    /// From a layer's pixels in proportion to their alpha (layer emitter).
    Layer(LayerEmitter),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Emitter {
    pub shape: EmitterShape,
    /// Composition pixels, animated in local seconds.
    pub position: [Track; 2],
}

/// A sprite drawn from a layer's pixels (straight RGBA, row-major).
#[derive(Clone, Debug, PartialEq)]
pub struct Sprite {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<[f32; 4]>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParticleKind {
    Dot,
    Sprite(Sprite),
}

/// The seeded turbulence field: the curl of a value-noise potential over
/// `(x / scale, y / scale, t * evolution)`, times `amplitude` (px/s^2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Turbulence {
    pub amplitude: f64,
    pub scale: f64,
    pub evolution: f64,
}

/// One particle system layer (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct ParticleWorld {
    /// STU-FX-011 explicit seed.
    pub seed: u64,
    /// The deterministic start (STU-CMP-075a), composition ticks.
    pub start_tick: u64,
    /// Integration substeps per frame, `1..=16`.
    pub substeps: u32,
    pub emitter: Emitter,
    /// Particles per second, `0..=1e9`.
    pub birth_rate: Track,
    /// Seconds, `(0, 600]`.
    pub life: f64,
    /// Share `0..=1`: life is `life (1 + r (2u - 1))`.
    pub life_random: f64,
    /// Degrees, 0 = up, clockwise.
    pub direction: f64,
    /// Degrees `0..=360` centred on `direction`.
    pub spread: f64,
    /// Pixels per second.
    pub velocity: f64,
    pub velocity_random: f64,
    /// Pixels per second squared (+y is down).
    pub gravity: [f64; 2],
    /// Per second, `0..=1000`: `v *= exp(-k h)`.
    pub air_resistance: f64,
    pub turbulence: Option<Turbulence>,
    /// Pixels, `0..=4096`.
    pub size: f64,
    pub size_random: f64,
    /// Multiplier `0..=1000`.
    pub size_over_life: LifeCurve,
    /// `0..=1`.
    pub opacity: f64,
    /// Multiplier `0..=1`.
    pub opacity_over_life: LifeCurve,
    /// Straight RGB `0..=1` at birth and at death, linear in age.
    pub colour_birth: [f32; 3],
    pub colour_death: [f32; 3],
    pub kind: ParticleKind,
}

impl ParticleWorld {
    /// A white 4 px dot point emitter at `(x, y)`: 100 particles per second, 1 s life, 100 px/s up
    /// with a full-circle spread, no gravity, resistance or turbulence.
    pub fn point(seed: u64, x: f64, y: f64) -> Self {
        Self {
            seed,
            start_tick: 0,
            substeps: 1,
            emitter: Emitter { shape: EmitterShape::Point, position: [Track::constant(x), Track::constant(y)] },
            birth_rate: Track::constant(100.0),
            life: 1.0,
            life_random: 0.0,
            direction: 0.0,
            spread: 360.0,
            velocity: 100.0,
            velocity_random: 0.0,
            gravity: [0.0; 2],
            air_resistance: 0.0,
            turbulence: None,
            size: 4.0,
            size_random: 0.0,
            size_over_life: LifeCurve::default(),
            opacity: 1.0,
            opacity_over_life: LifeCurve::default(),
            colour_birth: [1.0; 3],
            colour_death: [1.0; 3],
            kind: ParticleKind::Dot,
        }
    }

    /// Every parameter finite and in range (module docs, V4-01).
    pub fn validate(&self) -> Result<()> {
        if !(1..=MAX_SUBSTEPS).contains(&self.substeps) {
            return Err(ParticleError::Parameter("substeps"));
        }
        self.emitter.position[0].validate(-1.0e7, 1.0e7, "emitter_position")?;
        self.emitter.position[1].validate(-1.0e7, 1.0e7, "emitter_position")?;
        match &self.emitter.shape {
            EmitterShape::Point => {}
            EmitterShape::Box { size } => {
                finite_in(size[0], 0.0, 1.0e7, "emitter_box")?;
                finite_in(size[1], 0.0, 1.0e7, "emitter_box")?;
            }
            EmitterShape::Layer(l) => {
                let n = u64::from(l.width).checked_mul(u64::from(l.height)).ok_or(ParticleError::Parameter("emitter_layer"))?;
                if n == 0 || n > MAX_SOURCE_PIXELS || u64::try_from(l.alpha.len()).ok() != Some(n) {
                    return Err(ParticleError::Parameter("emitter_layer"));
                }
                if l.alpha.iter().any(|a| !a.is_finite() || !(0.0..=1.0).contains(a)) {
                    return Err(ParticleError::Parameter("emitter_layer"));
                }
                if !l.alpha.iter().any(|a| *a > 0.0) {
                    return Err(ParticleError::EmitterEmpty);
                }
            }
        }
        self.birth_rate.validate(0.0, MAX_BIRTH_RATE, "birth_rate")?;
        if !self.life.is_finite() || self.life <= 0.0 || self.life > MAX_LIFE_SECONDS {
            return Err(ParticleError::Parameter("life"));
        }
        finite_in(self.life_random, 0.0, 1.0, "life_random")?;
        finite_in(self.direction, -1.0e6, 1.0e6, "direction")?;
        finite_in(self.spread, 0.0, 360.0, "spread")?;
        finite_in(self.velocity, -1.0e6, 1.0e6, "velocity")?;
        finite_in(self.velocity_random, 0.0, 1.0, "velocity_random")?;
        finite_in(self.gravity[0], -1.0e6, 1.0e6, "gravity")?;
        finite_in(self.gravity[1], -1.0e6, 1.0e6, "gravity")?;
        finite_in(self.air_resistance, 0.0, 1.0e3, "air_resistance")?;
        if let Some(t) = &self.turbulence {
            finite_in(t.amplitude, 0.0, 1.0e6, "turbulence")?;
            finite_in(t.scale, 1.0e-3, 1.0e6, "turbulence")?;
            finite_in(t.evolution, 0.0, 1.0e3, "turbulence")?;
        }
        finite_in(self.size, 0.0, MAX_PARTICLE_SIZE, "size")?;
        finite_in(self.size_random, 0.0, 1.0, "size_random")?;
        self.size_over_life.validate(1.0e3, "size_over_life")?;
        finite_in(self.opacity, 0.0, 1.0, "opacity")?;
        self.opacity_over_life.validate(1.0, "opacity_over_life")?;
        for c in self.colour_birth.iter().chain(&self.colour_death) {
            finite_in(f64::from(*c), 0.0, 1.0, "colour")?;
        }
        if let ParticleKind::Sprite(s) = &self.kind {
            let n = u64::from(s.width).checked_mul(u64::from(s.height)).ok_or(ParticleError::Sprite)?;
            if n == 0 || n > MAX_SOURCE_PIXELS || u64::try_from(s.rgba.len()).ok() != Some(n) {
                return Err(ParticleError::Sprite);
            }
            if s.rgba.iter().flatten().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)) {
                return Err(ParticleError::Parameter("sprite_pixels"));
            }
        }
        Ok(())
    }
}

/// Work bounds of one frame (V4-02), checked before any allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParticleLimits {
    /// Particles alive at one sample time (clamped to [`HARD_MAX_ALIVE`]).
    pub max_alive: u64,
    /// Grid steps one birth window may span.
    pub max_window_steps: u64,
    /// Integration steps of one sample (alive x longest-life steps).
    pub max_steps: u64,
    /// Raster pixels.
    pub max_pixels: u64,
    /// Pixel visits of all particle footprints of one sample.
    pub max_raster_work: u64,
}

impl Default for ParticleLimits {
    fn default() -> Self {
        Self { max_alive: 262_144, max_window_steps: 1 << 22, max_steps: 1 << 27, max_pixels: 1 << 26, max_raster_work: 1 << 30 }
    }
}

/// The composition shutter for a particle layer (Folio `StudioCompositionMotionSettings`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParticleShutter {
    /// Degrees `0..=720`.
    pub angle: f64,
    /// Degrees `-360..=360`.
    pub phase: f64,
    /// `1..=64`.
    pub samples: u32,
}

impl ParticleShutter {
    /// The shutter of a composition whose motion blur is on with an angle above 0 and whose
    /// blurred layers include `layer_id`; `None` otherwise (a single sample at the frame tick).
    pub fn from_settings(s: &hsk_studio_folio::StudioCompositionMotionSettings, layer_id: &str) -> Option<Self> {
        (s.motion_blur && s.shutter_angle > 0.0 && s.motion_blur_layers.iter().any(|l| l == layer_id))
            .then_some(Self { angle: s.shutter_angle, phase: s.shutter_phase, samples: s.samples_per_frame })
    }

    fn validate(&self) -> Result<()> {
        finite_in(self.angle, 0.0, 720.0, "shutter_angle")?;
        finite_in(self.phase, -360.0, 360.0, "shutter_phase")?;
        if !(1..=MAX_SHUTTER_SAMPLES).contains(&self.samples) {
            return Err(ParticleError::Parameter("shutter_samples"));
        }
        Ok(())
    }

    /// Sample ticks of the frame at `tick` (composition adapter rule; may be before 0).
    pub fn sample_ticks(&self, tick: u64, ticks_per_frame: u64) -> Vec<i128> {
        let n = f64::from(self.samples);
        (0..self.samples)
            .map(|k| {
                let off = (ticks_per_frame as f64 * (self.phase + self.angle * (f64::from(k) + 0.5) / n) / 360.0).round();
                i128::from(tick).saturating_add(off as i128)
            })
            .collect()
    }
}

/// One particle at a sample time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParticleState {
    /// Global birth index (draw order).
    pub index: u64,
    pub position: [f64; 2],
    pub velocity: [f64; 2],
    /// Seconds since birth.
    pub age: f64,
    /// This particle's life in seconds.
    pub life: f64,
    /// Current size in pixels (birth size x size over life).
    pub size: f64,
    /// Current opacity `0..=1`.
    pub opacity: f64,
    /// Current straight RGB.
    pub colour: [f32; 3],
}

// ---- Seeded randomness and noise ----

fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

struct Draws(u64);

impl Draws {
    fn new(seed: u64, index: u64) -> Self {
        Self(splitmix64(seed ^ splitmix64(index)))
    }
    /// Uniform in `[0, 1)` (53 bits).
    fn uniform(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        (splitmix64(self.0) >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn lattice(seed: u64, x: i64, y: i64, z: i64) -> f64 {
    let h = splitmix64(
        seed ^ (x as u64).wrapping_mul(0x8CB9_2BA7_2F3D_8DD7)
            ^ (y as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93)
            ^ (z as u64).wrapping_mul(0xA076_1D64_78BD_642F),
    );
    (h >> 11) as f64 / (1u64 << 52) as f64 - 1.0
}

/// Value noise and its x / y partial derivatives (quintic fade, trilinear).
fn noise_grad(seed: u64, x: f64, y: f64, z: f64) -> (f64, f64) {
    let (fx, fy, fz) = (x.floor(), y.floor(), z.floor());
    let (tx, ty, tz) = (x - fx, y - fy, z - fz);
    let fade = |t: f64| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    let dfade = |t: f64| 30.0 * t * t * (t - 1.0) * (t - 1.0);
    let (ux, uy, uz) = (fade(tx), fade(ty), fade(tz));
    let (dx, dy) = (dfade(tx), dfade(ty));
    let (ix, iy, iz) = (fx as i64, fy as i64, fz as i64);
    let c = |a: i64, b: i64, d: i64| lattice(seed, ix.wrapping_add(a), iy.wrapping_add(b), iz.wrapping_add(d));
    let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
    // Values on the z-interpolated face.
    let v = |a: i64, b: i64| lerp(c(a, b, 0), c(a, b, 1), uz);
    let (v00, v10, v01, v11) = (v(0, 0), v(1, 0), v(0, 1), v(1, 1));
    let ddx = dx * lerp(v10 - v00, v11 - v01, uy);
    let ddy = dy * lerp(v01 - v00, v11 - v10, ux);
    (ddx, ddy)
}

// ---- Simulation ----

/// Seconds of a signed tick offset from the world start.
fn local_seconds(world: &ParticleWorld, tick: i128) -> f64 {
    let rel = tick.saturating_sub(i128::from(world.start_tick));
    rel as f64 / TICKS_PER_SECOND as f64
}

fn accel(world: &ParticleWorld, p: [f64; 2], t: f64) -> [f64; 2] {
    let mut a = world.gravity;
    if let Some(tu) = world.turbulence
        && tu.amplitude > 0.0
    {
        let (dx, dy) = noise_grad(splitmix64(world.seed ^ 0x7475_7262), p[0] / tu.scale, p[1] / tu.scale, t * tu.evolution);
        a[0] += tu.amplitude * dy;
        a[1] -= tu.amplitude * dx;
    }
    a
}

fn step(world: &ParticleWorld, p: &mut [f64; 2], v: &mut [f64; 2], t: f64, h: f64) {
    if h <= 0.0 {
        return;
    }
    let a = accel(world, *p, t);
    let damp = (-world.air_resistance * h).exp();
    for ((vi, pi), ai) in v.iter_mut().zip(p.iter_mut()).zip(a) {
        *vi = (*vi + ai * h) * damp;
        *pi += *vi * h;
    }
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f64) -> [f32; 3] {
    let t = t as f32;
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// Births at an index: (birth time, life, position, velocity, birth size).
fn born(world: &ParticleWorld, index: u64, g_s: f64, dt: f64, cdf: Option<&[f64]>) -> (f64, f64, [f64; 2], [f64; 2], f64) {
    let mut d = Draws::new(world.seed, index);
    let tau = g_s + d.uniform() * dt;
    let life = world.life * (1.0 + world.life_random * (2.0 * d.uniform() - 1.0));
    let theta = (world.direction + world.spread * (d.uniform() - 0.5)).to_radians();
    let speed = world.velocity * (1.0 + world.velocity_random * (2.0 * d.uniform() - 1.0));
    let size = world.size * (1.0 + world.size_random * (2.0 * d.uniform() - 1.0));
    let (u1, u2, u3) = (d.uniform(), d.uniform(), d.uniform());
    let base = [world.emitter.position[0].value(tau), world.emitter.position[1].value(tau)];
    let pos = match &world.emitter.shape {
        EmitterShape::Point => base,
        EmitterShape::Box { size } => [base[0] + (u1 - 0.5) * size[0], base[1] + (u2 - 0.5) * size[1]],
        EmitterShape::Layer(l) => {
            let cdf = cdf.unwrap_or(&[]);
            let total = cdf.last().copied().unwrap_or(0.0);
            let i = cdf.partition_point(|c| *c <= u1 * total).min(cdf.len().saturating_sub(1));
            let w = l.width.max(1) as usize;
            let (px, py) = (i.checked_rem(w).unwrap_or(0) as f64, i.checked_div(w).unwrap_or(0) as f64);
            [base[0] + px + u2, base[1] + py + u3]
        }
    };
    (tau, life, pos, [speed * theta.sin(), -speed * theta.cos()], size)
}

fn layer_cdf(world: &ParticleWorld) -> Option<Vec<f64>> {
    match &world.emitter.shape {
        EmitterShape::Layer(l) => {
            let mut acc = 0.0;
            Some(l.alpha.iter().map(|a| {
                acc += f64::from(*a);
                acc
            }).collect())
        }
        _ => None,
    }
}

/// The particles alive at composition tick `tick` (ascending birth index). Validates the world.
pub fn particles_at(
    world: &ParticleWorld,
    tick: i128,
    ticks_per_frame: u64,
    limits: &ParticleLimits,
    cancel: &CancellationToken,
) -> Result<Vec<ParticleState>> {
    world.validate()?;
    simulate(world, tick, ticks_per_frame, limits, layer_cdf(world).as_deref(), cancel)
}

fn simulate(
    world: &ParticleWorld,
    tick: i128,
    ticks_per_frame: u64,
    limits: &ParticleLimits,
    cdf: Option<&[f64]>,
    cancel: &CancellationToken,
) -> Result<Vec<ParticleState>> {
    cancel.check().map_err(|_| ParticleError::Canceled)?;
    if ticks_per_frame == 0 {
        return Err(ParticleError::Parameter("ticks_per_frame"));
    }
    let t = local_seconds(world, tick);
    if t < 0.0 {
        return Ok(Vec::new());
    }
    // Seconds per grid step.
    let dt = ticks_per_frame as f64 / f64::from(world.substeps) / TICKS_PER_SECOND as f64;
    let l_max = world.life * (1.0 + world.life_random);
    let lo_t = (t - l_max - dt).max(0.0);
    // Estimates before any allocation.
    let est = world.birth_rate.integral(t + dt).floor() - world.birth_rate.integral(lo_t).floor();
    let max_alive = limits.max_alive.min(HARD_MAX_ALIVE) as f64;
    if !est.is_finite() || est > max_alive {
        return Err(ParticleError::Count);
    }
    let s_hi_f = (t / dt).floor();
    let s_lo_f = ((t - l_max) / dt).floor().max(0.0);
    let window = s_hi_f - s_lo_f + 1.0;
    if !window.is_finite() || window > limits.max_window_steps as f64 {
        return Err(ParticleError::Window);
    }
    let life_steps = (l_max / dt).ceil() + 2.0;
    if est.max(0.0) * life_steps > limits.max_steps as f64 {
        return Err(ParticleError::Work);
    }
    let (s_lo, s_hi) = (s_lo_f as u64, s_hi_f as u64);
    let mut out = Vec::with_capacity(est.max(0.0) as usize);
    let mut s = s_lo;
    while s <= s_hi {
        if s & 0xFFF == 0 {
            cancel.check().map_err(|_| ParticleError::Canceled)?;
        }
        let g_s = s as f64 * dt;
        let g_n = s.saturating_add(1) as f64 * dt;
        let first = world.birth_rate.integral(g_s).floor();
        let last = world.birth_rate.integral(g_n).floor();
        if last > first {
            let (a, b) = (first.max(0.0) as u64, last.max(0.0) as u64);
            for index in a..b {
                let (tau, life, mut p, mut v, size0) = born(world, index, g_s, dt, cdf);
                if tau > t || t - tau >= life {
                    continue;
                }
                if out.len() as f64 >= max_alive {
                    return Err(ParticleError::Count);
                }
                // Partial step to the next grid point, whole grid steps, partial step to t.
                let mut now = tau;
                let mut k = s.saturating_add(1);
                loop {
                    let g_k = k as f64 * dt;
                    if g_k > t {
                        break;
                    }
                    step(world, &mut p, &mut v, now, g_k - now);
                    now = g_k;
                    k = k.saturating_add(1);
                }
                step(world, &mut p, &mut v, now, t - now);
                if !(p[0].is_finite() && p[1].is_finite() && v[0].is_finite() && v[1].is_finite()) {
                    return Err(ParticleError::NonFinite);
                }
                let age = t - tau;
                let a = (age / life).clamp(0.0, 1.0);
                let size = size0 * world.size_over_life.value(a);
                let opacity = world.opacity * world.opacity_over_life.value(a);
                out.push(ParticleState {
                    index,
                    position: p,
                    velocity: v,
                    age,
                    life,
                    size,
                    opacity,
                    colour: lerp3(world.colour_birth, world.colour_death, a),
                });
            }
        }
        s = s.saturating_add(1);
        if s == u64::MAX {
            break;
        }
    }
    Ok(out)
}

// ---- Raster ----

/// One rendered particle frame: premultiplied RGBA, row-major.
#[derive(Clone, Debug, PartialEq)]
pub struct ParticleFrame {
    pub width: u32,
    pub height: u32,
    pub premultiplied: Vec<[f32; 4]>,
    /// Particles drawn, summed over shutter samples.
    pub drawn: u64,
}

impl ParticleFrame {
    /// Premultiplied RGBA at `(x, y)`.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[f32; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.premultiplied.get((y as usize).checked_mul(self.width as usize)?.checked_add(x as usize)?).copied()
    }

    /// SHA-256 of the premultiplied plane (`f32le` RGBA).
    pub fn digest(&self) -> [u8; 32] {
        let bytes: Vec<u8> = self.premultiplied.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
        hsk_studio_prism::profile_hash(&bytes)
    }

    /// The frame as static content over `(0, 0, width, height)` (one straight owned tile under a
    /// coverage mask that is 255 wherever alpha is above 0); `None` when nothing was drawn.
    pub fn content(&self, layer_id: &str, space: ColourTag, revision: u64) -> std::result::Result<Option<StaticContent>, CompositeError> {
        let mut coverage = Vec::with_capacity(self.premultiplied.len());
        let mut colour = Vec::with_capacity(self.premultiplied.len().saturating_mul(12));
        let mut alpha = Vec::with_capacity(self.premultiplied.len().saturating_mul(4));
        for px in &self.premultiplied {
            let a = px[3].clamp(0.0, 1.0);
            coverage.push(if a > 0.0 { 255u8 } else { 0 });
            for c in &px[..3] {
                let s = if a > 0.0 { (c / a).clamp(0.0, 1.0) } else { 0.0 };
                colour.extend_from_slice(&s.to_le_bytes());
            }
            alpha.extend_from_slice(&a.to_le_bytes());
        }
        let placement = Rect { x: 0, y: 0, width: self.width, height: self.height };
        let tile = PaintedTile {
            placement,
            colour_sha256: hsk_studio_prism::profile_hash(&colour),
            alpha_sha256: hsk_studio_prism::profile_hash(&alpha),
            colour,
            alpha,
        };
        let plane = PaintedPlane {
            coverage: CoveragePlane { frame: PixelFrame { x: 0, y: 0, width: self.width, height: self.height }, data: coverage },
            rgba: [1.0; 4],
            opacity: 1.0,
            tile: Some(tile),
        };
        painted_content(layer_id, vec![plane], space, revision, DEFAULT_MAX_PAINTED_BYTES)
    }
}

/// Footprint (x0, y0, x1, y1) clipped to the raster, `None` when empty or outside.
fn footprint(c: [f64; 2], half: [f64; 2], w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
    let x0 = (c[0] - half[0] - 1.0).floor().max(0.0);
    let y0 = (c[1] - half[1] - 1.0).floor().max(0.0);
    let x1 = (c[0] + half[0] + 1.0).ceil().min(f64::from(w));
    let y1 = (c[1] + half[1] + 1.0).ceil().min(f64::from(h));
    (x1 > x0 && y1 > y0).then_some((x0 as u32, y0 as u32, x1 as u32, y1 as u32))
}

fn half_extent(world: &ParticleWorld, size: f64) -> [f64; 2] {
    match &world.kind {
        ParticleKind::Dot => [size.max(1.0) / 2.0; 2],
        ParticleKind::Sprite(s) => {
            let m = f64::from(s.width.max(s.height));
            [size * f64::from(s.width) / m / 2.0, size * f64::from(s.height) / m / 2.0]
        }
    }
}

fn over(dst: &mut [f32; 4], src: [f32; 4]) {
    let k = 1.0 - src[3];
    for (d, s) in dst.iter_mut().zip(src) {
        *d = s + *d * k;
    }
}

fn sprite_sample(s: &Sprite, u: f64, v: f64) -> [f32; 4] {
    // Bilinear over pixel centres, transparent outside.
    let (x, y) = (u - 0.5, v - 0.5);
    let (fx, fy) = (x.floor(), y.floor());
    let (tx, ty) = ((x - fx) as f32, (y - fy) as f32);
    let get = |ix: f64, iy: f64| -> [f32; 4] {
        if ix < 0.0 || iy < 0.0 || ix >= f64::from(s.width) || iy >= f64::from(s.height) {
            return [0.0; 4];
        }
        let p = s.rgba[(iy as usize).wrapping_mul(s.width as usize).wrapping_add(ix as usize)];
        [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]]
    };
    let (a, b, c, d) = (get(fx, fy), get(fx + 1.0, fy), get(fx, fy + 1.0), get(fx + 1.0, fy + 1.0));
    std::array::from_fn(|i| {
        let top = a[i] + (b[i] - a[i]) * tx;
        let bot = c[i] + (d[i] - c[i]) * tx;
        top + (bot - top) * ty
    })
}

fn raster(world: &ParticleWorld, parts: &[ParticleState], w: u32, h: u32, plane: &mut [[f32; 4]], weight: f32, limits: &ParticleLimits) -> Result<()> {
    // Work estimate first (V4-02).
    let mut work = 0u64;
    for p in parts {
        if let Some((x0, y0, x1, y1)) = footprint(p.position, half_extent(world, p.size), w, h) {
            let area = u64::from(x1.saturating_sub(x0)).saturating_mul(u64::from(y1.saturating_sub(y0)));
            work = work.saturating_add(area);
            if work > limits.max_raster_work {
                return Err(ParticleError::Work);
            }
        }
    }
    let mut layer = vec![[0.0f32; 4]; plane.len()];
    for p in parts {
        if p.opacity <= 0.0 || p.size <= 0.0 {
            continue;
        }
        let half = half_extent(world, p.size);
        let Some((x0, y0, x1, y1)) = footprint(p.position, half, w, h) else { continue };
        for y in y0..y1 {
            for x in x0..x1 {
                let (cx, cy) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
                let src = match &world.kind {
                    ParticleKind::Dot => {
                        let r = (p.size / 2.0).max(0.5);
                        let scale = if p.size < 1.0 { p.size * p.size } else { 1.0 };
                        let d = ((cx - p.position[0]).powi(2) + (cy - p.position[1]).powi(2)).sqrt();
                        let cov = (r + 0.5 - d).clamp(0.0, 1.0) * scale * p.opacity;
                        if cov <= 0.0 {
                            continue;
                        }
                        let a = cov as f32;
                        [p.colour[0] * a, p.colour[1] * a, p.colour[2] * a, a]
                    }
                    ParticleKind::Sprite(s) => {
                        let (bw, bh) = (2.0 * half[0], 2.0 * half[1]);
                        let u = (cx - (p.position[0] - half[0])) / bw * f64::from(s.width);
                        let v = (cy - (p.position[1] - half[1])) / bh * f64::from(s.height);
                        let t = sprite_sample(s, u, v);
                        let o = p.opacity as f32;
                        [t[0] * p.colour[0] * o, t[1] * p.colour[1] * o, t[2] * p.colour[2] * o, t[3] * o]
                    }
                };
                over(&mut layer[(y as usize).wrapping_mul(w as usize).wrapping_add(x as usize)], src);
            }
        }
    }
    for (d, s) in plane.iter_mut().zip(&layer) {
        for (dc, sc) in d.iter_mut().zip(s) {
            *dc += sc * weight;
        }
    }
    Ok(())
}

/// Renders the particle layer at composition `tick` over a `width x height` raster (composition
/// pixels), through `shutter` when given (module docs). Validates everything before allocating.
#[allow(clippy::too_many_arguments)]
pub fn render_particles(
    world: &ParticleWorld,
    width: u32,
    height: u32,
    tick: u64,
    ticks_per_frame: u64,
    shutter: Option<&ParticleShutter>,
    limits: &ParticleLimits,
    cancel: &CancellationToken,
) -> Result<ParticleFrame> {
    world.validate()?;
    if let Some(s) = shutter {
        s.validate()?;
    }
    let pixels = u64::from(width).checked_mul(u64::from(height)).ok_or(ParticleError::Raster)?;
    if pixels == 0 || pixels > limits.max_pixels {
        return Err(ParticleError::Raster);
    }
    let ticks = match shutter {
        Some(s) => s.sample_ticks(tick, ticks_per_frame),
        None => vec![i128::from(tick)],
    };
    let cdf = layer_cdf(world);
    // Every sample is simulated before the plane is allocated.
    let mut samples = Vec::with_capacity(ticks.len());
    for t in &ticks {
        samples.push(simulate(world, *t, ticks_per_frame, limits, cdf.as_deref(), cancel)?);
    }
    let mut plane = vec![[0.0f32; 4]; usize::try_from(pixels).map_err(|_| ParticleError::Raster)?];
    let weight = 1.0 / ticks.len() as f32;
    let mut drawn = 0u64;
    for parts in &samples {
        cancel.check().map_err(|_| ParticleError::Canceled)?;
        raster(world, parts, width, height, &mut plane, weight, limits)?;
        drawn = drawn.saturating_add(parts.len() as u64);
    }
    Ok(ParticleFrame { width, height, premultiplied: plane, drawn })
}
