//! Lane SMB-PS-SMART-FILTER-WIRING: a document whose raster records carry a smart-filter stack is
//! saved to a native package (records import entry), reopened, lowered by composite
//! (`lower_to_render_plan_extended` turns the Folio record into the envelope entry through pigment's
//! `SmartFilterOp::from_record`) and rendered by render-cpu: the reopened document re-renders its
//! smart filters, equal to pigment's `SmartFilterStack::render` of the same content.
#![cfg(feature = "viewer")]

mod common;

use common::{L, layered_psd};
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite as composite;
use hsk_studio_folio as folio;
use hsk_studio_harness::import::import;
use hsk_studio_harness::view::render::{VIEW_ROOT_LAYER, view_snapshot};
use hsk_studio_package::{ImportEntry, ImportRole, Limits, read_package};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BlendSpace, ColourTag, Exactness, ExecuteRequest, OutputTarget, PlanSources, RenderBackend, SampleFormat,
    SinkError, SourceKey, TilePlanes, TileSink,
};
use hsk_studio_pigment::retouch::RgbaPlane;
use hsk_studio_pigment::smart_filter::{SmartFilterStack, StackEntry, StackLimits};
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
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}

fn space() -> ColourTag {
    ColourTag { profile_sha256: hsk_studio_prism::profile_hash(hsk_studio_prism::WORKING_PROFILE_LINEAR_SRGB), blend_space: BlendSpace::LinearLight }
}

struct Sources<'a> {
    assets: &'a BTreeMap<String, Vec<u8>>,
    table: &'a composite::SourceTable,
}
impl PlanSources for Sources<'_> {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        let (_, tile) = self.table.iter().find(|(k, _)| *k == key)?;
        let bytes = self.assets.get(&tile.content_digest.digest)?;
        let (colour, alpha) = bytes.split_at(CELL * CELL * 12);
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
    fn mask(&self, _: SourceKey) -> Option<Mask<'_>> {
        None
    }
}

struct Canvas {
    px: Vec<[f32; 4]>,
}
impl TileSink for Canvas {
    fn accept(&mut self, r: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let f = |b: &[u8], at: usize| f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        for y in 0..r.height as usize {
            for x in 0..r.width as usize {
                let i = y * r.width as usize + x;
                self.px[(r.y as usize + y) * W as usize + r.x as usize + x] = [f(colour, i * 12), f(colour, i * 12 + 4), f(colour, i * 12 + 8), f(alpha, i * 4)];
            }
        }
        Ok(())
    }
}

/// Renders `snap` with `records` (None: the plain document) through composite and render-cpu.
fn render(snap: &folio::Snapshot, assets: &BTreeMap<String, Vec<u8>>, records: &folio::StudioRasterRecords) -> (Vec<[f32; 4]>, bool) {
    let cancel = CancellationToken::default();
    let view = view_snapshot(snap, &cancel, &mut Vec::new()).expect("view");
    let plan = composite::lower(&view, view.document().revision, BlendSpace::LinearLight, &composite::Limits::default(), &cancel).expect("lower");
    let mut records = records.clone();
    records.document_id = view.document().document_id.clone();
    let report = composite::evaluate_extended(&plan, &view, &records, &cancel).expect("evaluate");
    let root = composite::NodeAddr { layer_id: VIEW_ROOT_LAYER.to_string(), operation_key: "mix".into(), port_key: "out".into() };
    let target = OutputTarget { extent: Rect { x: 0, y: 0, width: W, height: H }, format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space: space() };
    let info = |t: &folio::TileRef| {
        let bytes = assets.get(&t.content_digest.digest)?;
        let (colour, alpha) = bytes.split_at(CELL * CELL * 12);
        Some(composite::TileInfo {
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: hsk_studio_prism::profile_hash(colour),
            alpha_sha256: hsk_studio_prism::profile_hash(alpha),
        })
    };
    let lowered = composite::lower_to_render_plan_extended(&plan, &report, &view, &records, &root, target, &info).expect("lower extended");
    let has_stack = serde_json::to_string(&lowered.extensions).unwrap().contains("smart_filters");
    let sources = Sources { assets, table: &lowered.sources };
    let request = ExecuteRequest { expected_revision: lowered.plan.revision, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 30 };
    let mut canvas = Canvas { px: vec![[f32::NAN; 4]; (W * H) as usize] };
    hsk_studio_render_cpu::Renderer::new(16)
        .unwrap()
        .execute_extended(&lowered.plan, &lowered.extensions, &request, &sources, &cancel, &mut canvas)
        .expect("render-cpu");
    (canvas.px, has_stack)
}

#[test]
fn saved_smart_filters_re_render_after_reopening() {
    let cancel = CancellationToken::default();
    let n = (W * H) as usize;
    let ch = |c: usize| -> Vec<u8> { (0..n).map(|i| (((i % W as usize) * (7 + c * 5) + (i / W as usize) * (3 + c)) % 256) as u8).collect() };
    let psd = layered_psd(W, H, &[L { name: "art", rect: [0, 0, H as i32, W as i32], channels: vec![(0, ch(0)), (1, ch(1)), (2, ch(2))] }]);
    let mut imported = import(&psd, &TrustAll, &cancel).expect("import");
    let doc = imported.snapshot.document();
    let layer = doc.layers.iter().find(|l| l.name == "art").expect("art").layer_id.clone();
    let stack: folio::StudioSmartFilterStack = serde_json::from_value(json!({"enabled": true, "filters": [
        {"instance_id": "blur", "enabled": true, "opacity": 100.0, "params": {"filter_kind": "gaussian_blur", "radius_px": 2.0}},
        {"instance_id": "noise", "enabled": true, "opacity": 60.0, "params": {"filter_kind": "add_noise", "amount_pct": 15.0, "distribution": "uniform", "monochromatic": false, "seed": 4}},
        {"instance_id": "median", "enabled": false, "opacity": 100.0, "params": {"filter_kind": "median", "radius_px": 1}}
    ]}))
    .unwrap();
    let mut records = folio::StudioRasterRecords::new(doc.document_id.clone());
    let mut rec = folio::StudioRasterLayerRecord::new(layer.clone());
    rec.smart_filters = Some(stack.clone());
    records.layers.push(rec);
    folio::validate_raster_records(&records, doc, folio::Budget::default(), &cancel, &TrustAll).expect("records validate");
    imported.imports.retain(|e| e.role != ImportRole::Records);
    imported.imports.push(ImportEntry::new(ImportRole::Records, serde_json::to_vec(&records).unwrap()));
    let package = imported.to_package(&cancel).expect("package");

    // Reopen: the document, its tiles and the records entry come back from the container.
    let opened = read_package(&package, &Limits::default(), folio::Budget::default(), &TrustAll, &cancel).expect("reopen");
    let snap = opened.snapshot().expect("editable").clone();
    let back: folio::StudioRasterRecords = serde_json::from_slice(opened.import_bytes(ImportRole::Records).expect("records entry")).unwrap();
    assert_eq!(back, records);

    let (filtered, has_stack) = render(&snap, &opened.assets, &back);
    assert!(has_stack, "the envelope carries the stack");
    let (plain, plain_stack) = render(&snap, &opened.assets, &folio::StudioRasterRecords::new(snap.document().document_id.clone()));
    assert!(!plain_stack);
    assert_ne!(filtered, plain, "the reopened document renders its smart filters");

    // Expected: pigment's stack over the plain content (opaque layer over nothing).
    let op = hsk_studio_pigment::smart_filter::SmartFilterOp::from_record(&stack, |_| None).unwrap();
    let stack = SmartFilterStack {
        entries: op.entries.iter().map(|e| StackEntry { filter: e.filter.clone(), enabled: e.enabled, opacity: e.opacity, mask: None }).collect(),
        stack_mask: None,
    };
    let want = stack.render(&RgbaPlane::new(Rect { x: 0, y: 0, width: W, height: H }, plain.clone()).unwrap(), &StackLimits::default()).unwrap();
    let worst = filtered
        .iter()
        .zip(&want.data)
        .flat_map(|(a, b)| (0..4).map(move |k| (a[k] - b[k]).abs()))
        .fold(0.0f32, f32::max);
    println!("reopened smart-filter render vs pigment stack: max |delta| = {worst:e}");
    assert!(worst <= 1e-6, "{worst}");
}
