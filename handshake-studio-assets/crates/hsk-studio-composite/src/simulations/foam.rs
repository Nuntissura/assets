//! Foam (MT-34183, STU-CMP-075 "Bubble-cluster generation with growth, stickiness and per-bubble
//! rendering").
//!
//! **Identity.** Its own [`super::SimulationKind::Foam`] id, name, import key and record ([`Foam`]);
//! it never resolves to Bubbles (`tests/simulations_water.rs::foam_is_its_own_identity_and_never_bubbles`).
//! Foam is surface-bound: its bubbles are born inside a producer rectangle and stay inside the
//! surface region; Bubbles rise through their region and pop at its top.
//!
//! **Record ([`FOAM_ROWS`], [`FOAM_MAPPINGS`], then the shared `shading.*` rows).** Seed, start tick,
//! preroll, a fixed step rate, the producer (centre and size), the surface region, a birth rate, a
//! life with a random share, a birth size with a random share and a growth rate, wind (speed and
//! direction, degrees, 0 = up, clockwise), an initial speed (seeded direction), viscosity,
//! stickiness with its radius, repulsion, an opacity and the shared [`BubbleShading`].
//!
//! **Recovered record ([`FOAM_RECORD`], STU-FX-131a `APC Foam`, 47 rows, MT-32012; rules in the
//! [`super`] module docs).** The engine reads one recovered row: `wind_direction` takes row 32
//! `Wind Direction` (`ANGLE`, degrees; no-range reading: no hard bound, default 0), replacing the
//! authored row (hard `-1e6..=1e6`, soft `-180..=180`, default 90). The other engine scalars have no
//! recovered row of the same quantity and unit and keep their authored rows ([`FOAM_ROWS`],
//! STU-FX-146 scope beside the record): the record declares no unit (`--`) for `Production Rate`,
//! `Size`, `Size Variance`, `Lifespan`, `Bubble Growth Speed`, `Initial Speed`, `Wind Speed`,
//! `Repulsion`, `Viscosity` and `Stickiness`, while the engine's counterparts are in pixels, seconds
//! or per second, so reading one as the other would invent a conversion (STU-FX-108); `Producer
//! Point` and `Producer X/Y Size` are layer fractions while the engine producer is in composition
//! pixels with no layer size in the record; `Random Seed` (`0..=16`) is not the STU-FX-011 `u64`
//! seed. The remaining rows (view, producer orientation, turbulence, wobble, pop velocity, zoom,
//! universe size, textures, maps, quality, groups) are stored and validated and do not drive the
//! simulation. After Effects parity of the mapping is UNVERIFIED (D5).
//!
//! **Simulation (stateful, a declared Studio reading; After Effects parity UNVERIFIED, D5).** Every
//! frame is re-simulated from the origin (`preroll` seconds before the start tick) on the fixed grid
//! `t_s = s / step_rate` ([`super::fixed_steps`] steps, 0 before the start tick), single-threaded:
//! at each step (1) bubbles born by `t_s` on the slot grid of [`super`] are appended (seeded position
//! in the producer, clamped to the region; seeded size, life and initial direction), (2) bubbles at
//! the end of their life pop, (3) every bubble's acceleration is summed from its neighbours within
//! `stick_radius` (a uniform-grid neighbour search: cells of `stick_radius`, the 3 x 3 cells around
//! the bubble, neighbours in ascending birth order): stickiness `s (1 - d / R)` toward each neighbour
//! and, where two bubbles overlap, repulsion `p overlap / (r_i + r_j)` apart, (4) semi-implicit
//! Euler `v' = (v + a dt) exp(-viscosity dt)`, `x' = x + (v' + wind) dt`, then clamped to the region
//! (the velocity component into the edge is zeroed). Accelerations are computed from the positions
//! at the step start for every bubble before any moves (a fixed order). The diameter at age `a` is
//! `d_k + growth a`. Bubbles x steps and the births are checked against the budget before the
//! simulation allocates; the neighbour pairs visited are counted against the same budget.
//! Rendering goes through [`super::bubbles::render_bubble_marks`] with the shared shading.
#![deny(clippy::arithmetic_side_effects)]
use super::bubbles::{BUBBLE_SHADING_ROWS, BubbleShading, default_of, render_bubble_marks};
use super::{
    ParamRow, ParamScope, RecoveredMapping, Result, SimParticle, check_count, check_raster, checked, checked_mapped, fixed_steps,
    mapped_default, rec_angle, rec_checkbox, rec_group, rec_layer, rec_point, rec_popup, rec_slider, row, validate_mapped,
    validate_rows, whole,
};
use crate::particles::{Draws, ParticleError, ParticleFrame, ParticleLimits, splitmix64};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::StudioEffectParamSpec;
use std::f64::consts::TAU;

const FOAM_SALT: u64 = 0x666F_616D;

/// Foam's authored rows: engine scalars with no counterpart in the recovered record [`FOAM_RECORD`]
/// (STU-FX-146 scope beside it, `studio_declared`; module docs).
pub const FOAM_ROWS: [ParamRow; 23] = [
    row("preroll", "s", (0.0, 60.0), (Some(0.0), Some(5.0)), 0.0),
    row("step_rate", "steps/s", (1.0, 240.0), (Some(24.0), Some(120.0)), 30.0),
    row("producer_x", "px", (-1.0e7, 1.0e7), (None, None), 960.0),
    row("producer_y", "px", (-1.0e7, 1.0e7), (None, None), 540.0),
    row("producer_width", "px", (0.0, 1.0e7), (Some(0.0), Some(1000.0)), 200.0),
    row("producer_height", "px", (0.0, 1.0e7), (Some(0.0), Some(1000.0)), 120.0),
    row("region_x", "px", (-1.0e7, 1.0e7), (None, None), 0.0),
    row("region_y", "px", (-1.0e7, 1.0e7), (None, None), 0.0),
    row("region_width", "px", (1.0, 1.0e7), (None, None), 1920.0),
    row("region_height", "px", (1.0, 1.0e7), (None, None), 1080.0),
    row("rate", "bubbles/s", (0.0, 1.0e5), (Some(0.0), Some(200.0)), 30.0),
    row("life", "s", (1.0e-3, 600.0), (Some(0.1), Some(30.0)), 6.0),
    row("life_random", "ratio", (0.0, 0.99), (None, None), 0.2),
    row("size", "px", (0.05, 4096.0), (Some(1.0), Some(64.0)), 10.0),
    row("size_random", "ratio", (0.0, 1.0), (None, None), 0.5),
    row("growth", "px/s", (0.0, 4096.0), (Some(0.0), Some(10.0)), 1.0),
    row("wind_speed", "px/s", (0.0, 1.0e5), (Some(0.0), Some(200.0)), 10.0),
    row("initial_speed", "px/s", (0.0, 1.0e5), (Some(0.0), Some(200.0)), 0.0),
    row("viscosity", "1/s", (0.0, 1000.0), (Some(0.0), Some(10.0)), 1.0),
    row("stickiness", "px/s2", (0.0, 1.0e6), (Some(0.0), Some(500.0)), 40.0),
    row("stick_radius", "px", (0.5, 4096.0), (Some(2.0), Some(128.0)), 24.0),
    row("repulsion", "px/s2", (0.0, 1.0e6), (Some(0.0), Some(2000.0)), 200.0),
    row("opacity", "ratio", (0.0, 1.0), (None, None), 1.0),
];

/// The engine scalars driven by a row of [`FOAM_RECORD`] (module docs).
pub const FOAM_MAPPINGS: [RecoveredMapping; 1] = [RecoveredMapping { key: "wind_direction", param_index: 32, component: 0, unit: "deg" }];

/// The recovered STU-FX-131a record of Foam (`APC Foam`, spec v02.225 module 14), every row in
/// source order (`tests/simulations_records.rs`).
pub const FOAM_RECORD: [StudioEffectParamSpec; 47] = [
    rec_layer("unnamed_0", 0, "(unnamed)"),
    rec_popup("view", 2, "View", &[1.0], FOAM_VIEW_2_OPTIONS),
    rec_point("producer_point", 4, "Producer Point", &[0.5, 0.5]),
    rec_group("producer", 5, "Producer"),
    rec_slider("producer_x_size", 6, "Producer X Size", (0.0, 0.449997), (0.0, 0.449997), &[0.029999], Some(3)),
    rec_slider("producer_y_size", 8, "Producer Y Size", (0.0, 0.449997), (0.0, 0.449997), &[0.029999], Some(3)),
    rec_angle("producer_orientation", 10, "Producer Orientation"),
    rec_checkbox("unnamed_12", 12, "(unnamed)", &[1.0]),
    rec_slider("production_rate", 14, "Production Rate", (0.0, 100.0), (0.0, 10.0), &[1.0], Some(3)),
    rec_slider("size", 16, "Size", (0.049988, 4.0), (0.049988, 4.0), &[0.5], Some(3)),
    rec_slider("size_variance", 18, "Size Variance", (0.0, 4.0), (0.0, 4.0), &[0.5], Some(3)),
    rec_group("unnamed_19", 19, "(unnamed)"),
    rec_slider("lifespan", 20, "Lifespan", (0.0, 1000.0), (0.0, 1000.0), &[300.0], Some(3)),
    rec_group("bubbles", 21, "Bubbles"),
    rec_slider("bubble_growth_speed", 22, "Bubble Growth Speed", (0.009995, 1.0), (0.009995, 1.0), &[0.099991], Some(3)),
    rec_slider("strength", 24, "Strength", (0.0, 100.0), (0.0, 100.0), &[10.0], Some(3)),
    rec_slider("initial_speed", 26, "Initial Speed", (0.0, 10.0), (0.0, 10.0), &[0.0], Some(3)),
    rec_angle("initial_direction", 28, "Initial Direction"),
    rec_slider("wind_speed", 30, "Wind Speed", (0.0, 10.0), (0.0, 10.0), &[0.5], Some(3)),
    rec_angle("wind_direction", 32, "Wind Direction"),
    rec_group("unnamed_33", 33, "(unnamed)"),
    rec_slider("turbulence", 34, "Turbulence", (0.0, 2.0), (0.0, 2.0), &[0.5], Some(3)),
    rec_group("physics", 35, "Physics"),
    rec_slider("wobble_amount", 36, "Wobble Amount", (0.0, 0.399994), (0.0, 0.399994), &[0.049988], Some(3)),
    rec_slider("repulsion", 38, "Repulsion", (0.0, 1.0), (0.0, 1.0), &[1.0], Some(3)),
    rec_slider("pop_velocity", 40, "Pop Velocity", (0.0, 10.0), (0.0, 10.0), &[0.0], Some(3)),
    rec_slider("viscosity", 42, "Viscosity", (0.0, 4.0), (0.0, 4.0), &[0.099991], Some(3)),
    rec_slider("stickiness", 44, "Stickiness", (0.0, 4.0), (0.0, 4.0), &[0.75], Some(3)),
    rec_slider("zoom", 46, "Zoom", (0.00499, 10.0), (0.00499, 5.0), &[1.0], Some(3)),
    rec_slider("universe_size", 48, "Universe Size", (0.099991, 50.0), (0.099991, 5.0), &[1.0], Some(3)),
    rec_popup("blend_mode", 50, "Blend Mode", &[1.0], FOAM_BLEND_MODE_50_OPTIONS),
    rec_popup("bubble_texture", 52, "Bubble Texture", &[3.0], FOAM_BUBBLE_TEXTURE_52_OPTIONS),
    rec_layer("bubble_texture_layer", 54, "Bubble Texture Layer"),
    rec_popup("bubble_orientation", 56, "Bubble Orientation", &[1.0], FOAM_BUBBLE_ORIENTATION_56_OPTIONS),
    rec_group("unnamed_57", 57, "(unnamed)"),
    rec_layer("environment_map", 58, "Environment Map"),
    rec_slider("reflection_strength", 60, "Reflection Strength", (0.0, 1.0), (0.0, 1.0), &[0.0], Some(3)),
    rec_slider("reflection_convergence", 62, "Reflection Convergence", (0.0, 1.0), (0.0, 1.0), &[0.799988], Some(3)),
    rec_group("rendering", 63, "Rendering"),
    rec_layer("flow_map_64", 64, "Flow Map"),
    rec_slider("flow_map_steepness", 66, "Flow Map Steepness", (0.0, 100.0), (0.0, 5.0), &[5.0], Some(3)),
    rec_popup("flow_map_fits", 68, "Flow Map Fits", &[1.0], FOAM_FLOW_MAP_FITS_68_OPTIONS),
    rec_popup("simulation_quality", 70, "Simulation Quality", &[1.0], FOAM_SIMULATION_QUALITY_70_OPTIONS),
    rec_slider("random_seed", 72, "Random Seed", (0.0, 16.0), (0.0, 16.0), &[1.0], None),
    rec_group("unnamed_79", 79, "(unnamed)"),
    rec_group("flow_map_81", 81, "Flow Map"),
    rec_group("unnamed_89", 89, "(unnamed)"),
];
/// `param_index` of every row whose flags carry `COLLAPSE_TWIRLY`.
pub const FOAM_COLLAPSED: [u16; 30] = [5, 6, 8, 10, 14, 16, 18, 20, 21, 22, 24, 26, 28, 30, 32, 34, 35, 36, 38, 40, 42, 44, 46, 48, 60, 62, 63, 66, 72, 81];
const FOAM_VIEW_2_OPTIONS: &[(u16, &str)] = &[(1, "Draft"), (2, "Draft + Flow Map"), (3, "Rendered")];
const FOAM_BLEND_MODE_50_OPTIONS: &[(u16, &str)] = &[(1, "Transparent"), (2, "Solid Old on top"), (3, "Solid New on top")];
const FOAM_BUBBLE_TEXTURE_52_OPTIONS: &[(u16, &str)] = &[(1, "User Defined"), (3, "Default Bubble"), (4, "Amber Bock"), (5, "Water Beads"), (6, "Spit"), (7, "Cartoon Coffee"), (8, "Winter Stream"), (9, "Soda Water"), (10, "Orange Soda"), (11, "Nuclear Waste"), (12, "Red Tide"), (13, "Magma Marbles"), (14, "Sunset Foam"), (15, "Pepto"), (16, "Algae"), (17, "Blisters"), (18, "Bubble Wrap"), (19, "Grape Soda")];
const FOAM_BUBBLE_ORIENTATION_56_OPTIONS: &[(u16, &str)] = &[(1, "Fixed"), (2, "Physical Orientation"), (3, "Bubble Velocity")];
const FOAM_FLOW_MAP_FITS_68_OPTIONS: &[(u16, &str)] = &[(1, "Universe"), (2, "Screen")];
const FOAM_SIMULATION_QUALITY_70_OPTIONS: &[(u16, &str)] = &[(1, "Normal"), (2, "High"), (3, "Intense")];

/// One foam layer (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct Foam {
    /// STU-FX-011 explicit seed.
    pub seed: u64,
    /// The deterministic start (STU-CMP-075a), composition ticks; nothing is drawn before it.
    pub start_tick: u64,
    /// Seconds already simulated at the start tick.
    pub preroll: f64,
    /// Steps per second, `1..=240`.
    pub step_rate: u32,
    /// Producer centre, composition pixels.
    pub producer: [f64; 2],
    /// Producer `[width, height]`, pixels.
    pub producer_size: [f64; 2],
    /// The surface `[x, y, width, height]` the bubbles are bound to.
    pub region: [f64; 4],
    /// Bubbles per second.
    pub rate: f64,
    /// Seconds.
    pub life: f64,
    pub life_random: f64,
    /// Birth diameter, pixels.
    pub size: f64,
    pub size_random: f64,
    /// Diameter growth, px/s.
    pub growth: f64,
    /// px/s.
    pub wind_speed: f64,
    /// Degrees, 0 = up, clockwise.
    pub wind_direction: f64,
    /// px/s, seeded direction.
    pub initial_speed: f64,
    /// Per second.
    pub viscosity: f64,
    /// px/s^2 toward each neighbour within `stick_radius`.
    pub stickiness: f64,
    /// Pixels.
    pub stick_radius: f64,
    /// px/s^2 apart at full overlap.
    pub repulsion: f64,
    pub opacity: f64,
    pub shading: BubbleShading,
}

impl Default for Foam {
    fn default() -> Self {
        let d = |k| default_of(&FOAM_ROWS, k);
        Self {
            seed: 0,
            start_tick: 0,
            preroll: d("preroll"),
            step_rate: d("step_rate") as u32,
            producer: [d("producer_x"), d("producer_y")],
            producer_size: [d("producer_width"), d("producer_height")],
            region: [d("region_x"), d("region_y"), d("region_width"), d("region_height")],
            rate: d("rate"),
            life: d("life"),
            life_random: d("life_random"),
            size: d("size"),
            size_random: d("size_random"),
            growth: d("growth"),
            wind_speed: d("wind_speed"),
            wind_direction: mapped_default(&FOAM_RECORD, &FOAM_MAPPINGS, "wind_direction"),
            initial_speed: d("initial_speed"),
            viscosity: d("viscosity"),
            stickiness: d("stickiness"),
            stick_radius: d("stick_radius"),
            repulsion: d("repulsion"),
            opacity: d("opacity"),
            shading: BubbleShading::default(),
        }
    }
}

/// One live foam bubble.
#[derive(Clone, Copy)]
struct FoamBubble {
    index: u64,
    birth: f64,
    life: f64,
    size: f64,
    p: [f64; 2],
    v: [f64; 2],
}

impl Foam {
    /// Authored record rows then the shared shading rows ([`FOAM_MAPPINGS`] are not rows).
    pub fn rows() -> Vec<ParamRow> {
        FOAM_ROWS.iter().chain(&BUBBLE_SHADING_ROWS).copied().collect()
    }

    pub fn param(&self, key: &str) -> Option<f64> {
        Some(match key {
            "preroll" => self.preroll,
            "step_rate" => f64::from(self.step_rate),
            "producer_x" => self.producer[0],
            "producer_y" => self.producer[1],
            "producer_width" => self.producer_size[0],
            "producer_height" => self.producer_size[1],
            "region_x" => self.region[0],
            "region_y" => self.region[1],
            "region_width" => self.region[2],
            "region_height" => self.region[3],
            "rate" => self.rate,
            "life" => self.life,
            "life_random" => self.life_random,
            "size" => self.size,
            "size_random" => self.size_random,
            "growth" => self.growth,
            "wind_speed" => self.wind_speed,
            "wind_direction" => self.wind_direction,
            "initial_speed" => self.initial_speed,
            "viscosity" => self.viscosity,
            "stickiness" => self.stickiness,
            "stick_radius" => self.stick_radius,
            "repulsion" => self.repulsion,
            "opacity" => self.opacity,
            _ => return self.shading.param(key),
        })
    }

    /// Sets a record or shading scalar (refused outside its row, or fractional for the step rate;
    /// nothing changes).
    pub fn set_param(&mut self, key: &str, value: f64) -> Result<()> {
        if self.shading.set(key, value)? {
            return Ok(());
        }
        if let Some(k) = checked_mapped(&FOAM_RECORD, &FOAM_MAPPINGS, key, value)? {
            match k {
                "wind_direction" => self.wind_direction = value,
                _ => return Err(ParticleError::Parameter("param_key")),
            }
            return Ok(());
        }
        match checked(&FOAM_ROWS, ParamScope::Record, key, value)? {
            "preroll" => self.preroll = value,
            "step_rate" => {
                whole("step_rate", value)?;
                self.step_rate = value as u32;
            }
            "producer_x" => self.producer[0] = value,
            "producer_y" => self.producer[1] = value,
            "producer_width" => self.producer_size[0] = value,
            "producer_height" => self.producer_size[1] = value,
            "region_x" => self.region[0] = value,
            "region_y" => self.region[1] = value,
            "region_width" => self.region[2] = value,
            "region_height" => self.region[3] = value,
            "rate" => self.rate = value,
            "life" => self.life = value,
            "life_random" => self.life_random = value,
            "size" => self.size = value,
            "size_random" => self.size_random = value,
            "growth" => self.growth = value,
            "wind_speed" => self.wind_speed = value,
            "initial_speed" => self.initial_speed = value,
            "viscosity" => self.viscosity = value,
            "stickiness" => self.stickiness = value,
            "stick_radius" => self.stick_radius = value,
            "repulsion" => self.repulsion = value,
            _ => self.opacity = value,
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        validate_rows(&FOAM_ROWS, ParamScope::Record, |k| self.param(k))?;
        validate_mapped(&FOAM_RECORD, &FOAM_MAPPINGS, |k| self.param(k))?;
        self.shading.validate()
    }

    /// The bubbles alive at composition `tick` (ascending birth order). Validates the record.
    pub fn states_at(&self, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<Vec<SimParticle>> {
        self.validate()?;
        self.states(tick, limits, cancel)
    }

    fn bind(&self, c: &mut FoamBubble) {
        let [rx, ry, rw, rh] = self.region;
        for (axis, (lo, hi)) in [(rx, rx + rw), (ry, ry + rh)].into_iter().enumerate() {
            if c.p[axis] < lo {
                c.p[axis] = lo;
                c.v[axis] = c.v[axis].max(0.0);
            } else if c.p[axis] > hi {
                c.p[axis] = hi;
                c.v[axis] = c.v[axis].min(0.0);
            }
        }
    }

    fn spawn(&self, index: u64, birth_draw: f64, d: &mut Draws) -> FoamBubble {
        let (ux, uy) = (d.uniform(), d.uniform());
        let life = self.life * (1.0 + self.life_random * (2.0 * d.uniform() - 1.0));
        let size = self.size * (1.0 + self.size_random * (2.0 * d.uniform() - 1.0));
        let th = TAU * d.uniform();
        let mut c = FoamBubble {
            index,
            birth: (index as f64 + birth_draw) / self.rate,
            life,
            size,
            p: [self.producer[0] + (ux - 0.5) * self.producer_size[0], self.producer[1] + (uy - 0.5) * self.producer_size[1]],
            v: [self.initial_speed * th.sin(), -self.initial_speed * th.cos()],
        };
        self.bind(&mut c);
        c
    }

    fn states(&self, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<Vec<SimParticle>> {
        if tick < self.start_tick {
            return Ok(Vec::new());
        }
        let steps = fixed_steps(self.start_tick, self.preroll, self.step_rate, 1, tick, limits)?;
        let dt = 1.0 / f64::from(self.step_rate);
        let t_end = steps as f64 * dt;
        // Budget (V4-02) before allocation: alive bound, bubbles x steps and births.
        let l_max = self.life * (1.0 + self.life_random);
        let alive = check_count(self.rate * (l_max + dt) + 2.0, limits)?;
        let births = self.rate * t_end + 1.0;
        if !births.is_finite()
            || births > limits.max_steps as f64
            || (alive as u64).checked_mul(steps).is_none_or(|w| w > limits.max_steps)
        {
            return Err(ParticleError::Work);
        }
        let key = self.seed ^ splitmix64(FOAM_SALT);
        let th = self.wind_direction.to_radians();
        let wind = [self.wind_speed * th.sin(), -self.wind_speed * th.cos()];
        let damp = (-self.viscosity * dt).exp();
        let r = self.stick_radius;
        let mut live: Vec<FoamBubble> = Vec::with_capacity(alive);
        let mut next_index = 0u64;
        let mut pending: Option<(f64, Draws)> = None;
        let mut pairs = 0u64;
        let mut acc: Vec<[f64; 2]> = Vec::with_capacity(alive);
        let mut grid: Vec<(i64, i64, usize)> = Vec::with_capacity(alive);
        for s in 1..=steps {
            if s & 0x3F == 1 {
                cancel.check().map_err(|_| ParticleError::Canceled)?;
            }
            let t = s as f64 * dt;
            // (1) births by t, in slot order.
            if self.rate > 0.0 {
                loop {
                    let (u, mut d) = match pending.take() {
                        Some(p) => p,
                        None => {
                            let mut d = Draws::new(key, next_index);
                            (d.uniform(), d)
                        }
                    };
                    if (next_index as f64 + u) / self.rate > t {
                        pending = Some((u, d));
                        break;
                    }
                    if live.len() >= alive {
                        return Err(ParticleError::Count);
                    }
                    live.push(self.spawn(next_index, u, &mut d));
                    next_index = next_index.checked_add(1).ok_or(ParticleError::Window)?;
                }
            }
            // (2) pops.
            live.retain(|c| t - c.birth < c.life);
            // (3) accelerations from the step-start positions.
            grid.clear();
            let [rx, ry, _, _] = self.region;
            for (i, c) in live.iter().enumerate() {
                grid.push((((c.p[0] - rx) / r).floor() as i64, ((c.p[1] - ry) / r).floor() as i64, i));
            }
            grid.sort_unstable();
            acc.clear();
            for (i, c) in live.iter().enumerate() {
                let (cx, cy) = (((c.p[0] - rx) / r).floor() as i64, ((c.p[1] - ry) / r).floor() as i64);
                let ri = (c.size + self.growth * (t - c.birth)) / 2.0;
                let mut a = [0.0f64; 2];
                for gx in [cx.saturating_sub(1), cx, cx.saturating_add(1)] {
                    for gy in [cy.saturating_sub(1), cy, cy.saturating_add(1)] {
                        let from = grid.partition_point(|e| (e.0, e.1) < (gx, gy));
                        for &(ex, ey, j) in &grid[from..] {
                            if (ex, ey) != (gx, gy) {
                                break;
                            }
                            if j == i {
                                continue;
                            }
                            pairs = pairs.saturating_add(1);
                            if pairs > limits.max_steps {
                                return Err(ParticleError::Work);
                            }
                            let o = &live[j];
                            let dv = [o.p[0] - c.p[0], o.p[1] - c.p[1]];
                            let dist = dv[0].hypot(dv[1]);
                            if dist <= 0.0 || dist >= r {
                                continue;
                            }
                            let n = [dv[0] / dist, dv[1] / dist];
                            let mut f = self.stickiness * (1.0 - dist / r);
                            let rj = (o.size + self.growth * (t - o.birth)) / 2.0;
                            let contact = ri + rj;
                            if contact > 0.0 && dist < contact {
                                f -= self.repulsion * (contact - dist) / contact;
                            }
                            a[0] += f * n[0];
                            a[1] += f * n[1];
                        }
                    }
                }
                acc.push(a);
            }
            // (4) integrate, then bind to the surface.
            for (c, a) in live.iter_mut().zip(&acc) {
                c.v = [(c.v[0] + a[0] * dt) * damp, (c.v[1] + a[1] * dt) * damp];
                c.p = [c.p[0] + (c.v[0] + wind[0]) * dt, c.p[1] + (c.v[1] + wind[1]) * dt];
                self.bind(c);
                if !(c.p[0].is_finite() && c.p[1].is_finite() && c.v[0].is_finite() && c.v[1].is_finite()) {
                    return Err(ParticleError::NonFinite);
                }
            }
        }
        Ok(live
            .iter()
            .map(|c| {
                let age = t_end - c.birth;
                SimParticle {
                    layer: 0,
                    index: c.index,
                    position: c.p,
                    velocity: [c.v[0] + wind[0], c.v[1] + wind[1]],
                    age,
                    life: c.life,
                    size: c.size + self.growth * age,
                    length: 0.0,
                    depth: 1.0,
                    opacity: self.opacity,
                    colour: self.shading.colour,
                }
            })
            .collect())
    }

    /// Renders the foam at composition `tick` over a `width x height` raster (composition pixels).
    pub fn render(&self, width: u32, height: u32, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<ParticleFrame> {
        self.validate()?;
        check_raster(width, height, limits)?;
        let parts = self.states(tick, limits, cancel)?;
        render_bubble_marks(&parts, &self.shading, width, height, limits, cancel)
    }
}
