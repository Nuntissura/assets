//! Lane SMB-PACKAGE-MASK-TILES (feature `viewer`): a masked document survives the package. A PSD
//! with a user mask (the layer's appearance mask coverage) plus raster records whose smart-filter
//! stack carries a stack mask and a per-filter mask is imported, saved through the session's
//! streaming save, reopened in the viewer, and the frame is byte-identical to a direct composite
//! (`lower_to_render_plan_extended`) + render-cpu render of the saved package with every mask plane
//! served. A package written before the fix (mask assets dropped) opens with typed loss notes and
//! the viewer refuses it typed (`missing_asset`), never a crash or a silent unmasked frame.
//! Fixture helpers are copied from `viewer_render_records.rs` (lane SMB-HARNESS-RENDER-RECORDS).
#![cfg(feature = "viewer")]

mod common;

use common::Scratch;
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite as composite;
use hsk_studio_folio as folio;
use hsk_studio_harness::import::import;
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::{Prefer, VIEW_ROOT_LAYER, encode_rgba8, view_snapshot};
use hsk_studio_package::{EntryKind, ImportEntry, ImportRole, Limits, LossReason, MANIFEST_NAME, Manifest, preflight, read_package, sha256_hex_of};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BlendSpace, ColourTag, Exactness, ExecuteRequest, OutputTarget, PlanSources, RenderBackend, SampleFormat,
    SinkError, SourceKey, TilePlanes, TileSink,
};
use hsk_studio_pigment::{Mask, Rect};
use serde_json::json;
use std::collections::BTreeMap;

const W: u32 = 48;
const H: u32 = 40;
const CELL: usize = 256;

struct TrustAll;
impl folio::Resolver for TrustAll {
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
    fn primitive_layer_binding(&self, kind: &str, schema_id: &str) -> bool {
        kind == "adjustment" && schema_id == folio::ADJUSTMENT_SCHEMA
    }
}

// ---- independent PSD bytes: RGB 8-bit, raw channels, user masks and additional-info blocks ----

struct Layer {
    name: &'static str,
    rect: [i32; 4],
    channels: Vec<(i16, Vec<u8>)>,
    mask: Option<[i32; 4]>,
    blocks: Vec<([u8; 4], Vec<u8>)>,
}

fn psd(layers: &[Layer]) -> Vec<u8> {
    let mut v = b"8BPS".to_vec();
    v.extend(1u16.to_be_bytes());
    v.extend([0u8; 6]);
    v.extend(3u16.to_be_bytes());
    v.extend(H.to_be_bytes());
    v.extend(W.to_be_bytes());
    v.extend(8u16.to_be_bytes());
    v.extend(3u16.to_be_bytes());
    v.extend(0u32.to_be_bytes());
    v.extend(0u32.to_be_bytes());
    let mut info = (layers.len() as i16).to_be_bytes().to_vec();
    for l in layers {
        for r in l.rect {
            info.extend(r.to_be_bytes());
        }
        info.extend((l.channels.len() as u16).to_be_bytes());
        for (id, data) in &l.channels {
            info.extend(id.to_be_bytes());
            info.extend(((2 + data.len()) as u32).to_be_bytes());
        }
        info.extend_from_slice(b"8BIMnorm");
        info.extend([255u8, 0, 0x08, 0]);
        let mut extra = Vec::new();
        match l.mask {
            Some(m) => {
                extra.extend(20u32.to_be_bytes());
                for r in m {
                    extra.extend(r.to_be_bytes());
                }
                extra.extend([0u8, 0, 0, 0]);
            }
            None => extra.extend(0u32.to_be_bytes()),
        }
        extra.extend(0u32.to_be_bytes());
        let mut pascal = vec![l.name.len() as u8];
        pascal.extend_from_slice(l.name.as_bytes());
        while pascal.len() % 4 != 0 {
            pascal.push(0);
        }
        extra.extend(pascal);
        for (key, data) in &l.blocks {
            extra.extend_from_slice(b"8BIM");
            extra.extend_from_slice(key);
            extra.extend((data.len() as u32).to_be_bytes());
            extra.extend(data);
        }
        info.extend((extra.len() as u32).to_be_bytes());
        info.extend(extra);
    }
    for l in layers {
        for (_, data) in &l.channels {
            info.extend(0u16.to_be_bytes());
            info.extend_from_slice(data);
        }
    }
    if info.len() % 2 == 1 {
        info.push(0);
    }
    let mut section = (info.len() as u32).to_be_bytes().to_vec();
    section.extend(info);
    section.extend(0u32.to_be_bytes());
    v.extend((section.len() as u32).to_be_bytes());
    v.extend(section);
    v.extend(0u16.to_be_bytes());
    v.extend(std::iter::repeat_n(10u8, (W * H * 3) as usize));
    v
}

fn plane(w: usize, h: usize, f: impl Fn(usize, usize) -> u8) -> Vec<u8> {
    (0..w * h).map(|i| f(i % w, i / w)).collect()
}

fn document_psd() -> Vec<u8> {
    let (w, h) = (W as usize, H as usize);
    let base = Layer {
        name: "base",
        rect: [0, 0, H as i32, W as i32],
        channels: (0..3).map(|c| (c as i16, plane(w, h, move |x, y| ((x * (3 + c) + y * 5) % 256) as u8))).collect(),
        mask: None,
        blocks: vec![],
    };
    // "art": x 6..38, y 8..36, a checker of two colours.
    let (aw, ah) = (32usize, 28usize);
    let mut art_channels: Vec<(i16, Vec<u8>)> = (0..3).map(|c| (c as i16, plane(aw, ah, move |x, y| if (x / 4 + y / 4) % 2 == 0 { [230u8, 40, 30][c] } else { [20u8, 90, 220][c] }))).collect();
    // A user mask over the same rectangle (a diagonal ramp).
    art_channels.push((-2, plane(aw, ah, |x, y| ((x + y) * 255 / (aw + ah)) as u8)));
    let art = Layer { name: "art", rect: [8, 6, 36, 38], channels: art_channels, mask: Some([8, 6, 36, 38]), blocks: vec![] };
    let inv = Layer { name: "inv", rect: [0, 0, 0, 0], channels: vec![(0, vec![]), (1, vec![]), (2, vec![])], mask: None, blocks: vec![(*b"nvrt", vec![])] };
    psd(&[base, art, inv])
}

// ---- direct composite + render-cpu (independent of the viewer's sources) ----

fn space() -> ColourTag {
    ColourTag { profile_sha256: hsk_studio_prism::profile_hash(hsk_studio_prism::WORKING_PROFILE_LINEAR_SRGB), blend_space: BlendSpace::LinearLight }
}

/// The coverage tiles of one mask over the union of their placements (grid of the largest tile).
fn stitch(assets: &BTreeMap<String, Vec<u8>>, tiles: &[folio::TileRef]) -> Option<(Rect, &'static str, Vec<u8>)> {
    let bpp = if tiles[0].format == "coverage_u16le" { 2 } else { 1 };
    let format = if bpp == 2 { "coverage_u16le" } else { "coverage_u8" };
    let gw = tiles.iter().map(|t| t.width.value as i64).max()?;
    let gh = tiles.iter().map(|t| t.height.value as i64).max()?;
    let rects: Vec<Rect> = tiles.iter().map(|t| Rect { x: t.column * gw, y: t.row * gh, width: t.width.value as u32, height: t.height.value as u32 }).collect();
    let x0 = rects.iter().map(|r| r.x).min()?;
    let y0 = rects.iter().map(|r| r.y).min()?;
    let x1 = rects.iter().map(|r| r.x + i64::from(r.width)).max()?;
    let y1 = rects.iter().map(|r| r.y + i64::from(r.height)).max()?;
    let bounds = Rect { x: x0, y: y0, width: (x1 - x0) as u32, height: (y1 - y0) as u32 };
    let stride = bounds.width as usize * bpp;
    let mut bytes = vec![0u8; stride * bounds.height as usize];
    for (t, r) in tiles.iter().zip(&rects) {
        let body = assets.get(&t.content_digest.digest)?;
        let row = r.width as usize * bpp;
        for y in 0..r.height as usize {
            let at = (r.y - y0) as usize * stride + y * stride + (r.x - x0) as usize * bpp;
            bytes[at..at + row].copy_from_slice(&body[y * row..y * row + row]);
        }
    }
    Some((bounds, format, bytes))
}

struct Info<'a>(&'a BTreeMap<String, Vec<u8>>);
impl composite::TileInfoSource for Info<'_> {
    fn tile_info(&self, t: &folio::TileRef) -> Option<composite::TileInfo> {
        let (colour, alpha) = self.0.get(&t.content_digest.digest)?.split_at(CELL * CELL * 12);
        Some(composite::TileInfo { format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, colour_sha256: hsk_studio_prism::profile_hash(colour), alpha_sha256: hsk_studio_prism::profile_hash(alpha) })
    }
    fn mask_sha256(&self, tiles: &[folio::TileRef]) -> Option<[u8; 32]> {
        stitch(self.0, tiles).map(|(_, _, b)| hsk_studio_prism::profile_hash(&b))
    }
}

struct Sources<'a> {
    assets: &'a BTreeMap<String, Vec<u8>>,
    table: &'a composite::SourceTable,
    masks: Vec<(SourceKey, Rect, &'static str, Vec<u8>)>,
}
impl PlanSources for Sources<'_> {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        let (_, tile) = self.table.iter().find(|(k, _)| *k == key)?;
        let (colour, alpha) = self.assets.get(&tile.content_digest.digest)?.split_at(CELL * CELL * 12);
        Some(TilePlanes {
            width: tile.width.value as u32,
            height: tile.height.value as u32,
            colour,
            colour_stride_bytes: (CELL * 12) as u64,
            alpha,
            alpha_stride_bytes: (CELL * 4) as u64,
            colour_sha256: hsk_studio_prism::profile_hash(colour),
            alpha_sha256: hsk_studio_prism::profile_hash(alpha),
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: space(),
        })
    }
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>> {
        let (_, bounds, format, bytes) = self.masks.iter().find(|(k, ..)| *k == key)?;
        let bpp = if *format == "coverage_u16le" { 2 } else { 1 };
        Some(Mask { bounds: *bounds, sample_format: format, bytes, stride_bytes: u64::from(bounds.width) * bpp, sha256: hsk_studio_prism::profile_hash(bytes) })
    }
}

struct Canvas(Vec<[f32; 4]>);
impl TileSink for Canvas {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for y in 0..r.height as usize {
            for x in 0..r.width as usize {
                let i = y * r.width as usize + x;
                self.0[(r.y as usize + y) * W as usize + r.x as usize + x] = [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            }
        }
        Ok(())
    }
}

fn direct_render(package: &[u8]) -> (Vec<u8>, usize) {
    let cancel = CancellationToken::default();
    let opened = read_package(package, &Limits::default(), folio::Budget::default(), &TrustAll, &cancel).expect("reopen");
    let snap = opened.snapshot().expect("editable").clone();
    let mut records: folio::StudioRasterRecords = serde_json::from_slice(opened.import_bytes(ImportRole::Records).expect("records")).unwrap();
    let view = view_snapshot(&snap, &cancel, &mut Vec::new()).expect("view");
    records.document_id = view.document().document_id.clone();
    let plan = composite::lower(&view, view.document().revision, BlendSpace::LinearLight, &composite::Limits::default(), &cancel).expect("lower");
    let report = composite::evaluate_extended(&plan, &view, &records, &cancel).expect("evaluate");
    let root = composite::NodeAddr { layer_id: VIEW_ROOT_LAYER.to_string(), operation_key: "mix".into(), port_key: "out".into() };
    let target = OutputTarget { extent: Rect { x: 0, y: 0, width: W, height: H }, format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space: space() };
    let lowered = composite::lower_to_render_plan_extended(&plan, &report, &view, &records, &root, target, &Info(&opened.assets)).expect("lower extended");
    let masks = lowered.mask_sources.iter().map(|(k, t)| {
        let (b, f, bytes) = stitch(&opened.assets, t).expect("mask tiles");
        (*k, b, f, bytes)
    }).collect::<Vec<_>>();
    let n_masks = masks.len();
    let sources = Sources { assets: &opened.assets, table: &lowered.sources, masks };
    let request = ExecuteRequest { expected_revision: lowered.plan.revision, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 30 };
    let mut canvas = Canvas(vec![[0.0; 4]; (W * H) as usize]);
    hsk_studio_render_cpu::Renderer::new(32)
        .unwrap()
        .execute_extended(&lowered.plan, &lowered.extensions, &request, &sources, &cancel, &mut canvas)
        .expect("render-cpu");
    (encode_rgba8(&canvas.0), n_masks)
}

// ---- the masked document ----

/// Every mask plane of the fixture: (role, bytes).
struct Masked {
    package: Vec<u8>,
    planes: Vec<(&'static str, Vec<u8>)>,
}

/// The PSD (user mask on `art`) imported, plus raster records on `art`: mask parameters and a
/// smart-filter stack with a stack mask and a per-filter mask (coverage planes of their own, placed
/// like the user mask tile), written by the package writer the importer uses.
fn masked_document() -> Masked {
    let cancel = CancellationToken::default();
    let mut imported = import(&document_psd(), &TrustAll, &cancel).expect("import");
    let doc = imported.snapshot.document().clone();
    let art = doc.layers.iter().find(|l| l.name == "art").expect("art layer");
    let user = art.appearance.as_ref().and_then(|a| a.mask.as_ref()).expect("the PSD user mask is imported as the appearance mask");
    assert_eq!(user.tiles.len(), 1, "one user mask tile");
    let user_tile = user.tiles[0].clone();
    let user_bytes = hsk_studio_package::AssetSource::read(&imported.assets, &user_tile.content_digest, u64::MAX).expect("user mask bytes from the importer");
    let (w, h) = (user_tile.width.value as usize, user_tile.height.value as usize);
    let stack_bytes = plane(w, h, |x, _| if x < w / 2 { 255 } else { 40 });
    let blur_bytes = plane(w, h, |_, y| if y < h / 2 { 0 } else { 255 });
    let mut plane_tile = |key: &str, bytes: &[u8]| {
        let mut t = user_tile.clone();
        t.object_key = key.into();
        t.content_digest = imported.assets.insert(bytes.to_vec());
        serde_json::to_value(t).unwrap()
    };
    let stack_tile = plane_tile("sf_stack_0", &stack_bytes);
    let blur_tile = plane_tile("sf_blur_0", &blur_bytes);

    let entry = imported.imports.iter().find(|e| e.role == ImportRole::Records).expect("importer records");
    let mut records: folio::StudioRasterRecords = serde_json::from_slice(&entry.bytes).unwrap();
    let i = match records.layers.iter().position(|r| r.layer_id == art.layer_id) {
        Some(i) => i,
        None => {
            records.layers.push(folio::StudioRasterLayerRecord::new(art.layer_id.clone()));
            records.layers.len() - 1
        }
    };
    records.layers[i].mask = Some(serde_json::from_value(json!({"density": 80.0, "feather": 2.0, "inverted": false})).unwrap());
    records.layers[i].smart_filters = Some(serde_json::from_value(json!({"enabled": true,
        "stack_mask": {"tiles": [stack_tile], "enabled": true, "linked": true},
        "filters": [
            {"instance_id": "blur", "enabled": true, "opacity": 100.0, "mask": {"tiles": [blur_tile], "enabled": true, "linked": true},
             "params": {"filter_kind": "gaussian_blur", "radius_px": 1.5}},
            {"instance_id": "noise", "enabled": true, "opacity": 50.0, "params": {"filter_kind": "add_noise", "amount_pct": 10.0, "distribution": "uniform", "monochromatic": true, "seed": 3}}
        ]})).unwrap());
    folio::validate_raster_records(&records, &doc, folio::Budget::default(), &cancel, &TrustAll).expect("records validate");
    imported.imports.retain(|e| e.role != ImportRole::Records);
    imported.imports.push(ImportEntry::new(ImportRole::Records, serde_json::to_vec(&records).unwrap()));
    Masked {
        package: imported.to_package(&cancel).expect("package"),
        planes: vec![("user_mask", user_bytes), ("smart_filter_stack_mask", stack_bytes), ("smart_filter_mask", blur_bytes)],
    }
}

#[test]
fn a_session_saved_masked_document_renders_like_a_direct_composite_render() {
    let scratch = Scratch::new("mask-package");
    let masked = masked_document();
    let imported_path = scratch.0.join("imported.handshake");
    std::fs::write(&imported_path, &masked.package).unwrap();

    // Save through the session (streaming save of the opened package), then reopen the saved file.
    let saved_path = scratch.0.join("saved.handshake");
    let mut first = Viewer::new(Prefer::Cpu);
    first.open(&imported_path).unwrap_or_else(|e| panic!("{e}"));
    first.save(&saved_path, false).unwrap_or_else(|e| panic!("{e}"));
    drop(first);
    let saved = std::fs::read(&saved_path).unwrap();

    let cancel = CancellationToken::default();
    let reopened = read_package(&saved, &Limits::default(), folio::Budget::default(), &TrustAll, &cancel).expect("reopen saved");
    assert!(reopened.loss.is_empty(), "{:?}", reopened.loss);
    for (role, bytes) in &masked.planes {
        assert_eq!(reopened.assets.get(&sha256_hex_of(bytes)), Some(bytes), "{role} plane saved byte-identical");
    }

    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&saved_path).unwrap_or_else(|e| panic!("{e}"));
    v.render_if_dirty();
    let frame = v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json())).clone();
    assert!(frame.notes.iter().any(|n| n.starts_with("records_envelope")), "{:?}", frame.notes);

    let (direct, n_masks) = direct_render(&saved);
    assert_eq!(n_masks, 3, "user mask, smart-filter stack mask and per-filter mask are all lowered and served");
    let differ = frame.rgba8.iter().zip(&direct).filter(|(a, b)| a != b).count();
    println!("viewer vs direct composite + render-cpu (masked, session-saved): {differ} of {} bytes differ", direct.len());
    assert_eq!(frame.rgba8, direct);

    // The masks change pixels: the same package with every mask plane inverted renders differently.
    let mut flipped = reopened.assets.clone();
    for (_, bytes) in &masked.planes {
        let body = flipped.get_mut(&sha256_hex_of(bytes)).unwrap();
        body.iter_mut().for_each(|b| *b = 255 - *b);
    }
    let snap = reopened.snapshot().unwrap();
    let records: folio::StudioRasterRecords = serde_json::from_slice(reopened.import_bytes(ImportRole::Records).unwrap()).unwrap();
    let other = hsk_studio_harness::view::render::render_full_with_records(snap, &flipped, None, None, Some(&records), Prefer::Cpu, None, None, &cancel)
        .expect("flipped-mask frame");
    assert_ne!(other.rgba8, frame.rgba8, "mask coverage reaches the frame");
}

/// STORE-only ZIP of `(name, bytes)` entries (the canonical form the reader accepts).
fn store_zip(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let (mut out, mut cd) = (Vec::new(), Vec::new());
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = hsk_studio_package::crc32_of(data);
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

#[test]
fn a_masked_package_written_before_the_fix_is_a_typed_loss_not_a_blank() {
    let scratch = Scratch::new("mask-legacy");
    let masked = masked_document();
    // What the pre-fix writer produced: the same package without the mask assets.
    let dropped: Vec<String> = masked.planes.iter().map(|(_, b)| format!("assets/{}", sha256_hex_of(b))).collect();
    let cancel = CancellationToken::default();
    let metas = preflight(&masked.package, &Limits::default(), &cancel).unwrap();
    let mut entries: Vec<(String, Vec<u8>)> = metas.iter().map(|m| (m.name.clone(), m.data(&masked.package).to_vec())).collect();
    entries.retain(|(n, _)| !dropped.contains(n));
    let slot = entries.iter().position(|(n, _)| n == MANIFEST_NAME).unwrap();
    let mut manifest = Manifest::parse(&entries[slot].1, &Limits::default()).unwrap();
    manifest.entries.retain(|e| e.kind != EntryKind::Asset || !dropped.contains(&e.name));
    entries[slot].1 = manifest.to_canonical_bytes().unwrap();
    let legacy = store_zip(&entries);

    let opened = read_package(&legacy, &Limits::default(), folio::Budget::default(), &TrustAll, &cancel).expect("a legacy masked package opens");
    let mut lost: Vec<String> = opened.loss.iter().filter(|n| n.reason == LossReason::MissingAsset).map(|n| n.name.clone()).collect();
    lost.sort();
    let mut wanted = dropped.clone();
    wanted.sort();
    assert_eq!(lost, wanted, "one typed loss note per missing mask plane");

    let path = scratch.0.join("legacy.handshake");
    std::fs::write(&path, &legacy).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    let err = v.open(&path).expect_err("the viewer refuses a document whose mask planes are gone");
    assert!(err.contains("missing_asset"), "{err}");
    assert!(v.frame().is_none(), "no silent unmasked frame");
}
