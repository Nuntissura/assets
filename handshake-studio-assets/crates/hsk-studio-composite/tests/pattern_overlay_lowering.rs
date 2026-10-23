//! Lane SMB-PS-ADJUST-STYLES-2: composite's lowering of a Folio pattern overlay whose record carries
//! its resolved pattern tile: the tile is registered as a plan tile source (key in the
//! `SourceTable`, hashes from the caller's `TileInfoSource`) and the pigment op carries that key,
//! the tile size, the scale fraction, the phase and the link flag. A record without a resolved
//! tile, a tile the caller cannot resolve and an out-of-range scale are typed refusals.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{
    BlendSpace, CompositeError, ExtendedLowering, Limits, TileInfo, TileInfoSource, evaluate_extended, lower,
    lower_to_render_plan_extended,
};
use hsk_studio_folio::{StudioRasterRecords, TileRef};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::effect_math::EffectOp;
use hsk_studio_pigment::render_plan::{AlphaConvention, ColourTag, OutputTarget, PlanNodeId, SampleFormat};
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
    resolve: bool,
}
impl TileInfoSource for Info {
    fn tile_info(&self, tile: &TileRef) -> Option<TileInfo> {
        if !self.resolve && tile.object_key == "pt" {
            return None;
        }
        Some(TileInfo {
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: plane_hash(&tile.object_key, 1),
            alpha_sha256: plane_hash(&tile.object_key, 2),
        })
    }
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
fn pattern(tile: Option<Value>, scale: f64) -> Value {
    let mut p = json!({"effect_kind":"pattern_overlay","pattern":{"schema_id":"hsk.studio.pattern@1","object_id":"p1"},
        "scale":scale,"phase_x":2.0,"phase_y":-1.0,"link_with_layer":false});
    if let Some(t) = tile {
        p["pattern_tile"] = t;
    }
    json!({"effect_stack_id":"SEFX-019abcde-0000-7000-8000-000000000001","enabled":true,"scale":100.0,
        "effects":[{"instance_id":"po","enabled":true,"visible_in_editor":true,"blend_mode":10,"opacity":60.0,
            "colour":{"r":0.0,"g":0.0,"b":0.0,"colour_profile_id":null},"anti_alias":false,"noise":0.0,"angle":0.0,
            "use_global_light":false,"params":p}]})
}
fn lowered(stack: Value, resolve: bool) -> Result<ExtendedLowering, CompositeError> {
    let snap = open(&doc());
    let token = CancellationToken::default();
    let recs: StudioRasterRecords = serde_json::from_value(json!({"schema_id":"hsk.studio.raster_records@1",
        "document_id":identity("SDOC",1),"layers":[{"layer_id":identity("SLYR",2),"effects":stack}]}))
    .expect("records shape");
    let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
    let report = evaluate_extended(&plan, &snap, &recs, &token).unwrap();
    lower_to_render_plan_extended(&plan, &report, &snap, &recs, &addr(1, "mix", "out"), target(), &Info { resolve })
}

#[test]
fn pattern_overlay_registers_its_tile_and_refuses_typed() {
    let mut pt = tile(2, "pt", 0, 0, 4, 3);
    pt["format"] = json!("rgb_f32le");
    let low = lowered(pattern(Some(pt.clone()), 150.0), true).expect("lowers");
    // Nodes: tiles 0, 1 -> group 2 (layer 2), tile 3 (layer 3), root 4: layer 2 is root layer 0.
    let ext = low.extensions.extension(PlanNodeId(4), 0).expect("entry of layer 2");
    let [EffectOp::PatternOverlay(o)] = ext.effects.as_slice() else { panic!("{:?}", ext.effects) };
    assert_eq!((o.pattern.width, o.pattern.height), (4, 3));
    assert_eq!((o.pattern.colour_sha256, o.pattern.alpha_sha256), (plane_hash("pt", 1), plane_hash("pt", 2)));
    assert_eq!((o.scale, o.phase, o.link_with_layer, o.opacity), (1.5, [2.0, -1.0], false, 0.6));
    let source = low.sources.iter().find(|(k, _)| *k == o.pattern.key).expect("registered source");
    assert_eq!(source.1.object_key, "pt");
    // typed refusals
    let reason = |r: Result<ExtendedLowering, CompositeError>| match r {
        Err(CompositeError::Unavailable(s)) => s,
        other => panic!("expected Unavailable, got {other:?}"),
    };
    assert!(reason(lowered(pattern(None, 100.0), true)).ends_with("pattern_overlay_unresolved"));
    assert_eq!(reason(lowered(pattern(Some(pt.clone()), 100.0), false)), format!("{}/tile/pt:tile_info", identity("SLYR", 2)));
    assert!(reason(lowered(pattern(Some(pt), 5000.0), true)).ends_with("effect_parameter"));
}
