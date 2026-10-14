//! Motion adapter (feature `motion`, lead decision T1): per-layer motion `Property` streams sampled
//! at the exact tick with `Property::sample` (keyframes only; expressions are not evaluated here).
//! Motion owns keyframe evaluation (STU-CMP-002); this adapter only maps sampled values onto
//! [`LayerFrameState`]. The only motion-aware code in composite.
#![deny(clippy::arithmetic_side_effects)]

use super::{LayerFrameState, TemporalError, TemporalSource};
use hsk_studio_accord::CancellationToken;
use hsk_studio_motion::{EvalTick, MotionError, Property};
use std::collections::BTreeMap;

/// The transform-group streams of one layer (STU-MOT-013). Absent = identity.
#[derive(Clone, Debug, Default)]
pub struct LayerProperties {
    /// 1 component, percent `0..=100`; multiplies the authored opacity.
    pub opacity: Option<Property>,
    /// 2 components, plan pixels: offset from the authored placement.
    pub position: Option<Property>,
    /// 2 components, percent (100 = unscaled).
    pub scale: Option<Property>,
    /// 1 component, degrees (stored unwrapped).
    pub rotation: Option<Property>,
}

/// [`TemporalSource`] over motion properties keyed by Folio layer id.
#[derive(Clone, Debug, Default)]
pub struct MotionTemporalSource {
    layers: BTreeMap<String, LayerProperties>,
}

impl MotionTemporalSource {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_layer(mut self, layer_id: &str, properties: LayerProperties) -> Self {
        self.layers.insert(layer_id.to_owned(), properties);
        self
    }
}

pub(super) fn sample(
    layer_id: &str,
    property: Option<&Property>,
    components: usize,
    tick: i64,
    cancel: &CancellationToken,
) -> Result<Option<Vec<f64>>, TemporalError> {
    let Some(p) = property else {
        return Ok(None);
    };
    let source_error = |e: MotionError| TemporalError::Source {
        layer_id: layer_id.to_owned(),
        code: e.code().to_owned(),
    };
    if p.dimension() != components {
        return Err(source_error(MotionError::ComponentMismatch));
    }
    p.sample(EvalTick::new(tick), cancel)
        .map(Some)
        .map_err(|e| match e {
            MotionError::Canceled => TemporalError::Composite(crate::CompositeError::Canceled),
            other => source_error(other),
        })
}

impl TemporalSource for MotionTemporalSource {
    fn layer_state(
        &self,
        layer_id: &str,
        tick: i64,
        cancel: &CancellationToken,
    ) -> Result<Option<LayerFrameState>, TemporalError> {
        let Some(p) = self.layers.get(layer_id) else {
            return Ok(None);
        };
        let mut state = LayerFrameState::IDENTITY;
        if let Some(v) = sample(layer_id, p.opacity.as_ref(), 1, tick, cancel)? {
            state.opacity_scale = v[0] / 100.0;
        }
        if let Some(v) = sample(layer_id, p.position.as_ref(), 2, tick, cancel)? {
            state.translate_px = [v[0], v[1]];
        }
        if let Some(v) = sample(layer_id, p.scale.as_ref(), 2, tick, cancel)? {
            state.scale = [v[0] / 100.0, v[1] / 100.0];
        }
        if let Some(v) = sample(layer_id, p.rotation.as_ref(), 1, tick, cancel)? {
            state.rotation_deg = v[0];
        }
        Ok(Some(state))
    }
}
