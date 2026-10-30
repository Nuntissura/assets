//! Clip volume envelopes follow timeline edits (lane SMB-AU-MULTITRACK-2, data integrity). A clip
//! gain envelope (Folio `StudioClipGainEnvelope` in the audio routing entry) holds offsets from its
//! clip's `timeline_start`; every timeline edit that changes a clip's source window, speed, length or
//! existence rewrites the envelopes of the edited sequence in the SAME history step as the sequence
//! (`Run::submit_sequence_and_entry`, [STU-VID-040] one reversible step per operation):
//!
//! - **Anchored to the audio.** A point stays on the source sample it was on: with the source
//!   position `s = source_in + offset * rate` (reversed clips: `source_out - offset * rate`, rate
//!   `|numerator| / denominator`) the new offset is where `s` plays in the edited clip, rounded down
//!   to a tick. So trim in shifts the offsets by the trimmed amount, slip shifts them by the slip,
//!   moves, slides and trim out keep them, rate stretch scales them.
//! - **Cut at the clip edges.** Points that land before 0 or after the clip's duration are dropped
//!   and an interpolated point (linear in dB between the neighbouring points, held outside them, as
//!   the mix reads the envelope) is put at that edge, so the gain at the new edge is unchanged
//!   (declared reading; Premiere's / Audition's own handling is UNVERIFIED, D5).
//! - **Split clips inherit.** A clip of the edited sequence that did not exist before (razor's right
//!   part, the remnant of an overwrite, the shifted right part of an insert that splits a
//!   sync-locked audio clip) gets the envelope of the old clip it was cut from: same audio track,
//!   same source and speed, its source window inside the old clip's window and apart from the window
//!   the old clip keeps (wherever it now sits on the timeline). Both parts then hold their points
//!   plus an edge point. (A fresh audio clip is always a remnant: timeline and source-monitor edits
//!   add new clips only to video tracks or as cut pieces.)
//! - **Removed clips lose theirs.** Lift, ripple delete and an overwrite that covers a clip remove the
//!   clip's envelope, so the routing never names a clip that is gone.
//!
//! Clip pan envelopes (lane SMB-AU-MULTITRACK-3, Folio `StudioClipPanEnvelope`) follow by the same
//! rules (edge points interpolated linearly in pan, as the mix reads them), in the same step.
//!
//! The rewritten routing set is validated by Folio against the edited document before anything is
//! committed; a refusal refuses the whole edit (`routing_invalid`). Documents without envelopes for
//! the edited clips commit the sequence alone (their routing entry stays byte-identical). An entry
//! that does not validate BEFORE the edit is left untouched and reported.

use hsk_studio_folio::{StudioClip, StudioClipGainEnvelope, StudioClipGainPoint, StudioClipPanEnvelope, StudioClipPanPoint, StudioSequence, TrackKind};
use std::collections::{BTreeMap, BTreeSet};

/// Session entry of the audio routing set (`audio_session::AUDIO_ROUTING_ENTRY`; repeated here so
/// the timeline maintains envelopes without the `audio-batch` feature).
pub(in crate::view) const ROUTING_ENTRY: &str = "audio_routing";

/// Audio clips of `seq` by id with their track id.
fn audio_clips(seq: &StudioSequence) -> BTreeMap<&str, (&str, &StudioClip)> {
    seq.tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .flat_map(|t| t.clips.iter().map(move |c| (c.clip_id.as_str(), (t.track_id.as_str(), c))))
        .collect()
}

/// The source anchor of a clip: `(anchor tick, |numerator|, denominator, reversed)`.
fn anchor(c: &StudioClip) -> (i128, i128, i128, bool) {
    let reversed = c.speed.is_reversed();
    let at = if reversed { c.source_out.value() } else { c.source_in.value() };
    (i128::from(at), i128::from(c.speed.numerator.unsigned_abs()), i128::from(c.speed.denominator), reversed)
}

/// Offset in `new` (ticks from its `timeline_start`, may be negative) of the source sample at
/// `offset` in `old`; `None` on overflow or a zero rate.
fn remap_offset(old: &StudioClip, new: &StudioClip, offset: u64) -> Option<i128> {
    let (a0, n0, d0, r0) = anchor(old);
    let (a1, n1, d1, r1) = anchor(new);
    // s = a0 +- offset * n0 / d0, as S / d0.
    let moved = i128::from(offset).checked_mul(n0)?;
    let s = a0.checked_mul(d0)?;
    let s = if r0 { s.checked_sub(moved)? } else { s.checked_add(moved)? };
    // offset' = +-(s - a1) / (n1 / d1) = +-(S - a1 d0) d1 / (d0 n1).
    let base = a1.checked_mul(d0)?;
    let diff = if r1 { base.checked_sub(s)? } else { s.checked_sub(base)? };
    let num = diff.checked_mul(d1)?;
    let den = d0.checked_mul(n1)?;
    (den > 0).then(|| num.div_euclid(den))
}

/// Value at `t` of `points` (`(position, dB)` sorted): linear between points, held outside them.
fn value_at(points: &[(f64, f64)], t: f64) -> f64 {
    let Some(first) = points.first() else { return 0.0 };
    if t <= first.0 {
        return first.1;
    }
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        if t <= b.0 {
            let span = (b.0 - a.0).max(f64::MIN_POSITIVE);
            return a.1 + (b.1 - a.1) * ((t - a.0) / span);
        }
    }
    points.last().map_or(0.0, |p| p.1)
}

/// The envelope `points` (`(offset ticks, value)`) of `old` rewritten for `new` (module docs);
/// `None` on overflow.
fn remap(old: &StudioClip, new: &StudioClip, points: &[(u64, f64)]) -> Option<Vec<(u64, f64)>> {
    if old.source_in == new.source_in && old.source_out == new.source_out && old.speed == new.speed && old.duration == new.duration {
        return Some(points.to_vec());
    }
    let end = i128::from(new.duration.value());
    let mut mapped: Vec<(i128, f64)> = Vec::with_capacity(points.len());
    for &(offset, value) in points {
        mapped.push((remap_offset(old, new, offset)?, value));
    }
    // A reversed / unreversed clip runs its points the other way.
    mapped.sort_by_key(|m| m.0);
    let curve: Vec<(f64, f64)> = mapped.iter().map(|&(o, g)| (o as f64, g)).collect();
    let mut out: Vec<(u64, f64)> = Vec::with_capacity(mapped.len().saturating_add(2));
    let push = |out: &mut Vec<(u64, f64)>, at: i128, value: f64| {
        let Ok(offset_ticks) = u64::try_from(at) else { return };
        if out.last().is_none_or(|l| l.0 < offset_ticks) {
            out.push((offset_ticks, value));
        }
    };
    if mapped.first().is_some_and(|m| m.0 < 0) && !mapped.iter().any(|m| m.0 == 0) {
        push(&mut out, 0, value_at(&curve, 0.0));
    }
    for &(o, g) in &mapped {
        if (0..=end).contains(&o) {
            push(&mut out, o, g);
        }
    }
    if mapped.last().is_some_and(|m| m.0 > end) && !mapped.iter().any(|m| m.0 == end) {
        push(&mut out, end, value_at(&curve, end as f64));
    }
    Some(out)
}

/// Whether `new` (a clip that did not exist in `old_seq`) was cut from `old` (module docs).
/// `survivor` is what became of `old` under its own id (if it is still there).
fn cut_from(old: (&str, &StudioClip), new: (&str, &StudioClip), survivor: Option<&StudioClip>) -> bool {
    let ((ot, o), (nt, n)) = (old, new);
    if ot != nt || o.source_ref != n.source_ref || o.speed != n.speed {
        return false;
    }
    if n.source_in < o.source_in || n.source_out > o.source_out {
        return false;
    }
    survivor.is_none_or(|s| n.source_out <= s.source_in || n.source_in >= s.source_out)
}

/// One clip envelope: clip id and `(offset ticks, value)` points.
type Envelope = (String, Vec<(u64, f64)>);

/// Clip envelopes of `old_seq` rewritten for `new_seq` (module docs). `Err("envelope_overflow")`
/// when an offset cannot be represented.
fn follow_points(old_seq: &StudioSequence, new_seq: &StudioSequence, envelopes: &[Envelope]) -> Result<Vec<Envelope>, String> {
    let before = audio_clips(old_seq);
    let after = audio_clips(new_seq);
    let fresh: Vec<(&str, (&str, &StudioClip))> = after.iter().filter(|(id, _)| !before.contains_key(*id)).map(|(id, v)| (*id, *v)).collect();
    let mut claimed: BTreeSet<&str> = BTreeSet::new();
    let mut out = Vec::with_capacity(envelopes.len());
    for (clip_id, points) in envelopes {
        let Some(&(old_track, old)) = before.get(clip_id.as_str()) else {
            // Not a clip of this sequence: not this edit's business (Folio refuses it anyway).
            out.push((clip_id.clone(), points.clone()));
            continue;
        };
        let survivor = after.get(clip_id.as_str()).map(|&(_, c)| c);
        if let Some(new) = survivor {
            out.push((clip_id.clone(), remap(old, new, points).ok_or("envelope_overflow")?));
        }
        for &(id, (track, clip)) in &fresh {
            if !claimed.contains(id) && cut_from((old_track, old), (track, clip), survivor) {
                claimed.insert(id);
                out.push((id.to_owned(), remap(old, clip, points).ok_or("envelope_overflow")?));
            }
        }
    }
    out.retain(|(_, p)| !p.is_empty());
    Ok(out)
}

/// The clip gain envelopes of `old_seq` rewritten for `new_seq` (module docs).
pub(in crate::view) fn follow_edit(old_seq: &StudioSequence, new_seq: &StudioSequence, gains: &[StudioClipGainEnvelope]) -> Result<Vec<StudioClipGainEnvelope>, String> {
    let flat: Vec<(String, Vec<(u64, f64)>)> = gains.iter().map(|g| (g.clip_id.clone(), g.points.iter().map(|p| (p.offset_ticks, p.gain_db)).collect())).collect();
    Ok(follow_points(old_seq, new_seq, &flat)?
        .into_iter()
        .map(|(clip_id, points)| StudioClipGainEnvelope { clip_id, points: points.into_iter().map(|(offset_ticks, gain_db)| StudioClipGainPoint { offset_ticks, gain_db }).collect() })
        .collect())
}

/// The clip pan envelopes of `old_seq` rewritten for `new_seq` (lane SMB-AU-MULTITRACK-3; module docs).
pub(in crate::view) fn follow_pans(old_seq: &StudioSequence, new_seq: &StudioSequence, pans: &[StudioClipPanEnvelope]) -> Result<Vec<StudioClipPanEnvelope>, String> {
    let flat: Vec<(String, Vec<(u64, f64)>)> = pans.iter().map(|g| (g.clip_id.clone(), g.points.iter().map(|p| (p.offset_ticks, p.pan)).collect())).collect();
    Ok(follow_points(old_seq, new_seq, &flat)?
        .into_iter()
        .map(|(clip_id, points)| StudioClipPanEnvelope { clip_id, points: points.into_iter().map(|(offset_ticks, pan)| StudioClipPanPoint { offset_ticks, pan }).collect() })
        .collect())
}

impl super::super::Viewer {
    /// The routing entry to commit together with `sequence` (the edited timeline sequence), or
    /// `None` to commit the sequence alone (no entry, no envelope of an edited clip, or nothing
    /// changed). Validated by Folio against the edited document; a refusal is the edit's refusal.
    pub(in crate::view) fn envelopes_for(&self, sequence: &StudioSequence, notes: &mut Vec<String>) -> Result<Option<Vec<u8>>, String> {
        use hsk_studio_accord::CancellationToken;
        use hsk_studio_folio as folio;
        let Some(bytes) = self.run.document_entry(ROUTING_ENTRY) else { return Ok(None) };
        let snapshot = self.run.snapshot().ok_or("not_open")?;
        let doc = snapshot.document();
        let mut set = match folio::parse_audio_routing(bytes, doc, folio::Budget::default(), &CancellationToken::default()) {
            Ok(set) => set,
            Err(d) => {
                notes.push(format!("audio routing left as is (invalid before the edit: {:?} {})", d.code, d.target));
                return Ok(None);
            }
        };
        let Some(old) = doc.sequences.iter().find(|s| s.sequence_id == sequence.sequence_id) else { return Ok(None) };
        let Some(routing) = set.routings.iter_mut().find(|r| r.sequence_id == sequence.sequence_id) else { return Ok(None) };
        if routing.clip_gains.is_empty() && routing.clip_pans.is_empty() {
            return Ok(None);
        }
        let gains = follow_edit(old, sequence, &routing.clip_gains)?;
        let pans = follow_pans(old, sequence, &routing.clip_pans)?;
        if gains == routing.clip_gains && pans == routing.clip_pans {
            return Ok(None);
        }
        let (was, now) = (routing.clip_gains.len().saturating_add(routing.clip_pans.len()), gains.len().saturating_add(pans.len()));
        routing.clip_gains = gains;
        routing.clip_pans = pans;
        let mut edited = doc.clone();
        if let Some(s) = edited.sequences.iter_mut().find(|s| s.sequence_id == sequence.sequence_id) {
            s.clone_from(sequence);
        }
        folio::validate_audio_routing(&set, &edited, folio::Budget::default(), &CancellationToken::default()).map_err(|d| format!("routing_invalid {:?} {}", d.code, d.target))?;
        notes.push(format!("clip envelopes followed ({was} -> {now})"));
        serde_json::to_vec(&set).map(Some).map_err(|e| e.to_string())
    }
}

#[cfg(all(test, feature = "audio-batch"))]
mod tests {
    #[test]
    fn routing_entry_is_the_audio_session_entry() {
        assert_eq!(super::ROUTING_ENTRY, super::super::super::audio_session::AUDIO_ROUTING_ENTRY);
    }
}
