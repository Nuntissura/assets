//! Lane SMB-HARNESS-RENDER-RECORDS (feature `viewer`): the viewer frame renders the document's Folio
//! raster records (an invert adjustment layer from the PSD, mask parameters on a PSD user mask,
//! an effect stack, a smart-filter stack) through composite's adapter. A saved document with those
//! records looks byte-identical in the viewer and in a direct composite
//! (`lower_to_render_plan_extended`) + render-cpu render of the reopened package, and differs from
//! the records-free frame.
#![cfg(feature = "viewer")]

mod common;

use common::Scratch;
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite as composite;
use hsk_studio_folio as folio;
use hsk_studio_harness::import::import;
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::{self, Prefer, VIEW_ROOT_LAYER, encode_rgba8, view_snapshot};
use hsk_studio_package::{ImportEntry, ImportRole, Limits, read_package};
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
    // "art": x 6..38, y 8..36, with a user mask over the same rectangle (a diagonal ramp).
    let (aw, ah) = (32usize, 28usize);
    let mut art_channels: Vec<(i16, Vec<u8>)> = (0..3).map(|c| (c as i16, plane(aw, ah, move |x, y| if (x / 4 + y / 4) % 2 == 0 { [230u8, 40, 30][c] } else { [20u8, 90, 220][c] }))).collect();
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

#[test]
fn viewer_frame_renders_the_raster_records_like_a_direct_composite_render() {
    let scratch = Scratch::new("render-records");
    let cancel = CancellationToken::default();
    let mut imported = import(&document_psd(), &TrustAll, &cancel).expect("import");
    let doc = imported.snapshot.document().clone();
    let id = |name: &str| doc.layers.iter().find(|l| l.name == name).unwrap_or_else(|| panic!("{name}")).layer_id.clone();
    let (base, art, inv) = (id("base"), id("art"), id("inv"));
    // The importer's records (the invert adjustment layer), plus mask parameters, an effect stack
    // and a smart-filter stack.
    let entry = imported.imports.iter().find(|e| e.role == ImportRole::Records).expect("importer records");
    let mut records: folio::StudioRasterRecords = serde_json::from_slice(&entry.bytes).unwrap();
    assert!(records.layer(&inv).and_then(|r| r.adjustment.as_ref()).is_some(), "invert adjustment imported");
    let layer_mut = |records: &mut folio::StudioRasterRecords, l: &str| -> usize {
        if let Some(i) = records.layers.iter().position(|r| r.layer_id == l) {
            return i;
        }
        records.layers.push(folio::StudioRasterLayerRecord::new(l));
        records.layers.len() - 1
    };
    let i = layer_mut(&mut records, &art);
    records.layers[i].mask = Some(serde_json::from_value(json!({"density": 80.0, "feather": 2.0, "inverted": false})).unwrap());
    records.layers[i].smart_filters = Some(serde_json::from_value(json!({"enabled": true, "filters": [
        {"instance_id": "blur", "enabled": true, "opacity": 100.0, "params": {"filter_kind": "gaussian_blur", "radius_px": 1.5}},
        {"instance_id": "noise", "enabled": true, "opacity": 50.0, "params": {"filter_kind": "add_noise", "amount_pct": 10.0, "distribution": "uniform", "monochromatic": true, "seed": 3}}
    ]})).unwrap());
    let i = layer_mut(&mut records, &base);
    records.layers[i].effects = Some(serde_json::from_value(json!({"effect_stack_id": "SEFX-019abcde-0000-7000-8000-0000000000aa", "enabled": true, "scale": 100.0, "effects": [
        {"instance_id": "co_1", "enabled": true, "visible_in_editor": true, "blend_mode": 2, "opacity": 40.0,
         "colour": {"r": 1.0, "g": 0.0, "b": 0.0, "colour_profile_id": null}, "anti_alias": false, "noise": 0.0, "angle": 0.0,
         "use_global_light": false, "params": {"effect_kind": "colour_overlay"}}
    ]})).unwrap());
    folio::validate_raster_records(&records, &doc, folio::Budget::default(), &cancel, &TrustAll).expect("records validate");
    imported.imports.retain(|e| e.role != ImportRole::Records);
    imported.imports.push(ImportEntry::new(ImportRole::Records, serde_json::to_vec(&records).unwrap()));
    let package = imported.to_package(&cancel).expect("package");
    let path = scratch.0.join("records.handshake");
    std::fs::write(&path, &package).unwrap();

    // The viewer opens the saved document and renders it with its records.
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&path).unwrap_or_else(|e| panic!("{e}"));
    v.render_if_dirty();
    let frame = v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json())).clone();
    assert!(frame.notes.iter().any(|n| n.starts_with("records_envelope")), "{:?}", frame.notes);

    // A direct composite + render-cpu render of the reopened package: identical bytes.
    let (direct, n_masks) = direct_render(&package);
    assert!(n_masks >= 1, "the PSD user mask is served");
    let differ = frame.rgba8.iter().zip(&direct).filter(|(a, b)| a != b).count();
    println!("viewer vs direct composite + render-cpu: {differ} of {} bytes differ", direct.len());
    assert_eq!(frame.rgba8, direct);

    // The records-free frame of the same document is different (the records are applied).
    let opened = read_package(&package, &Limits::default(), folio::Budget::default(), &TrustAll, &cancel).unwrap();
    let plain = render::render(opened.snapshot().unwrap(), &opened.assets, Prefer::Cpu, None, &cancel).expect("plain frame");
    assert_ne!(plain.rgba8, frame.rgba8);
}
