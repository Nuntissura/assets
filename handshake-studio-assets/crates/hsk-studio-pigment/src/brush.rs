//! Native brush engine, slice 1 (STU-RAS-125..127, 130): parametric round tip, pressure-driven
//! size / opacity / flow, dab placement along a polyline, stroke coverage, and the helpers that
//! feed the existing `masked_replace` for paint and erase. The engine produces data (a serde dab
//! list, which is also the develop brush-mask record of STU-RAW-121) and coverage planes; it never
//! publishes document state: publication stays in `masked_replace`'s ports.
//!
//! Every formula is a documented Studio reading of Adobe help text, class Approximate (oracle list
//! D5): the hardness falloff (smoothstep from `r * hardness` to `r`), the build-up rule
//! `buf + (1 - buf) * flow * footprint`, the stroke-level opacity cap (the largest dab opacity)
//! and the arc-length spacing origin are UNVERIFIED against Photoshop.
#![deny(clippy::arithmetic_side_effects)]
use crate::selection::{CombineMode, CoveragePlane, SelectionError, SelectionLimits, combine};
use crate::{Error, MAX_BYTES, Rect};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Typed failure of a brush operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrushError {
    Pigment(Error),
    Selection(SelectionError),
    /// A tip parameter, dynamics value or pressure is outside its range (never clamped).
    OutOfRange,
    /// `tip_kind` other than `Parametric`.
    TipKindUnsupported,
    /// A control signal, jitter or channel not evaluated in slice 1.
    DynamicsControlUnsupported,
}
impl BrushError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Pigment(e) => e.code(),
            Self::Selection(e) => e.code(),
            Self::OutOfRange => "brush_out_of_range",
            Self::TipKindUnsupported => "tip_kind_unsupported",
            Self::DynamicsControlUnsupported => "dynamics_control_unsupported",
        }
    }
}
impl From<Error> for BrushError {
    fn from(e: Error) -> Self {
        Self::Pigment(e)
    }
}
impl From<SelectionError> for BrushError {
    fn from(e: SelectionError) -> Self {
        Self::Selection(e)
    }
}
impl fmt::Display for BrushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for BrushError {}

/// Budgets checked before allocation or placement (V4-02).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrushLimits {
    pub max_dabs: usize,
    pub max_samples: usize,
    /// Working bytes of a stroke plane: 2 (u16 result) + 8 (f64 accumulation) per pixel.
    pub max_bytes: u64,
}
impl Default for BrushLimits {
    fn default() -> Self {
        Self {
            max_dabs: 1_000_000,
            max_samples: 1_000_000,
            max_bytes: MAX_BYTES,
        }
    }
}
impl BrushLimits {
    fn selection(&self) -> SelectionLimits {
        SelectionLimits {
            max_bytes: self.max_bytes,
            max_points: self.max_samples,
        }
    }
}

/// STU-RAS-126 tip kinds; only `Parametric` is accepted in slice 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TipKind {
    Parametric,
    Sampled,
    Erodible,
    AirbrushSpray,
}

/// STU-RAS-127 parametric tip.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrushTip {
    pub tip_kind: TipKind,
    pub diameter_px: f64,
    pub hardness_pct: f64,
    pub angle_deg: f64,
    pub roundness_pct: f64,
    /// Percent of the current diameter; no upper clamp (work is bounded by the dab cap).
    pub spacing_pct: f64,
    pub spacing_enabled: bool,
}
impl BrushTip {
    pub fn round(diameter_px: f64, hardness_pct: f64, spacing_pct: f64) -> Self {
        Self {
            tip_kind: TipKind::Parametric,
            diameter_px,
            hardness_pct,
            angle_deg: 0.0,
            roundness_pct: 100.0,
            spacing_pct,
            spacing_enabled: true,
        }
    }
    pub fn validate(&self) -> Result<(), BrushError> {
        if self.tip_kind != TipKind::Parametric {
            return Err(BrushError::TipKindUnsupported);
        }
        let ok = in_range(self.diameter_px, 1.0, 5000.0)
            && in_range(self.hardness_pct, 0.0, 100.0)
            && in_range(self.angle_deg, -180.0, 180.0)
            && in_range(self.roundness_pct, 0.0, 100.0)
            && self.roundness_pct > 0.0
            && self.spacing_pct.is_finite()
            && self.spacing_pct > 0.0;
        if ok { Ok(()) } else { Err(BrushError::OutOfRange) }
    }
}
fn in_range(v: f64, lo: f64, hi: f64) -> bool {
    v.is_finite() && v >= lo && v <= hi
}

/// STU-RAS-130 driving signal subset (discriminants are the spec values).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Control {
    Off = 0,
    Fade = 1,
    PenPressure = 2,
    PenTilt = 3,
    Velocity = 9,
}

/// One dynamics channel. Value = `base_value / 100 * f`, with `f = 1` for `Off` and
/// `minimum/100 + (1 - minimum/100) * pressure` for `PenPressure`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    pub base_value: f64,
    pub minimum: f64,
    pub jitter: f64,
    pub control: Control,
}
impl Channel {
    pub const fn off() -> Self {
        Self {
            base_value: 100.0,
            minimum: 0.0,
            jitter: 0.0,
            control: Control::Off,
        }
    }
    fn validate(&self) -> Result<(), BrushError> {
        if !in_range(self.base_value, 0.0, 100.0)
            || !in_range(self.minimum, 0.0, 100.0)
            || !self.jitter.is_finite()
        {
            return Err(BrushError::OutOfRange);
        }
        if self.jitter != 0.0 || !matches!(self.control, Control::Off | Control::PenPressure) {
            return Err(BrushError::DynamicsControlUnsupported);
        }
        Ok(())
    }
    fn value(&self, pressure: f64) -> f64 {
        let f = match self.control {
            Control::PenPressure => {
                let m = self.minimum / 100.0;
                m + (1.0 - m) * pressure
            }
            _ => 1.0,
        };
        self.base_value / 100.0 * f
    }
}

/// Slice 1 evaluates the `size`, `opacity` and `flow` channels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrushDynamics {
    pub size: Channel,
    pub opacity: Channel,
    pub flow: Channel,
}
impl Default for BrushDynamics {
    fn default() -> Self {
        Self {
            size: Channel::off(),
            opacity: Channel::off(),
            flow: Channel::off(),
        }
    }
}

/// One pointer sample in document pixels. Missing pressure is 1.0.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrokeSample {
    pub x: f64,
    pub y: f64,
    pub pressure: Option<f32>,
    pub time_ms: Option<f64>,
}

/// One stamp. The ordered list of dabs is the stroke data (STU-RAW-121): the mask is re-rendered
/// from it at any resolution. Opacity and flow are fractions in `[0, 1]`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dab {
    pub x: f64,
    pub y: f64,
    pub diameter: f64,
    pub opacity: f64,
    pub flow: f64,
    pub angle: f64,
    /// Fraction in `(0, 1]`.
    pub roundness: f64,
}

fn dab_at(tip: &BrushTip, dyn_: &BrushDynamics, x: f64, y: f64, pressure: f64) -> Dab {
    Dab {
        x,
        y,
        diameter: (tip.diameter_px * dyn_.size.value(pressure)).max(1.0),
        opacity: dyn_.opacity.value(pressure).clamp(0.0, 1.0),
        flow: dyn_.flow.value(pressure).clamp(0.0, 1.0),
        angle: tip.angle_deg,
        roundness: tip.roundness_pct / 100.0,
    }
}

/// Place dabs along the polyline: first dab at the first sample, then one per
/// `spacing_pct / 100 * current_diameter` of arc length, carrying the remainder across segments;
/// pressure is interpolated linearly; `spacing_enabled == false` is one dab per sample. The dab
/// cap is checked against an arc-length estimate before any dab is placed.
pub fn place_dabs(
    tip: &BrushTip,
    dyn_: &BrushDynamics,
    samples: &[StrokeSample],
    limits: &BrushLimits,
) -> Result<Vec<Dab>, BrushError> {
    tip.validate()?;
    dyn_.size.validate()?;
    dyn_.opacity.validate()?;
    dyn_.flow.validate()?;
    if samples.len() > limits.max_samples {
        return Err(Error::BudgetExceeded.into());
    }
    let mut pts = Vec::with_capacity(samples.len());
    for s in samples {
        if !s.x.is_finite() || !s.y.is_finite() || s.time_ms.is_some_and(|t| !t.is_finite()) {
            return Err(Error::Nonfinite.into());
        }
        let p = s.pressure.unwrap_or(1.0);
        if !p.is_finite() {
            return Err(Error::Nonfinite.into());
        }
        if !(0.0..=1.0).contains(&p) {
            return Err(BrushError::OutOfRange);
        }
        pts.push((s.x, s.y, f64::from(p)));
    }
    let Some(&(x0, y0, p0)) = pts.first() else {
        return Ok(Vec::new());
    };
    let step_of = |d: &Dab| d.diameter * tip.spacing_pct / 100.0;
    let mut out = Vec::new();
    out.push(dab_at(tip, dyn_, x0, y0, p0));
    if !tip.spacing_enabled {
        if pts.len() > limits.max_dabs {
            return Err(Error::BudgetExceeded.into());
        }
        for &(x, y, p) in pts.iter().skip(1) {
            out.push(dab_at(tip, dyn_, x, y, p));
        }
        return Ok(out);
    }
    let total: f64 = pts
        .windows(2)
        .map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1))
        .sum();
    let min_step = step_of(&dab_at(tip, dyn_, 0.0, 0.0, 0.0)).min(step_of(&dab_at(tip, dyn_, 0.0, 0.0, 1.0)));
    if total / min_step > limits.max_dabs as f64 || total.is_nan() || min_step.is_nan() {
        return Err(Error::BudgetExceeded.into());
    }
    let mut to_next = step_of(&out[0]);
    for w in pts.windows(2) {
        let ((ax, ay, ap), (bx, by, bp)) = (w[0], w[1]);
        let len = (bx - ax).hypot(by - ay);
        if len == 0.0 {
            continue;
        }
        let mut pos = 0.0f64;
        loop {
            let remaining = len - pos;
            if to_next <= remaining + 1e-9 {
                pos += to_next;
                let q = pos.min(len);
                let t = q / len;
                let d = dab_at(
                    tip,
                    dyn_,
                    ax + (bx - ax) * q / len,
                    ay + (by - ay) * q / len,
                    ap + t * (bp - ap),
                );
                if out.len() >= limits.max_dabs {
                    return Err(Error::BudgetExceeded.into());
                }
                to_next = step_of(&d);
                out.push(d);
            } else {
                to_next -= remaining;
                break;
            }
        }
    }
    Ok(out)
}

fn bound(v: f64) -> Result<f64, Error> {
    if v.is_finite() && v.abs() < 1.0e15 {
        Ok(v)
    } else if v.is_finite() {
        Err(Error::Overflow)
    } else {
        Err(Error::Nonfinite)
    }
}

fn footprint(dist: f64, h: f64) -> f64 {
    if dist <= h {
        1.0
    } else if dist >= 1.0 {
        0.0
    } else {
        let t = (1.0 - dist) / (1.0 - h);
        t * t * (3.0 - 2.0 * t)
    }
}

/// Stroke coverage over the dab bounding box clipped to `canvas` (zero-size when disjoint).
/// Footprint: 1 inside `r * hardness`, smoothstep to 0 at `r`, elliptical by angle and roundness,
/// sampled at pixel centres. Accumulation `buf += (1 - buf) * flow * footprint` per dab; the
/// stroke is `max(dab opacity) * buf`. The byte budget (10 bytes per pixel) is checked before
/// allocating.
pub fn stroke_coverage(
    dabs: &[Dab],
    hardness_pct: f64,
    canvas: Rect,
    limits: &BrushLimits,
) -> Result<CoveragePlane, BrushError> {
    if !in_range(hardness_pct, 0.0, 100.0) {
        return Err(BrushError::OutOfRange);
    }
    if dabs.len() > limits.max_dabs {
        return Err(Error::BudgetExceeded.into());
    }
    let mut lo = [f64::MAX; 2];
    let mut hi = [f64::MIN; 2];
    let mut cap = 0.0f64;
    for d in dabs {
        let vals = [d.x, d.y, d.diameter, d.opacity, d.flow, d.angle, d.roundness];
        if vals.iter().any(|v| !v.is_finite()) {
            return Err(Error::Nonfinite.into());
        }
        if d.diameter <= 0.0
            || d.roundness <= 0.0
            || d.roundness > 1.0
            || !(0.0..=1.0).contains(&d.opacity)
            || !(0.0..=1.0).contains(&d.flow)
        {
            return Err(BrushError::OutOfRange);
        }
        let r = d.diameter / 2.0;
        for (axis, c) in [d.x, d.y].into_iter().enumerate() {
            lo[axis] = lo[axis].min(bound(c - r)?);
            hi[axis] = hi[axis].max(bound(c + r)?);
        }
        cap = cap.max(d.opacity);
    }
    let empty = |c: Rect| CoveragePlane {
        bounds: Rect {
            x: c.x,
            y: c.y,
            width: 0,
            height: 0,
        },
        data: Vec::new(),
    };
    if dabs.is_empty() {
        return Ok(empty(canvas));
    }
    let (x0, y0) = (lo[0].floor() as i64, lo[1].floor() as i64);
    let w = (hi[0].ceil() as i64).checked_sub(x0).ok_or(Error::Overflow)?.max(0);
    let h = (hi[1].ceil() as i64).checked_sub(y0).ok_or(Error::Overflow)?.max(0);
    let bbox = Rect {
        x: x0,
        y: y0,
        width: u32::try_from(w).unwrap_or(u32::MAX),
        height: u32::try_from(h).unwrap_or(u32::MAX),
    };
    let Some(bounds) = bbox.intersect(canvas)? else {
        return Ok(empty(canvas));
    };
    if bounds.area().checked_mul(10).ok_or(Error::Overflow)? > limits.max_bytes {
        return Err(Error::BudgetExceeded.into());
    }
    let cells = usize::try_from(bounds.area()).map_err(|_| Error::Overflow)?;
    let (bw, bh) = (bounds.width as usize, bounds.height as usize);
    let mut buf = vec![0.0f64; cells];
    let hard = hardness_pct / 100.0;
    for d in dabs {
        let r = d.diameter / 2.0;
        let rv = r * d.roundness;
        let (sin, cos) = d.angle.to_radians().sin_cos();
        let bx = bounds.x as f64;
        let by = bounds.y as f64;
        let c0 = (d.x - r - bx).floor().clamp(0.0, bw as f64) as usize;
        let c1 = ((d.x + r - bx).ceil() + 1.0).clamp(0.0, bw as f64) as usize;
        let r0 = (d.y - r - by).floor().clamp(0.0, bh as f64) as usize;
        let r1 = ((d.y + r - by).ceil() + 1.0).clamp(0.0, bh as f64) as usize;
        for (ry, row) in buf
            .chunks_exact_mut(bw.max(1))
            .enumerate()
            .skip(r0)
            .take(r1.saturating_sub(r0))
        {
            let dy = by + ry as f64 + 0.5 - d.y;
            for (cx, cell) in row.iter_mut().enumerate().skip(c0).take(c1.saturating_sub(c0)) {
                let dx = bx + cx as f64 + 0.5 - d.x;
                let u = (dx * cos + dy * sin) / r;
                let v = (-dx * sin + dy * cos) / rv;
                let fp = footprint(u.hypot(v), hard);
                if fp > 0.0 {
                    *cell += (1.0 - *cell) * d.flow * fp;
                }
            }
        }
    }
    let data = buf
        .iter()
        .map(|b| (cap * b * f64::from(u16::MAX)).round().clamp(0.0, f64::from(u16::MAX)) as u16)
        .collect();
    Ok(CoveragePlane { bounds, data })
}

/// Re-frame a plane onto exactly `rect` (zero outside the plane), e.g. the tile rectangle a
/// `masked_replace` mask must contain. Budget checked before allocating.
pub fn crop_to(
    plane: &CoveragePlane,
    rect: Rect,
    limits: &BrushLimits,
) -> Result<CoveragePlane, BrushError> {
    let mut out = CoveragePlane::filled(rect, 0, &limits.selection())?;
    let (ex, ey) = rect.end()?;
    let mut data = Vec::with_capacity(out.data.len());
    for y in rect.y..ey {
        for x in rect.x..ex {
            data.push(plane.at(x, y));
        }
    }
    out.data = data;
    Ok(out)
}

/// Tight-stride `(rgb_f32le colour, f32le alpha)` planes of one solid colour over `bounds`: the
/// `replacement` of a paint stroke (alpha 1) for `masked_replace`.
pub fn paint_replacement(
    bounds: Rect,
    rgb: [f32; 3],
    limits: &BrushLimits,
) -> Result<(Vec<u8>, Vec<u8>), BrushError> {
    if rgb.iter().any(|v| !v.is_finite()) {
        return Err(Error::Nonfinite.into());
    }
    let n = pixels(bounds, 16, limits)?;
    let px: Vec<u8> = rgb.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut colour = Vec::with_capacity(n.saturating_mul(12));
    for _ in 0..n {
        colour.extend_from_slice(&px);
    }
    let alpha = std::iter::repeat_n(1.0f32.to_le_bytes(), n).flatten().collect();
    Ok((colour, alpha))
}

/// Replacement of an erase stroke: the layer's own colour with alpha 0 (erase to transparency),
/// so `masked_replace` lowers alpha by exactly the coverage and keeps the colour.
pub fn erase_replacement(
    before_colour: &[u8],
    bounds: Rect,
    limits: &BrushLimits,
) -> Result<(Vec<u8>, Vec<u8>), BrushError> {
    let n = pixels(bounds, 16, limits)?;
    if n.checked_mul(12) != Some(before_colour.len()) {
        return Err(Error::MalformedStride.into());
    }
    Ok((before_colour.to_vec(), vec![0u8; n.saturating_mul(4)]))
}

fn pixels(bounds: Rect, bytes_per_px: u64, limits: &BrushLimits) -> Result<usize, Error> {
    if bounds.area().checked_mul(bytes_per_px).ok_or(Error::Overflow)? > limits.max_bytes {
        return Err(Error::BudgetExceeded);
    }
    usize::try_from(bounds.area()).map_err(|_| Error::Overflow)
}

/// Where an erase stroke goes (STU-RAS-170).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EraseRoute {
    /// Pixel-editable layer: build the request with [`erase_replacement`].
    Pixels,
    /// Non-pixel-editable layer: the new layer-mask plane; the pixels are untouched and the
    /// command receipt names `receipt` (`erase_routed_to_mask`).
    Mask {
        plane: CoveragePlane,
        receipt: &'static str,
    },
}
pub const RECEIPT_ERASE_ROUTED_TO_MASK: &str = "erase_routed_to_mask";

/// Route an erase stroke. For a non-pixel-editable target the layer mask (1 = reveal; absent =
/// fully revealed over the stroke) gets the stroke removed with [`CombineMode::Diminish`].
pub fn route_erase(
    target_not_pixel_editable: bool,
    stroke: &CoveragePlane,
    layer_mask: Option<&CoveragePlane>,
    limits: &BrushLimits,
) -> Result<EraseRoute, BrushError> {
    if !target_not_pixel_editable {
        return Ok(EraseRoute::Pixels);
    }
    let sel = limits.selection();
    let implicit;
    let base = match layer_mask {
        Some(m) => m,
        None => {
            implicit = CoveragePlane::filled(stroke.bounds, u16::MAX, &sel)?;
            &implicit
        }
    };
    Ok(EraseRoute::Mask {
        plane: combine(Some(base), stroke, CombineMode::Diminish, &sel)?,
        receipt: RECEIPT_ERASE_ROUTED_TO_MASK,
    })
}

/// Brush on a mask, quick mask or selection through the selection lane's combine set.
pub fn paint_mask(
    plane: Option<&CoveragePlane>,
    stroke: &CoveragePlane,
    mode: CombineMode,
    limits: &BrushLimits,
) -> Result<CoveragePlane, BrushError> {
    Ok(combine(plane, stroke, mode, &limits.selection())?)
}

/// Clip a stroke to a selection (coverage product).
pub fn clip_to_selection(
    stroke: &CoveragePlane,
    selection: &CoveragePlane,
    limits: &BrushLimits,
) -> Result<CoveragePlane, BrushError> {
    paint_mask(Some(stroke), selection, CombineMode::Intersect, limits)
}

/// Brush surface description (Studio readings, parity NOT_PROVEN).
pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PIGMENT","surface":"brush","version":1,"tip":"Parametric only (sampled, erodible, airbrush_spray refuse tip_kind_unsupported); diameter 1..5000, hardness 0..100, angle -180..180, roundness (0,100], spacing >0 with no upper clamp","dynamics":"channels size, opacity, flow; control off|pen_pressure evaluated, other controls and jitter refuse dynamics_control_unsupported; response curves, smoothing, scatter, dual tip, texture pending","dabs":"serde Dab list = stroke data (STU-RAW-121); place_dabs: arc-length spacing with remainder carry, linear pressure interpolation","coverage":"smoothstep falloff r*hardness..r, elliptical; buf += (1-buf)*flow*footprint; stroke = max dab opacity * buf; Studio reading, Approximate, D5","application":"paint_replacement / erase_replacement feed masked_replace; route_erase sends a non-pixel-editable erase to the layer mask (erase_routed_to_mask, STU-RAS-170); paint_mask uses selection::combine","limits":{"max_dabs":1000000,"max_samples":1000000,"max_bytes":16777216},"parity":"NOT_PROVEN"}"#;
