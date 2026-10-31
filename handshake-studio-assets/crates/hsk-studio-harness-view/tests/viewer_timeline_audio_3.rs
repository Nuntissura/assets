//! Lane SMB-AU-MULTITRACK-3 (feature `audio-batch`): the Clip Pan lane (typed and undoable point
//! add / move / delete, envelopes following timeline edits in the edit's undo step, the mixer playing
//! the pan), the master row (master volume lane and master effect parameter lanes, drawn under the
//! tracks and driven by real clicks) and the automated-fader state of the mixer (a visible label; a
//! fader move writes the lane at the playhead as one undo step). Oracles: hand-computed points
//! (offsets move by the source shift, edge points interpolate linearly), score's pan laws on a
//! constant source (mono equal-power `(cos a, sin a)`, `a = (pan + 1) pi/4`), `10^(dB/20)` ratios,
//! the run's history and Folio's validation.
#![cfg(feature = "audio-batch")]

mod support_context;

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::mixer::{MixerEdit, effect_templates};
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::timeline::{self, AutomationCommand, LANE_BUTTON_PX, LaneTarget, MASTER_ROW, PointId, TimelineCommand, lane_y};
use hsk_studio_pulse::{AudioSources, CROSSFADE_PANNED_UNPANNED_MONO, SourceExtents};
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
        let dir = std::env::temp_dir().join(format!("hsk-harness-view-timeline-audio-3-{}-{label}", std::process::id()));
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
/// `tone` source [10, 20) at [40, 50). B1: an empty submix track.
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
    sequence_package_of(dir, &sequence())
}

fn sequence_package_of(dir: &Path, sequence: &StudioSequence) -> PathBuf {
    let doc = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000004","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[serde_json::to_value(sequence).unwrap()]
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
    let mut v = crate::support_context::viewer(Prefer::Cpu);
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

fn pan_points(v: &Viewer, clip_id: &str) -> Vec<(u64, f64)> {
    routing(v).clip_pans.iter().find(|g| g.clip_id == clip_id).map(|g| g.points.iter().map(|p| (p.offset_ticks, p.pan)).collect()).unwrap_or_default()
}

fn assert_points(got: &[(u64, f64)], want: &[(u64, f64)], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: {got:?} vs {want:?}");
    for (g, w) in got.iter().zip(want) {
        assert!(g.0 == w.0 && (g.1 - w.1).abs() < 1e-9, "{what}: {got:?} vs {want:?}");
    }
}

fn input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 1600.0))), events, ..Default::default() }
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

/// A real click at `at` (move, press, release, one idle frame).
fn click(v: &mut Viewer, ctx: &egui::Context, at: egui::Pos2) {
    let press = |pressed: bool| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    frame(v, ctx, vec![egui::Event::PointerMoved(at)]);
    frame(v, ctx, vec![press(true)]);
    frame(v, ctx, vec![press(false)]);
    frame(v, ctx, vec![]);
}

fn equal_power(pan: f64) -> (f64, f64) {
    let a = (pan + 1.0) * std::f64::consts::FRAC_PI_4;
    (a.cos(), a.sin())
}

// ---- slice 1: clip pan -----------------------------------------------------------------------------

/// Clip 3 (mono 0.5) panned hard left at its head to hard right at frame 20: points are typed edits
/// (one undo step each, drawn by the lane), the mixer plays them by the equal-power law sample for
/// sample at the probed frames; move and delete are one step each and the last point takes the
/// envelope with it.
#[test]
fn clip_pan_lane_edits_play_through_the_mixer() {
    let (_s, mut v) = opened("clip-pan");
    let a1 = tid(2);
    assert!(v.timeline_lane_choices(&a1).contains(&LaneTarget::ClipPan));
    v.multitrack_command(AutomationCommand::ShowLane { track_id: a1.clone(), lane: Some(LaneTarget::ClipPan) }).unwrap();
    let steps = undo_steps(&v);
    let add = |at: u64, value: f64| AutomationCommand::AddPoint { track_id: a1.clone(), lane: LaneTarget::ClipPan, at: Ticks::new(at), value };
    v.multitrack_command(add(0, -1.0)).unwrap();
    v.multitrack_command(add(20 * F, 1.0)).unwrap();
    assert_eq!(undo_steps(&v), steps + 2, "{:?}", v.run().history());
    assert_points(&pan_points(&v, &cid(3)), &[(0, -1.0), (20 * F, 1.0)], "added");
    let view = v.timeline_lane(&a1, &LaneTarget::ClipPan).unwrap();
    assert!(view.error.is_none() && view.range == (-1.0, 1.0), "{view:?}");
    assert_eq!(view.points().map(|p| (p.at.value(), p.value)).collect::<Vec<_>>(), vec![(0, -1.0), (20 * F, 1.0)]);
    let lanes = &v.state_json()["timeline"]["audio"]["lanes"];
    assert_eq!(lanes[0]["lane"], "clip_pan");
    assert_eq!(lanes[0]["editable"], true);
    // The mixer plays the pan (rebinding restarts its stream at sample 0).
    v.open_multitrack(RATE, 960, sources(), Arc::new(Ext)).unwrap();
    let [l, r] = v.mixer_render(30 * SPF).unwrap();
    for k in [0usize, 1, 4800, 10 * SPF, 15 * SPF, 20 * SPF, 25 * SPF] {
        let p = if k < 20 * SPF { -1.0 + 2.0 * k as f64 / (20 * SPF) as f64 } else { 1.0 };
        let (gl, gr) = equal_power(p);
        assert!((f64::from(l[k]) - 0.5 * gl).abs() < 2e-6 && (f64::from(r[k]) - 0.5 * gr).abs() < 2e-6, "sample {k}: ({}, {}) vs pan {p}", l[k], r[k]);
    }
    // Move the second point to frame 10 at 0 (centre), delete both: the envelope goes.
    v.multitrack_command(AutomationCommand::MovePoint { track_id: a1.clone(), lane: LaneTarget::ClipPan, point: PointId { clip_id: Some(cid(3)), offset_ticks: 20 * F }, at: Ticks::new(10 * F), value: 0.0 })
        .unwrap();
    assert_points(&pan_points(&v, &cid(3)), &[(0, -1.0), (10 * F, 0.0)], "moved");
    for at in [0, 10 * F] {
        v.multitrack_command(AutomationCommand::DeletePoint { track_id: a1.clone(), lane: LaneTarget::ClipPan, point: PointId { clip_id: Some(cid(3)), offset_ticks: at } }).unwrap();
    }
    assert!(routing(&v).clip_pans.is_empty(), "the last point takes the envelope with it");
    assert_eq!(undo_steps(&v), steps + 5);
    v.undo().unwrap();
    assert_points(&pan_points(&v, &cid(3)), &[(10 * F, 0.0)], "undo restores the deleted point");
}

/// Trim in to frame 5 moves clip 3's pan envelope with its audio in the edit's own undo step: the
/// head point is cut and an edge point holds the interpolated pan (-1 + 2 * 5/20 = -0.5); razor gives
/// the right part its part; undo restores clip and envelope together.
#[test]
fn clip_pan_envelopes_follow_timeline_edits() {
    let (_s, mut v) = opened("clip-pan-follow");
    let mut r = StudioAudioRouting::new("seq-main");
    r.clip_pans = vec![StudioClipPanEnvelope { clip_id: cid(3), points: vec![StudioClipPanPoint { offset_ticks: 0, pan: -1.0 }, StudioClipPanPoint { offset_ticks: 20 * F, pan: 1.0 }] }];
    v.set_audio_routing(&StudioAudioRoutingSet { routings: vec![r] }).unwrap();
    let (steps, before) = (undo_steps(&v), v.timeline_sequence().cloned().unwrap());
    v.timeline_command(TimelineCommand::TrimIn { clip_id: cid(3), head: Ticks::new(5 * F) }).unwrap();
    assert_eq!(undo_steps(&v), steps + 1, "{:?}", v.run().history());
    assert_points(&pan_points(&v, &cid(3)), &[(0, -0.5), (15 * F, 1.0)], "trim in");
    v.undo().unwrap();
    assert_eq!(v.timeline_sequence(), Some(&before));
    assert_points(&pan_points(&v, &cid(3)), &[(0, -1.0), (20 * F, 1.0)], "undo");
    // Razor at frame 10: left part keeps [0, 10] with an edge at 0 pan; right part starts at 0 pan.
    v.timeline_command(TimelineCommand::Scrub(Ticks::new(10 * F))).unwrap();
    v.timeline_command(TimelineCommand::Select(Some(cid(3)))).unwrap();
    v.timeline_command(TimelineCommand::RazorAtPlayhead).unwrap();
    let seq = v.timeline_sequence().cloned().unwrap();
    let right = seq.tracks[1].clips.iter().find(|c| c.timeline_start == Ticks::new(10 * F)).unwrap().clip_id.clone();
    assert_points(&pan_points(&v, &cid(3)), &[(0, -1.0), (10 * F, 0.0)], "left part");
    assert_points(&pan_points(&v, &right), &[(0, 0.0), (10 * F, 1.0)], "right part");
    let doc = v.run().snapshot().unwrap().document().clone();
    folio::validate_audio_routing(&v.audio_routing().unwrap().unwrap(), &doc, Budget::default(), &CancellationToken::default()).unwrap();
}

/// An `audio_crossfade` from clip 3 into clip 4 (both mono): panning clip 3 alone puts the two in
/// different strips, so the mixer's rebuild is refused `audio_unsupported` and its error names the
/// case and both clips (engine gap: the pair is not rendered as two stereo sides); panning clip 4
/// too puts both in the stereo strip and the mixer plays again.
#[test]
fn panned_unpanned_crossfade_is_refused_by_name_in_the_mixer() {
    let scratch = Scratch::new("xfade");
    let mut seq = sequence();
    seq.tracks[1].clips[0].transitions.tail = Some(TimelineTransition {
        schema_id: TransitionSchema::V1,
        transition_id: "x1".into(),
        transition_kind: "audio_crossfade".into(),
        duration: Ticks::new(2 * F),
        alignment: TransitionAlignment::CenteredOnCut,
        center_offset: 0,
        parameters: vec![],
        repeat_end_frame: false,
    });
    let mut v = crate::support_context::viewer(Prefer::Cpu);
    v.open(&sequence_package_of(&scratch.0, &seq)).unwrap_or_else(|e| panic!("{e}"));
    v.open_multitrack(RATE, 960, sources(), Arc::new(Ext)).unwrap_or_else(|e| panic!("{e}: {}", v.state_json()["status"]));
    assert!(v.mixer_json()["error"].is_null(), "unpanned crossfade plays: {}", v.mixer_json()["error"]);
    let a1 = tid(2);
    let pan = |at: u64| AutomationCommand::AddPoint { track_id: a1.clone(), lane: LaneTarget::ClipPan, at: Ticks::new(at), value: 0.5 };
    v.multitrack_command(pan(0)).unwrap();
    let error = v.mixer_json()["error"].as_str().unwrap_or_default().to_owned();
    for part in ["audio_unsupported", CROSSFADE_PANNED_UNPANNED_MONO, &a1, &cid(3), &cid(4)] {
        assert!(error.contains(part), "{part} missing from {error:?}");
    }
    assert!(v.mixer_render(960).is_err(), "no stream while refused");
    v.multitrack_command(pan(45 * F)).unwrap();
    assert!(v.mixer_json()["error"].is_null(), "both panned: {}", v.mixer_json()["error"]);
    assert!(v.mixer_render(960).is_ok());
}

/// Hostile clip pan and master commands are refused by name and store nothing.
#[test]
fn hostile_clip_pan_and_master_commands_are_refused_without_a_step() {
    let (_s, mut v) = opened("hostile");
    let (a1, b1) = (tid(2), tid(3));
    let steps = undo_steps(&v);
    let add = |track: &str, lane: LaneTarget, at: u64, value: f64| AutomationCommand::AddPoint { track_id: track.into(), lane, at: Ticks::new(at), value };
    let cases: Vec<(AutomationCommand, &str)> = vec![
        (add(&a1, LaneTarget::ClipPan, F, 1.5), "value_out_of_range"),
        (add(&a1, LaneTarget::ClipPan, F, f64::NAN), "value_not_finite"),
        (add(&a1, LaneTarget::ClipPan, 55 * F, 0.0), "no_clip_at"),
        (add(&b1, LaneTarget::ClipPan, F, 0.0), "lane_unavailable"),
        (AutomationCommand::DeletePoint { track_id: a1.clone(), lane: LaneTarget::ClipPan, point: PointId { clip_id: Some(cid(3)), offset_ticks: 0 } }, "unknown_point"),
        (AutomationCommand::DeletePoint { track_id: a1.clone(), lane: LaneTarget::ClipPan, point: PointId { clip_id: None, offset_ticks: 0 } }, "unknown_point"),
        (add(MASTER_ROW, LaneTarget::TrackPan, F, 0.0), "lane_unavailable"),
        (add(MASTER_ROW, LaneTarget::ClipVolume, F, 0.0), "lane_unavailable"),
        (add(MASTER_ROW, LaneTarget::TrackVolume, 1, 0.0), "not_sample_aligned"),
        (add(MASTER_ROW, LaneTarget::TrackVolume, 51 * F, 0.0), "outside_sequence"),
        (add(MASTER_ROW, LaneTarget::TrackVolume, F, 12.5), "value_out_of_range"),
        (add(MASTER_ROW, LaneTarget::BusParam { instance_id: "fx-nope".into(), key: "db".into() }, F, 0.0), "unknown_effect"),
        (AutomationCommand::DeletePoint { track_id: MASTER_ROW.into(), lane: LaneTarget::TrackVolume, point: PointId { clip_id: None, offset_ticks: 0 } }, "unknown_point"),
        (AutomationCommand::ShowLane { track_id: MASTER_ROW.into(), lane: Some(LaneTarget::TrackPan) }, "lane_unavailable"),
    ];
    for (c, code) in cases {
        let e = v.multitrack_command(c.clone()).unwrap_err();
        assert!(e.starts_with(code), "{c:?}: {e} (want {code})");
    }
    assert_eq!(undo_steps(&v), steps, "{:?}", v.run().history());
    let r = routing(&v);
    assert!(r.clip_pans.is_empty() && r.master.volume_automation.is_empty());
}

// ---- slice 2: master row ---------------------------------------------------------------------------

/// The master row is drawn under the tracks; its **A** button (a real click) opens the master Volume
/// lane, and a real click in the lane adds a point; typed points ramp the master 0 dB -> -6.02 dB over
/// 20 frames (the mixer's output ratio follows, linear in dB) and replace the static master volume;
/// a master effect's parameter is a lane too and plays (a gain effect keyframed 0 -> -6.02 dB).
#[test]
fn master_row_lanes_edit_and_play() {
    let (_s, mut v) = opened("master");
    // A static master volume of 0.25 (no lane yet: the fader sets the field).
    v.mixer_edit(MixerEdit::MasterVolume { linear: 0.25 }).unwrap();
    assert_eq!(routing(&v).master.volume, 0.25);
    let ctx = egui::Context::default();
    let l = settle(&mut v, &ctx);
    let row = l.master.expect("master row drawn under the tracks");
    assert!(l.tracks.iter().all(|t| t.rect.max.y <= row.min.y), "below every track");
    assert_eq!(v.state_json()["timeline"]["audio"]["master_row_drawn"], true);
    assert!(v.drawn().iter().any(|d| d == timeline::widget::MASTER), "{:?}", v.drawn());
    assert_eq!(v.timeline_lane_choices(MASTER_ROW), vec![LaneTarget::TrackVolume]);
    // A real click on the master A button shows the master volume lane.
    let button = egui::pos2(l.lanes_left - LANE_BUTTON_PX / 2.0 - 2.0, row.min.y + 2.0 + LANE_BUTTON_PX / 2.0);
    click(&mut v, &ctx, button);
    assert_eq!(v.timeline().unwrap().audio().lane(MASTER_ROW), Some(&LaneTarget::TrackVolume), "{}", v.state_json()["status"]);
    let l = settle(&mut v, &ctx);
    let lane = l.lane(MASTER_ROW).expect("master lane row drawn").rect;
    let ppf = v.timeline().unwrap().view.px_per_frame as f32;
    let steps = undo_steps(&v);
    click(&mut v, &ctx, egui::pos2(l.lanes_left + 20.0 * ppf, lane_y(lane, CLIP_GAIN_DB, 0.0)));
    assert_eq!(undo_steps(&v), steps + 1, "{}", v.state_json()["status"]);
    let pts = routing(&v).master.volume_automation;
    let px_db = (CLIP_GAIN_DB.1 - CLIP_GAIN_DB.0) / f64::from(lane.height() - 2.0 * timeline::LANE_PAD);
    assert!(pts.len() == 1 && pts[0].offset_ticks == 20 * F && (20.0 * pts[0].value.log10()).abs() <= px_db, "{pts:?}");
    let _ = settle(&mut v, &ctx);
    assert!(v.drawn().iter().any(|d| *d == timeline::widget::lane_point(MASTER_ROW, None, 20 * F)), "{:?}", v.drawn());
    // Typed points: 0 dB at 0, -6.02 dB at frame 20 (replacing the clicked point); the static master
    // volume (0.25) is no longer heard.
    let half_db = 20.0 * 0.5f64.log10();
    let add = |at: u64, value: f64| AutomationCommand::AddPoint { track_id: MASTER_ROW.into(), lane: LaneTarget::TrackVolume, at: Ticks::new(at), value };
    v.multitrack_command(add(0, 0.0)).unwrap();
    v.multitrack_command(add(20 * F, half_db)).unwrap();
    let pts: Vec<(u64, f64)> = routing(&v).master.volume_automation.iter().map(|p| (p.offset_ticks, p.value)).collect();
    assert!(pts.len() == 2 && pts[0] == (0, 1.0) && pts[1].0 == 20 * F && (pts[1].1 - 0.5).abs() < 1e-12, "{pts:?}");
    assert_eq!(v.state_json()["timeline"]["audio"]["master_row"]["volume_automated"], true);
    v.open_multitrack(RATE, 960, sources(), Arc::new(Ext)).unwrap();
    let [l0, _] = v.mixer_render(30 * SPF).unwrap();
    // The A1 tone is mono 0.5 at centre: 0.5 cos(pi/4) at 0 dB.
    let c = 0.5 * std::f64::consts::FRAC_1_SQRT_2;
    assert!((f64::from(l0[0]) - c).abs() < 2e-6, "master lane at 0 dB, not the static 0.25: {}", l0[0]);
    assert!((f64::from(l0[10 * SPF]) / f64::from(l0[0]) - 10f64.powf(half_db / 2.0 / 20.0)).abs() < 1e-5, "ramp linear in dB");
    assert!((f64::from(l0[25 * SPF]) / f64::from(l0[0]) - 0.5).abs() < 1e-5, "held after the last point");
    // A master gain effect: its `db` parameter becomes a master lane; keyframes play.
    let (_, gain) = effect_templates().into_iter().find(|(n, _)| *n == "gain").unwrap();
    v.mixer_edit(MixerEdit::AddEffect { bus: None, instance: gain.to_preset_instance("fx-0"), at: None }).unwrap();
    let param = LaneTarget::BusParam { instance_id: "fx-0".into(), key: "db".into() };
    assert_eq!(v.timeline_lane_choices(MASTER_ROW), vec![LaneTarget::TrackVolume, param.clone()]);
    v.multitrack_command(AutomationCommand::DeletePoint { track_id: MASTER_ROW.into(), lane: LaneTarget::TrackVolume, point: PointId { clip_id: None, offset_ticks: 20 * F } }).unwrap();
    for (at, db) in [(0, 0.0), (20 * F, half_db)] {
        v.multitrack_command(AutomationCommand::AddPoint { track_id: MASTER_ROW.into(), lane: param.clone(), at: Ticks::new(at), value: db }).unwrap();
    }
    let view = v.timeline_lane(MASTER_ROW, &param).unwrap();
    assert!(view.error.is_none() && view.points().count() == 2, "{view:?}");
    v.open_multitrack(RATE, 960, sources(), Arc::new(Ext)).unwrap();
    let [l1, _] = v.mixer_render(30 * SPF).unwrap();
    assert!((f64::from(l1[0]) - c).abs() < 2e-6, "{}", l1[0]);
    assert!((f64::from(l1[25 * SPF]) / f64::from(l1[0]) - 0.5).abs() < 1e-4, "master effect keyframes play: {} / {}", l1[25 * SPF], l1[0]);
}

// ---- slice 3: automated fader ----------------------------------------------------------------------

/// On a track with a volume lane the mixer shows the AUTO state (JSON and a drawn label) and a fader
/// move writes the lane at the playhead (frame 10) as ONE undo step, leaving the static volume; pan
/// and the master volume do the same; an unautomated fader still sets the static field; without a
/// timeline playhead the write is refused by name and nothing is stored.
#[test]
fn automated_fader_shows_auto_and_writes_the_lane_at_the_playhead() {
    let (s, mut v) = opened("fader");
    let a1 = tid(2);
    for (at, value) in [(0, 0.0), (20 * F, -12.0)] {
        v.multitrack_command(AutomationCommand::AddPoint { track_id: a1.clone(), lane: LaneTarget::TrackVolume, at: Ticks::new(at), value }).unwrap();
    }
    v.multitrack_command(AutomationCommand::AddPoint { track_id: a1.clone(), lane: LaneTarget::TrackPan, at: Ticks::new(0), value: 0.0 }).unwrap();
    v.multitrack_command(AutomationCommand::AddPoint { track_id: MASTER_ROW.into(), lane: LaneTarget::TrackVolume, at: Ticks::new(0), value: 0.0 }).unwrap();
    v.timeline_command(TimelineCommand::Scrub(Ticks::new(10 * F))).unwrap();
    let strip = |v: &Viewer| v.mixer_json()["strips"].as_array().unwrap().iter().find(|s| s["track_id"] == a1.as_str()).cloned().unwrap();
    let st = strip(&v);
    assert_eq!((st["volume_automated"].clone(), st["pan_automated"].clone()), (true.into(), true.into()), "{st}");
    assert!((st["lane_volume_db"].as_f64().unwrap() + 6.0).abs() < 1e-9, "lane value at the playhead: {st}");
    assert_eq!(v.mixer_json()["master"]["volume_automated"], true);
    let b1 = v.mixer_json()["strips"].as_array().unwrap().iter().find(|s| s["track_id"] == tid(3).as_str()).cloned().unwrap();
    assert_eq!(b1["volume_automated"], false);
    // The AUTO labels are drawn.
    let ctx = egui::Context::default();
    let _ = settle(&mut v, &ctx);
    for name in [format!("mixer.{a1}.volume.automated"), format!("mixer.{a1}.pan.automated"), "mixer.master.volume.automated".to_owned()] {
        assert!(v.drawn().contains(&name), "{name} not drawn: {:?}", v.drawn());
    }
    assert!(!v.drawn().contains(&format!("mixer.{}.volume.automated", tid(3))));
    // Fader move: one step, a lane point at frame 10, static volume unchanged.
    let steps = undo_steps(&v);
    v.mixer_edit(MixerEdit::Volume { track_id: a1.clone(), linear: 0.25 }).unwrap();
    assert_eq!(undo_steps(&v), steps + 1, "{:?}", v.run().history());
    let lane: Vec<(u64, f64)> = routing(&v).track_lane(&a1, StudioTrackParam::Volume).unwrap().points.iter().map(|p| (p.offset_ticks, p.value)).collect();
    assert_points(&lane, &[(0, 1.0), (10 * F, 0.25), (20 * F, 10f64.powf(-12.0 / 20.0))], "volume lane written at the playhead");
    let seq = v.timeline_sequence().cloned().unwrap();
    assert_eq!(seq.tracks[1].volume, 1.0, "the static fader field is left as it is");
    // Again at the same tick: the point is replaced (one more step).
    v.mixer_edit(MixerEdit::Volume { track_id: a1.clone(), linear: 0.5 }).unwrap();
    assert_eq!(routing(&v).track_lane(&a1, StudioTrackParam::Volume).unwrap().points[1].value, 0.5);
    v.mixer_edit(MixerEdit::Pan { track_id: a1.clone(), pan: -0.5 }).unwrap();
    assert_eq!(routing(&v).track_lane(&a1, StudioTrackParam::Pan).unwrap().points.iter().map(|p| (p.offset_ticks, p.value)).collect::<Vec<_>>(), vec![(0, 0.0), (10 * F, -0.5)]);
    assert_eq!(v.timeline_sequence().unwrap().tracks[1].pan, 0.0);
    v.mixer_edit(MixerEdit::MasterVolume { linear: 0.5 }).unwrap();
    let m = routing(&v).master;
    assert_eq!((m.volume, m.volume_automation.len(), m.volume_automation[1].offset_ticks, m.volume_automation[1].value), (1.0, 2, 10 * F, 0.5));
    assert_eq!(undo_steps(&v), steps + 4);
    v.undo().unwrap();
    assert!(routing(&v).master.volume_automation.len() == 1, "undo removes the master lane point");
    // Hostile: out of range values are refused with no step.
    let steps = undo_steps(&v);
    assert!(v.mixer_edit(MixerEdit::Volume { track_id: a1.clone(), linear: f64::NAN }).unwrap_err().contains("outside"));
    assert!(v.mixer_edit(MixerEdit::Pan { track_id: a1.clone(), pan: 2.0 }).unwrap_err().contains("outside"));
    assert_eq!(undo_steps(&v), steps);
    // An unautomated fader (B1) still sets the static field.
    v.mixer_edit(MixerEdit::Volume { track_id: tid(3), linear: 0.5 }).unwrap();
    assert_eq!(v.timeline_sequence().unwrap().tracks[2].volume, 0.5);

    // Without a timeline there is no playhead: refused by name, nothing stored.
    let saved = s.0.join("automated.handshake");
    v.save(&saved, false).unwrap();
    let mut w = crate::support_context::viewer(Prefer::Cpu);
    w.open(&saved).unwrap_or_else(|e| panic!("{e}"));
    w.open_mixer("seq-main", RATE, 960, sources(), Arc::new(Ext)).unwrap();
    let before = w.run().document_entry("audio_routing").map(<[u8]>::to_vec);
    let e = w.mixer_edit(MixerEdit::Volume { track_id: a1.clone(), linear: 0.25 }).unwrap_err();
    assert!(e.contains("fader_automated_no_playhead"), "{e}");
    assert_eq!(w.run().document_entry("audio_routing").map(<[u8]>::to_vec), before);
}
