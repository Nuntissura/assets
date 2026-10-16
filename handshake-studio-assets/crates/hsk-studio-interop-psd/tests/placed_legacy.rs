//! Lane SMB-PS-ADJUST-STYLES-3: a legacy placed layer (`PlLd` without `SoLd`) recovered into the
//! Folio placed-asset records (STU-RAS-012..014) when its data allows it: the source size from the
//! warp `bounds`, else from the embedded file's PNG or PSD header; otherwise a typed loss naming why
//! the size is unknown. Two tests: the `PlLd` blocks Photoshop wrote beside `SoLd` in the psd-tools
//! corpus, read with `SoLd` taken out (pinned by byte length; NOT_PROVEN when absent), and
//! hand-built hostile blocks. Photoshop display parity is UNVERIFIED (D5).
mod common;

use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{ADJUSTMENT_SCHEMA, Budget, Resolution, Resolver, StudioPlacedMode, TileRef, validate_raster_records};
use hsk_studio_interop_psd::*;

struct Mint(u64);
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
        Some(self.next("SDOC"))
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
fn proposal_of(doc: &PsdDocument, bytes: &[u8]) -> ImportProposal {
    let limits = Limits::default();
    let cfg = ProposalConfig::new(&limits, budget(), &Host);
    let p = to_proposal(doc, &SourceIdentity::of(bytes), &cfg, &mut Mint(0), None, &CancellationToken::default()).expect("proposal");
    let issues = validate_raster_records(&p.raster_records, p.snapshot.document(), budget(), &CancellationToken::default(), &Host)
        .expect("raster records validate");
    assert!(issues.is_empty());
    p
}
/// The document of `bytes` with every `SoLd` / `SoLE` block taken out: the `PlLd` beside it is
/// then the layer's only placed data.
fn legacy_only(bytes: &[u8]) -> PsdDocument {
    let mut doc = read(bytes);
    let info = doc.layer_section.layer_info.as_mut().expect("layers");
    for l in &mut info.layers {
        l.blocks.retain(|b| !matches!(&b.key, b"SoLd" | b"SoLE"));
    }
    doc
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

#[test]
fn photoshop_written_pllp_alone_recovers_the_placed_record() {
    let read_corpus = |name: &str, len: usize| match corpus_file(&format!("psd-tools/psd_files/{name}")) {
        Ok(b) if b.len() == len => Some(b),
        Ok(b) => {
            println!("NOT_PROVEN: photoshop_written_pllp_alone_recovers_the_placed_record {name} reason=length {} != {len}", b.len());
            None
        }
        Err(reason) => {
            println!("NOT_PROVEN: photoshop_written_pllp_alone_recovers_the_placed_record {name} reason={reason}");
            None
        }
    };
    // layers/smartobject-layer.psd: PlLd `plcL` v3, id 903957cc-..., quad (0, 0) (32, 0) (32, 32)
    // (0, 32), warpNone with bounds 0, 0, 32, 32 (bytes inspected 2026-10-16): the same record as
    // the SoLd path gives (tests/placed_records.rs), from the PlLd alone.
    if let Some(bytes) = read_corpus("layers/smartobject-layer.psd", 26_417) {
        let p = proposal_of(&legacy_only(&bytes), &bytes);
        let r = &p.raster_records;
        assert_eq!(r.placed_sources.len(), 1, "{:?}", p.loss.entries);
        let s = &r.placed_sources[0];
        assert_eq!((s.source_id.as_str(), s.width, s.height), ("psd-so-903957cc-082d-464a-b13e-d55023442674", 32.0, 32.0));
        assert!(matches!(s.location, StudioPlacedMode::Embedded { .. }));
        let a = r.layers.iter().find_map(|l| l.placed_asset.clone()).expect("placed instance");
        assert_eq!(a.transform, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        assert!(p.loss.has_code("placed_layer_legacy_size_from_warp_bounds"), "{:?}", p.loss.entries);
    }
    // placedLayer.psd: its one PlLd is the embedded copy at (96, 96), quad to (160, 160), a
    // warpCustom identity 4 x 4 mesh with bounds 0, 0, 64, 64.
    if let Some(bytes) = read_corpus("placedLayer.psd", 114_796) {
        let p = proposal_of(&legacy_only(&bytes), &bytes);
        let r = &p.raster_records;
        assert_eq!(r.placed_sources.len(), 1, "{:?}", p.loss.entries);
        assert_eq!((r.placed_sources[0].width, r.placed_sources[0].height), (64.0, 64.0));
        let a = r.layers.iter().find_map(|l| l.placed_asset.clone()).expect("placed instance");
        assert_eq!(a.transform, [1.0, 0.0, 0.0, 1.0, 96.0, 96.0]);
    }
}

// ---- hand-built blocks -----------------------------------------------------------------------

fn ds_key(k: &str) -> Vec<u8> {
    let mut v = if k.len() == 4 { be32(0) } else { be32(k.len() as u32) };
    v.extend_from_slice(k.as_bytes());
    v
}
fn ds_object(class: &str, items: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    let mut v = be32(0);
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
fn v_untf(value: f64) -> Vec<u8> {
    let mut v = b"UntF#Pxl".to_vec();
    v.extend(value.to_be_bytes());
    v
}
/// A `PlLd` block: `plcL`, `version`, id, page fields, the quad, warp version 0, the warp
/// descriptor (`style`, with `bounds` when given).
fn plld(version: u32, id: &str, quad: [f64; 8], style: &str, bounds: Option<[f64; 4]>) -> Vec<u8> {
    let mut v = b"plcL".to_vec();
    v.extend(be32(version));
    v.push(id.len() as u8);
    v.extend_from_slice(id.as_bytes());
    for x in [1u32, 1, 16, 2] {
        v.extend(be32(x));
    }
    for q in quad {
        v.extend(q.to_be_bytes());
    }
    v.extend(be32(0));
    v.extend(be32(16));
    let mut items = vec![("warpStyle", v_enum("warpStyle", style))];
    if let Some([top, left, bottom, right]) = bounds {
        let mut o = b"Objc".to_vec();
        o.extend(ds_object("Rctn", vec![("Top ", v_untf(top)), ("Left", v_untf(left)), ("Btom", v_untf(bottom)), ("Rght", v_untf(right))]));
        items.push(("bounds", o));
    }
    v.extend(ds_object("warp", items));
    v
}
/// One linked-file item (`kind`, version 7) with its u64 length, padded.
fn link_item(kind: &[u8; 4], id: &str, payload: &[u8]) -> Vec<u8> {
    let mut item = kind.to_vec();
    item.extend(be32(7));
    item.push(id.len() as u8);
    item.extend_from_slice(id.as_bytes());
    let units: Vec<u16> = "a.png".encode_utf16().chain([0]).collect();
    item.extend(be32(units.len() as u32));
    for u in units {
        item.extend(be16(u));
    }
    item.extend_from_slice(b"png ");
    item.extend([0; 4]);
    item.extend((payload.len() as u64).to_be_bytes());
    item.push(0);
    item.extend_from_slice(payload);
    let mut v = (item.len() as u64).to_be_bytes().to_vec();
    v.extend(item);
    pad_to(v, 4)
}
/// A PNG prefix (signature and `IHDR`) of `w` x `h`.
fn png(w: u32, h: u32) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend(be32(13));
    v.extend_from_slice(b"IHDR");
    v.extend(be32(w));
    v.extend(be32(h));
    v.extend([8, 6, 0, 0, 0]);
    v
}
fn doc(pl: Vec<u8>, lnk2: Option<Vec<u8>>) -> Vec<u8> {
    let mut l = TLayer::new("so", [0, 0, 2, 2]);
    l.channels = vec![(-1, 0, vec![255; 4]), (0, 0, vec![10; 4]), (1, 0, vec![20; 4]), (2, 0, vec![30; 4])];
    l.blocks = vec![layer_block(b"PlLd", &pl, false)];
    let info = layer_info(&[l], false, false);
    let globals: Vec<Vec<u8>> = lnk2.into_iter().map(|b| global_block(b"lnk2", &b, false)).collect();
    Doc {
        version: 1,
        channels: 3,
        height: 2,
        width: 2,
        depth: 8,
        mode: 3,
        color_mode_data: Vec::new(),
        resources: Vec::new(),
        layer_section: layer_section(&info, &globals, false),
        image: Some((0, vec![1; 12])),
    }
    .bytes()
}

#[test]
fn hand_built_legacy_placed_layers_recover_or_report_why_not() {
    let id = "aaaa-bbbb";
    // a rotated, scaled instance of a 10 x 5 source (corners (4, 1) along (0, 6) and (-2.5, 0))
    let quad = [4.0, 1.0, 4.0, 7.0, 1.5, 7.0, 1.5, 1.0];
    let run = |pl: Vec<u8>, lnk2: Option<Vec<u8>>| {
        let bytes = doc(pl, lnk2);
        proposal_of(&read(&bytes), &bytes)
    };
    // size from the warp bounds
    let p = run(plld(3, id, quad, "warpNone", Some([0.0, 0.0, 5.0, 10.0])), Some(link_item(b"liFD", id, &png(99, 99))));
    let a = p.raster_records.layers[0].placed_asset.clone().expect("instance");
    assert_eq!(a.transform, [0.0, 0.6, -0.5, 0.0, 4.0, 1.0]);
    assert_eq!((p.raster_records.placed_sources[0].width, p.raster_records.placed_sources[0].height), (10.0, 5.0));
    // no bounds: the size of the embedded PNG (10 x 5); a PSD header works the same way
    let p = run(plld(3, id, quad, "warpNone", None), Some(link_item(b"liFD", id, &png(10, 5))));
    assert_eq!(p.raster_records.layers[0].placed_asset.clone().expect("instance").transform, [0.0, 0.6, -0.5, 0.0, 4.0, 1.0]);
    assert!(p.loss.has_code("placed_layer_legacy_size_from_embedded_header"));
    let mut psd = b"8BPS".to_vec();
    psd.extend(be16(1));
    psd.extend([0; 6]);
    psd.extend(be16(3));
    psd.extend(be32(5)); // height
    psd.extend(be32(10)); // width
    let p = run(plld(3, id, quad, "warpNone", None), Some(link_item(b"liFD", id, &psd)));
    assert_eq!((p.raster_records.placed_sources[0].width, p.raster_records.placed_sources[0].height), (10.0, 5.0));

    // typed losses: the layer keeps its pixels and no placed record is made
    let refused = |pl: Vec<u8>, lnk2: Option<Vec<u8>>, code: &str| {
        let p = run(pl, lnk2);
        assert!(p.raster_records.layers.iter().all(|l| l.placed_asset.is_none()) && p.raster_records.placed_sources.is_empty(), "{code}");
        let codes: Vec<&str> = p.loss.entries.iter().filter(|e| e.path == "layer[0]/tagged/PlLd").map(|e| e.code).collect();
        assert!(codes.contains(&code), "{code}: {:?}", p.loss.entries);
    };
    let embedded = |payload: &[u8]| Some(link_item(b"liFD", id, payload));
    refused(plld(3, id, quad, "warpNone", None), Some(link_item(b"liFE", id, b"")), "placed_layer_legacy_size_unknown_external");
    refused(plld(3, id, quad, "warpNone", None), embedded(b"\xff\xd8\xff\xe0 a jpeg"), "placed_layer_legacy_size_unknown_format");
    refused(plld(3, id, quad, "warpNone", None), embedded(&png(0, 5)), "placed_layer_legacy_size_unknown_format");
    refused(plld(3, id, quad, "warpNone", None), embedded(&png(400_000, 5)), "placed_layer_legacy_size_unknown_format");
    refused(plld(3, id, quad, "warpNone", None), None, "placed_source_missing");
    refused(plld(2, id, quad, "warpNone", None), embedded(&png(10, 5)), "placed_layer_legacy_version_unsupported");
    refused(plld(3, id, quad, "warpArc", Some([0.0, 0.0, 5.0, 10.0])), embedded(&png(10, 5)), "placed_warp_unmapped");
    refused(plld(3, id, [f64::NAN; 8], "warpNone", Some([0.0, 0.0, 5.0, 10.0])), embedded(&png(10, 5)), "placed_layer_transform_missing");
    let skew = [0.0, 0.0, 10.0, 0.0, 12.0, 5.0, 0.0, 5.0];
    refused(plld(3, id, skew, "warpNone", Some([0.0, 0.0, 5.0, 10.0])), embedded(&png(10, 5)), "placed_transform_non_affine");
    // truncated: the block ends inside the quad; a Pascal id longer than the block
    let mut short = plld(3, id, quad, "warpNone", None);
    short.truncate(40);
    refused(short, embedded(&png(10, 5)), "placed_layer_legacy_truncated");
    let mut long_id = b"plcL".to_vec();
    long_id.extend(be32(3));
    long_id.push(255);
    long_id.extend_from_slice(b"short");
    refused(long_id, embedded(&png(10, 5)), "placed_layer_legacy_truncated");
}
