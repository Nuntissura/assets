//! Lane SMB-HARNESS-RENDER-RECORDS (2): composite's lowering of a Folio smart-filter record
//! (lane SMB-PS-SMART-FILTER-WIRING, `lower_render_plan/smart_filter.rs`) registers each enabled
//! per-filter mask and the stack mask as plan mask sources (key, placement, format and the
//! caller's stitched-plane hash), drops disabled masks, lowers a disabled stack to nothing and
//! refuses an unresolvable mask or a stack pigment would refuse with typed reasons. Expected values
//! are derived by hand from the fixture.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{
    BlendSpace, CompositeError, ExtendedLowering, Limits, TileInfo, TileInfoSource, evaluate_extended, lower,
    lower_to_render_plan_extended,
};
use hsk_studio_folio::{StudioRasterRecords, TileRef};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, ColourTag, MaskFormat, MaskInput, OutputTarget, PlanNodeId, SampleFormat,
};
use serde_json::{Value, json};

mod common;
use common::*;

fn target() -> OutputTarget {
    OutputTarget {
        extent: Rect { x: 0, y: 0, width: 128, height: 128 },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
    }
}
fn plane_hash(key: &str, salt: u8) -> [u8; 32] {
    [key.bytes().fold(salt, |a, b| a.wrapping_mul(31).wrapping_add(b)); 32]
}
/// Caller double: tile planes, and one stitched mask plane per mask hashed from its first key;
/// `resolve_masks: false` cannot resolve any mask.
struct Info {
    resolve_masks: bool,
}
impl TileInfoSource for Info {
    fn tile_info(&self, tile: &TileRef) -> Option<TileInfo> {
        Some(TileInfo {
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: plane_hash(&tile.object_key, 1),
            alpha_sha256: plane_hash(&tile.object_key, 2),
        })
    }
    fn mask_sha256(&self, tiles: &[TileRef]) -> Option<[u8; 32]> {
        self.resolve_masks.then(|| plane_hash(&tiles[0].object_key, 3))
    }
}
fn coverage_tile(key: &str, column: i64, w: u32, h: u32) -> Value {
    let mut t = tile(2, key, column, 0, w, h);
    t["format"] = json!("coverage_u8");
    t
}
/// Group 1 = raster 2 (tiles t00 64x64, t10 64x32) then raster 3 (t01): the `tiled` fixture of
/// render_plan_lowering.rs.
fn doc() -> Value {
    let mut v = mixed();
    assign_all_profiles(&mut v);
    v["layers"][1]["payload"] = json!({"payload_kind":"raster","tiles":[tile(2,"t00",0,0,64,64),tile(2,"t10",1,0,64,32)]});
    v["layers"][2]["payload"] = json!({"payload_kind":"raster","tiles":[tile(3,"t01",0,1,64,64)]});
    v
}
fn mask(tiles: Vec<Value>, enabled: bool) -> Value {
    json!({"tiles": tiles, "enabled": enabled, "linked": true})
}
/// Layer 2's stack: blur with a two-tile mask (32x32 at column 0, 32x16 at column 1), noise with
/// a disabled mask, a disabled median; a one-tile stack mask.
fn stack() -> Value {
    json!({"enabled": true,
        "stack_mask": mask(vec![coverage_tile("sm", 0, 48, 48)], true),
        "filters": [
            {"instance_id":"blur","enabled":true,"opacity":100.0,"params":{"filter_kind":"gaussian_blur","radius_px":2.0},
             "mask": mask(vec![coverage_tile("fa", 0, 32, 32), coverage_tile("fb", 1, 32, 16)], true)},
            {"instance_id":"noise","enabled":true,"opacity":50.0,"params":{"filter_kind":"add_noise","amount_pct":10.0,"distribution":"uniform","monochromatic":false,"seed":1},
             "mask": mask(vec![coverage_tile("fc", 0, 16, 16)], false)},
            {"instance_id":"median","enabled":false,"opacity":100.0,"params":{"filter_kind":"median","radius_px":1}}
        ]})
}
fn records(stack: Value) -> StudioRasterRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.raster_records@1","document_id":identity("SDOC",1),
        "layers":[{"layer_id":identity("SLYR",2),"smart_filters":stack}]}))
    .expect("records shape")
}
fn lowered(stack: Value, resolve_masks: bool) -> Result<ExtendedLowering, CompositeError> {
    let snap = open(&doc());
    let token = CancellationToken::default();
    let recs = records(stack);
    let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
    let report = evaluate_extended(&plan, &snap, &recs, &token).unwrap();
    lower_to_render_plan_extended(&plan, &report, &snap, &recs, &addr(1, "mix", "out"), target(), &Info { resolve_masks })
}

#[test]
fn smart_filter_masks_register_as_plan_mask_sources() {
    let low = lowered(stack(), true).expect("lowers");
    // Nodes: tiles 0, 1 -> group 2 (layer 2), tile 3 (layer 3), root 4: layer 2 is root layer 0.
    let ext = low.extensions.extension(PlanNodeId(4), 0).expect("entry of layer 2");
    let op = ext.smart_filters.as_ref().expect("smart filters lowered");
    assert_eq!(op.entries.len(), 3);
    assert_eq!(op.entries.iter().map(|e| (e.enabled, e.opacity)).collect::<Vec<_>>(), vec![(true, 1.0), (true, 0.5), (false, 1.0)]);
    let blur = op.entries[0].mask.expect("blur mask registered");
    // Grid 32x32: tile fa at column 0 (0..32), tile fb at column 1 (32..64, 16 rows): union 64x32.
    assert_eq!(
        blur,
        MaskInput { key: blur.key, placement: Rect { x: 0, y: 0, width: 64, height: 32 }, format: MaskFormat::CoverageU8, sha256: plane_hash("fa", 3), inverted: false, density: 1.0 }
    );
    assert!(op.entries[1].mask.is_none(), "a disabled mask applies everywhere");
    assert!(op.entries[2].mask.is_none());
    let stack_mask = op.stack_mask.expect("stack mask registered");
    assert_eq!((stack_mask.placement, stack_mask.sha256), (Rect { x: 0, y: 0, width: 48, height: 48 }, plane_hash("sm", 3)));
    // Each registered key names exactly that mask's tiles in the mask table.
    let keys = |k| low.mask_sources.iter().find(|(key, _)| *key == k).map(|(_, t)| t.iter().map(|t| t.object_key.clone()).collect::<Vec<_>>());
    assert_eq!(keys(blur.key), Some(vec!["fa".to_string(), "fb".to_string()]));
    assert_eq!(keys(stack_mask.key), Some(vec!["sm".to_string()]));
    assert_ne!(blur.key, stack_mask.key);
    assert!(!low.mask_sources.iter().any(|(_, t)| t.iter().any(|t| t.object_key == "fc")), "disabled mask not registered");
}

#[test]
fn disabled_stacks_and_refusals_are_typed() {
    // A disabled stack, or one with no enabled entry, lowers to nothing.
    let mut off = stack();
    off["enabled"] = json!(false);
    assert!(lowered(off, true).unwrap().extensions.extension(PlanNodeId(4), 0).and_then(|e| e.smart_filters.as_ref()).is_none());
    let mut none_on = stack();
    for f in none_on["filters"].as_array_mut().unwrap() {
        f["enabled"] = json!(false);
    }
    assert!(lowered(none_on, true).unwrap().extensions.extension(PlanNodeId(4), 0).and_then(|e| e.smart_filters.as_ref()).is_none());
    // An unresolvable mask plane, and a stack pigment refuses (median radius over 500).
    let reason = |r: Result<ExtendedLowering, CompositeError>| match r {
        Err(CompositeError::Unavailable(s)) => s,
        other => panic!("expected Unavailable, got {other:?}"),
    };
    assert_eq!(reason(lowered(stack(), false)), format!("{}/smart_filters:mask_hash", identity("SLYR", 2)));
    let mut bad = stack();
    bad["filters"][2] = json!({"instance_id":"median","enabled":true,"opacity":100.0,"params":{"filter_kind":"median","radius_px":1000}});
    assert_eq!(reason(lowered(bad, true)), format!("{}:smart_filter_parameter", identity("SLYR", 2)));
}
