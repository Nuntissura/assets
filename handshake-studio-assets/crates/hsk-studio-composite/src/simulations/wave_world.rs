//! Wave world (MT-34181, STU-CMP-075 "Wave propagation producing a displacement map for other layers
//! to consume").
//!
//! **Record ([`WaveWorld`]).** Seed, start tick, preroll, a `grid_width x grid_height` cell grid that
//! spans the layer, a fixed step grid (`step_rate` steps per second, each split into `substeps`),
//! wave speed (cells per second), damping (per second), an [`EdgeMode`], an optional seeded initial
//! roughness, the declared height range of the output and up to [`MAX_WAVE_PRODUCERS`]
//! [`WaveProducer`]s (a point or a line in layer-relative coordinates, `0..=1` across the layer,
//! with amplitude, frequency and phase). The authored rows are [`WAVE_WORLD_ROWS`] and
//! [`WAVE_PRODUCER_ROWS`], the scalars driven by the recovered record [`WAVE_WORLD_MAPPINGS`] and
//! [`WAVE_PRODUCER_MAPPINGS`]; all of them are the enforced validation (module docs of [`super`]).
//!
//! **Recovered record ([`WAVE_WORLD_RECORD`], STU-FX-131a `APC Wave World`, 48 rows, MT-32013; rules
//! in the [`super`] module docs).** Engine scalars that read a recovered row, no unit conversion:
//! `preroll` <- row 24 `Pre-roll (seconds)` (hard `0..=60`, default 0, as authored before);
//! producer `x`, `y` <- row 44 `Position` (`POINT`, layer fractions, no declared bound, default
//! `(0.5, 0.5)`; authored hard `-1e3..=1e3` before); `amplitude` <- row 52 `Amplitude` (hard
//! `0..=5`, soft `0..=1`, default 0.5; the engine's height unit is the record's own unitless scale;
//! authored hard `0..=1e6`, soft `0..=2` before); `frequency` <- row 54 `Frequency` (hard `0..=20`,
//! soft `0..=3`, default 1, read as Hz: the record's time base is seconds by its row 24; authored hard
//! `0..=1000`, soft `0..=5` before); `phase` <- row 56 `Phase` (`ANGLE`, degrees, no-range reading:
//! no hard bound, default 0; authored hard `-1e6..=1e6`, soft `-180..=180` before). Every producer
//! of the record reads the first set of producer rows (44, 52, 54, 56: the set whose amplitude
//! default 0.5 is the engine's); the second set (60, 62-72) and the `Type` popups (42, 58: index 1
//! `Ring` = [`ProducerShape::Point`], 2 `Line` = [`ProducerShape::Line`], default 1 = the engine's
//! default shape) are stored and do not drive the engine. Not mapped, authored rows kept (STU-FX-146
//! scope beside the record): `grid_width` / `grid_height` (two per-axis cell counts; the record's one
//! `Grid Resolution` does not say which axis it counts), `wave_speed` (cells per second; `Wave Speed`
//! declares no unit and no cell scale), `damping` (a per-second coefficient; `Damping` `0..=1` with no
//! unit is not shown to be the same quantity), `step_rate`, `substeps`, `initial_roughness`,
//! `height_range`, and producer `x2` / `y2` (the line end; the record describes a line by
//! `Height/Length`, `Width` and `Angle`). Every other recovered row (view, rotations, scale,
//! brightness, contrast, gamma, dry areas, transparency, reflect edges, ground, groups) is stored and
//! validated and does not drive the simulation. After Effects parity UNVERIFIED (D5).
//!
//! **Scheme (a declared Studio reading; After Effects parity UNVERIFIED, D5).** The surface height
//! `h` obeys the damped wave equation `h_tt + k h_t = c^2 lap(h)`, discretised with the explicit
//! leapfrog scheme on the cell grid (`dx = 1` cell, 5-point Laplacian):
//! `h' = (2 h - (1 - g) h_prev + r^2 lap(h)) / (1 + g)`, `r = c dt` (the Courant number),
//! `g = k dt / 2`, `dt = 1 / (step_rate substeps)` seconds. The scheme is stable for
//! `r <= 1 / sqrt 2` ([`CFL_LIMIT`]); a record above it is refused as `Parameter("cfl")`.
//! [`EdgeMode::Reflect`] mirrors the edge (zero normal slope), [`EdgeMode::Absorb`] applies the
//! first-order Mur boundary `h'_e = h_i + (r - 1) / (r + 1) (h'_i - h_e)` on the outer rows and
//! columns (columns first, then rows, so rows own the corners). A producer is a hard source: after
//! every step its cells are set to `A sin(2 pi f t + phase)` with `t = n / (step_rate substeps)`
//! seconds since the simulation origin (`n` the step index); producers apply in order, a later one
//! winning a shared cell. A point covers the cell containing it; a line covers every cell its
//! half-cell samples touch, clipped to the grid. The initial surface is flat (or seeded white noise
//! of `initial_roughness` height, STU-FX-011) and at rest.
//!
//! **Statefulness (STU-CMP-075a).** The simulation origin is `preroll` seconds before the start tick;
//! the surface at `tick` is the state after [`super::fixed_steps`] steps (0 before the start tick:
//! the initial surface). Every frame is re-simulated from the origin, single-threaded, in row-major
//! order; [`WaveWorld::resume`] continues a [`WaveState`] checkpoint of the SAME record (refused
//! otherwise) and re-simulates from the origin when the checkpoint is later than the tick, so a
//! cached state and a re-simulation give the same field
//! (`tests/simulations_water.rs::wave_world_scrub_and_checkpoint_equal_forward_play`). Steps and
//! steps x cells are checked against [`ParticleLimits`] before any allocation.
//!
//! **Output.** A [`HeightField`] (`f32` heights in record units, declared range `[-range, range]`)
//! that any consumer samples ([`HeightField::sample`], Caustics), and [`HeightField::to_frame`]: an
//! opaque greyscale [`ParticleFrame`] with `0.5` at rest, `0` at `-range`, `1` at `+range`, drawn
//! through the existing `StaticContent` route.
#![deny(clippy::arithmetic_side_effects)]
use super::{
    ParamRow, ParamScope, RecoveredMapping, Result, check_raster, checked, checked_mapped, fixed_steps, mapped_default, rec_angle,
    rec_checkbox, rec_group, rec_layer, rec_point, rec_popup, rec_slider, row, validate_mapped, validate_rows, whole,
};
use crate::particles::{Draws, MAX_SOURCE_PIXELS, ParticleError, ParticleFrame, ParticleLimits, splitmix64};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::StudioEffectParamSpec;
use std::f64::consts::{FRAC_1_SQRT_2, TAU};

/// Most producers of one record.
pub const MAX_WAVE_PRODUCERS: usize = 16;
/// The largest stable Courant number `c dt` of the 2D leapfrog scheme.
pub const CFL_LIMIT: f64 = FRAC_1_SQRT_2;

const ROUGHNESS_SALT: u64 = 0x7761_7665;

/// The authored record rows: engine scalars with no counterpart in [`WAVE_WORLD_RECORD`]
/// (STU-FX-146 scope beside it, `studio_declared`; module docs).
pub const WAVE_WORLD_ROWS: [ParamRow; 8] = [
    row("grid_width", "cells", (2.0, 1024.0), (Some(16.0), Some(256.0)), 64.0),
    row("grid_height", "cells", (2.0, 1024.0), (Some(16.0), Some(256.0)), 64.0),
    row("step_rate", "steps/s", (1.0, 1000.0), (Some(24.0), Some(240.0)), 60.0),
    row("substeps", "count", (1.0, 16.0), (None, None), 2.0),
    row("wave_speed", "cells/s", (0.0, 1.0e4), (Some(1.0), Some(60.0)), 20.0),
    row("damping", "1/s", (0.0, 1000.0), (Some(0.0), Some(5.0)), 0.5),
    row("initial_roughness", "height", (0.0, 100.0), (Some(0.0), Some(1.0)), 0.0),
    row("height_range", "height", (1.0e-6, 1.0e6), (Some(0.1), Some(10.0)), 1.0),
];

/// The authored per-producer rows (the line end, layer-relative, `0..=1` across the layer; no
/// recovered counterpart, module docs).
pub const WAVE_PRODUCER_ROWS: [ParamRow; 2] = [
    row("x2", "ratio", (-1.0e3, 1.0e3), (Some(0.0), Some(1.0)), 0.5),
    row("y2", "ratio", (-1.0e3, 1.0e3), (Some(0.0), Some(1.0)), 0.5),
];

/// The record scalars driven by a row of [`WAVE_WORLD_RECORD`] (module docs).
pub const WAVE_WORLD_MAPPINGS: [RecoveredMapping; 1] = [RecoveredMapping { key: "preroll", param_index: 24, component: 0, unit: "s" }];

/// The per-producer scalars driven by a row of [`WAVE_WORLD_RECORD`] (module docs).
pub const WAVE_PRODUCER_MAPPINGS: [RecoveredMapping; 5] = [
    RecoveredMapping { key: "x", param_index: 44, component: 0, unit: "ratio" },
    RecoveredMapping { key: "y", param_index: 44, component: 1, unit: "ratio" },
    RecoveredMapping { key: "amplitude", param_index: 52, component: 0, unit: "height" },
    RecoveredMapping { key: "frequency", param_index: 54, component: 0, unit: "Hz" },
    RecoveredMapping { key: "phase", param_index: 56, component: 0, unit: "deg" },
];

/// The recovered STU-FX-131a record of Wave world (`APC Wave World`, spec v02.225 module 14), every
/// row in source order (`tests/simulations_records.rs`).
pub const WAVE_WORLD_RECORD: [StudioEffectParamSpec; 48] = [
    rec_layer("unnamed_0", 0, "(unnamed)"),
    rec_popup("view", 2, "View", &[2.0], WAVE_WORLD_VIEW_2_OPTIONS),
    rec_angle("horizontal_rotation", 4, "Horizontal Rotation"),
    rec_group("wireframe_controls", 5, "Wireframe Controls"),
    rec_angle("vertical_rotation", 6, "Vertical Rotation"),
    rec_slider("vertical_scale", 8, "Vertical Scale", (0.0, 1.0), (0.0, 1.0), &[0.5], Some(3)),
    rec_slider("brightness", 10, "Brightness", (-5.0, 5.0), (0.0, 1.0), &[0.5], Some(3)),
    rec_slider("contrast", 12, "Contrast", (0.0, 2.0), (0.0, 1.0), &[0.25], Some(3)),
    rec_group("unnamed_13", 13, "(unnamed)"),
    rec_slider("gamma_adjustment", 14, "Gamma Adjustment", (0.299988, 3.0), (0.299988, 3.0), &[1.0], Some(3)),
    rec_group("height_map_controls", 15, "Height Map Controls"),
    rec_popup("render_dry_areas_as", 16, "Render Dry Areas As", &[1.0], WAVE_WORLD_RENDER_DRY_AREAS_AS_16_OPTIONS),
    rec_slider("transparency", 18, "Transparency", (0.0, 1.0), (0.0, 0.099991), &[0.009995], Some(3)),
    rec_popup("reflect_edges", 20, "Reflect Edges", &[1.0], WAVE_WORLD_REFLECT_EDGES_20_OPTIONS),
    rec_slider("pre_roll_seconds", 24, "Pre-roll (seconds)", (0.0, 60.0), (0.0, 5.0), &[0.0], Some(3)),
    rec_slider("grid_resolution", 26, "Grid Resolution", (10.0, 1000.0), (20.0, 300.0), &[40.0], None),
    rec_group("unnamed_27", 27, "(unnamed)"),
    rec_group("simulation", 29, "Simulation"),
    rec_slider("wave_speed", 30, "Wave Speed", (0.009995, 10.0), (0.009995, 1.0), &[0.5], Some(3)),
    rec_slider("damping", 32, "Damping", (0.0, 1.0), (0.0, 1.0), &[0.009995], Some(3)),
    rec_layer("ground_36", 36, "Ground"),
    rec_slider("steepness", 38, "Steepness", (0.0, 1.0), (0.0, 1.0), &[0.25], Some(3)),
    rec_slider("height", 40, "Height", (0.0, 1.0), (0.0, 1.0), &[0.299988], Some(3)),
    rec_popup("type_42", 42, "Type", &[1.0], WAVE_WORLD_TYPE_42_OPTIONS),
    rec_group("unnamed_43", 43, "(unnamed)"),
    rec_point("position_44", 44, "Position", &[0.5, 0.5]),
    rec_group("ground_45", 45, "Ground"),
    rec_slider("height_length_46", 46, "Height/Length", (0.000992, 100.0), (0.000992, 1.0), &[0.099991], Some(3)),
    rec_slider("width_48", 48, "Width", (0.000992, 100.0), (0.000992, 1.0), &[0.099991], Some(3)),
    rec_angle("angle_50", 50, "Angle"),
    rec_slider("amplitude_52", 52, "Amplitude", (0.0, 5.0), (0.0, 1.0), &[0.5], Some(3)),
    rec_slider("frequency_54", 54, "Frequency", (0.0, 20.0), (0.0, 3.0), &[1.0], Some(3)),
    rec_group("unnamed_55", 55, "(unnamed)"),
    rec_angle("phase_56", 56, "Phase"),
    rec_group("producer_1", 57, "Producer 1"),
    rec_popup("type_58", 58, "Type", &[1.0], WAVE_WORLD_TYPE_58_OPTIONS),
    rec_point("position_60", 60, "Position", &[0.5, 0.5]),
    rec_slider("height_length_62", 62, "Height/Length", (0.000992, 100.0), (0.000992, 1.0), &[0.5], Some(3)),
    rec_slider("width_64", 64, "Width", (0.000992, 100.0), (0.000992, 1.0), &[0.5], Some(3)),
    rec_angle("angle_66", 66, "Angle"),
    rec_slider("amplitude_68", 68, "Amplitude", (0.0, 5.0), (0.0, 1.0), &[0.0], Some(3)),
    rec_slider("frequency_70", 70, "Frequency", (0.0, 20.0), (0.0, 3.0), &[1.0], Some(3)),
    rec_angle("phase_72", 72, "Phase"),
    rec_group("unnamed_75", 75, "(unnamed)"),
    rec_group("producer_2", 77, "Producer 2"),
    rec_slider("wave_strength", 90, "Wave Strength", (0.0, 1.0), (0.0, 1.0), &[0.0], Some(3)),
    rec_checkbox("unnamed_92", 92, "(unnamed)", &[1.0]),
    rec_group("unnamed_95", 95, "(unnamed)"),
];
/// `param_index` of every row whose flags carry `COLLAPSE_TWIRLY`.
pub const WAVE_WORLD_COLLAPSED: [u16; 26] = [4, 6, 8, 10, 12, 14, 18, 24, 26, 30, 32, 38, 40, 46, 48, 50, 52, 54, 56, 62, 64, 66, 68, 70, 72, 90];
const WAVE_WORLD_VIEW_2_OPTIONS: &[(u16, &str)] = &[(1, "Height Map"), (2, "Wireframe Preview")];
const WAVE_WORLD_RENDER_DRY_AREAS_AS_16_OPTIONS: &[(u16, &str)] = &[(1, "Solid"), (2, "Transparent")];
const WAVE_WORLD_REFLECT_EDGES_20_OPTIONS: &[(u16, &str)] = &[(1, "None"), (2, "All"), (3, "Left"), (4, "Top"), (5, "Right"), (6, "Bottom"), (7, "Left & Top"), (8, "Right & Left"), (9, "Right & Top"), (10, "Bottom & Left"), (11, "Bottom & Top"), (12, "Bottom & Right"), (13, "Right & Left & Top"), (14, "Bottom & Left & Top"), (15, "Bottom & Right & Left"), (16, "Bottom & Right & Top")];
const WAVE_WORLD_TYPE_42_OPTIONS: &[(u16, &str)] = &[(1, "Ring"), (2, "Line")];
const WAVE_WORLD_TYPE_58_OPTIONS: &[(u16, &str)] = &[(1, "Ring"), (2, "Line")];

const INTEGER_KEYS: [&str; 4] = ["grid_width", "grid_height", "step_rate", "substeps"];

fn default_of(rows: &[ParamRow], key: &str) -> f64 {
    rows.iter().find(|r| r.key == key).map_or(0.0, |r| r.default)
}

/// How the grid edges treat a wave (module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EdgeMode {
    /// Zero normal slope: waves reflect.
    #[default]
    Reflect,
    /// First-order Mur boundary: waves leave the grid.
    Absorb,
}

/// The shape of a producer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProducerShape {
    /// The cell containing `position` (a ring of waves).
    #[default]
    Point,
    /// The cells along `position -> end` (a line of waves).
    Line,
}

/// One wave producer (module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaveProducer {
    pub shape: ProducerShape,
    /// Layer-relative `[x, y]`.
    pub position: [f64; 2],
    /// Layer-relative line end (unused by a point).
    pub end: [f64; 2],
    /// Height units.
    pub amplitude: f64,
    /// Hz.
    pub frequency: f64,
    /// Degrees.
    pub phase: f64,
}

impl Default for WaveProducer {
    fn default() -> Self {
        let d = |k| default_of(&WAVE_PRODUCER_ROWS, k);
        let m = |k| mapped_default(&WAVE_WORLD_RECORD, &WAVE_PRODUCER_MAPPINGS, k);
        Self {
            shape: ProducerShape::default(),
            position: [m("x"), m("y")],
            end: [d("x2"), d("y2")],
            amplitude: m("amplitude"),
            frequency: m("frequency"),
            phase: m("phase"),
        }
    }
}

impl WaveProducer {
    pub fn param(&self, key: &str) -> Option<f64> {
        Some(match key {
            "x" => self.position[0],
            "y" => self.position[1],
            "x2" => self.end[0],
            "y2" => self.end[1],
            "amplitude" => self.amplitude,
            "frequency" => self.frequency,
            "phase" => self.phase,
            _ => return None,
        })
    }

    /// Sets a producer scalar (refused outside its row or recovered row; nothing changes).
    pub fn set_param(&mut self, key: &str, value: f64) -> Result<()> {
        let k = match checked_mapped(&WAVE_WORLD_RECORD, &WAVE_PRODUCER_MAPPINGS, key, value)? {
            Some(k) => k,
            None => checked(&WAVE_PRODUCER_ROWS, ParamScope::Record, key, value)?,
        };
        match k {
            "x" => self.position[0] = value,
            "y" => self.position[1] = value,
            "x2" => self.end[0] = value,
            "y2" => self.end[1] = value,
            "amplitude" => self.amplitude = value,
            "frequency" => self.frequency = value,
            _ => self.phase = value,
        }
        Ok(())
    }

    /// The producer's scalars against its authored rows and recovered rows.
    fn validate(&self) -> Result<()> {
        validate_rows(&WAVE_PRODUCER_ROWS, ParamScope::Record, |k| self.param(k))?;
        validate_mapped(&WAVE_WORLD_RECORD, &WAVE_PRODUCER_MAPPINGS, |k| self.param(k))
    }

    /// The grid cells this producer drives, ascending, without duplicates.
    fn cells(&self, gw: u32, gh: u32) -> Vec<usize> {
        let (w, h) = (f64::from(gw), f64::from(gh));
        let cell = |x: f64, y: f64| -> Option<usize> {
            if !(0.0..=w).contains(&x) || !(0.0..=h).contains(&y) {
                return None;
            }
            let cx = (x.floor() as u32).min(gw.saturating_sub(1));
            let cy = (y.floor() as u32).min(gh.saturating_sub(1));
            Some((cy as usize).wrapping_mul(gw as usize).wrapping_add(cx as usize))
        };
        let a = [self.position[0] * w, self.position[1] * h];
        let mut out = Vec::new();
        match self.shape {
            ProducerShape::Point => {
                if a[0] < w && a[1] < h {
                    out.extend(cell(a[0], a[1]));
                }
            }
            ProducerShape::Line => {
                let b = [self.end[0] * w, self.end[1] * h];
                let Some((t0, t1)) = clip_segment(a, b, w, h) else { return out };
                let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                let len = (dx * (t1 - t0)).hypot(dy * (t1 - t0));
                // Half-cell samples; the clipped length is at most the grid diagonal (< 1449 cells).
                let n = (2.0 * len).ceil().max(1.0) as u32;
                for i in 0..=n {
                    let t = t0 + (t1 - t0) * f64::from(i) / f64::from(n);
                    out.extend(cell(a[0] + dx * t, a[1] + dy * t));
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Liang-Barsky clip of `a -> b` to `[0, w] x [0, h]`: the parameter range kept.
fn clip_segment(a: [f64; 2], b: [f64; 2], w: f64, h: f64) -> Option<(f64, f64)> {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [(-dx, a[0]), (dx, w - a[0]), (-dy, a[1]), (dy, h - a[1])] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 { t0 = t0.max(r) } else { t1 = t1.min(r) }
        }
    }
    (t0 <= t1).then_some((t0, t1))
}

/// One Wave world layer (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct WaveWorld {
    /// STU-FX-011 explicit seed (the initial roughness).
    pub seed: u64,
    /// The deterministic start (STU-CMP-075a), composition ticks.
    pub start_tick: u64,
    /// Seconds already simulated at the start tick.
    pub preroll: f64,
    /// `[width, height]` in cells, `2..=1024` each.
    pub grid: [u32; 2],
    /// Steps per second, `1..=1000`.
    pub step_rate: u32,
    /// Substeps per step, `1..=16`.
    pub substeps: u32,
    /// Cells per second.
    pub wave_speed: f64,
    /// Per second.
    pub damping: f64,
    pub edge: EdgeMode,
    /// Height of the seeded initial white noise (0 = a flat start).
    pub initial_roughness: f64,
    /// The declared output range `[-range, range]`.
    pub height_range: f64,
    pub producers: Vec<WaveProducer>,
}

impl Default for WaveWorld {
    fn default() -> Self {
        let d = |k| default_of(&WAVE_WORLD_ROWS, k);
        Self {
            seed: 0,
            start_tick: 0,
            preroll: mapped_default(&WAVE_WORLD_RECORD, &WAVE_WORLD_MAPPINGS, "preroll"),
            grid: [d("grid_width") as u32, d("grid_height") as u32],
            step_rate: d("step_rate") as u32,
            substeps: d("substeps") as u32,
            wave_speed: d("wave_speed"),
            damping: d("damping"),
            edge: EdgeMode::default(),
            initial_roughness: d("initial_roughness"),
            height_range: d("height_range"),
            producers: vec![WaveProducer::default()],
        }
    }
}

impl WaveWorld {
    pub fn param(&self, key: &str) -> Option<f64> {
        Some(match key {
            "preroll" => self.preroll,
            "grid_width" => f64::from(self.grid[0]),
            "grid_height" => f64::from(self.grid[1]),
            "step_rate" => f64::from(self.step_rate),
            "substeps" => f64::from(self.substeps),
            "wave_speed" => self.wave_speed,
            "damping" => self.damping,
            "initial_roughness" => self.initial_roughness,
            "height_range" => self.height_range,
            _ => return None,
        })
    }

    /// Sets a record scalar (refused outside its row or recovered row, or fractional for a count;
    /// nothing changes).
    pub fn set_param(&mut self, key: &str, value: f64) -> Result<()> {
        let k = match checked_mapped(&WAVE_WORLD_RECORD, &WAVE_WORLD_MAPPINGS, key, value)? {
            Some(k) => k,
            None => checked(&WAVE_WORLD_ROWS, ParamScope::Record, key, value)?,
        };
        if INTEGER_KEYS.contains(&k) {
            whole(k, value)?;
        }
        match k {
            "preroll" => self.preroll = value,
            "grid_width" => self.grid[0] = value as u32,
            "grid_height" => self.grid[1] = value as u32,
            "step_rate" => self.step_rate = value as u32,
            "substeps" => self.substeps = value as u32,
            "wave_speed" => self.wave_speed = value,
            "damping" => self.damping = value,
            "initial_roughness" => self.initial_roughness = value,
            _ => self.height_range = value,
        }
        Ok(())
    }

    /// Sets a scalar of producer `index` (refused outside its row or for a missing producer).
    pub fn set_producer_param(&mut self, index: usize, key: &str, value: f64) -> Result<()> {
        self.producers.get_mut(index).ok_or(ParticleError::Parameter("producers"))?.set_param(key, value)
    }

    /// Seconds per step.
    fn dt(&self) -> f64 {
        1.0 / (f64::from(self.step_rate) * f64::from(self.substeps))
    }

    /// The Courant number `c dt`.
    pub fn courant(&self) -> f64 {
        self.wave_speed * self.dt()
    }

    pub fn validate(&self) -> Result<()> {
        validate_rows(&WAVE_WORLD_ROWS, ParamScope::Record, |k| self.param(k))?;
        validate_mapped(&WAVE_WORLD_RECORD, &WAVE_WORLD_MAPPINGS, |k| self.param(k))?;
        if self.producers.len() > MAX_WAVE_PRODUCERS {
            return Err(ParticleError::Parameter("producers"));
        }
        for p in &self.producers {
            p.validate()?;
        }
        if self.courant().is_nan() || self.courant() > CFL_LIMIT {
            return Err(ParticleError::Parameter("cfl"));
        }
        Ok(())
    }

    fn cells(&self) -> Result<u64> {
        u64::from(self.grid[0]).checked_mul(u64::from(self.grid[1])).ok_or(ParticleError::Parameter("grid_width"))
    }

    /// Steps to `tick`, with steps x cells checked against the budget (before allocation).
    fn steps_checked(&self, tick: u64, from: u64, limits: &ParticleLimits) -> Result<u64> {
        let steps = fixed_steps(self.start_tick, self.preroll, self.step_rate, self.substeps, tick, limits)?;
        let work = steps.saturating_sub(from).checked_mul(self.cells()?).ok_or(ParticleError::Work)?;
        if work > limits.max_steps {
            return Err(ParticleError::Work);
        }
        Ok(steps)
    }

    /// The surface state at composition `tick`, re-simulated from the origin. Validates the record.
    pub fn state_at(&self, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<WaveState> {
        self.validate()?;
        let steps = self.steps_checked(tick, 0, limits)?;
        let initial = self.initial_state()?;
        self.advance(initial, steps, cancel)
    }

    /// The surface at `tick` continued from `from`, a checkpoint of this same record (refused as
    /// `Parameter("checkpoint")` otherwise); a checkpoint later than `tick` re-simulates from the
    /// origin. The result equals [`Self::state_at`].
    pub fn resume(&self, from: &WaveState, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<WaveState> {
        self.validate()?;
        if from.record != *self {
            return Err(ParticleError::Parameter("checkpoint"));
        }
        let target = fixed_steps(self.start_tick, self.preroll, self.step_rate, self.substeps, tick, limits)?;
        if from.step > target {
            return self.state_at(tick, limits, cancel);
        }
        self.steps_checked(tick, from.step, limits)?;
        self.advance(from.clone(), target, cancel)
    }

    /// The height field at composition `tick` (module docs).
    pub fn height_field_at(&self, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<HeightField> {
        Ok(self.state_at(tick, limits, cancel)?.height_field())
    }

    /// Renders the height field at `tick` as a greyscale frame of `width x height` (0.5 = rest).
    pub fn render(&self, width: u32, height: u32, tick: u64, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<ParticleFrame> {
        self.validate()?;
        check_raster(width, height, limits)?;
        self.height_field_at(tick, limits, cancel)?.to_frame(width, height, limits, cancel)
    }

    fn initial_state(&self) -> Result<WaveState> {
        let n = usize::try_from(self.cells()?).map_err(|_| ParticleError::Parameter("grid_width"))?;
        let mut cur = vec![0.0f64; n];
        if self.initial_roughness > 0.0 {
            let key = self.seed ^ splitmix64(ROUGHNESS_SALT);
            for (i, h) in cur.iter_mut().enumerate() {
                *h = self.initial_roughness * (2.0 * Draws::new(key, i as u64).uniform() - 1.0);
            }
        }
        Ok(WaveState { record: self.clone(), step: 0, prev: cur.clone(), cur })
    }

    /// Steps `state` (of this record) to step `target`.
    fn advance(&self, mut state: WaveState, target: u64, cancel: &CancellationToken) -> Result<WaveState> {
        let (gw, gh) = (self.grid[0] as usize, self.grid[1] as usize);
        let sources: Vec<(Vec<usize>, f64, f64, f64)> = self
            .producers
            .iter()
            .map(|p| (p.cells(self.grid[0], self.grid[1]), p.amplitude, TAU * p.frequency, p.phase.to_radians()))
            .collect();
        let r = self.courant();
        let r2 = r * r;
        let g = self.damping * self.dt() / 2.0;
        let mur = (r - 1.0) / (r + 1.0);
        let per_second = f64::from(self.step_rate) * f64::from(self.substeps);
        let mut next = vec![0.0f64; state.cur.len()];
        let at = |x: usize, y: usize| y.wrapping_mul(gw).wrapping_add(x);
        let (xl, yl) = (gw.saturating_sub(1), gh.saturating_sub(1));
        while state.step < target {
            if state.step & 0x3F == 0 {
                cancel.check().map_err(|_| ParticleError::Canceled)?;
            }
            let (cur, prev) = (&state.cur, &state.prev);
            for y in 0..gh {
                let (ya, yb) = (y.saturating_sub(1), y.saturating_add(1).min(yl));
                for x in 0..gw {
                    let (xa, xb) = (x.saturating_sub(1), x.saturating_add(1).min(xl));
                    let c = cur[at(x, y)];
                    let lap = cur[at(xa, y)] + cur[at(xb, y)] + cur[at(x, ya)] + cur[at(x, yb)] - 4.0 * c;
                    next[at(x, y)] = (2.0 * c - (1.0 - g) * prev[at(x, y)] + r2 * lap) / (1.0 + g);
                }
            }
            if self.edge == EdgeMode::Absorb {
                for y in 0..gh {
                    next[at(0, y)] = cur[at(1, y)] + mur * (next[at(1, y)] - cur[at(0, y)]);
                    let xi = xl.saturating_sub(1);
                    next[at(xl, y)] = cur[at(xi, y)] + mur * (next[at(xi, y)] - cur[at(xl, y)]);
                }
                for x in 0..gw {
                    next[at(x, 0)] = cur[at(x, 1)] + mur * (next[at(x, 1)] - cur[at(x, 0)]);
                    let yi = yl.saturating_sub(1);
                    next[at(x, yl)] = cur[at(x, yi)] + mur * (next[at(x, yi)] - cur[at(x, yl)]);
                }
            }
            state.step = state.step.saturating_add(1);
            let t = state.step as f64 / per_second;
            for (cells, amplitude, omega, phase) in &sources {
                let v = amplitude * (omega * t + phase).sin();
                for &i in cells {
                    next[i] = v;
                }
            }
            std::mem::swap(&mut state.prev, &mut state.cur);
            std::mem::swap(&mut state.cur, &mut next);
        }
        if state.cur.iter().any(|v| !v.is_finite()) {
            return Err(ParticleError::NonFinite);
        }
        Ok(state)
    }
}

/// A Wave world surface after some steps: a checkpoint for [`WaveWorld::resume`].
#[derive(Clone, Debug, PartialEq)]
pub struct WaveState {
    record: WaveWorld,
    step: u64,
    prev: Vec<f64>,
    cur: Vec<f64>,
}

impl WaveState {
    /// Steps simulated since the origin.
    pub fn step(&self) -> u64 {
        self.step
    }

    /// The surface as a height field in the record's declared range.
    pub fn height_field(&self) -> HeightField {
        HeightField {
            width: self.record.grid[0],
            height: self.record.grid[1],
            heights: self.cur.iter().map(|v| *v as f32).collect(),
            range: self.record.height_range as f32,
        }
    }
}

/// A displacement map: row-major `f32` heights over a `width x height` cell grid spanning a layer,
/// with the declared range `[-range, range]` (module docs). Wave world produces one; any caller may
/// build one for Caustics.
#[derive(Clone, Debug, PartialEq)]
pub struct HeightField {
    pub width: u32,
    pub height: u32,
    pub heights: Vec<f32>,
    pub range: f32,
}

impl HeightField {
    /// Non-empty, at most [`MAX_SOURCE_PIXELS`] cells, `width * height` finite heights, a finite
    /// positive range; the size is checked before the heights are read.
    pub fn validate(&self) -> Result<()> {
        let n = u64::from(self.width).checked_mul(u64::from(self.height)).ok_or(ParticleError::Parameter("height_field"))?;
        if n == 0 || n > MAX_SOURCE_PIXELS || u64::try_from(self.heights.len()).ok() != Some(n) {
            return Err(ParticleError::Parameter("height_field"));
        }
        if !self.range.is_finite() || self.range <= 0.0 || self.heights.iter().any(|v| !v.is_finite()) {
            return Err(ParticleError::Parameter("height_field"));
        }
        Ok(())
    }

    /// The height of cell `(x, y)`.
    pub fn at(&self, x: u32, y: u32) -> Option<f32> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.heights.get((y as usize).checked_mul(self.width as usize)?.checked_add(x as usize)?).copied()
    }

    /// The height at layer-relative `(u, v)` (`0..=1` across the layer): bilinear over cell centres,
    /// clamped to the edge cells. The field must be valid.
    pub fn sample(&self, u: f64, v: f64) -> f64 {
        let (w, h) = (self.width as usize, self.height as usize);
        let fx = (u * f64::from(self.width) - 0.5).clamp(0.0, f64::from(self.width) - 1.0);
        let fy = (v * f64::from(self.height) - 0.5).clamp(0.0, f64::from(self.height) - 1.0);
        let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
        let (x1, y1) = (x0.saturating_add(1).min(w.saturating_sub(1)), y0.saturating_add(1).min(h.saturating_sub(1)));
        let (tx, ty) = (fx - fx.floor(), fy - fy.floor());
        let get = |x: usize, y: usize| f64::from(self.heights[y.wrapping_mul(w).wrapping_add(x)]);
        let top = get(x0, y0) + (get(x1, y0) - get(x0, y0)) * tx;
        let bot = get(x0, y1) + (get(x1, y1) - get(x0, y1)) * tx;
        top + (bot - top) * ty
    }

    /// The field as an opaque greyscale frame of `width x height`: `clamp(0.5 + h / (2 range))`
    /// at each pixel centre's [`Self::sample`]. `drawn` is 0.
    pub fn to_frame(&self, width: u32, height: u32, limits: &ParticleLimits, cancel: &CancellationToken) -> Result<ParticleFrame> {
        self.validate()?;
        let pixels = check_raster(width, height, limits)?;
        let mut plane = Vec::with_capacity(pixels);
        let range = f64::from(self.range);
        for y in 0..height {
            if y & 0x3F == 0 {
                cancel.check().map_err(|_| ParticleError::Canceled)?;
            }
            let v = (f64::from(y) + 0.5) / f64::from(height);
            for x in 0..width {
                let u = (f64::from(x) + 0.5) / f64::from(width);
                let g = (0.5 + self.sample(u, v) / (2.0 * range)).clamp(0.0, 1.0) as f32;
                plane.push([g, g, g, 1.0]);
            }
        }
        Ok(ParticleFrame { width, height, premultiplied: plane, drawn: 0 })
    }
}
