//! Lane SMB-PS-ADJUST-STYLES slice 4: placed assets (smart objects). Folio validates the shared
//! sources and instances (refusing cycles, depth, dangling sources, singular transforms,
//! non-raster hosts, absolute or scheme URIs and self-embedding); composite turns each instance
//! into a precomposition scene layer of its shared source (`lower_extensions::placed`). Pixel
//! proof (rendering and source edits reaching every instance): render-cpu `tests/cpu_placed.rs`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::lower_extensions::placed::{
    PLACED_LINK_MISSING, PLACED_SOURCE_SIZE, PLACED_SOURCE_UNRESOLVED, placed_matrix, placed_scene_layer,
};
use hsk_studio_composite::{CompositeError, SceneComposition, SceneContent};
use hsk_studio_folio::{
    Budget, Code, Resolution, Resolver, StudioDocument, StudioRasterRecords, TileRef, validate_raster_records,
};
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn identity(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
struct Caller;
impl Resolver for Caller {
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
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        true
    }
}
/// Raster layers 1 and 2 (the instances) and a group 3.
fn document() -> StudioDocument {
    let layer = |n: u32, kind: &str, payload: Value| {
        json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",n),"name":format!("l{n}"),
            "parent":null,"kind":kind,"payload":payload,"visible":true})
    };
    serde_json::from_value(json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
        "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[layer(1, "raster", json!({"payload_kind":"raster","tiles":[]})),
                  layer(2, "raster", json!({"payload_kind":"raster","tiles":[]})),
                  layer(3, "group", json!({"payload_kind":"group"}))],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    }))
    .expect("document shape")
}
fn source(id: &str, uses: &[&str], mode: Value) -> Value {
    json!({"source_id":id,"location":mode,"width":2.0,"height":2.0,"uses":uses})
}
fn embedded() -> Value {
    json!({"mode":"embedded","document_id":identity("SDOC",2),"content_digest":"sha256-ab12"})
}
fn records(sources: Vec<Value>, instances: Vec<(u32, &str, [f64; 6])>) -> Value {
    let layers: Vec<Value> = instances
        .into_iter()
        .map(|(n, s, t)| json!({"layer_id":identity("SLYR",n),"placed_asset":{"source_id":s,"transform":t}}))
        .collect();
    json!({"schema_id":"hsk.studio.raster_records@1","document_id":identity("SDOC",1),"layers":layers,
        "placed_sources":sources})
}
fn validate(v: &Value) -> Result<StudioRasterRecords, Code> {
    let r: StudioRasterRecords = serde_json::from_value(v.clone()).map_err(|_| Code::InvalidField)?;
    validate_raster_records(&r, &document(), Budget::default(), &CancellationToken::default(), &Caller).map_err(|d| d.code)?;
    let again: StudioRasterRecords = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
    assert_eq!(again, r);
    Ok(r)
}
fn source_scene(rgba: [f32; 4], size: u32) -> SceneComposition {
    SceneComposition {
        composition_id: "src1".into(),
        width: size,
        height: size,
        background: Some(rgba),
        filter: ResampleFilter::Bilinear,
        layers: Vec::new(),
    }
}

#[test]
fn placed_records_validate_lower_to_shared_precomps_and_refuse_hostile_input() {
    let shift = |x: f64| [1.0, 0.0, 0.0, 1.0, x, 0.0];
    let recs = validate(&records(vec![source("src1", &[], embedded())], vec![(1, "src1", shift(1.0)), (2, "src1", shift(4.0))]))
        .expect("valid placed records");
    // Both instances are precompositions of the SAME source scene with their own transforms
    // (Folio [a, b, c, d, e, f] -> pigment row-major [a, c, e, b, d, f]); a new source scene
    // reaches both.
    let blue = [0.2, 0.4, 0.6, 1.0];
    for colour in [blue, [0.9, 0.1, 0.1, 1.0]] {
        let mut sources = BTreeMap::new();
        sources.insert("src1".to_owned(), source_scene(colour, 2));
        for (n, x) in [(1u32, 1.0), (2, 4.0)] {
            let layer = placed_scene_layer(&identity("SLYR", n), &recs, &sources, StudioBlendMode::Multiply, 0.5, 0.8)
                .unwrap()
                .unwrap();
            assert_eq!(layer.content, SceneContent::Precomp(Box::new(source_scene(colour, 2))));
            assert_eq!(layer.matrix, Affine2::translate(x, 0.0));
            assert_eq!((layer.mode, layer.opacity, layer.fill_opacity, layer.clipped), (StudioBlendMode::Multiply, 0.5, 0.8, false));
        }
    }
    assert_eq!(placed_matrix([1.0, 2.0, 3.0, 4.0, 5.0, 6.0]).m, [1.0, 3.0, 5.0, 2.0, 4.0, 6.0]);

    // Composite refusals: an unresolved source scene, a source scene of another size, a linked
    // source whose health is missing.
    let refused = |r: &StudioRasterRecords, sources: &BTreeMap<String, SceneComposition>| {
        match placed_scene_layer(&identity("SLYR", 1), r, sources, StudioBlendMode::Normal, 1.0, 1.0) {
            Err(CompositeError::Unavailable(m)) => m,
            other => panic!("{other:?}"),
        }
    };
    assert!(refused(&recs, &BTreeMap::new()).ends_with(PLACED_SOURCE_UNRESOLVED));
    let mut wrong = BTreeMap::new();
    wrong.insert("src1".to_owned(), source_scene(blue, 3));
    assert!(refused(&recs, &wrong).ends_with(PLACED_SOURCE_SIZE));
    let linked = |health: &str| json!({"mode":"linked","uri":"assets/logo.psd","content_digest":"sha256-cd34","health":health});
    let missing = validate(&records(vec![source("src1", &[], linked("missing"))], vec![(1, "src1", shift(1.0))])).unwrap();
    assert!(refused(&missing, &BTreeMap::new()).ends_with(PLACED_LINK_MISSING));
    // A layer without a record is simply not placed.
    assert_eq!(placed_scene_layer(&identity("SLYR", 3), &recs, &BTreeMap::new(), StudioBlendMode::Normal, 1.0, 1.0).unwrap(), None);

    // Folio refusals: recursion (a source placing itself through another), a dangling source,
    // a singular transform, NaN, a group host, an absolute or scheme URI, self-embedding.
    let cyc = records(vec![source("a", &["b"], embedded()), source("b", &["a"], embedded())], vec![(1, "a", shift(0.0))]);
    assert_eq!(validate(&cyc).unwrap_err(), Code::RenderCycle);
    let selfish = records(vec![source("a", &["a"], embedded())], vec![]);
    assert_eq!(validate(&selfish).unwrap_err(), Code::RenderCycle);
    assert_eq!(validate(&records(vec![source("a", &[], embedded())], vec![(1, "b", shift(0.0))])).unwrap_err(), Code::DanglingReference);
    assert_eq!(validate(&records(vec![source("a", &[], embedded())], vec![(1, "a", [1.0, 2.0, 2.0, 4.0, 0.0, 0.0])])).unwrap_err(), Code::InvalidField);
    let nan = records(vec![source("a", &[], embedded())], vec![(1, "a", shift(0.0))]);
    let mut nan_text = serde_json::to_string(&nan).unwrap();
    nan_text = nan_text.replacen("[1.0,0.0,0.0,1.0,0.0,0.0]", "[1.0,0.0,0.0,1.0,1e999,0.0]", 1);
    assert!(serde_json::from_str::<StudioRasterRecords>(&nan_text).is_err() || validate(&serde_json::from_str(&nan_text).unwrap()).is_err());
    assert_eq!(validate(&records(vec![source("a", &[], embedded())], vec![(3, "a", shift(0.0))])).unwrap_err(), Code::PayloadMismatch);
    for uri in ["/abs/logo.psd", "https://x/logo.psd"] {
        let bad = json!({"mode":"linked","uri":uri,"content_digest":"d","health":"up_to_date"});
        assert_eq!(validate(&records(vec![source("a", &[], bad)], vec![])).unwrap_err(), Code::InvalidField, "{uri}");
    }
    let me = json!({"mode":"embedded","document_id":identity("SDOC",1),"content_digest":"d"});
    assert_eq!(validate(&records(vec![source("a", &[], me)], vec![])).unwrap_err(), Code::InvalidField);
    // A chain deeper than MAX_PLACED_DEPTH (16) is refused; 16 links are fine.
    let chain = |n: usize| {
        let ids: Vec<String> = (0..=n).map(|i| format!("s{i}")).collect();
        let sources = (0..=n)
            .map(|i| {
                let uses: Vec<&str> = if i < n { vec![ids[i + 1].as_str()] } else { vec![] };
                source(&ids[i], &uses, embedded())
            })
            .collect();
        records(sources, vec![])
    };
    assert!(validate(&chain(16)).is_ok());
    assert_eq!(validate(&chain(17)).unwrap_err(), Code::InvalidField);
}
