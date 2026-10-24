//! Lane SMB-PR-RATE-STRETCH-1 (feature `viewer`): the Rate Stretch tool (`R`), the Speed/Duration
//! form (`Ctrl+R`) and slip at non-unit speed in the timeline panel, driven headlessly (one-frame
//! egui draws into a bare context, no window). Oracles: hand-computed frames and exact reduced
//! speeds of a three-clip V1 fixture, the run's document after each edit, the session history (one
//! undo step per edit) and EDIT > Undo restoring the exact prior sequence.
//!
//! NOT_PROVEN (printed): playback / render of a stretched clip's frames (the program monitor has no
//! frame decoder on document sequences); audio at non-unit speed; Premiere's rate-stretch limits,
//! cursor zones and Speed/Duration defaults (no Premiere oracle).
#![cfg(feature = "viewer")]

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::shell::{Panel, ShellCommand};
use hsk_studio_harness_view::view::timeline::{self, DEFAULT_PX_PER_FRAME, SpeedFix, TimelineCommand, TrimTool};
use hsk_studio_pulse::{RateEdge, SpeedDurationTarget};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// 25 fps ticks per frame (STU-VID-013).
const F: u64 = 10_160_640_000;
const PPF: f32 = DEFAULT_PX_PER_FRAME as f32;

// ---- fixtures --------------------------------------------------------------------------------------

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let base = std::env::temp_dir();
        let text = base.to_string_lossy().to_ascii_uppercase();
        assert!(!(cfg!(windows) && text.starts_with("C:")), "TMP is on C: ({}); run through studio-env.sh", base.display());
        let dir = base.join(format!("hsk-harness-view-rate-{}-{label}", std::process::id()));
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

fn media(name: &str) -> ClipSource {
    ClipSource::Media { artifact_manifest_id: name.into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() }, asset_id: None }
}

/// Clip `n` of `name`: source frames `[si, so)` at timeline frame `start` (unit speed).
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
        track_id: format!("STRK-019abcde-0000-7000-8000-{n:012x}"),
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

/// V1: clip 1 `a` source [10, 30) at frames [0, 20), clip 2 `b` source [100, 130) at [30, 60),
/// clip 3 `c` source [10, 30) at [70, 90). A1: clip 4 `d` source [0, 20) at [0, 20).
fn sequence() -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-rate".into(),
        name: "rate".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(90 * F),
        tracks: vec![
            track(1, TrackKind::Video, "V1", vec![clip(1, "a", 10, 30, 0), clip(2, "b", 100, 130, 30), clip(3, "c", 10, 30, 70)]),
            track(2, TrackKind::Audio, "A1", vec![clip(4, "d", 0, 20, 0)]),
        ],
        markers: Vec::new(),
        work_area: None,
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

/// A native package whose document holds `seq`.
fn sequence_package(dir: &Path, seq: StudioSequence) -> PathBuf {
    let doc = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000007","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[serde_json::to_value(seq).unwrap()]
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
    let path = dir.join("rate.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

/// Every source is 1000 frames long (the monitor's extents port).
struct Long;
impl hsk_studio_pulse::SourceExtents for Long {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(1000 * F))
    }
}
struct NoFrames;
impl hsk_studio_harness_view::view::monitor::FrameSource for NoFrames {
    fn frame(&self, _: &ClipSource, _: Ticks) -> Option<std::sync::Arc<hsk_studio_harness_view::view::monitor::SourceFrame>> {
        None
    }
}

/// The viewer with the (edited) fixture open, the timeline panel shown and 1000-frame sources.
fn opened_with(label: &str, edit: impl FnOnce(&mut StudioSequence)) -> (Scratch, Viewer, egui::Context) {
    let scratch = Scratch::new(label);
    let mut seq = sequence();
    edit(&mut seq);
    let mut v = Viewer::new(Prefer::Cpu);
    let ctx = egui::Context::default();
    v.open(&sequence_package(&scratch.0, seq)).unwrap_or_else(|e| panic!("{e}"));
    v.shell_command(ShellCommand::Panel(Panel::Timeline)).unwrap();
    v.set_monitor_sources(Box::new(NoFrames), Box::new(Long));
    (scratch, v, ctx)
}

fn opened(label: &str) -> (Scratch, Viewer, egui::Context) {
    opened_with(label, |_| {})
}

// ---- egui driving ----------------------------------------------------------------------------------

fn input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 1200.0))), events, ..Default::default() }
}

fn frame(v: &mut Viewer, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
    ctx.run(input(events), |ctx| v.ui(ctx))
}

fn key(k: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers }
}

fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
}

/// A real pointer drag from `from` to `to` (hover, press, move, release, idle).
fn drag(v: &mut Viewer, ctx: &egui::Context, from: egui::Pos2, to: egui::Pos2) {
    let _ = frame(v, ctx, vec![egui::Event::PointerMoved(from)]);
    let _ = frame(v, ctx, vec![button(from, true)]);
    let _ = frame(v, ctx, vec![egui::Event::PointerMoved(to)]);
    let _ = frame(v, ctx, vec![egui::Event::PointerMoved(to), button(to, false)]);
    let _ = frame(v, ctx, vec![]);
}

/// Frames until the timeline layout repeats (F-TL-2: the first frame lays out differently).
fn settle(v: &mut Viewer, ctx: &egui::Context) {
    let mut last = None;
    for _ in 0..6 {
        let _ = frame(v, ctx, vec![]);
        let now = v.timeline().and_then(|t| t.layout()).cloned();
        if now.is_some() && now == last {
            return;
        }
        last = now;
    }
    panic!("timeline layout did not settle in 6 frames");
}

fn seq(v: &Viewer) -> StudioSequence {
    v.timeline_sequence().expect("timeline sequence").clone()
}

fn get(v: &Viewer, n: u32) -> StudioClip {
    seq(v).tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.clip_id == cid(n)).cloned().unwrap_or_else(|| panic!("clip {n} missing"))
}

/// (timeline start, end, source in, source out) of clip `n` in frames.
fn window(v: &Viewer, n: u32) -> (u64, u64, u64, u64) {
    let c = get(v, n);
    (c.timeline_start.value() / F, c.timeline_end().unwrap().value() / F, c.source_in.value() / F, c.source_out.value() / F)
}

fn speed(v: &Viewer, n: u32) -> (i64, u64) {
    let s = get(v, n).speed;
    (s.numerator, s.denominator)
}

fn undo_len(v: &Viewer) -> usize {
    v.run().history()["undo"].as_array().unwrap().len()
}

fn last_undo(v: &Viewer) -> String {
    v.run().history()["undo"].as_array().unwrap().last().and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn status(v: &Viewer) -> Value {
    v.state_json()["status"].clone()
}

fn block(v: &Viewer, n: u32) -> timeline::ClipBlock {
    v.timeline().and_then(|t| t.layout()).and_then(|l| l.clip(&cid(n))).cloned().unwrap_or_else(|| panic!("clip {n} not drawn"))
}

// ---- slice 2: Rate Stretch tool and Speed/Duration ----------------------------------------------------

/// `R` with the panel active selects the Rate Stretch tool (spec command `cmd.tools.05ratestretch`);
/// its toolbar selector is drawn.
#[test]
fn r_selects_the_rate_stretch_tool() {
    let (_s, mut v, ctx) = opened("tool");
    settle(&mut v, &ctx);
    assert!(v.drawn().iter().any(|d| d == &timeline::widget::tool("rate_stretch")), "Rate Stretch selector drawn");
    assert_eq!(v.state_json()["timeline"]["tool"], "selection");
    let _ = frame(&mut v, &ctx, vec![key(egui::Key::R, egui::Modifiers::NONE)]);
    assert_eq!(v.state_json()["timeline"]["tool"], "rate_stretch", "status: {}", status(&v));
    assert_eq!(TimelineCommand::SetTool(TrimTool::RateStretch).spec_command(), Some("cmd.tools.05ratestretch"));
    assert_eq!(timeline::key_command(egui::Key::R, egui::Modifiers::NONE), Some(TimelineCommand::SetTool(TrimTool::RateStretch)));
    assert_eq!(timeline::key_command(egui::Key::R, egui::Modifiers::COMMAND), Some(TimelineCommand::SpeedDurationOpen));
    assert_eq!(TimelineCommand::SpeedDurationOpen.spec_command(), Some("cmd.clip.speed"));
}

/// Rate Stretch tool: a real drag of clip 1's tail +5 frames makes it 25 frames long at speed
/// 20/25 = 4/5 with the source window [10, 30) unchanged and clip 2 untouched (no ripple); its block
/// label shows 80.00%; one undo step, undo restores. A head drag of clip 2 by -4 frames: duration 34,
/// speed 30/34 = 15/17, end stays.
#[test]
fn rate_stretch_drag_changes_speed_not_source() {
    let (_s, mut v, ctx) = opened("drag");
    v.timeline_command(TimelineCommand::SetTool(TrimTool::RateStretch)).unwrap();
    settle(&mut v, &ctx);
    let before = seq(&v);
    let n = undo_len(&v);
    let b = block(&v, 1);
    let y = b.rect.center().y;
    drag(&mut v, &ctx, egui::pos2(b.rect.max.x - 2.0, y), egui::pos2(b.rect.max.x - 2.0 + 5.0 * PPF, y));
    assert_eq!(window(&v, 1), (0, 25, 10, 30), "status: {}", status(&v));
    assert_eq!(speed(&v, 1), (4, 5));
    assert_eq!(window(&v, 2), (30, 60, 100, 130), "no ripple");
    assert_eq!(undo_len(&v), n + 1, "one step per drag");
    assert!(last_undo(&v).starts_with("timeline_rate_stretch"), "{}", last_undo(&v));
    settle(&mut v, &ctx);
    assert!(block(&v, 1).label.ends_with("(80.00%)"), "{}", block(&v, 1).label);
    assert!(!block(&v, 2).label.contains('%'), "a 100 % clip shows no speed");
    v.shell_command(ShellCommand::Undo).unwrap();
    assert_eq!(seq(&v), before, "undo restores the exact sequence");
    settle(&mut v, &ctx);
    let b = block(&v, 2);
    drag(&mut v, &ctx, egui::pos2(b.rect.min.x + 2.0, y), egui::pos2(b.rect.min.x + 2.0 - 4.0 * PPF, y));
    assert_eq!(window(&v, 2), (26, 60, 100, 130), "status: {}", status(&v));
    assert_eq!(speed(&v, 2), (15, 17));
    let t = &v.state_json()["timeline"]["last_trim"];
    assert_eq!((t["kind"].as_str(), t["requested_frames"].as_i64(), t["achieved_frames"].as_i64()), (Some("rate_stretch"), Some(-4), Some(-4)), "{t}");
    println!("NOT_PROVEN: playback/render of a stretched clip's frames (no frame decoder on document sequences); audio at non-unit speed");
}

/// Ctrl+R with clip 3 selected opens the Speed/Duration form at its current values (100 %, 20
/// frames); 50 % applies speed 1/2 and duration 40 as one step and undo restores; a duration of 10
/// with Reverse gives speed -2; junk is refused by name; the two settings without clip storage are
/// shown unavailable.
#[test]
fn ctrl_r_speed_duration_sets_speed_and_undo_restores() {
    let (_s, mut v, ctx) = opened("speed");
    settle(&mut v, &ctx);
    v.timeline_command(TimelineCommand::Select(Some(cid(3)))).unwrap();
    let _ = frame(&mut v, &ctx, vec![key(egui::Key::R, egui::Modifiers::COMMAND)]);
    let form = v.state_json()["timeline"]["speed_form"].clone();
    assert_eq!((form["clip_id"].as_str(), form["percent"].as_str(), form["duration_frames"].as_str(), form["reverse"].as_bool()), (Some(cid(3).as_str()), Some("100.00"), Some("20"), Some(false)), "{form}");
    assert!(form["maintain_audio_pitch"].as_str().unwrap().starts_with("unavailable"));
    let _ = frame(&mut v, &ctx, vec![]);
    for name in [timeline::widget::SPEED_FORM, timeline::widget::SPEED_PERCENT, timeline::widget::SPEED_DURATION, timeline::widget::SPEED_REVERSE, timeline::widget::SPEED_PITCH, timeline::widget::SPEED_INTERPOLATION, timeline::widget::SPEED_APPLY] {
        assert!(v.drawn().iter().any(|d| d == name), "{name} not drawn");
    }
    let before = seq(&v);
    let n = undo_len(&v);
    let f = v.timeline_mut().and_then(|t| t.speed_form_mut()).unwrap();
    f.percent = "50".into();
    f.fixed = SpeedFix::Speed;
    v.timeline_command(TimelineCommand::SpeedDurationApply).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
    assert_eq!((window(&v, 3), speed(&v, 3)), ((70, 110, 10, 30), (1, 2)));
    assert!(v.state_json()["timeline"]["speed_form"].is_null(), "Apply closes the form");
    assert_eq!(undo_len(&v), n + 1);
    assert!(last_undo(&v).starts_with("timeline_speed_duration"));
    v.shell_command(ShellCommand::Undo).unwrap();
    assert_eq!(seq(&v), before);
    v.timeline_command(TimelineCommand::SpeedDurationOpen).unwrap();
    let f = v.timeline_mut().and_then(|t| t.speed_form_mut()).unwrap();
    f.duration_frames = "10".into();
    f.fixed = SpeedFix::Duration;
    f.reverse = true;
    v.timeline_command(TimelineCommand::SpeedDurationApply).unwrap();
    assert_eq!((window(&v, 3), speed(&v, 3)), ((70, 80, 10, 30), (-2, 1)));
    v.timeline_command(TimelineCommand::SpeedDurationOpen).unwrap();
    assert_eq!(v.state_json()["timeline"]["speed_form"]["percent"], "200.00");
    v.timeline_mut().and_then(|t| t.speed_form_mut()).unwrap().percent = "fast".into();
    assert_eq!(v.timeline_command(TimelineCommand::SpeedDurationApply).unwrap_err(), "invalid_speed_percent");
    v.timeline_command(TimelineCommand::SpeedDurationCancel).unwrap();
    assert_eq!(v.timeline_command(TimelineCommand::SpeedDurationApply).unwrap_err(), "no_speed_form");
    // The typed form: 300 % on clip 2 derives 10 frames.
    v.timeline_command(TimelineCommand::SpeedDuration { clip_id: cid(2), target: SpeedDurationTarget::Speed(Speed { numerator: 3, denominator: 1 }), reverse: false }).unwrap();
    assert_eq!((window(&v, 2), speed(&v, 2)), ((30, 40, 100, 130), (3, 1)));
    v.timeline_command(TimelineCommand::Select(None)).unwrap();
    assert_eq!(v.timeline_command(TimelineCommand::SpeedDurationOpen).unwrap_err(), "no_selection");
}

/// [STU-VID-090]: a tail stretch of clip 2 by +15 stops at clip 3 (+10, `rate_stretch_overlap`,
/// speed 30/40 = 3/4); -40 stops at one frame (-29, `rate_stretch_below_one_frame`, speed 30).
#[test]
fn achieved_delta_stops_at_the_neighbour() {
    let (_s, mut v, _ctx) = opened("achieved");
    let o = v.timeline_trim(TimelineCommand::RateStretch { clip_id: cid(2), edge: RateEdge::Tail, delta_frames: 15 }).unwrap();
    assert_eq!((o.requested_frames, o.achieved_frames, o.limit.as_deref()), (15, 10, Some("rate_stretch_overlap")), "{o:?}");
    assert!(o.planner_calls <= timeline::MAX_PLANNER_CALLS && o.refusal.is_none());
    assert_eq!((window(&v, 2), speed(&v, 2)), ((30, 70, 100, 130), (3, 4)));
    v.shell_command(ShellCommand::Undo).unwrap();
    let o = v.timeline_trim(TimelineCommand::RateStretch { clip_id: cid(2), edge: RateEdge::Tail, delta_frames: -40 }).unwrap();
    assert_eq!((o.achieved_frames, o.limit.as_deref()), (-29, Some("rate_stretch_below_one_frame")), "{o:?}");
    assert_eq!((window(&v, 2), speed(&v, 2)), ((30, 31, 100, 130), (30, 1)));
    v.shell_command(ShellCommand::Undo).unwrap();
    // A head stretch of clip 1 before tick 0 is refused (outside_range) and changes nothing.
    let base = seq(&v);
    assert_eq!(v.timeline_command(TimelineCommand::RateStretch { clip_id: cid(1), edge: RateEdge::Head, delta_frames: -3 }).unwrap_err(), "outside_range");
    assert_eq!(seq(&v), base);
}

// ---- slice 3: slip at non-unit speed -------------------------------------------------------------------

/// Clip 2 at 200 % (source [100, 160) over 30 frames): slip +3 frames moves its source window by 6
/// source frames, -2 by -4; a clip at 1/11 speed cannot slip one frame exactly
/// (`slip_inexact_at_speed`, nothing changes).
#[test]
fn slip_at_double_speed_moves_two_source_frames_per_frame() {
    let (_s, mut v, _ctx) = opened_with("slip-speed", |s| {
        let c = &mut s.tracks[0].clips[1];
        c.source_out = Ticks::new(160 * F);
        c.speed = Speed { numerator: 2, denominator: 1 };
        let c = &mut s.tracks[0].clips[2];
        c.source_out = Ticks::new(12 * F);
        c.duration = Ticks::new(22 * F);
        c.speed = Speed { numerator: 1, denominator: 11 };
        s.duration = Ticks::new(92 * F);
    });
    v.timeline_command(TimelineCommand::Slip { clip_id: cid(2), delta_frames: 3 }).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(window(&v, 2), (30, 60, 106, 166));
    assert_eq!(speed(&v, 2), (2, 1));
    v.shell_command(ShellCommand::Undo).unwrap();
    v.timeline_command(TimelineCommand::Slip { clip_id: cid(2), delta_frames: -2 }).unwrap();
    assert_eq!(window(&v, 2), (30, 60, 96, 156));
    let base = seq(&v);
    assert_eq!(v.timeline_command(TimelineCommand::Slip { clip_id: cid(3), delta_frames: 1 }).unwrap_err(), "slip_inexact_at_speed");
    assert_eq!(v.state_json()["timeline"]["last_trim"]["refusal"], "slip_inexact_at_speed");
    assert_eq!(seq(&v), base);
}
