//! Particle systems (classic) (MT-34177, STU-CMP-075 "The earlier particle generator retained
//! because shipped documents reference it; it is a separate identity, not a preset of the above").
//!
//! **Identity.** Its own [`super::SimulationKind::ParticleSystemsClassic`] id, name, import keys and
//! record ([`ParticleSystemsClassic`]); nothing here builds or reads a
//! [`crate::particles::ParticleWorld`], and neither identity resolves to the other.
//!
//! **Record.** A producer (a point, or an ellipse of `producer_radius` px), a constant birth rate, a
//! fixed longevity, direction / spread / velocity with a random share, gravity as a strength and a
//! direction (degrees, 0 = up, clockwise; 180 pulls down), resistance, a particle type ([`Line`],
//! [`FadedSphere`] or [`Dot`]), birth -> death size and colour (linear in age) and a maximum opacity
//! that fades linearly to 0 at death.
//!
//! **Physics (closed form, exact).** Linear drag `k` and constant gravity `g`:
//! `v(a) = v0 e^{-k a} + g E1(a)`, `p(a) = p0 + v0 E1(a) + g E2(a)`, with `E1 = (1 - e^{-k a}) / k`
//! and `E2 = (a - E1) / k` (their series below `k a = 1e-4`; `E1 = a`, `E2 = a^2 / 2` at `k = 0`).
//! A particle's state is a function of its own birth only, so frames are frame-rate independent and
//! random access. Births are on the shared slot grid (module docs of [`super`]).
//!
//! **Marks.** [`Line`]: a 1 px antialiased line, as long as the particle's size, back along its
//! velocity; [`FadedSphere`]: a dot whose coverage falls linearly from the centre to 0 at its
//! radius; [`Dot`]: the solid antialiased dot. Draw order: ascending birth index. After Effects
//! parity (CC Particle Systems classic behaviour, type looks, gravity convention) UNVERIFIED (D5).
//!
//! [`Line`]: ClassicParticleType::Line
//! [`FadedSphere`]: ClassicParticleType::FadedSphere
//! [`Dot`]: ClassicParticleType::Dot
#![deny(clippy::arithmetic_side_effects)]
use super::{
    Mark, ParamRow, ParamScope, Result, SimParticle, check_count, check_raster, checked, draw, row, sim_seconds,
    slot_window, validate_rows, window_len,
};
use crate::particles::{Draws, ParticleError, ParticleFrame, ParticleLimits, lerp3, splitmix64};
use hsk_studio_accord::CancellationToken;
use std::f64::consts::TAU;

const CLASSIC_SALT: u64 = 0x636C_6173;

/// The authored rows (STU-FX-146).
pub const CLASSIC_ROWS: [ParamRow; 22] = [
    row("producer_x", "px", (-1.0e7, 1.0e7), (None, None), 960.0),
    row("producer_y", "px", (-1.0e7, 1.0e7), (None, None), 540.0),
    row("producer_radius_x", "px", (0.0, 1.0e7), (Some(0.0), Some(500.0)), 0.0),
    row("producer_radius_y", "px", (0.0, 1.0e7), (Some(0.0), Some(500.0)), 0.0),
    row("birth_rate", "particles/s", (0.0, 1.0e6), (Some(0.0), Some(1000.0)), 100.0),
    row("longevity", "s", (1.0e-3, 600.0), (Some(0.1), Some(10.0)), 2.0),
    row("direction", "deg", (-1.0e6, 1.0e6), (Some(-180.0), Some(180.0)), 0.0),
    row("spread", "deg", (0.0, 360.0), (None, None), 360.0),
    row("velocity", "px/s", (0.0, 1.0e6), (Some(0.0), Some(2000.0)), 200.0),
    row("velocity_random", "ratio", (0.0, 1.0), (None, None), 0.25),
    row("gravity", "px/s2", (-1.0e6, 1.0e6), (Some(-2000.0), Some(2000.0)), 200.0),
    row("gravity_direction", "deg", (-1.0e6, 1.0e6), (Some(-180.0), Some(180.0)), 180.0),
    row("resistance", "1/s", (0.0, 1000.0), (Some(0.0), Some(10.0)), 0.0),
    row("birth_size", "px", (0.0, 4096.0), (Some(0.0), Some(64.0)), 4.0),
    row("death_size", "px", (0.0, 4096.0), (Some(0.0), Some(64.0)), 1.0),
    row("max_opacity", "ratio", (0.0, 1.0), (None, None), 1.0),
    row("birth_colour_r", "ratio", (0.0, 1.0), (None, None), 1.0),
    row("birth_colour_g", "ratio", (0.0, 1.0), (None, None), 1.0),
    row("birth_colour_b", "ratio", (0.0, 1.0), (None, None), 0.0),
    row("death_colour_r", "ratio", (0.0, 1.0), (None, None), 1.0),
    row("death_colour_g", "ratio", (0.0, 1.0), (None, None), 0.0),
    row("death_colour_b", "ratio", (0.0, 1.0), (None, None), 0.0),
];

/// How a classic particle is drawn (module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClassicParticleType {
    Line,
    #[default]
    FadedSphere,
    Dot,
}

/// One classic particle system layer (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct ParticleSystemsClassic {
    /// STU-FX-011 explicit seed.
    pub seed: u64,
    /// The deterministic start (STU-CMP-075a), composition ticks; nothing is drawn before it.
    pub start_tick: u64,
    pub particle_type: ClassicParticleType,
    pub producer: [f64; 2],
    pub producer_radius: [f64; 2],
    /// Particles per second.
    pub birth_rate: f64,
    /// Seconds.
    pub longevity: f64,
    /// Degrees, 0 = up, clockwise.
    pub direction: f64,
    /// Degrees centred on `direction`.
    pub spread: f64,
    /// px/s.
    pub velocity: f64,
    /// Share `0..=1`: speed is `velocity (1 + r (2u - 1))`.
    pub velocity_random: f64,
    /// px/s^2.
    pub gravity: f64,
    /// Degrees, 0 = up, clockwise.
    pub gravity_direction: f64,
    /// Per second.
    pub resistance: f64,
    pub birth_size: f64,
    pub death_size: f64,
    pub max_opacity: f64,
    pub birth_colour: [f32; 3],
    pub death_colour: [f32; 3],
}

impl Default for ParticleSystemsClassic {
    fn default() -> Self {
        let d = |k: &str| CLASSIC_ROWS.iter().find(|r| r.key == k).map_or(0.0, |r| r.default);
        Self {
            seed: 0,
            start_tick: 0,
            particle_type: ClassicParticleType::default(),
            producer: [d("producer_x"), d("producer_y")],
            producer_radius: [d("producer_radius_x"), d("producer_radius_y")],
            birth_rate: d("birth_rate"),
            longevity: d("longevity"),
            direction: d("direction"),
            spread: d("spread"),
            velocity: d("velocity"),
            velocity_random: d("velocity_random"),
            gravity: d("gravity"),
            gravity_direction: d("gravity_direction"),
            resistance: d("resistance"),
            birth_size: d("birth_size"),
            death_size: d("death_size"),
            max_opacity: d("max_opacity"),
            birth_colour: [d("birth_colour_r") as f32, d("birth_colour_g") as f32, d("birth_colour_b") as f32],
            death_colour: [d("death_colour_r") as f32, d("death_colour_g") as f32, d("death_colour_b") as f32],
        }
    }
}

/// `(E1, E2)` of the module docs.
fn drag_terms(k: f64, a: f64) -> (f64, f64) {
    let x = k * a;
    if x < 1.0e-4 {
        let (a2, a3) = (a * a, a * a * a);
        (a - k * a2 / 2.0 + k * k * a3 / 6.0, a2 / 2.0 - k * a3 / 6.0 + k * k * a3 * a / 24.0)
    } else {
        let e1 = -(-x).exp_m1() / k;
        (e1, (a - e1) / k)
    }
}

impl ParticleSystemsClassic {
    pub fn param(&self, key: &str) -> Option<f64> {
        Some(match key {
            "producer_x" => self.producer[0],
            "producer_y" => self.producer[1],
            "producer_radius_x" => self.producer_radius[0],
            "producer_radius_y" => self.producer_radius[1],
            "birth_rate" => self.birth_rate,
            "longevity" => self.longevity,
            "direction" => self.direction,
            "spread" => self.spread,
            "velocity" => self.velocity,
            "velocity_random" => self.velocity_random,
            "gravity" => self.gravity,
            "gravity_direction" => self.gravity_direction,
            "resistance" => self.resistance,
            "birth_size" => self.birth_size,
            "death_size" => self.death_size,
            "max_opacity" => self.max_opacity,
            "birth_colour_r" => f64::from(self.birth_colour[0]),
            "birth_colour_g" => f64::from(self.birth_colour[1]),
            "birth_colour_b" => f64::from(self.birth_colour[2]),
            "death_colour_r" => f64::from(self.death_colour[0]),
            "death_colour_g" => f64::from(self.death_colour[1]),
            "death_colour_b" => f64::from(self.death_colour[2]),
            _ => return None,
        })
    }

    /// Sets a record scalar (refused outside its row; nothing changes).
    pub fn set_param(&mut self, key: &str, v: f64) -> Result<()> {
        let slot = match checked(&CLASSIC_ROWS, ParamScope::Record, key, v)? {
            "producer_x" => &mut self.producer[0],
            "producer_y" => &mut self.producer[1],
            "producer_radius_x" => &mut self.producer_radius[0],
            "producer_radius_y" => &mut self.producer_radius[1],
            "birth_rate" => &mut self.birth_rate,
            "longevity" => &mut self.longevity,
            "direction" => &mut self.direction,
            "spread" => &mut self.spread,
            "velocity" => &mut self.velocity,
            "velocity_random" => &mut self.velocity_random,
            "gravity" => &mut self.gravity,
            "gravity_direction" => &mut self.gravity_direction,
            "resistance" => &mut self.resistance,
            "birth_size" => &mut self.birth_size,
            "death_size" => &mut self.death_size,
            "max_opacity" => &mut self.max_opacity,
            k => {
                let (colour, i) = match k {
                    "birth_colour_r" => (&mut self.birth_colour, 0),
                    "birth_colour_g" => (&mut self.birth_colour, 1),
                    "birth_colour_b" => (&mut self.birth_colour, 2),
                    "death_colour_r" => (&mut self.death_colour, 0),
                    "death_colour_g" => (&mut self.death_colour, 1),
                    _ => (&mut self.death_colour, 2),
                };
                colour[i] = v as f32;
                return Ok(());
            }
        };
        *slot = v;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        validate_rows(&CLASSIC_ROWS, ParamScope::Record, |k| self.param(k))
    }

    /// The particles alive at composition `tick` (ascending birth index). Validates the record.
    pub fn states_at(&self, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<Vec<SimParticle>> {
        self.validate()?;
        self.states(tick, limits, cancel)
    }

    fn states(&self, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<Vec<SimParticle>> {
        let Some(t) = sim_seconds(self.start_tick, 0.0, tick) else { return Ok(Vec::new()) };
        let life = self.longevity;
        let window = slot_window(self.birth_rate, t, life)?;
        let mut out = Vec::with_capacity(check_count(window_len(window), limits)?);
        let Some((lo, hi)) = window else { return Ok(out) };
        let gd = self.gravity_direction.to_radians();
        let g = [self.gravity * gd.sin(), -self.gravity * gd.cos()];
        let key = self.seed ^ splitmix64(CLASSIC_SALT);
        for index in lo..=hi {
            if index & 0xFFF == 0 {
                cancel.check().map_err(|_| ParticleError::Canceled)?;
            }
            let mut d = Draws::new(key, index);
            let birth = (index as f64 + d.uniform()) / self.birth_rate;
            let theta = (self.direction + self.spread * (d.uniform() - 0.5)).to_radians();
            let speed = self.velocity * (1.0 + self.velocity_random * (2.0 * d.uniform() - 1.0));
            let (r, phi) = (d.uniform().sqrt(), TAU * d.uniform());
            let age = t - birth;
            if age < 0.0 || age >= life {
                continue;
            }
            let p0 = [self.producer[0] + self.producer_radius[0] * r * phi.cos(), self.producer[1] + self.producer_radius[1] * r * phi.sin()];
            let v0 = [speed * theta.sin(), -speed * theta.cos()];
            let (e1, e2) = drag_terms(self.resistance, age);
            let damp = (-self.resistance * age).exp();
            let position = [p0[0] + v0[0] * e1 + g[0] * e2, p0[1] + v0[1] * e1 + g[1] * e2];
            let velocity = [v0[0] * damp + g[0] * e1, v0[1] * damp + g[1] * e1];
            if !position.iter().chain(&velocity).all(|v| v.is_finite()) {
                return Err(ParticleError::NonFinite);
            }
            let a = age / life;
            let size = self.birth_size + (self.death_size - self.birth_size) * a;
            let (size, length) = match self.particle_type {
                ClassicParticleType::Line => (1.0, size),
                _ => (size, 0.0),
            };
            out.push(SimParticle {
                layer: 0,
                index,
                position,
                velocity,
                age,
                life,
                size,
                length,
                depth: 1.0,
                opacity: self.max_opacity * (1.0 - a),
                colour: lerp3(self.birth_colour, self.death_colour, a),
            });
        }
        Ok(out)
    }

    /// Renders the system at composition `tick` over a `width x height` raster (composition pixels).
    pub fn render(&self, width: u32, height: u32, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<ParticleFrame> {
        self.validate()?;
        let pixels = check_raster(width, height, limits)?;
        let parts = self.states(tick, limits, cancel)?;
        let mark = match self.particle_type {
            ClassicParticleType::Line => Mark::Streak,
            ClassicParticleType::FadedSphere => Mark::Soft,
            ClassicParticleType::Dot => Mark::Dot,
        };
        draw(&parts, |_| mark, width, height, pixels, limits, cancel)
    }
}
