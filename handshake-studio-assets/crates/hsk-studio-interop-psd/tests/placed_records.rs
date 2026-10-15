//! Lane SMB-PS-ADJUST-STYLES-2: PSD smart objects (`SoLd` / `SoLE`, linked files `lnk2` / `lnkE`)
//! mapped to Folio placed-asset records (shared `placed_sources` plus a `placed_asset` instance on
//! the raster layer, whose tiles stay the cached display pixels). Two tests: Photoshop-saved corpus
//! files (psd-tools, pinned by byte length; NOT_PROVEN when absent) and hand-built hostile records.
//! These prove the byte mapping only; Photoshop display parity is UNVERIFIED (D5).
mod common;

use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    ADJUSTMENT_SCHEMA, Budget, Resolution, Resolver, StudioLinkHealth, StudioPlacedMode, TileRef,
    validate_raster_records,
};
use hsk_studio_interop_psd::*;

struct Mint(u64, bool);
impl Mint {
    fn next(&mut self, prefix: &str) -> String {
        self.0 += 1;
        format!("{prefix}-019abcde-0000-7000-8000-{:012x}", self.0)
    }
}
impl IdMint for Mint {
    fn document_id(&mut self) -> String {
        self.next("SDOC")
    }
    fn artboard_id(&mut self) -> String {
        self.next("SART")
    }
    fn layer_id(&mut self) -> String {
        self.next("SLYR")
    }
    fn linear_working_profile_id(&mut self) -> String {
        self.next("SCPF")
    }
    fn tile_manifest_id(&mut self, layer_id: &str, object_key: &str) -> String {
        format!("manifest/{layer_id}/{object_key}")
    }
    fn effect_stack_id(&mut self, _layer_id: &str) -> Option<String> {
        Some(self.next("SEFX"))
    }
    fn placed_document_id(&mut self, _unique_id: &str) -> Option<String> {
        self.1.then(|| self.next("SDOC"))
    }
}
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
    fn primitive_layer_binding(&self, kind: &str, schema_id: &str) -> bool {
        kind == "adjustment" && schema_id == ADJUSTMENT_SCHEMA
    }
}
fn budget() -> Budget {
    Budget { input_bytes: 8 << 20, payload_bytes: 4 << 20, ..Budget::default() }
}
fn proposal(bytes: &[u8], mint_sources: bool) -> ImportProposal {
    let limits = Limits::default();
    let doc = read(bytes);
    let cfg = ProposalConfig::new(&limits, budget(), &Host);
    let p = to_proposal(&doc, &SourceIdentity::of(bytes), &cfg, &mut Mint(0, mint_sources), None, &CancellationToken::default())
        .expect("proposal");
    let issues = validate_raster_records(&p.raster_records, p.snapshot.document(), budget(), &CancellationToken::default(), &Host)
        .expect("raster records validate");
    assert!(issues.is_empty());
    p
}
/// A 2 x 2 RGBA raster layer carrying `blocks`, in a document with the global `globals`.
fn placed_doc(blocks: Vec<Vec<u8>>, globals: &[Vec<u8>]) -> Vec<u8> {
    let mut l = TLayer::new("so", [0, 0, 2, 2]);
    l.channels = vec![(-1, 0, vec![255; 4]), (0, 0, vec![10; 4]), (1, 0, vec![20; 4]), (2, 0, vec![30; 4])];
    l.blocks = blocks;
    let info = layer_info(&[l], false, false);
    Doc {
        version: 1,
        channels: 3,
        height: 2,
        width: 2,
        depth: 8,
        mode: 3,
        color_mode_data: Vec::new(),
        resources: Vec::new(),
        layer_section: layer_section(&info, globals, false),
        image: Some((0, vec![1; 12])),
    }
    .bytes()
}
fn codes(p: &ImportProposal, key: &str) -> Vec<&'static str> {
    let path = format!("layer[0]/tagged/{key}");
    p.loss.entries.iter().filter(|e| e.path == path).map(|e| e.code).collect()
}

// Photoshop descriptor builders (independent of the crate's reader; as in adjustment_records.rs).
fn ds_key(k: &str) -> Vec<u8> {
    let mut v = if k.len() == 4 { be32(0) } else { be32(k.len() as u32) };
    v.extend_from_slice(k.as_bytes());
    v
}
fn ds_object(class: &str, items: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    let mut v = be32(0); // empty Unicode class name
    v.extend(ds_key(class));
    v.extend(be32(items.len() as u32));
    for (key, tagged) in items {
        v.extend(ds_key(key));
        v.extend(tagged);
    }
    v
}
fn v_enum(type_id: &str, value: &str) -> Vec<u8> {
    let mut v = b"enum".to_vec();
    v.extend(ds_key(type_id));
    v.extend(ds_key(value));
    v
}
fn v_text(s: &str) -> Vec<u8> {
    let mut v = b"TEXT".to_vec();
    let units: Vec<u16> = s.encode_utf16().chain([0]).collect();
    v.extend(be32(units.len() as u32));
    for u in units {
        v.extend(be16(u));
    }
    v
}
fn v_doubles(xs: &[f64]) -> Vec<u8> {
    let mut v = b"VlLs".to_vec();
    v.extend(be32(xs.len() as u32));
    for x in xs {
        v.extend_from_slice(b"doub");
        v.extend(x.to_be_bytes());
    }
    v
}
fn v_object(class: &str, items: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    let mut v = b"Objc".to_vec();
    v.extend(ds_object(class, items));
    v
}
fn v_double(x: f64) -> Vec<u8> {
    let mut v = b"doub".to_vec();
    v.extend(x.to_be_bytes());
    v
}
/// A `SoLd` block: `soLD`, version 4, the version-16 descriptor.
fn sold(id: &str, quad: [f64; 8], size: (f64, f64), warp: &str) -> Vec<u8> {
    let mut v = b"soLD".to_vec();
    v.extend(be32(4));
    v.extend(be32(16));
    v.extend(ds_object(
        "null",
        vec![
            ("Idnt", v_text(id)),
            ("Trnf", v_doubles(&quad)),
            ("nonAffineTransform", v_doubles(&quad)),
            ("warp", v_object("warp", vec![("warpStyle", v_enum("warpStyle", warp))])),
            ("Sz  ", v_object("Pnt ", vec![("Wdth", v_double(size.0)), ("Hght", v_double(size.1))])),
        ],
    ));
    v
}
/// One linked-file item (`kind`, version 7, unique id, name, payload) with its u64 length, padded.
fn link_item(kind: &[u8; 4], id: &str, name: &str, payload: &[u8]) -> Vec<u8> {
    let mut item = kind.to_vec();
    item.extend(be32(7));
    item.push(id.len() as u8);
    item.extend_from_slice(id.as_bytes());
    let units: Vec<u16> = name.encode_utf16().chain([0]).collect();
    item.extend(be32(units.len() as u32));
    for u in units {
        item.extend(be16(u));
    }
    item.extend_from_slice(b"png ");
    item.extend([0; 4]);
    item.extend((payload.len() as u64).to_be_bytes());
    item.push(0); // no open descriptor
    item.extend_from_slice(payload);
    let mut v = (item.len() as u64).to_be_bytes().to_vec();
    v.extend(item);
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
    v
}

/// A corpus file through the one corpus-root convention (`.config/oracle-corpus.toml`).
fn corpus_file(relative: &str) -> std::result::Result<Vec<u8>, String> {
    let ws = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().map_err(|e| e.to_string())?;
    let config =
        std::fs::read_to_string(ws.join(".config/oracle-corpus.toml")).map_err(|e| format!("corpus_convention_absent error={e}"))?;
    let value = |key: &str| {
        config.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
        })
    };
    let search = value("search").ok_or("corpus_convention_malformed")?;
    let max: usize = value("max-ancestors").and_then(|v| v.parse().ok()).ok_or("corpus_convention_malformed")?;
    let root = ws.ancestors().take(max + 1).map(|a| a.join(&search)).find(|c| c.is_dir()).ok_or("corpus_root_absent")?;
    std::fs::read(root.join(relative)).map_err(|e| format!("file_absent error={e}"))
}



fn sha256_label(bytes: &[u8]) -> String {
    format!("sha256-{}", hsk_studio_prism::hash_hex(&hsk_studio_prism::profile_hash(bytes)))
}

#[test]
fn photoshop_saved_smart_objects_map_to_placed_records() {
    let read = |name: &str, len: usize| match corpus_file(&format!("psd-tools/psd_files/{name}")) {
        Ok(b) if b.len() == len => Some(b),
        Ok(b) => {
            println!("NOT_PROVEN: photoshop_saved_smart_objects_map_to_placed_records {name} reason=length {} != {len}", b.len());
            None
        }
        Err(reason) => {
            println!("NOT_PROVEN: photoshop_saved_smart_objects_map_to_placed_records {name} reason={reason}");
            None
        }
    };
    // layers/smartobject-layer.psd (bytes inspected 2026-10-15): SoLd v4, Idnt 903957cc-...,
    // Trnf (0, 0) (32, 0) (32, 32) (0, 32), Sz 32 x 32; lnk2 holds one liFD v7 item, "A.png", a
    // 378-byte PNG. The instance is the identity; the source is embedded at the PNG's SHA-256.
    if let Some(bytes) = read("layers/smartobject-layer.psd", 26_417) {
        let png_at = bytes.windows(8).position(|w| w == b"\x89PNG\r\n\x1a\n").expect("embedded png");
        let digest = sha256_label(&bytes[png_at..png_at + 378]);
        let p = proposal(&bytes, true);
        let r = &p.raster_records;
        assert_eq!(r.placed_sources.len(), 1);
        let s = &r.placed_sources[0];
        assert_eq!((s.source_id.as_str(), s.width, s.height), ("psd-so-903957cc-082d-464a-b13e-d55023442674", 32.0, 32.0));
        assert!(matches!(&s.location, StudioPlacedMode::Embedded { content_digest, .. } if *content_digest == digest), "{:?}", s.location);
        let a = r.layers.iter().find_map(|l| l.placed_asset.clone()).expect("placed instance");
        assert_eq!((a.source_id.as_str(), a.transform), (s.source_id.as_str(), [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]));
        // A host that mints no source documents: the layer keeps its cached pixels, typed loss.
        let p = proposal(&bytes, false);
        assert!(p.raster_records.placed_sources.is_empty() && p.raster_records.layers.iter().all(|l| l.placed_asset.is_none()));
        assert!(p.loss.has_code("placed_source_document_unminted"), "{:?}", p.loss.entries);
    }
    // placedLayer.psd: two SoLE layers on external files (lnkE liFE: "linked-layer.png" at
    // (96, 96) size 64, "1layer.psd" at (27, 73) size 101 x 55 scaled by 2) and one SoLd on an
    // embedded copy (lnk2 liFD, 17272 bytes) at (96, 96) size 64.
    if let Some(bytes) = read("placedLayer.psd", 114_796) {
        let p = proposal(&bytes, true);
        let r = &p.raster_records;
        let instance = |source: &hsk_studio_folio::StudioPlacedSource| {
            r.layers.iter().find_map(|l| l.placed_asset.clone().filter(|a| a.source_id == source.source_id)).expect("instance").transform
        };
        let mut seen = 0;
        for s in &r.placed_sources {
            match &s.location {
                StudioPlacedMode::Linked { uri, health, .. } if uri == "linked-layer.png" => {
                    assert_eq!(*health, StudioLinkHealth::Missing);
                    assert_eq!((s.width, s.height, instance(s)), (64.0, 64.0, [1.0, 0.0, 0.0, 1.0, 96.0, 96.0]));
                }
                StudioPlacedMode::Linked { uri, .. } if uri == "1layer.psd" => {
                    assert_eq!((s.width, s.height, instance(s)), (101.0, 55.0, [2.0, 0.0, 0.0, 2.0, 27.0, 73.0]));
                }
                StudioPlacedMode::Embedded { .. } => {
                    assert_eq!((s.width, s.height, instance(s)), (64.0, 64.0, [1.0, 0.0, 0.0, 1.0, 96.0, 96.0]));
                }
                other => panic!("{other:?}"),
            }
            seen += 1;
        }
        assert_eq!(seen, 3, "{:?}", p.loss.entries);
        assert!(p.loss.has_code("placed_link_health_not_checked"));
    }
    // smart-object-slice.psd: a v2 liFD item of 1 MB embedded; the layer at (19, 28) size 22 x 20.
    if let Some(bytes) = read("smart-object-slice.psd", 1_081_897) {
        let p = proposal(&bytes, true);
        let a = p.raster_records.layers.iter().find_map(|l| l.placed_asset.clone()).expect("placed instance");
        assert_eq!(a.transform, [1.0, 0.0, 0.0, 1.0, 19.0, 28.0]);
    }
}

#[test]
fn hand_built_smart_objects_map_and_hostile_ones_are_typed() {
    let id = "11111111-2222-3333-4444-555555555555";
    let png = b"not really a png".to_vec();
    let lnk2 = global_block(b"lnk2", &link_item(b"liFD", id, "a.png", &png), false);
    // A rotated, scaled instance: corners of a 10 x 5 source at (4, 1) along (0, 6) and (-2.5, 0).
    let quad = [4.0, 1.0, 4.0, 7.0, 1.5, 7.0, 1.5, 1.0];
    let p = proposal(&placed_doc(vec![layer_block(b"SoLd", &sold(id, quad, (10.0, 5.0), "warpNone"), false)], std::slice::from_ref(&lnk2)), true);
    let s = &p.raster_records.placed_sources[0];
    assert!(matches!(&s.location, StudioPlacedMode::Embedded { content_digest, .. } if *content_digest == sha256_label(&png)));
    let a = p.raster_records.layers[0].placed_asset.clone().expect("instance");
    assert_eq!(a.transform, [0.0, 0.6, -0.5, 0.0, 4.0, 1.0]);
    // Typed losses (no placed record, the layer keeps its pixels): a quad that is not a
    // parallelogram, a warp, a missing source, a zero size; and a hostile linked-file block.
    let refused = |blocks: Vec<Vec<u8>>, globals: &[Vec<u8>], key: &str, code: &str| {
        let p = proposal(&placed_doc(blocks, globals), true);
        assert!(p.raster_records.layers.iter().all(|l| l.placed_asset.is_none()), "{code}");
        assert!(p.raster_records.placed_sources.is_empty(), "{code}");
        let got = if key.is_empty() { p.loss.entries.iter().map(|e| e.code).collect() } else { codes(&p, key) };
        assert!(got.contains(&code), "{code}: {:?}", p.loss.entries);
    };
    let skew = [0.0, 0.0, 10.0, 0.0, 12.0, 5.0, 0.0, 5.0];
    refused(vec![layer_block(b"SoLd", &sold(id, skew, (10.0, 5.0), "warpNone"), false)], std::slice::from_ref(&lnk2), "SoLd", "placed_transform_non_affine");
    refused(vec![layer_block(b"SoLd", &sold(id, quad, (10.0, 5.0), "warpArc"), false)], std::slice::from_ref(&lnk2), "SoLd", "placed_warp_unmapped");
    refused(vec![layer_block(b"SoLd", &sold("someone-else", quad, (10.0, 5.0), "warpNone"), false)], std::slice::from_ref(&lnk2), "SoLd", "placed_source_missing");
    refused(vec![layer_block(b"SoLd", &sold(id, quad, (0.0, 5.0), "warpNone"), false)], std::slice::from_ref(&lnk2), "SoLd", "placed_layer_size_missing");
    refused(vec![layer_block(b"SoLd", &sold(id, [0.0; 8], (10.0, 5.0), "warpNone"), false)], std::slice::from_ref(&lnk2), "SoLd", "placed_transform_singular");
    // An item length far beyond the block (u64::MAX) is refused before any slice is taken.
    let mut hostile = u64::MAX.to_be_bytes().to_vec();
    hostile.extend_from_slice(b"liFD");
    let sold_ok = || vec![layer_block(b"SoLd", &sold(id, quad, (10.0, 5.0), "warpNone"), false)];
    refused(sold_ok(), &[global_block(b"lnk2", &hostile, false)], "", "linked_file_truncated");
    // More items than the bound: refused as a count, not walked further.
    let tiny = link_item(b"liFA", "x", "", &[]);
    refused(sold_ok(), &[global_block(b"lnk2", &tiny.repeat(4097), false)], "", "linked_file_count");
    // An alias item is a typed loss on the layer.
    refused(sold_ok(), &[global_block(b"lnk2", &link_item(b"liFA", id, "a", &[]), false)], "SoLd", "placed_link_alias_unmapped");
}
