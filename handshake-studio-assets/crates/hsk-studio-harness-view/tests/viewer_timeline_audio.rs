//! Lane SMB-AU-MULTITRACK-1 (feature `audio-batch`): the timeline panel as a multitrack view,
//! driven headlessly (one-frame egui draws into a bare context, no window, CX-SAFE-002). Audio clip
//! blocks show the min / max overview of their source window; an audio track's lane edits its
//! clips' volume envelopes and a submix track's lane the keyframes of a bus effect parameter, by
//! typed commands and by real pointer clicks and drags, one undo step each; clip gain shifts an
//! envelope; and the mixer's bus stream (the one mixdown path) plays what is shown. Oracles:
//! hand-computed samples of a half-silent constant source (the overview columns and the mix
//! ratios 10^(dB/20)), the run's routing entry and history labels, and the drawn pixels of the
//! layout.
#![cfg(feature = "audio-batch")]

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::audio_batch::EffectSpec;
use hsk_studio_harness_view::view::mixer::MixerEdit;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::timeline::{self, AutomationCommand, LaneTarget, PointId, lane_y};
use hsk_studio_pulse::{AudioSources, SourceExtents};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 25 fps ticks per frame (STU-VID-013); 1920 samples per frame at 48 kHz.
const F: u64 = 10_160_640_000;
const SPF: usize = 1920;
const RATE: u32 = 48_000;
/// -6.0206 dB = a linear gain of one half.
const HALF_DB: f64 = -6.020_599_913_279_624;

// ---- fixtures --------------------------------------------------------------------------------------

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let base = std::env::temp_dir();
        let dir = base.join(format!("hsk-harness-view-timeline-audio-{}-{label}", std::process::id()));
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

/// V1: clip 1 `picture` at [10, 30). A1: clip 3 `half` source [0, 50) at frames [0, 50). B1: an
/// empty submix track.
fn sequence() -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(50 * F),
        tracks: vec![
            track(1, TrackKind::Video, "V1", vec![clip(1, "picture", 0, 20, 10)]),
            track(2, TrackKind::Audio, "A1", vec![clip(3, "half", 0, 50, 0)]),
            track(3, TrackKind::Submix, "B1", vec![]),
        ],
        markers: Vec::new(),
        work_area: None,
    }
}

/// Source frame where `half` turns from silence to 0.5 (sample 57 600 = 225 x 256, so the overview
/// columns of 256 samples never mix the two levels).
const STEP_FRAME: u64 = 30;

/// `half`: mono, 52 frames; zero before [`STEP_FRAME`], 0.5 from it on.
#[derive(Clone)]
struct Sources(Arc<Vec<(String, Vec<Vec<f32>>)>>);
fn sources() -> Sources {
    let n = 52 * SPF;
    let step = STEP_FRAME as usize * SPF;
    let half: Vec<f32> = (0..n).map(|k| if k < step { 0.0 } else { 0.5 }).collect();
    Sources(Arc::new(vec![("half".into(), vec![half])]))
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
        Some(Ticks::new(52 * F))
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
fn opened(label: &str) -> (Scratch, Viewer, egui::Context) {
    let scratch = Scratch::new(label);
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&sequence_package(&scratch.0)).unwrap_or_else(|e| panic!("{e}"));
    bind(&mut v);
    (scratch, v, egui::Context::default())
}

/// (Re)binds the source port: the mixer's bus stream restarts at sample 0.
fn bind(v: &mut Viewer) {
    v.open_multitrack(RATE, 960, sources(), Arc::new(Ext)).unwrap_or_else(|e| panic!("{e}: {}", v.state_json()["status"]));
}

// ---- egui driving ----------------------------------------------------------------------------------

fn input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 1200.0))), events, ..Default::default() }
}

fn frame(v: &mut Viewer, ctx: &egui::Context, events: Vec<egui::Event>) {
    let _ = ctx.run(input(events), |ctx| v.ui(ctx));
}

fn button(pos: egui::Pos2, which: egui::PointerButton, pressed: bool) -> egui::Event {
    egui::Event::PointerButton { pos, button: which, pressed, modifiers: egui::Modifiers::NONE }
}

fn click(v: &mut Viewer, ctx: &egui::Context, at: egui::Pos2, which: egui::PointerButton) {
    frame(v, ctx, vec![egui::Event::PointerMoved(at)]);
    frame(v, ctx, vec![button(at, which, true)]);
    frame(v, ctx, vec![button(at, which, false)]);
    frame(v, ctx, vec![]);
}

/// A real pointer drag from `from` to `to` over four UI frames (hover, press, move, release).
fn drag(v: &mut Viewer, ctx: &egui::Context, from: egui::Pos2, to: egui::Pos2) {
    frame(v, ctx, vec![egui::Event::PointerMoved(from)]);
    frame(v, ctx, vec![button(from, egui::PointerButton::Primary, true)]);
    frame(v, ctx, vec![egui::Event::PointerMoved(to)]);
    frame(v, ctx, vec![egui::Event::PointerMoved(to), button(to, egui::PointerButton::Primary, false)]);
    frame(v, ctx, vec![]);
}

/// Runs frames until the timeline layout is the same in two frames in a row (R1 lesson of lane
/// SMB-TIMELINE-UI-1: the first frames of a fresh context size the panels differently).
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

fn drew(v: &Viewer, name: &str) -> bool {
    v.drawn().iter().any(|d| d == name)
}

fn undo_steps(v: &Viewer) -> usize {
    v.run().history()["undo"].as_array().map_or(0, Vec::len)
}

fn routing(v: &Viewer) -> StudioAudioRouting {
    v.audio_routing().unwrap().and_then(|s| s.routings.into_iter().find(|r| r.sequence_id == "seq-main")).unwrap_or_else(|| StudioAudioRouting::new("seq-main"))
}

fn gain_points(v: &Viewer, n: u32) -> Vec<(u64, f64)> {
    routing(v).clip_gains.iter().find(|g| g.clip_id == cid(n)).map(|g| g.points.iter().map(|p| (p.offset_ticks, p.gain_db)).collect()).unwrap_or_default()
}

fn render(v: &mut Viewer, frames: usize) -> Vec<f32> {
    let [l, r] = v.mixer_render(frames * SPF).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(l, r, "a centred mono clip renders the same on both sides");
    l
}

fn close(a: f32, b: f64, tol: f64, what: &str) {
    assert!((f64::from(a) - b).abs() <= tol, "{what}: {a} vs {b}");
}

// ---- tests -----------------------------------------------------------------------------------------

/// The audio clip block draws its waveform overview: columns over the source window read the
/// source's own samples (silence then 0.5), the panel names the overview, and Waveforms hides it.
#[test]
fn audio_clips_show_the_overview_of_their_source_window() {
    let (_s, mut v, ctx) = opened("wave");
    let s = v.state_json();
    assert_eq!(s["timeline"]["audio"]["bound_rate"], RATE, "{}", s["timeline"]);
    assert_eq!(s["timeline"]["audio"]["mixer_open"], true);
    // Source window [0, 50) frames = 96 000 samples in 10 columns of 9 600: columns 0..=5 lie
    // before sample 57 600 (silence), 6..=9 after it (0.5).
    let cols = v.timeline_clip_peaks(&cid(3), 10).unwrap();
    assert_eq!(cols.len(), 10);
    for (j, c) in cols.iter().enumerate() {
        let want = if (j as u64) < STEP_FRAME * 10 / 50 { (0.0, 0.0) } else { (0.5, 0.5) };
        assert_eq!(*c, want, "column {j}");
    }
    // Only audio clips have an overview; refusals are named.
    assert_eq!(v.timeline_clip_peaks(&cid(1), 10).unwrap_err(), "unknown_audio_clip");
    assert_eq!(v.timeline_clip_peaks(&cid(3), 0).unwrap_err(), "waveform_out_of_range");
    // Drawn in the panel; the toggle hides it (a view setting: no history step).
    let _ = settle(&mut v, &ctx);
    assert!(drew(&v, &timeline::widget::clip_waveform(&cid(3))), "{:?}", v.drawn());
    assert!(!drew(&v, &timeline::widget::clip_waveform(&cid(1))), "video clips have no audio overview");
    assert!(drew(&v, timeline::widget::WAVEFORMS));
    let steps = undo_steps(&v);
    v.multitrack_command(AutomationCommand::ToggleWaveforms).unwrap();
    frame(&mut v, &ctx, vec![]);
    assert!(!drew(&v, &timeline::widget::clip_waveform(&cid(3))));
    assert_eq!(v.state_json()["timeline"]["audio"]["waveforms"], false);
    assert_eq!(undo_steps(&v), steps);
}

/// Clip gain and a clip volume point are one undo step each, stored in the routing entry, and the
/// mixer's bus stream plays them: -6.02 dB halves the clip; a 0 dB point at frame 40 ramps back to
/// unity linearly in dB; undo restores the unprocessed mix.
#[test]
fn clip_gain_and_volume_points_play_through_the_mixer_and_undo() {
    let (_s, mut v, _ctx) = opened("mix");
    let base = render(&mut v, 45);
    let k = 35 * SPF;
    assert!(base[k] > 0.1, "the 0.5 region plays: {}", base[k]);
    let steps = undo_steps(&v);
    v.multitrack_command(AutomationCommand::ClipGain { clip_id: cid(3), delta_db: HALF_DB }).unwrap();
    assert_eq!(undo_steps(&v), steps + 1, "{:?}", v.run().history());
    assert_eq!(gain_points(&v, 3), vec![(0, HALF_DB)]);
    bind(&mut v);
    let half = render(&mut v, 45);
    close(half[k], 0.5 * f64::from(base[k]), 1e-5, "clip gain halves the clip");
    // A 0 dB point at frame 40 (A1 clip volume lane): offsets from the clip start.
    let a1 = tid(2);
    v.multitrack_command(AutomationCommand::ShowLane { track_id: a1.clone(), lane: Some(LaneTarget::ClipVolume) }).unwrap();
    v.multitrack_command(AutomationCommand::AddPoint { track_id: a1.clone(), lane: LaneTarget::ClipVolume, at: Ticks::new(40 * F), value: 0.0 }).unwrap();
    assert_eq!(undo_steps(&v), steps + 2);
    assert_eq!(gain_points(&v, 3), vec![(0, HALF_DB), (40 * F, 0.0)]);
    bind(&mut v);
    let ramp = render(&mut v, 45);
    // Linear in dB between sample 0 and 76 800: at 67 200 the gain is HALF_DB x 1/8.
    close(ramp[k], f64::from(base[k]) * 10f64.powf(HALF_DB / 8.0 / 20.0), 1e-5, "ramp at frame 35");
    close(ramp[40 * SPF], f64::from(base[40 * SPF]), 1e-5, "unity at the point");
    close(ramp[44 * SPF], f64::from(base[44 * SPF]), 1e-5, "held after the last point");
    // The lane projection shows both points.
    let lanes = &v.state_json()["timeline"]["audio"]["lanes"];
    assert_eq!(lanes[0]["lane"], "clip_volume");
    assert_eq!(lanes[0]["points"].as_array().map(Vec::len), Some(2), "{lanes}");
    // EDIT > Undo twice: the entry returns to no envelope and the mix to the original.
    v.undo().unwrap();
    assert_eq!(gain_points(&v, 3), vec![(0, HALF_DB)]);
    v.undo().unwrap();
    assert!(routing(&v).clip_gains.is_empty());
    bind(&mut v);
    let again = render(&mut v, 45);
    close(again[k], f64::from(base[k]), 1e-6, "undo restores the mix");
}

/// The lane is opened by a real click on the A1 header's lane button, a click in the lane adds a
/// point at the clicked frame and value, a drag moves it to the frame under the pointer, a right
/// click deletes it; one undo step per gesture and the handles are named.
#[test]
fn lane_clicks_and_drags_edit_points_at_their_pixels() {
    let (_s, mut v, ctx) = opened("gui");
    let a1 = tid(2);
    let l = settle(&mut v, &ctx);
    assert!(drew(&v, &timeline::widget::lane_toggle(&a1)), "{:?}", v.drawn());
    assert!(!drew(&v, &timeline::widget::lane_toggle(&tid(1))), "video tracks have no lane button");
    let row = l.tracks.iter().find(|r| r.track_id == a1).unwrap().rect;
    let toggle = egui::pos2(l.lanes_left - timeline::LANE_BUTTON_PX / 2.0 - 2.0, row.min.y + 2.0 + timeline::LANE_BUTTON_PX / 2.0);
    click(&mut v, &ctx, toggle, egui::PointerButton::Primary);
    assert_eq!(v.timeline().unwrap().audio().lane(&a1), Some(&LaneTarget::ClipVolume), "{}", v.state_json()["status"]);
    let l = settle(&mut v, &ctx);
    let lane = l.lane(&a1).expect("lane row drawn").rect;
    assert_eq!(lane.height(), timeline::LANE_PX);
    assert_eq!(lane.min.y, row.max.y, "the lane sits under its track");
    let b1 = l.tracks.iter().find(|r| r.track_id == tid(3)).unwrap().rect;
    assert_eq!(b1.min.y, lane.max.y, "the next track moves down by the lane");
    assert!(drew(&v, &timeline::widget::lane(&a1)));
    // Click at frame 20, at the height of 0 dB.
    let ppf = v.timeline().unwrap().view.px_per_frame as f32;
    let x20 = l.lanes_left + 20.0 * ppf;
    let steps = undo_steps(&v);
    click(&mut v, &ctx, egui::pos2(x20, lane_y(lane, CLIP_GAIN_DB, 0.0)), egui::PointerButton::Primary);
    assert_eq!(undo_steps(&v), steps + 1, "{}", v.state_json()["status"]);
    let pts = gain_points(&v, 3);
    assert_eq!(pts.len(), 1, "{pts:?}");
    let px_db = (CLIP_GAIN_DB.1 - CLIP_GAIN_DB.0) / f64::from(lane.height() - 2.0 * timeline::LANE_PAD);
    assert_eq!(pts[0].0, 20 * F);
    assert!(pts[0].1.abs() <= px_db, "value within one pixel of 0 dB: {}", pts[0].1);
    // Drag the handle 10 frames right at the same height.
    let l = settle(&mut v, &ctx);
    let lane = l.lane(&a1).unwrap().rect;
    let handle = egui::pos2(x20, lane_y(lane, CLIP_GAIN_DB, pts[0].1));
    assert!(drew(&v, &timeline::widget::lane_point(&a1, Some(&cid(3)), 20 * F)), "{:?}", v.drawn());
    drag(&mut v, &ctx, handle, egui::pos2(x20 + 10.0 * ppf, handle.y));
    assert_eq!(undo_steps(&v), steps + 2, "{}", v.state_json()["status"]);
    let moved = gain_points(&v, 3);
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].0, 30 * F);
    assert!((moved[0].1 - pts[0].1).abs() <= px_db, "{moved:?}");
    // Right click deletes it (the envelope goes with its last point).
    let _ = settle(&mut v, &ctx);
    click(&mut v, &ctx, egui::pos2(x20 + 10.0 * ppf, handle.y), egui::PointerButton::Secondary);
    assert_eq!(undo_steps(&v), steps + 3, "{}", v.state_json()["status"]);
    assert!(routing(&v).clip_gains.is_empty());
    // The button cycles on: volume (static), pan (static), then none.
    for want in [Some(LaneTarget::TrackVolume), Some(LaneTarget::TrackPan), None] {
        v.multitrack_command(AutomationCommand::CycleLane { track_id: a1.clone() }).unwrap();
        assert_eq!(v.timeline().unwrap().audio().lane(&a1), want.as_ref());
    }
}

/// A submix track's lane keyframes a bus effect parameter: with A1 routed through B1 and a gain
/// effect on B1, a -6.02 dB keyframe halves the mix; keyframes off the sample grid are refused.
#[test]
fn bus_effect_parameter_keyframes_automate_the_submix() {
    let (_s, mut v, _ctx) = opened("bus");
    let (a1, b1) = (tid(2), tid(3));
    v.mixer_edit(MixerEdit::Output { track_id: a1.clone(), bus: Some(b1.clone()) }).unwrap();
    v.mixer_edit(MixerEdit::AddEffect { bus: Some(b1.clone()), instance: EffectSpec::Gain { db: 0.0 }.to_preset_instance("fx-0"), at: None }).unwrap();
    let lane = LaneTarget::BusParam { instance_id: "fx-0".into(), key: "db".into() };
    assert!(v.timeline_lane_choices(&b1).contains(&lane), "{:?}", v.timeline_lane_choices(&b1));
    assert!(!v.timeline_lane_choices(&b1).contains(&LaneTarget::ClipVolume));
    bind(&mut v);
    let base = render(&mut v, 40);
    let k = 35 * SPF;
    v.multitrack_command(AutomationCommand::ShowLane { track_id: b1.clone(), lane: Some(lane.clone()) }).unwrap();
    let steps = undo_steps(&v);
    v.multitrack_command(AutomationCommand::AddPoint { track_id: b1.clone(), lane: lane.clone(), at: Ticks::new(0), value: HALF_DB }).unwrap();
    assert_eq!(undo_steps(&v), steps + 1);
    let fx = &routing(&v).bus_effects[0].effects[0];
    assert_eq!(fx.keyframes.iter().map(|k| (k.key.as_str(), k.offset_ticks, k.value)).collect::<Vec<_>>(), vec![("db", 0, HALF_DB)]);
    bind(&mut v);
    let half = render(&mut v, 40);
    close(half[k], 0.5 * f64::from(base[k]), 1e-5, "bus gain keyframe");
    let view = v.timeline_lane(&b1, &lane).unwrap();
    assert_eq!(view.points().count(), 1);
    assert!(view.error.is_none(), "{:?}", view.error);
    // One tick is not a whole sample at 48 kHz; values outside the parameter range are refused.
    let refused = |v: &mut Viewer, c: AutomationCommand| v.multitrack_command(c).unwrap_err();
    assert_eq!(refused(&mut v, AutomationCommand::AddPoint { track_id: b1.clone(), lane: lane.clone(), at: Ticks::new(1), value: 0.0 }), "not_sample_aligned");
    assert!(refused(&mut v, AutomationCommand::AddPoint { track_id: b1.clone(), lane: lane.clone(), at: Ticks::new(F), value: 1e9 }).starts_with("value_out_of_range"));
    assert_eq!(refused(&mut v, AutomationCommand::AddPoint { track_id: b1.clone(), lane: LaneTarget::BusParam { instance_id: "fx-9".into(), key: "db".into() }, at: Ticks::new(F), value: 0.0 }), "unknown_effect");
    assert_eq!(undo_steps(&v), steps + 1);
    // Moving the keyframe to frame 10 and deleting it are one step each.
    v.multitrack_command(AutomationCommand::MovePoint { track_id: b1.clone(), lane: lane.clone(), point: PointId { clip_id: None, offset_ticks: 0 }, at: Ticks::new(10 * F), value: -3.0 }).unwrap();
    v.multitrack_command(AutomationCommand::DeletePoint { track_id: b1.clone(), lane: lane.clone(), point: PointId { clip_id: None, offset_ticks: 10 * F } }).unwrap();
    assert_eq!(undo_steps(&v), steps + 3);
    assert!(routing(&v).bus_effects[0].effects[0].keyframes.is_empty());
}

/// Hostile and impossible lane commands are refused by name and store nothing.
#[test]
fn hostile_lane_commands_are_refused_without_a_step() {
    let (_s, mut v, _ctx) = opened("hostile");
    let (v1, a1) = (tid(1), tid(2));
    let steps = undo_steps(&v);
    let add = |track: &str, lane: LaneTarget, frame: u64, value: f64| AutomationCommand::AddPoint { track_id: track.into(), lane, at: Ticks::new(frame * F), value };
    let cases: Vec<(AutomationCommand, &str)> = vec![
        (add(&a1, LaneTarget::ClipVolume, 10, f64::NAN), "value_not_finite"),
        (add(&a1, LaneTarget::ClipVolume, 10, f64::INFINITY), "value_not_finite"),
        (add(&a1, LaneTarget::ClipVolume, 10, 12.5), "value_out_of_range"),
        (add(&a1, LaneTarget::ClipVolume, 10, -96.5), "value_out_of_range"),
        (add(&a1, LaneTarget::ClipVolume, 55, 0.0), "no_clip_at"),
        (add(&a1, LaneTarget::TrackVolume, 10, 0.0), "track_automation_not_stored"),
        (add(&a1, LaneTarget::TrackPan, 10, 0.0), "track_automation_not_stored"),
        (add("STRK-nope", LaneTarget::ClipVolume, 10, 0.0), "unknown_track"),
        (add(&v1, LaneTarget::ClipVolume, 15, 0.0), "lane_unavailable"),
        (
            AutomationCommand::MovePoint { track_id: a1.clone(), lane: LaneTarget::ClipVolume, point: PointId { clip_id: Some(cid(3)), offset_ticks: 7 }, at: Ticks::new(F), value: 0.0 },
            "unknown_point",
        ),
        (AutomationCommand::DeletePoint { track_id: a1.clone(), lane: LaneTarget::ClipVolume, point: PointId { clip_id: None, offset_ticks: 0 } }, "unknown_point"),
        (AutomationCommand::ClipGain { clip_id: cid(1), delta_db: -1.0 }, "not_an_audio_clip"),
        (AutomationCommand::ClipGain { clip_id: cid(3), delta_db: 200.0 }, "clip_gain_out_of_range"),
        (AutomationCommand::ClipGain { clip_id: cid(3), delta_db: f64::NAN }, "value_not_finite"),
        (AutomationCommand::ClipGain { clip_id: cid(3), delta_db: 0.0 }, "no_change"),
        (AutomationCommand::ClipGain { clip_id: cid(9), delta_db: 1.0 }, "unknown_clip"),
        (AutomationCommand::ShowLane { track_id: v1.clone(), lane: Some(LaneTarget::ClipVolume) }, "lane_unavailable"),
        (AutomationCommand::CycleLane { track_id: v1.clone() }, "no_audio_track"),
    ];
    for (c, code) in cases {
        let e = v.multitrack_command(c.clone()).unwrap_err();
        assert!(e.starts_with(code), "{c:?}: {e} (want {code})");
    }
    assert_eq!(undo_steps(&v), steps, "{:?}", v.run().history());
    assert!(routing(&v).clip_gains.is_empty());
    // A point moved onto another point of the same clip is refused; an add there replaces it.
    v.multitrack_command(add(&a1, LaneTarget::ClipVolume, 10, -1.0)).unwrap();
    v.multitrack_command(add(&a1, LaneTarget::ClipVolume, 20, -2.0)).unwrap();
    let e = v
        .multitrack_command(AutomationCommand::MovePoint { track_id: a1.clone(), lane: LaneTarget::ClipVolume, point: PointId { clip_id: Some(cid(3)), offset_ticks: 10 * F }, at: Ticks::new(20 * F), value: 0.0 })
        .unwrap_err();
    assert_eq!(e, "point_exists");
    v.multitrack_command(add(&a1, LaneTarget::ClipVolume, 20, -3.0)).unwrap();
    assert_eq!(gain_points(&v, 3), vec![(10 * F, -1.0), (20 * F, -3.0)]);
    // A clip gain step that would push a point past +12 dB is refused as a whole.
    v.multitrack_command(add(&a1, LaneTarget::ClipVolume, 30, 11.5)).unwrap();
    assert_eq!(v.multitrack_command(AutomationCommand::ClipGain { clip_id: cid(3), delta_db: 1.0 }).unwrap_err(), "clip_gain_out_of_range");
    assert_eq!(gain_points(&v, 3).len(), 3);
    let s: Value = v.state_json();
    assert!(s["status"].to_string().contains("clip_gain_out_of_range"), "refusals reach the status line");
}
