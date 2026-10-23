//! SMB-PACKAGE-COMPLETENESS through the session: open reports unavailable external references as
//! typed loss notes, `Session::externals` gives their relink data, and a session save keeps an
//! embedded external embedded and a linked one linked (nothing dropped, nothing silently embedded).

use hsk_studio_accord::{ActorContext, CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_package::*;
use hsk_studio_session::*;
use serde_json::json;

const F: u64 = 10_160_640_000;

struct Host;
impl folio::Resolver for Host {
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
}

struct Quiet;
impl SinkPort for Quiet {
    fn try_send(&mut self, _: DeliveryClass, _: &[u8]) -> Result<(), DeliveryError> {
        Ok(())
    }
}

fn token() -> CancellationToken {
    CancellationToken::default()
}

fn clip(n: u32, name: &str, bytes: &[u8], start: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: format!("SCLP-019abcde-0000-7000-8000-{n:012x}"),
        source_ref: ClipSource::Media {
            artifact_manifest_id: name.into(),
            content_digest: ContentDigest { algorithm: "sha256".into(), digest: sha256_hex_of(bytes) },
            asset_id: None,
        },
        source_in: Ticks::new(0),
        source_out: Ticks::new(10 * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new(10 * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    }
}

#[test]
fn session_open_reports_and_save_keeps_external_links_and_embeds() {
    let embedded = b"embedded media bytes".to_vec();
    let linked = b"media only in the artifact tier".to_vec();
    let seq = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(20 * F),
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
            clips: vec![clip(1, "media-embedded", &embedded, 0), clip(2, "media-linked", &linked, 10)],
            volume: 1.0,
            pan: 0.0,
        }],
        markers: Vec::new(),
        work_area: None,
    };
    let doc = json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000001","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[serde_json::to_value(seq).unwrap()]
    });
    let snapshot = match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), folio::Budget::default(), &token(), &Host).unwrap() {
        folio::Inspection::Editable(s) => s,
        folio::Inspection::ReadOnly(_) => panic!("read-only fixture"),
    };
    // The package embeds the one media item the source grants.
    let mut assets = MemoryAssetSource::new();
    assets.insert(embedded.clone());
    let mut package = Vec::new();
    let options = WriteOptions { embed_external: EmbedExternal::Granted, ..WriteOptions::default() };
    write_package_with(&snapshot, &assets, &Extras::default(), &options, &Limits::default(), &token(), &mut package).unwrap();

    let dir = std::env::temp_dir().join(format!("hsk-session-externals-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("source.handshake");
    std::fs::write(&source, &package).unwrap();
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let ports = || SessionPorts::new(Box::new(Quiet)).with_resolver(Box::new(Host));
    let mut session = Session::open_path(&source, SessionConfig::new(actor.clone()), ports(), &token()).unwrap();

    // Open: the linked media no resolver has is a typed loss; the embedded one is not.
    let lost: Vec<&str> = session.loss().iter().filter(|n| n.reason == LossReason::UnavailableExternal).map(|n| n.name.as_str()).collect();
    assert_eq!(lost, [format!("external/timeline_media/{}", sha256_hex_of(&linked)).as_str()]);
    let externals = session.externals().unwrap();
    let state = |name: &str| externals.iter().find(|e| e.handle == name).map(|e| e.state);
    assert_eq!(state("media-embedded"), Some(LinkState::Embedded));
    assert_eq!(state("media-linked"), Some(LinkState::Missing));
    let linked_ref = externals.iter().find(|e| e.handle == "media-linked").unwrap();
    assert_eq!(linked_ref.content_digest.as_ref().unwrap().digest, sha256_hex_of(&linked), "relink by hash");

    // Save: the embedded copy stays, the link stays a link, the document is unchanged.
    let saved = dir.join("saved.handshake");
    session.save_to(&saved, &token()).unwrap();
    let reopened = read_package(&std::fs::read(&saved).unwrap(), &Limits::default(), folio::Budget::default(), &Host, &token()).unwrap();
    assert_eq!(reopened.assets.get(&sha256_hex_of(&embedded)), Some(&embedded), "embedded media kept");
    assert!(!reopened.assets.contains_key(&sha256_hex_of(&linked)), "a link is never silently embedded");
    assert_eq!(reopened.snapshot().unwrap().encoded_bytes(), snapshot.encoded_bytes(), "every link kept");
    assert_eq!(reopened.externals, {
        let again = Session::open_path(&saved, SessionConfig::new(actor), ports(), &token()).unwrap();
        again.externals().unwrap()
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn linked_placed_sources_are_health_checked_next_to_the_opened_file() {
    let source_bytes = b"placed source psd bytes".to_vec();
    let doc = json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000001","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","name":"placed","parent":null,"kind":"raster","payload":{"payload_kind":"raster","tiles":[]},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    });
    let snapshot = match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), folio::Budget::default(), &token(), &Host).unwrap() {
        folio::Inspection::Editable(s) => s,
        folio::Inspection::ReadOnly(_) => panic!("read-only fixture"),
    };
    let records = json!({"schema_id":"hsk.studio.raster_records@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000001",
        "layers":[{"layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","placed_asset":{"source_id":"src","transform":[1.0,0.0,0.0,1.0,0.0,0.0]}}],
        "placed_sources":[{"source_id":"src","location":{"mode":"linked","uri":"links/source.psd","content_digest":format!("sha256-{}", sha256_hex_of(&source_bytes)),"health":"up_to_date"},
            "width":2.0,"height":2.0,"asset_id":"0190f5c2-7a1b-7c3d-8e4f-000000000abc"}]});
    let typed: folio::StudioRasterRecords = serde_json::from_value(records.clone()).unwrap();
    folio::validate_raster_records(&typed, snapshot.document(), folio::Budget::default(), &token(), &Host).expect("valid placed-source records");
    let held = [ImportEntry::new(ImportRole::Records, serde_json::to_vec(&records).unwrap()).to_opaque()];
    let mut package = Vec::new();
    write_package_with(&snapshot, &MemoryAssetSource::new(), &Extras { quarantined: &held, previews: &[] }, &WriteOptions::default(), &Limits::default(), &token(), &mut package).unwrap();

    let dir = std::env::temp_dir().join(format!("hsk-session-linked-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("links")).unwrap();
    let path = dir.join("doc.handshake");
    std::fs::write(&path, &package).unwrap();
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let open = || Session::open_path(&path, SessionConfig::new(actor.clone()), SessionPorts::new(Box::new(Quiet)).with_resolver(Box::new(Host)), &token()).unwrap();
    let state = |s: &Session| s.externals().unwrap().into_iter().find(|e| e.handle == "links/source.psd").map(|e| (e.state, e.asset_id));
    let key = format!("external/placed_source/{}", sha256_hex_of(&source_bytes));
    let lost = |s: &Session| s.loss().iter().any(|n| n.reason == LossReason::UnavailableExternal && n.name == key);

    // Missing next to the document: a typed loss at open, whatever health the record carried.
    let s = open();
    assert_eq!(state(&s), Some((LinkState::Missing, Some("0190f5c2-7a1b-7c3d-8e4f-000000000abc".to_string()))));
    assert!(lost(&s));
    // Present with the recorded hash: resolved, no loss.
    std::fs::write(dir.join("links/source.psd"), &source_bytes).unwrap();
    let s = open();
    assert_eq!(state(&s).map(|x| x.0), Some(LinkState::Resolved));
    assert!(!lost(&s));
    // Edited since: modified, a typed loss with the relink data.
    std::fs::write(dir.join("links/source.psd"), b"edited").unwrap();
    let s = open();
    assert_eq!(state(&s).map(|x| x.0), Some(LinkState::Modified));
    assert!(lost(&s));
    let _ = std::fs::remove_dir_all(&dir);
}
