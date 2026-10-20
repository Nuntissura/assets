//! Waveform editor (lane SMB-AU-WAVEFORM-EDITOR, Audition parity row AU-20; feature `audio-batch`).
//! A sample editor over one source audio buffer bound to the open document: sample-accurate
//! selection (with optional snap to zero crossings), cut / copy / paste (insert, overwrite, mix) /
//! delete / silence / insert silence, fades with score's curve shapes, a gain envelope, amplify,
//! normalise, invert and reverse, all computed by score's [`WaveBuffer`].
//!
//! **Non-destructive storage, one undo step per edit.** The source samples are never rewritten.
//! The document keeps an edit list in the session entry [`WAVEFORM_EDIT_ENTRY`] (schema
//! [`WAVEFORM_SCHEMA`]: the source identity and the ordered operations, no samples), written
//! through `Run::set_tiles_and_entry` as an entry-only history step, so every edit is exactly one
//! undo step of the run's history (the path the mixer's routing edits use), saved and reopened
//! with the package. The edited audio is the source with the list replayed (deterministic; the
//! viewer caches the latest result and replays after undo / redo / reopen). A paste stores a
//! reference to the copied range: `clip.after_ops = n` names the audio after the first `n`
//! operations, so replay re-derives the clip; a clipboard whose operation prefix no longer matches
//! the list (its digest is kept) is refused as `waveform_clipboard_stale`. Copy and selection are
//! not edits (no step).
//!
//! **Hostile entries.** A reopened entry is untrusted bytes: bounded by
//! [`MAX_WAVEFORM_ENTRY_BYTES`] and [`MAX_WAVEFORM_OPS`] before parsing, every field typed and
//! range-checked, unknown operations refused, a paste reference must name an earlier prefix, and a
//! list recorded over another source (identity mismatch) is refused rather than replayed.
//!
//! **Spectral edits** (slice 3): [`WaveformEdit::Spectral`] applies score's `spectral_mask_edit`
//! (marquee or lasso time-frequency region; identity, attenuate, remove or heal; inverse STFT with
//! weighted overlap-add) to the masked channels as one more operation of the list, so it is one
//! undo step and replays like every other edit (see `super::spectral_display` for the selection on
//! the spectral display).
//!
//! **Effects on a selection** (slice 4): [`WaveformEdit::Effect`] runs one of score's effects (a
//! Folio effect instance read by the batch module's `EffectSpec`: gain, equalizer, dynamics,
//! lookahead limiter; keyframes refused) over the selection of one or two channels through a score
//! `Chain` bounced by `Engine::bounce` (latency compensated, so the processed selection is aligned
//! sample for sample) and writes it back over the selection: one operation, one undo step, replayed
//! like the others. The effect starts from its reset state at the selection start (no pre-roll).
//!
//! **Hosted plugins on a selection** (slice 4b, parity row AU-19): [`WaveformEdit::Plugin`] names an
//! approved plugin by its content SHA-256 plus parameter values; the plugin must first be registered
//! with the editor ([`super::Viewer::register_waveform_plugin`], feature `audio-plugin`: score-plugin's
//! process-isolated host, CLAP through its worker backend, VST3 / other formats refused). It is run over
//! the selection like an effect (one operation, one undo step). Replay needs the same plugin
//! registered: otherwise `waveform_plugin_unregistered`; builds without the feature refuse every
//! plugin operation with `waveform_plugin_hosting_unavailable` (the list still parses and is kept).
//!
//! Panel (egui, window-free): `waveform.panel` with the canvas `waveform.canvas` (drag selects,
//! one lane per channel, min/max per column) and the buttons of [`widget`]. Audition interaction
//! parity is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]

use super::audio_batch::EffectSpec;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio as folio;
use hsk_studio_score::{
    self as score, ALL_CHANNELS, FadeCurve, FadeDirection, PasteMode, ScoreError, SnapDirection, SpectralMaskOp, SpectralRegion,
    SpectralWindow, WaveBuffer, WaveClip, WaveEditReport, WaveSelection,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Session document entry holding the edit list (JSON).
pub const WAVEFORM_EDIT_ENTRY: &str = "waveform_edit";
pub const WAVEFORM_SCHEMA: &str = "hsk.studio.waveform_edit@1";
/// Most operations in one list.
pub const MAX_WAVEFORM_OPS: usize = 4096;
/// Largest entry read (bytes).
pub const MAX_WAVEFORM_ENTRY_BYTES: usize = 4 << 20;
/// Samples per hashed chunk of the source identity.
const HASH_CHUNK: usize = 1 << 16;

/// Widget names of the waveform panel.
pub mod widget {
    pub const PANEL: &str = "waveform.panel";
    pub const CANVAS: &str = "waveform.canvas";
    pub const OPEN: &str = "waveform.open";
    pub const CUT: &str = "waveform.cut";
    pub const COPY: &str = "waveform.copy";
    pub const PASTE: &str = "waveform.paste";
    pub const PASTE_MIX: &str = "waveform.paste_mix";
    pub const DELETE: &str = "waveform.delete";
    pub const SILENCE: &str = "waveform.silence";
    pub const INSERT_SILENCE: &str = "waveform.insert_silence";
    pub const FADE_IN: &str = "waveform.fade_in";
    pub const FADE_OUT: &str = "waveform.fade_out";
    pub const FADE_CURVE: &str = "waveform.fade_curve";
    pub const AMPLIFY: &str = "waveform.amplify";
    pub const AMPLIFY_DB: &str = "waveform.amplify_db";
    pub const NORMALIZE: &str = "waveform.normalize";
    pub const NORMALIZE_DBFS: &str = "waveform.normalize_dbfs";
    pub const INVERT: &str = "waveform.invert";
    pub const REVERSE: &str = "waveform.reverse";
    pub const SNAP: &str = "waveform.snap";
    pub const SELECT_ALL: &str = "waveform.select_all";
    pub const UNDO: &str = "waveform.undo";
    pub const REDO: &str = "waveform.redo";
}

/// A requested waveform edit (UI and API share it).
#[derive(Clone, Debug, PartialEq)]
pub enum WaveformEdit {
    Delete { start: u64, end: u64 },
    /// Copies `start .. end` of every channel to the clipboard, then deletes it (one step).
    Cut { start: u64, end: u64 },
    /// Clipboard only (no step).
    Copy(WaveSelection),
    Paste { at: u64, mode: PasteMode, channels: u32 },
    Silence(WaveSelection),
    InsertSilence { at: u64, frames: u64 },
    Fade { selection: WaveSelection, direction: FadeDirection, curve: FadeCurve },
    GainEnvelope { selection: WaveSelection, points: Vec<(u64, f64)> },
    Amplify { selection: WaveSelection, db: f64 },
    Normalize { selection: WaveSelection, target_dbfs: f64, linked: bool },
    Invert(WaveSelection),
    Reverse(WaveSelection),
    /// A spectral selection edit of the masked channels (slice 3).
    Spectral(SpectralEdit),
    /// A score effect over the selection (slice 4).
    Effect { selection: WaveSelection, effect: folio::StudioAudioEffectInstance },
    /// A registered hosted plugin over the selection (slice 4b).
    Plugin(PluginEdit),
}

/// A hosted plugin run over `selection` (1 or 2 channels): the plugin approved with SHA-256
/// `content_sha256` (lower-case hex), its parameters `(id, value)` set before the first block.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginEdit {
    pub selection: WaveSelection,
    pub content_sha256: String,
    pub parameters: Vec<(u32, f64)>,
}

/// Most plugin parameters of one operation.
pub const MAX_PLUGIN_PARAMETERS: usize = 256;

/// Runs hosted plugins for the edit list (the editor's registry).
pub trait PluginHost {
    fn run(&self, buffer: &mut WaveBuffer, edit: &PluginEdit, cancel: &CancellationToken) -> Result<WaveEditReport, String>;
}

/// The registry of a build without hosting: every plugin operation is refused.
#[cfg(not(feature = "audio-plugin"))]
#[derive(Default)]
pub struct PluginRegistry {
    _hosting_unavailable: (),
}

#[cfg(not(feature = "audio-plugin"))]
impl PluginHost for PluginRegistry {
    fn run(&self, _: &mut WaveBuffer, _: &PluginEdit, _: &CancellationToken) -> Result<WaveEditReport, String> {
        Err("waveform_plugin_hosting_unavailable: built without feature audio-plugin".into())
    }
}

#[cfg(feature = "audio-plugin")]
pub use super::waveform_plugin::PluginRegistry;

/// A spectral selection edit: `region` of the channels in `channels`, analysed with `window` at
/// `fft_size` (hop `fft_size / 4`).
#[derive(Clone, Debug, PartialEq)]
pub struct SpectralEdit {
    pub channels: u32,
    pub region: SpectralRegion,
    pub op: SpectralMaskOp,
    pub fft_size: usize,
    pub window: SpectralWindow,
}

/// A stored operation of the edit list.
#[derive(Clone, Debug, PartialEq)]
pub enum WaveformOp {
    Delete { start: u64, end: u64 },
    Paste { at: u64, mode: PasteMode, channels: u32, clip: ClipRef },
    Silence(WaveSelection),
    InsertSilence { at: u64, frames: u64 },
    Fade { selection: WaveSelection, direction: FadeDirection, curve: FadeCurve },
    GainEnvelope { selection: WaveSelection, points: Vec<(u64, f64)> },
    Amplify { selection: WaveSelection, db: f64 },
    Normalize { selection: WaveSelection, target_dbfs: f64, linked: bool },
    Invert(WaveSelection),
    Reverse(WaveSelection),
    Spectral(SpectralEdit),
    Effect { selection: WaveSelection, effect: folio::StudioAudioEffectInstance },
    Plugin(PluginEdit),
}

/// The copied range of the audio after the first `after_ops` operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipRef {
    pub after_ops: usize,
    pub selection: WaveSelection,
}

/// Identity of the source audio: rate, channels, frames and a SHA-256 over the per-chunk SHA-256 of
/// the little-endian f32 samples (chunks of 65536 samples per channel), lower-case hex.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceIdentity {
    pub sha256: String,
    pub sample_rate_hz: u32,
    pub channels: usize,
    pub frames: u64,
}

impl SourceIdentity {
    pub fn of(buffer: &WaveBuffer) -> Self {
        let mut acc = Vec::new();
        acc.extend_from_slice(&buffer.sample_rate_hz().to_le_bytes());
        acc.extend_from_slice(&(buffer.channel_count() as u64).to_le_bytes());
        acc.extend_from_slice(&buffer.frames().to_le_bytes());
        let mut bytes = Vec::with_capacity(HASH_CHUNK.saturating_mul(4));
        for channel in buffer.channels() {
            for chunk in channel.chunks(HASH_CHUNK) {
                bytes.clear();
                for v in chunk {
                    bytes.extend_from_slice(&v.to_bits().to_le_bytes());
                }
                acc.extend_from_slice(&score::sha256(&bytes));
            }
        }
        Self { sha256: score::to_hex(&score::sha256(&acc)), sample_rate_hz: buffer.sample_rate_hz(), channels: buffer.channel_count(), frames: buffer.frames() }
    }
    fn to_json(&self) -> Value {
        json!({"sha256": self.sha256, "sample_rate_hz": self.sample_rate_hz, "channels": self.channels, "frames": self.frames})
    }
}

/// A stored edit list.
#[derive(Clone, Debug, PartialEq)]
pub struct WaveformDocument {
    pub source: SourceIdentity,
    pub ops: Vec<WaveformOp>,
}

// ----- JSON form ------------------------------------------------------------------------------

fn curve_name(c: FadeCurve) -> &'static str {
    match c {
        FadeCurve::Linear => "linear",
        FadeCurve::EqualPower => "equal_power",
        FadeCurve::SCurve => "s_curve",
        FadeCurve::Logarithmic => "logarithmic",
    }
}
fn curve_of(s: &str) -> Option<FadeCurve> {
    Some(match s {
        "linear" => FadeCurve::Linear,
        "equal_power" => FadeCurve::EqualPower,
        "s_curve" => FadeCurve::SCurve,
        "logarithmic" => FadeCurve::Logarithmic,
        _ => return None,
    })
}
fn mode_name(m: PasteMode) -> &'static str {
    match m {
        PasteMode::Insert => "insert",
        PasteMode::Overwrite => "overwrite",
        PasteMode::Mix => "mix",
    }
}
fn mode_of(s: &str) -> Option<PasteMode> {
    Some(match s {
        "insert" => PasteMode::Insert,
        "overwrite" => PasteMode::Overwrite,
        "mix" => PasteMode::Mix,
        _ => return None,
    })
}
fn sel_json(s: &WaveSelection) -> Value {
    json!({"start": s.start, "end": s.end, "channels": s.channels})
}

impl WaveformOp {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Delete { .. } => "delete",
            Self::Paste { .. } => "paste",
            Self::Silence(_) => "silence",
            Self::InsertSilence { .. } => "insert_silence",
            Self::Fade { .. } => "fade",
            Self::GainEnvelope { .. } => "gain_envelope",
            Self::Amplify { .. } => "amplify",
            Self::Normalize { .. } => "normalize",
            Self::Invert(_) => "invert",
            Self::Reverse(_) => "reverse",
            Self::Spectral(_) => "spectral",
            Self::Effect { .. } => "effect",
            Self::Plugin(_) => "plugin",
        }
    }
    pub fn to_json(&self) -> Value {
        let mut v = match self {
            Self::Delete { start, end } => json!({"start": start, "end": end}),
            Self::Paste { at, mode, channels, clip } => json!({"at": at, "mode": mode_name(*mode), "channels": channels, "clip": {"after_ops": clip.after_ops, "selection": sel_json(&clip.selection)}}),
            Self::Silence(s) | Self::Invert(s) | Self::Reverse(s) => json!({"selection": sel_json(s)}),
            Self::InsertSilence { at, frames } => json!({"at": at, "frames": frames}),
            Self::Fade { selection, direction, curve } => json!({"selection": sel_json(selection), "direction": if *direction == FadeDirection::In { "in" } else { "out" }, "curve": curve_name(*curve)}),
            Self::GainEnvelope { selection, points } => json!({"selection": sel_json(selection), "points": points.iter().map(|(o, d)| json!([o, d])).collect::<Vec<_>>()}),
            Self::Amplify { selection, db } => json!({"selection": sel_json(selection), "db": db}),
            Self::Normalize { selection, target_dbfs, linked } => json!({"selection": sel_json(selection), "target_dbfs": target_dbfs, "linked": linked}),
            Self::Spectral(e) => json!({
                "channels": e.channels, "fft_size": e.fft_size, "window": e.window.name(),
                "region": match &e.region {
                    SpectralRegion::Marquee { start_sample, end_sample, low_hz, high_hz } => json!({"marquee": {"start": start_sample, "end": end_sample, "low_hz": low_hz, "high_hz": high_hz}}),
                    SpectralRegion::Lasso(points) => json!({"lasso": points.iter().map(|(t, f)| json!([t, f])).collect::<Vec<_>>()}),
                },
                "edit": match e.op {
                    SpectralMaskOp::Identity => json!("identity"),
                    SpectralMaskOp::Remove => json!("remove"),
                    SpectralMaskOp::Heal => json!("heal"),
                    SpectralMaskOp::Attenuate { gain_db } => json!({"attenuate_db": gain_db}),
                },
            }),
            Self::Effect { selection, effect } => json!({"selection": sel_json(selection), "effect": serde_json::to_value(effect).unwrap_or(Value::Null)}),
            Self::Plugin(p) => json!({"selection": sel_json(&p.selection), "content_sha256": p.content_sha256, "parameters": p.parameters.iter().map(|(i, v)| json!([i, v])).collect::<Vec<_>>()}),
        };
        v["op"] = Value::from(self.name());
        v
    }

    /// Parses one stored operation (index `index` of the list). Every field is required and typed.
    pub fn from_json(v: &Value, index: usize) -> Result<Self, String> {
        let bad = |what: &str| format!("waveform_entry_invalid: op {index}: {what}");
        let u = |k: &str| v.get(k).and_then(Value::as_u64).ok_or_else(|| bad(k));
        let f = |k: &str| v.get(k).and_then(Value::as_f64).filter(|x| x.is_finite()).ok_or_else(|| bad(k));
        let sel_of = |s: Option<&Value>| -> Result<WaveSelection, String> {
            let s = s.ok_or_else(|| bad("selection"))?;
            let n = |k: &str| s.get(k).and_then(Value::as_u64).ok_or_else(|| bad(k));
            let channels = u32::try_from(n("channels")?).map_err(|_| bad("channels"))?;
            Ok(WaveSelection { start: n("start")?, end: n("end")?, channels })
        };
        let sel = || sel_of(v.get("selection"));
        let op = v.get("op").and_then(Value::as_str).ok_or_else(|| bad("op"))?;
        Ok(match op {
            "delete" => Self::Delete { start: u("start")?, end: u("end")? },
            "paste" => {
                let clip = v.get("clip").ok_or_else(|| bad("clip"))?;
                let after_ops = clip.get("after_ops").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad("after_ops"))?;
                if after_ops > index {
                    return Err(bad("clip names a later prefix"));
                }
                Self::Paste {
                    at: u("at")?,
                    mode: v.get("mode").and_then(Value::as_str).and_then(mode_of).ok_or_else(|| bad("mode"))?,
                    channels: u32::try_from(u("channels")?).map_err(|_| bad("channels"))?,
                    clip: ClipRef { after_ops, selection: sel_of(clip.get("selection"))? },
                }
            }
            "silence" => Self::Silence(sel()?),
            "invert" => Self::Invert(sel()?),
            "reverse" => Self::Reverse(sel()?),
            "insert_silence" => Self::InsertSilence { at: u("at")?, frames: u("frames")? },
            "fade" => Self::Fade {
                selection: sel()?,
                direction: match v.get("direction").and_then(Value::as_str) {
                    Some("in") => FadeDirection::In,
                    Some("out") => FadeDirection::Out,
                    _ => return Err(bad("direction")),
                },
                curve: v.get("curve").and_then(Value::as_str).and_then(curve_of).ok_or_else(|| bad("curve"))?,
            },
            "gain_envelope" => {
                let list = v.get("points").and_then(Value::as_array).ok_or_else(|| bad("points"))?;
                if list.len() > score::MAX_WAVE_ENVELOPE_POINTS {
                    return Err(bad("points"));
                }
                let mut points = Vec::with_capacity(list.len());
                for p in list {
                    let o = p.get(0).and_then(Value::as_u64).ok_or_else(|| bad("point offset"))?;
                    let d = p.get(1).and_then(Value::as_f64).filter(|x| x.is_finite()).ok_or_else(|| bad("point db"))?;
                    points.push((o, d));
                }
                Self::GainEnvelope { selection: sel()?, points }
            }
            "amplify" => Self::Amplify { selection: sel()?, db: f("db")? },
            "normalize" => Self::Normalize {
                selection: sel()?,
                target_dbfs: f("target_dbfs")?,
                linked: v.get("linked").and_then(Value::as_bool).ok_or_else(|| bad("linked"))?,
            },
            "spectral" => {
                let window = v.get("window").and_then(Value::as_str).and_then(window_of).ok_or_else(|| bad("window"))?;
                let fft_size = v.get("fft_size").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad("fft_size"))?;
                let r = v.get("region").ok_or_else(|| bad("region"))?;
                let region = if let Some(m) = r.get("marquee") {
                    let n = |k: &str| m.get(k).and_then(Value::as_u64).ok_or_else(|| bad(k));
                    let hz = |k: &str| m.get(k).and_then(Value::as_f64).filter(|x| x.is_finite()).ok_or_else(|| bad(k));
                    SpectralRegion::Marquee { start_sample: n("start")?, end_sample: n("end")?, low_hz: hz("low_hz")?, high_hz: hz("high_hz")? }
                } else {
                    let list = r.get("lasso").and_then(Value::as_array).ok_or_else(|| bad("region"))?;
                    if list.len() > score::MAX_LASSO_POINTS {
                        return Err(bad("lasso"));
                    }
                    let mut points = Vec::with_capacity(list.len());
                    for p in list {
                        let c = |i: usize| p.get(i).and_then(Value::as_f64).filter(|x| x.is_finite()).ok_or_else(|| bad("lasso point"));
                        points.push((c(0)?, c(1)?));
                    }
                    SpectralRegion::Lasso(points)
                };
                let op = match v.get("edit") {
                    Some(Value::String(e)) if e == "identity" => SpectralMaskOp::Identity,
                    Some(Value::String(e)) if e == "remove" => SpectralMaskOp::Remove,
                    Some(Value::String(e)) if e == "heal" => SpectralMaskOp::Heal,
                    Some(e) => SpectralMaskOp::Attenuate { gain_db: e.get("attenuate_db").and_then(Value::as_f64).filter(|x| x.is_finite()).ok_or_else(|| bad("edit"))? },
                    None => return Err(bad("edit")),
                };
                Self::Spectral(SpectralEdit { channels: u32::try_from(u("channels")?).map_err(|_| bad("channels"))?, region, op, fft_size, window })
            }
            "effect" => {
                let effect: folio::StudioAudioEffectInstance = serde_json::from_value(v.get("effect").cloned().ok_or_else(|| bad("effect"))?).map_err(|_| bad("effect"))?;
                effect_spec(&effect).map_err(|e| bad(&e))?;
                Self::Effect { selection: sel()?, effect }
            }
            "plugin" => {
                let content_sha256 = v
                    .get("content_sha256")
                    .and_then(Value::as_str)
                    .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
                    .ok_or_else(|| bad("content_sha256"))?
                    .to_owned();
                let list = v.get("parameters").and_then(Value::as_array).ok_or_else(|| bad("parameters"))?;
                if list.len() > MAX_PLUGIN_PARAMETERS {
                    return Err(bad("parameters"));
                }
                let mut parameters = Vec::with_capacity(list.len());
                for p in list {
                    let id = p.get(0).and_then(Value::as_u64).and_then(|i| u32::try_from(i).ok()).ok_or_else(|| bad("parameter id"))?;
                    let value = p.get(1).and_then(Value::as_f64).filter(|x| x.is_finite()).ok_or_else(|| bad("parameter value"))?;
                    parameters.push((id, value));
                }
                Self::Plugin(PluginEdit { selection: sel()?, content_sha256, parameters })
            }
            _ => return Err(bad("unknown op")),
        })
    }
}

impl WaveformDocument {
    pub fn to_bytes(&self) -> Vec<u8> {
        let v = json!({"schema": WAVEFORM_SCHEMA, "source": self.source.to_json(), "ops": self.ops.iter().map(WaveformOp::to_json).collect::<Vec<_>>()});
        serde_json::to_vec(&v).unwrap_or_default()
    }

    /// Parses entry bytes (module docs: bounded, typed, unknown operations refused).
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_WAVEFORM_ENTRY_BYTES {
            return Err("waveform_entry_too_large".into());
        }
        let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("waveform_entry_invalid: {e}"))?;
        if v.get("schema").and_then(Value::as_str) != Some(WAVEFORM_SCHEMA) {
            return Err("waveform_entry_invalid: schema".into());
        }
        let s = v.get("source").ok_or("waveform_entry_invalid: source")?;
        let source = SourceIdentity {
            sha256: s.get("sha256").and_then(Value::as_str).filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).ok_or("waveform_entry_invalid: sha256")?.to_owned(),
            sample_rate_hz: s.get("sample_rate_hz").and_then(Value::as_u64).and_then(|r| u32::try_from(r).ok()).ok_or("waveform_entry_invalid: sample_rate_hz")?,
            channels: s.get("channels").and_then(Value::as_u64).and_then(|c| usize::try_from(c).ok()).ok_or("waveform_entry_invalid: channels")?,
            frames: s.get("frames").and_then(Value::as_u64).ok_or("waveform_entry_invalid: frames")?,
        };
        let list = v.get("ops").and_then(Value::as_array).ok_or("waveform_entry_invalid: ops")?;
        if list.len() > MAX_WAVEFORM_OPS {
            return Err("waveform_entry_too_many_ops".into());
        }
        let ops = list.iter().enumerate().map(|(i, o)| WaveformOp::from_json(o, i)).collect::<Result<Vec<_>, _>>()?;
        Ok(Self { source, ops })
    }
}

fn window_of(name: &str) -> Option<SpectralWindow> {
    [SpectralWindow::Hann, SpectralWindow::Hamming, SpectralWindow::Blackman, SpectralWindow::BlackmanHarris, SpectralWindow::Rectangular]
        .into_iter()
        .find(|w| w.name() == name)
}

/// The score effect of a stored instance (keyframes and unknown kinds refused).
fn effect_spec(effect: &folio::StudioAudioEffectInstance) -> Result<EffectSpec, String> {
    if !effect.keyframes.is_empty() {
        return Err("waveform_effect_keyframes_unsupported".into());
    }
    if effect.params.len() > 256 {
        return Err("waveform_effect_too_many_params".into());
    }
    EffectSpec::from_preset_instance(effect)
        .map_err(|e| format!("waveform_effect_invalid: {} {}", e.code, e.detail))?
        .ok_or_else(|| format!("waveform_effect_disabled: {}", effect.instance_id))
}

/// Block size of an effect bounce.
const EFFECT_BLOCK: usize = 4096;

/// Runs `spec` over `clip` (1 or 2 channels) at `rate` through a score chain bounce.
fn bounce_effect(clip: &[Vec<f32>], rate: u32, spec: &EffectSpec, cancel: &CancellationToken) -> Result<Vec<Vec<f32>>, String> {
    let channels = clip.len();
    let layout = match channels {
        1 => score::ChannelLayout::mono(score::STEREO_LABELS[0]),
        2 => score::ChannelLayout::stereo(),
        _ => return Err("waveform_effect_channels: effects run on one or two channels".into()),
    };
    let processor = spec.build(rate, channels).map_err(code)?;
    let stage = score::ChainStage { processor, output: layout.clone() };
    let mut chain = score::Chain::new(layout.clone(), EFFECT_BLOCK, vec![stage]).map_err(code)?;
    let resource = hsk_studio_accord::DomainId::parse(super::audio_import::IMPORT_CLIP_ID).map_err(|_| "waveform_effect: resource id".to_owned())?;
    let engine = score::Engine::new(resource.clone(), 1);
    let ctx = score::RequestContext { resource, actor: Some(crate::harness::default_actor()), expected_revision: 1 };
    let inputs: Vec<&[f32]> = clip.iter().map(Vec::as_slice).collect();
    let request = score::BounceRequest { sample_rate_hz: rate, layout: &layout, start_sample: 0, channels: &inputs, block_frames: EFFECT_BLOCK };
    let mut block = score::OutputBlock::with_capacity(layout.clone(), EFFECT_BLOCK).map_err(code)?;
    let frames = clip.first().map_or(0, Vec::len);
    let mut out: Vec<Vec<f32>> = (0..channels).map(|_| Vec::with_capacity(frames)).collect();
    let mut sink = |chunk: score::BounceChunk<'_>| {
        for (dst, src) in out.iter_mut().zip(chunk.channels) {
            dst.extend_from_slice(src);
        }
        Ok(())
    };
    engine.bounce(&ctx, &request, &mut chain, &mut block, &mut sink, cancel).map_err(code)?;
    if out.iter().any(|c| c.len() != frames) {
        return Err("waveform_effect_length".into());
    }
    Ok(out)
}

fn code(e: ScoreError) -> String {
    format!("waveform_{}", e.code())
}

/// Applies one operation (`clip`: the referenced audio of a paste).
pub fn apply_op(buffer: &mut WaveBuffer, op: &WaveformOp, clip: Option<&WaveClip>, plugins: &dyn PluginHost, cancel: &CancellationToken) -> Result<WaveEditReport, String> {
    match op {
        WaveformOp::Plugin(p) => return plugins.run(buffer, p, cancel),
        WaveformOp::Delete { start, end } => buffer.delete(*start, *end, cancel),
        WaveformOp::Paste { at, mode, channels, .. } => {
            let clip = clip.ok_or("waveform_clip_missing")?;
            buffer.paste(*at, clip, *mode, *channels, cancel)
        }
        WaveformOp::Silence(s) => buffer.silence(*s, cancel),
        WaveformOp::InsertSilence { at, frames } => buffer.insert_silence(*at, *frames, cancel),
        WaveformOp::Fade { selection, direction, curve } => buffer.fade(*selection, *direction, *curve, cancel),
        WaveformOp::GainEnvelope { selection, points } => buffer.gain_envelope(*selection, points, cancel),
        WaveformOp::Amplify { selection, db } => buffer.amplify(*selection, *db, cancel),
        WaveformOp::Normalize { selection, target_dbfs, linked } => buffer.normalize(*selection, *target_dbfs, *linked, cancel),
        WaveformOp::Invert(s) => buffer.invert(*s, cancel),
        WaveformOp::Reverse(s) => buffer.reverse(*s, cancel),
        WaveformOp::Effect { selection, effect } => {
            let spec = effect_spec(effect)?;
            let clip = buffer.copy(*selection).map_err(code)?;
            let processed = bounce_effect(&clip.channels, buffer.sample_rate_hz(), &spec, cancel)?;
            return buffer.paste(selection.start, &WaveClip { channels: processed }, PasteMode::Overwrite, selection.channels, cancel).map_err(code);
        }
        WaveformOp::Spectral(e) => {
            let rate = buffer.sample_rate_hz();
            buffer.process_channels(e.channels, cancel, |input, out| {
                score::spectral_mask_edit(input, out, rate, e.fft_size, e.window, &e.region, e.op, cancel).map(|_| ())
            })
        }
    }
    .map_err(code)
}

/// The source with `ops` replayed (pasted clips re-derived from their prefixes).
pub fn replay(source: &WaveBuffer, ops: &[WaveformOp], plugins: &dyn PluginHost, cancel: &CancellationToken) -> Result<WaveBuffer, String> {
    let mut wanted: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, op) in ops.iter().enumerate() {
        if let WaveformOp::Paste { clip, .. } = op {
            wanted.entry(clip.after_ops).or_default().push(i);
        }
    }
    let mut clips: BTreeMap<usize, WaveClip> = BTreeMap::new();
    let mut buffer = source.clone();
    for i in 0..=ops.len() {
        for j in wanted.get(&i).into_iter().flatten() {
            if let Some(WaveformOp::Paste { clip, .. }) = ops.get(*j) {
                clips.insert(*j, buffer.copy(clip.selection).map_err(|e| format!("op {j}: clip: {}", code(e)))?);
            }
        }
        let Some(op) = ops.get(i) else { break };
        apply_op(&mut buffer, op, clips.get(&i), plugins, cancel).map_err(|e| format!("op {i} ({}): {e}", op.name()))?;
    }
    Ok(buffer)
}

fn prefix_digest(ops: &[WaveformOp]) -> [u8; 32] {
    let v = Value::Array(ops.iter().map(WaveformOp::to_json).collect());
    score::sha256(&serde_json::to_vec(&v).unwrap_or_default())
}

/// Copied audio and where it came from.
#[derive(Clone, Debug)]
struct Clipboard {
    after_ops: usize,
    prefix: [u8; 32],
    selection: WaveSelection,
    clip: WaveClip,
}

/// The editor bound to one source buffer.
pub struct WaveformState {
    source: Arc<WaveBuffer>,
    identity: SourceIdentity,
    cache: Option<(Vec<WaveformOp>, WaveBuffer)>,
    selection: Option<WaveSelection>,
    snap: bool,
    /// Zero-crossing search distance in samples (10 ms at the source rate).
    snap_reach: u64,
    clipboard: Option<Clipboard>,
    amplify_db: f64,
    normalize_dbfs: f64,
    fade_curve: FadeCurve,
    drag_from: Option<u64>,
    error: Option<String>,
    /// Hosted plugins the edit list may name (slice 4b).
    pub(crate) plugins: PluginRegistry,
}

impl super::Viewer {
    /// Opens the waveform editor on `channels` at `sample_rate_hz` (a document must be open: the edit
    /// list lives in its session). A stored list of another source is refused
    /// (`waveform_source_mismatch`); a stored list of this source is replayed.
    pub fn open_waveform(&mut self, sample_rate_hz: u32, channels: Vec<Vec<f32>>) -> Result<(), String> {
        self.run.snapshot().ok_or("waveform: no document is open")?;
        let source = WaveBuffer::new(sample_rate_hz, channels).map_err(code)?;
        let identity = SourceIdentity::of(&source);
        if let Some(doc) = self.waveform_document()?
            && doc.source != identity
        {
            return Err(format!("waveform_source_mismatch: the stored edit list belongs to source {}", doc.source.sha256));
        }
        let snap_reach = u64::from(sample_rate_hz.checked_div(100).unwrap_or(0)).max(1);
        self.waveform = Some(WaveformState {
            source: Arc::new(source),
            identity,
            cache: None,
            selection: None,
            snap: false,
            snap_reach,
            clipboard: None,
            amplify_db: 0.0,
            normalize_dbfs: -0.1,
            fade_curve: FadeCurve::Linear,
            drag_from: None,
            error: None,
            plugins: PluginRegistry::default(),
        });
        self.waveform_sync()?;
        self.say("waveform editor open".into());
        Ok(())
    }

    /// Opens the waveform editor on the open audio file's decoded samples (feature `media`).
    pub fn open_waveform_from_audio(&mut self) -> Result<(), String> {
        let a = self.audio.as_ref().ok_or("waveform: no audio file is open")?;
        let (rate, planar) = (a.import.audio.sample_rate, a.import.audio.planar.as_ref().clone());
        self.open_waveform(rate, planar)
    }

    /// The stored edit list (`None`: no entry).
    pub fn waveform_document(&self) -> Result<Option<WaveformDocument>, String> {
        self.run.document_entry(WAVEFORM_EDIT_ENTRY).map(WaveformDocument::parse).transpose()
    }

    fn waveform_ops(&self) -> Result<Vec<WaveformOp>, String> {
        Ok(self.waveform_document()?.map(|d| d.ops).unwrap_or_default())
    }

    /// Brings the cache to the stored list (after undo, redo or open).
    fn waveform_sync(&mut self) -> Result<(), String> {
        let ops = self.waveform_ops()?;
        let cancel = self.cancel;
        let w = self.waveform.as_mut().ok_or("waveform: not open")?;
        if w.cache.as_ref().is_some_and(|(o, _)| *o == ops) {
            return Ok(());
        }
        // Appended operations without a paste apply to the cache; anything else replays.
        let incremental = w.cache.as_ref().is_some_and(|(o, _)| ops.len() > o.len() && ops.starts_with(o) && ops[o.len()..].iter().all(|x| !matches!(x, WaveformOp::Paste { .. })));
        let result = match w.cache.take() {
            Some((done, mut buffer)) if incremental => ops[done.len()..]
                .iter()
                .try_for_each(|op| apply_op(&mut buffer, op, None, &w.plugins, cancel).map(|_| ()))
                .map(|()| buffer),
            _ => replay(&w.source, &ops, &w.plugins, cancel),
        };
        match result {
            Ok(buffer) => {
                w.cache = Some((ops, buffer));
                w.error = None;
                Ok(())
            }
            Err(e) => {
                w.cache = None;
                w.error = Some(e.clone());
                Err(format!("waveform: replay failed: {e}"))
            }
        }
    }

    /// The edited audio (the source with the stored list replayed).
    pub fn waveform_buffer(&self) -> Result<WaveBuffer, String> {
        let w = self.waveform.as_ref().ok_or("waveform: not open")?;
        let ops = self.waveform_ops()?;
        match &w.cache {
            Some((o, b)) if *o == ops => Ok(b.clone()),
            _ => replay(&w.source, &ops, &w.plugins, self.cancel),
        }
    }

    /// Sets (or clears) the selection; with snap on, each end moves to the nearest zero crossing of
    /// the selected channels within 10 ms (an end with none stays). Returns the selection kept.
    pub fn waveform_select(&mut self, selection: Option<WaveSelection>) -> Result<Option<WaveSelection>, String> {
        self.waveform_sync()?;
        let cancel = self.cancel;
        let w = self.waveform.as_mut().ok_or("waveform: not open")?;
        let Some(sel) = selection else {
            w.selection = None;
            return Ok(None);
        };
        let buffer = &w.cache.as_ref().ok_or("waveform: no audio")?.1;
        buffer.mask_channels(sel.channels).map_err(code)?;
        if sel.start >= sel.end || sel.end > buffer.frames() {
            return Err("waveform_out_of_range".into());
        }
        let mut kept = sel;
        if w.snap {
            let snap = |p: u64| buffer.snap_to_zero_crossing(p, sel.channels, SnapDirection::Nearest, w.snap_reach, cancel).map_err(code);
            kept.start = snap(sel.start)?.unwrap_or(sel.start);
            kept.end = snap(sel.end)?.unwrap_or(sel.end);
            if kept.start >= kept.end {
                kept = sel;
            }
        }
        w.selection = Some(kept);
        Ok(Some(kept))
    }

    /// Snap to zero crossings on or off.
    pub fn set_waveform_snap(&mut self, on: bool) -> Result<(), String> {
        self.waveform.as_mut().ok_or("waveform: not open")?.snap = on;
        Ok(())
    }

    /// Runs one edit: every operation that changes audio is ONE undo step (an entry-only step of the
    /// run). Refusals change nothing (no step). Returns the edit report as JSON.
    pub fn waveform_edit(&mut self, edit: WaveformEdit) -> Result<Value, String> {
        self.waveform_sync()?;
        let cancel = self.cancel;
        let w = self.waveform.as_mut().ok_or("waveform: not open")?;
        let (ops, current) = w.cache.as_ref().ok_or("waveform: no audio")?;
        let n = ops.len();
        let copy_of = |sel: WaveSelection| -> Result<Clipboard, String> {
            Ok(Clipboard { after_ops: n, prefix: prefix_digest(ops), selection: sel, clip: current.copy(sel).map_err(code)? })
        };
        let (op, clip, new_clipboard) = match edit {
            WaveformEdit::Copy(sel) => {
                let c = copy_of(sel)?;
                let frames = c.clip.frames();
                w.clipboard = Some(c);
                return Ok(json!({"copied_frames": frames}));
            }
            WaveformEdit::Cut { start, end } => (WaveformOp::Delete { start, end }, None, Some(copy_of(WaveSelection::all(start, end))?)),
            WaveformEdit::Paste { at, mode, channels } => {
                let c = w.clipboard.as_ref().ok_or("waveform_clipboard_empty")?;
                if c.after_ops > n || prefix_digest(&ops[..c.after_ops]) != c.prefix {
                    return Err("waveform_clipboard_stale: the edits the clip was copied after were undone; copy again".into());
                }
                (WaveformOp::Paste { at, mode, channels, clip: ClipRef { after_ops: c.after_ops, selection: c.selection } }, Some(c.clip.clone()), None)
            }
            WaveformEdit::Delete { start, end } => (WaveformOp::Delete { start, end }, None, None),
            WaveformEdit::Silence(s) => (WaveformOp::Silence(s), None, None),
            WaveformEdit::InsertSilence { at, frames } => (WaveformOp::InsertSilence { at, frames }, None, None),
            WaveformEdit::Fade { selection, direction, curve } => (WaveformOp::Fade { selection, direction, curve }, None, None),
            WaveformEdit::GainEnvelope { selection, points } => (WaveformOp::GainEnvelope { selection, points }, None, None),
            WaveformEdit::Amplify { selection, db } => (WaveformOp::Amplify { selection, db }, None, None),
            WaveformEdit::Normalize { selection, target_dbfs, linked } => (WaveformOp::Normalize { selection, target_dbfs, linked }, None, None),
            WaveformEdit::Invert(s) => (WaveformOp::Invert(s), None, None),
            WaveformEdit::Reverse(s) => (WaveformOp::Reverse(s), None, None),
            WaveformEdit::Spectral(e) => (WaveformOp::Spectral(e), None, None),
            WaveformEdit::Effect { selection, effect } => (WaveformOp::Effect { selection, effect }, None, None),
            WaveformEdit::Plugin(p) => (WaveformOp::Plugin(p), None, None),
        };
        if n >= MAX_WAVEFORM_OPS {
            return Err("waveform_too_many_ops".into());
        }
        let mut next = current.clone();
        let report = apply_op(&mut next, &op, clip.as_ref(), &w.plugins, cancel)?;
        let mut list = ops.clone();
        list.push(op.clone());
        let doc = WaveformDocument { source: w.identity.clone(), ops: list.clone() };
        let bytes = doc.to_bytes();
        if bytes.len() > MAX_WAVEFORM_ENTRY_BYTES {
            return Err("waveform_entry_too_large".into());
        }
        let label = format!("waveform_{}", op.name());
        if !self.run.set_tiles_and_entry(&label, "", None, WAVEFORM_EDIT_ENTRY, bytes) {
            return Err(format!("waveform: {label} refused: {}", self.op_error()));
        }
        let w = self.waveform.as_mut().ok_or("waveform: not open")?;
        let frames = next.frames();
        w.cache = Some((list, next));
        if new_clipboard.is_some() {
            w.clipboard = new_clipboard;
        }
        if let Some(s) = w.selection
            && s.end > frames
        {
            w.selection = None;
        }
        Ok(json!({
            "op": op.name(), "frames_before": report.frames_before, "frames_after": report.frames_after,
            "changed_start": report.changed_start, "changed_end": report.changed_end, "gains_db": report.gains_db,
        }))
    }

    /// Undo / redo of the run, then the editor follows the stored list.
    pub fn waveform_undo(&mut self, undo: bool) -> Result<(), String> {
        let ok = if undo { self.run.undo() } else { self.run.redo() };
        if !ok {
            return Err(format!("waveform: {}", self.op_error()));
        }
        if self.waveform.is_some() { self.waveform_sync() } else { Ok(()) }
    }

    /// Per channel, `(min, max)` of each of `columns` equal spans of `start .. end` (display data).
    pub fn waveform_peaks(&self, start: u64, end: u64, columns: usize) -> Result<Vec<Vec<(f32, f32)>>, String> {
        let b = self.waveform_buffer()?;
        peaks_of(&b, start, end, columns)
    }

    /// Structured projection (`Null` when closed).
    pub fn waveform_json(&self) -> Value {
        let Some(w) = &self.waveform else { return Value::Null };
        let ops = self.waveform_ops();
        let buffer = self.waveform_buffer();
        let peak_dbfs = buffer.as_ref().ok().map(|b| {
            b.channels().iter().map(|c| {
                let p = c.iter().fold(0.0_f32, |m, v| m.max(v.abs()));
                if p > 0.0 { Value::from(20.0 * f64::from(p).log10()) } else { Value::Null }
            }).collect::<Vec<_>>()
        });
        json!({
            "source": w.identity.to_json(),
            "frames": buffer.as_ref().ok().map(WaveBuffer::frames),
            "channels": w.identity.channels,
            "ops": ops.as_ref().ok().map(|o| o.iter().map(WaveformOp::name).collect::<Vec<_>>()),
            "selection": w.selection.as_ref().map(sel_json),
            "snap": w.snap,
            "clipboard": w.clipboard.as_ref().map(|c| json!({"frames": c.clip.frames(), "channels": c.clip.channels.len(), "after_ops": c.after_ops})),
            "peak_dbfs": peak_dbfs,
            "error": w.error.clone().or_else(|| buffer.err()),
        })
    }

    /// The waveform panel (bottom panel while the editor is open; an open button while only an audio
    /// file is open).
    pub(crate) fn waveform_ui(&mut self, ctx: &egui::Context) {
        if self.waveform.is_none() {
            if self.audio.is_some() && self.run.snapshot().is_some() {
                let mut open = false;
                egui::TopBottomPanel::bottom(super::widget::id(widget::PANEL)).show(ctx, |ui| {
                    open = ui.push_id(super::widget::id(widget::OPEN), |ui| ui.button("Edit waveform")).inner.clicked();
                });
                self.drawn.extend([widget::PANEL, widget::OPEN].map(String::from));
                if open {
                    let r = self.open_waveform_from_audio();
                    if let Err(e) = r {
                        self.say(e);
                    }
                }
            }
            return;
        }
        let _ = self.waveform_sync();
        let Ok(buffer) = self.waveform_buffer() else { return };
        let mut requests: Vec<WaveformEdit> = Vec::new();
        let mut select: Option<Option<WaveSelection>> = None;
        let mut history: Option<bool> = None;
        let mut drawn: Vec<&'static str> = Vec::new();
        let frames = buffer.frames();
        let Some(w) = self.waveform.as_mut() else { return };
        let sel = w.selection;
        let all = WaveSelection::all(0, frames.max(1));
        let target = sel.unwrap_or(all);
        egui::TopBottomPanel::bottom(super::widget::id(widget::PANEL)).resizable(true).min_height(160.0).show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                let has = sel.is_some();
                if button(ui, &mut drawn, widget::SELECT_ALL, "All", frames > 0) {
                    select = Some(Some(all));
                }
                if button(ui, &mut drawn, widget::CUT, "Cut", has) {
                    requests.push(WaveformEdit::Cut { start: target.start, end: target.end });
                }
                if button(ui, &mut drawn, widget::COPY, "Copy", has) {
                    requests.push(WaveformEdit::Copy(target));
                }
                let at = sel.map_or(0, |s| s.start);
                if button(ui, &mut drawn, widget::PASTE, "Paste", w.clipboard.is_some()) {
                    requests.push(WaveformEdit::Paste { at, mode: PasteMode::Insert, channels: ALL_CHANNELS });
                }
                if button(ui, &mut drawn, widget::PASTE_MIX, "Mix paste", w.clipboard.is_some()) {
                    requests.push(WaveformEdit::Paste { at, mode: PasteMode::Mix, channels: target.channels });
                }
                if button(ui, &mut drawn, widget::DELETE, "Delete", has) {
                    requests.push(WaveformEdit::Delete { start: target.start, end: target.end });
                }
                if button(ui, &mut drawn, widget::SILENCE, "Silence", has) {
                    requests.push(WaveformEdit::Silence(target));
                }
                if button(ui, &mut drawn, widget::INSERT_SILENCE, "Insert 1 s", true) {
                    requests.push(WaveformEdit::InsertSilence { at, frames: u64::from(w.source.sample_rate_hz()) });
                }
                if button(ui, &mut drawn, widget::FADE_IN, "Fade in", has) {
                    requests.push(WaveformEdit::Fade { selection: target, direction: FadeDirection::In, curve: w.fade_curve });
                }
                if button(ui, &mut drawn, widget::FADE_OUT, "Fade out", has) {
                    requests.push(WaveformEdit::Fade { selection: target, direction: FadeDirection::Out, curve: w.fade_curve });
                }
                drawn.push(widget::FADE_CURVE);
                egui::ComboBox::from_id_salt(super::widget::id(widget::FADE_CURVE)).selected_text(curve_name(w.fade_curve)).show_ui(ui, |ui| {
                    for c in [FadeCurve::Linear, FadeCurve::EqualPower, FadeCurve::SCurve, FadeCurve::Logarithmic] {
                        ui.selectable_value(&mut w.fade_curve, c, curve_name(c));
                    }
                });
                drawn.push(widget::AMPLIFY_DB);
                ui.push_id(super::widget::id(widget::AMPLIFY_DB), |ui| ui.add(egui::DragValue::new(&mut w.amplify_db).range(score::WAVE_AMPLIFY_DB.0..=score::WAVE_AMPLIFY_DB.1).speed(0.1).suffix(" dB")));
                if button(ui, &mut drawn, widget::AMPLIFY, "Amplify", true) {
                    requests.push(WaveformEdit::Amplify { selection: target, db: w.amplify_db });
                }
                drawn.push(widget::NORMALIZE_DBFS);
                ui.push_id(super::widget::id(widget::NORMALIZE_DBFS), |ui| ui.add(egui::DragValue::new(&mut w.normalize_dbfs).range(score::WAVE_NORMALIZE_DBFS.0..=score::WAVE_NORMALIZE_DBFS.1).speed(0.1).suffix(" dBFS")));
                if button(ui, &mut drawn, widget::NORMALIZE, "Normalize", true) {
                    requests.push(WaveformEdit::Normalize { selection: target, target_dbfs: w.normalize_dbfs, linked: true });
                }
                if button(ui, &mut drawn, widget::INVERT, "Invert", true) {
                    requests.push(WaveformEdit::Invert(target));
                }
                if button(ui, &mut drawn, widget::REVERSE, "Reverse", true) {
                    requests.push(WaveformEdit::Reverse(target));
                }
                drawn.push(widget::SNAP);
                ui.push_id(super::widget::id(widget::SNAP), |ui| ui.checkbox(&mut w.snap, "Snap to zero crossings"));
                if button(ui, &mut drawn, widget::UNDO, "Undo", true) {
                    history = Some(true);
                }
                if button(ui, &mut drawn, widget::REDO, "Redo", true) {
                    history = Some(false);
                }
            });
            ui.label(match sel {
                Some(s) => format!("selection {} .. {} ({} samples) of {frames}", s.start, s.end, s.end.saturating_sub(s.start)),
                None => format!("{frames} samples, no selection"),
            });
            drawn.push(widget::PANEL);
            drawn.push(widget::CANVAS);
            let size = egui::vec2(ui.available_width().max(64.0), ui.available_height().max(96.0));
            let (response, painter) = ui.allocate_painter(size, egui::Sense::click_and_drag());
            let rect = response.rect;
            let columns = rect.width().max(1.0) as usize;
            let lanes = buffer.channel_count().max(1);
            let to_sample = |x: f32| -> u64 { (f64::from(((x - rect.left()) / rect.width()).clamp(0.0, 1.0)) * frames as f64).round() as u64 };
            let to_x = |s: u64| -> f32 { rect.left() + (s as f64 / frames.max(1) as f64) as f32 * rect.width() };
            if let Some(s) = sel {
                painter.rect_filled(egui::Rect::from_x_y_ranges(to_x(s.start)..=to_x(s.end), rect.y_range()), 0.0, egui::Color32::from_rgba_unmultiplied(80, 140, 255, 60));
            }
            if let Ok(peaks) = peaks_of(&buffer, 0, frames, columns) {
                let lane_h = rect.height() / lanes as f32;
                for (c, cols) in peaks.iter().enumerate() {
                    let mid = rect.top() + lane_h * (c as f32 + 0.5);
                    for (i, (lo, hi)) in cols.iter().enumerate() {
                        let x = rect.left() + i as f32;
                        painter.line_segment([egui::pos2(x, mid - hi.clamp(-1.0, 1.0) * lane_h * 0.5), egui::pos2(x, mid - lo.clamp(-1.0, 1.0) * lane_h * 0.5)], egui::Stroke::new(1.0_f32, egui::Color32::LIGHT_GREEN));
                    }
                }
            }
            if response.drag_started()
                && let Some(p) = response.interact_pointer_pos()
            {
                w.drag_from = Some(to_sample(p.x));
            }
            if response.dragged()
                && let (Some(from), Some(p)) = (w.drag_from, response.interact_pointer_pos())
            {
                let to = to_sample(p.x);
                let (a, b) = if from <= to { (from, to) } else { (to, from) };
                if a < b {
                    select = Some(Some(WaveSelection { start: a, end: b, channels: ALL_CHANNELS }));
                }
            }
            if response.clicked() {
                select = Some(None);
            }
        });
        self.drawn.extend(drawn.into_iter().map(String::from));
        if let Some(s) = select {
            let _ = self.waveform_select(s);
        }
        for r in requests {
            if let Err(e) = self.waveform_edit(r) {
                self.say(e);
            }
        }
        if let Some(undo) = history {
            let _ = self.waveform_undo(undo);
        }
    }
}

fn button(ui: &mut egui::Ui, drawn: &mut Vec<&'static str>, name: &'static str, text: &str, enabled: bool) -> bool {
    drawn.push(name);
    ui.push_id(super::widget::id(name), |ui| ui.add_enabled(enabled, egui::Button::new(text))).inner.clicked()
}

/// Per channel, `(min, max)` over `columns` equal spans of `start .. end`.
pub fn peaks_of(b: &WaveBuffer, start: u64, end: u64, columns: usize) -> Result<Vec<Vec<(f32, f32)>>, String> {
    if start >= end || end > b.frames() || columns == 0 || columns > 65_536 {
        return Err("waveform_out_of_range".into());
    }
    let span = end.saturating_sub(start);
    let cols = columns as u64;
    Ok(b.channels()
        .iter()
        .map(|c| {
            (0..cols)
                .map(|i| {
                    let a = start.saturating_add(span.saturating_mul(i).checked_div(cols).unwrap_or(0));
                    let z = start.saturating_add(span.saturating_mul(i.saturating_add(1)).checked_div(cols).unwrap_or(0)).max(a.saturating_add(1)).min(end);
                    let slice = &c[usize::try_from(a).unwrap_or(usize::MAX).min(c.len())..usize::try_from(z).unwrap_or(usize::MAX).min(c.len())];
                    slice.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| (lo.min(*v), hi.max(*v)))
                })
                .map(|(lo, hi)| if lo > hi { (0.0, 0.0) } else { (lo, hi) })
                .collect()
        })
        .collect())
}
