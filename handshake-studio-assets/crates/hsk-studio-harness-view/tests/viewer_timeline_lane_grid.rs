//! R44 finding F-R44-12 (lane SMB-R44-FOLLOWUPS-1, feature `audio-batch`): Clip Volume and Clip Pan
//! lane points must lie on the sample grid, like the Track, Master and Bus lanes. A hostile
//! `AutomationCommand::AddPoint` (and `MovePoint`) at a tick between two samples is refused
//! `not_sample_aligned` with no history step and the routing unchanged; the same command one sample
//! later is accepted as one step. Oracles: pulse's tick/sample relation (254016000000 ticks per
//! second, 5292000 ticks per sample at 48 kHz), the run's history and the stored routing.
#![cfg(feature = "audio-batch")]

mod support_context;

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::timeline::{AutomationCommand, LaneTarget, PointId};
use hsk_studio_pulse::{AudioSources, SourceExtents};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 25 fps ticks per frame; 1920 samples per frame at 48 kHz.
const F: u64 = 10_160_640_000;
const SPF: usize = 1920;
const RATE: u32 = 48_000;
/// Ticks per sample at 48 kHz.
const TPS: u64 = 254_016_000_000 / 48_000;

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-harness-view-lane-grid-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const A1: &str = "STRK-019abcde-0000-7000-8000-000000000002";
const CLIP: &str = "SCLP-019abcde-0000-7000-8000-000000000003";

/// A1: clip 3 `tone` source [0, 40) at frames [0, 40).
fn sequence() -> StudioSequence {
    let clip = StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: CLIP.into(),
        source_ref: ClipSource::Media { artifact_manifest_id: "tone".into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() }, asset_id: None },
        source_in: Ticks::new(0),
        source_out: Ticks::new(40 * F),
        timeline_start: Ticks::new(0),
        duration: Ticks::new(40 * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    };
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(40 * F),
        tracks: vec![StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: A1.into(),
            kind: TrackKind::Audio,
            index: 0,
            name: "A1".into(),
            enabled: true,
            locked: false,
            sync_locked: true,
            targeted: true,
            muted: false,
            soloed: false,
            clips: vec![clip],
            volume: 1.0,
            pan: 0.0,
        }],
        markers: Vec::new(),
        work_area: None,
    }
}

#[derive(Clone)]
struct Sources(Arc<Vec<Vec<f32>>>);
impl AudioSources for Sources {
    fn audio(&self, _: &ClipSource) -> Option<&[Vec<f32>]> {
        Some(self.0.as_slice())
    }
}
struct Ext;
impl SourceExtents for Ext {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(40 * F))
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
    let path = dir.join("lane-grid.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

fn history(v: &Viewer) -> serde_json::Value {
    v.run().history()
}
fn entry(v: &Viewer) -> Option<Vec<u8>> {
    v.run().document_entry("audio_routing").map(<[u8]>::to_vec)
}

#[test]
fn clip_volume_and_clip_pan_points_off_the_sample_grid_are_refused_without_a_step() {
    let scratch = Scratch::new("grid");
    let mut v = crate::support_context::viewer(Prefer::Cpu);
    v.open(&package(&scratch.0)).unwrap_or_else(|e| panic!("{e}"));
    v.open_multitrack(RATE, 960, Sources(Arc::new(vec![vec![0.5; 40 * SPF]])), Arc::new(Ext)).unwrap_or_else(|e| panic!("{e}: {}", v.state_json()["status"]));
    assert_eq!(TPS * u64::from(RATE), 254_016_000_000, "whole ticks per sample");
    let on_grid = 5 * F + 7 * TPS;
    for (lane, value) in [(LaneTarget::ClipVolume, -3.0), (LaneTarget::ClipPan, 0.5)] {
        let (h0, e0) = (history(&v), entry(&v));
        // Hostile: one tick past a sample.
        let add = AutomationCommand::AddPoint { track_id: A1.into(), lane: lane.clone(), at: Ticks::new(on_grid + 1), value };
        let e = v.multitrack_command(add).expect_err("off-grid point refused");
        assert!(e.contains("not_sample_aligned"), "{lane:?}: {e}");
        assert_eq!((history(&v), entry(&v)), (h0.clone(), e0.clone()), "{lane:?}: no history step, routing unchanged");
        // The same lane on the grid: one step.
        v.multitrack_command(AutomationCommand::AddPoint { track_id: A1.into(), lane: lane.clone(), at: Ticks::new(on_grid), value }).unwrap_or_else(|e| panic!("{lane:?}: {e}"));
        let h1 = history(&v);
        assert_eq!(h1["undo"].as_array().map(Vec::len), h0["undo"].as_array().map(|u| u.len() + 1), "{lane:?}: one step");
        // A move of that point off the grid is refused the same way.
        let point = PointId { clip_id: Some(CLIP.into()), offset_ticks: on_grid };
        let e1 = entry(&v);
        let e = v.multitrack_command(AutomationCommand::MovePoint { track_id: A1.into(), lane: lane.clone(), point, at: Ticks::new(on_grid + TPS / 2), value }).expect_err("off-grid move refused");
        assert!(e.contains("not_sample_aligned"), "{lane:?}: {e}");
        assert_eq!((history(&v), entry(&v)), (h1, e1), "{lane:?}: the refused move leaves no step");
    }
}
