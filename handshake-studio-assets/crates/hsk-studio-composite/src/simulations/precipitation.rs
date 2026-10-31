//! Rain (MT-34184, STU-CMP-075 "Falling-rain generation with wind, speed and depth layering") and
//! Snow (MT-34185, "Falling-snow generation with the same axes as rain").
//!
//! **Shared core ([`Precipitation`]).** Seed, start tick, preroll, drop rate, fall speed with a
//! random share, wind (direction in degrees, 0 = up, clockwise, so 90 blows to the right; strength
//! in px/s), the region the precipitation fills, colour, opacity and 1..=16 [`DepthLayer`]s drawn
//! in order (the first is the farthest). A layer multiplies the drop rate (`density`), the drop's
//! size (`scale`), its whole screen velocity (`speed`, so far layers fall and drift slower: parallax)
//! and its opacity. Rain and Snow carry the same core rows (keys, units, hard bounds); only defaults
//! and their own mark rows differ.
//!
//! **Motion.** A drop has a constant screen velocity `v = layer.speed * ((0, s_i) + wind)`, `s_i`
//! the fall speed with its seeded random share (terminal velocity; no acceleration), and is born on
//! the slot grid at the spawn line `margin` above the region, where `margin` is the drop's largest
//! extent; it dies when it has travelled `region height + 2 margin` downwards. Its spawn x is
//! uniform over the region widened upwind by the slowest drop's drift (and by the snow sway), and
//! the layer's birth rate is `rate * density * spawn width / region width`, so `rate` is the visible
//! rate over the region whatever the wind. A fall speed of at least [`MIN_FALL_SPEED`] px/s on every
//! layer is required (a wind that lifts the rain is refused as `wind`). Snow adds a seeded sway:
//! `x += A scale sin(2 pi f_i age + phi_i)`, `f_i = drift_frequency (0.5 + u)`.
//!
//! **Marks.** Rain draws an antialiased capsule `streak_length * scale` long back along its velocity
//! and `streak_width * scale` wide; Snow draws an antialiased dot of `flake_size * scale` with its
//! random share. Draw order: layers in order, then ascending slot index. After Effects parity
//! (CC Rain / CC Snow / Rainfall / Snowfall behaviour) is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use super::{
    Mark, ParamRow, ParamScope, Result, SimParticle, check_count, check_raster, checked, draw, layer_row, row,
    sim_seconds, slot_window, validate_rows, window_len,
};
use crate::particles::{Draws, ParticleError, ParticleFrame, ParticleLimits, splitmix64};
use hsk_studio_accord::CancellationToken;
use std::f64::consts::TAU;

/// Most depth layers.
pub const MAX_DEPTH_LAYERS: usize = 16;
/// Slowest fall a layer may have (px/s).
pub const MIN_FALL_SPEED: f64 = 1.0;

const RAIN_SALT: u64 = 0x7261_696E;
const SNOW_SALT: u64 = 0x736E_6F77;

/// The core rows with per-type defaults `(rate, speed, opacity)`.
const fn core_rows(rate: f64, speed: f64, opacity: f64) -> [ParamRow; 18] {
    [
        row("preroll", "s", (0.0, 600.0), (Some(0.0), Some(10.0)), 0.0),
        row("rate", "drops/s", (0.0, 1.0e6), (Some(0.0), Some(2000.0)), rate),
        row("speed", "px/s", (MIN_FALL_SPEED, 1.0e6), (Some(10.0), Some(3000.0)), speed),
        row("speed_random", "ratio", (0.0, 1.0), (None, None), 0.2),
        row("wind_direction", "deg", (-1.0e6, 1.0e6), (Some(-180.0), Some(180.0)), 90.0),
        row("wind_strength", "px/s", (0.0, 1.0e6), (Some(0.0), Some(2000.0)), 0.0),
        row("region_x", "px", (-1.0e7, 1.0e7), (None, None), 0.0),
        row("region_y", "px", (-1.0e7, 1.0e7), (None, None), 0.0),
        row("region_width", "px", (1.0, 1.0e7), (None, None), 1920.0),
        row("region_height", "px", (1.0, 1.0e7), (None, None), 1080.0),
        row("colour_r", "ratio", (0.0, 1.0), (None, None), 1.0),
        row("colour_g", "ratio", (0.0, 1.0), (None, None), 1.0),
        row("colour_b", "ratio", (0.0, 1.0), (None, None), 1.0),
        row("opacity", "ratio", (0.0, 1.0), (None, None), opacity),
        layer_row("layer.density", "ratio", (0.0, 1000.0), (Some(0.0), Some(4.0)), 1.0),
        layer_row("layer.scale", "ratio", (0.01, 100.0), (Some(0.1), Some(4.0)), 1.0),
        layer_row("layer.speed", "ratio", (0.01, 100.0), (Some(0.1), Some(4.0)), 1.0),
        layer_row("layer.opacity", "ratio", (0.0, 1.0), (None, None), 1.0),
    ]
}

/// Rain's core rows.
pub const RAIN_CORE_ROWS: [ParamRow; 18] = core_rows(200.0, 1200.0, 0.6);
/// Snow's core rows (same keys, units and hard bounds as rain).
pub const SNOW_CORE_ROWS: [ParamRow; 18] = core_rows(60.0, 80.0, 0.9);
/// Rain's own rows.
pub const RAIN_ROWS: [ParamRow; 2] = [
    row("streak_length", "px", (0.0, 4096.0), (Some(0.0), Some(200.0)), 40.0),
    row("streak_width", "px", (0.05, 256.0), (Some(0.5), Some(8.0)), 1.5),
];
/// Snow's own rows.
pub const SNOW_ROWS: [ParamRow; 4] = [
    row("flake_size", "px", (0.05, 4096.0), (Some(1.0), Some(32.0)), 4.0),
    row("flake_size_random", "ratio", (0.0, 1.0), (None, None), 0.5),
    row("drift_amplitude", "px", (0.0, 1.0e4), (Some(0.0), Some(100.0)), 12.0),
    row("drift_frequency", "Hz", (0.0, 100.0), (Some(0.0), Some(2.0)), 0.5),
];

fn default_of(rows: &[ParamRow], key: &str) -> f64 {
    rows.iter().find(|r| r.key == key).map_or(0.0, |r| r.default)
}

/// One depth layer (module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DepthLayer {
    /// Multiplier of the drop rate.
    pub density: f64,
    /// Multiplier of the drop's size.
    pub scale: f64,
    /// Multiplier of the drop's screen velocity.
    pub speed: f64,
    pub opacity: f64,
}

impl Default for DepthLayer {
    fn default() -> Self {
        Self { density: 1.0, scale: 1.0, speed: 1.0, opacity: 1.0 }
    }
}

impl DepthLayer {
    pub fn param(&self, key: &str) -> Option<f64> {
        Some(match key {
            "layer.density" => self.density,
            "layer.scale" => self.scale,
            "layer.speed" => self.speed,
            "layer.opacity" => self.opacity,
            _ => return None,
        })
    }

    fn slot(&mut self, key: &str) -> Option<&mut f64> {
        Some(match key {
            "layer.density" => &mut self.density,
            "layer.scale" => &mut self.scale,
            "layer.speed" => &mut self.speed,
            "layer.opacity" => &mut self.opacity,
            _ => return None,
        })
    }
}

/// The record shared by Rain and Snow (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct Precipitation {
    /// STU-FX-011 explicit seed.
    pub seed: u64,
    /// The deterministic start (STU-CMP-075a), composition ticks; nothing is drawn before it.
    pub start_tick: u64,
    /// Seconds already simulated at the start tick (`0` = an empty sky at the start).
    pub preroll: f64,
    /// Drops per second over the region width per layer at density 1.
    pub rate: f64,
    /// Fall speed, px/s.
    pub speed: f64,
    /// Share `0..=1`: a drop falls at `speed (1 + r (2u - 1))`.
    pub speed_random: f64,
    /// Degrees, 0 = up, clockwise.
    pub wind_direction: f64,
    /// px/s.
    pub wind_strength: f64,
    /// `[x, y, width, height]` in composition pixels.
    pub region: [f64; 4],
    /// Drawn in order: the first is the farthest.
    pub layers: Vec<DepthLayer>,
    /// Straight RGB.
    pub colour: [f32; 3],
    pub opacity: f64,
}

impl Precipitation {
    fn from_rows(rows: &[ParamRow]) -> Self {
        let d = |k| default_of(rows, k);
        Self {
            seed: 0,
            start_tick: 0,
            preroll: d("preroll"),
            rate: d("rate"),
            speed: d("speed"),
            speed_random: d("speed_random"),
            wind_direction: d("wind_direction"),
            wind_strength: d("wind_strength"),
            region: [d("region_x"), d("region_y"), d("region_width"), d("region_height")],
            layers: vec![DepthLayer::default()],
            colour: [d("colour_r") as f32, d("colour_g") as f32, d("colour_b") as f32],
            opacity: d("opacity"),
        }
    }

    pub fn param(&self, key: &str) -> Option<f64> {
        Some(match key {
            "preroll" => self.preroll,
            "rate" => self.rate,
            "speed" => self.speed,
            "speed_random" => self.speed_random,
            "wind_direction" => self.wind_direction,
            "wind_strength" => self.wind_strength,
            "region_x" => self.region[0],
            "region_y" => self.region[1],
            "region_width" => self.region[2],
            "region_height" => self.region[3],
            "colour_r" => f64::from(self.colour[0]),
            "colour_g" => f64::from(self.colour[1]),
            "colour_b" => f64::from(self.colour[2]),
            "opacity" => self.opacity,
            _ => return None,
        })
    }

    /// `false` for a key that is not a core scalar.
    fn set(&mut self, key: &str, v: f64) -> bool {
        match key {
            "preroll" => self.preroll = v,
            "rate" => self.rate = v,
            "speed" => self.speed = v,
            "speed_random" => self.speed_random = v,
            "wind_direction" => self.wind_direction = v,
            "wind_strength" => self.wind_strength = v,
            "region_x" => self.region[0] = v,
            "region_y" => self.region[1] = v,
            "region_width" => self.region[2] = v,
            "region_height" => self.region[3] = v,
            "colour_r" => self.colour[0] = v as f32,
            "colour_g" => self.colour[1] = v as f32,
            "colour_b" => self.colour[2] = v as f32,
            "opacity" => self.opacity = v,
            _ => return false,
        }
        true
    }

    /// Sets a depth-layer field on layer `index` (refused outside its row or for a missing layer).
    pub fn set_layer_param(&mut self, index: usize, key: &str, value: f64) -> Result<()> {
        let k = checked(&RAIN_CORE_ROWS, ParamScope::DepthLayer, key, value)?;
        let layer = self.layers.get_mut(index).ok_or(ParticleError::Parameter("layers"))?;
        *layer.slot(k).ok_or(ParticleError::Parameter("param_key"))? = value;
        Ok(())
    }

    /// The wind vector, px/s.
    fn wind(&self) -> [f64; 2] {
        let th = self.wind_direction.to_radians();
        [self.wind_strength * th.sin(), -self.wind_strength * th.cos()]
    }

    /// The slowest fall before the layer's speed multiplier.
    fn slowest_fall(&self) -> f64 {
        self.speed * (1.0 - self.speed_random) + self.wind()[1]
    }

    fn validate_core(&self, rows: &[ParamRow]) -> Result<()> {
        validate_rows(rows, ParamScope::Record, |k| self.param(k))?;
        if self.layers.is_empty() || self.layers.len() > MAX_DEPTH_LAYERS {
            return Err(ParticleError::Parameter("layers"));
        }
        let fall = self.slowest_fall();
        for l in &self.layers {
            validate_rows(rows, ParamScope::DepthLayer, |k| l.param(k))?;
            if !(l.speed * fall).is_finite() || l.speed * fall < MIN_FALL_SPEED {
                return Err(ParticleError::Parameter("wind"));
            }
        }
        Ok(())
    }

    /// The per-layer birth plans at `t` and their alive estimate (no allocation beyond the layers).
    fn plan(&self, margin: impl Fn(&DepthLayer) -> f64, sway: impl Fn(&DepthLayer) -> f64, t: f64) -> Result<(Vec<LayerPlan>, f64)> {
        let [x, y, w, h] = self.region;
        let wind = self.wind();
        let mut plans = Vec::with_capacity(self.layers.len());
        let mut estimate = 0.0;
        for (i, l) in self.layers.iter().enumerate() {
            let m = margin(l);
            let e = sway(l);
            let travel = h + 2.0 * m;
            let vx = l.speed * wind[0];
            let l_max = travel / (l.speed * self.slowest_fall());
            let drift = vx * l_max;
            let (lo_x, hi_x) = if drift >= 0.0 { (x - drift - e, x + w + e) } else { (x - e, x + w - drift + e) };
            let rate = self.rate * l.density * (hi_x - lo_x) / w;
            let window = slot_window(rate, t, l_max)?;
            estimate += window_len(window);
            plans.push(LayerPlan {
                layer: u32::try_from(i).map_err(|_| ParticleError::Parameter("layers"))?,
                rate,
                lo_x,
                span: hi_x - lo_x,
                y0: y - m,
                travel,
                speed: l.speed,
                window,
            });
        }
        Ok((plans, estimate))
    }

    /// Every drop alive at `t` in draw order, with its own draws for the type's extras.
    fn drops(
        &self,
        salt: u64,
        plans: &[LayerPlan],
        t: f64,
        cancel: &CancellationToken,
        mut f: impl FnMut(&LayerPlan, DropBase, &mut Draws) -> Result<()>,
    ) -> Result<()> {
        let wind = self.wind();
        let mut n = 0u64;
        for p in plans {
            let Some((lo, hi)) = p.window else { continue };
            let key = self.seed ^ splitmix64(salt ^ u64::from(p.layer));
            for index in lo..=hi {
                if n & 0xFFF == 0 {
                    cancel.check().map_err(|_| ParticleError::Canceled)?;
                }
                n = n.wrapping_add(1);
                let mut d = Draws::new(key, index);
                let birth = (index as f64 + d.uniform()) / p.rate;
                let fall = self.speed * (1.0 + self.speed_random * (2.0 * d.uniform() - 1.0));
                let ux = d.uniform();
                let velocity = [p.speed * wind[0], p.speed * (fall + wind[1])];
                let life = p.travel / velocity[1];
                let age = t - birth;
                if age < 0.0 || age >= life {
                    continue;
                }
                f(p, DropBase { index, age, life, origin: [p.lo_x + ux * p.span, p.y0], velocity }, &mut d)?;
            }
        }
        Ok(())
    }
}

/// A layer's birth plan at one sample time.
struct LayerPlan {
    layer: u32,
    rate: f64,
    lo_x: f64,
    span: f64,
    y0: f64,
    travel: f64,
    speed: f64,
    window: Option<(u64, u64)>,
}

struct DropBase {
    index: u64,
    age: f64,
    life: f64,
    origin: [f64; 2],
    velocity: [f64; 2],
}

fn layer_of<'a>(core: &'a Precipitation, p: &LayerPlan) -> &'a DepthLayer {
    &core.layers[p.layer as usize]
}

fn checked_set(core: &mut Precipitation, rows: &[ParamRow], key: &str, value: f64) -> Result<bool> {
    if rows.iter().any(|r| r.scope == ParamScope::Record && r.key == key) {
        let k = checked(rows, ParamScope::Record, key, value)?;
        return Ok(core.set(k, value));
    }
    Ok(false)
}

/// Falling rain (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct Rain {
    pub precipitation: Precipitation,
    /// Pixels at layer scale 1.
    pub streak_length: f64,
    /// Pixels at layer scale 1.
    pub streak_width: f64,
}

impl Default for Rain {
    fn default() -> Self {
        Self {
            precipitation: Precipitation::from_rows(&RAIN_CORE_ROWS),
            streak_length: default_of(&RAIN_ROWS, "streak_length"),
            streak_width: default_of(&RAIN_ROWS, "streak_width"),
        }
    }
}

impl Rain {
    /// Core rows then rain rows.
    pub fn rows() -> Vec<ParamRow> {
        RAIN_CORE_ROWS.iter().chain(&RAIN_ROWS).copied().collect()
    }

    pub fn param(&self, key: &str) -> Option<f64> {
        match key {
            "streak_length" => Some(self.streak_length),
            "streak_width" => Some(self.streak_width),
            _ => self.precipitation.param(key),
        }
    }

    /// Sets a record scalar (refused outside its row; nothing changes).
    pub fn set_param(&mut self, key: &str, value: f64) -> Result<()> {
        if checked_set(&mut self.precipitation, &RAIN_CORE_ROWS, key, value)? {
            return Ok(());
        }
        let k = checked(&RAIN_ROWS, ParamScope::Record, key, value)?;
        if k == "streak_length" { self.streak_length = value } else { self.streak_width = value }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        self.precipitation.validate_core(&RAIN_CORE_ROWS)?;
        validate_rows(&RAIN_ROWS, ParamScope::Record, |k| self.param(k))
    }

    fn margin(&self, l: &DepthLayer) -> f64 {
        (self.streak_length + self.streak_width) * l.scale
    }

    /// The drops alive at composition `tick` (draw order). Validates the record.
    pub fn states_at(&self, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<Vec<SimParticle>> {
        self.validate()?;
        self.states(tick, limits, cancel)
    }

    fn states(&self, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<Vec<SimParticle>> {
        let core = &self.precipitation;
        let Some(t) = sim_seconds(core.start_tick, core.preroll, tick) else { return Ok(Vec::new()) };
        let (plans, estimate) = core.plan(|l| self.margin(l), |_| 0.0, t)?;
        let mut out = Vec::with_capacity(check_count(estimate, limits)?);
        core.drops(RAIN_SALT, &plans, t, cancel, |p, b, _| {
            let l = layer_of(core, p);
            out.push(SimParticle {
                layer: p.layer,
                index: b.index,
                position: [b.origin[0] + b.velocity[0] * b.age, b.origin[1] + b.velocity[1] * b.age],
                velocity: b.velocity,
                age: b.age,
                life: b.life,
                size: self.streak_width * l.scale,
                length: self.streak_length * l.scale,
                depth: 1.0,
                opacity: core.opacity * l.opacity,
                colour: core.colour,
            });
            Ok(())
        })?;
        Ok(out)
    }

    /// Renders the rain at composition `tick` over a `width x height` raster (composition pixels).
    pub fn render(&self, width: u32, height: u32, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<ParticleFrame> {
        self.validate()?;
        let pixels = check_raster(width, height, limits)?;
        let parts = self.states(tick, limits, cancel)?;
        draw(&parts, |_| Mark::Streak, width, height, pixels, limits, cancel)
    }
}

/// Falling snow (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct Snow {
    pub precipitation: Precipitation,
    /// Flake diameter in pixels at layer scale 1.
    pub flake_size: f64,
    /// Share `0..=1`: a flake is `flake_size (1 + r (2u - 1))`.
    pub flake_size_random: f64,
    /// Sway amplitude in pixels at layer scale 1.
    pub drift_amplitude: f64,
    /// Mean sway frequency, Hz (a flake's own is `f (0.5 + u)`).
    pub drift_frequency: f64,
}

impl Default for Snow {
    fn default() -> Self {
        Self {
            precipitation: Precipitation::from_rows(&SNOW_CORE_ROWS),
            flake_size: default_of(&SNOW_ROWS, "flake_size"),
            flake_size_random: default_of(&SNOW_ROWS, "flake_size_random"),
            drift_amplitude: default_of(&SNOW_ROWS, "drift_amplitude"),
            drift_frequency: default_of(&SNOW_ROWS, "drift_frequency"),
        }
    }
}

impl Snow {
    /// Core rows then snow rows.
    pub fn rows() -> Vec<ParamRow> {
        SNOW_CORE_ROWS.iter().chain(&SNOW_ROWS).copied().collect()
    }

    pub fn param(&self, key: &str) -> Option<f64> {
        match key {
            "flake_size" => Some(self.flake_size),
            "flake_size_random" => Some(self.flake_size_random),
            "drift_amplitude" => Some(self.drift_amplitude),
            "drift_frequency" => Some(self.drift_frequency),
            _ => self.precipitation.param(key),
        }
    }

    /// Sets a record scalar (refused outside its row; nothing changes).
    pub fn set_param(&mut self, key: &str, value: f64) -> Result<()> {
        if checked_set(&mut self.precipitation, &SNOW_CORE_ROWS, key, value)? {
            return Ok(());
        }
        match checked(&SNOW_ROWS, ParamScope::Record, key, value)? {
            "flake_size" => self.flake_size = value,
            "flake_size_random" => self.flake_size_random = value,
            "drift_amplitude" => self.drift_amplitude = value,
            _ => self.drift_frequency = value,
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        self.precipitation.validate_core(&SNOW_CORE_ROWS)?;
        validate_rows(&SNOW_ROWS, ParamScope::Record, |k| self.param(k))
    }

    /// The flakes alive at composition `tick` (draw order). Validates the record.
    pub fn states_at(&self, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<Vec<SimParticle>> {
        self.validate()?;
        self.states(tick, limits, cancel)
    }

    fn states(&self, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<Vec<SimParticle>> {
        let core = &self.precipitation;
        let Some(t) = sim_seconds(core.start_tick, core.preroll, tick) else { return Ok(Vec::new()) };
        let largest = self.flake_size * (1.0 + self.flake_size_random);
        let (plans, estimate) = core.plan(|l| largest * l.scale, |l| self.drift_amplitude * l.scale, t)?;
        let mut out = Vec::with_capacity(check_count(estimate, limits)?);
        core.drops(SNOW_SALT, &plans, t, cancel, |p, b, d| {
            let l = layer_of(core, p);
            let size = self.flake_size * l.scale * (1.0 + self.flake_size_random * (2.0 * d.uniform() - 1.0));
            let phase = TAU * d.uniform();
            let freq = self.drift_frequency * (0.5 + d.uniform());
            let amp = self.drift_amplitude * l.scale;
            let arg = TAU * freq * b.age + phase;
            out.push(SimParticle {
                layer: p.layer,
                index: b.index,
                position: [b.origin[0] + b.velocity[0] * b.age + amp * arg.sin(), b.origin[1] + b.velocity[1] * b.age],
                velocity: [b.velocity[0] + amp * TAU * freq * arg.cos(), b.velocity[1]],
                age: b.age,
                life: b.life,
                size,
                length: 0.0,
                depth: 1.0,
                opacity: core.opacity * l.opacity,
                colour: core.colour,
            });
            Ok(())
        })?;
        Ok(out)
    }

    /// Renders the snow at composition `tick` over a `width x height` raster (composition pixels).
    pub fn render(&self, width: u32, height: u32, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<ParticleFrame> {
        self.validate()?;
        let pixels = check_raster(width, height, limits)?;
        let parts = self.states(tick, limits, cancel)?;
        draw(&parts, |_| Mark::Dot, width, height, pixels, limits, cancel)
    }
}
