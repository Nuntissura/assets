//! Lane SMB-PR-ENTRY-REFERENCES-1 (lead decision on F-R56-3, requirement 4): `Viewer::audio_routing`
//! answers Ok when the routing names a sequence, a clip or a track the document does not hold, keeps
//! the bytes, and the sequence's own routing still builds. No window is opened.
#![cfg(all(feature = "viewer", feature = "audio-batch"))]

mod support_context;

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_package::{AssetSource, Extras, Limits, MemoryAssetSource, Quarantined, WriteOptions, sha256_hex_of, write_package_with};

const F: u64 = 10_160_640_000;
const STALE_CLIP: &str = "SCLP-019abcde-0000-7000-8000-0000000000ee";
const STALE_TRACK: &str = "STRK-019abcde-0000-7000-8000-0000000000ee";

struct Any;
impl Resolver for Any {
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

fn sequence() -> StudioSequence {
    let clip = StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: "SCLP-019abcde-0000-7000-8000-000000000001".into(),
        source_ref: ClipSource::Media { artifact_manifest_id: "tone".into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() }, asset_id: None },
        source_in: Ticks::new(0),
        source_out: Ticks::new(4 * F),
        timeline_start: Ticks::new(0),
        duration: Ticks::new(4 * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
        retime: Default::default(),
    };
    let track = StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: "STRK-019abcde-0000-7000-8000-000000000001".into(),
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
    };
    let mut s = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "seq-main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(0),
        tracks: vec![track],
        markers: Vec::new(),
        work_area: None,
    };
    s.duration = s.content_end().unwrap();
    s
}

fn stale_routing() -> StudioAudioRoutingSet {
    let mut own = StudioAudioRouting::new("seq-main");
    own.clip_gains.push(StudioClipGainEnvelope { clip_id: STALE_CLIP.into(), points: vec![StudioClipGainPoint { offset_ticks: 0, gain_db: -6.0 }] });
    own.routes.push(StudioTrackRoute { track_id: STALE_TRACK.into(), output: None, sends: vec![] });
    StudioAudioRoutingSet { routings: vec![own, StudioAudioRouting::new("seq-gone")] }
}

fn package(path: &std::path::Path, body: &[u8]) {
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
    let sha256_hex = sha256_hex_of(body);
    let held = [Quarantined { name: format!("studio/audio_routing.{sha256_hex}"), bytes: body.to_vec(), sha256_hex }];
    let assets = MemoryAssetSource::new();
    let mut bytes = Vec::new();
    write_package_with(&snapshot, &assets as &dyn AssetSource, &Extras { quarantined: &held, previews: &[] }, &WriteOptions::default(), &Limits::default(), &token, &mut bytes).unwrap();
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn the_viewer_reads_a_routing_with_inert_ids_and_keeps_its_bytes() {
    let dir = std::env::temp_dir().join(format!("hsk-viewer-inert-routing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("inert.handshake");
    let body = serde_json::to_vec(&stale_routing()).unwrap();
    package(&path, &body);
    let mut v = support_context::viewer(Prefer::Cpu);
    v.open(&path).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(v.audio_routing().unwrap(), Some(stale_routing()));
    assert_eq!(v.run().document_entry("audio_routing"), Some(body.as_slice()));
    // The Viewer's own write takes the same answer: an inert id is not refused.
    v.set_audio_routing(&stale_routing()).unwrap();
    assert_eq!(v.audio_routing().unwrap(), Some(stale_routing()));
    drop(v);
    let _ = std::fs::remove_file(&path);
}
