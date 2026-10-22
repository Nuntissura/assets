//! Lane SMB-AE-MOTION-RECORDS slices 2-4: saved Folio motion records render in a composition.
//! A shape layer record (rect with a keyframed position and a linear gradient fill) and a
//! text-animator record (expression selector moving each glyph up by `textIndex / textTotal`) make
//! two empty raster layers procedural; every plane, gradient sample and offset is hand-computed.
#![cfg(feature = "motion")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::temporal::TemporalError;
use hsk_studio_composite::temporal::composition::{CompositionInput, CompositionTemporalSource};
use hsk_studio_composite::temporal::motion_records::{ExpressionSelectorEngine, ProviderSelectorEngine};
use hsk_studio_composite::text::animate::glyph_from_outline;
use hsk_studio_composite::*;
use hsk_studio_folio::{Budget, StudioCompositionRecords, StudioMotionRecords, TileRef};
use hsk_studio_motion::{ExprError, ExprRequest, ExpressionProvider, Fuel, HostRead, NoProvider};
use hsk_studio_pigment::render_plan::{ColourTag, SourceKey};
use hsk_studio_pigment::Rect;
use hsk_studio_type::{GlyphOutline, OutlineCommand};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

mod common;
use common::*;

const S: u64 = 254_016_000_000;
const SPACE: ColourTag = ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded };

fn info(_: &TileRef) -> Option<TileInfo> {
    None
}

fn document() -> Value {
    let mut v = mixed();
    assign_all_profiles(&mut v);
    v["geometry_unit"] = json!("px");
    v
}

fn compositions() -> StudioCompositionRecords {
    let layer = |n: u8| json!({"layer_id":identity("SLYR",n),
        "timing":{"in_point":0,"out_point":S,"start_time":0,"stretch":{"numerator":1,"denominator":1}}});
    serde_json::from_value(json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),
        "compositions":[{"schema_id":"hsk.studio.composition@1","composition_id":identity("SCOM",9),"name":"c","width":64,"height":32,
            "pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":25,"denominator":1},
            "duration":S,"layers":[layer(2),layer(3)]}]}))
    .unwrap()
}

fn motion() -> StudioMotionRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.motion_records@1","document_id":identity("SDOC",1),
        "shape_layers":[{"layer_id":identity("SLYR",2),"root":{"items":[
            {"kind":"rect","position":[8.0,8.0],"size":[16.0,16.0]},
            {"kind":"gradient_fill","opacity":100.0,"rule":"non_zero","gradient":{"radial":false,"start":[0.0,0.0],"end":[16.0,0.0],
                "stops":[{"offset":0.0,"rgba":[1.0,0.0,0.0,1.0]},{"offset":1.0,"rgba":[0.0,0.0,1.0,1.0]}]}}]},
            "streams":[{"path":[0],"field":"rect_position","value":{"static_value":[8.0,8.0],"keyframes":[
                {"time":0,"value":[8.0,8.0],"in_interp":"linear","out_interp":"linear"},
                {"time":S,"value":[40.0,8.0],"in_interp":"linear","out_interp":"linear"}]}}]}],
        "text_animators":[{"layer_id":identity("SLYR",3),"animators":[{"name":"rise","properties":{"position":[0.0,-10.0]},
            "selectors":[{"kind":"expression","based_on":"characters","source":"textIndex / textTotal * 100"}]}]}]}))
    .unwrap()
}

fn square() -> GlyphOutline {
    GlyphOutline::synthetic(1000, vec![
        OutlineCommand::MoveTo([0.0, 0.0]),
        OutlineCommand::LineTo([500.0, 0.0]),
        OutlineCommand::LineTo([500.0, 500.0]),
        OutlineCommand::LineTo([0.0, 500.0]),
        OutlineCommand::Close,
    ])
}

fn glyphs() -> BTreeMap<String, (Vec<hsk_studio_composite::text::animate::AnimatedGlyph>, [f32; 4])> {
    let s = square();
    let g = vec![
        glyph_from_outline(&s, [10.0, 30.0], 0.02, 12.0, 20.0, 0).unwrap(),
        glyph_from_outline(&s, [22.0, 30.0], 0.02, 12.0, 20.0, 0).unwrap(),
    ];
    BTreeMap::from([(identity("SLYR", 3), (g, [1.0, 1.0, 1.0, 1.0]))])
}

/// A stand-in expression engine behind motion's provider port: reads the `textIndex` / `textTotal`
/// prelude the selector bridge writes and returns `textIndex / textTotal * 100`.
struct PreludeProvider;
impl ExpressionProvider for PreludeProvider {
    fn profile_id(&self) -> &'static str {
        "test-prelude"
    }
    fn evaluate(&self, r: &ExprRequest<'_>, _: &dyn HostRead, _: &mut Fuel, _: &CancellationToken) -> Result<Vec<f64>, ExprError> {
        let num = |name: &str| -> f64 {
            let at = r.source.find(&format!("var {name} = ")).unwrap() + name.len() + 7;
            r.source[at..].split(';').next().unwrap().trim().parse().unwrap()
        };
        assert!(r.source.ends_with("textIndex / textTotal * 100"));
        assert_eq!((r.dimension, r.base.map(|b| b.len())), (1, Some(1)));
        Ok(vec![num("textIndex") / num("textTotal") * 100.0])
    }
}

fn source(engine: Option<Arc<dyn ExpressionSelectorEngine>>) -> Result<CompositionTemporalSource, TemporalError> {
    let snap = open(&document());
    let rec = compositions();
    let id = identity("SCOM", 9);
    CompositionTemporalSource::new(
        &CompositionInput {
            snapshot: &snap, expected_revision: 7, records: &rec, composition_id: &id, budget: Budget::default(),
            resolver: &Available, blend_space: BlendSpace::Encoded, info: &info, space: SPACE, inputs: RenderInputs::default(),
        },
        &CancellationToken::default(),
    )?
    .with_motion_records(&motion(), &glyphs(), SPACE, 7, engine)
}

fn layer(scene: &SceneComposition, n: u8) -> &StaticContent {
    let l = scene.layers.iter().find(|l| l.layer_id == identity("SLYR", n)).expect("layer");
    let SceneContent::Static(c) = &l.content else { panic!("procedural static content: {:?}", l.content) };
    c
}

fn rgb(c: &StaticContent, key: u32, x: usize) -> [f32; 3] {
    let t = &c.owned_tiles[&SourceKey(key)];
    let f = |i: usize| f32::from_le_bytes(t.colour[i * 4..i * 4 + 4].try_into().unwrap());
    [f(x * 3), f(x * 3 + 1), f(x * 3 + 2)]
}

#[test]
fn saved_shape_and_text_records_render_in_the_composition() {
    let engine: Arc<dyn ExpressionSelectorEngine> = Arc::new(ProviderSelectorEngine { provider: PreludeProvider, fuel_per_unit: 16 });
    let src = source(Some(engine)).unwrap();
    let cancel = CancellationToken::default();

    // Frame 0: rect centre (8, 8) => x 0..16, y 0..16, the linear gradient (0,0)->(16,0) red -> blue
    // sampled at pixel centres: x = 0.5 => t = 1/32 (r 0.96875, b 0.03125), x = 15.5 => t = 31/32.
    let s0 = src.scene_at(0, &cancel).unwrap();
    let shape = layer(&s0, 2);
    assert_eq!(shape.owned_masks[&SourceKey(0)].bounds, Rect { x: 0, y: 0, width: 16, height: 16 });
    for (x, want) in [(0usize, [0.96875f32, 0.0, 0.03125]), (15, [0.03125, 0.0, 0.96875])] {
        let got = rgb(shape, 0, x);
        assert!(got.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-6), "x {x}: {got:?}");
    }
    // Half way (0.5 s): rect_position (24, 8) => x 16..32; the gradient stays in shape space, so
    // every covered pixel is past its end: pad = blue.
    let s1 = src.scene_at((S / 2) as i64, &cancel).unwrap();
    let moved = layer(&s1, 2);
    assert_eq!(moved.owned_masks[&SourceKey(0)].bounds, Rect { x: 16, y: 0, width: 16, height: 16 });
    assert_eq!(rgb(moved, 0, 3), [0.0, 0.0, 1.0]);

    // Text: textIndex / textTotal = 1/2 and 2/2 => glyphs move up 5 and 10 px:
    // glyph 0 x 10..20, y 20..30 -> y 15..25; glyph 1 x 22..32 -> y 10..20.
    let text = layer(&s0, 3);
    assert_eq!(text.owned_masks[&SourceKey(0)].bounds, Rect { x: 10, y: 15, width: 10, height: 10 });
    assert_eq!(text.owned_masks[&SourceKey(1)].bounds, Rect { x: 22, y: 10, width: 10, height: 10 });
}

#[test]
fn missing_inputs_and_engines_are_typed_refusals() {
    let cancel = CancellationToken::default();
    // No expression engine: the expression selector is refused at evaluation, never ignored.
    let src = source(None).unwrap();
    let e = src.scene_at(0, &cancel).unwrap_err();
    assert!(matches!(&e, TemporalError::Source { code, .. } if code == "text_expression_selector"), "{e:?}");
    // An engine whose provider refuses (motion's pure closure): typed failure.
    let none: Arc<dyn ExpressionSelectorEngine> = Arc::new(ProviderSelectorEngine { provider: NoProvider, fuel_per_unit: 16 });
    let e = source(Some(none)).unwrap().scene_at(0, &cancel).unwrap_err();
    assert!(matches!(&e, TemporalError::Source { code, .. } if code == "text_expression_selector_failed"), "{e:?}");
    // Text-animator records without the caller's glyphs are refused when attached.
    let snap = open(&document());
    let rec = compositions();
    let id = identity("SCOM", 9);
    let base = CompositionTemporalSource::new(
        &CompositionInput {
            snapshot: &snap, expected_revision: 7, records: &rec, composition_id: &id, budget: Budget::default(),
            resolver: &Available, blend_space: BlendSpace::Encoded, info: &info, space: SPACE, inputs: RenderInputs::default(),
        },
        &cancel,
    )
    .unwrap();
    let e = base.with_motion_records(&motion(), &BTreeMap::new(), SPACE, 7, None).unwrap_err();
    assert!(matches!(&e, TemporalError::Source { code, .. } if code == "text_animator_glyphs"), "{e:?}");
}
