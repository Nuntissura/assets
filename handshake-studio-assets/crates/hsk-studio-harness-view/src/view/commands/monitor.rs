//! `monitor.*` (lane SMB-DRIVE-TYPED-2, STU-VID-024/025): the program monitor on a sequence, media
//! bound to it (`Viewer::bind_program_media`, as the CLI's `--media`), the source monitor on a media
//! file, its marks, the monitor keys and the source-to-program edit (`Viewer::source_edit`, one undo
//! step, `source_insert` or `source_overwrite`). Media files are read whole within fixed limits before
//! any byte is decoded; opening media needs feature `media`, else `feature_unavailable`.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::super::monitor::MonitorFail;
use super::super::source_monitor::{MonitorKey, MonitorTarget, SourceEdit, SourceMonitor};
use super::decode::{Fields, invalid};
use super::CommandRefusal;
use hsk_studio_pulse::{AssemblyMode, FourPointFit};
use serde_json::{Value, json};

/// Largest media file one row reads.
#[cfg(feature = "media")]
pub const MAX_MEDIA_FILE_BYTES: u64 = 1 << 30;
/// Most files one `monitor.bind_program_media` binds, and their bytes together.
#[cfg(feature = "media")]
pub const MAX_MEDIA_FILES: usize = 64;
#[cfg(feature = "media")]
pub const MAX_MEDIA_TOTAL_BYTES: u64 = 1 << 31;
/// The monitors' default picture and the largest one a row asks for.
const DEFAULT_SIZE: (u32, u32) = (1920, 1080);
const MAX_SIDE: u32 = 8192;

fn refusal(e: MonitorFail) -> CommandRefusal {
    CommandRefusal::new(e.code, json!({"message": e.detail}))
}

fn size(f: &Fields<'_>) -> Result<(u32, u32), CommandRefusal> {
    let side = |key: &str, default: u32| -> Result<u32, CommandRefusal> {
        if !f.has(key) {
            return Ok(default);
        }
        let n = f.count(key)?;
        if n == 0 || n > MAX_SIDE { Err(invalid(key, "1..=8192")) } else { Ok(n) }
    };
    Ok((side("width", DEFAULT_SIZE.0)?, side("height", DEFAULT_SIZE.1)?))
}

fn key_of(f: &Fields<'_>) -> Result<MonitorKey, CommandRefusal> {
    let steps = || -> Result<u8, CommandRefusal> {
        if !f.has("frames") {
            return Ok(1);
        }
        u8::try_from(f.unsigned("frames")?).ok().filter(|n| *n > 0).ok_or_else(|| invalid("frames", "1..=255"))
    };
    let key = match f.text("key")?.as_str() {
        "shuttle_left" => MonitorKey::ShuttleLeft,
        "shuttle_stop" => MonitorKey::ShuttleStop,
        "shuttle_right" => MonitorKey::ShuttleRight,
        "slow_left" => MonitorKey::SlowLeft,
        "slow_right" => MonitorKey::SlowRight,
        "toggle_play" => MonitorKey::TogglePlay,
        "step_back" => MonitorKey::StepBack(steps()?),
        "step_forward" => MonitorKey::StepForward(steps()?),
        "mark_in" => MonitorKey::MarkIn,
        "mark_out" => MonitorKey::MarkOut,
        "insert" => MonitorKey::Insert,
        "overwrite" => MonitorKey::Overwrite,
        _ => return Err(invalid("key", "shuttle_left|shuttle_stop|shuttle_right|slow_left|slow_right|toggle_play|step_back|step_forward|mark_in|mark_out|insert|overwrite")),
    };
    // A step count on a key that takes none is refused, never dropped (review F7 on 2bbe2207).
    if f.has("frames") && !matches!(key, MonitorKey::StepBack(_) | MonitorKey::StepForward(_)) {
        return Err(CommandRefusal::new("parameter_conflict", json!({"keys": ["frames", "key"]})));
    }
    Ok(key)
}

/// Both marks as one pair (review F3 on 2bbe2207): `in > out` is refused first; then the given marks
/// are set over cleared ones (one mark alone is checked against the other, as the engine does), and
/// any refusal puts the previous marks back.
fn set_marks(s: &mut SourceMonitor, marks: (Option<u64>, Option<u64>)) -> Result<(), CommandRefusal> {
    if let (Some(i), Some(o)) = marks
        && o < i
    {
        return Err(CommandRefusal::new("out_before_in", json!({"message": format!("out {o} before in {i}")})));
    }
    let prev = s.marks();
    let apply = |s: &mut SourceMonitor| -> Result<(), MonitorFail> {
        if marks.0.is_some() && marks.1.is_some() {
            s.clear_marks();
        }
        if let Some(i) = marks.0 {
            s.mark_in(i)?;
        }
        if let Some(o) = marks.1 {
            s.mark_out(o)?;
        }
        Ok(())
    };
    apply(s).map_err(|e| {
        // The previous marks were a valid pair, so setting them again cannot be refused.
        s.clear_marks();
        if let Some(i) = prev.0 {
            let _ = s.mark_in(i);
        }
        if let Some(o) = prev.1 {
            let _ = s.mark_out(o);
        }
        refusal(e)
    })
}

fn monitors(v: &Viewer) -> Value {
    json!({"program": v.monitor().map(|m| m.state_json()), "source": v.source_monitor().map(|s| s.state_json())})
}

#[cfg(feature = "media")]
fn media(v: &mut Viewer, id: &str, f: &Fields<'_>) -> Result<Value, CommandRefusal> {
    use super::read_bounded;
    let (width, height) = size(f)?;
    if id == "monitor.open_source_media" {
        let bytes = read_bounded(&f.path("path")?, MAX_MEDIA_FILE_BYTES)?;
        let source = v.open_source_media(bytes, width, height).map_err(refusal)?;
        return Ok(json!({"source": source_id(&source), "monitors": monitors(v)}));
    }
    let names = f.list("files")?;
    // Nothing to bind has its own code (review F6 on 2bbe2207).
    if names.is_empty() {
        return Err(CommandRefusal::new("media_files_empty", json!({"key": "files", "expected": "1..=64 files"})));
    }
    if names.len() > MAX_MEDIA_FILES {
        return Err(CommandRefusal::new("media_too_many", json!({"files": names.len(), "limit": MAX_MEDIA_FILES})));
    }
    let mut files = Vec::with_capacity(names.len());
    let mut total = 0u64;
    for name in &names {
        let left = MAX_MEDIA_TOTAL_BYTES.saturating_sub(total);
        let bytes = read_bounded(std::path::Path::new(name), MAX_MEDIA_FILE_BYTES.min(left))?;
        total = total.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        files.push(bytes);
    }
    let sources = v.bind_program_media(&f.text("sequence_id")?, files, width, height).map_err(refusal)?;
    Ok(json!({"sources": sources.iter().map(source_id).collect::<Vec<_>>(), "monitors": monitors(v)}))
}

#[cfg(feature = "media")]
fn source_id(source: &hsk_studio_folio::ClipSource) -> Value {
    match source {
        hsk_studio_folio::ClipSource::Media { artifact_manifest_id, .. } => Value::String(artifact_manifest_id.clone()),
        other => serde_json::to_value(other).unwrap_or(Value::Null),
    }
}

#[cfg(not(feature = "media"))]
fn media(_: &mut Viewer, id: &str, _: &Fields<'_>) -> Result<Value, CommandRefusal> {
    Err(CommandRefusal::new("feature_unavailable", json!({"command_id": id, "feature": "media"})))
}

/// One `monitor.*` row (module docs).
pub(super) fn run(v: &mut Viewer, id: &str, f: Fields<'_>) -> Result<Value, CommandRefusal> {
    match id {
        "monitor.open" => {
            let (width, height) = size(&f)?;
            v.open_monitor(&f.text("sequence_id")?, width, height).map_err(refusal)?;
        }
        "monitor.bind_program_media" | "monitor.open_source_media" => return media(v, id, &f),
        "monitor.source_mark" => {
            let marks = (if f.has("mark_in") { Some(f.unsigned("mark_in")?) } else { None }, if f.has("mark_out") { Some(f.unsigned("mark_out")?) } else { None });
            let s = v.source_monitor_mut().ok_or_else(|| CommandRefusal::new("no_source_monitor", json!({"hint": "monitor.open_source_media first"})))?;
            match marks {
                (None, None) => s.clear_marks(),
                given => set_marks(s, given)?,
            }
        }
        "monitor.key" => {
            let target = f.token("target", &[("source", MonitorTarget::Source), ("program", MonitorTarget::Program)])?;
            v.monitor_key(target, key_of(&f)?).map_err(refusal)?;
        }
        "monitor.source_edit" => {
            let mode = f.token("mode", &[("insert", AssemblyMode::Insert), ("overwrite", AssemblyMode::Overwrite)])?;
            let fit = if f.has("fit") {
                Some(f.token("fit", &[("change_speed", FourPointFit::ChangeSpeed), ("trim_head", FourPointFit::TrimHead), ("trim_tail", FourPointFit::TrimTail), ("ignore_sequence_out", FourPointFit::IgnoreSequenceOut)])?)
            } else {
                None
            };
            let targets = if f.has("targets") { f.list("targets")? } else { Vec::new() };
            let revision = v.source_edit(&SourceEdit { mode, targets, fit }).map_err(refusal)?;
            return Ok(json!({"revision": revision, "monitors": monitors(v)}));
        }
        other => return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
    }
    Ok(monitors(v))
}
