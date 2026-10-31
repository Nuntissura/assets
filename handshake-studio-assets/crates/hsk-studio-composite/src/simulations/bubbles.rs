//! Bubbles (MT-34186, STU-CMP-075 "Rising-bubble generation, distinct from foam in that it is not
//! surface-bound") and the per-bubble shading shared with Foam ([`BubbleShading`]).
//!
//! **Record ([`Bubbles`]).** Seed, start tick, preroll, a birth rate, a life with a random share, the
//! region the bubbles rise through, a rise speed with a random share, a sinusoidal wobble, a birth
//! size with a random share, a growth rate, an opacity and the shared [`BubbleShading`]. Rows:
//! [`BUBBLES_ROWS`] then the `shading.*` rows [`BUBBLE_SHADING_ROWS`] ([`Bubbles::rows`]).
//!
//! **Motion (closed form, random access).** Bubbles are born on the slot grid of [`super`] (bubble
//! `k` at `(k + u_k) / rate`) at a seeded x on the region's bottom edge. At age `a` a bubble is at
//! `x = x_k + A sin(2 pi f_k a + phi_k)`, `y = bottom - v_k a`, with diameter `d_k + growth a`;
//! `v_k = rise_speed (1 + r (2u - 1))`, `f_k = wobble_frequency (0.5 + u)`,
//! `d_k = size (1 + r (2u - 1))`, `life_k = life (1 + r (2u - 1))`, every `u` the bubble's own
//! draw. A bubble pops when its centre reaches the region's top edge or at the end of its life,
//! whichever is first, so it is not bound to a surface. Draw order: ascending slot index.
//!
//! **Shading ([`BubbleShading`], declared once, `studio_declared`).** An antialiased disc whose
//! opacity runs from `fill_opacity` inside to `rim_opacity` across an outer rim of
//! `rim_width x radius`, tinted `colour`, with a white highlight of `highlight_size x radius` and
//! `highlight_opacity` centred `0.35 radius` up and left of the centre, composited over the body
//! ([`BubbleShading::sample`]); the record opacity scales the whole bubble. After Effects parity
//! (CC Bubbles behaviour and look) is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use super::{
    ParamRow, ParamScope, Result, SimParticle, check_count, check_raster, checked, row, sim_seconds, slot_window,
    validate_rows, window_len,
};
use crate::particles::{Draws, ParticleError, ParticleFrame, ParticleLimits, footprint, over, splitmix64};
use hsk_studio_accord::CancellationToken;
use std::f64::consts::TAU;

const BUBBLE_SALT: u64 = 0x6275_6262;
/// Highlight centre offset up and left, in radii.
pub const HIGHLIGHT_OFFSET: f64 = 0.35;

/// The shared per-bubble shading rows (STU-FX-146: authored, `studio_declared`).
pub const BUBBLE_SHADING_ROWS: [ParamRow; 8] = [
    row("shading.rim_width", "ratio", (0.0, 1.0), (None, None), 0.15),
    row("shading.rim_opacity", "ratio", (0.0, 1.0), (None, None), 0.9),
    row("shading.fill_opacity", "ratio", (0.0, 1.0), (None, None), 0.15),
    row("shading.highlight_size", "ratio", (0.0, 1.0), (None, None), 0.3),
    row("shading.highlight_opacity", "ratio", (0.0, 1.0), (None, None), 0.8),
    row("shading.colour_r", "ratio", (0.0, 1.0), (None, None), 0.875),
    row("shading.colour_g", "ratio", (0.0, 1.0), (None, None), 0.9375),
    row("shading.colour_b", "ratio", (0.0, 1.0), (None, None), 1.0),
];

/// Bubbles' own record rows.
pub const BUBBLES_ROWS: [ParamRow; 16] = [
    row("preroll", "s", (0.0, 600.0), (Some(0.0), Some(10.0)), 0.0),
    row("rate", "bubbles/s", (0.0, 1.0e6), (Some(0.0), Some(500.0)), 20.0),
    row("life", "s", (1.0e-3, 600.0), (Some(0.1), Some(20.0)), 4.0),
    row("life_random", "ratio", (0.0, 0.99), (None, None), 0.2),
    row("region_x", "px", (-1.0e7, 1.0e7), (None, None), 0.0),
    row("region_y", "px", (-1.0e7, 1.0e7), (None, None), 0.0),
    row("region_width", "px", (1.0, 1.0e7), (None, None), 1920.0),
    row("region_height", "px", (1.0, 1.0e7), (None, None), 1080.0),
    row("rise_speed", "px/s", (0.0, 1.0e6), (Some(0.0), Some(1000.0)), 120.0),
    row("rise_speed_random", "ratio", (0.0, 1.0), (None, None), 0.3),
    row("wobble_amplitude", "px", (0.0, 1.0e4), (Some(0.0), Some(50.0)), 6.0),
    row("wobble_frequency", "Hz", (0.0, 100.0), (Some(0.0), Some(4.0)), 1.0),
    row("size", "px", (0.05, 4096.0), (Some(1.0), Some(128.0)), 12.0),
    row("size_random", "ratio", (0.0, 1.0), (None, None), 0.5),
    row("growth", "px/s", (0.0, 4096.0), (Some(0.0), Some(20.0)), 2.0),
    row("opacity", "ratio", (0.0, 1.0), (None, None), 1.0),
];

pub(crate) fn default_of(rows: &[ParamRow], key: &str) -> f64 {
    rows.iter().find(|r| r.key == key).map_or(0.0, |r| r.default)
}

/// The shared per-bubble look (module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BubbleShading {
    /// Share of the radius.
    pub rim_width: f64,
    pub rim_opacity: f64,
    pub fill_opacity: f64,
    /// Share of the radius.
    pub highlight_size: f64,
    pub highlight_opacity: f64,
    /// Straight RGB.
    pub colour: [f32; 3],
}

impl Default for BubbleShading {
    fn default() -> Self {
        let d = |k| default_of(&BUBBLE_SHADING_ROWS, k);
        Self {
            rim_width: d("shading.rim_width"),
            rim_opacity: d("shading.rim_opacity"),
            fill_opacity: d("shading.fill_opacity"),
            highlight_size: d("shading.highlight_size"),
            highlight_opacity: d("shading.highlight_opacity"),
            colour: [d("shading.colour_r") as f32, d("shading.colour_g") as f32, d("shading.colour_b") as f32],
        }
    }
}

impl BubbleShading {
    pub fn param(&self, key: &str) -> Option<f64> {
        Some(match key {
            "shading.rim_width" => self.rim_width,
            "shading.rim_opacity" => self.rim_opacity,
            "shading.fill_opacity" => self.fill_opacity,
            "shading.highlight_size" => self.highlight_size,
            "shading.highlight_opacity" => self.highlight_opacity,
            "shading.colour_r" => f64::from(self.colour[0]),
            "shading.colour_g" => f64::from(self.colour[1]),
            "shading.colour_b" => f64::from(self.colour[2]),
            _ => return None,
        })
    }

    /// `Ok(false)` for a key that is not a shading row; a shading row outside its bounds is refused.
    pub(crate) fn set(&mut self, key: &str, value: f64) -> Result<bool> {
        if !BUBBLE_SHADING_ROWS.iter().any(|r| r.key == key) {
            return Ok(false);
        }
        match checked(&BUBBLE_SHADING_ROWS, ParamScope::Record, key, value)? {
            "shading.rim_width" => self.rim_width = value,
            "shading.rim_opacity" => self.rim_opacity = value,
            "shading.fill_opacity" => self.fill_opacity = value,
            "shading.highlight_size" => self.highlight_size = value,
            "shading.highlight_opacity" => self.highlight_opacity = value,
            "shading.colour_r" => self.colour[0] = value as f32,
            "shading.colour_g" => self.colour[1] = value as f32,
            _ => self.colour[2] = value as f32,
        }
        Ok(true)
    }

    pub fn validate(&self) -> Result<()> {
        validate_rows(&BUBBLE_SHADING_ROWS, ParamScope::Record, |k| self.param(k))
    }

    /// Premultiplied RGBA of a bubble of `radius` pixels at offset `d` (pixel centre minus bubble
    /// centre), before the record opacity (module docs).
    pub fn sample(&self, d: [f64; 2], radius: f64) -> [f32; 4] {
        let r = radius.max(0.5);
        let small = if radius < 0.5 { 4.0 * radius * radius } else { 1.0 };
        let dist = d[0].hypot(d[1]);
        let cov = (r + 0.5 - dist).clamp(0.0, 1.0) * small;
        if cov <= 0.0 {
            return [0.0; 4];
        }
        let rw = (self.rim_width * r).max(0.5);
        let rim = ((dist - (r - rw)) / rw).clamp(0.0, 1.0);
        let a = (cov * (self.fill_opacity + (self.rim_opacity - self.fill_opacity) * rim)) as f32;
        let mut px = [self.colour[0] * a, self.colour[1] * a, self.colour[2] * a, a];
        let hr = self.highlight_size * r;
        if hr > 0.0 {
            let o = HIGHLIGHT_OFFSET * r;
            let hd = (d[0] + o).hypot(d[1] + o);
            let h = ((1.0 - hd / hr).clamp(0.0, 1.0) * self.highlight_opacity * cov) as f32;
            if h > 0.0 {
                over(&mut px, [h, h, h, h]);
            }
        }
        px
    }
}

/// Draws bubbles (`size` = diameter, `opacity` = the record opacity) in order with `shading` over a
/// `width x height` raster; the raster size and the work are checked before the plane is allocated.
/// Bubbles and Foam render through this one path.
pub fn render_bubble_marks(
    parts: &[SimParticle],
    shading: &BubbleShading,
    width: u32,
    height: u32,
    limits: &ParticleLimits,
    cancel: &CancellationToken,
) -> Result<ParticleFrame> {
    shading.validate()?;
    let pixels = check_raster(width, height, limits)?;
    let bounds = |p: &SimParticle| (p.position, [(p.size / 2.0).max(0.5); 2]);
    let mut work = 0u64;
    for p in parts {
        let (c, half) = bounds(p);
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
        let (c, half) = bounds(p);
        let Some((x0, y0, x1, y1)) = footprint(c, half, width, height) else { continue };
        let o = p.opacity as f32;
        for y in y0..y1 {
            for x in x0..x1 {
                let s = shading.sample([f64::from(x) + 0.5 - c[0], f64::from(y) + 0.5 - c[1]], p.size / 2.0);
                if s[3] <= 0.0 {
                    continue;
                }
                let i = (y as usize).wrapping_mul(width as usize).wrapping_add(x as usize);
                over(&mut plane[i], [s[0] * o, s[1] * o, s[2] * o, s[3] * o]);
            }
        }
    }
    Ok(ParticleFrame { width, height, premultiplied: plane, drawn: parts.len() as u64 })
}

/// Rising bubbles (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct Bubbles {
    /// STU-FX-011 explicit seed.
    pub seed: u64,
    /// The deterministic start (STU-CMP-075a), composition ticks; nothing is drawn before it.
    pub start_tick: u64,
    /// Seconds already simulated at the start tick.
    pub preroll: f64,
    /// Bubbles per second.
    pub rate: f64,
    /// Seconds.
    pub life: f64,
    /// Share `0..=0.99`.
    pub life_random: f64,
    /// `[x, y, width, height]` in composition pixels: born on the bottom edge, popped at the top.
    pub region: [f64; 4],
    /// px/s upwards.
    pub rise_speed: f64,
    pub rise_speed_random: f64,
    /// Pixels.
    pub wobble_amplitude: f64,
    /// Mean Hz (a bubble's own is `f (0.5 + u)`).
    pub wobble_frequency: f64,
    /// Birth diameter, pixels.
    pub size: f64,
    pub size_random: f64,
    /// Diameter growth, px/s.
    pub growth: f64,
    pub opacity: f64,
    pub shading: BubbleShading,
}

impl Default for Bubbles {
    fn default() -> Self {
        let d = |k| default_of(&BUBBLES_ROWS, k);
        Self {
            seed: 0,
            start_tick: 0,
            preroll: d("preroll"),
            rate: d("rate"),
            life: d("life"),
            life_random: d("life_random"),
            region: [d("region_x"), d("region_y"), d("region_width"), d("region_height")],
            rise_speed: d("rise_speed"),
            rise_speed_random: d("rise_speed_random"),
            wobble_amplitude: d("wobble_amplitude"),
            wobble_frequency: d("wobble_frequency"),
            size: d("size"),
            size_random: d("size_random"),
            growth: d("growth"),
            opacity: d("opacity"),
            shading: BubbleShading::default(),
        }
    }
}

impl Bubbles {
    /// Record rows then the shared shading rows.
    pub fn rows() -> Vec<ParamRow> {
        BUBBLES_ROWS.iter().chain(&BUBBLE_SHADING_ROWS).copied().collect()
    }

    pub fn param(&self, key: &str) -> Option<f64> {
        Some(match key {
            "preroll" => self.preroll,
            "rate" => self.rate,
            "life" => self.life,
            "life_random" => self.life_random,
            "region_x" => self.region[0],
            "region_y" => self.region[1],
            "region_width" => self.region[2],
            "region_height" => self.region[3],
            "rise_speed" => self.rise_speed,
            "rise_speed_random" => self.rise_speed_random,
            "wobble_amplitude" => self.wobble_amplitude,
            "wobble_frequency" => self.wobble_frequency,
            "size" => self.size,
            "size_random" => self.size_random,
            "growth" => self.growth,
            "opacity" => self.opacity,
            _ => return self.shading.param(key),
        })
    }

    /// Sets a record or shading scalar (refused outside its row; nothing changes).
    pub fn set_param(&mut self, key: &str, value: f64) -> Result<()> {
        if self.shading.set(key, value)? {
            return Ok(());
        }
        match checked(&BUBBLES_ROWS, ParamScope::Record, key, value)? {
            "preroll" => self.preroll = value,
            "rate" => self.rate = value,
            "life" => self.life = value,
            "life_random" => self.life_random = value,
            "region_x" => self.region[0] = value,
            "region_y" => self.region[1] = value,
            "region_width" => self.region[2] = value,
            "region_height" => self.region[3] = value,
            "rise_speed" => self.rise_speed = value,
            "rise_speed_random" => self.rise_speed_random = value,
            "wobble_amplitude" => self.wobble_amplitude = value,
            "wobble_frequency" => self.wobble_frequency = value,
            "size" => self.size = value,
            "size_random" => self.size_random = value,
            "growth" => self.growth = value,
            _ => self.opacity = value,
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        validate_rows(&BUBBLES_ROWS, ParamScope::Record, |k| self.param(k))?;
        self.shading.validate()
    }

    /// The bubbles alive at composition `tick` (draw order). Validates the record.
    pub fn states_at(&self, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<Vec<SimParticle>> {
        self.validate()?;
        self.states(tick, limits, cancel)
    }

    fn states(&self, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<Vec<SimParticle>> {
        let Some(t) = sim_seconds(self.start_tick, self.preroll, tick) else { return Ok(Vec::new()) };
        let l_max = self.life * (1.0 + self.life_random);
        let window = slot_window(self.rate, t, l_max)?;
        let mut out = Vec::with_capacity(check_count(window_len(window), limits)?);
        let Some((lo, hi)) = window else { return Ok(out) };
        let [rx, ry, rw, rh] = self.region;
        let key = self.seed ^ splitmix64(BUBBLE_SALT);
        let mut n = 0u64;
        for index in lo..=hi {
            if n & 0xFFF == 0 {
                cancel.check().map_err(|_| ParticleError::Canceled)?;
            }
            n = n.wrapping_add(1);
            let mut d = Draws::new(key, index);
            let birth = (index as f64 + d.uniform()) / self.rate;
            let ux = d.uniform();
            let life = self.life * (1.0 + self.life_random * (2.0 * d.uniform() - 1.0));
            let speed = self.rise_speed * (1.0 + self.rise_speed_random * (2.0 * d.uniform() - 1.0));
            let size = self.size * (1.0 + self.size_random * (2.0 * d.uniform() - 1.0));
            let phase = TAU * d.uniform();
            let freq = self.wobble_frequency * (0.5 + d.uniform());
            let age = t - birth;
            let top = if speed > 0.0 { rh / speed } else { f64::INFINITY };
            let end = life.min(top);
            if age < 0.0 || age >= end {
                continue;
            }
            let arg = TAU * freq * age + phase;
            out.push(SimParticle {
                layer: 0,
                index,
                position: [rx + ux * rw + self.wobble_amplitude * arg.sin(), ry + rh - speed * age],
                velocity: [self.wobble_amplitude * TAU * freq * arg.cos(), -speed],
                age,
                life: end,
                size: size + self.growth * age,
                length: 0.0,
                depth: 1.0,
                opacity: self.opacity,
                colour: self.shading.colour,
            });
        }
        Ok(out)
    }

    /// Renders the bubbles at composition `tick` over a `width x height` raster (composition pixels).
    pub fn render(&self, width: u32, height: u32, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<ParticleFrame> {
        self.validate()?;
        check_raster(width, height, limits)?;
        let parts = self.states(tick, limits, cancel)?;
        render_bubble_marks(&parts, &self.shading, width, height, limits, cancel)
    }
}
