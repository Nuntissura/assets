//! Convert audio to keyframes (lane SMB-AE-PARTICLES-AUDIO-PRESETS slice 2; STU-MOT-016 audio
//! levels on the ordinary keyframe model, STU-MOT-017 an effect instance is a property group whose
//! parameters are ordinary properties).
//!
//! A per-frame amplitude series of a composition mix (pulse `audio_amplitude`: RMS x 100 of the left
//! and right sides and their mean) becomes three slider properties of an amplitude layer,
//! `<layer>/effects/left_channel/slider`, `<layer>/effects/right_channel/slider` and
//! `<layer>/effects/both_channels/slider`, each with one LINEAR key per frame at
//! `first_tick + f * ticks_per_frame`. They are ordinary store properties, so an expression reads
//! them through the store's `HostRead` like any other property (declared in its read set). The
//! insertion is all-or-nothing: every path is checked free and every property built before the
//! first insert. After Effects' effect and layer naming ("Audio Amplitude", "Both Channels") is the
//! import key, not the Studio key; parity UNVERIFIED (D5).
use crate::depgraph::MAX_GRAPH_NODES;
use crate::error::MotionError;
use crate::keyframe::{Keyframe, MAX_KEYFRAMES};
use crate::path::PropertyPath;
use crate::property::Property;
use crate::store::PropertyStore;
use hsk_studio_accord::Ticks;

/// The three effect-instance keys of an amplitude layer, in `[left, right, both]` order.
pub const AMPLITUDE_CHANNELS: [&str; 3] = ["left_channel", "right_channel", "both_channels"];
/// The slider parameter key of each instance.
pub const AMPLITUDE_SLIDER: &str = "slider";

/// One slider property: a LINEAR key per frame (`values[f]` at `first_tick + f tpf`), static value
/// the first value. Refusals: empty or more than `MAX_KEYFRAMES` values (`KeyframeLimit`), a
/// non-finite or negative value (`NonFinite` / `OutOfBounds`), `ticks_per_frame` 0 or a key time
/// past `u64` (`TickOverflow`).
pub fn amplitude_property(values: &[f64], first_tick: u64, ticks_per_frame: u64) -> Result<Property, MotionError> {
    if values.is_empty() || values.len() > MAX_KEYFRAMES {
        return Err(MotionError::KeyframeLimit);
    }
    if ticks_per_frame == 0 {
        return Err(MotionError::TickOverflow);
    }
    let mut keys = Vec::with_capacity(values.len());
    for (f, v) in values.iter().enumerate() {
        if !v.is_finite() {
            return Err(MotionError::NonFinite);
        }
        if *v < 0.0 {
            return Err(MotionError::OutOfBounds);
        }
        let t = (f as u64)
            .checked_mul(ticks_per_frame)
            .and_then(|o| o.checked_add(first_tick))
            .ok_or(MotionError::TickOverflow)?;
        keys.push(Keyframe::new(Ticks::new(t), vec![*v]));
    }
    Property::new(AMPLITUDE_SLIDER, vec![values[0]], keys)
}

/// Inserts the three slider properties under `layer` (a property path prefix such as
/// `comp/audio_amplitude`) and returns their paths in `[left, right, both]` order. The three series
/// must have equal lengths (`ComponentMismatch`); an occupied path is `DuplicateProperty` and the
/// store is unchanged on every refusal.
pub fn insert_audio_amplitude(
    store: &mut PropertyStore,
    layer: &str,
    series: [&[f64]; 3],
    first_tick: u64,
    ticks_per_frame: u64,
) -> Result<[PropertyPath; 3], MotionError> {
    if series[1].len() != series[0].len() || series[2].len() != series[0].len() {
        return Err(MotionError::ComponentMismatch);
    }
    let mut built = Vec::with_capacity(3);
    for (channel, values) in AMPLITUDE_CHANNELS.iter().zip(series) {
        let path = PropertyPath::new(&format!("{layer}/effects/{channel}/{AMPLITUDE_SLIDER}"))?;
        if store.get(&path).is_some() {
            return Err(MotionError::DuplicateProperty);
        }
        built.push((path, amplitude_property(values, first_tick, ticks_per_frame)?));
    }
    if store.len().saturating_add(built.len()) > MAX_GRAPH_NODES {
        return Err(MotionError::NodeLimit);
    }
    let paths = [built[0].0.clone(), built[1].0.clone(), built[2].0.clone()];
    for (path, property) in built {
        store.insert(path, property)?;
    }
    Ok(paths)
}
