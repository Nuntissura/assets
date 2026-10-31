//! Star burst (MT-34189, STU-CMP-075 "Radial particle burst used for starfields and impacts").
//!
//! **Model.** `star_count` stars, each with seeded polar coordinates in a disc of `field_radius`
//! pixels at depth 1 (`rho = sqrt(u)`, uniform over the disc) and a depth between `near` and `far`.
//! A star approaches the camera at its speed (depth units per second, with a seeded random share)
//! and is projected through the burst centre: `screen = centre + rho R (cos th, sin th) / z`, so every
//! star moves radially out of the centre and accelerates as it nears (perspective). Its dot diameter
//! is `star_size (1 + r (2u - 1)) / z` (perspective scaling) and its opacity
//! `opacity (1 - depth_fade (z - near) / (far - near))` (far stars fade).
//!
//! **Modes.** [`BurstMode::Starfield`]: a star's depth is `near + (far - near) frac(phi - v t / (far -
//! near))`, so stars recycle from the far plane forever (a steady starfield from the start tick).
//! [`BurstMode::Impact`]: every star starts at the far plane at the start tick (all at the centre)
//! and bursts out once; a star dies when it reaches the near plane.
//!
//! Stars draw farthest first (ties by index). After Effects parity (CC Star Burst) UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use super::{
    Mark, ParamRow, ParamScope, Result, SimParticle, check_count, check_raster, checked, draw, row, sim_seconds,
    validate_rows,
};
use crate::particles::{Draws, HARD_MAX_ALIVE, ParticleError, ParticleFrame, ParticleLimits, splitmix64};
use hsk_studio_accord::CancellationToken;
use std::f64::consts::TAU;

const STAR_SALT: u64 = 0x7374_6172;

/// The authored rows (STU-FX-146).
pub const STAR_BURST_ROWS: [ParamRow; 15] = [
    row("centre_x", "px", (-1.0e7, 1.0e7), (None, None), 960.0),
    row("centre_y", "px", (-1.0e7, 1.0e7), (None, None), 540.0),
    row("star_count", "count", (0.0, HARD_MAX_ALIVE as f64), (Some(0.0), Some(5000.0)), 1000.0),
    row("field_radius", "px", (0.0, 1.0e7), (Some(0.0), Some(4000.0)), 1500.0),
    row("near", "depth", (1.0e-3, 1.0e6), (Some(0.05), Some(1.0)), 0.1),
    row("far", "depth", (1.0e-3, 1.0e6), (Some(1.0), Some(20.0)), 10.0),
    row("speed", "depth/s", (0.0, 1.0e6), (Some(0.0), Some(20.0)), 2.0),
    row("speed_random", "ratio", (0.0, 1.0), (None, None), 0.0),
    row("star_size", "px", (0.0, 4096.0), (Some(0.0), Some(8.0)), 1.5),
    row("size_random", "ratio", (0.0, 1.0), (None, None), 0.5),
    row("depth_fade", "ratio", (0.0, 1.0), (None, None), 1.0),
    row("opacity", "ratio", (0.0, 1.0), (None, None), 1.0),
    row("colour_r", "ratio", (0.0, 1.0), (None, None), 1.0),
    row("colour_g", "ratio", (0.0, 1.0), (None, None), 1.0),
    row("colour_b", "ratio", (0.0, 1.0), (None, None), 1.0),
];

/// Starfield (recycling) or impact (one burst).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BurstMode {
    #[default]
    Starfield,
    Impact,
}

/// One star burst layer (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct StarBurst {
    /// STU-FX-011 explicit seed.
    pub seed: u64,
    /// The deterministic start (STU-CMP-075a), composition ticks; nothing is drawn before it.
    pub start_tick: u64,
    pub mode: BurstMode,
    pub centre: [f64; 2],
    pub star_count: u32,
    /// Pixels at depth 1.
    pub field_radius: f64,
    /// `0 < near < far`.
    pub near: f64,
    pub far: f64,
    /// Depth units per second.
    pub speed: f64,
    pub speed_random: f64,
    /// Pixels at depth 1.
    pub star_size: f64,
    pub size_random: f64,
    pub depth_fade: f64,
    pub opacity: f64,
    pub colour: [f32; 3],
}

impl Default for StarBurst {
    fn default() -> Self {
        let d = |k: &str| STAR_BURST_ROWS.iter().find(|r| r.key == k).map_or(0.0, |r| r.default);
        Self {
            seed: 0,
            start_tick: 0,
            mode: BurstMode::Starfield,
            centre: [d("centre_x"), d("centre_y")],
            star_count: d("star_count") as u32,
            field_radius: d("field_radius"),
            near: d("near"),
            far: d("far"),
            speed: d("speed"),
            speed_random: d("speed_random"),
            star_size: d("star_size"),
            size_random: d("size_random"),
            depth_fade: d("depth_fade"),
            opacity: d("opacity"),
            colour: [d("colour_r") as f32, d("colour_g") as f32, d("colour_b") as f32],
        }
    }
}

impl StarBurst {
    pub fn param(&self, key: &str) -> Option<f64> {
        Some(match key {
            "centre_x" => self.centre[0],
            "centre_y" => self.centre[1],
            "star_count" => f64::from(self.star_count),
            "field_radius" => self.field_radius,
            "near" => self.near,
            "far" => self.far,
            "speed" => self.speed,
            "speed_random" => self.speed_random,
            "star_size" => self.star_size,
            "size_random" => self.size_random,
            "depth_fade" => self.depth_fade,
            "opacity" => self.opacity,
            "colour_r" => f64::from(self.colour[0]),
            "colour_g" => f64::from(self.colour[1]),
            "colour_b" => f64::from(self.colour[2]),
            _ => return None,
        })
    }

    /// Sets a record scalar (refused outside its row, or a non-integral count; nothing changes).
    pub fn set_param(&mut self, key: &str, v: f64) -> Result<()> {
        match checked(&STAR_BURST_ROWS, ParamScope::Record, key, v)? {
            "centre_x" => self.centre[0] = v,
            "centre_y" => self.centre[1] = v,
            "star_count" => {
                if v.fract() != 0.0 {
                    return Err(ParticleError::Parameter("star_count"));
                }
                self.star_count = v as u32;
            }
            "field_radius" => self.field_radius = v,
            "near" => self.near = v,
            "far" => self.far = v,
            "speed" => self.speed = v,
            "speed_random" => self.speed_random = v,
            "star_size" => self.star_size = v,
            "size_random" => self.size_random = v,
            "depth_fade" => self.depth_fade = v,
            "opacity" => self.opacity = v,
            "colour_r" => self.colour[0] = v as f32,
            "colour_g" => self.colour[1] = v as f32,
            _ => self.colour[2] = v as f32,
        }
        Ok(())
    }

    /// Every row in bounds and `near < far`.
    pub fn validate(&self) -> Result<()> {
        validate_rows(&STAR_BURST_ROWS, ParamScope::Record, |k| self.param(k))?;
        if self.near >= self.far {
            return Err(ParticleError::Parameter("near"));
        }
        Ok(())
    }

    /// The stars alive at composition `tick` (draw order: farthest first). Validates the record.
    pub fn states_at(&self, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<Vec<SimParticle>> {
        self.validate()?;
        self.states(tick, limits, cancel)
    }

    fn states(&self, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<Vec<SimParticle>> {
        let Some(t) = sim_seconds(self.start_tick, 0.0, tick) else { return Ok(Vec::new()) };
        let mut out = Vec::with_capacity(check_count(f64::from(self.star_count), limits)?);
        let span = self.far - self.near;
        let key = self.seed ^ splitmix64(STAR_SALT);
        for index in 0..u64::from(self.star_count) {
            if index & 0xFFF == 0 {
                cancel.check().map_err(|_| ParticleError::Canceled)?;
            }
            let mut d = Draws::new(key, index);
            let theta = TAU * d.uniform();
            let rho = d.uniform().sqrt() * self.field_radius;
            let phase = d.uniform();
            let speed = self.speed * (1.0 + self.speed_random * (2.0 * d.uniform() - 1.0));
            let size = self.star_size * (1.0 + self.size_random * (2.0 * d.uniform() - 1.0));
            let (z, life) = match self.mode {
                BurstMode::Starfield => {
                    let x = phase - speed * t / span;
                    (self.near + span * (x - x.floor()), f64::INFINITY)
                }
                BurstMode::Impact => {
                    let z = self.far - speed * t;
                    if z <= self.near {
                        continue;
                    }
                    (z, if speed > 0.0 { span / speed } else { f64::INFINITY })
                }
            };
            let off = [rho * theta.cos() / z, rho * theta.sin() / z];
            if !(off[0].is_finite() && off[1].is_finite()) {
                return Err(ParticleError::NonFinite);
            }
            out.push(SimParticle {
                layer: 0,
                index,
                position: [self.centre[0] + off[0], self.centre[1] + off[1]],
                velocity: [off[0] * speed / z, off[1] * speed / z],
                age: t,
                life,
                size: size / z,
                length: 0.0,
                depth: z,
                opacity: self.opacity * (1.0 - self.depth_fade * (z - self.near) / span),
                colour: self.colour,
            });
        }
        out.sort_by(|a, b| b.depth.total_cmp(&a.depth).then(a.index.cmp(&b.index)));
        Ok(out)
    }

    /// Renders the burst at composition `tick` over a `width x height` raster (composition pixels).
    pub fn render(&self, width: u32, height: u32, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<ParticleFrame> {
        self.validate()?;
        let pixels = check_raster(width, height, limits)?;
        let parts = self.states(tick, limits, cancel)?;
        draw(&parts, |_| Mark::Dot, width, height, pixels, limits, cancel)
    }
}
