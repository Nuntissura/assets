//! Lane SMB-TIMELINE-TRIM-1 (feature `viewer`): ripple / roll / slip / slide trims, nudges, extend
//! edit and the trim session of the timeline panel, driven headlessly (one-frame egui draws into a
//! bare context, no window). Oracles: hand-computed frames of a three-clip abutting chain on V1
//! plus one sync-locked A1 clip, the run's document (the Folio sequence after each edit), the
//! session history (one undo step per trim) and EDIT > Undo restoring the exact prior sequence.
//!
//! NOT_PROVEN (printed): outgoing / incoming frame pixels and the composite preview picture (no
//! frame decoder bound on a document sequence); a real desktop window; Premiere cursor zones,
//! default multi-frame step and ripple / roll toggle feel (no Premiere oracle).
#![cfg(feature = "viewer")]

mod support_context;

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::shell::{Panel, ShellCommand};
use hsk_studio_harness_view::view::timeline::{self, DEFAULT_PX_PER_FRAME, EditMode, MAX_PLANNER_CALLS, TimelineCommand, TrimFocus, TrimTool};
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
        let dir = base.join(format!("hsk-harness-view-trim-{}-{label}", std::process::id()));
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

/// V1 (abutting chain): clip 1 `a` source [10, 30) at frames [0, 20), clip 2 `b` source
/// [100, 130) at [20, 50), clip 3 `c` source [10, 30) at [50, 70). A1 (sync-locked, targeted):
/// clip 4 `d` source [0, 20) at [60, 80).
fn sequence() -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-trim".into(),
        name: "trim".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(80 * F),
        tracks: vec![
            track(1, TrackKind::Video, "V1", vec![clip(1, "a", 10, 30, 0), clip(2, "b", 100, 130, 20), clip(3, "c", 10, 30, 50)]),
            track(2, TrackKind::Audio, "A1", vec![clip(4, "d", 0, 20, 60)]),
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
    let path = dir.join("trim.handshake");
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

/// The viewer with the fixture open and the timeline panel shown; `extents` binds 1000-frame
/// sources (else the viewer default: unknown source lengths).
fn opened_with(label: &str, extents: bool, edit: impl FnOnce(&mut StudioSequence)) -> (Scratch, Viewer, egui::Context) {
    let scratch = Scratch::new(label);
    let mut seq = sequence();
    edit(&mut seq);
    let mut v = crate::support_context::viewer(Prefer::Cpu);
    let ctx = egui::Context::default();
    v.open(&sequence_package(&scratch.0, seq)).unwrap_or_else(|e| panic!("{e}"));
    v.shell_command(ShellCommand::Panel(Panel::Timeline)).unwrap();
    if extents {
        v.set_monitor_sources(Box::new(NoFrames), Box::new(Long));
    }
    (scratch, v, ctx)
}

fn opened(label: &str) -> (Scratch, Viewer, egui::Context) {
    opened_with(label, true, |_| {})
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

/// (timeline start, end, source in, source out) of clip `n` in frames.
fn window(v: &Viewer, n: u32) -> (u64, u64, u64, u64) {
    let s = seq(v);
    let c = s.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.clip_id == cid(n)).unwrap_or_else(|| panic!("clip {n} missing"));
    (c.timeline_start.value() / F, c.timeline_end().unwrap().value() / F, c.source_in.value() / F, c.source_out.value() / F)
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

fn trim_json(v: &Viewer) -> Value {
    v.state_json()["timeline"]["last_trim"].clone()
}

fn session_json(v: &Viewer) -> Value {
    v.state_json()["timeline"]["trim_session"].clone()
}

/// Commits `c`, asserts one new undo step named `op`, then undoes it and asserts the exact prior
/// sequence is back.
fn one_step_then_undo(v: &mut Viewer, c: TimelineCommand, op: &str, check: impl FnOnce(&Viewer)) {
    let before = seq(v);
    let n = undo_len(v);
    v.timeline_command(c).unwrap_or_else(|e| panic!("{e}: {}", status(v)));
    assert_eq!(undo_len(v), n + 1, "one history step");
    assert!(last_undo(v).starts_with(op), "{} vs {op}", last_undo(v));
    check(v);
    v.shell_command(ShellCommand::Undo).unwrap();
    assert_eq!(seq(v), before, "undo restores the exact sequence");
}

// ---- slice 1: typed trims with achieved delta -------------------------------------------------------

/// Ripple trim out +5 on clip 1 lengthens it and shifts every later clip on V1 and the
/// sync-locked A1 by 5 (no gap opens); ripple trim in +4 on clip 2 keeps its start, consumes 4
/// source frames and shifts later content back by 4. Each is one undo step.
#[test]
fn ripple_trim_shifts_later_clips_on_sync_locked_tracks() {
    let (_s, mut v, _ctx) = opened("ripple");
    one_step_then_undo(&mut v, TimelineCommand::RippleTrimOut { clip_id: cid(1), delta_frames: 5 }, "timeline_ripple_trim_out", |v| {
        assert_eq!(window(v, 1), (0, 25, 10, 35));
        assert_eq!(window(v, 2), (25, 55, 100, 130));
        assert_eq!(window(v, 3), (55, 75, 10, 30));
        assert_eq!(window(v, 4), (65, 85, 0, 20), "the sync-locked A1 clip shifts too");
        assert_eq!(seq(v).duration, Ticks::new(85 * F));
        let t = trim_json(v);
        assert_eq!((t["requested_frames"].as_i64(), t["achieved_frames"].as_i64(), t["refusal"].is_null()), (Some(5), Some(5), true), "{t}");
    });
    one_step_then_undo(&mut v, TimelineCommand::RippleTrimIn { clip_id: cid(2), delta_frames: 4 }, "timeline_ripple_trim_in", |v| {
        assert_eq!(window(v, 1), (0, 20, 10, 30));
        assert_eq!(window(v, 2), (20, 46, 104, 130), "keeps its start, head consumed");
        assert_eq!(window(v, 3), (46, 66, 10, 30));
        assert_eq!(window(v, 4), (56, 76, 0, 20));
    });
}

/// Roll +5 on the clip 1 | clip 2 edit: clip 1 lengthens by exactly what clip 2 shortens, the
/// sequence duration and every other clip stay.
#[test]
fn roll_moves_the_edit_between_neighbours() {
    let (_s, mut v, _ctx) = opened("roll");
    let duration = seq(&v).duration;
    one_step_then_undo(&mut v, TimelineCommand::Roll { clip_id: cid(1), delta_frames: 5 }, "timeline_roll", |v| {
        assert_eq!(window(v, 1), (0, 25, 10, 35));
        assert_eq!(window(v, 2), (25, 50, 105, 130));
        assert_eq!((window(v, 3), window(v, 4)), ((50, 70, 10, 30), (60, 80, 0, 20)));
        assert_eq!(seq(v).duration, duration);
    });
    one_step_then_undo(&mut v, TimelineCommand::Roll { clip_id: cid(2), delta_frames: -6 }, "timeline_roll", |v| {
        assert_eq!(window(v, 2), (20, 44, 100, 124));
        assert_eq!(window(v, 3), (44, 70, 4, 30), "the incoming head extends into its handle");
        assert_eq!(seq(v).duration, Ticks::new(80 * F));
    });
}

/// Slip +7 / -7 on clip 2 changes only its source window; its timeline start and duration and
/// every neighbour stay.
#[test]
fn slip_changes_source_window_only() {
    let (_s, mut v, _ctx) = opened("slip");
    for (delta, (si, so)) in [(7i64, (107u64, 137u64)), (-7, (93, 123))] {
        one_step_then_undo(&mut v, TimelineCommand::Slip { clip_id: cid(2), delta_frames: delta }, "timeline_slip", |v| {
            assert_eq!(window(v, 2), (20, 50, si, so));
            assert_eq!((window(v, 1), window(v, 3)), ((0, 20, 10, 30), (50, 70, 10, 30)));
        });
    }
}

/// Slide +4 on clip 2: it keeps its source window and length, clip 1 lengthens by 4, clip 3
/// shortens by 4 (its head moves 4 source frames on).
#[test]
fn slide_keeps_source_and_resizes_neighbours() {
    let (_s, mut v, _ctx) = opened("slide");
    one_step_then_undo(&mut v, TimelineCommand::Slide { clip_id: cid(2), delta_frames: 4 }, "timeline_slide", |v| {
        assert_eq!(window(v, 1), (0, 24, 10, 34));
        assert_eq!(window(v, 2), (24, 54, 100, 130));
        assert_eq!(window(v, 3), (54, 70, 14, 30));
    });
}

/// [STU-VID-090]: the achieved delta is the largest admitted toward zero (slip -15 on a clip with
/// 10 source frames of head handle achieves -10; a ripple +2000 against 1000-frame sources
/// achieves 970) within the planner-call bound; refusals are named and change nothing (unknown
/// source length, no abutting neighbour, a locked sync-locked track).
#[test]
fn achieved_delta_is_clamped_and_refusals_are_named() {
    let (_s, mut v, _ctx) = opened("achieved");
    let o = v.timeline_trim(TimelineCommand::Slip { clip_id: cid(1), delta_frames: -15 }).unwrap();
    assert_eq!((o.requested_frames, o.achieved_frames, o.limit.as_deref()), (-15, -10, Some("past_source_start")));
    assert!(o.planner_calls <= MAX_PLANNER_CALLS && o.refusal.is_none(), "{o:?}");
    assert_eq!(window(&v, 1), (0, 20, 0, 20));
    assert_eq!(trim_json(&v)["achieved_frames"], -10);
    v.shell_command(ShellCommand::Undo).unwrap();
    let o = v.timeline_trim(TimelineCommand::RippleTrimOut { clip_id: cid(1), delta_frames: 2000 }).unwrap();
    assert_eq!((o.achieved_frames, o.limit.as_deref()), (970, Some("past_source_end")), "{o:?}");
    assert!(o.planner_calls <= MAX_PLANNER_CALLS);
    assert_eq!(window(&v, 1), (0, 990, 10, 1000));
    v.shell_command(ShellCommand::Undo).unwrap();
    // Roll -25 on a 20-frame outgoing clip keeps one frame (achieved -19).
    let o = v.timeline_trim(TimelineCommand::Roll { clip_id: cid(1), delta_frames: -25 }).unwrap();
    assert_eq!(o.achieved_frames, -19, "{o:?}");
    v.shell_command(ShellCommand::Undo).unwrap();
    let base = seq(&v);
    let steps = undo_len(&v);
    // No abutting neighbour: roll on the last clip, slide on the first.
    for (c, code) in [(TimelineCommand::Roll { clip_id: cid(3), delta_frames: 2 }, "not_adjacent"), (TimelineCommand::Slide { clip_id: cid(1), delta_frames: 2 }, "not_adjacent")] {
        let e = v.timeline_command(c).unwrap_err();
        assert_eq!(e, code);
        assert_eq!((trim_json(&v)["refusal"].as_str(), trim_json(&v)["achieved_frames"].as_i64()), (Some(code), Some(0)));
    }
    assert!(v.timeline_command(TimelineCommand::Slip { clip_id: cid(9), delta_frames: 1 }).unwrap_err().contains("unknown_clip"));
    assert_eq!((seq(&v), undo_len(&v)), (base.clone(), steps), "refusals change nothing");
    // Unknown source extents (viewer default): a slip later or a longer tail is refused, not guessed.
    let (_s2, mut u, _c2) = opened_with("unknown", false, |_| {});
    assert_eq!(u.timeline_command(TimelineCommand::Slip { clip_id: cid(2), delta_frames: 3 }).unwrap_err(), "source_extent_unknown");
    assert_eq!(trim_json(&u)["refusal"], "source_extent_unknown");
    assert_eq!(window(&u, 2), (20, 50, 100, 130));
    // A locked sync-locked track blocks a ripple that would shift it.
    let (_s3, mut w, _c3) = opened_with("locked", true, |s| s.tracks[1].locked = true);
    assert_eq!(w.timeline_command(TimelineCommand::RippleTrimOut { clip_id: cid(1), delta_frames: 3 }).unwrap_err(), "sync_lock_blocked");
    assert_eq!(window(&w, 2), (20, 50, 100, 130));
}

// ---- slice 2: drag trim modes in the panel ----------------------------------------------------------

/// Ripple tool: a real drag of clip 1's tail +5 frames ripples (clip 2, 3 and A1 move with it, no
/// gap), one undo step, and EDIT > Undo restores the exact sequence.
#[test]
fn ripple_drag_closes_no_gap_and_undo_restores() {
    let (_s, mut v, ctx) = opened("ripple-drag");
    settle(&mut v, &ctx);
    assert!(v.drawn().iter().any(|d| d == &timeline::widget::tool("ripple")), "trim mode selector drawn");
    v.timeline_command(TimelineCommand::SetTool(TrimTool::Ripple)).unwrap();
    assert_eq!(v.state_json()["timeline"]["tool"], "ripple");
    settle(&mut v, &ctx);
    let before = seq(&v);
    let n = undo_len(&v);
    let b = block(&v, 1);
    let y = b.rect.center().y;
    drag(&mut v, &ctx, egui::pos2(b.rect.max.x - 2.0, y), egui::pos2(b.rect.max.x - 2.0 + 5.0 * PPF, y));
    assert_eq!(window(&v, 1), (0, 25, 10, 35), "status: {}", status(&v));
    assert_eq!((window(&v, 2), window(&v, 3), window(&v, 4)), ((25, 55, 100, 130), (55, 75, 10, 30), (65, 85, 0, 20)));
    assert_eq!(undo_len(&v), n + 1, "one step per drag");
    assert!(last_undo(&v).starts_with("timeline_ripple_trim_out"));
    v.shell_command(ShellCommand::Undo).unwrap();
    assert_eq!(seq(&v), before);
}

/// Roll tool: dragging clip 2's head -3 frames rolls the clip 1 | clip 2 edit (clip 1 shortens,
/// clip 2 lengthens into its head handle), one step.
#[test]
fn roll_drag_on_a_shared_edit() {
    let (_s, mut v, ctx) = opened("roll-drag");
    v.timeline_command(TimelineCommand::SetTool(TrimTool::Roll)).unwrap();
    settle(&mut v, &ctx);
    let n = undo_len(&v);
    let b = block(&v, 2);
    let y = b.rect.center().y;
    drag(&mut v, &ctx, egui::pos2(b.rect.min.x + 2.0, y), egui::pos2(b.rect.min.x + 2.0 - 3.0 * PPF, y));
    assert_eq!(window(&v, 1), (0, 17, 10, 27), "status: {}", status(&v));
    assert_eq!(window(&v, 2), (17, 50, 97, 130));
    assert_eq!(window(&v, 3), (50, 70, 10, 30));
    assert_eq!(undo_len(&v), n + 1);
    assert!(last_undo(&v).starts_with("timeline_roll"));
}

/// Slip tool: dragging clip 2's body +6 frames moves its source window, not its position.
#[test]
fn slip_drag_changes_frames_not_position() {
    let (_s, mut v, ctx) = opened("slip-drag");
    v.timeline_command(TimelineCommand::SetTool(TrimTool::Slip)).unwrap();
    settle(&mut v, &ctx);
    let b = block(&v, 2);
    let from = egui::pos2(b.rect.center().x, b.rect.center().y);
    drag(&mut v, &ctx, from, egui::pos2(from.x + 6.0 * PPF, from.y));
    assert_eq!(window(&v, 2), (20, 50, 106, 136), "status: {}", status(&v));
    assert_eq!((window(&v, 1), window(&v, 3)), ((0, 20, 10, 30), (50, 70, 10, 30)));
    assert!(last_undo(&v).starts_with("timeline_slip"));
    // Slide tool on the same body: +3 frames between the neighbours.
    v.timeline_command(TimelineCommand::SetTool(TrimTool::Slide)).unwrap();
    settle(&mut v, &ctx);
    let b = block(&v, 2);
    let from = egui::pos2(b.rect.center().x, b.rect.center().y);
    drag(&mut v, &ctx, from, egui::pos2(from.x + 3.0 * PPF, from.y));
    assert_eq!(window(&v, 2), (23, 53, 106, 136), "status: {}", status(&v));
    assert_eq!((window(&v, 1), window(&v, 3)), ((0, 23, 10, 33), (53, 70, 13, 30)));
}

// ---- slice 3: nudges, bindings, extend edit, multi-frame step --------------------------------------

/// [STU-VID-041] bindings with the panel active and clip 2 selected: Ctrl+Alt+Right slips one
/// frame, Ctrl+Alt+Shift+Left slips back the multi-frame step (5), Alt+. slides one frame, Alt+Shift+,
/// slides back 5; every nudge is one step and maps to its spec command id.
#[test]
fn spec_bindings_nudge_slip_and_slide() {
    let (_s, mut v, ctx) = opened("bindings");
    settle(&mut v, &ctx);
    v.timeline_command(TimelineCommand::Select(Some(cid(2)))).unwrap();
    let m = |ctrl: bool, shift: bool| egui::Modifiers { alt: true, ctrl, command: ctrl, shift, ..Default::default() };
    let n = undo_len(&v);
    let _ = frame(&mut v, &ctx, vec![key(egui::Key::ArrowRight, m(true, false))]);
    assert_eq!(window(&v, 2), (20, 50, 101, 131), "status: {}", status(&v));
    let _ = frame(&mut v, &ctx, vec![key(egui::Key::ArrowLeft, m(true, true))]);
    assert_eq!(window(&v, 2), (20, 50, 96, 126), "status: {}", status(&v));
    let _ = frame(&mut v, &ctx, vec![key(egui::Key::Period, m(false, false))]);
    assert_eq!(window(&v, 2), (21, 51, 96, 126), "status: {}", status(&v));
    let _ = frame(&mut v, &ctx, vec![key(egui::Key::Comma, m(false, true))]);
    assert_eq!(window(&v, 2), (16, 46, 96, 126), "status: {}", status(&v));
    assert_eq!((window(&v, 1), window(&v, 3)), ((0, 16, 10, 26), (46, 70, 6, 30)));
    assert_eq!(undo_len(&v), n + 4, "one step per nudge");
    let ids: Vec<Option<&str>> = [(false, false), (false, true), (true, false), (true, true)]
        .iter()
        .flat_map(|&(right, many)| [TimelineCommand::SlipNudge { right, many }.spec_command(), TimelineCommand::SlideNudge { right, many }.spec_command()])
        .collect();
    assert_eq!(
        ids,
        [
            "cmd.timeline.slip.left.one",
            "cmd.timeline.slide.left.one",
            "cmd.timeline.slip.left.several",
            "cmd.timeline.slide.left.several",
            "cmd.timeline.slip.right.one",
            "cmd.timeline.slide.right.one",
            "cmd.timeline.slip.right.several",
            "cmd.timeline.slide.right.several"
        ]
        .map(Some)
    );
    // Nothing selected: a nudge is refused by name.
    v.timeline_command(TimelineCommand::Select(None)).unwrap();
    assert_eq!(v.timeline_command(TimelineCommand::SlipNudge { right: true, many: false }).unwrap_err(), "no_selection");
}

/// Extend edit to playhead rolls the nearest edit on the targeted tracks (clip 1 | clip 2 at 20,
/// not clip 2 | clip 3 at 50) to the playhead at frame 23; a selected roll edit wins over nearest.
#[test]
fn extend_edit_to_playhead_rolls_the_nearest_edit() {
    let (_s, mut v, ctx) = opened("extend");
    settle(&mut v, &ctx);
    v.timeline_command(TimelineCommand::Scrub(Ticks::new(23 * F))).unwrap();
    one_step_then_undo(&mut v, TimelineCommand::ExtendEditToPlayhead, "timeline_extend_edit", |v| {
        assert_eq!(window(v, 1), (0, 23, 10, 33));
        assert_eq!(window(v, 2), (23, 50, 103, 130));
    });
    // Playhead at 47 is nearest the clip 2 | clip 3 edit (50), but the selected clip 1 | clip 2
    // roll edit wins: it rolls +27 to 47.
    v.timeline_command(TimelineCommand::Scrub(Ticks::new(47 * F))).unwrap();
    v.timeline_command(TimelineCommand::EditPointAt { clip_id: cid(1), mode: EditMode::Roll }).unwrap();
    let o = v.timeline_trim(TimelineCommand::ExtendEditToPlayhead).unwrap();
    assert_eq!((o.clip_id.as_str(), o.requested_frames, o.achieved_frames), (cid(1).as_str(), 27, 27), "{o:?}");
    assert_eq!((window(&v, 1), window(&v, 2)), ((0, 47, 10, 57), (47, 50, 127, 130)));
    v.shell_command(ShellCommand::Undo).unwrap();
    // Without a selected roll edit the nearest (50) rolls back to 47.
    v.timeline_command(TimelineCommand::EditPointAt { clip_id: cid(1), mode: EditMode::TrimIn }).unwrap();
    v.timeline_command(TimelineCommand::ExtendEditToPlayhead).unwrap();
    assert_eq!((window(&v, 2), window(&v, 3)), ((20, 47, 100, 127), (47, 70, 7, 30)));
}

/// One multi-frame step (set to 3) governs slip, slide, trim and clip "several" nudges alike; an
/// out-of-range step is refused.
#[test]
fn one_multi_frame_step_governs_every_several_variant() {
    let (_s, mut v, _ctx) = opened("step");
    assert_eq!(v.state_json()["timeline"]["multi_frame_step"], timeline::DEFAULT_MULTI_FRAME_STEP);
    v.timeline_command(TimelineCommand::SetMultiFrameStep(3)).unwrap();
    assert_eq!(v.timeline_command(TimelineCommand::SetMultiFrameStep(0)).unwrap_err(), "step_out_of_range");
    v.timeline_command(TimelineCommand::Select(Some(cid(2)))).unwrap();
    v.timeline_command(TimelineCommand::SlipNudge { right: true, many: true }).unwrap();
    assert_eq!(window(&v, 2), (20, 50, 103, 133));
    v.timeline_command(TimelineCommand::SlideNudge { right: true, many: true }).unwrap();
    assert_eq!(window(&v, 2), (23, 53, 103, 133));
    // Trim forward many on the ripple-out edit point of clip 1 (the edit selected nearest frame 22).
    v.timeline_command(TimelineCommand::SelectEdit { mode: EditMode::RippleOut, nearest_to: Ticks::new(22 * F) }).unwrap();
    assert_eq!(v.state_json()["timeline"]["edit_point"]["clip_id"], cid(1).as_str());
    assert_eq!(TimelineCommand::SelectEdit { mode: EditMode::RippleOut, nearest_to: Ticks::new(0) }.spec_command(), Some("cmd.timeline.editpoint.rippletrimout"));
    v.timeline_command(TimelineCommand::TrimNudge { forward: true, many: true }).unwrap();
    assert_eq!(window(&v, 1), (0, 26, 10, 36));
    assert_eq!(window(&v, 2), (26, 56, 103, 133), "rippled by the same 3");
    // Clip 3 nudged right by 3 (an overwrite move into the empty space after it).
    v.timeline_command(TimelineCommand::Select(Some(cid(3)))).unwrap();
    v.timeline_command(TimelineCommand::ClipNudge { right: true, many: true }).unwrap();
    assert_eq!(window(&v, 3), (59, 76, 13, 30), "status: {}", status(&v));
}

// ---- slice 4: trim session ---------------------------------------------------------------------------

/// Enter a session on the clip 1 | clip 2 roll edit, plus minor twice and plus major once (5):
/// the pending adjustment is 7 and the tiles show the outgoing source out 37 and incoming source in
/// 107 while the document is untouched; Apply commits ONE step.
#[test]
fn trim_session_adjusts_and_commits_one_step() {
    let (_s, mut v, ctx) = opened("session");
    settle(&mut v, &ctx);
    v.timeline_command(TimelineCommand::SelectEdit { mode: EditMode::Roll, nearest_to: Ticks::new(21 * F) }).unwrap();
    v.timeline_command(TimelineCommand::TrimSessionEnter).unwrap();
    let base = seq(&v);
    let n = undo_len(&v);
    for (forward, many) in [(true, false), (true, false), (true, true)] {
        v.timeline_command(TimelineCommand::TrimNudge { forward, many }).unwrap_or_else(|e| panic!("{e}"));
    }
    let s = session_json(&v);
    assert_eq!((s["type"].as_str(), s["focus"].as_str(), s["pending_frames"].as_i64()), (Some("roll"), Some("both"), Some(7)), "{s}");
    assert_eq!((s["outgoing_source_out"].as_u64(), s["incoming_source_in"].as_u64()), (Some(37 * F), Some(107 * F)));
    assert_eq!((seq(&v), undo_len(&v)), (base, n), "nothing committed while the session is open");
    let _ = frame(&mut v, &ctx, vec![]);
    for name in [timeline::widget::TRIM_SESSION, timeline::widget::TRIM_OUTGOING, timeline::widget::TRIM_INCOMING, timeline::widget::TRIM_PLUS_MAJOR, timeline::widget::TRIM_COMMIT] {
        assert!(v.drawn().iter().any(|d| d == name), "{name} not drawn");
    }
    println!("NOT_PROVEN: trim session outgoing/incoming frame pixels and composite preview picture (no frame decoder bound on a document sequence)");
    v.timeline_command(TimelineCommand::TrimSessionCommit).unwrap();
    assert_eq!(undo_len(&v), n + 1, "one step for the whole session");
    assert!(last_undo(&v).starts_with("timeline_trim_session_commit"));
    assert_eq!((window(&v, 1), window(&v, 2)), ((0, 27, 10, 37), (27, 50, 107, 130)));
    assert!(session_json(&v).is_null());
}

/// Rollback abandons the session: no history step, the document is the one before the session.
#[test]
fn rollback_abandons_the_session() {
    let (_s, mut v, _ctx) = opened("rollback");
    assert_eq!(v.timeline_command(TimelineCommand::TrimSessionEnter).unwrap_err(), "no_edit_point");
    v.timeline_command(TimelineCommand::EditPointAt { clip_id: cid(1), mode: EditMode::RippleOut }).unwrap();
    v.timeline_command(TimelineCommand::TrimSessionEnter).unwrap();
    let base = seq(&v);
    let n = undo_len(&v);
    v.timeline_command(TimelineCommand::TrimNudge { forward: true, many: true }).unwrap();
    assert_eq!(session_json(&v)["pending_frames"], 5);
    v.timeline_command(TimelineCommand::TrimSessionRollback).unwrap();
    assert!(session_json(&v).is_null());
    assert_eq!((seq(&v), undo_len(&v)), (base, n));
    assert_eq!(v.timeline_command(TimelineCommand::TrimSessionRollback).unwrap_err(), "no_trim_session");
}

/// Focus in / out / both and the ripple <-> roll toggle are typed commands with their spec ids;
/// focus Out ripples the outgoing tail, the toggle makes it a roll, focus In ripples the incoming
/// head; a minus past the outgoing clip's length clamps with the limit named.
#[test]
fn focus_and_type_toggle_are_typed_commands() {
    let (_s, mut v, _ctx) = opened("focus");
    v.timeline_command(TimelineCommand::EditPointAt { clip_id: cid(1), mode: EditMode::Roll }).unwrap();
    v.timeline_command(TimelineCommand::TrimSessionEnter).unwrap();
    assert_eq!(session_json(&v)["focus"], "both");
    v.timeline_command(TimelineCommand::TrimSessionFocus(TrimFocus::Out)).unwrap();
    assert_eq!((session_json(&v)["type"].as_str(), session_json(&v)["focus"].as_str()), (Some("ripple"), Some("out")));
    v.timeline_command(TimelineCommand::TrimNudge { forward: true, many: false }).unwrap();
    v.timeline_command(TimelineCommand::TrimNudge { forward: true, many: false }).unwrap();
    assert_eq!(session_json(&v)["outgoing_source_out"], 32 * F);
    v.timeline_command(TimelineCommand::TrimSessionToggleType).unwrap();
    let s = session_json(&v);
    assert_eq!((s["type"].as_str(), s["focus"].as_str(), s["pending_frames"].as_i64()), (Some("roll"), Some("both"), Some(0)), "a type change starts over");
    v.timeline_command(TimelineCommand::TrimSessionFocus(TrimFocus::In)).unwrap();
    v.timeline_command(TimelineCommand::TrimNudge { forward: true, many: true }).unwrap();
    assert_eq!(session_json(&v)["incoming_source_in"], 105 * F);
    v.timeline_command(TimelineCommand::ToggleCompositePreview).unwrap();
    assert_eq!(v.state_json()["timeline"]["composite_preview"], true);
    let n = undo_len(&v);
    v.timeline_command(TimelineCommand::TrimSessionCommit).unwrap();
    assert_eq!(undo_len(&v), n + 1);
    assert_eq!(window(&v, 2), (20, 45, 105, 130), "ripple in on the incoming clip");
    assert_eq!(window(&v, 3), (45, 65, 10, 30));
    // Clamp: a session on clip 1's ripple-out with minus far past its 20 frames.
    v.timeline_command(TimelineCommand::EditPointAt { clip_id: cid(1), mode: EditMode::RippleOut }).unwrap();
    v.timeline_command(TimelineCommand::TrimSessionEnter).unwrap();
    v.timeline_command(TimelineCommand::SetMultiFrameStep(1000)).unwrap();
    v.timeline_command(TimelineCommand::TrimNudge { forward: false, many: true }).unwrap();
    let s = session_json(&v);
    assert_eq!((s["pending_frames"].as_i64(), s["requested_frames"].as_i64(), s["limit"].as_str()), (Some(-19), Some(-1000), Some("zero_duration")), "{s}");
    // Spec ids.
    let ids: Vec<Option<&str>> =
        [TimelineCommand::TrimSessionFocus(TrimFocus::In), TimelineCommand::TrimSessionFocus(TrimFocus::Out), TimelineCommand::TrimSessionFocus(TrimFocus::Both), TimelineCommand::ToggleCompositePreview, TimelineCommand::TrimNudge { forward: true, many: false }, TimelineCommand::TrimNudge { forward: false, many: true }]
            .iter()
            .map(TimelineCommand::spec_command)
            .collect();
    assert_eq!(ids, ["cmd.trim.focusin", "cmd.trim.focusout", "cmd.trim.focusinandout", "cmd.timeline.composite.preview.during.trim", "cmd.trim.plus.minor", "cmd.trim.minus.major"].map(Some));
    println!("NOT_PROVEN: Premiere trim cursor zones, default multi-frame step and ripple/roll toggle feel (no Premiere oracle; UNVERIFIED)");
}
