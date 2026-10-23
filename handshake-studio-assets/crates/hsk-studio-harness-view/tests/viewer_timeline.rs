//! Lane SMB-TIMELINE-UI-1 (feature `viewer`): the timeline panel driven headlessly (one-frame egui
//! draws into a bare context, no window, CX-SAFE-002). The panel opens from WORKSPACE > Panels and
//! from F10 (with feature `desktop` also by an AccessKit click), clip blocks sit at the pixels of
//! their in and out, a real pointer drag on a block end trims the clip through pulse as one
//! undoable step, a drag near an edit point snaps onto it, razor / lift / ripple delete / markers
//! run through pulse, and hostile sequences draw bounded. Oracles: hand-computed ticks and pixels,
//! the run's document (Folio sequence after each edit), the session history and the program
//! monitor's sequence and playhead.
#![cfg(feature = "viewer")]

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::shell::{Panel, ShellCommand};
use hsk_studio_harness_view::view::timeline::{
    self, DEFAULT_PX_PER_FRAME, Drag, DragKind, HEADER_PX, MAX_DRAWN_CLIPS, MAX_DRAWN_TRACKS, RULER_PX, TRACK_PX, TimelineCommand, TimelineView, WORK_AREA_PX, drag_target, layout,
};
use hsk_studio_pulse::snap_targets;
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
        let dir = base.join(format!("hsk-harness-view-timeline-{}-{label}", std::process::id()));
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

/// V1: clip 1 `tone-a` source [0, 20) at frames [10, 30), clip 2 `tone-b` source [100, 150) at
/// [40, 90). A1: clip 3 `tone-c` source [0, 50) at [0, 50). One sequence marker at frame 70.
fn sequence() -> StudioSequence {
    let marker = StudioMarker { marker_id: "mk-seq".into(), name: "cue".into(), comment: String::new(), color: LabelColor::Rose, start: Ticks::new(70 * F), duration: Ticks::new(0), role: None };
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(90 * F),
        tracks: vec![
            track(1, TrackKind::Video, "V1", vec![clip(1, "tone-a", 0, 20, 10), clip(2, "tone-b", 100, 150, 40)]),
            track(2, TrackKind::Audio, "A1", vec![clip(3, "tone-c", 0, 50, 0)]),
        ],
        markers: vec![marker],
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

/// A native package whose document holds [`sequence`].
fn sequence_package(dir: &Path) -> PathBuf {
    let doc = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000003","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
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
    let path = dir.join("timeline.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

// ---- egui driving ----------------------------------------------------------------------------------

/// A 1400 x 1200 screen: at 900 px the viewer's top bars leave the bottom panels (status, timeline,
/// program monitor) less than the timeline's minimum height (measured: the panel reached y 965).
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

/// A real pointer drag from `from` to `to` over four UI frames (hover, press, move, release).
fn drag(v: &mut Viewer, ctx: &egui::Context, from: egui::Pos2, to: egui::Pos2) {
    let _ = frame(v, ctx, vec![egui::Event::PointerMoved(from)]);
    let _ = frame(v, ctx, vec![button(from, true)]);
    let _ = frame(v, ctx, vec![egui::Event::PointerMoved(to)]);
    let _ = frame(v, ctx, vec![egui::Event::PointerMoved(to), button(to, false)]);
    let _ = frame(v, ctx, vec![]);
}

/// Runs frames until the timeline layout is the same in two frames in a row (at most 6). The first
/// frame of a fresh context sizes the side, status and timeline panels differently from the
/// following frames (measured: the panel moved from y 850 to 885 and clip 1 from y 903 to 938), so
/// a pointer aimed at the first frame's block misses it; a real user never drags in frame one.
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

fn drew(v: &Viewer, name: &str) -> bool {
    v.drawn().iter().any(|d| d == name)
}

fn seq(v: &Viewer) -> StudioSequence {
    v.timeline_sequence().expect("timeline sequence").clone()
}

fn window(v: &Viewer, n: u32) -> (u64, u64, u64, u64) {
    let s = seq(v);
    let c = s.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.clip_id == cid(n)).unwrap_or_else(|| panic!("clip {n} missing"));
    (c.timeline_start.value() / F, c.timeline_end().unwrap().value() / F, c.source_in.value() / F, c.source_out.value() / F)
}

fn history(v: &Viewer) -> Value {
    v.run().history()
}

fn opened(label: &str) -> (Scratch, Viewer, egui::Context) {
    let scratch = Scratch::new(label);
    let mut v = Viewer::new(Prefer::Cpu);
    let ctx = egui::Context::default();
    v.open(&sequence_package(&scratch.0)).unwrap_or_else(|e| panic!("{e}"));
    (scratch, v, ctx)
}

fn block(v: &Viewer, n: u32) -> timeline::ClipBlock {
    v.timeline().and_then(|t| t.layout()).and_then(|l| l.clip(&cid(n))).cloned().unwrap_or_else(|| panic!("clip {n} not drawn"))
}

#[cfg(feature = "desktop")]
fn ak_find(out: &egui::FullOutput, pred: impl Fn(&str) -> bool) -> Option<egui::accesskit::NodeId> {
    let tree = out.platform_output.accesskit_update.as_ref()?;
    tree.nodes.iter().find(|(_, n)| n.label().is_some_and(&pred)).map(|(id, _)| *id)
}

#[cfg(feature = "desktop")]
fn ak_click(id: egui::accesskit::NodeId) -> egui::Event {
    egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest { action: egui::accesskit::Action::Click, target: id, data: None })
}

// ---- tests -----------------------------------------------------------------------------------------

/// WORKSPACE > Panels > Timeline and F10 open the panel on the document's sequence (opening the
/// program monitor: one clock); F10 hides and shows it; without a sequence the entry is refused.
#[test]
fn timeline_opens_from_the_workspace_menu_and_f10() {
    let mut empty = Viewer::new(Prefer::Cpu);
    assert_eq!(empty.state_json()["shell"]["panels"]["timeline"]["availability"], "unavailable");
    assert!(empty.toggle_panel(Panel::Timeline).unwrap_err().contains("no sequence"));

    let (_s, mut v, ctx) = opened("open");
    let entry = &v.state_json()["shell"]["panels"]["timeline"];
    assert_eq!((entry["label"].as_str(), entry["shortcut"].as_str(), entry["availability"].as_str()), (Some("Timeline"), Some("F10"), Some("available")));
    assert_eq!(entry["command"], "studio.workspace.panel.timeline");
    assert!(v.monitor().is_none());
    let _ = frame(&mut v, &ctx, vec![key(egui::Key::F10, egui::Modifiers::NONE)]);
    let _ = frame(&mut v, &ctx, vec![]);
    let s = v.state_json();
    assert_eq!(s["shell"]["panels"]["timeline"]["visible"], true, "{}", s["status"]);
    assert_eq!(s["timeline"]["sequence_id"], "seq-main");
    assert_eq!(v.monitor().map(|m| m.sequence().sequence_id.as_str()), Some("seq-main"), "the program monitor holds the clock");
    assert_eq!(s["timeline"]["playhead_ticks"], 0);
    assert_eq!(s["timeline"]["playhead_timecode"], "00:00:00:00");
    for name in [timeline::widget::PANEL, timeline::widget::RULER, timeline::widget::SNAP, timeline::widget::RAZOR, timeline::widget::ADD_MARKER] {
        assert!(drew(&v, name), "{name} not drawn: {:?}", v.drawn());
    }
    for n in 1..=3 {
        assert!(drew(&v, &timeline::widget::clip(&cid(n))), "clip {n} block not drawn");
    }
    assert!(drew(&v, &timeline::widget::marker("mk-seq")));
    // F10 hides, the menu call shows it again.
    let _ = frame(&mut v, &ctx, vec![key(egui::Key::F10, egui::Modifiers::NONE)]);
    let _ = frame(&mut v, &ctx, vec![]);
    assert!(!drew(&v, timeline::widget::PANEL));
    v.shell_command(ShellCommand::Panel(Panel::Timeline)).unwrap();
    let _ = frame(&mut v, &ctx, vec![]);
    assert!(drew(&v, timeline::widget::PANEL));
    // The scrubbed playhead is the monitor's playhead.
    v.timeline_command(TimelineCommand::Scrub(Ticks::new(12 * F))).unwrap();
    assert_eq!(v.monitor().unwrap().scheduler().playhead(), Ticks::new(12 * F));
    assert_eq!(v.state_json()["timeline"]["playhead_timecode"], "00:00:00:12");
    // FILE > Close drops the panel with the document.
    v.shell_command(ShellCommand::Close).unwrap();
    assert!(v.timeline().is_none() && v.state_json()["timeline"].is_null());
}

/// Feature `desktop`: Workspace and its Timeline entry clicked through AccessKit open the panel,
/// and the clip blocks are AccessKit nodes labelled with the clip name and frames.
#[cfg(feature = "desktop")]
#[test]
fn accesskit_click_opens_the_timeline_and_clips_are_nodes() {
    let (_s, mut v, ctx) = opened("accesskit");
    ctx.enable_accesskit();
    let out = frame(&mut v, &ctx, vec![]);
    let workspace = ak_find(&out, |l| l == "Workspace").expect("Workspace menu node");
    let _ = frame(&mut v, &ctx, vec![ak_click(workspace)]);
    let out = frame(&mut v, &ctx, vec![]);
    // WORKSPACE > Panels submenu (SMB-HARNESS-SHELL-2 descriptor menu, [STU-SHL-036]).
    let panels = ak_find(&out, |l| l == "Panels").expect("Panels submenu node");
    let _ = frame(&mut v, &ctx, vec![ak_click(panels)]);
    let out = frame(&mut v, &ctx, vec![]);
    let entry = ak_find(&out, |l| l.starts_with("Timeline")).expect("Timeline entry node");
    let _ = frame(&mut v, &ctx, vec![ak_click(entry)]);
    let out = frame(&mut v, &ctx, vec![]);
    assert!(v.timeline().is_some(), "{}", v.state_json()["status"]);
    assert!(ak_find(&out, |l| l == "tone-a [10..30)").is_some(), "clip 1 node");
    assert!(ak_find(&out, |l| l == "trim out tone-b [40..90)").is_some(), "clip 2 tail handle node");
}

/// Clip blocks sit at `lanes_left + frame * px_per_frame` for their in and out, rows stack video
/// over audio under the ruler and work-area bar, and zoom / scroll move them by the stated amounts.
#[test]
fn clip_blocks_sit_at_the_pixels_of_their_in_and_out() {
    // Pure layout in a known area.
    let area = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(1000.0, 200.0));
    let view = TimelineView::default();
    let l = layout(&sequence(), &view, area, &[]);
    let left = 100.0 + HEADER_PX;
    assert_eq!(l.lanes_left, left);
    let rows: Vec<(&str, f32)> = l.tracks.iter().map(|r| (r.name.as_str(), r.rect.min.y)).collect();
    assert_eq!(rows, vec![("V1", 50.0 + RULER_PX + WORK_AREA_PX), ("A1", 50.0 + RULER_PX + WORK_AREA_PX + TRACK_PX)]);
    for (n, start, end, row) in [(1u32, 10.0f32, 30.0f32, 0usize), (2, 40.0, 90.0, 0), (3, 0.0, 50.0, 1)] {
        let b = l.clip(&cid(n)).unwrap();
        assert_eq!((b.x0, b.x1), (left + start * PPF, left + end * PPF), "clip {n}");
        assert_eq!(b.rect.min.y, l.tracks[row].rect.min.y + 2.0);
        assert_eq!(b.track_id, l.tracks[row].track_id);
    }
    assert_eq!(l.clip(&cid(1)).unwrap().label, "tone-a");
    assert_eq!(l.ruler_labels.first().map(|(x, s)| (*x, s.as_str())), Some((left, "00:00:00:00")));
    assert_eq!(l.ruler_labels.get(1).map(|(x, s)| (*x, s.as_str())), Some((left + 25.0 * PPF, "00:00:01:00")), "a label every 25 frames at 4 px/frame");
    // Scrolled by 20 frames and zoomed 2x: clip 1 starts at (10 - 20) * 8 px (clipped at the lanes).
    let zoomed = TimelineView { px_per_frame: 8.0, scroll_frames: 20.0 };
    let l2 = layout(&sequence(), &zoomed, area, &[]);
    let b = l2.clip(&cid(1)).unwrap();
    assert_eq!((b.x0, b.x1, b.rect.min.x), (left - 80.0, left + 80.0, left));
    assert_eq!(l2.clip(&cid(2)).unwrap().x0, left + 160.0);

    // The drawn panel uses the same geometry.
    let (_s, mut v, ctx) = opened("pixels");
    v.shell_command(ShellCommand::Panel(Panel::Timeline)).unwrap();
    let _ = frame(&mut v, &ctx, vec![]);
    let drawn = v.timeline().and_then(|t| t.layout()).cloned().expect("drawn layout");
    for (n, start, end) in [(1u32, 10.0f32, 30.0f32), (2, 40.0, 90.0), (3, 0.0, 50.0)] {
        let b = drawn.clip(&cid(n)).unwrap();
        assert_eq!((b.x0, b.x1), (drawn.lanes_left + start * PPF, drawn.lanes_left + end * PPF), "drawn clip {n}");
    }
    v.timeline_command(TimelineCommand::ZoomIn).unwrap();
    v.timeline_command(TimelineCommand::ScrollTo(5)).unwrap();
    let _ = frame(&mut v, &ctx, vec![]);
    let b = block(&v, 2);
    let lanes_left = v.timeline().unwrap().layout().unwrap().lanes_left;
    assert_eq!(b.x0, lanes_left + (40.0 - 5.0) * 2.0 * PPF);
}

/// A real pointer drag on clip 1's tail trims it 5 frames longer through pulse (one undoable
/// step), a drag on its head trims the head, EDIT > Undo restores each, and the program monitor
/// plays the edited sequence.
#[test]
fn dragging_a_clip_edge_trims_it_through_pulse_and_undo_restores_it() {
    let (_s, mut v, ctx) = opened("trim");
    v.shell_command(ShellCommand::Panel(Panel::Timeline)).unwrap();
    settle(&mut v, &ctx);
    // Clip 1 plays source [0, 20); its source is known to be at least that long, so a longer tail
    // needs the source length: give the monitor extents of 1000 frames for every source.
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
    v.set_monitor_sources(Box::new(NoFrames), Box::new(Long));
    let b = block(&v, 1);
    let y = b.rect.center().y;
    let undo_before = history(&v)["undo"].as_array().unwrap().len();
    // Tail: +20 px = +5 frames (30 -> 35; the next edit point is 5 frames away, beyond the snap).
    drag(&mut v, &ctx, egui::pos2(b.rect.max.x - 2.0, y), egui::pos2(b.rect.max.x - 2.0 + 5.0 * PPF, y));
    assert_eq!(window(&v, 1), (10, 35, 0, 25), "status: {}", v.state_json()["status"]);
    let h = history(&v);
    assert_eq!(h["undo"].as_array().unwrap().len(), undo_before + 1, "one step: {h}");
    assert!(h["undo"].as_array().unwrap().last().unwrap().as_str().unwrap().starts_with("timeline_trim_out"));
    assert_eq!(v.monitor().unwrap().sequence(), &seq(&v), "the monitor plays the edited sequence");
    v.shell_command(ShellCommand::Undo).unwrap();
    settle(&mut v, &ctx);
    assert_eq!(window(&v, 1), (10, 30, 0, 20));
    assert_eq!(v.monitor().unwrap().sequence(), &seq(&v), "undo reaches the monitor too");
    // Head: +12 px = +3 frames (10 -> 13, source in 0 -> 3).
    let b = block(&v, 1);
    drag(&mut v, &ctx, egui::pos2(b.rect.min.x + 2.0, y), egui::pos2(b.rect.min.x + 2.0 + 3.0 * PPF, y));
    assert_eq!(window(&v, 1), (13, 30, 3, 20), "status: {}", v.state_json()["status"]);
    v.shell_command(ShellCommand::Undo).unwrap();
    assert_eq!(window(&v, 1), (10, 30, 0, 20));
    // A trim that would leave the source is refused typed and changes nothing.
    let e = v.timeline_command(TimelineCommand::TrimIn { clip_id: cid(2), head: Ticks::new(5 * F) }).unwrap_err();
    assert!(e.contains("past_source_start") || e.contains("overlap"), "{e}");
    assert_eq!(window(&v, 2), (40, 90, 100, 150));
}

/// Snapping (STU-VID-035, PR-12): a tail dragged to within the snap distance of the next clip's
/// in point lands on it; with snapping off it lands where the pointer put it; a moved clip's head
/// snaps to an edit point and the move keeps the clip id, its source window and is one step.
#[test]
fn dragged_edges_snap_to_edit_points_within_the_snap_distance() {
    // Pure: clip 1's tail dragged +9 frames (30 -> 39) is one frame from clip 2's in point (40).
    let s = sequence();
    let token = CancellationToken::default();
    let c1 = cid(1);
    let targets = snap_targets(&s, None, &[c1.as_str()], &token).unwrap();
    let d = Drag { clip_id: cid(1), kind: DragKind::TrimOut, origin_x: 0.0, start: Ticks::new(10 * F), end: Ticks::new(30 * F), track_id: s.tracks[0].track_id.clone(), track_kind: TrackKind::Video, preview: None };
    let view = TimelineView::default();
    let snapped = drag_target(&d, 9.0 * PPF, &d.track_id, &view, F, Some(&targets));
    assert_eq!((snapped.end, snapped.snapped.as_ref().map(|t| t.kind.key())), (Ticks::new(40 * F), Some("edit_point")));
    let free = drag_target(&d, 9.0 * PPF, &d.track_id, &view, F, None);
    assert_eq!((free.end, free.snapped), (Ticks::new(39 * F), None));
    // Three frames away (12 px) is beyond the 8 px snap distance at 4 px/frame.
    assert_eq!(drag_target(&d, 7.0 * PPF, &d.track_id, &view, F, Some(&targets)).end, Ticks::new(37 * F));

    // Drawn: the same drag with snapping on, then off.
    let (_s, mut v, ctx) = opened("snap");
    v.shell_command(ShellCommand::Panel(Panel::Timeline)).unwrap();
    settle(&mut v, &ctx);
    assert_eq!(v.state_json()["timeline"]["snapping"], true);
    let b = block(&v, 1);
    let y = b.rect.center().y;
    // Clip 1's tail dragged -9 frames to 21: no target within 2 frames of 21, so it stays there.
    drag(&mut v, &ctx, egui::pos2(b.rect.max.x - 2.0, y), egui::pos2(b.rect.max.x - 2.0 - 9.0 * PPF, y));
    assert_eq!(window(&v, 1), (10, 21, 0, 11), "status: {}", v.state_json()["status"]);
    // Clip 2's body dragged -17 frames puts its head at 23, two frames (8 px, the snap distance)
    // from clip 1's new out point at 21: the head snaps onto 21 (tail 73 has no target near).
    let b2 = block(&v, 2);
    let from = egui::pos2(b2.rect.center().x, y);
    drag(&mut v, &ctx, from, egui::pos2(from.x - 17.0 * PPF, y));
    assert_eq!(window(&v, 2), (21, 71, 100, 150), "moved and snapped, same id and source window: {}", v.state_json()["status"]);
    let h = history(&v);
    assert!(h["undo"].as_array().unwrap().last().unwrap().as_str().unwrap().starts_with("timeline_move"), "{h}");
    // Snapping off (S key, panel active): a +2 frame move from 21 would snap back onto 21 with
    // snapping on; off, it lands on 23 exactly.
    let _ = frame(&mut v, &ctx, vec![key(egui::Key::S, egui::Modifiers::NONE)]);
    assert_eq!(v.state_json()["timeline"]["snapping"], false);
    let b2 = block(&v, 2);
    let from = egui::pos2(b2.rect.center().x, y);
    drag(&mut v, &ctx, from, egui::pos2(from.x + 2.0 * PPF, y));
    assert_eq!(window(&v, 2), (23, 73, 100, 150), "status: {}", v.state_json()["status"]);
    // Undo walks the move back as one step.
    v.shell_command(ShellCommand::Undo).unwrap();
    assert_eq!(window(&v, 2), (21, 71, 100, 150));
}

/// Razor at the playhead, lift and ripple delete run through pulse as one undoable step each; a
/// move onto the audio track is refused typed (kind mismatch).
#[test]
fn razor_lift_and_ripple_delete_are_single_undo_steps() {
    let (_s, mut v, ctx) = opened("razor");
    v.shell_command(ShellCommand::Panel(Panel::Timeline)).unwrap();
    let _ = frame(&mut v, &ctx, vec![]);
    let steps = |v: &Viewer| history(v)["undo"].as_array().unwrap().len();
    let before = steps(&v);
    // Playhead at frame 20: clip 1 (V1, [10, 30)) and clip 3 (A1, [0, 50)) are cut (both targeted).
    v.timeline_command(TimelineCommand::Scrub(Ticks::new(20 * F))).unwrap();
    let _ = frame(&mut v, &ctx, vec![key(egui::Key::K, egui::Modifiers::COMMAND)]);
    let s = seq(&v);
    let v1: Vec<(u64, u64)> = s.tracks[0].clips.iter().map(|c| (c.timeline_start.value() / F, c.timeline_end().unwrap().value() / F)).collect();
    let a1: Vec<(u64, u64)> = s.tracks[1].clips.iter().map(|c| (c.timeline_start.value() / F, c.timeline_end().unwrap().value() / F)).collect();
    assert_eq!(v1, vec![(10, 20), (20, 30), (40, 90)], "status: {}", v.state_json()["status"]);
    assert_eq!(a1, vec![(0, 20), (20, 50)]);
    assert_eq!(s.tracks[0].clips[1].source_in, Ticks::new(10 * F), "the right half continues the source");
    assert_eq!(steps(&v), before + 1, "both cuts are one step");
    v.shell_command(ShellCommand::Undo).unwrap();
    assert_eq!(seq(&v), sequence(), "undo restores the opened sequence");
    // Lift clip 1 (gap stays), then ripple delete clip 2's predecessor gap via clip 1 again.
    v.timeline_command(TimelineCommand::Select(Some(cid(1)))).unwrap();
    v.timeline_command(TimelineCommand::Lift).unwrap();
    assert!(seq(&v).tracks[0].clips.iter().all(|c| c.clip_id != cid(1)));
    assert_eq!(window(&v, 2), (40, 90, 100, 150), "lift leaves the gap");
    v.shell_command(ShellCommand::Undo).unwrap();
    v.timeline_command(TimelineCommand::Select(Some(cid(1)))).unwrap();
    v.timeline_command(TimelineCommand::RippleDelete).unwrap();
    assert_eq!(window(&v, 2), (20, 70, 100, 150), "ripple delete closes 20 frames");
    assert_eq!(window(&v, 3), (0, 50, 0, 50), "the sync-locked audio clip starts before the gap and stays");
    v.shell_command(ShellCommand::Undo).unwrap();
    assert_eq!(window(&v, 2), (40, 90, 100, 150));
    // Refusals are typed and change nothing.
    let a1 = seq(&v).tracks[1].track_id.clone();
    let e = v.timeline_command(TimelineCommand::MoveClip { clip_id: cid(1), start: Ticks::new(60 * F), track_id: Some(a1) }).unwrap_err();
    assert_eq!(e, "track_kind_mismatch");
    v.timeline_command(TimelineCommand::Select(None)).unwrap();
    assert_eq!(v.timeline_command(TimelineCommand::Lift).unwrap_err(), "no_selection");
    // A scrub past the end clamps to the last frame (89, inside clip 2); at frame 0 nothing is cut.
    v.timeline_command(TimelineCommand::Scrub(Ticks::new(95 * F))).unwrap();
    assert_eq!(v.monitor().unwrap().scheduler().playhead(), Ticks::new(89 * F));
    v.timeline_command(TimelineCommand::Scrub(Ticks::new(0))).unwrap();
    assert_eq!(v.timeline_command(TimelineCommand::RazorAtPlayhead).unwrap_err(), "nothing_under_playhead");
}

/// Markers: the sequence marker is drawn on the ruler, M adds one at the playhead frame (one
/// undoable step), Shift+M / Ctrl+Shift+M and the buttons jump the playhead between markers.
#[test]
fn markers_show_add_at_the_playhead_and_jump() {
    let (_s, mut v, ctx) = opened("markers");
    v.shell_command(ShellCommand::Panel(Panel::Timeline)).unwrap();
    let _ = frame(&mut v, &ctx, vec![]);
    let m = v.timeline().unwrap().layout().unwrap().markers.iter().find(|m| m.marker_id == "mk-seq").cloned().unwrap();
    let left = v.timeline().unwrap().layout().unwrap().lanes_left;
    assert_eq!(m.x, left + 70.0 * PPF);
    v.timeline_command(TimelineCommand::Scrub(Ticks::new(12 * F))).unwrap();
    let _ = frame(&mut v, &ctx, vec![key(egui::Key::M, egui::Modifiers::NONE)]);
    let s = seq(&v);
    let added: Vec<(String, u64, String)> = s.markers.iter().map(|m| (m.marker_id.clone(), m.start.value() / F, m.name.clone())).collect();
    assert_eq!(added, vec![("mk-seq".to_owned(), 70, "cue".to_owned()), ("mk-tl-1".to_owned(), 12, "Marker 2".to_owned())], "status: {}", v.state_json()["status"]);
    assert!(history(&v)["undo"].as_array().unwrap().last().unwrap().as_str().unwrap().starts_with("timeline_marker_add"));
    let _ = frame(&mut v, &ctx, vec![]);
    assert!(drew(&v, &timeline::widget::marker("mk-tl-1")));
    // Jumps: from 12, next = 70; previous from 70 = 12; previous from 12 = none (typed).
    let _ = frame(&mut v, &ctx, vec![key(egui::Key::M, egui::Modifiers::SHIFT)]);
    assert_eq!(v.monitor().unwrap().scheduler().playhead(), Ticks::new(70 * F));
    let _ = frame(&mut v, &ctx, vec![key(egui::Key::M, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT)]);
    assert_eq!(v.monitor().unwrap().scheduler().playhead(), Ticks::new(12 * F));
    assert_eq!(v.timeline_command(TimelineCommand::PreviousMarker).unwrap_err(), "no_previous_marker");
    // Next / previous edit (Down / Up): from 12 the next edit point is 30 (clip 1 out).
    let _ = frame(&mut v, &ctx, vec![key(egui::Key::ArrowDown, egui::Modifiers::NONE)]);
    assert_eq!(v.monitor().unwrap().scheduler().playhead(), Ticks::new(30 * F));
    // A marker added mid-frame lands on the frame start.
    v.timeline_command(TimelineCommand::Scrub(Ticks::new(33 * F))).unwrap();
    v.timeline_command(TimelineCommand::AddMarker).unwrap();
    assert!(seq(&v).markers.iter().any(|m| m.marker_id == "mk-tl-2" && m.start == Ticks::new(33 * F)));
    v.shell_command(ShellCommand::Undo).unwrap();
    assert!(!seq(&v).markers.iter().any(|m| m.marker_id == "mk-tl-2"));
}

/// Hostile sequences (never valid in Folio, built directly): zero-length, overlapping and
/// overflowing clips, 100 000 clips on one track, 2 000 tracks and a zero frame rate all lay out
/// without panicking and within the drawing bounds.
#[test]
fn hostile_sequences_lay_out_bounded() {
    let area = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 400.0));
    let view = TimelineView::default();
    let mut s = sequence();
    let mut zero = clip(9, "zero", 5, 5, 31);
    zero.duration = Ticks::new(0);
    s.tracks[0].clips.insert(1, zero);
    s.tracks[0].clips.push(clip(10, "overlap", 0, 60, 35));
    let mut huge = clip(11, "overflow", 0, 10, 0);
    huge.timeline_start = Ticks::new(u64::MAX - 1);
    s.tracks[0].clips.push(huge);
    let l = layout(&s, &view, area, &[]);
    let z = l.clip(&cid(9)).expect("zero-length clip drawn");
    assert!(z.rect.width() >= 1.0, "one pixel at least");
    assert!(l.clip(&cid(10)).is_some(), "overlapping clip drawn");
    assert!(l.clip(&cid(11)).is_none());

    // 100 000 one-frame clips: at most MAX_DRAWN_CLIPS blocks, flagged truncated only when the view
    // holds more; at the default zoom only the visible ~284 frames are drawn.
    let mut many = sequence();
    many.tracks[0].clips = (0..100_000u32).map(|i| clip(100 + i, "c", 0, 1, u64::from(i))).collect();
    let l = layout(&many, &view, area, &[]);
    let visible = l.clips.iter().filter(|c| c.track_id == many.tracks[0].track_id).count();
    assert!(visible <= MAX_DRAWN_CLIPS && visible > 200, "{visible}");
    let wide = TimelineView { px_per_frame: 1.0 / 4096.0, scroll_frames: 0.0 };
    let l = layout(&many, &wide, area, &[]);
    assert!(l.clips.len() <= MAX_DRAWN_CLIPS && l.clips_truncated, "{} drawn", l.clips.len());

    // 2 000 tracks: at most MAX_DRAWN_TRACKS rows (and only as many as fit), the rest counted.
    let mut tall = sequence();
    tall.tracks = (0..2000u32).map(|i| track(1000 + i, TrackKind::Video, "V", vec![])).collect();
    let l = layout(&tall, &view, area, &[]);
    assert!(l.tracks.len() <= MAX_DRAWN_TRACKS);
    assert_eq!(l.tracks.len() + l.tracks_hidden, 2000);

    // A zero frame rate and an absurd zoom do not divide by zero or overflow.
    let mut broken = sequence();
    broken.frame_rate = SequenceFrameRate { ticks_per_frame: 0 };
    let l = layout(&broken, &TimelineView { px_per_frame: 1e-9, scroll_frames: 1e300 }, area, &[]);
    assert!(l.ruler_labels.len() <= hsk_studio_harness_view::view::timeline::MAX_RULER_LABELS);
}

