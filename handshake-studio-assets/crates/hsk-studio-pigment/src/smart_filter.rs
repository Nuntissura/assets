//! Smart (live) filter stack evaluation (STU-RAS-037 "`StudioLiveFilter` ... a re-editable,
//! maskable filter ... per-filter masking"; STU-FX-011a "a filter on any layer is already the
//! non-destructive stack"; STU-FX-024 liquify "stored as a re-editable mesh on the stack entry").
//! Lane SMB-PS-SELECT-LIQUIFY slice 5.
//!
//! Pigment already carries filters non-destructively in the plan envelope
//! ([`crate::render_plan::LayerExtension::filters`], unmasked, at most
//! [`MAX_FILTERS_PER_LAYER`], executed by render-cpu / render-gpu). This module adds the stack
//! semantics that envelope lacks: per-entry enable, opacity and coverage mask, a stack-wide filter
//! mask, and liquify meshes as entries. [`SmartFilterStack::render`] always re-renders from the
//! untouched source plane, entry by entry in list order:
//! `out = mix(prev, filter(prev), opacity * entry_mask * stack_mask)`, mixed premultiplied;
//! a mix weight of exactly 0 keeps `prev`'s bytes and exactly 1 takes the filter's bytes.
//!
//! Gaps (recorded, not faked): the Folio `live_filter` layer record (schema id
//! `hsk.studio.live_filter@1` exists in accord; folio refuses the kind today) is a folio-owner
//! task; the plan envelope does not lower a masked stack, so backends do not run this module yet.
//! Blend modes per entry are not implemented (normal only). Photoshop parity UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use crate::filter_math::{FilterError, FilterLimits, LayerFilter, MAX_FILTERS_PER_LAYER, filter_plane};
use crate::liquify::{LiquifyError, LiquifyLimits, LiquifyMesh};
use crate::retouch::RgbaPlane;
use crate::selection::{ColourView, CoveragePlane};
use crate::transform_math::ResampleFilter;
use crate::{Error, MAX_BYTES};
use std::fmt;

/// Typed failure of a stack evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackError {
    Pigment(Error),
    Filter(FilterError),
    Liquify(LiquifyError),
    /// More entries than [`StackLimits::max_entries`].
    TooManyEntries,
    /// Opacity non-finite or outside `[0, 1]`.
    Opacity,
}
impl StackError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Pigment(e) => e.code(),
            Self::Filter(e) => e.code(),
            Self::Liquify(e) => e.code(),
            Self::TooManyEntries => "smart_filter_too_many_entries",
            Self::Opacity => "smart_filter_opacity",
        }
    }
}
impl From<Error> for StackError {
    fn from(e: Error) -> Self {
        Self::Pigment(e)
    }
}
impl From<FilterError> for StackError {
    fn from(e: FilterError) -> Self {
        Self::Filter(e)
    }
}
impl From<LiquifyError> for StackError {
    fn from(e: LiquifyError) -> Self {
        Self::Liquify(e)
    }
}
impl fmt::Display for StackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for StackError {}

/// Budgets checked before work (V4-02).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StackLimits {
    pub max_entries: usize,
    pub filter: FilterLimits,
    pub liquify: LiquifyLimits,
}
impl Default for StackLimits {
    fn default() -> Self {
        Self {
            max_entries: MAX_FILTERS_PER_LAYER,
            filter: FilterLimits {
                max_bytes: 16 * MAX_BYTES,
                max_work: 1 << 34,
            },
            liquify: LiquifyLimits::default(),
        }
    }
}

/// What one stack entry does.
#[derive(Clone, Debug, PartialEq)]
pub enum StackFilter {
    /// A core pixel filter of [`crate::filter_math`].
    Filter(LayerFilter),
    /// A liquify mesh (its bounds must equal the source plane's).
    Liquify { mesh: LiquifyMesh, resample: ResampleFilter },
}

/// One re-editable stack entry.
#[derive(Clone, Debug, PartialEq)]
pub struct StackEntry {
    pub filter: StackFilter,
    pub enabled: bool,
    /// `[0, 1]`.
    pub opacity: f32,
    /// Per-filter coverage mask in plan space (zero outside its bounds); `None` = everywhere.
    pub mask: Option<CoveragePlane>,
}
impl StackEntry {
    pub fn new(filter: StackFilter) -> Self {
        Self {
            filter,
            enabled: true,
            opacity: 1.0,
            mask: None,
        }
    }
}

/// A layer's smart-filter stack: entries bottom (first applied) to top, plus the stack mask.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SmartFilterStack {
    pub entries: Vec<StackEntry>,
    pub stack_mask: Option<CoveragePlane>,
}

fn coverage(mask: Option<&CoveragePlane>, x: i64, y: i64) -> f64 {
    mask.map_or(1.0, |m| f64::from(m.at(x, y)) / 65535.0)
}

fn to_view_bytes(plane: &RgbaPlane) -> (Vec<u8>, Vec<u8>) {
    let mut colour = Vec::with_capacity(plane.data.len().saturating_mul(12));
    let mut alpha = Vec::with_capacity(plane.data.len().saturating_mul(4));
    for p in &plane.data {
        for c in &p[..3] {
            colour.extend_from_slice(&c.to_le_bytes());
        }
        alpha.extend_from_slice(&p[3].to_le_bytes());
    }
    (colour, alpha)
}

fn from_view_bytes(colour: &[u8], alpha: &[u8]) -> Vec<[f32; 4]> {
    let f = |b: &[u8], i: usize| -> f32 {
        let at = i.saturating_mul(4);
        b.get(at..at.saturating_add(4))
            .and_then(|s| s.try_into().ok())
            .map(f32::from_le_bytes)
            .unwrap_or(0.0)
    };
    (0..alpha.len().checked_div(4).unwrap_or(0))
        .map(|i| {
            let c = i.saturating_mul(3);
            [f(colour, c), f(colour, c.saturating_add(1)), f(colour, c.saturating_add(2)), f(alpha, i)]
        })
        .collect()
}

/// Premultiplied mix of two straight samples at weight `t` in `[0, 1]`.
fn mix(a: [f32; 4], b: [f32; 4], t: f64) -> [f32; 4] {
    if t <= 0.0 {
        return a;
    }
    if t >= 1.0 {
        return b;
    }
    let (aa, ba) = (f64::from(a[3]), f64::from(b[3]));
    let alpha = aa + (ba - aa) * t;
    if alpha <= 0.0 {
        return [0.0; 4];
    }
    let c = |k: usize| {
        let p = f64::from(a[k]) * aa + (f64::from(b[k]) * ba - f64::from(a[k]) * aa) * t;
        (p / alpha).clamp(0.0, 1.0) as f32
    };
    [c(0), c(1), c(2), alpha.clamp(0.0, 1.0) as f32]
}

impl SmartFilterStack {
    /// Re-render the stack over `source` (never modified). Every entry's parameters are checked
    /// before any filter runs.
    pub fn render(&self, source: &RgbaPlane, limits: &StackLimits) -> Result<RgbaPlane, StackError> {
        if self.entries.len() > limits.max_entries {
            return Err(StackError::TooManyEntries);
        }
        source.validate().map_err(|_| Error::InvalidInput)?;
        for e in &self.entries {
            if !e.opacity.is_finite() || !(0.0..=1.0).contains(&e.opacity) {
                return Err(StackError::Opacity);
            }
            match &e.filter {
                StackFilter::Filter(f) => f.validate()?,
                StackFilter::Liquify { mesh, .. } => {
                    mesh.validate(&limits.liquify)?;
                    if mesh.bounds != source.bounds {
                        return Err(Error::InvalidInput.into());
                    }
                }
            }
        }
        let mut cur = source.clone();
        for e in self.entries.iter().filter(|e| e.enabled && e.opacity > 0.0) {
            let filtered = match &e.filter {
                StackFilter::Filter(f) => filter_plane(&cur, f, limits.filter)?,
                StackFilter::Liquify { mesh, resample } => {
                    let (colour, alpha) = to_view_bytes(&cur);
                    let view = ColourView { bounds: cur.bounds, colour: &colour, alpha: &alpha };
                    let (c, a) = mesh.render(&view, *resample, &limits.liquify)?;
                    RgbaPlane { bounds: cur.bounds, data: from_view_bytes(&c, &a) }
                }
            };
            let b = cur.bounds;
            let w = (b.width as usize).max(1);
            let opacity = f64::from(e.opacity);
            let data = cur
                .data
                .iter()
                .zip(&filtered.data)
                .enumerate()
                .map(|(i, (p, q))| {
                    let x = b.x.saturating_add(i.checked_rem(w).unwrap_or(0) as i64);
                    let y = b.y.saturating_add(i.checked_div(w).unwrap_or(0) as i64);
                    let t = opacity * coverage(e.mask.as_ref(), x, y) * coverage(self.stack_mask.as_ref(), x, y);
                    mix(*p, *q, t)
                })
                .collect();
            cur = RgbaPlane { bounds: b, data };
        }
        Ok(cur)
    }
}

/// Stack surface description (Studio readings; Photoshop parity NOT_PROVEN, D5).
pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PIGMENT","surface":"smart_filter","version":1,"lane":"SMB-PS-SELECT-LIQUIFY","spec_basis":["STU-RAS-037","STU-FX-011a","STU-FX-024"],"stack":"entries bottom-to-top: filter_math LayerFilter or liquify mesh; enabled, opacity [0,1], per-entry coverage mask, stack mask; re-rendered from the untouched source","mix":"premultiplied lerp(prev, filtered, opacity * mask * stack_mask); weight 0 keeps prev bytes, 1 takes filtered bytes","limits":{"max_entries":16},"gaps":["Folio live_filter record (folio owner; accord schema hsk.studio.live_filter@1)","plan envelope lowering of masked stacks (render-cpu/gpu)","per-entry blend modes"],"parity":"NOT_PROVEN"}"#;
