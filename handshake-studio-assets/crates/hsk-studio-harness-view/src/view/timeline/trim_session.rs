//! The trim session of the timeline panel (lane SMB-TIMELINE-TRIM-1, [STU-VID-026a]).
//!
//! # Manual
//!
//! Select an edit point (click a clip edge, or [`TimelineCommand::SelectEdit`]), then press
//! **Trim Session** (`timeline.trim_session.enter`, [`TimelineCommand::TrimSessionEnter`]). The
//! session row shows the outgoing clip (the clip before the edit; its source out tick) and the
//! incoming clip (the clip after it; its source in tick) side by side as labelled tiles
//! (`timeline.trim_session.outgoing` / `.incoming`), the buttons minus major / minus minor / plus
//! minor / plus major (one frame or the multi-frame step; [`TimelineCommand::TrimNudge`]), the
//! focus In / Out / In+Out ([`TimelineCommand::TrimSessionFocus`]), the ripple / roll type toggle
//! ([`TimelineCommand::TrimSessionToggleType`]), the composite preview switch, **Apply**
//! ([`TimelineCommand::TrimSessionCommit`]: ONE history step) and **Cancel**
//! ([`TimelineCommand::TrimSessionRollback`]: abandons the session; nothing was committed).
//!
//! Model: focus Out ripple-trims the outgoing tail, focus In ripple-trims the incoming head, focus
//! In+Out rolls the edit; so the type is roll exactly when the focus is In+Out and the toggle
//! switches between In+Out and Out (In when there is no outgoing clip). A focus or type change
//! starts the adjustment over at zero. Every adjustment is planned (not committed) with the
//! achieved-delta rule of module `trim`, so the pending delta and the tiles show what Apply will
//! commit; a clamp names its limit in `trim_session.limit`. The session is a panel-local draft:
//! Apply re-plans on the document as it is then.
//!
//! NOT_PROVEN here: the outgoing / incoming frame PIXELS and the composite preview picture (the
//! program monitor binds no frame decoder to a document sequence); the tiles show source ticks.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::trim::{EditMode, TrimKind, abutting, achieve, trim_edit};
use super::{TimelineCommand, find_clip, plan_one, sequence_in};
use hsk_studio_folio::StudioSequence;
use serde_json::{Value, json};

/// Which side of the edit a session adjusts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrimFocus {
    In,
    Out,
    Both,
}

impl TrimFocus {
    pub fn key(self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
            Self::Both => "both",
        }
    }
    /// [STU-VID-041] command id.
    pub fn spec_command(self) -> &'static str {
        match self {
            Self::In => "cmd.trim.focusin",
            Self::Out => "cmd.trim.focusout",
            Self::Both => "cmd.trim.focusinandout",
        }
    }
}

/// An open trim session (module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrimSession {
    pub outgoing: Option<String>,
    pub incoming: Option<String>,
    pub focus: TrimFocus,
    /// The adjustment last asked for and what pulse admits of it (what Apply commits).
    pub requested_frames: i64,
    pub pending_frames: i64,
    pub limit: Option<String>,
    /// Source out tick of the outgoing clip and source in tick of the incoming clip with the
    /// pending adjustment applied.
    pub outgoing_source_out: Option<u64>,
    pub incoming_source_in: Option<u64>,
}

impl TrimSession {
    /// "ripple" or "roll" ([STU-VID-026a] type toggle).
    pub fn trim_type(&self) -> &'static str {
        if self.focus == TrimFocus::Both { "roll" } else { "ripple" }
    }
    /// The clip and the pulse edit the session adjusts.
    pub fn edit(&self) -> Result<(String, TrimKind), &'static str> {
        match self.focus {
            TrimFocus::Out => self.outgoing.clone().map(|c| (c, TrimKind::RippleOut)).ok_or("no_outgoing_clip"),
            TrimFocus::In => self.incoming.clone().map(|c| (c, TrimKind::RippleIn)).ok_or("no_incoming_clip"),
            TrimFocus::Both => match (&self.outgoing, &self.incoming) {
                (Some(o), Some(_)) => Ok((o.clone(), TrimKind::Roll)),
                _ => Err("roll_needs_two_clips"),
            },
        }
    }
    pub fn json(&self) -> Value {
        json!({
            "outgoing": self.outgoing,
            "incoming": self.incoming,
            "focus": self.focus.key(),
            "type": self.trim_type(),
            "requested_frames": self.requested_frames,
            "pending_frames": self.pending_frames,
            "limit": self.limit,
            "outgoing_source_out": self.outgoing_source_out,
            "incoming_source_in": self.incoming_source_in,
        })
    }
    /// Tile ticks from `seq` (the document, or the planned preview).
    fn read_ticks(&mut self, seq: &StudioSequence) {
        self.outgoing_source_out = self.outgoing.as_deref().and_then(|id| find_clip(seq, id)).map(|(_, c)| c.source_out.value());
        self.incoming_source_in = self.incoming.as_deref().and_then(|id| find_clip(seq, id)).map(|(_, c)| c.source_in.value());
    }
    fn restart(&mut self, seq: Option<&StudioSequence>) {
        self.requested_frames = 0;
        self.pending_frames = 0;
        self.limit = None;
        if let Some(s) = seq {
            self.read_ticks(s);
        }
    }
}

impl Viewer {
    fn trim_session_open(&self) -> Result<&TrimSession, String> {
        self.timeline.as_ref().and_then(|t| t.session.as_ref()).ok_or_else(|| "no_trim_session".to_owned())
    }

    pub(super) fn timeline_session_enter(&mut self) -> Result<(), String> {
        if self.trim_session_open().is_ok() {
            return Err("trim_session_open".into());
        }
        let point = self.timeline.as_ref().and_then(|t| t.edit_point.clone()).ok_or("no_edit_point")?;
        let seq = self.timeline_sequence().ok_or("unknown_sequence")?;
        find_clip(seq, &point.clip_id).ok_or("unknown_clip")?;
        let (outgoing, incoming, focus) = match point.mode {
            EditMode::TrimIn | EditMode::RippleIn => (abutting(seq, &point.clip_id, true), Some(point.clip_id.clone()), TrimFocus::In),
            EditMode::TrimOut | EditMode::RippleOut => (Some(point.clip_id.clone()), abutting(seq, &point.clip_id, false), TrimFocus::Out),
            EditMode::Roll => (Some(point.clip_id.clone()), abutting(seq, &point.clip_id, false), TrimFocus::Both),
        };
        let mut session = TrimSession { outgoing, incoming, focus, requested_frames: 0, pending_frames: 0, limit: None, outgoing_source_out: None, incoming_source_in: None };
        session.edit()?;
        session.read_ticks(seq);
        let label = format!("timeline: trim session on {} ({})", point.clip_id, session.trim_type());
        if let Some(t) = self.timeline.as_mut() {
            t.session = Some(session);
        }
        self.say(label);
        Ok(())
    }

    /// Plus / minus one frame or the multi-frame step: re-plans (never commits) the new total.
    pub(super) fn timeline_session_adjust(&mut self, forward: bool, many: bool) -> Result<(), String> {
        let session = self.trim_session_open()?.clone();
        let (clip_id, kind) = session.edit()?;
        let step = if many { self.timeline.as_ref().map_or(1, |t| t.multi_frame_step) } else { 1 };
        let step = i64::from(step);
        let requested = if forward { session.pending_frames.saturating_add(step) } else { session.pending_frames.saturating_sub(step) };
        if requested == 0 {
            let seq = self.timeline_sequence().cloned();
            if let Some(s) = self.timeline.as_mut().and_then(|t| t.session.as_mut()) {
                s.restart(seq.as_ref());
            }
            return Ok(());
        }
        let a = self.timeline_plan(|snap, seq_id, ctx| {
            let seq = sequence_in(snap, seq_id)?;
            Ok(achieve(requested, |d| plan_one(snap, seq_id, ctx, &trim_edit(&seq, &clip_id, kind, d)?)))
        })?;
        let Some(s) = self.timeline.as_mut().and_then(|t| t.session.as_mut()) else { return Err("no_trim_session".into()) };
        s.requested_frames = requested;
        match a.result {
            Ok(planned) => {
                let moved = a.achieved != s.pending_frames;
                s.pending_frames = a.achieved;
                s.limit = a.limit.clone();
                s.read_ticks(&planned.sequence);
                if moved { Ok(()) } else { Err(a.limit.unwrap_or_else(|| "no_change".into())) }
            }
            Err(code) => {
                s.limit = Some(code.clone());
                Err(code)
            }
        }
    }

    pub(super) fn timeline_session_focus(&mut self, focus: TrimFocus) -> Result<(), String> {
        let mut session = self.trim_session_open()?.clone();
        session.focus = focus;
        session.edit()?;
        let seq = self.timeline_sequence().cloned();
        session.restart(seq.as_ref());
        if let Some(t) = self.timeline.as_mut() {
            t.session = Some(session);
        }
        Ok(())
    }

    pub(super) fn timeline_session_toggle_type(&mut self) -> Result<(), String> {
        let session = self.trim_session_open()?;
        let focus = match session.focus {
            TrimFocus::Both if session.outgoing.is_some() => TrimFocus::Out,
            TrimFocus::Both => TrimFocus::In,
            TrimFocus::In | TrimFocus::Out => TrimFocus::Both,
        };
        self.timeline_session_focus(focus)
    }

    /// Commits the pending adjustment as ONE history step (nothing pending: just closes).
    pub(super) fn timeline_session_commit(&mut self) -> Result<(), String> {
        let session = self.trim_session_open()?.clone();
        if session.pending_frames != 0 {
            let (clip_id, kind) = session.edit()?;
            let result = self.timeline_trim_by(TimelineCommand::TrimSessionCommit.key(), &clip_id, kind, session.pending_frames);
            if let Err(e) = result {
                if let Some(s) = self.timeline.as_mut().and_then(|t| t.session.as_mut()) {
                    s.limit = Some(e.clone());
                }
                return Err(e);
            }
        } else {
            self.say("timeline: trim session closed (nothing to apply)".to_owned());
        }
        if let Some(t) = self.timeline.as_mut() {
            t.session = None;
        }
        Ok(())
    }

    pub(super) fn timeline_session_rollback(&mut self) -> Result<(), String> {
        self.trim_session_open()?;
        if let Some(t) = self.timeline.as_mut() {
            t.session = None;
        }
        self.say("timeline: trim session rolled back (nothing committed)".to_owned());
        Ok(())
    }
}
