//! Nesting in the timeline panel (lane SMB-PR-NEST-1; [STU-VID-030] "A sequence may be a clip").
//! Manual:
//!
//! - **Nest** (toolbar `Nest`, widget `timeline.nest`, `TimelineCommand::Nest(clip ids)`, empty =
//!   the selected clip; `cmd.clip.nestify`): pulse `plan_nest` moves the clips and their whole link
//!   groups into a new sequence (`nest-<n>`, named `Nested Sequence <n>`) and puts one nested clip per
//!   track kind on the lowest selected track of that kind, linked. It commits as ONE undo step
//!   through `Run::submit_sequence_edits` (op `sequence.nest`; the history label is `sequence.nest
//!   <new id>,<parent id>`). The session's receipt names its provider `multicam.step`: the session's
//!   only multi-sequence step is the multicam one, reused unchanged with the multicam records passed
//!   through, so that provider name is a session reading the label does not repeat.
//! - **Make Subsequence** (`Make Subsequence`, `Shift+U`, `cmd.sequence.makesubsequence`): a new
//!   sequence (`subsequence-<n>`) from copies of the given clips, else the selected clip, else the
//!   program monitor's in-to-out range (`cmd.sequence.makesubsequencefromintoout`, cut at the
//!   edges on the targeted tracks); the open sequence is unchanged. One undo step (op
//!   `sequence.make_subsequence`).
//! - **Open Nested** (`Open Nested`, `Ctrl+Alt+F`, a double-click on a nested clip;
//!   `cmd.tlnav.reveal.nested.sequence`, the explicit reveal command): the timeline and the program
//!   monitor switch to the nested sequence, the playhead mapped into it (source tick of the clip at
//!   the playhead, else 0); the sequence left goes on the back stack (`state_json()["timeline"]
//!   ["nest_back"]`, the toolbar shows the path). **Back to Parent** returns to it with its
//!   playhead.
//! - A nested clip is an ordinary clip to every timeline edit (trim, move, razor): its source
//!   length is the nested sequence's duration (pulse `NestedExtents`, used by every planner here),
//!   so a trim moves only the window onto the nested sequence ([STU-VID-030] "opaque").
//! - The program monitor shows nested clips through pulse `frame_layers_nested`; the timeline keeps
//!   its document sequences current (`nest_sync_monitor`).
//!
//! Studio readings: Premiere's receiving track, track mirroring and range cut are UNVERIFIED (D5;
//! pulse module `nest`). Declared gap: a clip with a clip gain or pan envelope in the audio routing
//! entry is refused `nest_clip_envelopes_unsupported` (the envelope is keyed by the parent sequence;
//! moving it with the clip is not implemented). Ids are minted here (pulse mints none).
#![deny(clippy::arithmetic_side_effects)]

use super::{TimelineCommand, envelopes, find_clip, mint_clip_ids, mint_key, sequence_in};
use crate::view::Viewer;
use hsk_studio_accord::Ticks;
use hsk_studio_folio::{self as folio, ClipSource, StudioSequence};
use hsk_studio_pulse::{NestIds, NestProposal, NestedExtents, SequenceChange, SubsequenceSource, TickRange, clip_source_at, plan_make_subsequence, plan_nest};
use hsk_studio_session::SequenceEdit;
use std::collections::BTreeSet;

/// Whether `clip_id` of `seq` is a nested sequence clip.
pub fn is_nested(seq: &StudioSequence, clip_id: &str) -> bool {
    seq.tracks.iter().flat_map(|t| t.clips.iter()).any(|c| c.clip_id == clip_id && matches!(c.source_ref, ClipSource::Sequence { .. }))
}

/// `count` fresh `STRK` ids unused by the document (same scheme as clip ids, own block `9001`).
fn mint_track_ids(doc: &folio::StudioDocument, count: usize) -> Vec<String> {
    let uuid = doc.document_id.split_once('-').map_or("", |(_, u)| u);
    let hex = |s: Option<&str>, n: usize| s.filter(|v| v.len() == n && v.bytes().all(|b| b.is_ascii_hexdigit())).map_or_else(|| "0".repeat(n), str::to_owned);
    let (high, low) = (hex(uuid.get(0..8), 8), hex(uuid.get(9..13), 4));
    let used: BTreeSet<&str> = doc.sequences.iter().flat_map(|s| s.tracks.iter()).map(|t| t.track_id.as_str()).collect();
    let mut out = Vec::with_capacity(count);
    let mut n: u64 = 1;
    while out.len() < count {
        let id = format!("STRK-{high}-{low}-7e1e-9001-{n:012x}");
        if !used.contains(id.as_str()) {
            out.push(id);
        }
        n = n.wrapping_add(1);
    }
    out
}

/// `count` keys `prefix-<n>` unused by `taken`.
fn mint_keys(prefix: &str, taken: &BTreeSet<&str>, count: usize) -> Vec<String> {
    let mut out = Vec::with_capacity(count);
    let mut n: u64 = 1;
    while out.len() < count {
        let id = format!("{prefix}-{n}");
        if !taken.contains(id.as_str()) {
            out.push(id);
        }
        n = n.wrapping_add(1);
    }
    out
}

/// Every marker and transition id of the document.
fn record_ids(doc: &folio::StudioDocument) -> BTreeSet<&str> {
    let mut out = BTreeSet::new();
    for s in &doc.sequences {
        out.extend(s.markers.iter().map(|m| m.marker_id.as_str()));
        for c in s.tracks.iter().flat_map(|t| t.clips.iter()) {
            out.extend(c.markers.iter().map(|m| m.marker_id.as_str()));
            out.extend([&c.transitions.head, &c.transitions.tail].into_iter().flatten().map(|t| t.transition_id.as_str()));
        }
    }
    out
}

/// Ids for one nest or subsequence of `seq` touching at most `clips` clips with `records` records.
fn ids_for(doc: &folio::StudioDocument, seq: &StudioSequence, prefix: &str, label: &str, clips: usize, records: usize) -> NestIds {
    let sequences: BTreeSet<&str> = doc.sequences.iter().map(|s| s.sequence_id.as_str()).collect();
    let sequence_id = mint_key(prefix, &sequences);
    let n = sequence_id.rsplit('-').next().unwrap_or("1").to_owned();
    let groups: BTreeSet<&str> = seq.tracks.iter().flat_map(|t| t.clips.iter()).filter_map(|c| c.link_group.as_deref()).collect();
    NestIds {
        sequence_name: format!("{label} {n}"),
        track_ids: mint_track_ids(doc, seq.tracks.len()),
        clip_ids: mint_clip_ids(doc, clips),
        link_group: Some(mint_key("link-nest", &groups)),
        record_ids: mint_keys("rec-nest", &record_ids(doc), records),
        sequence_id,
    }
}

/// Clips (and their markers and transitions) a subsequence of `seq` can copy at most.
fn copy_counts(seq: &StudioSequence, keep: impl Fn(&folio::StudioTrack, &folio::StudioClip) -> bool) -> (usize, usize) {
    seq.tracks.iter().flat_map(|t| t.clips.iter().map(move |c| (t, c))).filter(|(t, c)| keep(t, c)).fold((0usize, 0usize), |(n, r), (_, c)| {
        let transitions = usize::from(c.transitions.head.is_some()).saturating_add(usize::from(c.transitions.tail.is_some()));
        (n.saturating_add(1), r.saturating_add(c.markers.len()).saturating_add(transitions))
    })
}

fn to_session(changes: &[SequenceChange]) -> Vec<SequenceEdit> {
    changes
        .iter()
        .map(|c| match c {
            SequenceChange::Replace { expect, write } => SequenceEdit::Replace { expect: expect.clone(), write: write.clone() },
            SequenceChange::Insert { at, write } => SequenceEdit::Insert { at: *at, write: write.clone() },
            SequenceChange::Remove { expect } => SequenceEdit::Remove { expect: expect.clone() },
        })
        .collect()
}

impl Viewer {
    /// The nesting commands (module docs); `None` for every other command.
    pub(super) fn timeline_nest_command(&mut self, command: &TimelineCommand) -> Option<Result<(), String>> {
        Some(match command {
            TimelineCommand::Nest(ids) => self.timeline_nest(ids),
            TimelineCommand::MakeSubsequence(ids) => self.timeline_make_subsequence(ids),
            TimelineCommand::OpenNested(id) => self.timeline_open_nested(id.clone()),
            TimelineCommand::BackToParent => self.timeline_back_to_parent(),
            _ => return None,
        })
    }

    /// The program monitor resolves nested clips against the current document (one copy of the
    /// document's sequences per revision).
    pub(super) fn nest_sync_monitor(&mut self) {
        let Some(doc) = self.run.snapshot().map(|s| s.document()) else { return };
        let Some(m) = self.monitor.as_mut() else { return };
        if m.monitor.document_revision() != Some(doc.revision) {
            m.monitor.set_document(Some(doc.revision), doc.sequences.clone());
        }
    }

    fn nest_selection(&self, ids: &[String]) -> Vec<String> {
        if ids.is_empty() { self.timeline.as_ref().and_then(|t| t.selected.clone()).into_iter().collect() } else { ids.to_vec() }
    }

    fn timeline_nest(&mut self, ids: &[String]) -> Result<(), String> {
        let clips = self.nest_selection(ids);
        if clips.is_empty() {
            return Err("no_selection".into());
        }
        let proposal = self.timeline_plan(|snap, seq_id, ctx| {
            let doc = snap.document();
            let seq = sequence_in(snap, seq_id)?;
            let ids = ids_for(doc, &seq, "nest", "Nested Sequence", 4, 0);
            plan_nest(snap, seq_id, doc.revision, &clips, &ids, ctx).map_err(|e| e.code().to_owned())
        })?;
        self.nest_envelope_guard(&proposal)?;
        self.nest_commit("sequence.nest", &proposal)?;
        if let Some(t) = self.timeline.as_mut() {
            t.selected = proposal.nested_clip_ids().first().cloned();
        }
        Ok(())
    }

    /// Refuses a nest that would move a clip whose envelope the routing entry keys by the parent.
    fn nest_envelope_guard(&self, proposal: &NestProposal) -> Result<(), String> {
        let Some(SequenceChange::Replace { expect, write }) = proposal.changes().get(1) else { return Ok(()) };
        let kept: BTreeSet<&str> = write.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| c.clip_id.as_str()).collect();
        let moved: BTreeSet<&str> = expect.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| c.clip_id.as_str()).filter(|id| !kept.contains(id)).collect();
        let Some(bytes) = self.run.document_entry(envelopes::ROUTING_ENTRY) else { return Ok(()) };
        let Ok(set) = serde_json::from_slice::<folio::StudioAudioRoutingSet>(bytes) else { return Ok(()) };
        let enveloped = set
            .routings
            .iter()
            .filter(|r| r.sequence_id == expect.sequence_id)
            .flat_map(|r| r.clip_gains.iter().map(|g| g.clip_id.as_str()).chain(r.clip_pans.iter().map(|p| p.clip_id.as_str())))
            .find(|id| moved.contains(id));
        match enveloped {
            Some(id) => Err(format!("nest_clip_envelopes_unsupported {id}")),
            None => Ok(()),
        }
    }

    fn timeline_make_subsequence(&mut self, ids: &[String]) -> Result<(), String> {
        let clips = self.nest_selection(ids);
        let source = if clips.is_empty() {
            let m = self.monitor.as_ref().ok_or("no_selection")?;
            let range: TickRange = m.monitor.scheduler().in_out().ok_or("no_selection_or_in_out")?;
            SubsequenceSource::Range(range)
        } else {
            SubsequenceSource::Clips(clips)
        };
        let proposal = self.timeline_plan(|snap, seq_id, ctx| {
            let doc = snap.document();
            let seq = sequence_in(snap, seq_id)?;
            let (n, r) = match &source {
                SubsequenceSource::Clips(_) => copy_counts(&seq, |_, _| true),
                SubsequenceSource::Range(span) => {
                    let any = seq.tracks.iter().any(|t| t.targeted);
                    copy_counts(&seq, |t, c| (!any || t.targeted) && c.timeline_start < span.end() && c.timeline_end().is_some_and(|e| e > span.start()))
                }
            };
            let ids = ids_for(doc, &seq, "subsequence", "Subsequence", n, r);
            plan_make_subsequence(snap, seq_id, doc.revision, &source, &ids, ctx).map_err(|e| e.code().to_owned())
        })?;
        self.nest_commit("sequence.make_subsequence", &proposal)
    }

    /// Commits a proposal as ONE undo step and lets the monitor and mixer follow.
    fn nest_commit(&mut self, op: &str, proposal: &NestProposal) -> Result<(), String> {
        if !self.run.submit_sequence_edits(op, to_session(proposal.changes())) {
            let code = self.run.ops().last().and_then(|o| o.get("error").and_then(serde_json::Value::as_str)).unwrap_or("submit_failed").to_owned();
            return Err(format!("submit_failed {code}"));
        }
        let revision = self.run.snapshot().map(|s| s.document().revision);
        self.nest_sync_monitor();
        let current = self.timeline.as_ref().map(|t| t.sequence_id.clone()).unwrap_or_default();
        let edited = self.run.snapshot().and_then(|s| sequence_in(s, &current).ok());
        if let (Some(seq), Some(m)) = (edited, self.monitor.as_mut())
            && m.monitor.sequence().sequence_id == current
            && *m.monitor.sequence() != seq
            && let Err(e) = m.monitor.replace_sequence(seq, &m.clock)
        {
            self.say(format!("timeline: program monitor kept the previous sequence ({} {})", e.code, e.detail));
        }
        if let Some(t) = self.timeline.as_mut() {
            t.synced_revision = revision;
        }
        #[cfg(feature = "audio-batch")]
        self.timeline_audio_follow();
        let dropped = if proposal.dropped_transitions().is_empty() { String::new() } else { format!("; dropped transitions {:?}", proposal.dropped_transitions()) };
        self.say(format!("timeline: {op} created sequence {} (revision {}){dropped}", proposal.sequence_id(), revision.unwrap_or(0)));
        Ok(())
    }

    /// Switches the timeline and the program monitor to `sequence_id` at `playhead`.
    fn nest_switch(&mut self, sequence_id: &str, playhead: Ticks) -> Result<(), String> {
        let seq = self.run.snapshot().ok_or("not_open").and_then(|s| sequence_in(s, sequence_id).map_err(|_| "unknown_sequence"))?;
        let revision = self.run.snapshot().map(|s| s.document().revision);
        if let Some(t) = self.timeline.as_mut() {
            t.sequence_id = sequence_id.to_owned();
            t.selected = None;
            t.drag = None;
            t.edit_point = None;
            t.session = None;
            t.speed_form = None;
            t.cache = None;
            t.layout = None;
            t.synced_revision = revision;
        }
        if let Some(m) = self.monitor.as_mut() {
            m.monitor.replace_sequence(seq, &m.clock).map_err(|e| format!("program monitor: {} {}", e.code, e.detail))?;
            let last = m.monitor.scheduler().range().end().value().saturating_sub(1);
            m.monitor.scrub(Ticks::new(playhead.value().min(last)), &m.clock).map_err(|e| format!("program monitor: {} {}", e.code, e.detail))?;
        }
        #[cfg(feature = "audio-batch")]
        self.timeline_audio_follow();
        Ok(())
    }

    fn timeline_open_nested(&mut self, clip_id: Option<String>) -> Result<(), String> {
        let id = clip_id.or_else(|| self.timeline.as_ref().and_then(|t| t.selected.clone())).ok_or("no_selection")?;
        let parent = self.timeline_sequence().cloned().ok_or("unknown_sequence")?;
        let (_, clip) = find_clip(&parent, &id).ok_or("unknown_clip")?;
        let ClipSource::Sequence { sequence_id } = &clip.source_ref else { return Err("not_nested".into()) };
        let playhead = self.timeline_playhead();
        // The playhead inside the clip maps to the nested sequence's tick; elsewhere it starts at 0.
        let inside = playhead.filter(|p| clip.timeline_start <= *p && clip.timeline_end().is_some_and(|e| *p < e));
        let mapped = match (inside, self.run.snapshot()) {
            (Some(p), Some(s)) => {
                let ext = NestedExtents::new(&s.document().sequences, &hsk_studio_pulse::UnknownExtents);
                clip_source_at(clip, p, &ext, true).map_or(Ticks::new(0), |(t, _)| t)
            }
            _ => Ticks::new(0),
        };
        let target = sequence_id.clone();
        self.nest_switch(&target, mapped)?;
        if let Some(t) = self.timeline.as_mut() {
            t.nest_back.push((parent.sequence_id.clone(), playhead));
        }
        self.say(format!("timeline: opened nested sequence {target} from {}", parent.sequence_id));
        Ok(())
    }

    fn timeline_back_to_parent(&mut self) -> Result<(), String> {
        let (parent, playhead) = self.timeline.as_mut().and_then(|t| t.nest_back.pop()).ok_or("no_parent")?;
        if let Err(e) = self.nest_switch(&parent, playhead.unwrap_or(Ticks::new(0))) {
            // The parent is gone (an undo removed it): the back stack stays as it was.
            if let Some(t) = self.timeline.as_mut() {
                t.nest_back.push((parent, playhead));
            }
            return Err(e);
        }
        self.say(format!("timeline: back to sequence {parent}"));
        Ok(())
    }
}
