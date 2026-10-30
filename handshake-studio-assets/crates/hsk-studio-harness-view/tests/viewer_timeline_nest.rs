//! Lane SMB-PR-NEST-1 (feature `viewer`): Nest, Make Subsequence, Open Nested and Back to Parent
//! in the timeline panel ([STU-VID-030] / [STU-VID-040]), driven headlessly (one-frame egui draws,
//! no window). Oracles: the run's document after each step (hand-computed clips of a two-clip V1
//! with a linked A1 clip), the session history (one undo step per nest), EDIT > Undo / Redo
//! restoring and re-applying the exact sequences, and the program monitor's requested source frame.
//!
//! NOT_PROVEN (printed): program-monitor pixels of a nested clip (no frame decoder is bound on a
//! document sequence; the test proves which source and tick the monitor asks for); a real desktop
//! window; Premiere's receiving track, track mirroring and range cut (no Premiere oracle, D5).
#![cfg(feature = "viewer")]

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::monitor::{FrameSource, MonitorFail, SourceFrame};
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::shell::{Panel, ShellCommand};
use hsk_studio_harness_view::view::source_monitor::{MonitorKey, MonitorTarget};
use hsk_studio_harness_view::view::timeline::{TimelineCommand, widget};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 25 fps ticks per frame (STU-VID-013).
const F: u64 = 10_160_640_000;

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let base = std::env::temp_dir();
        let text = base.to_string_lossy().to_ascii_uppercase();
        assert!(!(cfg!(windows) && text.starts_with("C:")), "TMP is on C: ({}); run through studio-env.sh", base.display());
        let dir = base.join(format!("hsk-harness-view-nest-{}-{label}", std::process::id()));
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
fn clip(n: u32, name: &str, si: u64, so: u64, start: u64, group: Option<&str>) -> StudioClip {
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
        link_group: group.map(str::to_owned),
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
/// V1: clip 1 `a` source [10, 30) at frames [0, 20) linked `lg-1` with A1 clip 3 `d` source [0, 20)
/// at [0, 20); V1 clip 2 `b` source [0, 10) at [30, 40).
fn sequence() -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(40 * F),
        tracks: vec![
            track(1, TrackKind::Video, "V1", vec![clip(1, "a", 10, 30, 0, Some("lg-1")), clip(2, "b", 0, 10, 30, None)]),
            track(2, TrackKind::Audio, "A1", vec![clip(3, "d", 0, 20, 0, Some("lg-1"))]),
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

fn package(dir: &Path) -> PathBuf {
    let doc = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000009","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
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
    let path = dir.join("nest.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

/// The viewer with the fixture open and the timeline shown (source lengths unknown).
fn opened(label: &str) -> (Scratch, Viewer, egui::Context) {
    let scratch = Scratch::new(label);
    let mut v = Viewer::new(Prefer::Cpu);
    let ctx = egui::Context::default();
    v.open(&package(&scratch.0)).unwrap_or_else(|e| panic!("{e}"));
    v.shell_command(ShellCommand::Panel(Panel::Timeline)).unwrap();
    (scratch, v, ctx)
}

fn input(events: Vec<egui::Event>, time: f64) -> egui::RawInput {
    egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 1200.0))), events, time: Some(time), ..Default::default() }
}
fn frame(v: &mut Viewer, ctx: &egui::Context, events: Vec<egui::Event>, time: f64) -> egui::FullOutput {
    ctx.run(input(events, time), |ctx| v.ui(ctx))
}
fn sequences(v: &Viewer) -> Vec<StudioSequence> {
    v.run().snapshot().unwrap().document().sequences.clone()
}
fn undo_labels(v: &Viewer) -> Vec<String> {
    v.run().history()["undo"].as_array().unwrap().iter().filter_map(Value::as_str).map(str::to_owned).collect()
}
fn status(v: &Viewer) -> Value {
    v.state_json()["status"].clone()
}
/// The nested sequence clip on V1 of `seq-main` after a nest of clip 1.
fn nested_clip(v: &Viewer) -> StudioClip {
    let s = sequences(v);
    let main = s.iter().find(|q| q.sequence_id == "seq-main").unwrap();
    main.tracks[0].clips.iter().find(|c| matches!(c.source_ref, ClipSource::Sequence { .. })).cloned().expect("nested clip on V1")
}

/// Checks the document after nesting clip 1 (its link group brings clip 3).
fn assert_nested(v: &Viewer) -> String {
    let s = sequences(v);
    assert_eq!(s.len(), 2, "one new sequence");
    let main = &s[0];
    let inner = &s[1];
    let n = nested_clip(v);
    assert_eq!((n.timeline_start, n.duration, n.source_in, n.source_out), (Ticks::new(0), Ticks::new(20 * F), Ticks::new(0), Ticks::new(20 * F)));
    assert_eq!(n.source_ref, ClipSource::Sequence { sequence_id: inner.sequence_id.clone() });
    assert_eq!(main.tracks[0].clips[1].clip_id, cid(2), "the unselected clip stays");
    let a = &main.tracks[1].clips;
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].source_ref, n.source_ref, "one nested audio clip");
    assert_eq!(a[0].link_group, n.link_group);
    assert!(n.link_group.is_some());
    let moved: Vec<String> = inner.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| c.clip_id.clone()).collect();
    assert_eq!(moved, [cid(1), cid(3)]);
    assert_eq!(inner.frame_rate, main.frame_rate);
    inner.sequence_id.clone()
}

#[cfg(feature = "desktop")]
fn ak_find(out: &egui::FullOutput, pred: impl Fn(&str) -> bool) -> Option<egui::accesskit::NodeId> {
    let tree = out.platform_output.accesskit_update.as_ref()?;
    tree.nodes.iter().find(|(_, n)| n.label().is_some_and(&pred)).map(|(id, _)| *id)
}

/// The toolbar's Nest button (an AccessKit click, as a screen reader or a model does) nests the
/// selected clip and its link group as ONE undo step; EDIT > Undo restores the exact sequences and
/// EDIT > Redo re-applies them.
#[cfg(feature = "desktop")]
#[test]
fn nest_from_the_toolbar_is_one_undo_step() {
    let (_s, mut v, ctx) = opened("toolbar");
    ctx.enable_accesskit();
    let before = sequences(&v);
    v.timeline_command(TimelineCommand::Select(Some(cid(1)))).unwrap();
    let out = frame(&mut v, &ctx, vec![], 1.0);
    let nest = ak_find(&out, |l| l == "Nest").expect("Nest toolbar node");
    let click = egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest { action: egui::accesskit::Action::Click, target: nest, data: None });
    let _ = frame(&mut v, &ctx, vec![click], 1.1);
    let _ = frame(&mut v, &ctx, vec![], 1.2);
    let labels = undo_labels(&v);
    assert_eq!(labels.len(), 1, "one history step: {labels:?} {}", status(&v));
    assert!(labels[0].starts_with("sequence.nest "), "{labels:?}");
    let inner = assert_nested(&v);
    assert!(labels[0].contains(&inner) && labels[0].contains("seq-main"), "{labels:?}");
    let after = sequences(&v);
    v.shell_command(ShellCommand::Undo).unwrap();
    assert_eq!(sequences(&v), before, "undo restores the exact sequences");
    v.shell_command(ShellCommand::Redo).unwrap();
    assert_eq!(sequences(&v), after, "redo re-applies the nest");
}

/// The headless Nest command is the same one step; Make Subsequence from the program monitor's
/// in-to-out range copies the clips cut at the range edges into a new sequence and leaves
/// `seq-main` unchanged (one undo step); refusals change nothing.
#[test]
fn nest_and_make_subsequence_commands_are_one_step_each() {
    let (_s, mut v, _ctx) = opened("commands");
    let before = sequences(&v);
    assert_eq!(v.timeline_command(TimelineCommand::Nest(Vec::new())).unwrap_err(), "no_selection");
    assert!(v.timeline_command(TimelineCommand::Nest(vec![cid(99)])).unwrap_err().contains("unknown_clip"));
    assert_eq!(sequences(&v), before, "refusals change nothing");
    v.timeline_command(TimelineCommand::Nest(vec![cid(1)])).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
    assert_nested(&v);
    assert_eq!(undo_labels(&v).len(), 1);
    v.shell_command(ShellCommand::Undo).unwrap();
    assert_eq!(sequences(&v), before);
    // In-to-out [5, 35): clip 1 -> [5, 20) source 15..30 at 0; clip 2 -> [30, 35) source 0..5 at 25;
    // clip 3 -> [5, 20) source 5..20 at 0.
    v.timeline_command(TimelineCommand::Select(None)).unwrap();
    // Program monitor marks: in at frame 5, out at frame 34 (inclusive).
    v.timeline_command(TimelineCommand::Scrub(Ticks::new(5 * F))).unwrap();
    v.monitor_key(MonitorTarget::Program, MonitorKey::MarkIn).unwrap();
    v.timeline_command(TimelineCommand::Scrub(Ticks::new(34 * F))).unwrap();
    v.monitor_key(MonitorTarget::Program, MonitorKey::MarkOut).unwrap();
    v.timeline_command(TimelineCommand::MakeSubsequence(Vec::new())).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
    let s = sequences(&v);
    assert_eq!(s[0], before[0], "the open sequence is unchanged");
    let sub = &s[1];
    assert_eq!(sub.name, "Subsequence 1");
    let got: Vec<(u64, u64, u64)> = sub.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| (c.source_in.value() / F, c.source_out.value() / F, c.timeline_start.value() / F)).collect();
    assert_eq!(got, [(15, 30, 0), (0, 5, 25), (5, 20, 0)]);
    assert!(sub.tracks.iter().flat_map(|t| t.clips.iter()).all(|c| ![cid(1), cid(2), cid(3)].contains(&c.clip_id)), "copies take fresh ids");
    let labels = undo_labels(&v);
    assert!(labels.last().is_some_and(|l| l.starts_with("sequence.make_subsequence ")), "{labels:?}");
    v.shell_command(ShellCommand::Undo).unwrap();
    assert_eq!(sequences(&v), before);
}

/// Open Nested (the selected nested clip, then a double-click on the clip block) switches the
/// timeline and the program monitor to the nested sequence with the playhead mapped into it and a
/// back stack; Back to Parent returns with the parent's playhead.
#[test]
fn open_nested_and_back_to_parent_switch_the_timeline_sequence() {
    let (_s, mut v, ctx) = opened("open");
    v.timeline_command(TimelineCommand::Nest(vec![cid(1)])).unwrap();
    let inner = assert_nested(&v);
    let nested = nested_clip(&v);
    assert_eq!(v.timeline_command(TimelineCommand::OpenNested(Some(cid(2)))).unwrap_err(), "not_nested");
    v.timeline_command(TimelineCommand::Scrub(Ticks::new(7 * F))).unwrap();
    v.timeline_command(TimelineCommand::Select(Some(nested.clip_id.clone()))).unwrap();
    v.timeline_command(TimelineCommand::OpenNested(None)).unwrap();
    assert_eq!(v.timeline().unwrap().sequence_id(), inner);
    assert_eq!(v.monitor().unwrap().sequence().sequence_id, inner, "one clock: the monitor follows");
    assert_eq!(v.timeline_playhead(), Some(Ticks::new(7 * F)), "playhead mapped into the nested sequence");
    assert_eq!(v.state_json()["timeline"]["nest_back"], serde_json::json!(["seq-main"]));
    v.timeline_command(TimelineCommand::Scrub(Ticks::new(3 * F))).unwrap();
    v.timeline_command(TimelineCommand::BackToParent).unwrap();
    assert_eq!(v.timeline().unwrap().sequence_id(), "seq-main");
    assert_eq!(v.timeline_playhead(), Some(Ticks::new(7 * F)), "the parent's playhead comes back");
    assert_eq!(v.timeline_command(TimelineCommand::BackToParent).unwrap_err(), "no_parent");
    // A double-click on the nested clip block opens it.
    let mut t = 2.0;
    for _ in 0..3 {
        let _ = frame(&mut v, &ctx, vec![], t);
        t += 0.5;
    }
    let b = v.timeline().and_then(|tl| tl.layout()).and_then(|l| l.clip(&nested.clip_id)).cloned().expect("nested clip drawn");
    let at = b.rect.center();
    let press = |down| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: down, modifiers: egui::Modifiers::NONE };
    let _ = frame(&mut v, &ctx, vec![egui::Event::PointerMoved(at)], t);
    for dt in [0.05, 0.1, 0.15, 0.2] {
        let _ = frame(&mut v, &ctx, vec![press(dt == 0.05 || dt == 0.15)], t + dt);
    }
    let _ = frame(&mut v, &ctx, vec![], t + 0.3);
    assert_eq!(v.timeline().unwrap().sequence_id(), inner, "double-click opens the nested sequence: {}", status(&v));
    let _ = frame(&mut v, &ctx, vec![], t + 0.4);
    for name in [widget::NEST, widget::MAKE_SUBSEQUENCE, widget::OPEN_NESTED, widget::BACK_TO_PARENT, widget::NEST_PATH] {
        assert!(v.drawn().iter().any(|d| d == name), "{name} not drawn: {:?}", v.drawn());
    }
}

/// A nested clip is one clip to timeline edits: trimming its tail changes only its window onto
/// the nested sequence (bounded by that sequence's length, [STU-VID-030] "opaque").
#[test]
fn trimming_a_nested_clip_changes_only_its_window() {
    let (_s, mut v, _ctx) = opened("trim");
    v.timeline_command(TimelineCommand::Nest(vec![cid(1)])).unwrap();
    let inner_before = sequences(&v)[1].clone();
    let id = nested_clip(&v).clip_id;
    v.timeline_command(TimelineCommand::TrimOut { clip_id: id.clone(), tail: Ticks::new(12 * F) }).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
    let n = nested_clip(&v);
    assert_eq!((n.source_in, n.source_out, n.duration), (Ticks::new(0), Ticks::new(12 * F), Ticks::new(12 * F)));
    assert_eq!(sequences(&v)[1], inner_before, "the nested sequence's clips are untouched");
    // Extending back to the nested sequence's end is allowed (its length is known), past it refused.
    v.timeline_command(TimelineCommand::TrimOut { clip_id: id.clone(), tail: Ticks::new(20 * F) }).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
    assert_eq!(nested_clip(&v).source_out, Ticks::new(20 * F));
    let past = v.timeline_command(TimelineCommand::TrimOut { clip_id: id, tail: Ticks::new(21 * F) }).unwrap_err();
    assert!(past.contains("past_source_end"), "{past}");
}

/// Asks for no pixels: records the (source, tick) the monitor requests and refuses typed.
#[derive(Clone, Default)]
struct Probe(Arc<Mutex<Vec<(ClipSource, Ticks)>>>);
impl FrameSource for Probe {
    fn frame(&self, _: &ClipSource, _: Ticks) -> Option<Arc<SourceFrame>> {
        None
    }
    fn frame_result(&self, source: &ClipSource, tick: Ticks) -> Result<Arc<SourceFrame>, MonitorFail> {
        self.0.lock().unwrap().push((source.clone(), tick));
        Err(MonitorFail { code: "probe", detail: String::new() })
    }
}

/// The program monitor resolves a nested clip: at parent frame 6 it asks for media `a` at source
/// frame 16 (the nested sequence's clip 1 reads `a` from 10), never for the Sequence source.
#[test]
fn program_monitor_asks_for_the_nested_sequences_media() {
    let (_s, mut v, _ctx) = opened("monitor");
    v.timeline_command(TimelineCommand::Nest(vec![cid(1)])).unwrap();
    let probe = Probe::default();
    let cancel = CancellationToken::default();
    let e = v.monitor().unwrap().render_at(Ticks::new(6 * F), &probe, &hsk_studio_pulse::UnknownExtents, &cancel).unwrap_err();
    assert_eq!(e.code, "probe", "{e:?}");
    assert_eq!(probe.0.lock().unwrap().as_slice(), [(media("a"), Ticks::new(16 * F))]);
    println!("NOT_PROVEN: program-monitor pixels of a nested clip (no frame decoder bound on a document sequence)");
}

/// A clip whose clip gain envelope lives in the audio routing entry (keyed by the parent sequence)
/// nests with its envelope (lane SMB-PR-NEST-2 closed NEST-1's `nest_clip_envelopes_unsupported`
/// refusal): one history step, the envelope now in the new sequence's routing, unchanged (the full
/// proof is tests/viewer_timeline_nest_2.rs).
#[cfg(feature = "audio-batch")]
#[test]
fn nest_moves_a_clip_routing_envelope_with_the_clip() {
    let (_s, mut v, _ctx) = opened("envelope");
    let mut routing = StudioAudioRouting::new("seq-main");
    let envelope = StudioClipGainEnvelope { clip_id: cid(3), points: vec![StudioClipGainPoint { offset_ticks: 0, gain_db: -6.0 }] };
    routing.clip_gains.push(envelope.clone());
    v.set_audio_routing(&StudioAudioRoutingSet { routings: vec![routing] }).unwrap();
    let undo = undo_labels(&v).len();
    // Clip 1's link group brings clip 3 along.
    v.timeline_command(TimelineCommand::Nest(vec![cid(1)])).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
    let inner = assert_nested(&v);
    assert_eq!(undo_labels(&v).len(), undo + 1, "one history step");
    let set = v.audio_routing().unwrap().unwrap();
    let find = |id: &str| set.routings.iter().find(|r| r.sequence_id == id).cloned().unwrap();
    assert!(find("seq-main").clip_gains.is_empty());
    assert_eq!(find(&inner).clip_gains, [envelope]);
}

/// Export of a sequence holding a nested clip never drops it silently: the job is refused
/// `nest_unresolved` naming the nested clip, before anything renders.
#[cfg(feature = "media")]
#[test]
fn export_refuses_a_nested_clip_typed() {
    use hsk_studio_harness_view::view::timeline_export::{TimelineExport, timeline_export_job};
    use hsk_studio_reel::{ChromaFormat, Container, ExportRecipe, Vc3Profile, VideoCodecChoice};
    let (s, mut v, _ctx) = opened("export");
    v.timeline_command(TimelineCommand::Nest(vec![cid(1)])).unwrap();
    let main = sequences(&v)[0].clone();
    let nested = nested_clip(&v);
    struct Unknown;
    impl hsk_studio_pulse::SourceExtents for Unknown {
        fn extent(&self, _: &ClipSource) -> Option<Ticks> {
            None
        }
    }
    #[derive(Clone)]
    struct Silent;
    impl hsk_studio_pulse::AudioSources for Silent {
        fn audio(&self, _: &ClipSource) -> Option<&[Vec<f32>]> {
            None
        }
    }
    let job = timeline_export_job(
        TimelineExport {
            sequence: main,
            in_tick: 0,
            out_tick: 20 * F,
            recipe: ExportRecipe { recipe_id: "r".into(), container: Container::Mov, video: Some(VideoCodecChoice::Vc3(Vc3Profile::DnxhrHq)), audio: None },
            width: 64,
            height: 32,
            bit_depth: 8,
            chroma: ChromaFormat::Yuv422,
            video_sources: Probe::default(),
            video_extents: Unknown,
            audio: Some((Silent, 48_000)),
            audio_extents: Unknown,
            output: s.0.join("never.mov"),
            overwrite: false,
            artifact: "nest".into(),
        },
        &CancellationToken::default(),
    );
    let e = job.err().expect("refused");
    assert_eq!(e.code, "nest_unresolved");
    assert!(e.detail.starts_with(&nested.clip_id), "{}", e.detail);
    assert!(!s.0.join("never.mov").exists());
}
