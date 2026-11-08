//! `waveform.*` and `spectral.*` (lane SMB-DRIVE-TYPED-2): every `WaveformEdit` member but the hosted
//! plugin through `Viewer::waveform_edit` (one entry-only undo step each, `waveform_<op>`, saved with
//! the package as the edit list), the editor's selection, snap and peaks, and the spectral display:
//! its parameters, a marquee or lasso region and a `SpectralMaskOp` applied through
//! `Viewer::apply_spectral` (the waveform editor's `waveform_spectral` step). The editor opens with
//! `audio.waveform_open_clip`. Feature `audio-batch`; elsewhere every row refuses `feature_unavailable`.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::CommandRefusal;
use super::decode::Fields;
use serde_json::Value;

#[cfg(feature = "audio-batch")]
mod enabled {
    use super::super::super::Viewer;
    use super::super::super::spectral_display::default_params;
    use super::super::super::waveform::{NoiseReduceParams, WaveformEdit};
    use super::super::decode::{Fields, invalid, missing};
    use super::super::mixer::enabled::effect_instance;
    use super::super::{CommandRefusal, engine_refusal};
    use hsk_studio_score::{self as score, ALL_CHANNELS, FadeCurve, FadeDirection, FrequencyScale, PasteMode, SpectralMaskOp, SpectralRegion, SpectralWindow, WaveSelection};
    use serde_json::{Value, json};

    /// Most peak columns one reply carries per channel.
    pub const MAX_PEAK_COLUMNS: usize = 4096;
    const CURVES: &[(&str, FadeCurve)] = &[("linear", FadeCurve::Linear), ("equal_power", FadeCurve::EqualPower), ("s_curve", FadeCurve::SCurve), ("logarithmic", FadeCurve::Logarithmic)];
    const WINDOWS: &[(&str, SpectralWindow)] = &[
        ("hann", SpectralWindow::Hann),
        ("hamming", SpectralWindow::Hamming),
        ("blackman", SpectralWindow::Blackman),
        ("blackman_harris", SpectralWindow::BlackmanHarris),
        ("rectangular", SpectralWindow::Rectangular),
    ];

    /// Declared keys that do not apply together.
    fn conflict(keys: &[&str]) -> CommandRefusal {
        CommandRefusal::new("parameter_conflict", json!({"keys": keys}))
    }

    fn channels(f: &Fields<'_>) -> Result<u32, CommandRefusal> {
        if f.has("channels") { f.count("channels") } else { Ok(ALL_CHANNELS) }
    }

    fn selection(f: &Fields<'_>) -> Result<WaveSelection, CommandRefusal> {
        Ok(WaveSelection { start: f.unsigned("start")?, end: f.unsigned("end")?, channels: channels(f)? })
    }

    /// `[[offset_frames, db], ...]`, at most score's envelope point count, checked before any is held.
    fn envelope(f: &Fields<'_>) -> Result<Vec<(u64, f64)>, CommandRefusal> {
        let expected = "[[offset_frames, db], ...]";
        let value = f.json("points", expected)?;
        let items = value.as_array().ok_or_else(|| invalid("points", expected))?;
        if items.len() > score::MAX_WAVE_ENVELOPE_POINTS {
            return Err(CommandRefusal::new("parameter_too_large", json!({"key": "points", "limit": score::MAX_WAVE_ENVELOPE_POINTS})));
        }
        items
            .iter()
            .map(|p| match p.as_array().map(Vec::as_slice) {
                Some([o, d]) => match (o.as_u64(), d.as_f64().filter(|d| d.is_finite())) {
                    (Some(o), Some(d)) => Ok((o, d)),
                    _ => Err(invalid("points", expected)),
                },
                _ => Err(invalid("points", expected)),
            })
            .collect()
    }

    fn edit_of(id: &str, f: &Fields<'_>) -> Result<WaveformEdit, CommandRefusal> {
        let (start, end) = (|| f.unsigned("start"), || f.unsigned("end"));
        Ok(match id {
            "waveform.copy" => WaveformEdit::Copy(selection(f)?),
            "waveform.cut" => WaveformEdit::Cut { start: start()?, end: end()? },
            "waveform.paste" => WaveformEdit::Paste {
                at: f.unsigned("at")?,
                mode: f.token("mode", &[("insert", PasteMode::Insert), ("overwrite", PasteMode::Overwrite), ("mix", PasteMode::Mix)])?,
                channels: channels(f)?,
            },
            "waveform.delete" => WaveformEdit::Delete { start: start()?, end: end()? },
            "waveform.silence" => WaveformEdit::Silence(selection(f)?),
            "waveform.insert_silence" => WaveformEdit::InsertSilence { at: f.unsigned("at")?, frames: f.unsigned("frames")? },
            "waveform.fade" => WaveformEdit::Fade {
                selection: selection(f)?,
                direction: f.token("direction", &[("in", FadeDirection::In), ("out", FadeDirection::Out)])?,
                curve: if f.has("curve") { f.token("curve", CURVES)? } else { FadeCurve::Linear },
            },
            "waveform.gain_envelope" => WaveformEdit::GainEnvelope { selection: selection(f)?, points: envelope(f)? },
            "waveform.amplify" => WaveformEdit::Amplify { selection: selection(f)?, db: f.number("db")? },
            "waveform.normalize" => WaveformEdit::Normalize { selection: selection(f)?, target_dbfs: f.number("target_dbfs")?, linked: f.flag("linked")? },
            "waveform.invert" => WaveformEdit::Invert(selection(f)?),
            "waveform.reverse" => WaveformEdit::Reverse(selection(f)?),
            "waveform.effect" => WaveformEdit::Effect { selection: selection(f)?, effect: effect_instance(f, &[])? },
            "waveform.noise_reduce" => {
                let d = score::NoiseReductionParams::default();
                let reduction = score::NoiseReductionParams {
                    reduction_db: f.number_or("reduction_db", d.reduction_db)?,
                    over_subtraction: f.number_or("over_subtraction", d.over_subtraction)?,
                    smoothing_up_ms: f.number_or("smoothing_up_ms", d.smoothing_up_ms)?,
                    smoothing_down_ms: f.number_or("smoothing_down_ms", d.smoothing_down_ms)?,
                    freq_smoothing_bins: if f.has("freq_smoothing_bins") { f.index("freq_smoothing_bins")? } else { d.freq_smoothing_bins },
                };
                let channels = channels(f)?;
                WaveformEdit::NoiseReduce {
                    selection: WaveSelection { start: start()?, end: end()?, channels },
                    print: WaveSelection { start: f.unsigned("print_start")?, end: f.unsigned("print_end")?, channels },
                    params: NoiseReduceParams { fft_size: f.index("fft_size")?, reduction },
                }
            }
            other => return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
        })
    }

    fn waveform(v: &mut Viewer, id: &str, f: &Fields<'_>, before: usize) -> Result<Value, CommandRefusal> {
        if v.waveform.is_none() {
            return Err(CommandRefusal::new("waveform_closed", json!({"hint": "audio.waveform_open_clip first"})));
        }
        let result = match id {
            "waveform.state" => Value::Null,
            "waveform.select" => {
                let wanted = match (f.has("start"), f.has("end")) {
                    // Channels select nothing without a range: refused, never dropped (review F7).
                    (false, false) if f.has("channels") => return Err(conflict(&["channels", "start"])),
                    (false, false) => None,
                    (true, true) => Some(selection(f)?),
                    (false, true) => return Err(missing("start")),
                    (true, false) => return Err(missing("end")),
                };
                let kept = v.waveform_select(wanted).map_err(|e| engine_refusal(v, before, e))?;
                json!({"selection": kept.map(|s| json!({"start": s.start, "end": s.end, "channels": s.channels}))})
            }
            "waveform.snap" => {
                v.set_waveform_snap(f.flag("on")?).map_err(|e| engine_refusal(v, before, e))?;
                Value::Null
            }
            "waveform.peaks" => {
                let columns = f.index("columns")?;
                if columns == 0 || columns > MAX_PEAK_COLUMNS {
                    return Err(invalid("columns", "1..=4096"));
                }
                let peaks = v.waveform_peaks(f.unsigned("start")?, f.unsigned("end")?, columns).map_err(|e| engine_refusal(v, before, e))?;
                json!({"channels": peaks.iter().map(|c| c.iter().map(|(lo, hi)| json!([lo, hi])).collect::<Vec<_>>()).collect::<Vec<_>>()})
            }
            _ => {
                let edit = edit_of(id, f)?;
                v.waveform_edit(edit).map_err(|e| engine_refusal(v, before, e))?
            }
        };
        Ok(json!({"result": result, "editor": v.waveform_json()}))
    }

    fn spectral(v: &mut Viewer, id: &str, f: &Fields<'_>, before: usize) -> Result<Value, CommandRefusal> {
        if id == "spectral.open" {
            let mut p = default_params();
            if f.has("fft_size") {
                p.fft_size = f.index("fft_size")?;
            }
            if f.has("hop") {
                p.hop = f.index("hop")?;
            }
            if f.has("window") {
                p.window = f.token("window", WINDOWS)?;
            }
            let min_hz = f.opt_number("min_hz")?;
            match f.opt_text("scale")?.as_deref() {
                Some("linear") => p.scale = FrequencyScale::Linear,
                Some("log") => p.scale = FrequencyScale::Log { min_hz: min_hz.unwrap_or(20.0) },
                Some(_) => return Err(invalid("scale", "linear|log")),
                None => {
                    if let (Some(hz), FrequencyScale::Log { .. }) = (min_hz, p.scale) {
                        p.scale = FrequencyScale::Log { min_hz: hz };
                    }
                }
            }
            // A floor frequency on a linear scale is refused, never dropped (review F7).
            if min_hz.is_some() && matches!(p.scale, FrequencyScale::Linear) {
                return Err(conflict(&["min_hz", "scale"]));
            }
            if f.has("rows") {
                p.rows = f.index("rows")?;
            }
            p.floor_db = f.number_or("floor_db", p.floor_db)?;
            // A refused open keeps the display that was open, its parameters and region (review F2).
            let open = v.spectral.take();
            if let Err(e) = v.open_spectral_display(p) {
                let refusal = engine_refusal(v, before, e);
                v.spectral = open;
                return Err(refusal);
            }
            return Ok(v.spectral_json());
        }
        if v.spectral.is_none() {
            return Err(CommandRefusal::new("spectral_closed", json!({"hint": "spectral.open first"})));
        }
        match id {
            "spectral.state" => {}
            "spectral.set_region" => {
                let marquee = ["start", "end", "low_hz", "high_hz"];
                let given = marquee.iter().filter(|k| f.has(k)).count();
                let region = match (given, f.has("lasso")) {
                    (0, false) => None,
                    (4, false) => Some(SpectralRegion::Marquee { start_sample: f.unsigned("start")?, end_sample: f.unsigned("end")?, low_hz: f.number("low_hz")?, high_hz: f.number("high_hz")? }),
                    (0, true) => {
                        let value = f.json("lasso", "[[sample, hz], ...]")?;
                        let items = value.as_array().ok_or_else(|| invalid("lasso", "[[sample, hz], ...]"))?;
                        if items.len() > score::MAX_LASSO_POINTS {
                            return Err(CommandRefusal::new("parameter_too_large", json!({"key": "lasso", "limit": score::MAX_LASSO_POINTS})));
                        }
                        let points = f.points("lasso").map_err(|_| invalid("lasso", "[[sample, hz], ...]"))?;
                        Some(SpectralRegion::Lasso(points.into_iter().map(|[t, hz]| (t, hz)).collect()))
                    }
                    (_, true) => return Err(CommandRefusal::new("parameter_conflict", json!({"keys": ["lasso", "start"]}))),
                    _ => return Err(missing(marquee.iter().find(|k| !f.has(k)).copied().unwrap_or("start"))),
                };
                v.set_spectral_region(region).map_err(|e| engine_refusal(v, before, e))?;
            }
            "spectral.apply" => {
                let op = match f.text("op")?.as_str() {
                    "identity" => SpectralMaskOp::Identity,
                    "remove" => SpectralMaskOp::Remove,
                    "heal" => SpectralMaskOp::Heal,
                    "attenuate" => SpectralMaskOp::Attenuate { gain_db: f.number("gain_db")? },
                    _ => return Err(invalid("op", "identity|remove|heal|attenuate")),
                };
                // Only attenuate takes a gain (review F7).
                if f.has("gain_db") && !matches!(op, SpectralMaskOp::Attenuate { .. }) {
                    return Err(conflict(&["gain_db", "op"]));
                }
                if v.spectral_json()["region"].is_null() {
                    return Err(CommandRefusal::new("spectral_no_selection", json!({"hint": "spectral.set_region first"})));
                }
                let report = v.apply_spectral(op).map_err(|e| engine_refusal(v, before, e))?;
                return Ok(json!({"result": report, "display": v.spectral_json()}));
            }
            other => return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
        }
        Ok(v.spectral_json())
    }

    pub(super) fn run(v: &mut Viewer, id: &str, f: Fields<'_>, before: usize) -> Result<Value, CommandRefusal> {
        if id.starts_with("spectral.") { spectral(v, id, &f, before) } else { waveform(v, id, &f, before) }
    }
}

/// One `waveform.*` or `spectral.*` row (module docs).
pub(super) fn run(v: &mut Viewer, id: &str, f: Fields<'_>, before: usize) -> Result<Value, CommandRefusal> {
    #[cfg(feature = "audio-batch")]
    {
        enabled::run(v, id, f, before)
    }
    #[cfg(not(feature = "audio-batch"))]
    {
        let _ = (v, f, before);
        Err(CommandRefusal::new("feature_unavailable", serde_json::json!({"command_id": id, "feature": "audio-batch"})))
    }
}
