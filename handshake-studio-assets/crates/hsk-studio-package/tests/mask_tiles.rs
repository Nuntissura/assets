//! SMB-PACKAGE-MASK-TILES: a package carries every tile plane its document and its typed import
//! records reference (raster tiles, layer mask coverage, smart-filter entry and stack masks) and
//! every records image asset, byte-identically through the buffered and the streaming writers; a
//! package written before this fix (mask assets dropped) opens with a typed loss per missing plane.

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

fn tile(key: &str, format: &str, digest: &str) -> Value {
    json!({"object_key":key,"layer_id":id("SLYR",1),"column":0,"row":0,"width":{"value":4,"unit":"px"},"height":{"value":4,"unit":"px"},"format":format,"colour_profile_id":id("SCPF",1),"schema_id":"hsk.studio.raster_tile@1","content_digest":{"algorithm":"sha256","digest":digest},"artifact_manifest_id":"caller-owned-existing-handle"})
}

/// Every distinct plane of the fixture, by role.
struct Planes {
    raster: Vec<u8>,
    layer_mask: Vec<u8>,
    stack_mask: Vec<u8>,
    filter_mask: Vec<u8>,
    image: Vec<u8>,
}

fn planes() -> Planes {
    let fill = |seed: u8, n: usize| (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect::<Vec<u8>>();
    Planes {
        raster: fill(1, 4 * 4 * 4),
        layer_mask: fill(2, 16),
        stack_mask: fill(3, 32),
        filter_mask: fill(4, 16),
        image: fill(5, 100),
    }
}

fn hex(bytes: &[u8]) -> String {
    sha256_hex_of(bytes)
}

/// One raster layer with a raster tile and a layer (appearance) mask.
fn document(p: &Planes) -> folio::Snapshot {
    let doc = json!({
        "schema_id":"hsk.studio.document@1","document_id":id("SDOC",1),"revision":3,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[{"schema_id":"hsk.studio.artboard@1","artboard_id":id("SART",1),"name":"canvas","parent":null,"bounds":{"x":{"value":0,"unit":"px"},"y":{"value":0,"unit":"px"},"width":{"value":4,"unit":"px"},"height":{"value":4,"unit":"px"}}}],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":id("SLYR",1),"name":"Masked","parent":null,"kind":"raster",
            "payload":{"payload_kind":"raster","tiles":[tile("t0","rgba8",&hex(&p.raster))]},"visible":true,
            "appearance":{"opacity":100.0,"fill_opacity":100.0,"blend_mode":2,"clipped":false,"isolation":"isolated",
                "mask":{"tiles":[tile("m0","coverage_u8",&hex(&p.layer_mask))],"enabled":true,"linked":true}}}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[{"layer_id":id("SLYR",1),"operation_key":"source","operation_kind":"source","inputs":[],"outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"source"}]}],"edges":[],"outputs":[{"layer_id":id("SLYR",1),"operation_key":"source","port_key":"out"}]}
    });
    match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), folio::Budget::default(), &token(), &Granted).unwrap() {
        folio::Inspection::Editable(s) => s,
        folio::Inspection::ReadOnly(_) => panic!("fixture document became read-only"),
    }
}

/// Raster records of the layer: a smart-filter stack with a stack mask and one per-filter mask.
/// Validated by Folio so the fixture is a real record shape, not a package-local guess.
fn raster_records(p: &Planes, snapshot: &folio::Snapshot) -> Value {
    let records = json!({"schema_id":"hsk.studio.raster_records@1","document_id":id("SDOC",1),"layers":[{"layer_id":id("SLYR",1),
        "smart_filters":{"enabled":true,
            "stack_mask":{"tiles":[tile("s0","coverage_u16le",&hex(&p.stack_mask))],"enabled":true,"linked":true},
            "filters":[{"instance_id":"blur","enabled":true,"opacity":100.0,
                "mask":{"tiles":[tile("f0","coverage_u8",&hex(&p.filter_mask))],"enabled":true,"linked":false},
                "params":{"filter_kind":"gaussian_blur","radius_px":1.5}}]}}]});
    let typed: folio::StudioRasterRecords = serde_json::from_value(records.clone()).unwrap();
    folio::validate_raster_records(&typed, snapshot.document(), folio::Budget::default(), &token(), &Granted)
        .expect("fixture raster records are valid Folio records");
    records
}

/// A records bundle in the shape the native `.fig` importer writes: the raster records nested
/// one level down, plus a records image paint naming a package asset.
fn bundle(p: &Planes, snapshot: &folio::Snapshot) -> Value {
    json!({"records":"fig_native","records_version":1,"raster_records":raster_records(p, snapshot),
        "vector_records":{"image_paints":[{"style_id":id("SSTY",1),"row":0,
            "image":{"source_kind":"package_asset","content_digest":{"algorithm":"sha256","digest":hex(&p.image)}}}]}})
}

fn source(p: &Planes) -> MemoryAssetSource {
    let mut assets = MemoryAssetSource::new();
    for bytes in [&p.raster, &p.layer_mask, &p.stack_mask, &p.filter_mask, &p.image] {
        assets.insert(bytes.clone());
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

fn open(bytes: &[u8]) -> Result<Opened, PackageError> {
    read_package(bytes, &Limits::default(), folio::Budget::default(), &Granted, &token())
}

fn expect_planes(opened: &Opened, wanted: &[&[u8]]) {
    for bytes in wanted {
        assert_eq!(
            opened.assets.get(&hex(bytes)).map(Vec::as_slice),
            Some(*bytes),
            "plane {} reopens byte-identical",
            hex(bytes)
        );
    }
}

#[test]
fn every_tile_plane_and_records_asset_round_trips_byte_identically() {
    let p = planes();
    let snapshot = document(&p);
    let assets = source(&p);
    for options in [WriteOptions::default(), WriteOptions { asset_codec: Codec::Zstd, ..WriteOptions::default() }] {
        // Plain Folio raster records as the records entry (the PSD importer's shape).
        let records = ImportEntry::new(ImportRole::Records, serde_json::to_vec(&raster_records(&p, &snapshot)).unwrap());
        let package = write_both(&snapshot, &assets, &[records.to_opaque()], &options);
        let opened = open(&package).unwrap();
        expect_planes(&opened, &[&p.raster, &p.layer_mask, &p.stack_mask, &p.filter_mask]);
        assert!(opened.loss.is_empty(), "nothing lost: {:?}", opened.loss);
        assert_eq!(opened.import_bytes(ImportRole::Records), Some(records.bytes.as_slice()));

        // The `.fig` bundle shape: nested raster records and a package-asset image paint.
        let records = ImportEntry::new(ImportRole::Records, serde_json::to_vec(&bundle(&p, &snapshot)).unwrap());
        let package = write_both(&snapshot, &assets, &[records.to_opaque()], &options);
        let opened = open(&package).unwrap();
        expect_planes(&opened, &[&p.raster, &p.layer_mask, &p.stack_mask, &p.filter_mask, &p.image]);
        assert!(opened.loss.is_empty(), "nothing lost: {:?}", opened.loss);

        // Re-save of the reopened package (records carried verbatim) keeps every plane.
        let again = write_both(
            opened.snapshot().unwrap(),
            &{
                let mut m = MemoryAssetSource::new();
                for bytes in opened.assets.values() {
                    m.insert(bytes.clone());
                }
                m
            },
            &opened.quarantined,
            &options,
        );
        assert_eq!(again, package, "re-save of the reopened package is byte-identical");
    }
}

#[test]
fn a_referenced_plane_the_source_cannot_supply_fails_the_save_typed() {
    let p = planes();
    let snapshot = document(&p);
    let records = ImportEntry::new(ImportRole::Records, serde_json::to_vec(&raster_records(&p, &snapshot)).unwrap());
    let held = [records.to_opaque()];
    let extras = Extras { quarantined: &held, previews: &[] };
    // Every plane but the smart-filter stack mask.
    let mut assets = MemoryAssetSource::new();
    for bytes in [&p.raster, &p.layer_mask, &p.filter_mask] {
        assets.insert(bytes.clone());
    }
    let mut out = Vec::new();
    let err = write_package_with(&snapshot, &assets, &extras, &WriteOptions::default(), &Limits::default(), &token(), &mut out)
        .unwrap_err();
    assert_eq!(err.code(), "asset_absent");
    let mut streamed = Cursor::new(Vec::new());
    let err = write_package_streaming(&snapshot, &assets, &extras, &WriteOptions::default(), &Limits::default(), &token(), &mut streamed)
        .unwrap_err();
    assert_eq!(err.code(), "asset_absent");
}

/// STORE-only ZIP of `(name, bytes)` entries (the canonical form the reader accepts).
fn store_zip(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let (mut out, mut cd) = (Vec::new(), Vec::new());
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crc32_of(data);
        for local in [true, false] {
            let buf = if local { &mut out } else { &mut cd };
            buf.extend_from_slice(&(if local { 0x0403_4b50u32 } else { 0x0201_4b50u32 }).to_le_bytes());
            if !local {
                buf.extend_from_slice(&45u16.to_le_bytes());
            }
            buf.extend_from_slice(&20u16.to_le_bytes());
            buf.extend_from_slice(&0x0800u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&0x0021u16.to_le_bytes());
            buf.extend_from_slice(&crc.to_le_bytes());
            buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
            buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
            buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            if !local {
                buf.extend_from_slice(&[0u8; 4]);
                buf.extend_from_slice(&0u16.to_le_bytes());
                buf.extend_from_slice(&0u32.to_le_bytes());
                buf.extend_from_slice(&offset.to_le_bytes());
            }
            buf.extend_from_slice(name.as_bytes());
            if local {
                buf.extend_from_slice(data);
            }
        }
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// The package a writer from before SMB-PACKAGE-MASK-TILES produced for the same document: it
/// wrote raster payload tiles only, so every other `assets/<hex>` entry is removed from the archive
/// and the manifest (the manifest stays canonical: entries keep their order).
fn legacy_of(package: &[u8], raster: &[&[u8]]) -> Vec<u8> {
    let keep: Vec<String> = raster.iter().map(|b| format!("assets/{}", hex(b))).collect();
    let metas = preflight(package, &Limits::default(), &token()).unwrap();
    let mut entries: Vec<(String, Vec<u8>)> =
        metas.iter().map(|m| (m.name.clone(), m.data(package).to_vec())).collect();
    entries.retain(|(n, _)| !n.starts_with("assets/") || keep.contains(n));
    let slot = entries.iter().position(|(n, _)| n == MANIFEST_NAME).unwrap();
    let mut manifest = Manifest::parse(&entries[slot].1, &Limits::default()).unwrap();
    manifest.entries.retain(|e| e.kind != EntryKind::Asset || keep.contains(&e.name));
    entries[slot].1 = manifest.to_canonical_bytes().unwrap();
    store_zip(&entries)
}

#[test]
fn a_package_written_before_the_fix_opens_with_a_typed_loss_per_missing_plane() {
    let p = planes();
    let snapshot = document(&p);
    let records = ImportEntry::new(ImportRole::Records, serde_json::to_vec(&raster_records(&p, &snapshot)).unwrap());
    let full = write_both(&snapshot, &source(&p), &[records.to_opaque()], &WriteOptions::default());
    let legacy = legacy_of(&full, &[&p.raster]);
    let opened = open(&legacy).expect("a legacy masked package still opens");
    expect_planes(&opened, &[&p.raster]);
    let missing: Vec<(&str, LossReason)> = opened.loss.iter().map(|n| (n.name.as_str(), n.reason)).collect();
    let mut wanted = [
        (format!("assets/{}", hex(&p.layer_mask)), LossReason::MissingAsset),
        (format!("assets/{}", hex(&p.stack_mask)), LossReason::MissingAsset),
        (format!("assets/{}", hex(&p.filter_mask)), LossReason::MissingAsset),
    ];
    wanted.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(missing, wanted.iter().map(|(n, r)| (n.as_str(), *r)).collect::<Vec<_>>());
}

#[test]
fn the_collector_classifies_every_plane_and_refuses_hostile_records_typed() {
    let p = planes();
    let snapshot = document(&p);
    let records = ImportEntry::new(ImportRole::Records, serde_json::to_vec(&bundle(&p, &snapshot)).unwrap());
    let refs = referenced_assets(snapshot.document(), &[records.to_opaque()]).unwrap();
    let uses = |b: &[u8]| refs.get(&hex(b)).map(|r| (r.uses.clone(), r.elem_size, r.required()));
    assert_eq!(uses(&p.raster), Some((vec![AssetUse::RasterTile], None, true)));
    assert_eq!(uses(&p.layer_mask), Some((vec![AssetUse::DocumentPlane], None, false)));
    assert_eq!(uses(&p.stack_mask), Some((vec![AssetUse::RecordPlane], Some(2), false)));
    assert_eq!(uses(&p.filter_mask), Some((vec![AssetUse::RecordPlane], None, false)));
    assert_eq!(uses(&p.image), Some((vec![AssetUse::RecordImage], None, false)));
    assert_eq!(refs.len(), 5);

    // Only `import/records.*` entries are records; other opaque entries are never interpreted.
    let foreign = Quarantined { name: "vendor/extra.json".into(), sha256_hex: hex(&records.bytes), bytes: records.bytes.clone() };
    assert_eq!(referenced_assets(snapshot.document(), &[foreign]).unwrap().len(), 2);

    let held = |bytes: Vec<u8>| ImportEntry::new(ImportRole::Records, bytes).to_opaque();
    // Records that are not JSON, or nest beyond the JSON parser's depth bound, are opaque to the
    // package: no planes, no crash.
    let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000)).into_bytes();
    for bytes in [b"\x00\xffnot json".to_vec(), deep] {
        assert_eq!(referenced_assets(snapshot.document(), &[held(bytes)]).unwrap().len(), 2);
    }
    // A tile-shaped or image-shaped records object that is not a valid reference is refused typed,
    // on write as well, never silently skipped; no partial output.
    let mut bad_tile = tile("x0", "coverage_u8", &hex(&p.stack_mask));
    bad_tile["content_digest"]["digest"] = json!("NOT-HEX");
    let mut extra_field = tile("x1", "coverage_u8", &hex(&p.stack_mask));
    extra_field["smuggled"] = json!(1);
    let mut wrong_algorithm = tile("x2", "coverage_u8", &hex(&p.stack_mask));
    wrong_algorithm["content_digest"]["algorithm"] = json!("md5");
    let bad_image = json!({"source_kind":"package_asset","content_digest":{"algorithm":"sha256","digest":"ab"}});
    for hostile in [bad_tile, extra_field, wrong_algorithm, bad_image] {
        let entry = held(serde_json::to_vec(&json!({"nested":[{"deeper":hostile}]})).unwrap());
        let err = referenced_assets(snapshot.document(), std::slice::from_ref(&entry)).unwrap_err();
        assert_eq!(err.code(), "invalid_digest");
        let extras = Extras { quarantined: std::slice::from_ref(&entry), previews: &[] };
        let mut out = Vec::new();
        let err = write_package_with(&snapshot, &source(&p), &extras, &WriteOptions::default(), &Limits::default(), &token(), &mut out)
            .unwrap_err();
        assert_eq!(err.code(), "invalid_digest");
        assert!(out.is_empty(), "no partial output");
    }
}

/// Every place a Folio wire schema can name content by digest, classified. The collector finds
/// tiles by their required `schema_id` token and package images by `source_kind`; this test ties
/// those discriminators to Folio's generated schemas and fails when a schema gains a digest-bearing
/// position nobody classified (a new tile-like or asset-like record kind): extend the collector
/// (`src/references.rs`) and this table together.
#[test]
fn every_folio_digest_position_is_classified() {
    let schemas: Vec<(&str, Value)> = vec![
        ("document", serde_json::to_value(folio::document_schema()).unwrap()),
        ("raster_records", serde_json::to_value(folio::raster_records_schema()).unwrap()),
        ("vector_records", serde_json::to_value(folio::vector_records_schema()).unwrap()),
        ("design_records", serde_json::to_value(folio::design_records_schema()).unwrap()),
        ("composition_records", serde_json::to_value(folio::composition_records_schema()).unwrap()),
        ("live_shapes", serde_json::to_value(folio::live_shapes_schema()).unwrap()),
        ("longdoc_records", serde_json::to_value(folio::longdoc_records_schema()).unwrap()),
        ("parent_page_records", serde_json::to_value(folio::parent_page_records_schema()).unwrap()),
        ("text_story", serde_json::to_value(folio::text_story_schema()).unwrap()),
        ("type_style_records", serde_json::to_value(folio::type_style_records_schema()).unwrap()),
        ("vector_network", serde_json::to_value(folio::vector_network_schema()).unwrap()),
        ("motion_records", serde_json::to_value(folio::motion_records_schema()).unwrap()),
        ("layout_flow_records", serde_json::to_value(folio::layout_flow_records_schema()).unwrap()),
        ("book", serde_json::to_value(folio::book_schema()).unwrap()),
        ("clip_finishing", serde_json::to_value(folio::clip_finishing_schema()).unwrap()),
        ("layout_grids", serde_json::to_value(folio::layout_grids_schema()).unwrap()),
    ];
    let mut found = std::collections::BTreeSet::new();
    for (schema_name, schema) in &schemas {
        let defs = schema.get("$defs").and_then(Value::as_object).cloned().unwrap_or_default();
        let mut roots: Vec<(String, &Value)> = vec![(format!("{schema_name}#root"), schema)];
        roots.extend(defs.iter().map(|(k, v)| (k.clone(), v)));
        for (def, body) in roots {
            let mut stack: Vec<&Value> = vec![body];
            while let Some(v) = stack.pop() {
                match v {
                    Value::Object(map) => {
                        if let Some(Value::Object(props)) = map.get("properties") {
                            let tag: Vec<String> = props
                                .iter()
                                .filter_map(|(k, p)| p.get("const").and_then(Value::as_str).map(|c| format!("{k}={c}")))
                                .collect();
                            for (prop, p) in props {
                                if p.to_string().contains("#/$defs/ContentDigest") {
                                    let tag = if tag.is_empty() { String::new() } else { format!("[{}]", tag.join(",")) };
                                    found.insert(format!("{def}.{prop}{tag}"));
                                }
                            }
                        }
                        if def.ends_with("#root") {
                            // The root's own `$defs` are walked as separate roots.
                            stack.extend(map.iter().filter(|(k, _)| k.as_str() != "$defs").map(|(_, v)| v));
                        } else {
                            stack.extend(map.values());
                        }
                    }
                    Value::Array(items) => stack.extend(items.iter()),
                    _ => {}
                }
            }
        }
        // The tile discriminator the collector relies on: required, and exactly the token.
        if let Some(tile) = defs.get("TileRef") {
            let required: Vec<&str> = tile["required"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
            assert!(required.contains(&"schema_id") && required.contains(&"content_digest"), "{schema_name}: TileRef requires schema_id");
            let token = &tile["properties"]["schema_id"];
            let token = token
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|r| r.rsplit('/').next())
                .map_or(token, |name| &defs[name]);
            assert_eq!(token.to_string().matches(TILE_SCHEMA_ID).count(), 1, "{schema_name}: TileRef schema_id is the tile token: {token}");
        }
    }
    let classified: [&str; 8] = [
        // Linked media (caller-owned manifest handle): kept as a link, embedded only under
        // EmbedExternal::Granted / All, reported with its link state (externals.rs).
        "ClipSource.content_digest[source_kind=media]",
        // A stored text stream "resolved by its owner": linked, like media (externals.rs).
        "StoryContent.content_digest[content_kind=stored]",
        // ICC profile bytes that travel with the document: embedded when granted, else a typed
        // loss (externals.rs, kind icc_profile).
        "StudioIccProfile.content_digest",
        // A part inside the `import/original.<sha256>` entry, carried verbatim with that entry.
        "StudioImageSource.content_digest[source_kind=import_original]",
        // Records image naming `assets/<sha256>`: collected (AssetUse::RecordImage) and written.
        "StudioImageSource.content_digest[source_kind=package_asset]",
        // The import-original entry's own digest (its name carries it): not an asset.
        "StudioImageSource.entry_digest[source_kind=import_original]",
        // A clip-finishing LUT resource (manifest handle + hash): linked, embedded under
        // Granted / All (externals.rs, kind colour_lookup).
        "StudioLutRef.content_digest",
        // Every tile plane: collected by its `schema_id` token and written; required on read for a
        // raster payload, else a typed loss.
        "TileRef.content_digest",
    ];
    let found: Vec<String> = found.into_iter().collect();
    assert_eq!(found, classified, "unclassified digest positions in Folio schemas");
}
