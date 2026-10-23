//! SMB-PACKAGE-COMPLETENESS-2: optional CKC catalog `asset_id` on media clips and placed sources
//! (serde-default: absent is not encoded, earlier documents stay byte-identical; present must be
//! the CKC `Asset.asset_id` UUID grammar) and the default resolver ports for fonts, audio, LUTs and
//! linked placed sources (Absent, so no resolver is falsely treated as a provider).

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::*;
use serde_json::{Value, json};

const F: u64 = 10_160_640_000;
const ASSET: &str = "0190f5c2-7a1b-7c3d-8e4f-000000000abc";

struct Host;
impl Resolver for Host {
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

fn document(asset_id: Option<&str>) -> Value {
    let clip = StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: "SCLP-019abcde-0000-7000-8000-000000000001".into(),
        source_ref: ClipSource::Media {
            artifact_manifest_id: "media".into(),
            content_digest: ContentDigest { algorithm: "sha256".into(), digest: "ab".into() },
            asset_id: asset_id.map(str::to_owned),
        },
        source_in: Ticks::new(0),
        source_out: Ticks::new(10 * F),
        timeline_start: Ticks::new(0),
        duration: Ticks::new(10 * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    };
    let seq = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(10 * F),
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
            clips: vec![clip],
            volume: 1.0,
            pan: 0.0,
        }],
        markers: Vec::new(),
        work_area: None,
    };
    json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000001","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","name":"placed","parent":null,"kind":"raster","payload":{"payload_kind":"raster","tiles":[]},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[serde_json::to_value(seq).unwrap()]
    })
}

fn inspect(v: &Value) -> std::result::Result<Snapshot, Code> {
    match inspect_bytes(&serde_json::to_vec(v).unwrap(), Budget::default(), &CancellationToken::default(), &Host) {
        Ok(Inspection::Editable(s)) => Ok(s),
        Ok(Inspection::ReadOnly(_)) => panic!("fixture became read-only"),
        Err(d) => Err(d.code),
    }
}

#[test]
fn media_clip_catalog_asset_id_is_optional_typed_and_absent_is_not_encoded() {
    let without = inspect(&document(None)).unwrap();
    let wire = String::from_utf8(without.encoded_bytes().to_vec()).unwrap();
    assert!(!wire.contains("asset_id"), "absent is not encoded");
    // A document written before the field existed reads unchanged.
    assert_eq!(serde_json::from_slice::<Value>(without.encoded_bytes()).unwrap(), document(None));
    let with = inspect(&document(Some(ASSET))).unwrap();
    assert!(String::from_utf8(with.encoded_bytes().to_vec()).unwrap().contains(ASSET));
    for bad in ["0190F5C2-7A1B-7C3D-8E4F-000000000ABC", "not-a-uuid", "", "0190f5c2-7a1b-7c3d-8e4f-000000000abc\n"] {
        assert_eq!(inspect(&document(Some(bad))).err(), Some(Code::InvalidField), "{bad:?}");
    }
}

#[test]
fn placed_source_catalog_asset_id_is_optional_and_typed() {
    let doc: StudioDocument = serde_json::from_value(document(None)).unwrap();
    let records = |asset: Option<&str>| {
        let mut source = json!({"source_id":"src","location":{"mode":"linked","uri":"links/a.psd","content_digest":"sha256-ab","health":"up_to_date"},"width":2.0,"height":2.0});
        if let Some(a) = asset {
            source["asset_id"] = json!(a);
        }
        json!({"schema_id":"hsk.studio.raster_records@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000001",
            "layers":[{"layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","placed_asset":{"source_id":"src","transform":[1.0,0.0,0.0,1.0,0.0,0.0]}}],
            "placed_sources":[source]})
    };
    let check = |v: Value| {
        let r: StudioRasterRecords = serde_json::from_value(v).unwrap();
        validate_raster_records(&r, &doc, Budget::default(), &CancellationToken::default(), &Host).map(|_| r).map_err(|d| d.code)
    };
    let plain = check(records(None)).unwrap();
    assert!(!serde_json::to_string(&plain).unwrap().contains("asset_id"), "absent is not encoded");
    assert_eq!(check(records(Some(ASSET))).unwrap().placed_sources[0].asset_id.as_deref(), Some(ASSET));
    assert_eq!(check(records(Some("SAST-1"))).err(), Some(Code::InvalidField));
}

#[test]
fn new_resolver_ports_default_to_absent() {
    assert_eq!(Unresolved.font("Inter", "Bold"), Resolution::Absent);
    assert_eq!(Unresolved.audio("a"), Resolution::Absent);
    assert_eq!(Unresolved.lut("l", None), Resolution::Absent);
    assert_eq!(Unresolved.placed_link("links/a.psd", "sha256-ab"), Resolution::Absent);
    assert!(is_catalog_asset_id(ASSET));
}
