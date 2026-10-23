//! Lane SMB-PR-SOURCE-MONITOR slice 1 (feature `viewer`; PR-19, STU-VID-023/024/025): the source
//! monitor plays one media source in isolation (frame-accurate scrub and step, marks, play
//! in-to-out, J/K/L shuttle), and insert / overwrite carry its marked window into the program
//! sequence through pulse's assembly edit as one undoable step. Every source frame carries its own
//! source frame number in red, so a frame shown by a monitor names exactly which source tick it
//! came from. Headless (no window); Premiere parity UNVERIFIED (D5).
#![cfg(feature = "viewer")]

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::monitor::{FrameSource, HeadlessClock, SourceFrame, Transport};
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::source_monitor::{
    self, AssemblyMode, FourPointFit, MonitorKey, MonitorTarget, SourceEdit, SourceMonitor,
};
use hsk_studio_pulse::SourceExtents;
use std::sync::Arc;

mod common;
use common::Scratch;

/// 25 fps; 48 kHz = 1920 samples per frame.
const F: u64 = 10_160_640_000;
const SPF: u64 = 1920;
const SIDE: u32 = 2;
const RATE: SequenceFrameRate = SequenceFrameRate { ticks_per_frame: F };

fn media(name: &str) -> ClipSource {
    ClipSource::Media {
        artifact_manifest_id: name.into(),
        content_digest: ContentDigest {
            algorithm: "sha256".into(),
            digest: "abc123".into(),
        },
        asset_id: None,
    }
}

/// Source frame `n` of a medium: red = n / 1000, green = medium tag, blue 0.5; exact in binary32.
fn frame_of(n: u64, tag: f32) -> Arc<SourceFrame> {
    let (mut colour, mut alpha) = (Vec::new(), Vec::new());
    #[allow(clippy::cast_precision_loss)]
    let r = n as f32 / 1024.0;
    for _ in 0..SIDE * SIDE {
        for c in [r, tag, 0.5f32] {
            colour.extend_from_slice(&c.to_le_bytes());
        }
        alpha.extend_from_slice(&1.0f32.to_le_bytes());
    }
    Arc::new(SourceFrame {
        width: SIDE,
        height: SIDE,
        colour_sha256: hsk_studio_prism::profile_hash(&colour),
        alpha_sha256: hsk_studio_prism::profile_hash(&alpha),
        colour,
        alpha,
    })
}
fn tag(source: &ClipSource) -> Option<f32> {
    let ClipSource::Media { artifact_manifest_id, .. } = source else { return None };
    match artifact_manifest_id.as_str() {
        "red" => Some(0.25),
        "blue" => Some(0.5),
        "green" => Some(0.75),
        _ => None,
    }
}
/// Every medium's frame `n` (source tick / F, whole frames only).
struct Numbered;
impl FrameSource for Numbered {
    fn frame(&self, source: &ClipSource, tick: Ticks) -> Option<Arc<SourceFrame>> {
        Some(frame_of(tick.value() / F, tag(source)?))
    }
}
struct Extent;
impl SourceExtents for Extent {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(1000 * F))
    }
}
/// (source frame, medium tag) of the frame a monitor shows.
fn shown(m: &hsk_studio_harness::view::monitor::ProgramMonitor) -> (u64, f32) {
    let p = m.shown().expect("a frame is shown").image.pixel(0, 0).unwrap();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    ((p[0] * 1024.0).round() as u64, p[1])
}

fn clip(n: u32, src: &str, si: u64, frames: u64, start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: format!("SCLP-019abcde-0000-7000-8000-{n:012x}"),
        source_ref: media(src),
        source_in: Ticks::new(si * F),
        source_out: Ticks::new((si + frames) * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new(frames * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    }
}
/// V1: red 0..20 (source 0..20), blue 20..40 (source 100..120).
fn sequence() -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: RATE,
        duration: Ticks::new(40 * F),
        tracks: vec![StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: "STRK-019abcde-0000-7000-8000-000000000001".into(),
            kind: TrackKind::Video,
            index: 0,
            name: "V1".into(),
            enabled: true,
            locked: false,
            sync_locked: true,
            targeted: true,
            muted: false,
            soloed: false,
            clips: vec![clip(1, "red", 0, 20, 0), clip(2, "blue", 100, 20, 20)],
            volume: 1.0,
            pan: 0.0,
        }],
        markers: Vec::new(),
        work_area: None,
    }
}

fn open_viewer(scratch: &Scratch) -> Viewer {
    let doc = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000001","revision":3,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[serde_json::to_value(sequence()).unwrap()]
    });
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
    let token = CancellationToken::default();
    let Inspection::Editable(snapshot) =
        folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), Budget::default(), &token, &Any).unwrap()
    else {
        panic!("read-only fixture")
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
    let path = scratch.0.join("timeline.handshake");
    std::fs::write(&path, bytes).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&path).unwrap_or_else(|e| panic!("{e}: {}", v.state_json()));
    v.open_monitor("seq-main", SIDE, SIDE).unwrap();
    v.set_monitor_sources(Box::new(Numbered), Box::new(Extent));
    v
}

/// Clips of V1 as (source tag, source in frame, sequence start frame, frames).
fn v1(v: &Viewer) -> Vec<(String, u64, u64, u64)> {
    let doc = v.run().snapshot().unwrap().document();
    let seq = doc.sequences.iter().find(|s| s.sequence_id == "seq-main").unwrap();
    let mut out: Vec<_> = seq.tracks[0]
        .clips
        .iter()
        .map(|c| {
            let ClipSource::Media { artifact_manifest_id, .. } = &c.source_ref else { panic!() };
            (artifact_manifest_id.clone(), c.source_in.value() / F, c.timeline_start.value() / F, c.duration.value() / F)
        })
        .collect();
    out.sort_by_key(|c| c.2);
    out
}

#[test]
fn source_monitor_plays_scrubs_marks_and_shuttles_frame_accurately() {
    let cancel = CancellationToken::default();
    let mut s = SourceMonitor::new(media("green"), Ticks::new(60 * F + 7), RATE, SIDE, SIDE).unwrap();
    assert_eq!(s.frames(), 60, "a partial last frame is not shown");
    let mut clock = HeadlessClock::new(48_000);
    let m = s.monitor_mut();
    assert_eq!(m.refresh(&clock, &Numbered, &Extent, &cancel).unwrap(), Some(0));
    assert_eq!(shown(m), (0, 0.75));
    // Frame-accurate scrub: any tick inside frame 37 shows source frame 37; steps are exact.
    m.scrub(Ticks::new(37 * F + F - 1), &clock).unwrap();
    assert_eq!(m.refresh(&clock, &Numbered, &Extent, &cancel).unwrap(), Some(37));
    assert_eq!(shown(m), (37, 0.75));
    m.step(-5, &clock).unwrap();
    m.refresh(&clock, &Numbered, &Extent, &cancel).unwrap();
    assert_eq!(shown(m), (32, 0.75));
    // Marks at the playhead and explicit; the window is whole source frames, out inclusive.
    s.mark_in_at_playhead().unwrap();
    s.mark_out(40).unwrap();
    assert_eq!(s.marks(), (Some(32), Some(40)));
    assert_eq!(s.source_window().unwrap(), (Ticks::new(32 * F), Some(Ticks::new(41 * F))));
    assert_eq!(s.mark_in(41).unwrap_err().code, "in_after_out");
    assert_eq!(s.mark_out(31).unwrap_err().code, "out_before_in");
    assert_eq!(s.mark_out(60).unwrap_err().code, "outside_range");
    assert_eq!(s.marks(), (Some(32), Some(40)), "refused marks change nothing");
    // Play in to out: starts at the in mark and ends after the out frame.
    let m = s.monitor_mut();
    m.play_in_to_out(&clock).unwrap();
    clock.advance(3 * SPF);
    assert_eq!(m.refresh(&clock, &Numbered, &Extent, &cancel).unwrap(), Some(35));
    assert_eq!(shown(m), (35, 0.75));
    clock.advance(20 * SPF);
    assert_eq!(m.refresh(&clock, &Numbered, &Extent, &cancel).unwrap(), None);
    assert_eq!(m.scheduler().transport(), Transport::Ended);
    // J/K/L: L twice = 2x forward from frame 10; K stops; J = 1x reverse.
    m.scrub(Ticks::new(10 * F), &clock).unwrap();
    m.step(0, &clock).unwrap(); // back to the whole range after play in-to-out
    m.shuttle(false, &clock).unwrap();
    let speed = m.shuttle(false, &clock).unwrap();
    assert_eq!((speed.numerator(), speed.denominator()), (2, 1));
    clock.advance(4 * SPF);
    assert_eq!(m.refresh(&clock, &Numbered, &Extent, &cancel).unwrap(), Some(18));
    assert_eq!(shown(m), (18, 0.75));
    m.shuttle_stop(&clock).unwrap();
    assert_eq!(m.scheduler().transport(), Transport::Paused);
    let speed = m.shuttle(true, &clock).unwrap();
    assert_eq!((speed.numerator(), speed.denominator()), (-1, 1));
    clock.advance(3 * SPF);
    m.refresh(&clock, &Numbered, &Extent, &cancel).unwrap();
    assert_eq!(shown(m).0, 15, "reverse 1x for three frames from 18");
    // Key table (STU-VID transport / clip command rows; I/O are Studio keys).
    use egui::Key;
    let k = source_monitor::key_command;
    assert_eq!(k(Key::J, false), Some(MonitorKey::ShuttleLeft));
    assert_eq!(k(Key::J, true), Some(MonitorKey::SlowLeft));
    assert_eq!(k(Key::K, false), Some(MonitorKey::ShuttleStop));
    assert_eq!(k(Key::L, false), Some(MonitorKey::ShuttleRight));
    assert_eq!(k(Key::ArrowRight, true), Some(MonitorKey::StepForward(5)));
    assert_eq!(k(Key::Comma, false), Some(MonitorKey::Insert));
    assert_eq!(k(Key::Period, false), Some(MonitorKey::Overwrite));
    assert_eq!(k(Key::I, false), Some(MonitorKey::MarkIn));
    assert_eq!(k(Key::Q, false), None);
    // Hostile sources are typed refusals.
    let fail = |extent: u64, tpf: u64| SourceMonitor::new(media("green"), Ticks::new(extent), SequenceFrameRate { ticks_per_frame: tpf }, SIDE, SIDE).err().unwrap().code;
    assert_eq!(fail(F - 1, F), "source_empty");
    assert_eq!(fail(10 * F, 0), "source_frame_rate");
    assert_eq!(fail(10 * F, 12_345), "source_frame_rate");
    // 60 fps (STU-VID-013 table): one frame more than MAX_SOURCE_FRAMES.
    const F60: u64 = 4_233_600_000;
    assert_eq!(fail(F60 * (source_monitor::MAX_SOURCE_FRAMES + 1), F60), "limit_exceeded");
    assert_eq!(SourceMonitor::new(media("green"), Ticks::new(10 * F), RATE, 0, 4).err().unwrap().code, "monitor_size");
}

#[test]
fn insert_and_overwrite_from_the_source_monitor_land_in_the_program_with_undo() {
    let scratch = Scratch::new("source-monitor");
    let mut v = open_viewer(&scratch);
    assert_eq!(v.source_edit(&SourceEdit { mode: AssemblyMode::Insert, targets: vec![], fit: None }).unwrap_err().code, "no_source_monitor");
    v.open_source_monitor(media("green"), Ticks::new(50 * F), RATE, SIDE, SIDE).unwrap();
    v.set_source_monitor_sources(Box::new(Numbered));
    // Source marks 2..=5 (4 frames); program playhead at frame 10; ',' inserts.
    let s = v.source_monitor_mut().unwrap();
    s.mark_in(2).unwrap();
    s.mark_out(5).unwrap();
    let clock = HeadlessClock::new(48_000);
    v.program_monitor_mut().unwrap().scrub(Ticks::new(10 * F), &clock).unwrap();
    v.monitor_key(MonitorTarget::Source, MonitorKey::Insert).unwrap();
    assert_eq!(
        v1(&v),
        [
            ("red".into(), 0, 0, 10),
            ("green".into(), 2, 10, 4),
            ("red".into(), 10, 14, 10),
            ("blue".into(), 100, 24, 20)
        ],
        "insert splits red at 10 and shifts the rest by 4 frames"
    );
    // The program monitor plays the edited sequence: frame 11 = green source frame 3.
    let cancel = CancellationToken::default();
    let p = v.program_monitor_mut().unwrap();
    p.scrub(Ticks::new(11 * F), &clock).unwrap();
    p.refresh(&clock, &Numbered, &Extent, &cancel).unwrap();
    assert_eq!(shown(p), (3, 0.75));
    assert_eq!(p.sequence().duration, Ticks::new(44 * F));
    // Undo restores the original sequence exactly; redo re-applies.
    assert!(v_undo(&mut v));
    assert_eq!(v1(&v), [("red".into(), 0, 0, 20), ("blue".into(), 100, 20, 20)]);
    // Overwrite: program in/out 30..=32 (3 frames) and no source out: 3-point backtimes the source
    // out from the program span (source 2..5); blue 30..33 is replaced, nothing moves.
    let s = v.source_monitor_mut().unwrap();
    s.clear_marks();
    s.mark_in(2).unwrap();
    let p = v.program_monitor_mut().unwrap();
    p.set_in_out(30, 32).unwrap();
    v.source_edit(&SourceEdit { mode: AssemblyMode::Overwrite, targets: vec![], fit: None }).unwrap();
    assert_eq!(
        v1(&v),
        [
            ("red".into(), 0, 0, 20),
            ("blue".into(), 100, 20, 10),
            ("green".into(), 2, 30, 3),
            ("blue".into(), 113, 33, 7)
        ]
    );
    // Four points that disagree need an explicit fit; TrimTail keeps the source in and the span.
    let s = v.source_monitor_mut().unwrap();
    s.mark_out(20).unwrap();
    v.program_monitor_mut().unwrap().set_in_out(0, 1).unwrap();
    let four = |fit| SourceEdit { mode: AssemblyMode::Overwrite, targets: vec![], fit };
    assert_eq!(v.source_edit(&four(None)).unwrap_err().code, "fit_required");
    v.source_edit(&four(Some(FourPointFit::TrimTail))).unwrap();
    assert_eq!(v1(&v)[0], ("green".into(), 2, 0, 2));
    // Unknown destination tracks are refused before planning.
    let bad = SourceEdit { mode: AssemblyMode::Insert, targets: vec!["STRK-nope".into()], fit: None };
    assert_eq!(v.source_edit(&bad).unwrap_err().code, "unknown_track");
    // The panel draws its stable ids headlessly.
    let ctx = egui::Context::default();
    let _ = ctx.run(egui::RawInput::default(), |ctx| v.ui(ctx));
    for id in [source_monitor::widget::PANEL, source_monitor::widget::MARK_IN, source_monitor::widget::INSERT, source_monitor::widget::OVERWRITE, "source_monitor.play", "source_monitor.shuttle_right"] {
        assert!(v.drawn().iter().any(|d| d == id), "{id} not drawn: {:?}", v.drawn());
    }
}

fn v_undo(v: &mut Viewer) -> bool {
    v.undo().is_ok()
}

/// Feature `media`: the source monitor opens a real media file (reel's ProRes fixture, 40x24, 25 fps,
/// 4 frames, ffmpeg-made, provenance in `hsk-studio-reel/tests/fixtures/prores/provenance.json`),
/// shows its decoded frames frame-accurately and its marked window inserts into the program.
#[cfg(feature = "media")]
#[test]
fn source_monitor_decodes_a_media_file_and_inserts_its_marked_window() {
    const PRORES: &[u8] = include_bytes!("../../hsk-studio-reel/tests/fixtures/prores/prores-422hq-40x24-p.mov");
    let scratch = Scratch::new("source-media");
    let mut v = open_viewer(&scratch);
    let source = v.open_source_media(PRORES.to_vec(), 40, 24).unwrap();
    let s = v.source_monitor().unwrap();
    assert_eq!((s.frames(), s.ticks_per_frame()), (4, F));
    let ctx = egui::Context::default();
    let mut frame_pixels = Vec::new();
    for frame in [0u64, 3, 0] {
        let clock = HeadlessClock::new(48_000);
        v.source_monitor_mut().unwrap().monitor_mut().scrub(Ticks::new(frame * F), &clock).unwrap();
        let _ = ctx.run(egui::RawInput::default(), |ctx| v.ui(ctx));
        let m = v.source_monitor().unwrap().monitor();
        assert_eq!(m.last_error(), None, "{}", m.state_json());
        let shown = m.shown().unwrap();
        assert_eq!(shown.frame, frame);
        frame_pixels.push((0..40).flat_map(|x| shown.image.pixel(x, 12).unwrap()).collect::<Vec<f32>>());
    }
    assert_ne!(frame_pixels[0], frame_pixels[1], "frames 0 and 3 of the pattern differ");
    assert_eq!(frame_pixels[0], frame_pixels[2], "scrubbing back shows the same decoded frame");
    // Marks 1..=2 overwrite program frames 5..7 of the red clip.
    let s = v.source_monitor_mut().unwrap();
    s.mark_in(1).unwrap();
    s.mark_out(2).unwrap();
    v.program_monitor_mut().unwrap().scrub(Ticks::new(5 * F), &HeadlessClock::new(48_000)).unwrap();
    v.monitor_key(MonitorTarget::Source, MonitorKey::Overwrite).unwrap();
    let seq = v.run().snapshot().unwrap().document().sequences.iter().find(|q| q.sequence_id == "seq-main").unwrap().clone();
    let placed = seq.tracks[0].clips.iter().find(|c| c.source_ref == source).expect("the media clip is on V1");
    assert_eq!((placed.source_in, placed.timeline_start, placed.duration), (Ticks::new(F), Ticks::new(5 * F), Ticks::new(2 * F)));
    // A file that is not media is a typed refusal.
    assert!(v.open_source_media(b"not a media file".to_vec(), 40, 24).is_err());
}
