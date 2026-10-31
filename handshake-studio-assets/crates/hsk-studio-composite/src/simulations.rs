//! STU-CMP-075 simulation types beside the particle world (lane SMB-AE-PARTICLES-2): Rain
//! (MT-34184), Snow (MT-34185), Star burst (MT-34189) and Particle systems (classic) (MT-34177).
//!
//! **Identity.** Every simulation is a [`SimulationKind`] with a stable id (`hsk.studio.simulation.*`)
//! and its native Studio name (the STU-CMP-075 capability name). Vendor names are import keys only
//! (STU-CMP-075b, STU-SECTION-003), copied from the module's Simulation effect table (import key
//! column); [`SimulationKind::from_import_key`] maps a key onto the Studio identity and
//! [`SimulationKind::from_id`] accepts only stable ids, so an import key never becomes a Studio name.
//! Particle systems (classic) is its own identity with its own record ([`classic`]), never a preset
//! of the particle world (`tests/simulations.rs::classic_particle_systems_are_their_own_identity_and_record`).
//!
//! **Parameter records (STU-FX-146).** No typed record was recovered for these simulations, so each
//! record is authored here as explicit scope: one [`ParamRow`] per scalar with its unit, hard bounds,
//! separately stored soft bounds (`None` = no soft bound declared, never mirrored from the hard
//! bound), default and provenance `studio_declared`. Validation IS the table: `validate` walks the
//! rows (`validate_rows`), and [`Simulation::set_param`] refuses a value outside a row's hard bounds,
//! so the stored bounds and the enforced bounds cannot drift
//! (`tests/simulations.rs::authored_parameter_rows_are_the_enforced_bounds`).
//!
//! **Statefulness (STU-CMP-075a).** Each type declares a deterministic start tick and an explicit
//! integer seed (STU-FX-011). Every element's state is a closed-form function of its own seeded draws
//! (a splitmix64 stream keyed by seed, layer and element index) and of the time since the start, so
//! any frame is computed directly: scrubbing backwards, a jump and forward play give bit-identical
//! frames, no frame depends on another frame, and no state is carried across frames (the frame
//! scheduler has nothing to parallelise across). Elements are born on a constant-rate slot grid:
//! element `k` is born at `(k + u_k) / rate`, `u_k` its own draw, so the elements alive at `t` are a
//! contiguous index window computed before any allocation.
//!
//! **Output.** Every type renders to a [`ParticleFrame`] (premultiplied `f32` RGBA, source-over in
//! draw order), whose [`ParticleFrame::content`] is the existing `StaticContent` route drawn by
//! render-cpu and render-gpu through pigment; no new render path or device. Terminal outcomes go
//! through composite [`crate::report`] with [`outcome`] (codes only).
//!
//! **Bounds (V4-01/02).** Every parameter is finite and inside its row; the raster size, the alive
//! count and the raster work are checked BEFORE allocation; refusals are [`ParticleError`] codes.
//! After Effects parity of all four types is UNVERIFIED (D5).
//!
//! **Water family (lane SMB-AE-WATER-SIMS-1).** Wave world (MT-34181, [`wave_world`]), Caustics
//! (MT-34182, [`caustics`]) and Bubbles (MT-34186, [`bubbles`]) follow the same identity, record,
//! bounds and output rules. Wave world is a stateful step simulation: every frame is re-simulated
//! from the start tick on a fixed step grid (a Wave world may also resume from a [`WaveState`]
//! checkpoint of the same record, which gives the same field), single-threaded in a fixed order.
//! Bubbles is closed form on the slot grid like Rain. Caustics is a pure function of a
//! [`HeightField`] (a Wave world at the tick, or a caller field) and a bottom image.
//! [`SimulationKind::ALL`] keeps its published five particle-family kinds;
//! [`SimulationKind::EVERY`] lists every kind.
#![deny(clippy::arithmetic_side_effects)]
pub mod bubbles;
pub mod caustics;
pub mod classic;
pub mod precipitation;
pub mod star_burst;
pub mod wave_world;

use crate::particles::{
    HARD_MAX_ALIVE, ParticleError, ParticleFrame, ParticleLimits, ParticleWorld, finite_in, footprint, over,
    render_particles,
};
use crate::{CompositeError, outcome_of};
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND};
use hsk_studio_observe::Outcome;

pub use classic::{ClassicParticleType, ParticleSystemsClassic};
pub use precipitation::{DepthLayer, Precipitation, Rain, Snow};
pub use star_burst::{BurstMode, StarBurst};
pub use wave_world::{EdgeMode, HeightField, ProducerShape, WaveProducer, WaveState, WaveWorld};
pub use caustics::{CausticSurface, Caustics, WaterBottom};
pub use bubbles::{BubbleShading, Bubbles};

pub(crate) type Result<T> = std::result::Result<T, ParticleError>;

/// Provenance of every authored row (STU-FX-146: no recovered record).
pub const PROVENANCE: &str = "studio_declared";
/// Largest slot index a birth window may reach (exact in `f64`).
pub const MAX_SLOT_INDEX: f64 = 4_503_599_627_370_496.0; // 2^52

/// A simulation identity (STU-CMP-075 rows implemented in this crate).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SimulationKind {
    ParticleWorld,
    ParticleSystemsClassic,
    Rain,
    Snow,
    StarBurst,
    WaveWorld,
    Caustics,
    Bubbles,
}

impl SimulationKind {
    /// The particle-family kinds of lane SMB-AE-PARTICLES-2 (published length kept); [`Self::EVERY`]
    /// lists every implemented kind.
    pub const ALL: [Self; 5] = [Self::ParticleWorld, Self::ParticleSystemsClassic, Self::Rain, Self::Snow, Self::StarBurst];
    /// The water family (lane SMB-AE-WATER-SIMS-1).
    pub const WATER: [Self; 3] = [Self::WaveWorld, Self::Caustics, Self::Bubbles];
    /// Every implemented kind: [`Self::ALL`] then [`Self::WATER`].
    pub const EVERY: [Self; 8] = [
        Self::ParticleWorld,
        Self::ParticleSystemsClassic,
        Self::Rain,
        Self::Snow,
        Self::StarBurst,
        Self::WaveWorld,
        Self::Caustics,
        Self::Bubbles,
    ];

    /// The stable id.
    pub const fn id(self) -> &'static str {
        match self {
            Self::ParticleWorld => "hsk.studio.simulation.particle_world",
            Self::ParticleSystemsClassic => "hsk.studio.simulation.particle_systems_classic",
            Self::Rain => "hsk.studio.simulation.rain",
            Self::Snow => "hsk.studio.simulation.snow",
            Self::StarBurst => "hsk.studio.simulation.star_burst",
            Self::WaveWorld => "hsk.studio.simulation.wave_world",
            Self::Caustics => "hsk.studio.simulation.caustics",
            Self::Bubbles => "hsk.studio.simulation.bubbles",
        }
    }

    /// The native Studio name (the STU-CMP-075 capability name).
    pub const fn name(self) -> &'static str {
        match self {
            Self::ParticleWorld => "Particle world",
            Self::ParticleSystemsClassic => "Particle systems (classic)",
            Self::Rain => "Rain",
            Self::Snow => "Snow",
            Self::StarBurst => "Star burst",
            Self::WaveWorld => "Wave world",
            Self::Caustics => "Caustics",
            Self::Bubbles => "Bubbles",
        }
    }

    /// Import keys (provenance only) from the module's Simulation effect table rows of the same
    /// capability; behaviour parity with those effects is UNVERIFIED (D5). `CC Particle Systems II`
    /// is not mapped (its table row does not say which capability it is). Each water kind has one
    /// table row (no obsolete / new pair).
    pub const fn import_keys(self) -> &'static [&'static str] {
        match self {
            Self::ParticleWorld => &["CC Particle World"],
            Self::ParticleSystemsClassic => &["CC PS Classic", "CC PS LE Classic"],
            Self::Rain => &["CC Rain", "CSRainfall"],
            Self::Snow => &["CC Snow", "CSSnowfall"],
            Self::StarBurst => &["CC Star Burst"],
            Self::WaveWorld => &["APC Wave World"],
            Self::Caustics => &["APC Caustics"],
            Self::Bubbles => &["CC Bubbles"],
        }
    }

    /// The identity of a stable id; import keys and names are not ids.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::EVERY.into_iter().find(|k| k.id() == id)
    }

    /// The Studio identity an import key maps onto.
    pub fn from_import_key(key: &str) -> Option<Self> {
        Self::EVERY.into_iter().find(|k| k.import_keys().contains(&key))
    }

    /// The authored parameter rows; `None` for the particle world (its record is the typed
    /// [`ParticleWorld`] struct of lane SMB-AE-PARTICLES-AUDIO-PRESETS). Bubbles includes the
    /// shared `shading.*` rows ([`bubbles::BUBBLE_SHADING_ROWS`]); Wave world's per-producer rows
    /// are [`wave_world::WAVE_PRODUCER_ROWS`].
    pub fn params(self) -> Option<Vec<ParamRow>> {
        match self {
            Self::ParticleWorld => None,
            Self::ParticleSystemsClassic => Some(classic::CLASSIC_ROWS.to_vec()),
            Self::Rain => Some(precipitation::Rain::rows()),
            Self::Snow => Some(precipitation::Snow::rows()),
            Self::StarBurst => Some(star_burst::STAR_BURST_ROWS.to_vec()),
            Self::WaveWorld => Some(wave_world::WAVE_WORLD_ROWS.to_vec()),
            Self::Caustics => Some(caustics::CAUSTICS_ROWS.to_vec()),
            Self::Bubbles => Some(bubbles::Bubbles::rows()),
        }
    }
}

/// Where a row's value lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamScope {
    /// A scalar of the record.
    Record,
    /// A field of every depth layer ([`DepthLayer`]).
    DepthLayer,
}

/// One authored parameter (STU-FX-146). Hard bounds are enforced (inclusive); soft bounds are the
/// suggested editing range, stored separately, `None` where no soft bound is declared.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamRow {
    pub key: &'static str,
    pub unit: &'static str,
    pub scope: ParamScope,
    pub hard_min: f64,
    pub hard_max: f64,
    pub soft_min: Option<f64>,
    pub soft_max: Option<f64>,
    pub default: f64,
    /// Always [`PROVENANCE`].
    pub provenance: &'static str,
}

pub(crate) const fn row(key: &'static str, unit: &'static str, hard: (f64, f64), soft: (Option<f64>, Option<f64>), default: f64) -> ParamRow {
    ParamRow { key, unit, scope: ParamScope::Record, hard_min: hard.0, hard_max: hard.1, soft_min: soft.0, soft_max: soft.1, default, provenance: PROVENANCE }
}

pub(crate) const fn layer_row(key: &'static str, unit: &'static str, hard: (f64, f64), soft: (Option<f64>, Option<f64>), default: f64) -> ParamRow {
    ParamRow { scope: ParamScope::DepthLayer, ..row(key, unit, hard, soft, default) }
}

/// Every `scope` row of `rows` read through `get` is finite and inside its hard bounds.
pub(crate) fn validate_rows(rows: &[ParamRow], scope: ParamScope, get: impl Fn(&str) -> Option<f64>) -> Result<()> {
    for r in rows.iter().filter(|r| r.scope == scope) {
        let v = get(r.key).ok_or(ParticleError::Parameter(r.key))?;
        finite_in(v, r.hard_min, r.hard_max, r.key)?;
    }
    Ok(())
}

/// The row of `key` with `scope`, and `value` inside it.
pub(crate) fn checked(rows: &[ParamRow], scope: ParamScope, key: &str, value: f64) -> Result<&'static str> {
    let r = rows.iter().find(|r| r.scope == scope && r.key == key).ok_or(ParticleError::Parameter("param_key"))?;
    finite_in(value, r.hard_min, r.hard_max, r.key)?;
    Ok(r.key)
}

/// A row value that must be a whole number (counts and grid sizes): `Parameter(key)` otherwise.
pub(crate) fn whole(key: &'static str, value: f64) -> Result<()> {
    if value.fract() == 0.0 { Ok(()) } else { Err(ParticleError::Parameter(key)) }
}

/// Fixed-grid steps from the simulation origin to `tick` for a stepped type: whole `step_rate` steps
/// elapsed since `start_tick` (floor, exact integer arithmetic) plus `preroll` seconds of steps
/// (floor), each split into `substeps`; 0 before the start tick. Refused as `Window` when it does not
/// fit `u64` or exceeds `limits.max_window_steps` (V4-02, before any allocation).
pub(crate) fn fixed_steps(start_tick: u64, preroll: f64, step_rate: u32, substeps: u32, tick: u64, limits: &ParticleLimits) -> Result<u64> {
    let Some(d) = tick.checked_sub(start_tick) else { return Ok(0) };
    let base = u128::from(d)
        .checked_mul(u128::from(step_rate))
        .and_then(|v| v.checked_div(u128::from(TICKS_PER_SECOND)))
        .ok_or(ParticleError::Window)?;
    let pre = (preroll * f64::from(step_rate)).floor();
    if !(0.0..=1.0e9).contains(&pre) {
        return Err(ParticleError::Parameter("preroll"));
    }
    let steps = base
        .checked_add(pre as u128)
        .and_then(|v| v.checked_mul(u128::from(substeps)))
        .and_then(|v| u64::try_from(v).ok())
        .ok_or(ParticleError::Window)?;
    if steps > limits.max_window_steps {
        return Err(ParticleError::Window);
    }
    Ok(steps)
}

/// Seconds of simulation time at `tick` (`preroll` seconds already elapsed at the start tick);
/// `None` before the start tick.
pub(crate) fn sim_seconds(start_tick: u64, preroll: f64, tick: u64) -> Option<f64> {
    tick.checked_sub(start_tick).map(|d| d as f64 / TICKS_PER_SECOND as f64 + preroll)
}

/// The inclusive slot window of elements that can be alive at `t` with lives up to `l_max`, for a
/// constant birth `rate` (module docs); `None` when nothing is born.
pub(crate) fn slot_window(rate: f64, t: f64, l_max: f64) -> Result<Option<(u64, u64)>> {
    if rate <= 0.0 || t < 0.0 {
        return Ok(None);
    }
    let hi = (t * rate).floor();
    let lo = (((t - l_max) * rate).floor() - 1.0).max(0.0);
    if !hi.is_finite() || !lo.is_finite() || hi > MAX_SLOT_INDEX {
        return Err(ParticleError::Window);
    }
    Ok(Some((lo as u64, hi as u64)))
}

/// Elements of a slot window (`f64`, for the pre-allocation estimate).
pub(crate) fn window_len(w: Option<(u64, u64)>) -> f64 {
    w.map_or(0.0, |(lo, hi)| (hi as f64 - lo as f64 + 1.0).max(0.0))
}

/// The alive estimate against the limits, before allocation.
pub(crate) fn check_count(estimate: f64, limits: &ParticleLimits) -> Result<usize> {
    let cap = limits.max_alive.min(HARD_MAX_ALIVE) as f64;
    if !estimate.is_finite() || estimate > cap {
        return Err(ParticleError::Count);
    }
    Ok(estimate.max(0.0) as usize)
}

/// The raster size against the limits, before any simulation or allocation.
pub(crate) fn check_raster(width: u32, height: u32, limits: &ParticleLimits) -> Result<usize> {
    let pixels = u64::from(width).checked_mul(u64::from(height)).ok_or(ParticleError::Raster)?;
    if pixels == 0 || pixels > limits.max_pixels {
        return Err(ParticleError::Raster);
    }
    usize::try_from(pixels).map_err(|_| ParticleError::Raster)
}

/// One element of a simulation at a sample time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SimParticle {
    /// Depth layer (0 = first drawn); 0 for types without layers.
    pub layer: u32,
    /// Slot or star index (draw order within the layer).
    pub index: u64,
    /// Composition pixels (a streak's head).
    pub position: [f64; 2],
    /// Pixels per second on screen.
    pub velocity: [f64; 2],
    /// Seconds since birth.
    pub age: f64,
    /// This element's life in seconds (`f64::INFINITY` for a recycling star).
    pub life: f64,
    /// Dot diameter or streak width, pixels.
    pub size: f64,
    /// Streak length, pixels (0 for a dot).
    pub length: f64,
    /// Depth (star burst); 1 for flat types.
    pub depth: f64,
    /// `0..=1`.
    pub opacity: f64,
    /// Straight RGB.
    pub colour: [f32; 3],
}

/// How an element is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Mark {
    /// Antialiased solid dot (the particle world's dot rule).
    Dot,
    /// Dot with a linear radial falloff to 0 at its radius.
    Soft,
    /// Antialiased capsule from the position back along the velocity by `length`.
    Streak,
}

impl Mark {
    /// `(centre, half extent)` of the footprint.
    fn bounds(self, p: &SimParticle) -> ([f64; 2], [f64; 2]) {
        match self {
            Self::Dot | Self::Soft => (p.position, [p.size.max(1.0) / 2.0; 2]),
            Self::Streak => {
                let tail = streak_tail(p);
                let hw = (p.size / 2.0).max(0.5);
                (
                    [(p.position[0] + tail[0]) / 2.0, (p.position[1] + tail[1]) / 2.0],
                    [(p.position[0] - tail[0]).abs() / 2.0 + hw, (p.position[1] - tail[1]).abs() / 2.0 + hw],
                )
            }
        }
    }

    /// Straight coverage `0..=1` at pixel centre `c` (before opacity).
    fn coverage(self, p: &SimParticle, c: [f64; 2]) -> f64 {
        match self {
            Self::Dot | Self::Soft => {
                let r = (p.size / 2.0).max(0.5);
                let small = if p.size < 1.0 { p.size * p.size } else { 1.0 };
                let d = ((c[0] - p.position[0]).powi(2) + (c[1] - p.position[1]).powi(2)).sqrt();
                let cov = if self == Self::Dot { r + 0.5 - d } else { 1.0 - d / r };
                cov.clamp(0.0, 1.0) * small
            }
            Self::Streak => {
                let hw = (p.size / 2.0).max(0.5);
                let thin = p.size.min(1.0);
                (hw + 0.5 - segment_distance(c, p.position, streak_tail(p))).clamp(0.0, 1.0) * thin
            }
        }
    }
}

/// The streak's tail: `length` back from the head along the velocity (the head itself when still).
fn streak_tail(p: &SimParticle) -> [f64; 2] {
    let speed = p.velocity[0].hypot(p.velocity[1]);
    if speed <= 0.0 || p.length <= 0.0 {
        return p.position;
    }
    [p.position[0] - p.velocity[0] / speed * p.length, p.position[1] - p.velocity[1] / speed * p.length]
}

fn segment_distance(c: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((c[0] - a[0]) * dx + (c[1] - a[1]) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (c[0] - (a[0] + t * dx)).hypot(c[1] - (a[1] + t * dy))
}

/// Draws `parts` in order (source-over, premultiplied) over a `width x height` raster whose size
/// was checked by [`check_raster`]; the raster work is estimated before the plane is allocated.
pub(crate) fn draw(
    parts: &[SimParticle],
    mark: impl Fn(&SimParticle) -> Mark,
    width: u32,
    height: u32,
    pixels: usize,
    limits: &ParticleLimits,
    cancel: &CancellationToken,
) -> Result<ParticleFrame> {
    let mut work = 0u64;
    for p in parts {
        let (c, half) = mark(p).bounds(p);
        if let Some((x0, y0, x1, y1)) = footprint(c, half, width, height) {
            work = work.saturating_add(u64::from(x1.saturating_sub(x0)).saturating_mul(u64::from(y1.saturating_sub(y0))));
            if work > limits.max_raster_work {
                return Err(ParticleError::Work);
            }
        }
    }
    let mut plane = vec![[0.0f32; 4]; pixels];
    for (n, p) in parts.iter().enumerate() {
        if n & 0xFFF == 0 {
            cancel.check().map_err(|_| ParticleError::Canceled)?;
        }
        if p.opacity <= 0.0 || p.size <= 0.0 {
            continue;
        }
        let m = mark(p);
        let (c, half) = m.bounds(p);
        let Some((x0, y0, x1, y1)) = footprint(c, half, width, height) else { continue };
        for y in y0..y1 {
            for x in x0..x1 {
                let a = (m.coverage(p, [f64::from(x) + 0.5, f64::from(y) + 0.5]) * p.opacity) as f32;
                if a <= 0.0 {
                    continue;
                }
                let i = (y as usize).wrapping_mul(width as usize).wrapping_add(x as usize);
                over(&mut plane[i], [p.colour[0] * a, p.colour[1] * a, p.colour[2] * a, a]);
            }
        }
    }
    Ok(ParticleFrame { width, height, premultiplied: plane, drawn: parts.len() as u64 })
}

/// One simulation record of any implemented type (the headless entry point).
#[derive(Clone, Debug, PartialEq)]
pub enum Simulation {
    World(ParticleWorld),
    Classic(ParticleSystemsClassic),
    Rain(Rain),
    Snow(Snow),
    StarBurst(StarBurst),
    WaveWorld(WaveWorld),
    Caustics(Caustics),
    Bubbles(Bubbles),
}

impl Simulation {
    /// The default record of `kind` (row defaults; the particle world's `point(0, 0, 0)`).
    pub fn new(kind: SimulationKind) -> Self {
        match kind {
            SimulationKind::ParticleWorld => Self::World(ParticleWorld::point(0, 0.0, 0.0)),
            SimulationKind::ParticleSystemsClassic => Self::Classic(ParticleSystemsClassic::default()),
            SimulationKind::Rain => Self::Rain(Rain::default()),
            SimulationKind::Snow => Self::Snow(Snow::default()),
            SimulationKind::StarBurst => Self::StarBurst(StarBurst::default()),
            SimulationKind::WaveWorld => Self::WaveWorld(WaveWorld::default()),
            SimulationKind::Caustics => Self::Caustics(Caustics::default()),
            SimulationKind::Bubbles => Self::Bubbles(Bubbles::default()),
        }
    }

    pub fn kind(&self) -> SimulationKind {
        match self {
            Self::World(_) => SimulationKind::ParticleWorld,
            Self::Classic(_) => SimulationKind::ParticleSystemsClassic,
            Self::Rain(_) => SimulationKind::Rain,
            Self::Snow(_) => SimulationKind::Snow,
            Self::StarBurst(_) => SimulationKind::StarBurst,
            Self::WaveWorld(_) => SimulationKind::WaveWorld,
            Self::Caustics(_) => SimulationKind::Caustics,
            Self::Bubbles(_) => SimulationKind::Bubbles,
        }
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            Self::World(w) => w.validate(),
            Self::Classic(c) => c.validate(),
            Self::Rain(r) => r.validate(),
            Self::Snow(s) => s.validate(),
            Self::StarBurst(s) => s.validate(),
            Self::WaveWorld(w) => w.validate(),
            Self::Caustics(c) => c.validate(),
            Self::Bubbles(b) => b.validate(),
        }
    }

    /// A record scalar by row key (`None` for an unknown key or the particle world).
    pub fn param(&self, key: &str) -> Option<f64> {
        match self {
            Self::World(_) => None,
            Self::Classic(c) => c.param(key),
            Self::Rain(r) => r.param(key),
            Self::Snow(s) => s.param(key),
            Self::StarBurst(s) => s.param(key),
            Self::WaveWorld(w) => w.param(key),
            Self::Caustics(c) => c.param(key),
            Self::Bubbles(b) => b.param(key),
        }
    }

    /// Sets a record scalar by row key; a value outside the row's hard bounds (or an unknown key)
    /// is refused and nothing changes.
    pub fn set_param(&mut self, key: &str, value: f64) -> Result<()> {
        match self {
            Self::World(_) => Err(ParticleError::Parameter("param_key")),
            Self::Classic(c) => c.set_param(key, value),
            Self::Rain(r) => r.set_param(key, value),
            Self::Snow(s) => s.set_param(key, value),
            Self::StarBurst(s) => s.set_param(key, value),
            Self::WaveWorld(w) => w.set_param(key, value),
            Self::Caustics(c) => c.set_param(key, value),
            Self::Bubbles(b) => b.set_param(key, value),
        }
    }

    /// Renders the frame at composition `tick`. `ticks_per_frame` is the particle world's grid
    /// (its integrator steps per frame); the other types do not use it (Wave world steps on
    /// its own declared grid).
    pub fn render(
        &self,
        width: u32,
        height: u32,
        tick: u64,
        ticks_per_frame: u64,
        limits: &ParticleLimits,
        cancel: &CancellationToken,
    ) -> Result<ParticleFrame> {
        match self {
            Self::World(w) => render_particles(w, width, height, tick, ticks_per_frame, None, limits, cancel),
            Self::Classic(c) => c.render(width, height, tick, limits, cancel),
            Self::Rain(r) => r.render(width, height, tick, limits, cancel),
            Self::Snow(s) => s.render(width, height, tick, limits, cancel),
            Self::StarBurst(s) => s.render(width, height, tick, limits, cancel),
            Self::WaveWorld(w) => w.render(width, height, tick, limits, cancel),
            Self::Caustics(c) => c.render(width, height, tick, limits, cancel),
            Self::Bubbles(b) => b.render(width, height, tick, limits, cancel),
        }
    }
}

/// The observe outcome of a simulation step (codes only; deliver with [`crate::report`]).
pub fn outcome<T>(result: &std::result::Result<T, ParticleError>) -> Outcome {
    outcome_of(&result.as_ref().map(|_| ()).map_err(|e| CompositeError::from(e.clone())))
}
