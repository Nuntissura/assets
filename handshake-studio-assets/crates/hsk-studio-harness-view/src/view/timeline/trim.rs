//! Trim modes, nudges and the achieved-delta rule of the timeline panel (lane SMB-TIMELINE-TRIM-1,
//! [STU-VID-026], [STU-VID-026b], [STU-VID-035a], [STU-VID-090]). Every trim is planned by pulse's
//! existing `TimelineEdit` planners (`TrimIn` / `TrimOut` with or without ripple, `Roll`, `Slip`,
//! `Slide`) and commits as ONE history step through `Run::submit_sequence` ([STU-VID-040]).
//!
//! # Manual (no-context operator or model)
//!
//! Open the timeline panel (WORKSPACE > Panels > Timeline, F10). Then:
//!
//! - **Trim mode toolbar** (`timeline.tool.selection|ripple|roll|slip|slide`,
//!   [`TimelineCommand::SetTool`]): selects what a drag does. Selection: edges trim (no ripple),
//!   body moves. Ripple: edges ripple-trim (later content on the clip's track and on every
//!   sync-locked track shifts so no gap opens). Roll: an edge drag rolls the edit between the clip
//!   and its abutting neighbour (a head edge rolls the previous clip's tail; with no abutting
//!   neighbour the edge trims). Slip: a body drag moves the source window (right = later source).
//!   Slide: a body drag moves the clip between its neighbours. Every drag is ONE undo step.
//! - **Edit point**: a click on a clip edge selects that edit point in the tool's mode;
//!   [`TimelineCommand::SelectEdit`] selects the nearest edit on the targeted tracks (all tracks
//!   when none is targeted) as a trim-in, trim-out, ripple-in, ripple-out or roll.
//! - **Nudges**: [`TimelineCommand::TrimNudge`] trims the selected edit point by one frame or by
//!   the multi-frame step; slip Ctrl+Alt+Left / Right (Shift: step), slide Alt+, / Alt+. (Shift:
//!   step), clip nudge Alt+Left / Right (Shift: step) act on the selected clip. One step
//!   preference ([`TimelineCommand::SetMultiFrameStep`], default [`DEFAULT_MULTI_FRAME_STEP`])
//!   governs every "many" variant.
//! - **Extend Edit** (`timeline.extend_edit`, [`TimelineCommand::ExtendEditToPlayhead`]): rolls
//!   the selected roll edit, else the nearest abutting edit on the targeted tracks, to the playhead.
//! - **Trim session** (`timeline.trim_session.enter`): see module `trim_session`.
//!
//! Achieved delta ([STU-VID-090] obligations 1 and 3): a trim asks for a signed frame delta and
//! commits the largest delta toward zero that pulse accepts, found by a deterministic halving
//! search of at most [`MAX_PLANNER_CALLS`] planner calls, only when the requested delta failed on a
//! LIMIT ([`LIMIT_CODES`]: source start/end, zero duration, overlap, sync-locked track, range);
//! structural refusals (unknown clip, locked track, no abutting neighbour, unknown source length,
//! linked edit) are named at once. `state_json()["timeline"]["last_trim"]` holds the requested and
//! achieved frames, the planner calls and the named refusal when nothing changed. Unknown source
//! extents (no frame source bound) refuse source extensions `source_extent_unknown`, never guess.
//!
//! Studio readings (Premiere behaviour UNVERIFIED, no oracle): the multi-frame step default 5, the
//! slip direction (right = later source), roll on a lone edge trims, cursor zones of the tools.
//! The Rate Stretch tool and Speed/Duration live in module `rate`.
//!
//! State: `state_json()["timeline"]` keys `tool`, `multi_frame_step`, `edit_point`, `last_trim`,
//! `trim_session`, `composite_preview`. Recovery: EDIT > Undo (Ctrl+Z) undoes any trim; a refused
//! trim changes nothing and writes `timeline <command>: <code>` to the status log.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::{Planned, TimelineCommand, find_clip, plan_one, sequence_in, ticks_per_frame};
use hsk_studio_accord::Ticks;
use hsk_studio_folio::StudioSequence;
use hsk_studio_pulse::{TickDelta, TimelineEdit};
use serde_json::{Value, json};

/// Studio default of the multi-frame step (Premiere's default UNVERIFIED).
pub const DEFAULT_MULTI_FRAME_STEP: u32 = 5;
pub const MAX_MULTI_FRAME_STEP: u32 = 1000;
/// Upper bound of planner calls for one achieved-delta search (1 + 63 halvings of an i64).
pub const MAX_PLANNER_CALLS: u32 = 64;
/// Refusal codes that mean "too far": a smaller delta toward zero may still be admitted.
pub const LIMIT_CODES: &[&str] = &[
    "outside_range",
    "past_source_start",
    "past_source_end",
    "zero_duration",
    "invalid_range",
    "overlap",
    "sync_lock_blocked",
    "underflow",
    "overflow",
    "rate_stretch_overlap",
    "rate_stretch_below_one_frame",
];

/// What an edge or body drag does in the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrimTool {
    Selection,
    Ripple,
    Roll,
    Slip,
    Slide,
    /// Edge drags change the clip's speed, never its source window (module `rate`).
    RateStretch,
}

impl TrimTool {
    pub const ALL: [TrimTool; 6] = [Self::Selection, Self::Ripple, Self::Roll, Self::Slip, Self::Slide, Self::RateStretch];
    pub fn key(self) -> &'static str {
        match self {
            Self::Selection => "selection",
            Self::Ripple => "ripple",
            Self::Roll => "roll",
            Self::Slip => "slip",
            Self::Slide => "slide",
            Self::RateStretch => "rate_stretch",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Selection => "Selection",
            Self::Ripple => "Ripple",
            Self::Roll => "Roll",
            Self::Slip => "Slip",
            Self::Slide => "Slide",
            Self::RateStretch => "Rate Stretch",
        }
    }
}

/// The stated mode of a selected edit point ([STU-VID-026b]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditMode {
    TrimIn,
    TrimOut,
    RippleIn,
    RippleOut,
    /// The edit at the clip's tail and the clip abutting it.
    Roll,
}

impl EditMode {
    pub fn key(self) -> &'static str {
        match self {
            Self::TrimIn => "trim_in",
            Self::TrimOut => "trim_out",
            Self::RippleIn => "ripple_in",
            Self::RippleOut => "ripple_out",
            Self::Roll => "roll",
        }
    }
    /// [STU-VID-041] command id.
    pub fn spec_command(self) -> &'static str {
        match self {
            Self::TrimIn => "cmd.timeline.editpoint.trimin",
            Self::TrimOut => "cmd.timeline.editpoint.trimout",
            Self::RippleIn => "cmd.timeline.editpoint.rippletrimin",
            Self::RippleOut => "cmd.timeline.editpoint.rippletrimout",
            Self::Roll => "cmd.timeline.editpoint.rolledit",
        }
    }
    /// The edit point sits on the clip's head.
    pub fn is_head(self) -> bool {
        matches!(self, Self::TrimIn | Self::RippleIn)
    }
    pub fn kind(self) -> TrimKind {
        match self {
            Self::TrimIn => TrimKind::TrimIn,
            Self::TrimOut => TrimKind::TrimOut,
            Self::RippleIn => TrimKind::RippleIn,
            Self::RippleOut => TrimKind::RippleOut,
            Self::Roll => TrimKind::Roll,
        }
    }
}

/// A selected edit point: the clip and the mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditPoint {
    pub clip_id: String,
    pub mode: EditMode,
}

/// The pulse edit a trim plans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrimKind {
    TrimIn,
    TrimOut,
    RippleIn,
    RippleOut,
    Roll,
    Slip,
    Slide,
    /// A rate stretch (module `rate`; planned by pulse `plan_rate_stretch`, not a `TimelineEdit`).
    RateStretch,
}

impl TrimKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::TrimIn => "trim_in",
            Self::TrimOut => "trim_out",
            Self::RippleIn => "ripple_in",
            Self::RippleOut => "ripple_out",
            Self::Roll => "roll",
            Self::Slip => "slip",
            Self::Slide => "slide",
            Self::RateStretch => "rate_stretch",
        }
    }
}

/// What one trim requested and achieved ([STU-VID-090]: the achieved delta, a determinate refusal).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrimOutcome {
    pub command: &'static str,
    pub clip_id: String,
    pub kind: TrimKind,
    pub requested_frames: i64,
    pub achieved_frames: i64,
    pub planner_calls: u32,
    /// The limit that clamped the delta (`achieved != requested`) or the refusal (`achieved == 0`).
    pub limit: Option<String>,
    pub refusal: Option<String>,
    /// Document revision after the commit (`None` when refused).
    pub revision: Option<u64>,
}

impl TrimOutcome {
    pub fn json(&self) -> Value {
        json!({
            "command": self.command,
            "clip_id": self.clip_id,
            "kind": self.kind.key(),
            "requested_frames": self.requested_frames,
            "achieved_frames": self.achieved_frames,
            "planner_calls": self.planner_calls,
            "limit": self.limit,
            "refusal": self.refusal,
            "revision": self.revision,
        })
    }
}

/// The pulse edit moving `clip_id` by `delta_frames` in `kind`, against `seq` as it is now.
pub(super) fn trim_edit(seq: &StudioSequence, clip_id: &str, kind: TrimKind, delta_frames: i64) -> Result<TimelineEdit, String> {
    let tpf = ticks_per_frame(seq);
    let delta = i128::from(delta_frames).checked_mul(i128::from(tpf)).ok_or("overflow")?;
    let (track, clip) = find_clip(seq, clip_id).ok_or("unknown_clip")?;
    let index = track.clips.iter().position(|c| c.clip_id == clip_id).ok_or("unknown_clip")?;
    let abuts = |left: Option<&hsk_studio_folio::StudioClip>, right: Option<&hsk_studio_folio::StudioClip>| match (left, right) {
        (Some(l), Some(r)) => l.timeline_end() == Some(r.timeline_start),
        _ => false,
    };
    let tick_delta = || TickDelta::new(delta).map_err(|e| e.code().to_owned());
    let id = clip_id.to_owned();
    Ok(match kind {
        TrimKind::TrimIn | TrimKind::RippleIn => {
            let head = i128::from(clip.timeline_start.value()).checked_add(delta).ok_or("overflow")?;
            let head = u64::try_from(head).map_err(|_| "outside_range")?;
            TimelineEdit::TrimIn { clip_id: id, new_head: Ticks::new(head), ripple: kind == TrimKind::RippleIn }
        }
        TrimKind::TrimOut | TrimKind::RippleOut => {
            let d = i128::from(clip.duration.value()).checked_add(delta).ok_or("overflow")?;
            if d <= 0 {
                return Err("zero_duration".into());
            }
            let d = u64::try_from(d).map_err(|_| "overflow")?;
            TimelineEdit::TrimOut { clip_id: id, new_duration: Ticks::new(d), ripple: kind == TrimKind::RippleOut }
        }
        TrimKind::Roll => {
            if !abuts(Some(clip), track.clips.get(index.saturating_add(1))) {
                return Err("not_adjacent".into());
            }
            TimelineEdit::Roll { left_clip_id: id, delta: tick_delta()? }
        }
        TrimKind::Slide => {
            let previous = index.checked_sub(1).and_then(|i| track.clips.get(i));
            if !abuts(previous, Some(clip)) || !abuts(Some(clip), track.clips.get(index.saturating_add(1))) {
                return Err("not_adjacent".into());
            }
            TimelineEdit::Slide { clip_id: id, delta: tick_delta()? }
        }
        // Source ticks = frames x sequence ticks per frame (unit-speed reading; module docs).
        TrimKind::Slip => TimelineEdit::Slip { clip_id: id, delta: tick_delta()? },
        TrimKind::RateStretch => return Err("rate_stretch_not_a_timeline_edit".into()),
    })
}

/// Result of an achieved-delta search.
pub(super) struct Achieved {
    pub(super) achieved: i64,
    pub(super) calls: u32,
    /// The code the requested delta failed with (a limit when `achieved != 0`).
    pub(super) limit: Option<String>,
    pub(super) result: Result<Planned, String>,
}

/// The largest delta toward zero (at most `|requested|`) that `attempt` admits (module docs):
/// the requested delta first; on a limit refusal a halving search keeping the largest admitted
/// magnitude; at most [`MAX_PLANNER_CALLS`] attempts; deterministic.
pub(super) fn achieve(requested: i64, mut attempt: impl FnMut(i64) -> Result<Planned, String>) -> Achieved {
    if requested == 0 {
        return Achieved { achieved: 0, calls: 0, limit: None, result: Err("no_change".into()) };
    }
    let first = match attempt(requested) {
        Ok(p) => return Achieved { achieved: requested, calls: 1, limit: None, result: Ok(p) },
        Err(code) => code,
    };
    if !LIMIT_CODES.contains(&first.as_str()) {
        return Achieved { achieved: 0, calls: 1, limit: Some(first.clone()), result: Err(first) };
    }
    let negative = requested < 0;
    let (mut lo, mut hi) = (0u64, requested.unsigned_abs());
    let mut best: Option<Planned> = None;
    let mut calls = 1u32;
    while hi.saturating_sub(lo) > 1 && calls < MAX_PLANNER_CALLS {
        let mid = lo.saturating_add(hi.saturating_sub(lo) / 2);
        let Ok(magnitude) = i64::try_from(mid) else { break };
        calls = calls.saturating_add(1);
        match attempt(if negative { magnitude.saturating_neg() } else { magnitude }) {
            Ok(p) => {
                lo = mid;
                best = Some(p);
            }
            Err(_) => hi = mid,
        }
    }
    match best {
        Some(mut p) => {
            let achieved = i64::try_from(lo).map_or(0, |v| if negative { v.saturating_neg() } else { v });
            p.notes.push(format!("achieved {achieved} of {requested} frames ({first})"));
            Achieved { achieved, calls, limit: Some(first), result: Ok(p) }
        }
        None => Achieved { achieved: 0, calls, limit: Some(first.clone()), result: Err(first) },
    }
}

/// The nearest edit to `at` in `mode` on the targeted unlocked tracks (all unlocked tracks when
/// none is targeted); ties go to the first track, then the first clip. A roll edit is a clip tail
/// abutted by the next clip. Scans every clip of the Folio-bounded sequence.
pub(super) fn nearest_edit(seq: &StudioSequence, mode: EditMode, at: Ticks) -> Option<String> {
    let any_targeted = seq.tracks.iter().any(|t| t.targeted);
    let mut best: Option<(u64, &str)> = None;
    for track in seq.tracks.iter().filter(|t| !t.locked && (t.targeted || !any_targeted)) {
        for (i, c) in track.clips.iter().enumerate() {
            let point = match mode {
                EditMode::TrimIn | EditMode::RippleIn => Some(c.timeline_start),
                EditMode::TrimOut | EditMode::RippleOut => c.timeline_end(),
                EditMode::Roll => c.timeline_end().filter(|e| track.clips.get(i.saturating_add(1)).is_some_and(|n| n.timeline_start == *e)),
            };
            let Some(point) = point else { continue };
            let distance = point.value().abs_diff(at.value());
            if best.is_none_or(|(d, _)| distance < d) {
                best = Some((distance, c.clip_id.as_str()));
            }
        }
    }
    best.map(|(_, id)| id.to_owned())
}

/// The clip abutting `clip_id`'s head (`previous`) or tail on its track.
pub(super) fn abutting(seq: &StudioSequence, clip_id: &str, previous: bool) -> Option<String> {
    let (track, clip) = find_clip(seq, clip_id)?;
    let index = track.clips.iter().position(|c| c.clip_id == clip_id)?;
    if previous {
        let p = track.clips.get(index.checked_sub(1)?)?;
        (p.timeline_end() == Some(clip.timeline_start)).then(|| p.clip_id.clone())
    } else {
        let n = track.clips.get(index.checked_add(1)?)?;
        (clip.timeline_end() == Some(n.timeline_start)).then(|| n.clip_id.clone())
    }
}

impl Viewer {
    /// Runs a trim-family command (`None`: not one; module docs).
    pub(super) fn timeline_trim_command(&mut self, command: &TimelineCommand) -> Option<Result<(), String>> {
        let key = command.key();
        Some(match command {
            TimelineCommand::RippleTrimIn { clip_id, delta_frames } => self.timeline_trim_by(key, clip_id, TrimKind::RippleIn, *delta_frames),
            TimelineCommand::RippleTrimOut { clip_id, delta_frames } => self.timeline_trim_by(key, clip_id, TrimKind::RippleOut, *delta_frames),
            TimelineCommand::Roll { clip_id, delta_frames } => self.timeline_trim_by(key, clip_id, TrimKind::Roll, *delta_frames),
            TimelineCommand::Slip { clip_id, delta_frames } => self.timeline_trim_by(key, clip_id, TrimKind::Slip, *delta_frames),
            TimelineCommand::Slide { clip_id, delta_frames } => self.timeline_trim_by(key, clip_id, TrimKind::Slide, *delta_frames),
            TimelineCommand::SelectEdit { mode, nearest_to } => self.timeline_select_edit(*mode, *nearest_to),
            TimelineCommand::EditPointAt { clip_id, mode } => self.timeline_edit_point_at(clip_id, *mode),
            TimelineCommand::TrimNudge { forward, many } => self.timeline_trim_nudge(key, *forward, *many),
            TimelineCommand::SlipNudge { right, many } => self.timeline_selected_nudge(key, TrimKind::Slip, *right, *many),
            TimelineCommand::SlideNudge { right, many } => self.timeline_selected_nudge(key, TrimKind::Slide, *right, *many),
            TimelineCommand::ClipNudge { right, many } => self.timeline_clip_nudge(*right, *many),
            TimelineCommand::ExtendEditToPlayhead => self.timeline_extend_edit(key),
            TimelineCommand::SetMultiFrameStep(step) => self.timeline_set_step(*step),
            TimelineCommand::SetTool(tool) => {
                if let Some(t) = self.timeline.as_mut() {
                    t.tool = *tool;
                }
                Ok(())
            }
            TimelineCommand::ToggleCompositePreview => {
                let on = self.timeline.as_mut().map(|t| {
                    t.composite_preview = !t.composite_preview;
                    t.composite_preview
                });
                self.say(format!("timeline: composite preview during trim {}", if on == Some(true) { "on" } else { "off" }));
                Ok(())
            }
            TimelineCommand::TrimSessionEnter => self.timeline_session_enter(),
            TimelineCommand::TrimSessionFocus(focus) => self.timeline_session_focus(*focus),
            TimelineCommand::TrimSessionToggleType => self.timeline_session_toggle_type(),
            TimelineCommand::TrimSessionCommit => self.timeline_session_commit(),
            TimelineCommand::TrimSessionRollback => self.timeline_session_rollback(),
            _ => return None,
        })
    }

    /// Runs `command` and returns what the trim achieved (the typed headless form of
    /// [STU-VID-090]); a command that is no trim returns `not_a_trim`.
    pub fn timeline_trim(&mut self, command: TimelineCommand) -> Result<TrimOutcome, String> {
        if let Some(t) = self.timeline.as_mut() {
            t.last_trim = None;
        }
        self.timeline_command(command)?;
        self.timeline.as_ref().and_then(|t| t.last_trim.clone()).ok_or_else(|| "not_a_trim".to_owned())
    }

    /// The multi-frame step (`many`) or one frame, signed.
    fn timeline_step(&self, forward: bool, many: bool) -> i64 {
        let n = if many { self.timeline.as_ref().map_or(DEFAULT_MULTI_FRAME_STEP, |t| t.multi_frame_step) } else { 1 };
        if forward { i64::from(n) } else { i64::from(n).saturating_neg() }
    }

    /// Plans `kind` on `clip_id` with the achieved-delta rule and commits it as one step.
    pub(super) fn timeline_trim_by(&mut self, command: &'static str, clip_id: &str, kind: TrimKind, requested: i64) -> Result<(), String> {
        let mut search = (0i64, 0u32, None);
        let op = format!("timeline_{command}");
        let result = self.timeline_commit(&op, |snap, seq_id, ctx| {
            let seq = sequence_in(snap, seq_id)?;
            let a = achieve(requested, |d| plan_one(snap, seq_id, ctx, &trim_edit(&seq, clip_id, kind, d)?));
            search = (a.achieved, a.calls, a.limit);
            a.result
        });
        let revision = self.run.snapshot().map(|s| s.document().revision);
        let (achieved, calls, limit) = search;
        if let Some(t) = self.timeline.as_mut() {
            t.last_trim = Some(TrimOutcome {
                command,
                clip_id: clip_id.to_owned(),
                kind,
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

    fn timeline_select_edit(&mut self, mode: EditMode, at: Ticks) -> Result<(), String> {
        let seq = self.timeline_sequence().ok_or("unknown_sequence")?;
        let clip_id = nearest_edit(seq, mode, at).ok_or("no_edit_point")?;
        if let Some(t) = self.timeline.as_mut() {
            t.selected = Some(clip_id.clone());
            t.edit_point = Some(EditPoint { clip_id, mode });
        }
        Ok(())
    }

    fn timeline_edit_point_at(&mut self, clip_id: &str, mode: EditMode) -> Result<(), String> {
        let seq = self.timeline_sequence().ok_or("unknown_sequence")?;
        find_clip(seq, clip_id).ok_or("unknown_clip")?;
        if let Some(t) = self.timeline.as_mut() {
            t.selected = Some(clip_id.to_owned());
            t.edit_point = Some(EditPoint { clip_id: clip_id.to_owned(), mode });
        }
        Ok(())
    }

    fn timeline_trim_nudge(&mut self, command: &'static str, forward: bool, many: bool) -> Result<(), String> {
        if self.timeline.as_ref().is_some_and(|t| t.session.is_some()) {
            return self.timeline_session_adjust(forward, many);
        }
        let point = self.timeline.as_ref().and_then(|t| t.edit_point.clone()).ok_or("no_edit_point")?;
        let step = self.timeline_step(forward, many);
        self.timeline_trim_by(command, &point.clip_id, point.mode.kind(), step)
    }

    fn timeline_selected_nudge(&mut self, command: &'static str, kind: TrimKind, right: bool, many: bool) -> Result<(), String> {
        let clip_id = self.timeline.as_ref().and_then(|t| t.selected.clone()).ok_or("no_selection")?;
        let step = self.timeline_step(right, many);
        self.timeline_trim_by(command, &clip_id, kind, step)
    }

    fn timeline_clip_nudge(&mut self, right: bool, many: bool) -> Result<(), String> {
        let clip_id = self.timeline.as_ref().and_then(|t| t.selected.clone()).ok_or("no_selection")?;
        let seq = self.timeline_sequence().ok_or("unknown_sequence")?;
        let start = find_clip(seq, &clip_id).map(|(_, c)| c.timeline_start).ok_or("unknown_clip")?;
        let delta = i128::from(self.timeline_step(right, many)).saturating_mul(i128::from(ticks_per_frame(seq)));
        let to = i128::from(start.value()).saturating_add(delta);
        let to = u64::try_from(to).map_err(|_| "outside_range")?;
        self.timeline_command_inner(&TimelineCommand::MoveClip { clip_id, start: Ticks::new(to), track_id: None })
    }

    fn timeline_extend_edit(&mut self, command: &'static str) -> Result<(), String> {
        let here = self.timeline_playhead().ok_or("no_program_monitor")?;
        let seq = self.timeline_sequence().ok_or("unknown_sequence")?;
        let selected = self.timeline.as_ref().and_then(|t| t.edit_point.clone()).filter(|e| e.mode == EditMode::Roll && abutting(seq, &e.clip_id, false).is_some());
        let clip_id = match selected {
            Some(e) => e.clip_id,
            None => nearest_edit(seq, EditMode::Roll, here).ok_or("no_edit_point")?,
        };
        let tpf = ticks_per_frame(seq);
        let end = find_clip(seq, &clip_id).and_then(|(_, c)| c.timeline_end()).ok_or("unknown_clip")?;
        let frames = i128::from(here.value().checked_div(tpf).unwrap_or(0)).saturating_sub(i128::from(end.value().checked_div(tpf).unwrap_or(0)));
        let frames = i64::try_from(frames).map_err(|_| "overflow")?;
        if let Some(t) = self.timeline.as_mut() {
            t.edit_point = Some(EditPoint { clip_id: clip_id.clone(), mode: EditMode::Roll });
        }
        self.timeline_trim_by(command, &clip_id, TrimKind::Roll, frames)
    }

    fn timeline_set_step(&mut self, step: u32) -> Result<(), String> {
        if !(1..=MAX_MULTI_FRAME_STEP).contains(&step) {
            return Err("step_out_of_range".into());
        }
        if let Some(t) = self.timeline.as_mut() {
            t.multi_frame_step = step;
        }
        self.say(format!("timeline: multi-frame step {step}"));
        Ok(())
    }
}
