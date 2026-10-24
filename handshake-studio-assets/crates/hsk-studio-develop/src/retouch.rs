//! Develop-scoped retouch (lane SMB-LR-HEALING-1): heal / clone spot removal and the core
//! distraction removal on `StudioRawDevelop`, reusing the raster retouch primitives of
//! hsk-studio-pigment (STU-RAW-015 "they are not a parallel retouch implementation", STU-DOC-004).
//!
//! Spec basis: STU-RAW-124 (a retouch area carries `spot_type` heal|clone, `heal_version`, `feather`,
//! `opacity`, `seed`, `method` (`gaussian` observed), `source_state` (`source_set_explicitly` versus an
//! engine-chosen source), a target dab-mask list in the STU-RAW-121 brush geometry and the source
//! offset `source_x`, `source_y`, `offset_y`, `centre_x`, `centre_y`, `radius`; lists default to empty;
//! gated by `enable_retouch`; a generative distraction fill is a `StudioModelAdapter` lane whose record
//! carries the adapter identity, model identity and version, locality, the patch variation set and the
//! digests), STU-RAS-143 (core distraction removal = the native content-aware fill), STU-RAW-105 (the
//! enable flags are state: a disabled group keeps its records and changes nothing), STU-RAW-106.
//!
//! Engine (tests/develop_retouch.rs):
//! - Target mask: the union (screen, `1 - (1 - a)(1 - b)`) of the area's brush masks, rasterised at the
//!   current resolution with develop's own brush rasteriser (`local::rasterise_brush_window`: radius a
//!   fraction of the shorter side, smoothstep feather, dab screen accumulation); an area with no dab
//!   mask is the circle `centre_x, centre_y, radius` with the area `feather`. Coverage is multiplied
//!   by `opacity` and quantised to pigment's 16-bit coverage.
//! - Explicit source: the target is the source translated by `(centre - source)` in pixels
//!   (`source_x * width`, `source_y * height`); pigment `clone_stamp` (clone) or `heal` (heal, the
//!   membrane Poisson blend) with that offset; an integer offset copies exactly.
//! - Engine-chosen source: pigment `spot_heal`'s proximity candidate search picks the offset; heal
//!   areas keep its healed result, clone areas clone from the chosen offset.
//! - Distraction areas: pigment `content_aware::fill` (sampling `auto`, default options, the area
//!   `seed` as the synthesis seed) over the hole grown by `max(16, hole side)` pixels of context.
//! - Adapter fill records are validated and retained, never rendered: with
//!   `enable_distraction_removal` a recipe holding one is refused `model_unavailable:distraction_adapter_fill`.
//!
//! Declared readings (UNVERIFIED against Lightroom Classic, D5 oracle list):
//! - Stage position: whole frame, first of the post-tile stages (before detail / clarity, local
//!   corrections and effects), on the developed scene-linear frame after the crop; mask and source
//!   coordinates are normalised to that developed frame. Retouch areas run before distraction areas.
//! - Domain: pigment planes hold `[0, 1]`. Each area's target and sample windows are divided by the
//!   smallest power of two `s >= max(1, largest value)` and the covered pixels multiplied back by
//!   `s`; both are exact in binary32 for normal values, so scene-linear values above 1 are not
//!   clipped by the scaling (pigment's own `[0, alpha]` clamp of a solved value still applies). A
//!   negative or non-finite value cannot be represented and is refused
//!   (`unsupported_contribution:retouch_negative_value`, `non_finite`), never clipped.
//! - `heal_version`, `method`, `seed` (retouch areas) and `offset_y` are validated and retained; the
//!   engine has one heal algorithm and one feather profile, so they do not change the pixels.
//! - A source footprint that leaves the frame is refused (`unsupported_contribution:retouch_source_outside_frame`).
//! - The tile API refuses an active stage (`unsupported_contribution:retouch_whole_frame`).
//!
//! Hostile records (V4-01/02): list lengths and the aggregate dab count are checked before any
//! per-record work, every number must be finite and in range, every window is checked against
//! [`RETOUCH_MAX_BYTES`] before a plane is allocated, and integer arithmetic is checked.
#![deny(clippy::arithmetic_side_effects)]

use crate::DevelopError;
use crate::local::{Brush, Mask, MaskBlend, MaskSource};
use crate::par::Threads;
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::brush::Dab;
use hsk_studio_pigment::content_aware::{self, ContentAwareOptions, SamplingArea};
use hsk_studio_pigment::retouch::{
    self as pr, CloneMap, HEAL_BYTES_PER_PIXEL, RetouchError, RetouchLimits, RgbaPlane, SPOT_RING_PX, StrokeFootprint,
};
use hsk_studio_pigment::selection::CoveragePlane;
use hsk_studio_pigment::transform_math::ResampleFilter;

/// Most retouch areas one recipe may carry.
pub const MAX_RETOUCH_AREAS: usize = 256;
/// Most distraction areas one recipe may carry.
pub const MAX_DISTRACTION_AREAS: usize = 64;
/// Most generative adapter fill records one recipe may carry.
pub const MAX_ADAPTER_FILLS: usize = 64;
/// Most dab masks one area may carry.
pub const MAX_AREA_MASKS: usize = 16;
/// Most dabs over the whole stack (every mask of every area).
pub const MAX_STACK_DABS: usize = 1 << 20;
/// Longest text field of an adapter record, in bytes.
pub const MAX_RECORD_TEXT: usize = 256;
/// Most patch variations / remap-info strings of one adapter record.
pub const MAX_VARIATIONS: usize = 64;
/// Working-byte budget of one area (pigment planes, heal solver, coverage), checked before allocation.
pub const RETOUCH_MAX_BYTES: u64 = 1 << 30;
/// Smallest context margin (pixels) around a distraction hole.
pub const DISTRACTION_CONTEXT_MIN_PX: u32 = 16;
/// `model_unavailable` kind of a generative adapter fill record.
pub const ADAPTER_FILL_KIND: &str = "distraction_adapter_fill";

/// STU-RAW-124 `spot_type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpotType {
    Heal,
    Clone,
}

impl SpotType {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Heal => "heal",
            Self::Clone => "clone",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        [Self::Heal, Self::Clone].into_iter().find(|v| v.key() == key)
    }
}

/// STU-RAW-124 `source_state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceState {
    SourceSetExplicitly,
    EngineChosen,
}

impl SourceState {
    pub const fn key(self) -> &'static str {
        match self {
            Self::SourceSetExplicitly => "source_set_explicitly",
            Self::EngineChosen => "engine_chosen",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        [Self::SourceSetExplicitly, Self::EngineChosen].into_iter().find(|v| v.key() == key)
    }
}

/// STU-RAW-124 `method` (only `gaussian` is observed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetouchMethod {
    Gaussian,
}

impl RetouchMethod {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Gaussian => "gaussian",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        (key == "gaussian").then_some(Self::Gaussian)
    }
}

/// One STU-RAW-124 retouch area (every field of the record).
#[derive(Clone, Debug, PartialEq)]
pub struct RetouchArea {
    pub spot_type: SpotType,
    pub heal_version: u32,
    /// 0..1 fraction of the radius over which the circle form falls off.
    pub feather: f64,
    /// 0..1, multiplies the target coverage.
    pub opacity: f64,
    pub seed: u64,
    pub method: RetouchMethod,
    pub source_state: SourceState,
    /// Target dab masks in the STU-RAW-121 brush geometry (normalised dabs, radius, flow, feather).
    pub masks: Vec<Brush>,
    /// Normalised source point.
    pub source_x: f64,
    pub source_y: f64,
    /// Retained (meaning UNVERIFIED; see the module readings).
    pub offset_y: f64,
    /// Normalised target centre.
    pub centre_x: f64,
    pub centre_y: f64,
    /// Fraction of the shorter side, 0..1; the target circle when `masks` is empty.
    pub radius: f64,
}

impl RetouchArea {
    /// A circle spot (no dab mask) at `centre` with `radius`, sourced explicitly from `source`;
    /// feather 0, opacity 1, heal version 0, seed 0, method gaussian.
    pub fn circle(spot_type: SpotType, centre: (f64, f64), radius: f64, source: (f64, f64)) -> Self {
        Self {
            spot_type,
            heal_version: 0,
            feather: 0.0,
            opacity: 1.0,
            seed: 0,
            method: RetouchMethod::Gaussian,
            source_state: SourceState::SourceSetExplicitly,
            masks: Vec::new(),
            source_x: source.0,
            source_y: source.1,
            offset_y: 0.0,
            centre_x: centre.0,
            centre_y: centre.1,
            radius,
        }
    }

    fn validate(&self) -> Result<(), DevelopError> {
        let values = [
            self.feather, self.opacity, self.source_x, self.source_y, self.offset_y, self.centre_x, self.centre_y, self.radius,
        ];
        if values.iter().any(|v| !v.is_finite()) {
            return Err(DevelopError::NonFinite);
        }
        let positions = [self.source_x, self.source_y, self.offset_y, self.centre_x, self.centre_y];
        if !(0.0..=1.0).contains(&self.feather)
            || !(0.0..=1.0).contains(&self.opacity)
            || !(0.0..=1.0).contains(&self.radius)
            || positions.iter().any(|v| v.abs() > 16.0)
            || (self.masks.is_empty() && self.radius == 0.0)
        {
            return Err(DevelopError::InvalidMask);
        }
        validate_masks(&self.masks)
    }

    /// The masks the target coverage is rasterised from: the dab masks, or the circle.
    fn target_masks(&self) -> Vec<Brush> {
        if self.masks.is_empty() {
            vec![Brush { dabs: vec![(self.centre_x, self.centre_y)], radius: self.radius, flow: 1.0, feather: self.feather }]
        } else {
            self.masks.clone()
        }
    }
}

/// Core distraction removal (STU-RAS-143 native fill) over a dab-mask hole.
#[derive(Clone, Debug, PartialEq)]
pub struct DistractionArea {
    pub masks: Vec<Brush>,
    /// Synthesis seed of the native fill (fixed seed = fixed bytes).
    pub seed: u64,
}

impl DistractionArea {
    fn validate(&self) -> Result<(), DevelopError> {
        validate_masks(&self.masks)
    }
}

/// Pixel rectangle of a generative adapter record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdapterRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// One cycleable patch variation of a generative fill.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchVariation {
    pub id: String,
    pub patch_digest: [u8; 32],
}

/// A distraction fill produced by a generative model (STU-RAW-124 third path): a
/// `StudioModelAdapter` lane record, classified and retained, never rendered by develop.
#[derive(Clone, Debug, PartialEq)]
pub struct AdapterFillRecord {
    pub adapter_id: String,
    pub model_id: String,
    pub model_version: String,
    pub ran_locally: bool,
    pub fill_method: String,
    /// Image extent `(width, height)` in pixels the fill was computed on.
    pub image_extent: (u32, u32),
    pub search_rect: AdapterRect,
    pub target_rect: AdapterRect,
    pub black_level: f64,
    pub gamma_type: String,
    pub input_digest: [u8; 32],
    pub patch_digest: [u8; 32],
    pub remap_info: Vec<String>,
    pub variations: Vec<PatchVariation>,
    /// Index into `variations` of the variation in use.
    pub selected_variation: Option<u32>,
}

impl AdapterFillRecord {
    /// The adapter lane this record belongs to (STU-RAS-045 / STU-RAS-164).
    pub const CLASS: &'static str = "studio_model_adapter";

    pub fn validate(&self) -> Result<(), DevelopError> {
        let texts = [&self.adapter_id, &self.model_id, &self.model_version, &self.fill_method, &self.gamma_type];
        if self.remap_info.len() > MAX_VARIATIONS || self.variations.len() > MAX_VARIATIONS {
            return Err(DevelopError::InvalidMask);
        }
        if texts.iter().any(|t| t.len() > MAX_RECORD_TEXT)
            || [&self.adapter_id, &self.model_id, &self.model_version].iter().any(|t| t.is_empty())
            || self.remap_info.iter().any(|t| t.len() > MAX_RECORD_TEXT)
            || self.variations.iter().any(|v| v.id.is_empty() || v.id.len() > MAX_RECORD_TEXT)
        {
            return Err(DevelopError::InvalidMask);
        }
        if !self.black_level.is_finite() {
            return Err(DevelopError::NonFinite);
        }
        if self.black_level < 0.0 {
            return Err(DevelopError::InvalidMask);
        }
        let (w, h) = self.image_extent;
        for r in [self.search_rect, self.target_rect] {
            let right = r.x.checked_add(r.width).ok_or(DevelopError::Overflow)?;
            let bottom = r.y.checked_add(r.height).ok_or(DevelopError::Overflow)?;
            if right > w || bottom > h {
                return Err(DevelopError::InvalidMask);
            }
        }
        if let Some(i) = self.selected_variation
            && usize::try_from(i).ok().is_none_or(|i| i >= self.variations.len())
        {
            return Err(DevelopError::InvalidMask);
        }
        Ok(())
    }

    /// Develop never renders an adapter fill: a valid record is `model_unavailable:distraction_adapter_fill`.
    pub fn render(&self) -> Result<(), DevelopError> {
        self.validate()?;
        Err(DevelopError::ModelUnavailable(ADAPTER_FILL_KIND))
    }
}

/// The STU-RAW-124 lists of a recipe; empty by default.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RetouchStack {
    pub areas: Vec<RetouchArea>,
    pub distraction: Vec<DistractionArea>,
    pub adapter_fills: Vec<AdapterFillRecord>,
}

impl RetouchStack {
    pub fn is_empty(&self) -> bool {
        self.areas.is_empty() && self.distraction.is_empty() && self.adapter_fills.is_empty()
    }

    /// Structural validation: counts and the aggregate dab count first, then every record.
    pub fn validate(&self) -> Result<(), DevelopError> {
        if self.areas.len() > MAX_RETOUCH_AREAS
            || self.distraction.len() > MAX_DISTRACTION_AREAS
            || self.adapter_fills.len() > MAX_ADAPTER_FILLS
        {
            return Err(DevelopError::InvalidMask);
        }
        let mask_lists = self.areas.iter().map(|a| &a.masks).chain(self.distraction.iter().map(|d| &d.masks));
        let mut dabs = 0usize;
        for masks in mask_lists {
            if masks.len() > MAX_AREA_MASKS {
                return Err(DevelopError::InvalidMask);
            }
            for brush in masks {
                dabs = dabs.checked_add(brush.dabs.len()).ok_or(DevelopError::Overflow)?;
            }
            if dabs > MAX_STACK_DABS {
                return Err(DevelopError::InvalidMask);
            }
        }
        self.areas.iter().try_for_each(RetouchArea::validate)?;
        self.distraction.iter().try_for_each(DistractionArea::validate)?;
        self.adapter_fills.iter().try_for_each(AdapterFillRecord::validate)
    }
}

fn validate_masks(masks: &[Brush]) -> Result<(), DevelopError> {
    if masks.len() > MAX_AREA_MASKS {
        return Err(DevelopError::InvalidMask);
    }
    for brush in masks {
        Mask { id: String::new(), active: true, inverted: false, value: 1.0, blend: MaskBlend::Add, source: MaskSource::Brush(brush.clone()) }
            .validate()?;
    }
    Ok(())
}

/// Which retouch families run (enable flag on and a non-empty list).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PreparedRetouch {
    pub retouch: bool,
    pub distraction: bool,
}

impl PreparedRetouch {
    pub(crate) fn prepare(stack: &RetouchStack, retouch_on: bool, distraction_on: bool) -> Result<Self, DevelopError> {
        stack.validate()?;
        if distraction_on && !stack.adapter_fills.is_empty() {
            return Err(DevelopError::ModelUnavailable(ADAPTER_FILL_KIND));
        }
        Ok(Self { retouch: retouch_on && !stack.areas.is_empty(), distraction: distraction_on && !stack.distraction.is_empty() })
    }

    pub(crate) fn is_active(self) -> bool {
        self.retouch || self.distraction
    }
}

fn pigment(e: RetouchError) -> DevelopError {
    match e {
        RetouchError::Pigment(hsk_studio_pigment::Error::BudgetExceeded | hsk_studio_pigment::Error::Overflow)
        | RetouchError::ContentAwareHoleTooLarge => DevelopError::OutputTooLarge,
        RetouchError::OutOfDomain => DevelopError::SampleOutOfRange,
        other => DevelopError::UnsupportedContribution(other.code()),
    }
}

fn frame(width: u32, height: u32) -> Rect {
    Rect { x: 0, y: 0, width, height }
}

/// `r` grown by `g` pixels on every side, intersected with `within`.
fn grow_within(r: Rect, g: u32, within: Rect) -> Result<Rect, DevelopError> {
    let twice = g.checked_mul(2).ok_or(DevelopError::Overflow)?;
    let grown = Rect {
        x: r.x.checked_sub(i64::from(g)).ok_or(DevelopError::Overflow)?,
        y: r.y.checked_sub(i64::from(g)).ok_or(DevelopError::Overflow)?,
        width: r.width.checked_add(twice).ok_or(DevelopError::Overflow)?,
        height: r.height.checked_add(twice).ok_or(DevelopError::Overflow)?,
    };
    grown
        .intersect(within)
        .map_err(|_| DevelopError::Overflow)?
        .ok_or(DevelopError::InvalidMask)
}

fn admit(r: Rect) -> Result<(), DevelopError> {
    let bytes = r.area().checked_mul(HEAL_BYTES_PER_PIXEL).ok_or(DevelopError::OutputTooLarge)?;
    if bytes > RETOUCH_MAX_BYTES { Err(DevelopError::OutputTooLarge) } else { Ok(()) }
}

/// Coverage of a mask union over a `width x height` frame, times `scale`, on the masks' bounding
/// window; `None` when no pixel is covered.
fn masks_coverage(masks: &[Brush], width: u32, height: u32, scale: f64) -> Result<Option<CoveragePlane>, DevelopError> {
    validate_masks(masks)?;
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 {
        return Ok(None);
    }
    let short = w.min(h) as f64;
    let (mut lo_x, mut lo_y, mut hi_x, mut hi_y) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for brush in masks {
        let radius = (brush.radius * short).max(0.5);
        for (nx, ny) in &brush.dabs {
            let (cx, cy) = (nx * w as f64, ny * h as f64);
            lo_x = lo_x.min((cx - radius).floor());
            lo_y = lo_y.min((cy - radius).floor());
            hi_x = hi_x.max((cx + radius).ceil());
            hi_y = hi_y.max((cy + radius).ceil());
        }
    }
    let (x0, y0) = (lo_x.max(0.0), lo_y.max(0.0));
    let (x1, y1) = (hi_x.min(w as f64 - 1.0), hi_y.min(h as f64 - 1.0));
    if !(x0 <= x1 && y0 <= y1) {
        return Ok(None);
    }
    let (x0, y0, x1, y1) = (x0 as usize, y0 as usize, x1 as usize, y1 as usize);
    let ww = x1.checked_sub(x0).and_then(|v| v.checked_add(1)).ok_or(DevelopError::Overflow)?;
    let wh = y1.checked_sub(y0).and_then(|v| v.checked_add(1)).ok_or(DevelopError::Overflow)?;
    let bounds = Rect {
        x: i64::try_from(x0).map_err(|_| DevelopError::Overflow)?,
        y: i64::try_from(y0).map_err(|_| DevelopError::Overflow)?,
        width: u32::try_from(ww).map_err(|_| DevelopError::Overflow)?,
        height: u32::try_from(wh).map_err(|_| DevelopError::Overflow)?,
    };
    admit(bounds)?;
    let mut acc = vec![0.0f64; ww.checked_mul(wh).ok_or(DevelopError::Overflow)?];
    for brush in masks {
        let plane = crate::local::rasterise_brush_window(brush, w, h, (x0, y0, ww, wh))?;
        for (slot, a) in acc.iter_mut().zip(plane) {
            *slot = 1.0 - (1.0 - *slot) * (1.0 - f64::from(a));
        }
    }
    let full = f64::from(u16::MAX);
    let data: Vec<u16> = acc.iter().map(|c| (c * scale * full).round().clamp(0.0, full) as u16).collect();
    if data.iter().all(|c| *c == 0) {
        return Ok(None);
    }
    Ok(Some(CoveragePlane { bounds, data }))
}

/// Target coverage of a retouch area (mask union or circle, times `opacity`) on its bounding window
/// of a `width x height` developed frame; `None` when no pixel is covered.
pub fn area_coverage(area: &RetouchArea, width: u32, height: u32) -> Result<Option<CoveragePlane>, DevelopError> {
    area.validate()?;
    masks_coverage(&area.target_masks(), width, height, area.opacity)
}

/// Hole of a distraction area (mask union) on its bounding window; `None` when no pixel is covered.
pub fn distraction_coverage(area: &DistractionArea, width: u32, height: u32) -> Result<Option<CoveragePlane>, DevelopError> {
    area.validate()?;
    masks_coverage(&area.masks, width, height, 1.0)
}

fn index(width: u32, x: i64, y: i64) -> Result<usize, DevelopError> {
    let (x, y) = (usize::try_from(x).map_err(|_| DevelopError::Overflow)?, usize::try_from(y).map_err(|_| DevelopError::Overflow)?);
    y.checked_mul(width as usize)
        .and_then(|v| v.checked_add(x))
        .and_then(|v| v.checked_mul(3))
        .ok_or(DevelopError::Overflow)
}

/// The power-of-two divisor of the module's domain reading over every pixel of `rects`.
fn scale_for(rgb: &[f32], width: u32, rects: &[Rect]) -> Result<f32, DevelopError> {
    let mut max = 0.0f32;
    for r in rects {
        let (ex, ey) = r.end().map_err(|_| DevelopError::Overflow)?;
        for y in r.y..ey {
            let start = index(width, r.x, y)?;
            let end = index(width, ex, y)?;
            for v in rgb.get(start..end).ok_or(DevelopError::InvalidLength)? {
                if !v.is_finite() {
                    return Err(DevelopError::NonFinite);
                }
                if *v < 0.0 {
                    return Err(DevelopError::UnsupportedContribution("retouch_negative_value"));
                }
                max = max.max(*v);
            }
        }
    }
    let mut s = 1.0f32;
    while s < max {
        s *= 2.0;
    }
    Ok(s)
}

fn plane(rgb: &[f32], width: u32, r: Rect, s: f32) -> Result<RgbaPlane, DevelopError> {
    admit(r)?;
    let (ex, ey) = r.end().map_err(|_| DevelopError::Overflow)?;
    let mut data = Vec::with_capacity(usize::try_from(r.area()).map_err(|_| DevelopError::Overflow)?);
    for y in r.y..ey {
        let start = index(width, r.x, y)?;
        let end = index(width, ex, y)?;
        for px in rgb.get(start..end).ok_or(DevelopError::InvalidLength)?.chunks_exact(3) {
            data.push([px[0] / s, px[1] / s, px[2] / s, 1.0]);
        }
    }
    RgbaPlane::new(r, data).map_err(pigment)
}

/// Copy every covered pixel of `out` back into the frame, multiplied by `s`.
fn write_back(rgb: &mut [f32], width: u32, out: &RgbaPlane, cov: &CoveragePlane, s: f32) -> Result<(), DevelopError> {
    let (ex, ey) = cov.bounds.end().map_err(|_| DevelopError::Overflow)?;
    for y in cov.bounds.y..ey {
        for x in cov.bounds.x..ex {
            if cov.at(x, y) == 0 {
                continue;
            }
            let p = out.at(x, y);
            let at = index(width, x, y)?;
            let slot = rgb.get_mut(at..at.checked_add(3).ok_or(DevelopError::Overflow)?).ok_or(DevelopError::InvalidLength)?;
            for (o, v) in slot.iter_mut().zip(p) {
                *o = v * s;
            }
        }
    }
    Ok(())
}

/// One hard dab covering `r` entirely (coverage 1 everywhere in `r`), so the stroke coverage
/// clipped to the selection is exactly the selection (pigment `clip_to_selection`, a product).
fn cover_dab(r: Rect) -> Dab {
    let (w, h) = (f64::from(r.width), f64::from(r.height));
    Dab {
        x: r.x as f64 + w / 2.0,
        y: r.y as f64 + h / 2.0,
        diameter: w.hypot(h) + 4.0,
        opacity: 1.0,
        flow: 1.0,
        angle: 0.0,
        roundness: 1.0,
    }
}

fn limits() -> RetouchLimits {
    RetouchLimits { max_bytes: RETOUCH_MAX_BYTES, ..RetouchLimits::default() }
}

fn apply_area(area: &RetouchArea, rgb: &mut [f32], width: u32, height: u32) -> Result<(), DevelopError> {
    let Some(cov) = area_coverage(area, width, height)? else {
        return Ok(());
    };
    let whole = frame(width, height);
    let target_rect = grow_within(cov.bounds, 2, whole)?;
    let dabs = [cover_dab(target_rect)];
    let footprint = StrokeFootprint { dabs: &dabs, hardness_pct: 100.0, selection: Some(&cov) };
    let limits = limits();
    let (out, s) = match area.source_state {
        SourceState::SourceSetExplicitly => {
            let dx = (area.centre_x - area.source_x) * f64::from(width);
            let dy = (area.centre_y - area.source_y) * f64::from(height);
            let (sx, sy) = (cov.bounds.x as f64 - dx, cov.bounds.y as f64 - dy);
            if sx < 0.0
                || sy < 0.0
                || sx + f64::from(cov.bounds.width) > f64::from(width)
                || sy + f64::from(cov.bounds.height) > f64::from(height)
            {
                return Err(DevelopError::UnsupportedContribution("retouch_source_outside_frame"));
            }
            let source_rect = Rect { x: sx.floor() as i64, y: sy.floor() as i64, width: cov.bounds.width, height: cov.bounds.height };
            let sample_rect = grow_within(source_rect, 2, whole)?;
            let s = scale_for(rgb, width, &[target_rect, sample_rect])?;
            let target = plane(rgb, width, target_rect, s)?;
            let sample = plane(rgb, width, sample_rect, s)?;
            let map = CloneMap::offset(dx, dy, ResampleFilter::Bilinear).map_err(pigment)?;
            let out = match area.spot_type {
                SpotType::Clone => pr::clone_stamp(&target, &sample, &map, &footprint, &limits).map_err(pigment)?.plane,
                SpotType::Heal => pr::heal(&target, &sample, &map, &footprint, &limits).map_err(pigment)?.result.plane,
            };
            (out, s)
        }
        SourceState::EngineChosen => {
            // spot_heal tries displacements of up to 2 (side + 2 ring + 1) and scores a ring around them.
            let side = cov.bounds.width.max(cov.bounds.height);
            let reach = side
                .checked_add(SPOT_RING_PX.checked_mul(2).ok_or(DevelopError::Overflow)?)
                .and_then(|v| v.checked_add(1))
                .and_then(|v| v.checked_mul(2))
                .and_then(|v| v.checked_add(SPOT_RING_PX))
                .and_then(|v| v.checked_add(2))
                .ok_or(DevelopError::Overflow)?;
            let sample_rect = grow_within(cov.bounds, reach, whole)?;
            let s = scale_for(rgb, width, &[target_rect, sample_rect])?;
            let target = plane(rgb, width, target_rect, s)?;
            let sample = plane(rgb, width, sample_rect, s)?;
            let healed = pr::spot_heal(&target, &sample, &footprint, &limits).map_err(pigment)?;
            let out = match area.spot_type {
                SpotType::Heal => healed.result.plane,
                SpotType::Clone => {
                    let (ox, oy) = healed.receipt.source_offset.ok_or(DevelopError::UnsupportedContribution("spot_heal_no_source"))?;
                    let map = CloneMap::offset(-(ox as f64), -(oy as f64), ResampleFilter::Bilinear).map_err(pigment)?;
                    pr::clone_stamp(&target, &sample, &map, &footprint, &limits).map_err(pigment)?.plane
                }
            };
            (out, s)
        }
    };
    write_back(rgb, width, &out, &cov, s)
}

fn fill_area(area: &DistractionArea, rgb: &mut [f32], width: u32, height: u32, threads: Threads) -> Result<(), DevelopError> {
    let Some(hole) = distraction_coverage(area, width, height)? else {
        return Ok(());
    };
    let margin = hole.bounds.width.max(hole.bounds.height).max(DISTRACTION_CONTEXT_MIN_PX);
    let target_rect = grow_within(hole.bounds, margin, frame(width, height))?;
    let s = scale_for(rgb, width, &[target_rect])?;
    let target = plane(rgb, width, target_rect, s)?;
    let options = ContentAwareOptions {
        seed: area.seed,
        threads: u32::try_from(threads.get().min(64)).map_err(|_| DevelopError::Overflow)?,
        ..ContentAwareOptions::default()
    };
    let out = content_aware::fill(&target, &hole, SamplingArea::Auto, &options, &limits()).map_err(pigment)?;
    write_back(rgb, width, &out.result.plane, &hole, s)
}

/// The retouch stage over a developed `width x height` scene-linear RGB frame (see the module
/// readings): every retouch area in list order, then every distraction area.
pub(crate) fn apply(
    stack: &RetouchStack,
    gates: PreparedRetouch,
    rgb: &mut [f32],
    width: u32,
    height: u32,
    threads: Threads,
    cancel: &CancellationToken,
) -> Result<(), DevelopError> {
    if !gates.is_active() {
        return Ok(());
    }
    let samples = (width as usize).checked_mul(height as usize).and_then(|p| p.checked_mul(3)).ok_or(DevelopError::Overflow)?;
    if rgb.len() != samples {
        return Err(DevelopError::InvalidLength);
    }
    if gates.retouch {
        for area in &stack.areas {
            cancel.check()?;
            apply_area(area, rgb, width, height)?;
        }
    }
    if gates.distraction {
        for area in &stack.distraction {
            cancel.check()?;
            fill_area(area, rgb, width, height, threads)?;
        }
    }
    Ok(())
}
