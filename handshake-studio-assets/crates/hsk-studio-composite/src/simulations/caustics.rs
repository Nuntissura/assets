//! Caustics (MT-34182, STU-CMP-075 "Refracted light through a simulated water surface").
//!
//! **Inputs.** A water surface ([`CausticSurface`]: a [`WaveWorld`] simulated at the frame tick, or
//! any caller [`HeightField`]) and a bottom: a premultiplied `f32` image ([`WaterBottom`], at most
//! [`MAX_SOURCE_PIXELS`] pixels, checked before its pixels are read) or, without one, the record's
//! uniform opaque bottom colour. Both span the output raster. The record ([`CAUSTICS_ROWS`]) holds
//! the light direction (degrees, 0 = up, clockwise) and elevation, the refractive index of the water
//! (1.33 for water, `studio_declared`), the bottom depth below the rest surface in pixels, the surface
//! height scale (pixels per height unit), the caustic strength and the surface tint and opacity.
//!
//! **Optics (a declared Studio reading; After Effects parity UNVERIFIED, D5).** Heights are sampled
//! at every pixel centre ([`HeightField::sample`]) and scaled to pixels; the surface normal comes
//! from central differences (one-sided at the raster edge). Rays refract by Snell's law in vector
//! form (`eta = 1 / refractive_index`) and travel to the bottom plane `depth` below the rest
//! surface (a surface below the bottom is dry: no travel).
//! - Refraction: the view ray (straight down) refracts at each pixel; the bottom and the caustic
//!   light are sampled bilinearly (clamped to the edge) where it lands.
//! - Caustic light: the light ray refracts at each pixel centre and lands on the bottom; its landing
//!   point, minus the landing offset of a flat surface (so a flat surface lights every pixel exactly
//!   once), is splatted bilinearly with weight 1 into a light map in row-major order (a fixed,
//!   single-threaded accumulation order: deterministic). A flat surface gives a light map of exactly
//!   1; a curved one focuses (above 1) and spreads (below 1) the same light; rays that leave the
//!   raster are lost, never created.
//! - Shading: the bottom (premultiplied) times `1 + strength (light - 1)` (clamped to `0..=alpha`),
//!   then the surface tint over it at `surface_opacity`.
//!
//! The frame is a pure function of the surface at the tick, the bottom and the record: with a Wave
//! world surface it inherits STU-CMP-075a from Wave world (re-simulated from its start tick), and with
//! a caller field it is the same at every tick.
#![deny(clippy::arithmetic_side_effects)]
use super::wave_world::{HeightField, WaveWorld};
use super::{ParamRow, ParamScope, Result, check_raster, checked, row, validate_rows};
use crate::particles::{MAX_SOURCE_PIXELS, ParticleError, ParticleFrame, ParticleLimits, over};
use hsk_studio_accord::CancellationToken;
use std::borrow::Cow;

/// Pixel passes per frame counted against `max_raster_work` (heights, light, view).
pub const CAUSTIC_PASSES: u64 = 3;

/// The record rows (STU-FX-146: authored, `studio_declared`).
pub const CAUSTICS_ROWS: [ParamRow; 13] = [
    row("light_direction", "deg", (-1.0e6, 1.0e6), (Some(-180.0), Some(180.0)), 315.0),
    row("light_elevation", "deg", (1.0, 90.0), (Some(10.0), Some(90.0)), 60.0),
    row("refractive_index", "ratio", (1.0, 3.0), (Some(1.0), Some(2.0)), 1.33),
    row("depth", "px", (0.0, 1.0e4), (Some(0.0), Some(200.0)), 32.0),
    row("surface_height", "px/height", (0.0, 1.0e4), (Some(0.0), Some(64.0)), 8.0),
    row("caustic_strength", "ratio", (0.0, 100.0), (Some(0.0), Some(4.0)), 1.0),
    row("surface_opacity", "ratio", (0.0, 1.0), (None, None), 0.1),
    row("surface_colour_r", "ratio", (0.0, 1.0), (None, None), 0.75),
    row("surface_colour_g", "ratio", (0.0, 1.0), (None, None), 0.875),
    row("surface_colour_b", "ratio", (0.0, 1.0), (None, None), 1.0),
    row("bottom_colour_r", "ratio", (0.0, 1.0), (None, None), 0.375),
    row("bottom_colour_g", "ratio", (0.0, 1.0), (None, None), 0.5625),
    row("bottom_colour_b", "ratio", (0.0, 1.0), (None, None), 0.625),
];

fn default_of(key: &str) -> f64 {
    CAUSTICS_ROWS.iter().find(|r| r.key == key).map_or(0.0, |r| r.default)
}

/// The water surface Caustics refracts through.
#[derive(Clone, Debug, PartialEq)]
pub enum CausticSurface {
    /// Simulated at the frame tick.
    Wave(WaveWorld),
    /// A caller displacement map (the same at every tick).
    Field(HeightField),
}

/// The image under the water: premultiplied RGBA, row-major, spanning the output raster.
#[derive(Clone, Debug, PartialEq)]
pub struct WaterBottom {
    pub width: u32,
    pub height: u32,
    pub premultiplied: Vec<[f32; 4]>,
}

impl WaterBottom {
    /// Non-empty, at most [`MAX_SOURCE_PIXELS`] pixels (checked before the pixels are read),
    /// `width * height` pixels, every channel in `0..=1` and colour at most alpha.
    pub fn validate(&self) -> Result<()> {
        let n = u64::from(self.width).checked_mul(u64::from(self.height)).ok_or(ParticleError::Parameter("bottom"))?;
        if n == 0 || n > MAX_SOURCE_PIXELS || u64::try_from(self.premultiplied.len()).ok() != Some(n) {
            return Err(ParticleError::Parameter("bottom"));
        }
        let bad = |p: &[f32; 4]| p.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)) || p[..3].iter().any(|c| *c > p[3]);
        if self.premultiplied.iter().any(bad) {
            return Err(ParticleError::Parameter("bottom_pixels"));
        }
        Ok(())
    }

    /// Bilinear over pixel centres at bottom pixel coordinates, clamped to the edge.
    fn sample(&self, x: f64, y: f64) -> [f64; 4] {
        let (w, h) = (self.width as usize, self.height as usize);
        let fx = (x - 0.5).clamp(0.0, f64::from(self.width) - 1.0);
        let fy = (y - 0.5).clamp(0.0, f64::from(self.height) - 1.0);
        let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
        let (x1, y1) = (x0.saturating_add(1).min(w.saturating_sub(1)), y0.saturating_add(1).min(h.saturating_sub(1)));
        let (tx, ty) = (fx - fx.floor(), fy - fy.floor());
        let get = |x: usize, y: usize| self.premultiplied[y.wrapping_mul(w).wrapping_add(x)];
        std::array::from_fn(|i| {
            let (a, b, c, d) = (f64::from(get(x0, y0)[i]), f64::from(get(x1, y0)[i]), f64::from(get(x0, y1)[i]), f64::from(get(x1, y1)[i]));
            let top = a + (b - a) * tx;
            let bot = c + (d - c) * tx;
            top + (bot - top) * ty
        })
    }
}

/// One Caustics layer (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct Caustics {
    pub surface: CausticSurface,
    /// `None`: the uniform opaque `bottom_colour`.
    pub bottom: Option<WaterBottom>,
    /// Degrees, 0 = up, clockwise (where the light comes from).
    pub light_direction: f64,
    /// Degrees above the surface, `1..=90`.
    pub light_elevation: f64,
    pub refractive_index: f64,
    /// Pixels from the rest surface down to the bottom.
    pub depth: f64,
    /// Pixels per height unit.
    pub surface_height: f64,
    pub caustic_strength: f64,
    pub surface_opacity: f64,
    /// Straight RGB.
    pub surface_colour: [f32; 3],
    /// Straight RGB of the uniform bottom.
    pub bottom_colour: [f32; 3],
}

impl Default for Caustics {
    fn default() -> Self {
        let d = default_of;
        Self {
            surface: CausticSurface::Wave(WaveWorld::default()),
            bottom: None,
            light_direction: d("light_direction"),
            light_elevation: d("light_elevation"),
            refractive_index: d("refractive_index"),
            depth: d("depth"),
            surface_height: d("surface_height"),
            caustic_strength: d("caustic_strength"),
            surface_opacity: d("surface_opacity"),
            surface_colour: [d("surface_colour_r") as f32, d("surface_colour_g") as f32, d("surface_colour_b") as f32],
            bottom_colour: [d("bottom_colour_r") as f32, d("bottom_colour_g") as f32, d("bottom_colour_b") as f32],
        }
    }
}

/// `I` refracted through unit normal `n` (`eta` = n1 / n2); `None` when `I` leaves the surface.
fn refract(i: [f64; 3], n: [f64; 3], eta: f64) -> Option<[f64; 3]> {
    let cosi = -(n[0] * i[0] + n[1] * i[1] + n[2] * i[2]);
    if cosi <= 0.0 {
        return None;
    }
    let k = 1.0 - eta * eta * (1.0 - cosi * cosi);
    if k < 0.0 {
        return None;
    }
    let m = eta * cosi - k.sqrt();
    let t = [eta * i[0] + m * n[0], eta * i[1] + m * n[1], eta * i[2] + m * n[2]];
    (t[2] < 0.0).then_some(t)
}

/// Horizontal travel of refracted ray `t` down `drop` pixels.
fn travel(t: [f64; 3], drop: f64) -> [f64; 2] {
    let s = drop.max(0.0) / -t[2];
    [t[0] * s, t[1] * s]
}

/// Bilinear sample of a row-major plane at raster coordinates, clamped to the edge.
fn sample_plane(plane: &[f64], w: u32, h: u32, x: f64, y: f64) -> f64 {
    let (wu, hu) = (w as usize, h as usize);
    let fx = (x - 0.5).clamp(0.0, f64::from(w) - 1.0);
    let fy = (y - 0.5).clamp(0.0, f64::from(h) - 1.0);
    let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
    let (x1, y1) = (x0.saturating_add(1).min(wu.saturating_sub(1)), y0.saturating_add(1).min(hu.saturating_sub(1)));
    let (tx, ty) = (fx - fx.floor(), fy - fy.floor());
    let get = |x: usize, y: usize| plane[y.wrapping_mul(wu).wrapping_add(x)];
    let top = get(x0, y0) + (get(x1, y0) - get(x0, y0)) * tx;
    let bot = get(x0, y1) + (get(x1, y1) - get(x0, y1)) * tx;
    top + (bot - top) * ty
}

impl Caustics {
    pub fn param(&self, key: &str) -> Option<f64> {
        Some(match key {
            "light_direction" => self.light_direction,
            "light_elevation" => self.light_elevation,
            "refractive_index" => self.refractive_index,
            "depth" => self.depth,
            "surface_height" => self.surface_height,
            "caustic_strength" => self.caustic_strength,
            "surface_opacity" => self.surface_opacity,
            "surface_colour_r" => f64::from(self.surface_colour[0]),
            "surface_colour_g" => f64::from(self.surface_colour[1]),
            "surface_colour_b" => f64::from(self.surface_colour[2]),
            "bottom_colour_r" => f64::from(self.bottom_colour[0]),
            "bottom_colour_g" => f64::from(self.bottom_colour[1]),
            "bottom_colour_b" => f64::from(self.bottom_colour[2]),
            _ => return None,
        })
    }

    /// Sets a record scalar (refused outside its row; nothing changes).
    pub fn set_param(&mut self, key: &str, value: f64) -> Result<()> {
        match checked(&CAUSTICS_ROWS, ParamScope::Record, key, value)? {
            "light_direction" => self.light_direction = value,
            "light_elevation" => self.light_elevation = value,
            "refractive_index" => self.refractive_index = value,
            "depth" => self.depth = value,
            "surface_height" => self.surface_height = value,
            "caustic_strength" => self.caustic_strength = value,
            "surface_opacity" => self.surface_opacity = value,
            "surface_colour_r" => self.surface_colour[0] = value as f32,
            "surface_colour_g" => self.surface_colour[1] = value as f32,
            "surface_colour_b" => self.surface_colour[2] = value as f32,
            "bottom_colour_r" => self.bottom_colour[0] = value as f32,
            "bottom_colour_g" => self.bottom_colour[1] = value as f32,
            _ => self.bottom_colour[2] = value as f32,
        }
        Ok(())
    }

    /// The record rows, the surface and the bottom.
    pub fn validate(&self) -> Result<()> {
        validate_rows(&CAUSTICS_ROWS, ParamScope::Record, |k| self.param(k))?;
        match &self.surface {
            CausticSurface::Wave(w) => w.validate()?,
            CausticSurface::Field(f) => f.validate()?,
        }
        if let Some(b) = &self.bottom {
            b.validate()?;
        }
        Ok(())
    }

    /// The unit vector from the surface toward the light (`+z` up out of the water).
    fn toward_light(&self) -> [f64; 3] {
        let (az, el) = (self.light_direction.to_radians(), self.light_elevation.to_radians());
        [el.cos() * az.sin(), -el.cos() * az.cos(), el.sin()]
    }

    /// The validated surface heights in pixels at every pixel centre and the caustic light map of a
    /// `width x height` raster (module docs); size and work checked before allocation.
    fn passes(&self, width: u32, height: u32, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<(Surface, Vec<f64>)> {
        self.validate()?;
        let pixels = check_raster(width, height, limits)?;
        if (pixels as u64).checked_mul(CAUSTIC_PASSES).is_none_or(|w| w > limits.max_raster_work) {
            return Err(ParticleError::Work);
        }
        let field = match &self.surface {
            CausticSurface::Wave(w) => Cow::Owned(w.height_field_at(tick, limits, cancel)?),
            CausticSurface::Field(f) => Cow::Borrowed(f),
        };
        let mut hp = Vec::with_capacity(pixels);
        for y in 0..height {
            let v = (f64::from(y) + 0.5) / f64::from(height);
            for x in 0..width {
                hp.push(field.sample((f64::from(x) + 0.5) / f64::from(width), v) * self.surface_height);
            }
        }
        cancel.check().map_err(|_| ParticleError::Canceled)?;
        let s = Surface { hp, w: width as usize, h: height as usize };
        let eta = 1.0 / self.refractive_index;
        let l = self.toward_light();
        let incident = [-l[0], -l[1], -l[2]];
        let flat = refract(incident, [0.0, 0.0, 1.0], eta).map_or([0.0; 2], |t| travel(t, self.depth));
        // Forward splat in row-major order.
        let mut light = vec![0.0f64; pixels];
        for y in 0..s.h {
            for x in 0..s.w {
                let Some(t) = refract(incident, s.normal(x, y), eta) else { continue };
                let off = travel(t, self.depth + s.hp[s.at(x, y)]);
                let px = x as f64 + (off[0] - flat[0]);
                let py = y as f64 + (off[1] - flat[1]);
                let (fx, fy) = (px.floor(), py.floor());
                let (tx, ty) = (px - fx, py - fy);
                for (dx, dy, wgt) in [(0.0, 0.0, (1.0 - tx) * (1.0 - ty)), (1.0, 0.0, tx * (1.0 - ty)), (0.0, 1.0, (1.0 - tx) * ty), (1.0, 1.0, tx * ty)] {
                    let (cx, cy) = (fx + dx, fy + dy);
                    if wgt > 0.0 && cx >= 0.0 && cy >= 0.0 && cx < f64::from(width) && cy < f64::from(height) {
                        light[s.at(cx as usize, cy as usize)] += wgt;
                    }
                }
            }
            if y & 0x3F == 0 {
                cancel.check().map_err(|_| ParticleError::Canceled)?;
            }
        }
        Ok((s, light))
    }

    /// The caustic light map at `tick`: row-major, 1 where a flat surface would light the bottom,
    /// above 1 where the surface focuses light, below where it spreads it; its sum never exceeds
    /// the pixel count (rays leaving the raster are lost)
    /// (`tests/simulations_water.rs::caustics_focus_conserve_and_consume_the_wave_world`).
    pub fn light_map(&self, width: u32, height: u32, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<Vec<f64>> {
        Ok(self.passes(width, height, tick, limits, cancel)?.1)
    }

    /// Row-major horizontal offsets (pixels) from each pixel centre to where its straight-down view
    /// ray lands on the bottom, at `tick` (the refraction the render samples through).
    pub fn view_offsets(&self, width: u32, height: u32, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<Vec<[f64; 2]>> {
        let (s, light) = self.passes(width, height, tick, limits, cancel)?;
        let mut out = Vec::with_capacity(light.len());
        for y in 0..s.h {
            for x in 0..s.w {
                out.push(self.view(&s, x, y));
            }
        }
        Ok(out)
    }

    fn view(&self, s: &Surface, x: usize, y: usize) -> [f64; 2] {
        refract([0.0, 0.0, -1.0], s.normal(x, y), 1.0 / self.refractive_index).map_or([0.0; 2], |t| travel(t, self.depth + s.hp[s.at(x, y)]))
    }

    /// Renders the caustics at composition `tick` over a `width x height` raster.
    pub fn render(&self, width: u32, height: u32, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<ParticleFrame> {
        let (s, light) = self.passes(width, height, tick, limits, cancel)?;
        let tint = [
            self.surface_colour[0] * self.surface_opacity as f32,
            self.surface_colour[1] * self.surface_opacity as f32,
            self.surface_colour[2] * self.surface_opacity as f32,
            self.surface_opacity as f32,
        ];
        let flat_bottom = [f64::from(self.bottom_colour[0]), f64::from(self.bottom_colour[1]), f64::from(self.bottom_colour[2]), 1.0];
        let mut plane = Vec::with_capacity(light.len());
        for y in 0..s.h {
            for x in 0..s.w {
                let off = self.view(&s, x, y);
                let qx = x as f64 + 0.5 + off[0];
                let qy = y as f64 + 0.5 + off[1];
                let b = match &self.bottom {
                    Some(img) => img.sample(qx * f64::from(img.width) / f64::from(width), qy * f64::from(img.height) / f64::from(height)),
                    None => flat_bottom,
                };
                let gain = (1.0 + self.caustic_strength * (sample_plane(&light, width, height, qx, qy) - 1.0)).max(0.0);
                let a = b[3];
                let mut px = [(b[0] * gain).min(a) as f32, (b[1] * gain).min(a) as f32, (b[2] * gain).min(a) as f32, a as f32];
                over(&mut px, tint);
                plane.push(px);
            }
            if y & 0x3F == 0 {
                cancel.check().map_err(|_| ParticleError::Canceled)?;
            }
        }
        Ok(ParticleFrame { width, height, premultiplied: plane, drawn: 0 })
    }
}

/// Surface heights in pixels over the raster.
struct Surface {
    hp: Vec<f64>,
    w: usize,
    h: usize,
}

impl Surface {
    fn at(&self, x: usize, y: usize) -> usize {
        y.wrapping_mul(self.w).wrapping_add(x)
    }

    /// Unit normal from central differences (one-sided at the raster edge).
    fn normal(&self, x: usize, y: usize) -> [f64; 3] {
        let (xa, xb) = (x.saturating_sub(1), x.saturating_add(1).min(self.w.saturating_sub(1)));
        let (ya, yb) = (y.saturating_sub(1), y.saturating_add(1).min(self.h.saturating_sub(1)));
        let dx = if xb > xa { (self.hp[self.at(xb, y)] - self.hp[self.at(xa, y)]) / xb.wrapping_sub(xa) as f64 } else { 0.0 };
        let dy = if yb > ya { (self.hp[self.at(x, yb)] - self.hp[self.at(x, ya)]) / yb.wrapping_sub(ya) as f64 } else { 0.0 };
        let len = (dx * dx + dy * dy + 1.0).sqrt();
        [-dx / len, -dy / len, 1.0 / len]
    }
}
