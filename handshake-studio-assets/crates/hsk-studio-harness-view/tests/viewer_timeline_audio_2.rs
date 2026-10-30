//! Lane SMB-AU-MULTITRACK-2 (feature `audio-batch`): clip volume envelopes stay attached to their
//! audio through timeline edits, in the same undo step as the edit, and the routing never names a
//! removed clip (save, reopen, validate). Oracles: hand-computed envelope points (offsets move by the
//! source shift; the edge points are linear-in-dB interpolations of the original envelope), the
//! clips' own fields after the edit, the run's history labels and Folio's validation of the reopened
//! package. Track volume and pan lanes are edited by typed commands and a real click (one undo step
//! each) and the mixer's bus stream plays them (ratios 10^(dB/20), pan direction).
#![cfg(feature = "audio-batch")]

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::timeline::{self, AutomationCommand, LaneTarget, PointId, TimelineCommand, lane_y};
use hsk_studio_pulse::{AudioSources, SourceExtents};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 25 fps ticks per frame (STU-VID-013); 1920 samples per frame at 48 kHz.
const F: u64 = 10_160_640_000;
const SPF: usize = 1920;
const RATE: u32 = 48_000;

// ---- fixtures --------------------------------------------------------------------------------------

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-harness-view-timeline-audio-2-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn cid(n: u32) -> String {
    format!("SCLP-019abcde-0000-7000-8000-{n:012x}")
}
fn tid(n: u32) -> String {
    format!("STRK-019abcde-0000-7000-8000-{n:012x}")
}

fn media(name: &str) -> ClipSource {
    ClipSource::Media { artifact_manifest_id: name.into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() }, asset_id: None }
}

fn clip(n: u32, name: &str, si: u64, so: u64, start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: cid(n),
        source_ref: media(name),
        source_in: Ticks::new(si * F),
        source_out: Ticks::new(so * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new((so - si) * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    }
}

fn track(n: u32, kind: TrackKind, name: &str, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: tid(n),
        kind,
        index: 0,
        name: name.into(),
        enabled: true,
        locked: false,
        sync_locked: true,
        targeted: true,
        muted: false,
        soloed: false,
        clips,
        volume: 1.0,
        pan: 0.0,
    }
}

/// V1: clip 1 `picture` at [10, 30). A1: clip 3 `tone` source [5, 45) at frames [0, 40) and clip 4
/// `tone` source [10, 20) at [40, 50) (abutting). B1: an empty submix track.
fn sequence() -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(50 * F),
        tracks: vec![
            track(1, TrackKind::Video, "V1", vec![clip(1, "picture", 0, 20, 10)]),
            track(2, TrackKind::Audio, "A1", vec![clip(3, "tone", 5, 45, 0), clip(4, "tone", 10, 20, 40)]),
            track(3, TrackKind::Submix, "B1", vec![]),
        ],
        markers: Vec::new(),
        work_area: None,
    }
}

/// `tone`: mono, 60 frames of 0.5.
#[derive(Clone)]
struct Sources(Arc<Vec<(String, Vec<Vec<f32>>)>>);
fn sources() -> Sources {
    Sources(Arc::new(vec![("tone".into(), vec![vec![0.5; 60 * SPF]])]))
}
impl AudioSources for Sources {
    fn audio(&self, s: &ClipSource) -> Option<&[Vec<f32>]> {
        let ClipSource::Media { artifact_manifest_id: id, .. } = s else { return None };
        self.0.iter().find(|(n, _)| n == id).map(|(_, v)| v.as_slice())
    }
}
struct Ext;
impl SourceExtents for Ext {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(60 * F))
    }
}

struct Any;
impl folio::Resolver for Any {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
    fn media(&self, _: &str, _: &ContentDigest) -> Resolution {
        Resolution::Available
    }
}

fn sequence_package(dir: &Path) -> PathBuf {
    let doc = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000004","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[serde_json::to_value(sequence()).unwrap()]
    });
    let token = CancellationToken::default();
    let snapshot = match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), Budget::default(), &token, &Any).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read-only fixture"),
    };
    let assets = hsk_studio_package::MemoryAssetSource::new();
    let mut bytes = Vec::new();
    hsk_studio_package::write_package_with(
        &snapshot,
        &assets as &dyn hsk_studio_package::AssetSource,
        &hsk_studio_package::Extras { quarantined: &[], previews: &[] },
        &hsk_studio_package::WriteOptions::default(),
        &hsk_studio_package::Limits::default(),
        &token,
        &mut bytes,
    )
    .unwrap();
    let path = dir.join("multitrack.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

/// Opens the fixture and binds the multitrack view (timeline, mixer, source port).
fn opened(label: &str) -> (Scratch, Viewer) {
    let scratch = Scratch::new(label);
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&sequence_package(&scratch.0)).unwrap_or_else(|e| panic!("{e}"));
    v.open_multitrack(RATE, 960, sources(), Arc::new(Ext)).unwrap_or_else(|e| panic!("{e}: {}", v.state_json()["status"]));
    (scratch, v)
}

fn undo_steps(v: &Viewer) -> usize {
    v.run().history()["undo"].as_array().map_or(0, Vec::len)
}

fn routing(v: &Viewer) -> StudioAudioRouting {
    v.audio_routing().unwrap().and_then(|s| s.routings.into_iter().find(|r| r.sequence_id == "seq-main")).unwrap_or_else(|| StudioAudioRouting::new("seq-main"))
}

fn points_of(v: &Viewer, clip_id: &str) -> Vec<(u64, f64)> {
    routing(v).clip_gains.iter().find(|g| g.clip_id == clip_id).map(|g| g.points.iter().map(|p| (p.offset_ticks, p.gain_db)).collect()).unwrap_or_default()
}

fn envelope(clip_id: String, points: &[(u64, f64)]) -> StudioClipGainEnvelope {
    StudioClipGainEnvelope { clip_id, points: points.iter().map(|&(offset_ticks, gain_db)| StudioClipGainPoint { offset_ticks, gain_db }).collect() }
}

/// Clip 3: -10 dB at 0, 0 dB at frame 10, -20 dB at 30, -6 dB at 40 (its end). Clip 4: -3 dB at 0,
/// -9 dB at frame 10 (its end).
const ENV3: [(u64, f64); 4] = [(0, -10.0), (10 * F, 0.0), (30 * F, -20.0), (40 * F, -6.0)];
const ENV4: [(u64, f64); 2] = [(0, -3.0), (10 * F, -9.0)];

fn with_envelopes(v: &mut Viewer) {
    let mut r = StudioAudioRouting::new("seq-main");
    r.clip_gains = vec![envelope(cid(3), &ENV3), envelope(cid(4), &ENV4)];
    v.set_audio_routing(&StudioAudioRoutingSet { routings: vec![r] }).unwrap();
}

fn clip_now(v: &Viewer, clip_id: &str) -> StudioClip {
    v.timeline_sequence().unwrap().tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.clip_id == clip_id).cloned().unwrap_or_else(|| panic!("no clip {clip_id}"))
}

/// Points equal within 1e-9 dB.
fn assert_points(got: &[(u64, f64)], want: &[(u64, f64)], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: {got:?} vs {want:?}");
    for (g, w) in got.iter().zip(want) {
        assert!(g.0 == w.0 && (g.1 - w.1).abs() < 1e-9, "{what}: {got:?} vs {want:?}");
    }
}

/// One undo step for the edit; undo restores the sequence and the envelopes together, redo both.
fn undo_restores(v: &mut Viewer, steps: usize, before: &StudioSequence) {
    assert_eq!(undo_steps(v), steps + 1, "{:?}", v.run().history());
    let after = v.timeline_sequence().cloned().unwrap();
    let edited = routing(v);
    v.undo().unwrap();
    assert_eq!(v.timeline_sequence(), Some(before));
    assert_points(&points_of(v, &cid(3)), &ENV3, "undo clip 3");
    assert_points(&points_of(v, &cid(4)), &ENV4, "undo clip 4");
    v.redo().unwrap();
    assert_eq!(v.timeline_sequence(), Some(&after));
    assert_eq!(routing(v), edited, "redo restores the edited envelopes");
}

// ---- tests -----------------------------------------------------------------------------------------

/// Trim in to frame 15: the clip's source in moves 15 frames, so every offset moves -15 frames; the
/// points before the new head go and an edge point at 0 holds the envelope's value there (between
/// frame 10 = 0 dB and 30 = -20 dB at offset 15: -5 dB).
#[test]
fn trim_in_shifts_the_envelope_and_cuts_it_at_the_new_head() {
    let (_s, mut v) = opened("trim-in");
    with_envelopes(&mut v);
    let (steps, before) = (undo_steps(&v), v.timeline_sequence().cloned().unwrap());
    v.timeline_command(TimelineCommand::TrimIn { clip_id: cid(3), head: Ticks::new(15 * F) }).unwrap();
    let c = clip_now(&v, &cid(3));
    assert_eq!((c.timeline_start, c.source_in, c.duration), (Ticks::new(15 * F), Ticks::new(20 * F), Ticks::new(25 * F)));
    assert_points(&points_of(&v, &cid(3)), &[(0, -5.0), (15 * F, -20.0), (25 * F, -6.0)], "trim in");
    assert_points(&points_of(&v, &cid(4)), &ENV4, "the other clip keeps its envelope");
    undo_restores(&mut v, steps, &before);
}

/// Trim out to frame 25: offsets stay; the points past the new tail go and an edge point at the new
/// duration holds the envelope's value there (frame 25 between 10 = 0 dB and 30 = -20 dB: -15 dB).
#[test]
fn trim_out_cuts_the_envelope_at_the_new_tail() {
    let (_s, mut v) = opened("trim-out");
    with_envelopes(&mut v);
    let (steps, before) = (undo_steps(&v), v.timeline_sequence().cloned().unwrap());
    v.timeline_command(TimelineCommand::TrimOut { clip_id: cid(3), tail: Ticks::new(25 * F) }).unwrap();
    assert_eq!(clip_now(&v, &cid(3)).duration, Ticks::new(25 * F));
    assert_points(&points_of(&v, &cid(3)), &[(0, -10.0), (10 * F, 0.0), (25 * F, -15.0)], "trim out");
    undo_restores(&mut v, steps, &before);
}

/// Razor at frame 20: the left part keeps its id and its points up to the cut plus an edge point
/// (-10 dB, half way from 0 to -20 dB); the new right part gets the rest moved by -20 frames plus the
/// same edge value at its head. The routing validates against the split sequence.
#[test]
fn razor_splits_the_envelope_between_both_parts() {
    let (_s, mut v) = opened("razor");
    with_envelopes(&mut v);
    let (steps, before) = (undo_steps(&v), v.timeline_sequence().cloned().unwrap());
    v.timeline_command(TimelineCommand::Scrub(Ticks::new(20 * F))).unwrap();
    v.timeline_command(TimelineCommand::Select(Some(cid(3)))).unwrap();
    v.timeline_command(TimelineCommand::RazorAtPlayhead).unwrap();
    let seq = v.timeline_sequence().cloned().unwrap();
    let a1 = seq.tracks.iter().find(|t| t.track_id == tid(2)).unwrap();
    assert_eq!(a1.clips.len(), 3, "{:?}", a1.clips.iter().map(|c| &c.clip_id).collect::<Vec<_>>());
    let right = a1.clips.iter().find(|c| c.timeline_start == Ticks::new(20 * F)).unwrap();
    assert_ne!(right.clip_id, cid(3));
    assert_eq!(right.source_in, Ticks::new(25 * F));
    assert_points(&points_of(&v, &cid(3)), &[(0, -10.0), (10 * F, 0.0), (20 * F, -10.0)], "left part");
    assert_points(&points_of(&v, &right.clip_id), &[(0, -10.0), (10 * F, -20.0), (20 * F, -6.0)], "right part");
    assert_eq!(routing(&v).clip_gains.len(), 3);
    undo_restores(&mut v, steps, &before);
}

/// Slip by +2 frames: the source window moves 2 frames later under a fixed clip, so the audio (and
/// its envelope) moves 2 frames earlier: offsets -2 frames, the head point is cut (value at offset 2
/// between -10 and 0 dB: -8 dB).
#[test]
fn slip_shifts_the_envelope_with_the_audio() {
    let (_s, mut v) = opened("slip");
    with_envelopes(&mut v);
    let (steps, before) = (undo_steps(&v), v.timeline_sequence().cloned().unwrap());
    v.timeline_command(TimelineCommand::Slip { clip_id: cid(3), delta_frames: 2 }).unwrap();
    let c = clip_now(&v, &cid(3));
    assert_eq!((c.timeline_start, c.source_in, c.duration), (Ticks::new(0), Ticks::new(7 * F), Ticks::new(40 * F)), "slip moved the source window");
    assert_points(&points_of(&v, &cid(3)), &[(0, -8.0), (8 * F, 0.0), (28 * F, -20.0), (38 * F, -6.0)], "slip");
    undo_restores(&mut v, steps, &before);
}

/// Roll the edit between clips 3 and 4 by -5 frames: clip 3's tail is cut at 35 (between 30 = -20 and
/// 40 = -6 dB: -13 dB); clip 4's head moves 5 frames earlier in its source, so its offsets move +5
/// frames (both still inside its new duration of 15 frames; before 5 the first value holds).
#[test]
fn roll_moves_both_envelopes_with_their_audio() {
    let (_s, mut v) = opened("roll");
    with_envelopes(&mut v);
    let (steps, before) = (undo_steps(&v), v.timeline_sequence().cloned().unwrap());
    v.timeline_command(TimelineCommand::Roll { clip_id: cid(3), delta_frames: -5 }).unwrap();
    let (c3, c4) = (clip_now(&v, &cid(3)), clip_now(&v, &cid(4)));
    assert_eq!(c3.duration, Ticks::new(35 * F));
    assert_eq!((c4.timeline_start, c4.source_in, c4.duration), (Ticks::new(35 * F), Ticks::new(5 * F), Ticks::new(15 * F)));
    assert_points(&points_of(&v, &cid(3)), &[(0, -10.0), (10 * F, 0.0), (30 * F, -20.0), (35 * F, -13.0)], "outgoing");
    assert_points(&points_of(&v, &cid(4)), &[(5 * F, -3.0), (15 * F, -9.0)], "incoming");
    undo_restores(&mut v, steps, &before);
}

/// Lift removes the clip's envelope in the same step; the saved package reopens with a routing that
/// validates (no dangling clip id); undo brings clip and envelope back together. Ripple delete does
/// the same. A routing entry without envelopes is not rewritten by a trim (byte-identical).
#[test]
fn lift_never_leaves_a_dangling_envelope() {
    let (s, mut v) = opened("lift");
    // No envelope: a trim leaves the routing entry's bytes untouched.
    let mut plain = StudioAudioRouting::new("seq-main");
    plain.master.volume = 0.5;
    v.set_audio_routing(&StudioAudioRoutingSet { routings: vec![plain] }).unwrap();
    let bytes = v.run().document_entry("audio_routing").map(<[u8]>::to_vec);
    v.timeline_command(TimelineCommand::TrimIn { clip_id: cid(3), head: Ticks::new(F) }).unwrap();
    assert_eq!(v.run().document_entry("audio_routing").map(<[u8]>::to_vec), bytes, "no envelope: entry byte-identical");
    v.undo().unwrap();
    with_envelopes(&mut v);
    let (steps, before) = (undo_steps(&v), v.timeline_sequence().cloned().unwrap());
    v.timeline_command(TimelineCommand::Select(Some(cid(3)))).unwrap();
    v.timeline_command(TimelineCommand::Lift).unwrap();
    assert!(points_of(&v, &cid(3)).is_empty(), "the lifted clip's envelope is gone");
    assert_points(&points_of(&v, &cid(4)), &ENV4, "the other envelope stays");
    let saved = s.0.join("lifted.handshake");
    v.save(&saved, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&saved).unwrap_or_else(|e| panic!("{e}"));
    let set = w.audio_routing().expect("the reopened routing validates").expect("routing saved");
    let doc = w.run().snapshot().unwrap().document().clone();
    folio::validate_audio_routing(&set, &doc, Budget::default(), &CancellationToken::default()).unwrap();
    assert_eq!(set.routings[0].clip_gains.iter().map(|g| g.clip_id.clone()).collect::<Vec<_>>(), vec![cid(4)]);
    undo_restores(&mut v, steps, &before);
    // Ripple delete of clip 4 (after undoing the lift): its envelope goes in the same step.
    v.undo().unwrap();
    let steps = undo_steps(&v);
    v.timeline_command(TimelineCommand::Select(Some(cid(4)))).unwrap();
    v.timeline_command(TimelineCommand::RippleDelete).unwrap();
    assert_eq!(undo_steps(&v), steps + 1);
    assert!(points_of(&v, &cid(4)).is_empty());
    assert_points(&points_of(&v, &cid(3)), &ENV3, "clip 3 untouched");
    v.undo().unwrap();
    assert_points(&points_of(&v, &cid(4)), &ENV4, "undo restores the rippled clip's envelope");
}

/// A move keeps the envelope unchanged (offsets are from the clip start); the mixer plays it: with a
/// constant 0.5 source the gain at the clip's new position is the envelope's.
#[test]
fn a_moved_clip_keeps_its_envelope_and_plays_it() {
    let (_s, mut v) = opened("move");
    with_envelopes(&mut v);
    v.timeline_command(TimelineCommand::Select(Some(cid(4)))).unwrap();
    v.timeline_command(TimelineCommand::Lift).unwrap();
    v.timeline_command(TimelineCommand::MoveClip { clip_id: cid(3), start: Ticks::new(10 * F), track_id: None }).unwrap();
    assert_eq!(clip_now(&v, &cid(3)).timeline_start, Ticks::new(10 * F));
    assert_points(&points_of(&v, &cid(3)), &ENV3, "move");
    v.open_multitrack(RATE, 960, sources(), Arc::new(Ext)).unwrap();
    let [l, _] = v.mixer_render(30 * SPF).unwrap();
    // Timeline frame 10 = clip offset 0 = -10 dB; frame 20 = clip offset 10 = 0 dB.
    let (at10, at20) = (f64::from(l[10 * SPF]), f64::from(l[20 * SPF]));
    assert!(at20 > 0.1, "the clip plays at its new position: {at20}");
    assert!((at10 / at20 - 10f64.powf(-0.5)).abs() < 1e-5, "{at10} / {at20}");
}

// ---- track automation lanes (slice 3) ---------------------------------------------------------------

fn track_points(v: &Viewer, track: u32, param: StudioTrackParam) -> Vec<(u64, f64)> {
    routing(v).track_lane(&tid(track), param).map(|a| a.points.iter().map(|p| (p.offset_ticks, p.value)).collect()).unwrap_or_default()
}

fn input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 1200.0))), events, ..Default::default() }
}

fn frame(v: &mut Viewer, ctx: &egui::Context, events: Vec<egui::Event>) {
    let _ = ctx.run(input(events), |ctx| v.ui(ctx));
}

/// Runs frames until the timeline layout is the same in two frames in a row.
fn settle(v: &mut Viewer, ctx: &egui::Context) -> timeline::TimelineLayout {
    let mut last = None;
    for _ in 0..8 {
        frame(v, ctx, vec![]);
        let now = v.timeline().and_then(|t| t.layout()).cloned();
        if let Some(l) = &now
            && now == last
        {
            return l.clone();
        }
        last = now;
    }
    panic!("timeline layout did not settle");
}

/// The A1 volume lane: points are added, moved and deleted in dB (stored as linear gains), one undo
/// step each; the mixer's bus stream follows the lane instead of the static fader (0 dB at frame 0
/// falling to -6.02 dB at frame 20: at frame 10 the gain is -3.01 dB relative to frame 0); undo
/// removes the lane and the static fader plays again.
#[test]
fn track_volume_lane_points_edit_and_play_through_the_mixer() {
    let (_s, mut v) = opened("track-volume");
    let a1 = tid(2);
    let half_db = 20.0 * 0.5f64.log10();
    v.multitrack_command(AutomationCommand::ShowLane { track_id: a1.clone(), lane: Some(LaneTarget::TrackVolume) }).unwrap();
    let steps = undo_steps(&v);
    let add = |at: u64, value: f64| AutomationCommand::AddPoint { track_id: a1.clone(), lane: LaneTarget::TrackVolume, at: Ticks::new(at), value };
    v.multitrack_command(add(0, 0.0)).unwrap();
    v.multitrack_command(add(20 * F, half_db)).unwrap();
    assert_eq!(undo_steps(&v), steps + 2, "{:?}", v.run().history());
    let pts = track_points(&v, 2, StudioTrackParam::Volume);
    assert_eq!(pts.len(), 2);
    assert!(pts[0] == (0, 1.0) && pts[1].0 == 20 * F && (pts[1].1 - 0.5).abs() < 1e-12, "{pts:?}");
    let view = v.timeline_lane(&a1, &LaneTarget::TrackVolume).unwrap();
    assert!(view.error.is_none(), "{:?}", view.error);
    let drawn: Vec<f64> = view.points().map(|p| p.value).collect();
    assert!(drawn.len() == 2 && drawn[0].abs() < 1e-9 && (drawn[1] - half_db).abs() < 1e-9, "the lane draws dB: {drawn:?}");
    assert_eq!(v.state_json()["timeline"]["audio"]["lanes"][0]["editable"], true);
    // The mixer follows the lane (rebinding restarts its stream at sample 0).
    v.open_multitrack(RATE, 960, sources(), Arc::new(Ext)).unwrap();
    let [l, _] = v.mixer_render(30 * SPF).unwrap();
    let (at0, at10, at25) = (f64::from(l[0]), f64::from(l[10 * SPF]), f64::from(l[25 * SPF]));
    assert!(at0 > 0.1, "{at0}");
    assert!((at10 / at0 - 10f64.powf(half_db / 2.0 / 20.0)).abs() < 1e-5, "ramp: {at10} / {at0}");
    assert!((at25 / at0 - 0.5).abs() < 1e-5, "held after the last point: {at25} / {at0}");
    // Move the second point to frame 10 at -12 dB, then delete both: one step each, the lane goes.
    v.multitrack_command(AutomationCommand::MovePoint { track_id: a1.clone(), lane: LaneTarget::TrackVolume, point: PointId { clip_id: None, offset_ticks: 20 * F }, at: Ticks::new(10 * F), value: -12.0 }).unwrap();
    let pts = track_points(&v, 2, StudioTrackParam::Volume);
    assert!(pts[1].0 == 10 * F && (pts[1].1 - 10f64.powf(-12.0 / 20.0)).abs() < 1e-12, "{pts:?}");
    for at in [0, 10 * F] {
        v.multitrack_command(AutomationCommand::DeletePoint { track_id: a1.clone(), lane: LaneTarget::TrackVolume, point: PointId { clip_id: None, offset_ticks: at } }).unwrap();
    }
    assert!(routing(&v).track_automation.is_empty(), "the last point takes the lane with it");
    assert_eq!(undo_steps(&v), steps + 5);
    // Undo back to the two-point ramp, then to nothing: the static fader plays (flat).
    for _ in 0..3 {
        v.undo().unwrap();
    }
    assert_eq!(track_points(&v, 2, StudioTrackParam::Volume).len(), 2);
    v.undo().unwrap();
    v.undo().unwrap();
    assert!(routing(&v).track_automation.is_empty());
    v.open_multitrack(RATE, 960, sources(), Arc::new(Ext)).unwrap();
    let [l, _] = v.mixer_render(30 * SPF).unwrap();
    assert!((f64::from(l[25 * SPF]) / f64::from(l[0]) - 1.0).abs() < 1e-6, "static fader after undo");
}

/// The A1 pan lane: hard left at frame 0 to hard right at frame 20 (equal-power mono pan): the right
/// side is silent at the start and the left side at the end, equal at frame 10; a real click in the
/// B1 volume lane adds a point at the clicked frame and dB.
#[test]
fn track_pan_lane_pans_the_mix_and_a_click_adds_a_volume_point() {
    let (_s, mut v) = opened("track-pan");
    let (a1, b1) = (tid(2), tid(3));
    let pan = |at: u64, value: f64| AutomationCommand::AddPoint { track_id: a1.clone(), lane: LaneTarget::TrackPan, at: Ticks::new(at), value };
    v.multitrack_command(pan(0, -1.0)).unwrap();
    v.multitrack_command(pan(20 * F, 1.0)).unwrap();
    assert_eq!(track_points(&v, 2, StudioTrackParam::Pan), vec![(0, -1.0), (20 * F, 1.0)]);
    v.open_multitrack(RATE, 960, sources(), Arc::new(Ext)).unwrap();
    let [l, r] = v.mixer_render(30 * SPF).unwrap();
    assert!(f64::from(l[0]) > 0.1 && f64::from(r[0]).abs() < 1e-6, "hard left: {} {}", l[0], r[0]);
    assert!(f64::from(r[25 * SPF]) > 0.1 && f64::from(l[25 * SPF]).abs() < 1e-6, "hard right: {} {}", l[25 * SPF], r[25 * SPF]);
    assert!((f64::from(l[10 * SPF]) - f64::from(r[10 * SPF])).abs() < 1e-5, "centre at frame 10");
    // A real click in the submix volume lane at frame 20, at the height of 0 dB.
    let ctx = egui::Context::default();
    v.multitrack_command(AutomationCommand::ShowLane { track_id: b1.clone(), lane: Some(LaneTarget::TrackVolume) }).unwrap();
    let l = settle(&mut v, &ctx);
    let lane = l.lane(&b1).expect("B1 lane row drawn").rect;
    let ppf = v.timeline().unwrap().view.px_per_frame as f32;
    let at = egui::pos2(l.lanes_left + 20.0 * ppf, lane_y(lane, CLIP_GAIN_DB, 0.0));
    let steps = undo_steps(&v);
    let press = |pressed: bool| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    frame(&mut v, &ctx, vec![egui::Event::PointerMoved(at)]);
    frame(&mut v, &ctx, vec![press(true)]);
    frame(&mut v, &ctx, vec![press(false)]);
    frame(&mut v, &ctx, vec![]);
    assert_eq!(undo_steps(&v), steps + 1, "{}", v.state_json()["status"]);
    let pts = track_points(&v, 3, StudioTrackParam::Volume);
    let px_db = (CLIP_GAIN_DB.1 - CLIP_GAIN_DB.0) / f64::from(lane.height() - 2.0 * timeline::LANE_PAD);
    assert!(pts.len() == 1 && pts[0].0 == 20 * F && (20.0 * pts[0].1.log10()).abs() <= px_db, "{pts:?}");
    let _ = settle(&mut v, &ctx);
    assert!(v.drawn().iter().any(|d| *d == timeline::widget::lane_point(&b1, None, 20 * F)), "{:?}", v.drawn());
}

/// Hostile track lane commands are refused by name and store nothing.
#[test]
fn hostile_track_lane_commands_are_refused_without_a_step() {
    let (_s, mut v) = opened("track-hostile");
    let (v1, a1) = (tid(1), tid(2));
    let steps = undo_steps(&v);
    let add = |track: &str, lane: LaneTarget, at: u64, value: f64| AutomationCommand::AddPoint { track_id: track.into(), lane, at: Ticks::new(at), value };
    let cases: Vec<(AutomationCommand, &str)> = vec![
        (add(&a1, LaneTarget::TrackVolume, F, f64::NAN), "value_not_finite"),
        (add(&a1, LaneTarget::TrackVolume, F, 12.5), "value_out_of_range"),
        (add(&a1, LaneTarget::TrackVolume, F, -97.0), "value_out_of_range"),
        (add(&a1, LaneTarget::TrackPan, F, 1.5), "value_out_of_range"),
        (add(&a1, LaneTarget::TrackPan, F, f64::INFINITY), "value_not_finite"),
        (add(&a1, LaneTarget::TrackPan, 1, 0.0), "not_sample_aligned"),
        (add(&a1, LaneTarget::TrackPan, 51 * F, 0.0), "outside_sequence"),
        (add(&v1, LaneTarget::TrackVolume, F, 0.0), "lane_unavailable"),
        (add("STRK-nope", LaneTarget::TrackPan, F, 0.0), "unknown_track"),
        (AutomationCommand::DeletePoint { track_id: a1.clone(), lane: LaneTarget::TrackPan, point: PointId { clip_id: None, offset_ticks: 0 } }, "unknown_point"),
        (
            AutomationCommand::MovePoint { track_id: a1.clone(), lane: LaneTarget::TrackVolume, point: PointId { clip_id: Some(cid(3)), offset_ticks: 0 }, at: Ticks::new(F), value: 0.0 },
            "unknown_point",
        ),
    ];
    for (c, code) in cases {
        let e = v.multitrack_command(c.clone()).unwrap_err();
        assert!(e.starts_with(code), "{c:?}: {e} (want {code})");
    }
    assert_eq!(undo_steps(&v), steps, "{:?}", v.run().history());
    assert!(routing(&v).track_automation.is_empty());
    // +12 dB is the top of the range: stored as the largest linear level, accepted by Folio.
    v.multitrack_command(add(&a1, LaneTarget::TrackVolume, F, 12.0)).unwrap();
    assert!(track_points(&v, 2, StudioTrackParam::Volume)[0].1 <= MAX_AUDIO_LEVEL);
    // A point moved onto another of the same lane is refused.
    v.multitrack_command(add(&a1, LaneTarget::TrackVolume, 2 * F, 0.0)).unwrap();
    let e = v
        .multitrack_command(AutomationCommand::MovePoint { track_id: a1.clone(), lane: LaneTarget::TrackVolume, point: PointId { clip_id: None, offset_ticks: F }, at: Ticks::new(2 * F), value: 0.0 })
        .unwrap_err();
    assert_eq!(e, "point_exists");
}
