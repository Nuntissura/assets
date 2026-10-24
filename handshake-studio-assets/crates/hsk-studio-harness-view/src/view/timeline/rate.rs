//! Rate Stretch tool and the Speed/Duration command of the timeline panel (lane
//! SMB-PR-RATE-STRETCH-1; [STU-VID-026] row "Rate stretch", [STU-VID-031], [STU-VID-090]
//! obligations 1 and 3, [STU-VID-040]). Both are planned by pulse's `plan_rate_stretch` /
//! `plan_speed_duration` and commit as ONE history step through `Run::submit_sequence`.
//!
//! # Manual (no-context operator or model)
//!
//! Open the timeline panel (WORKSPACE > Panels > Timeline, F10). Then:
//!
//! - **Rate Stretch tool** (toolbar `timeline.tool.rate_stretch`, key `R` with the panel active,
//!   spec command `cmd.tools.05ratestretch`; [`TimelineCommand::SetTool`] with
//!   [`TrimTool::RateStretch`]): dragging a clip's head or tail edge changes the clip's duration by
//!   changing its speed; the source window (`source_in` / `source_out`) never changes, the other
//!   edge stays and nothing ripples. One drag is ONE undo step. A drag that would run into the
//!   neighbour stops at it (achieved delta, `rate_stretch_overlap`), a drag below one frame stops at
//!   one frame (`rate_stretch_below_one_frame`). The body of a clip still moves it (as Selection).
//!   Typed form: [`TimelineCommand::RateStretch`] `{clip_id, edge, delta_frames}`; the result is in
//!   `state_json()["timeline"]["last_trim"]` (kind `rate_stretch`, requested / achieved frames,
//!   limit, refusal).
//! - **Speed/Duration** (`Ctrl+R` with a clip selected and the panel active, spec command
//!   `cmd.clip.speed`; [`TimelineCommand::SpeedDurationOpen`]): a form row
//!   (`timeline.speed_form`) with Speed % (`timeline.speed_form.percent`), Duration in frames
//!   (`timeline.speed_form.duration`), Reverse (`timeline.speed_form.reverse`), Apply
//!   (`timeline.speed_form.apply`, [`TimelineCommand::SpeedDurationApply`]) and Cancel
//!   (`timeline.speed_form.cancel`). The field edited last is the fixed one; the other is derived
//!   ([STU-VID-031] "fixing either derives the other"). The head stays, the tail follows, no ripple.
//!   Typed form: [`TimelineCommand::SpeedDuration`] `{clip_id, target, reverse}`.
//!   `Maintain audio pitch` and the frame-interpolation choice (frame sampling / frame blending /
//!   optical flow) are shown as unavailable: the clip record has no field for them (spec gap).
//! - Clip blocks of clips not at 100 % show their speed after the name (`(-50.00%)` = reversed
//!   half speed).
//! - **Slip at speed**: a slip of N timeline frames moves the source window by N x ticks per frame
//!   x |speed| source ticks (at 200 % one frame slips two source frames); a slip that is not a whole
//!   number of source ticks is refused `slip_inexact_at_speed`.
//!
//! Refusals (status log `timeline <command>: <code>`, nothing changes): `rate_stretch_overlap`,
//! `rate_stretch_below_one_frame`, `rate_stretch_unrepresentable` (speed or derived duration not
//! exact), `rate_stretch_multicam_reverse` (reverse on a multicam clip or in a multicam source
//! sequence), `not_frame_aligned` (a video clip's derived duration off the frame grid),
//! `track_locked`, `linked_edit_unsupported`, `invalid_speed_percent`, `invalid_duration`,
//! `no_selection`, `no_speed_form`. Recovery: EDIT > Undo (Ctrl+Z) undoes any rate stretch or
//! Speed/Duration change exactly.
//!
//! Studio readings (Premiere behaviour UNVERIFIED, no oracle): rate stretch never ripples; the
//! Speed/Duration command does not ripple trailing clips; the limits are representability, one
//! frame and the neighbours (STU-VID-031 declares no numeric speed limits); the body of a clip under
//! the Rate Stretch tool moves it.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::trim::{TrimKind, TrimOutcome, achieve};
use super::{Planned, TimelineCommand, find_clip, sequence_in, ticks_per_frame};
use hsk_studio_accord::Ticks;
use hsk_studio_folio::{self as folio, StudioMulticamRecords};
use hsk_studio_pulse::{EditContext, RateEdge, RateStretch, SpeedDuration, SpeedDurationTarget, plan_rate_stretch, plan_speed_duration, speed_from_percent, speed_percent_label};
use serde_json::{Value, json};

/// Session document entry of the multicam records (`hsk_studio_session::MULTICAM_ENTRY`).
const MULTICAM_ENTRY: &str = "multicam";

/// Which field of the Speed/Duration form is fixed (the other is derived).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeedFix {
    Speed,
    Duration,
}

/// The open Speed/Duration form (text as typed; parsed on Apply).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeedForm {
    pub clip_id: String,
    /// Speed magnitude in percent (`"50"`, `"33.5"`).
    pub percent: String,
    /// Duration in whole sequence frames.
    pub duration_frames: String,
    pub reverse: bool,
    pub fixed: SpeedFix,
}

impl SpeedForm {
    pub fn json(&self) -> Value {
        json!({
            "clip_id": self.clip_id,
            "percent": self.percent,
            "duration_frames": self.duration_frames,
            "reverse": self.reverse,
            "fixed": match self.fixed { SpeedFix::Speed => "speed", SpeedFix::Duration => "duration" },
            "maintain_audio_pitch": "unavailable: no clip field (STU-VID-031 spec gap)",
            "frame_interpolation": "unavailable: no clip field (STU-VID-031 spec gap)",
        })
    }

    /// The typed command Apply runs.
    pub fn command(&self, ticks_per_frame: u64) -> Result<TimelineCommand, String> {
        let target = match self.fixed {
            SpeedFix::Speed => SpeedDurationTarget::Speed(speed_from_percent(&self.percent).ok_or("invalid_speed_percent")?),
            SpeedFix::Duration => {
                let frames: u64 = self.duration_frames.trim().parse().ok().filter(|f| *f > 0).ok_or("invalid_duration")?;
                SpeedDurationTarget::Duration(Ticks::new(frames.checked_mul(ticks_per_frame).ok_or("invalid_duration")?))
            }
        };
        Ok(TimelineCommand::SpeedDuration { clip_id: self.clip_id.clone(), target, reverse: self.reverse })
    }
}

/// The document's multicam records from the session entry (`None` when absent or unreadable; the
/// session validates them on every read and write).
fn multicam_records(v: &Viewer) -> Option<StudioMulticamRecords> {
    v.run.document_entry(MULTICAM_ENTRY).and_then(|b| serde_json::from_slice(b).ok())
}

fn rate_plan(snapshot: &folio::Snapshot, sequence_id: &str, ctx: &EditContext, records: Option<&StudioMulticamRecords>, edit: &RateStretch) -> Result<Planned, String> {
    let p = plan_rate_stretch(snapshot, sequence_id, snapshot.document().revision, edit, records, ctx).map_err(|e| e.code().to_owned())?;
    let mut notes = Vec::new();
    super::noted(&p, &mut notes);
    Ok(Planned { sequence: sequence_in(p.snapshot(), sequence_id)?, notes })
}

impl Viewer {
    /// Runs a rate command (`None`: not one; module docs).
    pub(super) fn timeline_rate_command(&mut self, command: &TimelineCommand) -> Option<Result<(), String>> {
        Some(match command {
            TimelineCommand::RateStretch { clip_id, edge, delta_frames } => self.timeline_rate_stretch_by(clip_id, *edge, *delta_frames),
            TimelineCommand::SpeedDuration { clip_id, target, reverse } => self.timeline_speed_duration(clip_id, *target, *reverse),
            TimelineCommand::SpeedDurationOpen => self.timeline_speed_form_open(),
            TimelineCommand::SpeedDurationApply => self.timeline_speed_form_apply(),
            TimelineCommand::SpeedDurationCancel => match self.timeline.as_mut().and_then(|t| t.speed_form.take()) {
                Some(_) => Ok(()),
                None => Err("no_speed_form".into()),
            },
            _ => return None,
        })
    }

    /// Rate stretch of `edge` by `requested` frames with the achieved-delta rule (one step).
    fn timeline_rate_stretch_by(&mut self, clip_id: &str, edge: RateEdge, requested: i64) -> Result<(), String> {
        let records = multicam_records(self);
        let mut search = (0i64, 0u32, None);
        let result = self.timeline_commit("timeline_rate_stretch", |snap, seq_id, ctx| {
            let seq = sequence_in(snap, seq_id)?;
            let tpf = ticks_per_frame(&seq);
            let (_, clip) = find_clip(&seq, clip_id).ok_or("unknown_clip")?;
            let at = match edge {
                RateEdge::Head => clip.timeline_start,
                RateEdge::Tail => clip.timeline_end().ok_or("overflow")?,
            };
            let a = achieve(requested, |d| {
                let moved = i128::from(d).checked_mul(i128::from(tpf)).and_then(|m| m.checked_add(i128::from(at.value()))).ok_or("overflow")?;
                let new_edge = Ticks::new(u64::try_from(moved).map_err(|_| "outside_range")?);
                rate_plan(snap, seq_id, ctx, records.as_ref(), &RateStretch { clip_id: clip_id.to_owned(), edge, new_edge })
            });
            search = (a.achieved, a.calls, a.limit);
            a.result
        });
        let revision = self.run.snapshot().map(|s| s.document().revision);
        let (achieved, calls, limit) = search;
        if let Some(t) = self.timeline.as_mut() {
            t.last_trim = Some(TrimOutcome {
                command: "rate_stretch",
                clip_id: clip_id.to_owned(),
                kind: TrimKind::RateStretch,
                requested_frames: requested,
                achieved_frames: if result.is_ok() { achieved } else { 0 },
                planner_calls: calls,
                limit,
                refusal: result.as_ref().err().cloned(),
                revision: result.is_ok().then_some(revision).flatten(),
            });
        }
        result
    }

    fn timeline_speed_duration(&mut self, clip_id: &str, target: SpeedDurationTarget, reverse: bool) -> Result<(), String> {
        let records = multicam_records(self);
        let edit = SpeedDuration { clip_id: clip_id.to_owned(), target, reverse };
        self.timeline_commit("timeline_speed_duration", |snap, seq_id, ctx| {
            let p = plan_speed_duration(snap, seq_id, snap.document().revision, &edit, records.as_ref(), ctx).map_err(|e| e.code().to_owned())?;
            let mut notes = Vec::new();
            super::noted(&p, &mut notes);
            Ok(Planned { sequence: sequence_in(p.snapshot(), seq_id)?, notes })
        })
    }

    fn timeline_speed_form_open(&mut self) -> Result<(), String> {
        let clip_id = self.timeline.as_ref().and_then(|t| t.selected.clone()).ok_or("no_selection")?;
        let seq = self.timeline_sequence().ok_or("unknown_sequence")?;
        let (_, clip) = find_clip(seq, &clip_id).ok_or("unknown_clip")?;
        let tpf = ticks_per_frame(seq);
        let label = speed_percent_label(clip.speed);
        let form = SpeedForm {
            clip_id,
            percent: label.trim_start_matches('-').to_owned(),
            duration_frames: clip.duration.value().checked_div(tpf).unwrap_or(0).to_string(),
            reverse: clip.speed.is_reversed(),
            fixed: SpeedFix::Speed,
        };
        if let Some(t) = self.timeline.as_mut() {
            t.speed_form = Some(form);
        }
        Ok(())
    }

    fn timeline_speed_form_apply(&mut self) -> Result<(), String> {
        let form = self.timeline.as_ref().and_then(|t| t.speed_form.clone()).ok_or("no_speed_form")?;
        let tpf = self.timeline_sequence().map(ticks_per_frame).ok_or("unknown_sequence")?;
        let command = form.command(tpf)?;
        self.timeline_rate_command(&command).unwrap_or_else(|| Err("not_a_rate_command".into()))?;
        if let Some(t) = self.timeline.as_mut() {
            t.speed_form = None;
        }
        Ok(())
    }
}
