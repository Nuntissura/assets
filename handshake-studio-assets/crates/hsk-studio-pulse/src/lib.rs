//! Canonical integer-tick sequence time at 254016000000 ticks/second per STU-VID-012 and normalized
//! broadcast ticks/frame per STU-VID-013. Preserve declared field types with checked
//! arithmetic/range; rational time is for interchange adapters, never a competing stored authority.
//! Own edit/trim/ripple/multicam references and scheduling; media decoder behind typed port.
#![forbid(unsafe_code)]

mod annotate;
mod assemble;
mod audio;
mod bus_mix;
mod comp_audio;
mod display;
mod error;
mod frame_cache;
mod multicam;
mod playback;
mod range;
mod rate;
mod rational;
mod reference_time;
mod snap;
mod time;
mod timeline_edit;
mod transition;

pub use annotate::*;
pub use assemble::*;
pub use audio::*;
pub use bus_mix::{EffectFactory, TimelineBusStream};
pub use comp_audio::*;
pub use display::*;
pub use error::{PulseError, Result};
pub use frame_cache::*;
pub use multicam::*;
pub use playback::*;
pub use range::*;
pub use rate::*;
pub use rational::*;
pub use reference_time::*;
pub use snap::*;
pub use time::*;
pub use timeline_edit::*;
pub use transition::*;

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{Observation, Observe, Outcome, Receipt, SinkPort};

/// Maps a pulse result to exactly one Observe terminal outcome (no project text, no ids).
/// `correlation_id` and `revision` must equal the values `observe` was created with.
pub fn report<T>(
    result: &Result<T>,
    correlation_id: u64,
    revision: u64,
    cancel: &CancellationToken,
    observe: &mut Observe,
    sink: &mut (impl SinkPort + ?Sized),
) -> std::result::Result<Receipt, hsk_studio_observe::Error> {
    let outcome = match result {
        Ok(_) => Outcome::Success,
        Err(error) => match error.failure_code() {
            None => Outcome::Canceled,
            Some(code) => Outcome::Failure(code),
        },
    };
    observe.emit(
        Observation {
            correlation_id,
            revision,
            outcome,
            progress: None,
            private_project_text: None,
        },
        cancel,
        sink,
    )
}

/// Same descriptor for models, human manual and Argus/diagnostic adapters.
pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-PULSE","version":1,"operation":"plan_timeline_edit(Snapshot, sequence_id, expected_revision, TimelineEdit{TrimOut,TrimIn,Razor,Roll,Slip,Slide,LiftClip,ExtractClip,LiftRange,ExtractRange,CloseGap}, EditContext) over Folio timeline members returns an EditProposal (changes, read/write footprint, Folio-validated prospective Snapshot at revision+1, inverse); plan_assembly_edit(Snapshot, sequence_id, expected_revision, AssemblyEdit{mode Insert|Overwrite, source, speed, AssemblyPoints, fit, patches, link_group, fresh_clip_ids}, EditContext) places a source window by three- or four-point editing (resolve_points derives the fourth point); plan_timeline_revert(Snapshot, sequence_id, expected_revision, TimelineInverse, EditContext) is undo and redo; plan_record_edit(Snapshot, sequence_id, expected_revision, RecordEdit{AddMarker, ReplaceMarker, RemoveMarker, SetTransition, SetWorkArea}, EditContext) edits Folio's StudioMarker, TimelineTransition and WorkArea records as one undoable proposal, with marker_span, marker_spans, next_marker, prev_marker, play_range, transition_placement, transition_handles, transition_frame and clip_source_at; PlaybackScheduler (AudioClock port, HeadlessClock, DropPolicy DropLate|PresentEveryFrame, play/pause/scrub/step/poll) and frame_layers for the program monitor; plus integer-tick time algebra over accord::Ticks: frame rate table and legacy normalization, frame/tick and sample/tick conversion, drop-frame and non-drop timecode label functions, rational interchange with explicit rounding, signed TickDelta, TickRange, checked trim, ripple, lift, extract, roll and slide over an ordered lane of ranges with scheduling queries (range_at, edit points, gaps), frame snapping, typed timecode parsing and the STU-VID-012a time-display enumeration; plus Media Foundation REFERENCE_TIME (100 ns) to ticks and back with an explicit accord Rounding and a residual receipt (ticks_from_reference_time, reference_time_from_ticks, reference_time_from_sequence_ticks)","operator":{"intent":"Move, trim and ripple sequence time without float drift or silent rounding","result":"New range lane or tick value, or a stable error code; inputs are never modified","recovery":"overlap: the move collides with a neighbour, shorten it or ripple; underflow: the move passes time zero; overflow: value exceeds u64 ticks; not_frame_aligned: use whole frames on a frame grid; inexact: choose Floor/Ceil/NearestHalfUp or a table rate; invalid_timecode: drop-frame skips that label; canceled: nothing was produced"},"model":{"time":"u64 ticks at 254016000000 per second (accord::Ticks); ticks per frame from the STU-VID-013 table; legacy 10594594594 and 8475675675 normalize to 10594584000 and 8475667200 with a FrameRateNormalization receipt; drop-frame (2 frame numbers per minute at 29.97, 4 at 59.94, none every tenth minute) only labels frame numbers and never changes ticks or frame counts","inputs":"Ticks, FrameRate, TickRange [start,end), TickDelta (signed, checked), Grid::Free or Grid::Frames(rate), ordered non-overlapping lanes of TickRange, Speed (reduced rational, sign = reverse), Rounding for rational seconds","consumer":"pulse-timeline-consumer [--descriptor] [--op OP] [--clip N] [--frames N] [--ripple] [--new-id N] [--from F --to T] [--track N] [--cancel] plans one timeline edit; pulse-consumer [--descriptor] [--rate TICKS_PER_FRAME] [--drop] [--frame N] [--a-frames N] [--b-frames N] [--trim-to-frames N] [--ripple] [--cancel]","timeline":"link group = shared link_group, one clip per track, edited together by trim, slip, lift, extract, razor (caller supplies right-half ids and a new link group, else insufficient_ids) and roll/slide (only when every touched link group is wholly among the touched clips, else linked_edit_unsupported); range lift/extract refuse any overlapped linked clip; LiftClip/ExtractClip/LiftRange/ExtractRange/CloseGap are timeline commands, range ops act on targeted tracks and a straddled clip takes the next caller-supplied fresh id; ripple, extract and close-gap shift later content on edited tracks and on other sync-locked tracks, refused (sync_lock_blocked) when such a track is locked or would collide; locked edited track = track_locked; video edits move whole frames; a later source out-point needs SourceExtents (source_extent_unknown / past_source_end), an earlier source in-point below zero is past_source_start; zero-effect edits are no_change; Folio validates the result and pulse never commits","reference_time":"Media Foundation REFERENCE_TIME: one 100 ns unit is 127008/5 ticks (25401.6); 100 ns to ticks is whole iff n % 5 == 0, else the caller's accord Rounding applies (interchange rule NearestHalfUp, REFERENCE_TIME_ROUNDING); ticks to 100 ns is whole iff ticks % 127008 == 0 and a tie (63504) rounds up under NearestHalfUp; every call returns a receipt {input, output, rounding, exact, residual} where residual = exact - result as a numerator over 5 (forward) or 127008 (backward); 100 ns to ticks to 100 ns is the identity under NearestHalfUp; tick values are i64, beyond +-363102010773131 units (about 420 days) is overflow and a negative result as sequence Ticks is underflow","errors":"PulseError::code() snake_case; canceled maps to the Observe Canceled outcome, overflow/unsupported/inexact to Failure Unsupported, everything else to Failure Validation"},"limits":{"ranges_per_lane":262144,"case_seconds":30},"cancel":"CancellationToken is checked at entry and every 256 ranges; a canceled call returns canceled and produces nothing","assembly":"STU-VID-024/025: the fourth point is derived from three (source in/out, sequence in/out; backtiming when sequence in is missing); four points that disagree need FourPointFit ChangeSpeed|TrimHead|TrimTail|IgnoreSequenceOut else fit_required; source patching (SourcePatch track_id + new clip id per stream, one per track else overlap) decides where clips land; insert splits a clip straddling the in point (next fresh id) and shifts content at or after it on the patched tracks and on every other sync-locked track with later content (locked one: sync_lock_blocked), unlocked tracks stay; overwrite lifts its span on the patched tracks (a straddled clip splits, next fresh id) and moves nothing; a linked clip under the span or straddling the in point is linked_edit_unsupported; the source out must be within SourceExtents; patches over 1024 or fresh ids over 262144 are limit_exceeded before any work, as is a result that could exceed 262144 clips; Premiere parity UNVERIFIED (D5)","markers":"STU-VID-034 over Folio StudioMarker (no pulse copy): sequence markers in sequence ticks on whole frames never move with clips; clip markers in the clip SOURCE ticks travel with the clip, a trim or slip only hides them, and a clip cut in two (razor, insert, overwrite, range lift/extract) hands each marker to the piece whose source window holds its start; markers of a removed clip leave with it and are listed in EditProposal::removed_markers; every RecordEdit is one revision step undone by plan_timeline_revert (test markers_keep_their_scope_through_edits_with_spans_and_undo)","transitions":"STU-VID-032/032a over Folio TimelineTransition stored as the OUTGOING clip's tail (kind keys cross_dissolve / audio_crossfade are Studio keys, canonical filter_kind UNVERIFIED); placement: centred = floor(frames / 2) whole frames before the cut on video, inside both clips (outside_range), no overlap on a track; handles in source ticks (insufficient_handle / source_extent_unknown unless the stored repeat_end_frame holds the last available source tick); every clip edit removes transitions that no longer play and lists them in EditProposal::dropped_transitions (undo restores them); transition_frame(tick) gives each clip's source tick and progress k/n (Premiere sampling UNVERIFIED); a cross dissolve frame is the incoming frame at opacity p over the opaque outgoing frame through composite render_scene = (1 - p) out + p in (test cross_dissolve_frame_matches_hand_computed_blend)","argus":{"inspect":"all values are plain data","action":"the same functions with the same cancellation token","capture":"caller captures inputs and results under a fresh grant"},"diagnostics":"report() emits one Observe terminal outcome; no ids, paths or project text are serialized by pulse","playback":"PR-19: audio is the master clock (AudioClock port: sample rate + played samples; HeadlessClock for tests); master ticks = anchor + ticks_from_samples (exact for rates dividing 254016000000); DropLate presents the due frame floor(master / ticks_per_frame) and counts skipped frames, so at every presentation 0 <= master - frame_start < one frame period (stated tolerance; test playback_stays_within_one_frame_of_the_audio_clock); PresentEveryFrame never skips and reports the lag (tests playback_stays_within_one_frame_of_the_audio_clock: 1x forward with a presenter at half real time presents 50 frames before the audio clock reaches the end, 0 dropped, max_lag_ticks 49 frames; reverse_and_slow_play_end_at_the_range_edges_within_bounds: 2x reverse presents 99, 98, 97, 96 one per poll, 0 dropped, lag above one frame); play range = work area or whole sequence; in/out points (set_in_out, out inclusive) and play_in_to_out; set_looping wraps the position exactly in integer ticks (start + offset rem_euclid span; test loop_wraps_exactly_and_shuttles_in_both_directions); PlaySpeed (signed ratio, power-of-two denominator <= 8, magnitude <= 8) with shuttle_right/left (J/L: 1x then doubling to 8x, opposite direction restarts at 1x; Premiere steps UNVERIFIED), shuttle_stop (K) and shuttle_slow (1/2); reverse presentations satisfy 0 < frame_end - position <= one frame; Nx shuttle under DropLate drops N-1 frames per real frame; pause holds, scrub and step are frame-exact and clamped; frame_layers(tick) lists enabled video tracks bottom-to-top with transition weights for the renderer","undo":"each plan_* call is one revision step with one inverse (sequence before = restore, after = expect); plan_timeline_revert restores only while the sequence equals expect (else revert_mismatch) and its inverse is the redo; session submit_timeline carries the same step through the host","audio":"PR-23 timeline side / PR-19 audio path: build_timeline_mix(sequence, sample_rate_hz, max_block_frames, AudioSources, SourceExtents, cancel) -> score Session: enabled, unmuted audio tracks (solo honoured) with the Folio track volume (linear -> score dB fader) and pan; each enabled clip cut by TrackSpec::from_source (unit speed) or from_source_at_speed; an audio_crossfade tail becomes score's fade pair over the transition span (outgoing runs into its tail handle with a fade out, incoming starts early from its head handle with a fade in; parameter curve linear|equal_power, default equal_power, Studio key); positions must sit on the sample grid (not_frame_aligned); more than score MAX_TRACKS clips is limit_exceeded before any sample is copied; the score Session implements AudioClock (position = next sample) and is the playback master (test timeline_audio_mixes_through_score_and_clocks_video_within_one_frame: samples within 1e-6 of the hand value, every presentation within [0, 1 frame) of the score position); TimelineMixStream mixes any number of clips (StreamLimits max_clips 65536, group_clips <= MAX_TRACKS, max_resident_frames): clips sorted by start, packed into groups of <= 64, a group session is built when the position reaches it and dropped after it, blocks sum the active groups (equal to the hand mix and to the one-session mix within 1e-6; test streamed_mix_of_more_than_64_clips_matches_the_hand_mix), seek rebuilds and discards up to the position, the stream is an AudioClock","snapping":"STU-VID-035 / PR-12: snap_targets(sequence, playhead, exclude_clips, cancel) lists the sequence start, clip edit points (timeline start and end), marker starts and span-marker ends (clip markers mapped from source ticks), work-area bounds and the playhead, sorted by tick then kind, excluding the dragged clips; nearest_snap(targets, at, distance) picks the nearest within an inclusive tick distance (ties: nearer, earlier, kind order); adjacent_edit gives the next or previous edit point; clip or marker counts over the Folio limits are limit_exceeded before collection (test snap targets in tests/snap.rs); the toggle and the screen snap distance belong to the timeline surface; Premiere snap priorities UNVERIFIED (D5)","frames":"FrameDecoder port (declared_bytes, decode_run, DecodeRefusal code), FrameCache (LRU by source and span, bytes/entries/per-frame bounds admitted before decode), FramePipeline (refusal memory, prefetch with a decoder-call budget), PlaybackScheduler::upcoming_frames and prefetch_layers (test frame_pipeline_caches_prefetches_and_refuses_before_allocation)"}"#;
