//! Lane SMB-PULSE-PLAYBACK (feature `viewer`): the program monitor driven headlessly (no window).
//! A sequence with two clips and a 10-frame cross dissolve plays on a deterministic headless
//! clock; frames render through composite's scene path on render-cpu and are compared with the
//! hand values (tolerance 2e-6, binary32 compositing); pause, step and scrub are frame-exact; the
//! viewer opens the monitor on a sequence of a native package and draws its stable widget ids.
#![cfg(feature = "viewer")]

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::monitor::{
    self, DropPolicy, FrameSource, HeadlessClock, ProgramMonitor, SourceFrame, Transport,
};
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_pulse::{CROSS_DISSOLVE, SourceExtents};
use std::sync::Arc;

mod common;
use common::Scratch;

/// 25 fps; 48 kHz = 1920 samples per frame.
const F: u64 = 10_160_640_000;
const SPF: u64 = 1920;
const SIDE: u32 = 4;
const RED: [f32; 3] = [0.8, 0.2, 0.1];
const BLUE: [f32; 3] = [0.1, 0.4, 0.9];

fn solid(rgb: [f32; 3]) -> Arc<SourceFrame> {
    let (mut colour, mut alpha) = (Vec::new(), Vec::new());
    for _ in 0..SIDE * SIDE {
        for c in rgb {
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
/// Every frame of manifest "red" is red, of "blue" blue.
struct Solids(Arc<SourceFrame>, Arc<SourceFrame>);
impl FrameSource for Solids {
    fn frame(&self, source: &ClipSource, _: Ticks) -> Option<Arc<SourceFrame>> {
        match source {
            ClipSource::Media {
                artifact_manifest_id,
                ..
            } if artifact_manifest_id == "red" => Some(self.0.clone()),
            ClipSource::Media {
                artifact_manifest_id,
                ..
            } if artifact_manifest_id == "blue" => Some(self.1.clone()),
            _ => None,
        }
    }
}
struct Extent;
impl SourceExtents for Extent {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(1000 * F))
    }
}

fn clip(n: u32, src: &str, si: u64, frames: u64, start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: format!("SCLP-019abcde-0000-7000-8000-{n:012x}"),
        source_ref: ClipSource::Media {
            artifact_manifest_id: src.into(),
            content_digest: ContentDigest {
                algorithm: "sha256".into(),
                digest: "abc123".into(),
            },
        },
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
/// V1: red 0..20, blue 20..40, a 10-frame cross dissolve at the cut (frames 15..25).
fn sequence() -> StudioSequence {
    let mut red = clip(1, "red", 0, 20, 0);
    red.transitions.tail = Some(TimelineTransition {
        schema_id: TransitionSchema::V1,
        transition_id: "t1".into(),
        transition_kind: CROSS_DISSOLVE.into(),
        duration: Ticks::new(10 * F),
        alignment: TransitionAlignment::CenteredOnCut,
        center_offset: 0,
        parameters: Vec::new(),
        repeat_end_frame: false,
    });
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
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
            clips: vec![red, clip(2, "blue", 100, 20, 20)],
            volume: 1.0,
            pan: 0.0,
        }],
        markers: Vec::new(),
        work_area: None,
    }
}
fn mix(p: f32) -> [f32; 4] {
    [
        RED[0] * (1.0 - p) + BLUE[0] * p,
        RED[1] * (1.0 - p) + BLUE[1] * p,
        RED[2] * (1.0 - p) + BLUE[2] * p,
        1.0,
    ]
}
fn close(got: [f32; 4], want: [f32; 4]) {
    assert!(
        got.iter().zip(want).all(|(g, w)| (g - w).abs() <= 2e-6),
        "{got:?} vs {want:?}"
    );
}

#[test]
fn program_monitor_plays_pauses_steps_and_scrubs_through_composite() {
    let cancel = CancellationToken::default();
    let sources = Solids(solid(RED), solid(BLUE));
    let mut m = ProgramMonitor::new(sequence(), SIDE, SIDE, DropPolicy::DropLate).unwrap();
    let mut clock = HeadlessClock::new(48_000);
    let shown = |m: &ProgramMonitor| m.shown().unwrap().image.pixel(1, 2).unwrap();

    // Paused at frame 0: the first refresh shows it (red).
    assert_eq!(
        m.refresh(&clock, &sources, &Extent, &cancel).unwrap(),
        Some(0)
    );
    close(shown(&m), mix(0.0));
    // Play 18 frames of audio: the due frame is 18, inside the dissolve at progress 3/10.
    m.play(&clock).unwrap();
    clock.advance(18 * SPF + 7);
    assert_eq!(
        m.refresh(&clock, &sources, &Extent, &cancel).unwrap(),
        Some(18)
    );
    close(shown(&m), mix(0.3));
    assert_eq!(
        m.refresh(&clock, &sources, &Extent, &cancel).unwrap(),
        None,
        "hold"
    );
    // Pause holds frame 18; step forward/back are frame-exact; scrub lands on whole frames.
    m.pause(&clock).unwrap();
    clock.advance(30 * SPF);
    assert_eq!(m.refresh(&clock, &sources, &Extent, &cancel).unwrap(), None);
    assert_eq!(m.step(1, &clock).unwrap(), Ticks::new(19 * F));
    assert_eq!(
        m.refresh(&clock, &sources, &Extent, &cancel).unwrap(),
        Some(19)
    );
    close(shown(&m), mix(0.4));
    m.step(-15, &clock).unwrap();
    assert_eq!(
        m.refresh(&clock, &sources, &Extent, &cancel).unwrap(),
        Some(4)
    );
    close(shown(&m), mix(0.0));
    m.scrub(Ticks::new(33 * F + 5), &clock).unwrap();
    assert_eq!(
        m.refresh(&clock, &sources, &Extent, &cancel).unwrap(),
        Some(33)
    );
    close(shown(&m), [BLUE[0], BLUE[1], BLUE[2], 1.0]);
    // Play to the end.
    m.play(&clock).unwrap();
    clock.advance(10 * SPF);
    assert_eq!(m.refresh(&clock, &sources, &Extent, &cancel).unwrap(), None);
    assert_eq!(m.scheduler().transport(), Transport::Ended);
    assert_eq!(m.state_json()["transport"], "Ended");

    // A missing decoded frame is a typed failure, the shown frame stays.
    struct Nothing;
    impl FrameSource for Nothing {
        fn frame(&self, _: &ClipSource, _: Ticks) -> Option<Arc<SourceFrame>> {
            None
        }
    }
    m.scrub(Ticks::new(2 * F), &clock).unwrap();
    assert_eq!(
        m.refresh(&clock, &Nothing, &Extent, &cancel)
            .unwrap_err()
            .code,
        "frame_unavailable"
    );
    assert_eq!(m.last_error().unwrap().code, "frame_unavailable");
    assert!(ProgramMonitor::new(sequence(), 0, 4, DropPolicy::DropLate).is_err());

    // The monitor's transport row draws its stable ids on a bare egui context.
    let ctx = egui::Context::default();
    let mut drawn = Vec::new();
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| drawn = m.ui(ui, &clock));
    });
    assert_eq!(
        drawn,
        [
            monitor::widget::PLAY,
            monitor::widget::PAUSE,
            monitor::widget::STEP_BACK,
            monitor::widget::STEP_FORWARD,
            monitor::widget::SCRUB,
            monitor::widget::FRAME
        ]
    );
}

#[test]
fn viewer_opens_the_monitor_on_a_package_sequence() {
    let scratch = Scratch::new("monitor");
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
    let snapshot = match folio::inspect_bytes(
        &serde_json::to_vec(&doc).unwrap(),
        Budget::default(),
        &token,
        &Any,
    )
    .unwrap()
    {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read-only fixture"),
    };
    let assets = hsk_studio_package::MemoryAssetSource::new();
    let mut bytes = Vec::new();
    hsk_studio_package::write_package_with(
        &snapshot,
        &assets as &dyn hsk_studio_package::AssetSource,
        &hsk_studio_package::Extras {
            quarantined: &[],
            previews: &[],
        },
        &hsk_studio_package::WriteOptions::default(),
        &hsk_studio_package::Limits::default(),
        &token,
        &mut bytes,
    )
    .unwrap();
    let path = scratch.0.join("timeline.handshake");
    std::fs::write(&path, bytes).unwrap();

    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&path)
        .unwrap_or_else(|e| panic!("{e}: {}", v.state_json()));
    assert_eq!(
        v.open_monitor("seq-other", SIDE, SIDE).unwrap_err().code,
        "unknown_sequence"
    );
    v.open_monitor("seq-main", SIDE, SIDE).unwrap();
    v.set_monitor_sources(Box::new(Solids(solid(RED), solid(BLUE))), Box::new(Extent));
    let ctx = egui::Context::default();
    let _ = ctx.run(egui::RawInput::default(), |ctx| v.ui(ctx));
    let m = v.monitor().unwrap();
    assert_eq!(m.shown().unwrap().frame, 0, "{}", m.state_json());
    close(m.shown().unwrap().image.pixel(0, 0).unwrap(), mix(0.0));
    let drawn = v.drawn();
    for id in [
        monitor::widget::PANEL,
        monitor::widget::PLAY,
        monitor::widget::SCRUB,
    ] {
        assert!(drawn.iter().any(|d| d == id), "{id} not drawn: {drawn:?}");
    }
}
