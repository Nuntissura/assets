//! Lane SMB-FOLIO-CHANNEL-RECORD (STU-RAS-022 "preserved in round-trippable interchange", rule 4b):
//! a document whose raster records hold persistent alpha and spot channels
//! (`StudioRasterRecords.channels`) carries every channel tile plane through the buffered and the
//! streaming package writers and the reader byte-identically. The channel tiles are Folio
//! `TileRef`s, so the package's one reference collector (`references.rs`) finds them with no
//! package source change.

use hsk_studio_accord::CancellationToken;
use hsk_studio_folio as folio;
use hsk_studio_package::*;
use serde_json::{Value, json};
use std::io::Cursor;

struct Granted;
impl folio::Resolver for Granted {
    fn primitive(&self, _: &str, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn profile(&self, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn tile(&self, _: &folio::TileRef) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}

fn token() -> CancellationToken {
    CancellationToken::default()
}
fn id(prefix: &str, n: u8) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn hex(bytes: &[u8]) -> String {
    sha256_hex_of(bytes)
}
fn tile(owner: &str, key: &str, column: i64, format: &str, bytes: &[u8]) -> Value {
    json!({"object_key":key,"layer_id":owner,"column":column,"row":0,"width":{"value":4,"unit":"px"},"height":{"value":4,"unit":"px"},
        "format":format,"colour_profile_id":id("SCPF",1),"schema_id":"hsk.studio.raster_tile@1",
        "content_digest":{"algorithm":"sha256","digest":hex(bytes)},"artifact_manifest_id":"caller-owned-existing-handle"})
}

/// One channel of the fixture: id, kind, depth, tile format and its two cells.
type Channel = (&'static str, &'static str, u8, &'static str, [Vec<u8>; 2]);
/// Every plane of the fixture: the layer's raster tile and two cells of each of three channels
/// (8-bit masked-area alpha, 16-bit selected-area alpha, 32-bit spot).
struct Planes {
    raster: Vec<u8>,
    channels: Vec<Channel>,
}
fn planes() -> Planes {
    let fill = |seed: u8, n: usize| (0..n).map(|i| (i as u8).wrapping_mul(37).wrapping_add(seed)).collect::<Vec<u8>>();
    Planes {
        raster: fill(1, 4 * 4 * 4),
        channels: vec![
            ("alpha-8", "masked_area_alpha", 8, "coverage_u8", [fill(2, 16), fill(3, 16)]),
            ("alpha-16", "selected_area_alpha", 16, "coverage_u16le", [fill(4, 32), fill(5, 32)]),
            ("spot-32", "spot_colour", 32, "coverage_f32le", [fill(6, 64), fill(7, 64)]),
        ],
    }
}

fn document(p: &Planes) -> folio::Snapshot {
    let doc = json!({
        "schema_id":"hsk.studio.document@1","document_id":id("SDOC",1),"revision":3,"geometry_unit":"px","colour_mode":2,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[{"schema_id":"hsk.studio.artboard@1","artboard_id":id("SART",1),"name":"canvas","parent":null,"bounds":{"x":{"value":0,"unit":"px"},"y":{"value":0,"unit":"px"},"width":{"value":8,"unit":"px"},"height":{"value":4,"unit":"px"}}}],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":id("SLYR",1),"name":"Pixels","parent":null,"kind":"raster",
            "payload":{"payload_kind":"raster","tiles":[tile(&id("SLYR",1),"t0",0,"rgba8",&p.raster)]},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[{"layer_id":id("SLYR",1),"operation_key":"source","operation_kind":"source","inputs":[],"outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"source"}]}],"edges":[],"outputs":[{"layer_id":id("SLYR",1),"operation_key":"source","port_key":"out"}]}
    });
    match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), folio::Budget::default(), &token(), &Granted).unwrap() {
        folio::Inspection::Editable(s) => s,
        folio::Inspection::ReadOnly(_) => panic!("fixture document became read-only"),
    }
}

/// Raster records holding the three channels (8 x 4 pixels, two 4-pixel cells each), validated by
/// Folio so the fixture is the real record shape.
fn raster_records(p: &Planes, snapshot: &folio::Snapshot) -> Value {
    let channels: Vec<Value> = p
        .channels
        .iter()
        .map(|(cid, kind, depth, format, cells)| {
            json!({"channel_id":cid,"name":format!("Channel {cid}"),"kind":kind,"visible":false,
                "colour":{"model":"rgb","r":1.0,"g":0.0,"b":0.0},"opacity":50.0,"depth":depth,"width":8,"height":4,
                "tiles":[tile(cid,"c0",0,format,&cells[0]),tile(cid,"c1",1,format,&cells[1])]})
        })
        .collect();
    let records = json!({"schema_id":"hsk.studio.raster_records@1","document_id":id("SDOC",1),"channels":channels});
    let typed: folio::StudioRasterRecords = serde_json::from_value(records.clone()).unwrap();
    assert_eq!(typed.channels.len(), 3);
    folio::validate_raster_records(&typed, snapshot.document(), folio::Budget::default(), &token(), &Granted)
        .expect("fixture raster records are valid Folio records");
    records
}

fn source(p: &Planes, skip: Option<&[u8]>) -> MemoryAssetSource {
    let mut assets = MemoryAssetSource::new();
    assets.insert(p.raster.clone());
    for (.., cells) in &p.channels {
        for cell in cells {
            if Some(cell.as_slice()) != skip {
                assets.insert(cell.clone());
            }
        }
    }
    assets
}

fn write_both(snapshot: &folio::Snapshot, assets: &MemoryAssetSource, held: &[Quarantined], options: &WriteOptions) -> Vec<u8> {
    let extras = Extras { quarantined: held, previews: &[] };
    let mut buffered = Vec::new();
    write_package_with(snapshot, assets, &extras, options, &Limits::default(), &token(), &mut buffered).unwrap();
    let mut streamed = Cursor::new(Vec::new());
    write_package_streaming(snapshot, assets, &extras, options, &Limits::default(), &token(), &mut streamed).unwrap();
    assert_eq!(streamed.into_inner(), buffered, "streaming and buffered writers agree byte for byte");
    buffered
}

#[test]
fn channel_tile_planes_round_trip_byte_identically() {
    let p = planes();
    let snapshot = document(&p);
    let assets = source(&p, None);
    for options in [WriteOptions::default(), WriteOptions { asset_codec: Codec::Zstd, ..WriteOptions::default() }] {
        let records = ImportEntry::new(ImportRole::Records, serde_json::to_vec(&raster_records(&p, &snapshot)).unwrap());
        let package = write_both(&snapshot, &assets, &[records.to_opaque()], &options);
        let opened = read_package(&package, &Limits::default(), folio::Budget::default(), &Granted, &token()).unwrap();
        for (cid, .., cells) in &p.channels {
            for cell in cells {
                assert_eq!(opened.assets.get(&hex(cell)).map(Vec::as_slice), Some(cell.as_slice()), "{cid} cell reopens byte-identical");
            }
        }
        assert!(opened.loss.is_empty(), "nothing lost: {:?}", opened.loss);
        assert_eq!(opened.import_bytes(ImportRole::Records), Some(records.bytes.as_slice()), "records carried verbatim");
        // The records still decode to the same channels after the round trip.
        let back: folio::StudioRasterRecords = serde_json::from_slice(opened.import_bytes(ImportRole::Records).unwrap()).unwrap();
        let want: folio::StudioRasterRecords = serde_json::from_value(raster_records(&p, &snapshot)).unwrap();
        assert_eq!(back, want);
        // Re-save of the reopened package keeps every plane.
        let mut again_assets = MemoryAssetSource::new();
        for bytes in opened.assets.values() {
            again_assets.insert(bytes.clone());
        }
        let again = write_both(opened.snapshot().unwrap(), &again_assets, &opened.quarantined, &options);
        assert_eq!(again, package, "re-save of the reopened package is byte-identical");
    }
}

#[test]
fn a_channel_plane_the_source_cannot_supply_fails_the_save_typed() {
    let p = planes();
    let snapshot = document(&p);
    let records = ImportEntry::new(ImportRole::Records, serde_json::to_vec(&raster_records(&p, &snapshot)).unwrap());
    let held = [records.to_opaque()];
    let extras = Extras { quarantined: &held, previews: &[] };
    let missing = p.channels[2].4[1].clone();
    let assets = source(&p, Some(&missing));
    let mut out = Vec::new();
    let err = write_package_with(&snapshot, &assets, &extras, &WriteOptions::default(), &Limits::default(), &token(), &mut out).unwrap_err();
    assert_eq!(err.code(), "asset_absent");
    let mut streamed = Cursor::new(Vec::new());
    let err = write_package_streaming(&snapshot, &assets, &extras, &WriteOptions::default(), &Limits::default(), &token(), &mut streamed)
        .unwrap_err();
    assert_eq!(err.code(), "asset_absent");
}
