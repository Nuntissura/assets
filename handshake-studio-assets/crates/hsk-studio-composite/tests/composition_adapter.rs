//! Lane SMB-COMPOSITE-COMPOSITION-ADAPTER: a stored Folio composition (`hsk.studio.composition@1`)
//! evaluated by `CompositionTemporalSource` (feature `motion`) into the scenes of
//! `tests/support/composition_scenes.rs`, lowered by `lower_scene`, and the typed refusals.
//! Pixel proofs (CPU and GPU against the shared oracle) live in render-cpu / render-gpu
//! `tests/composition_frames.rs` (decision B1).
#![cfg(feature = "motion")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::temporal::composition::{
    COMPOSITION_LAYER_CLIPPED, COMPOSITION_LAYER_CONTENT, COMPOSITION_PRECOMP_COLLAPSE,
    COMPOSITION_PRECOMP_ON_DOCUMENT_GRAPH, COMPOSITION_PRECOMP_PAYLOAD, COMPOSITION_TRACK_MATTE, CompositionInput, CompositionTemporalSource,
};
use hsk_studio_composite::temporal::TRANSFORM_MULTI_TILE_LAYER;
use hsk_studio_composite::{
    BlendSpace, CompositeError, FrameRequest, Limits, SceneContent, SceneContext, SceneLimits,
    TemporalError, TileInfo, TileInfoSource, lower_frame, lower_scene,
};
use hsk_studio_folio::{Budget, Snapshot, StudioCompositionRecords, TileRef};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BackendIdentity, DeviceClass, ExecuteRequest, Exactness, NoVector,
    Operator, OutputTarget, PlanNode, PlanSources, RenderBackend, RenderPlan, RenderReceipt, SampleFormat,
    TileSink,
};
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};
use serde_json::{Value, json};

mod common;
#[path = "support/composition_scenes.rs"]
mod scenes;
use common::{Available, addr, bounded_case, open};
use scenes::{Pixels, TICKS_PER_FRAME, composition_id, expected_scene, identity};

fn tile(layer: u8, key: &str, w: u32, h: u32) -> Value {
    json!({"object_key":key,"layer_id":identity("SLYR",layer),"column":0,"row":0,
        "width":{"value":w,"unit":"px"},"height":{"value":h,"unit":"px"},"format":"caller-format",
        "colour_profile_id":identity("SCPF",1),"schema_id":"hsk.studio.raster_tile@1",
        "content_digest":{"algorithm":"sha256","digest":"00"},"artifact_manifest_id":"manifest-1"})
}
fn raster(n: u8, tiles: Vec<Value>) -> Value {
    json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",n),"name":format!("layer-{n}"),
        "parent":null,"kind":"raster","payload":{"payload_kind":"raster","tiles":tiles},"visible":true})
}
fn source_op(n: u8) -> Value {
    json!({"layer_id":identity("SLYR",n),"operation_key":"source","operation_kind":"source","inputs":[],
        "outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"source"}]})
}
/// Document: group 1 composites raster 2 (ordinal 0, bottom) and raster 3 (ordinal 1), the same
/// stack as the composition; raster 4 (two tiles) and vector 5 are extra layers for refusals.
fn document() -> Value {
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"px",
        "colour_mode":3,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[
            {"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",1),"name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true},
            raster(2, vec![tile(2,"ta",8,6)]),
            raster(3, vec![tile(3,"tb",6,6)]),
            raster(4, vec![tile(4,"t0",4,4), {let mut t = tile(4,"t1",4,4); t["column"]=json!(1); t}]),
            {"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",5),"name":"vector","parent":null,"kind":"vector","payload":{"payload_kind":"vector","path_ids":[identity("SVPT",1)]},"visible":true},
            {"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",6),"name":"null","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true},
            raster(7, vec![tile(7,"tc",6,4)]),
            {"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",8),"name":"precomp","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true},
            raster(9, vec![tile(9,"td",6,6)])
        ],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[source_op(2),source_op(3),
            {"layer_id":identity("SLYR",1),"operation_key":"mix","operation_kind":"composite",
             "inputs":[{"port_key":"inputs","value_type":"image","cardinality":"many","semantic":"ordered_composite"}],
             "outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"composite_result"}]}],
            "edges":[
                {"source":common::address(2,"source","out"),"target":common::address(1,"mix","inputs"),"dependency_kind":"data","input_ordinal":0},
                {"source":common::address(3,"source","out"),"target":common::address(1,"mix","inputs"),"dependency_kind":"data","input_ordinal":1}],
            "outputs":[common::address(1,"mix","out")]}
    });
    common::assign_all_profiles(&mut v);
    v
}
fn f(frame: u64) -> u64 {
    frame * TICKS_PER_FRAME
}
fn key(frame: u64, value: Value) -> Value {
    json!({"time":f(frame),"value":value,"in_interp":"linear","out_interp":"linear"})
}
fn timing(in_frame: u64, out_frame: u64) -> Value {
    json!({"in_point":f(in_frame),"out_point":f(out_frame),"start_time":0,"stretch":{"numerator":1,"denominator":1}})
}
/// The records of `tests/support/composition_scenes.rs` (module docs there).
fn records() -> Value {
    json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),"compositions":[{
        "schema_id":"hsk.studio.composition@1","composition_id":composition_id(),"name":"comp",
        "width":32,"height":24,"pixel_aspect":{"numerator":1,"denominator":1},
        "frame_rate":{"numerator":24,"denominator":1},"duration":f(48),
        "background":{"r":0.1,"g":0.2,"b":0.3,"a":1.0,"colour_profile_id":null},
        "layers":[
            {"layer_id":identity("SLYR",3),"timing":timing(2,40),"streams":[
                {"property":"position","static_value":[0.0,0.0],"keyframes":[key(0,json!([5.0,5.0])),key(10,json!([15.5,12.25]))]},
                {"property":"scale","static_value":[100.0,100.0],"keyframes":[key(0,json!([100.0,100.0])),key(10,json!([150.0,150.0]))]},
                {"property":"rotation","static_value":[0.0],"keyframes":[key(0,json!([0.0])),key(10,json!([30.0]))]},
                {"property":"opacity","static_value":[100.0],"keyframes":[key(0,json!([100.0])),key(10,json!([50.0]))]}]},
            {"layer_id":identity("SLYR",2),"timing":timing(0,20),"streams":[
                {"property":"position","static_value":[12.0,9.0]},
                {"property":"opacity","static_value":[80.0]}]}
        ]}, composition2(), composition3(), composition4()]})
}
/// Composition 3 of `tests/support/composition_scenes.rs` (`expected_scene3`).
fn composition3() -> Value {
    json!({"schema_id":"hsk.studio.composition@1","composition_id":scenes::composition3_id(),"name":"outer",
        "width":32,"height":24,"pixel_aspect":{"numerator":1,"denominator":1},
        "frame_rate":{"numerator":24,"denominator":1},"duration":f(48),
        "background":{"r":0.2,"g":0.2,"b":0.2,"a":1.0,"colour_profile_id":null},
        "layers":[
            {"layer_id":identity("SLYR",8),
             "timing":{"in_point":0,"out_point":f(48),"start_time":f(2),"stretch":{"numerator":2,"denominator":1}},
             "precomp":{"composition_id":scenes::composition4_id(),"collapse_transformations":false},
             "streams":[
                {"property":"position","static_value":[16.0,12.0]},
                {"property":"rotation","static_value":[15.0]}]}
        ]})
}
/// Composition 4 (nested, 12 frames, red background that is not drawn when nested).
fn composition4() -> Value {
    json!({"schema_id":"hsk.studio.composition@1","composition_id":scenes::composition4_id(),"name":"inner",
        "width":12,"height":10,"pixel_aspect":{"numerator":1,"denominator":1},
        "frame_rate":{"numerator":24,"denominator":1},"duration":f(12),
        "background":{"r":1.0,"g":0.0,"b":0.0,"a":1.0,"colour_profile_id":null},
        "layers":[
            {"layer_id":identity("SLYR",9),"timing":timing(0,12),"streams":[
                {"property":"position","static_value":[0.0,0.0],"keyframes":[key(0,json!([3.0,3.0])),key(10,json!([9.0,7.0]))]}]}
        ]})
}
/// Composition 2 of `tests/support/composition_scenes.rs` (`expected_scene2`).
fn composition2() -> Value {
    json!({"schema_id":"hsk.studio.composition@1","composition_id":scenes::composition2_id(),"name":"parented",
        "width":32,"height":24,"pixel_aspect":{"numerator":1,"denominator":1},
        "frame_rate":{"numerator":24,"denominator":1},"duration":f(48),
        "layers":[
            {"layer_id":identity("SLYR",6),"timing":timing(0,48),"streams":[
                {"property":"position","static_value":[16.0,12.0]},
                {"property":"scale","static_value":[150.0,75.0]},
                {"property":"rotation","static_value":[0.0],"keyframes":[key(0,json!([0.0])),key(10,json!([90.0]))]}]},
            {"layer_id":identity("SLYR",7),"timing":timing(0,48),"parent":identity("SLYR",6),"streams":[
                {"property":"anchor","static_value":[0.0,0.0],"keyframes":[key(0,json!([0.0,0.0])),key(10,json!([6.0,4.0]))]},
                {"property":"position","static_value":[2.0,1.0]},
                {"property":"rotation","static_value":[20.0]}]}
        ]})
}

struct Info<'a>(&'a Pixels);
impl TileInfoSource for Info<'_> {
    fn tile_info(&self, tile: &TileRef) -> Option<TileInfo> {
        let id = format!("{}/{}", tile.layer_id, tile.object_key);
        let t = self.0.get(&id);
        Some(TileInfo {
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: t.map_or([9; 32], |t| t.colour_sha256),
            alpha_sha256: t.map_or([9; 32], |t| t.alpha_sha256),
        })
    }
}

fn source_from(doc: &Value, rec: &Value, pixels: &Pixels) -> Result<CompositionTemporalSource, TemporalError> {
    source_of(doc, rec, pixels, &composition_id())
}
fn source_of(doc: &Value, rec: &Value, pixels: &Pixels, id: &str) -> Result<CompositionTemporalSource, TemporalError> {
    let snap = open(doc);
    let records: StudioCompositionRecords = serde_json::from_value(rec.clone()).expect("records shape");
    let input = CompositionInput {
        snapshot: &snap,
        expected_revision: 7,
        records: &records,
        composition_id: id,
        budget: Budget::default(),
        resolver: &Available,
        blend_space: BlendSpace::Encoded,
        info: &Info(pixels),
    };
    CompositionTemporalSource::new(&input, &CancellationToken::default())
}

/// A backend that is never asked to render (lowering only).
struct NoBackend;
impl RenderBackend for NoBackend {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity { name: "none", version: "0", device: DeviceClass::Cpu, adapter: String::new(), generation: 0 }
    }
    fn exactness(&self, _: Operator) -> Exactness {
        Exactness::Unsupported
    }
    fn execute(&self, _: &RenderPlan<NoVector>, _: &ExecuteRequest, _: &dyn PlanSources, _: &CancellationToken, _: &mut dyn TileSink) -> Result<RenderReceipt, BackendError> {
        Err(BackendError::Unavailable("lowering_only"))
    }
}
fn context<'a>(pixels: &'a Pixels, limits: SceneLimits) -> SceneContext<'a> {
    SceneContext {
        backend: &NoBackend,
        pixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
        space: scenes::space(),
        limits,
    }
}
fn near(a: &Affine2, b: &Affine2) {
    for (x, y) in a.m.iter().zip(b.m) {
        assert!((x - y).abs() < 1e-9, "{:?} vs {:?}", a.m, b.m);
    }
}

#[test]
fn stored_composition_evaluates_to_the_hand_written_scenes() {
    bounded_case("stored_composition_evaluates_to_the_hand_written_scenes", || {
        let pixels = Pixels::fixture();
        let src = source_from(&document(), &records(), &pixels).unwrap();
        assert_eq!(src.filter(), ResampleFilter::Bilinear, "declared default filter");
        let cancel = CancellationToken::default();
        for frame in scenes::FRAMES {
            let tick = src.frame_tick(frame).unwrap();
            assert_eq!(tick as u64, f(frame));
            let got = src.scene_at(tick, &cancel).unwrap();
            let want = expected_scene(frame, &pixels);
            assert_eq!(got.layers.len(), want.layers.len(), "frame {frame}");
            assert_eq!((got.width, got.height, got.background, got.filter), (want.width, want.height, want.background, want.filter));
            for (g, w) in got.layers.iter().zip(&want.layers) {
                assert_eq!((&g.layer_id, &g.content, g.mode, g.fill_opacity), (&w.layer_id, &w.content, w.mode, w.fill_opacity));
                assert!((g.opacity - w.opacity).abs() < 1e-12, "frame {frame} opacity {} vs {}", g.opacity, w.opacity);
                near(&g.matrix, &w.matrix);
            }
            // Lowering: the bottom layer is an exact integer move (no envelope); the animated top
            // layer resamples with the declared bilinear filter.
            let low = lower_scene(&got, &context(&pixels, SceneLimits::default()), &cancel).unwrap();
            let mut placements: Vec<_> = low.plan.nodes.iter().filter_map(|n| match n {
                PlanNode::Tile(t) => Some(t.placement),
                _ => None,
            }).collect();
            placements.sort_by_key(|r| (r.width, r.x));
            let resampled = (2..40).contains(&frame) && frame != 0;
            assert_eq!(low.extensions.layers.len(), usize::from(resampled), "frame {frame}");
            if frame < 20 {
                assert!(placements.iter().any(|r| (r.x, r.y, r.width, r.height) == (8, 6, 8, 6)), "frame {frame}: {placements:?}");
            }
            let keys: Vec<_> = low.exactness.operators.iter().map(|o| o.key).collect();
            assert_eq!(keys.contains(&"transform_bilinear"), resampled, "{keys:?}");
            if resampled {
                let t = low.extensions.layers[0].transform.unwrap();
                near(&t.affine, &want.layers.last().unwrap().matrix);
            }
        }
        // Outside every window (and before tick 0) only the background renders.
        assert!(src.scene_at(f(45) as i64, &cancel).unwrap().layers.is_empty());
        assert!(src.scene_at(-1, &cancel).unwrap().layers.is_empty());
    });
}

#[test]
fn temporal_source_on_the_document_graph_matches_the_scene() {
    bounded_case("temporal_source_on_the_document_graph_matches_the_scene", || {
        let pixels = Pixels::fixture();
        let doc = document();
        let snap: Snapshot = open(&doc);
        let src = source_from(&doc, &records(), &pixels).unwrap();
        let root = addr(1, "mix", "out");
        let timings = src.timings();
        let req = FrameRequest {
            snapshot: &snap,
            expected_revision: 7,
            blend_space: BlendSpace::Encoded,
            limits: Limits::default(),
            budget: Budget::default(),
            resolver: &Available,
            root: &root,
            target: OutputTarget {
                extent: hsk_studio_pigment::Rect { x: 0, y: 0, width: 32, height: 24 },
                format: SampleFormat::RgbF32Le,
                alpha: AlphaConvention::Straight,
                space: scenes::space(),
            },
            info: &Info(&pixels),
            timings: &timings,
        };
        let cancel = CancellationToken::default();
        let frame = lower_frame(&req, &src, f(5) as i64, &cancel).unwrap();
        let scene = src.scene_at(f(5) as i64, &cancel).unwrap();
        // The animated layer's envelope matrix equals the scene matrix; the static layer moves by
        // the same integer translation.
        assert_eq!(frame.extensions.layers.len(), 1);
        near(&frame.extensions.layers[0].transform.unwrap().affine, &scene.layers[1].matrix);
        assert!(frame.lowering.plan.nodes.iter().any(|n| matches!(n, PlanNode::Tile(t) if (t.placement.x, t.placement.y) == (8, 6))));
        // Frame 0: layer 3 is outside its window.
        let early = lower_frame(&req, &src, 0, &cancel).unwrap();
        assert_eq!(early.hidden, vec![identity("SLYR", 3)]);
    });
}

#[test]
fn refusals_are_typed_and_nothing_is_dropped() {
    bounded_case("refusals_are_typed_and_nothing_is_dropped", || {
        let pixels = Pixels::fixture();
        let unsupported = |e: TemporalError| match e {
            TemporalError::Unsupported { feature, .. } => feature,
            other => panic!("{other:?}"),
        };
        let edit = |change: &dyn Fn(&mut Value)| {
            let mut r = records();
            change(&mut r);
            source_from(&document(), &r, &pixels)
        };
        let layer0 = "/compositions/0/layers/0";
        // Features of later slices are refused, never ignored.
        let e = edit(&|r| r.pointer_mut(layer0).unwrap()["track_matte"] = json!({"source_layer_id":identity("SLYR",2),"mode":"alpha","inverted":false})).unwrap_err();
        assert_eq!(unsupported(e), COMPOSITION_TRACK_MATTE);
        // Precompositions: collapse transformations, and a precomposition layer with own content.
        let e = edit(&|r| r.pointer_mut(layer0).unwrap()["precomp"] = json!({"composition_id":scenes::composition4_id(),"collapse_transformations":true})).unwrap_err();
        assert_eq!(unsupported(e), COMPOSITION_PRECOMP_COLLAPSE);
        let e = edit(&|r| r.pointer_mut(layer0).unwrap()["precomp"] = json!({"composition_id":scenes::composition4_id(),"collapse_transformations":false})).unwrap_err();
        assert_eq!(unsupported(e), COMPOSITION_PRECOMP_PAYLOAD);
        // Content other than raster tiles or a null layer.
        let e = edit(&|r| r.pointer_mut(layer0).unwrap()["layer_id"] = json!(identity("SLYR", 5))).unwrap_err();
        assert_eq!(unsupported(e), COMPOSITION_LAYER_CONTENT);
        // A clipped layer.
        let mut doc = document();
        doc["layers"][2]["appearance"] = json!({"opacity":100.0,"fill_opacity":100.0,"blend_mode":2,"clipped":true,"isolation":"isolated"});
        let e = source_from(&doc, &records(), &pixels).unwrap_err();
        assert_eq!(unsupported(e), COMPOSITION_LAYER_CLIPPED);
        // Folio refusals of the records surface as Document; unknown composition and stale revision.
        let e = edit(&|r| r.pointer_mut(layer0).unwrap()["layer_id"] = json!(identity("SLYR", 99))).unwrap_err();
        assert!(matches!(&e, TemporalError::Document(t) if t.starts_with("DanglingReference")), "{e:?}");
        let e = edit(&|r| r["compositions"][0]["composition_id"] = json!(identity("SCOM", 9))).unwrap_err();
        assert!(matches!(e, TemporalError::Composite(CompositeError::DanglingInput(_))), "{e:?}");
        let mut stale = document();
        stale["revision"] = json!(8);
        let e = source_from(&stale, &records(), &pixels).unwrap_err();
        assert!(matches!(e, TemporalError::Composite(CompositeError::StaleRevision { expected: 7, actual: 8 })), "{e:?}");
        // Scene-level bounds (hostile scenes from any caller).
        let src = source_from(&document(), &records(), &pixels).unwrap();
        let cancel = CancellationToken::default();
        let mut scene = src.scene_at(f(5) as i64, &cancel).unwrap();
        let tight = SceneLimits { max_layers: 1, ..SceneLimits::default() };
        assert!(matches!(lower_scene(&scene, &context(&pixels, tight), &cancel), Err(TemporalError::Composite(CompositeError::BudgetExceeded))));
        let ctx = context(&pixels, SceneLimits::default());
        scene.layers[1].opacity = f64::NAN;
        assert!(matches!(lower_scene(&scene, &ctx, &cancel), Err(TemporalError::InvalidState { field: "opacity", .. })));
        scene.layers[1].opacity = 0.5;
        scene.layers[1].matrix.m[0] = f64::INFINITY;
        assert!(matches!(lower_scene(&scene, &ctx, &cancel), Err(TemporalError::InvalidState { field: "matrix", .. })));
        // Resampling a layer lowered to two tiles would seam: refused.
        let SceneContent::Tiles(t) = scene.layers[1].content.clone() else { panic!() };
        let mut second = t[0].clone();
        second.placement.x += 6;
        scene.layers[1].content = SceneContent::Tiles(vec![t[0].clone(), second]);
        scene.layers[1].matrix = Affine2::from_parts(3.0, 3.0, 1.5, 1.5, 10.0, 0.25, 0.0);
        let e = lower_scene(&scene, &ctx, &cancel).unwrap_err();
        assert_eq!(unsupported(e), TRANSFORM_MULTI_TILE_LAYER);
        // A scale of 0 renders nothing (singular), it is not an error.
        scene.layers[1].matrix = Affine2::from_parts(3.0, 3.0, 0.0, 1.0, 0.0, 0.0, 0.0);
        let low = lower_scene(&scene, &ctx, &cancel).unwrap();
        assert_eq!(low.skipped, vec![identity("SLYR", 3)]);
        let canceled = CancellationToken::default();
        canceled.cancel();
        assert!(matches!(src.scene_at(0, &canceled), Err(TemporalError::Composite(CompositeError::Canceled))));
    });
}

#[test]
fn lowering_is_identical_across_threads() {
    bounded_case("lowering_is_identical_across_threads", || {
        let pixels = std::sync::Arc::new(Pixels::fixture());
        let src = std::sync::Arc::new(source_from(&document(), &records(), &pixels).unwrap());
        let digest = |src: &CompositionTemporalSource, pixels: &Pixels| -> Vec<[u8; 32]> {
            let cancel = CancellationToken::default();
            scenes::FRAMES.iter().map(|frame| {
                let scene = src.scene_at(f(*frame) as i64, &cancel).unwrap();
                let low = lower_scene(&scene, &context(pixels, SceneLimits::default()), &cancel).unwrap();
                let ext = format!("{:?}", low.extensions).into_bytes();
                let mut bytes = low.plan.digest().unwrap().to_vec();
                bytes.extend_from_slice(&ext);
                hsk_studio_prism::profile_hash(&bytes)
            }).collect()
        };
        let base = digest(&src, &pixels);
        let handles: Vec<_> = (0..4).map(|_| {
            let (s, p) = (src.clone(), pixels.clone());
            std::thread::spawn(move || digest(&s, &p))
        }).collect();
        for h in handles {
            assert_eq!(h.join().unwrap(), base);
        }
    });
}

#[test]
fn parent_chain_anchor_stream_and_source_time() {
    bounded_case("parent_chain_anchor_stream_and_source_time", || {
        let pixels = Pixels::fixture();
        let cancel = CancellationToken::default();
        let src = source_of(&document(), &records(), &pixels, &scenes::composition2_id()).unwrap();
        for frame in scenes::FRAMES2 {
            let got = src.scene_at(f(frame) as i64, &cancel).unwrap();
            let want = scenes::expected_scene2(frame, &pixels);
            assert_eq!(got.layers.len(), 2);
            for (g, w) in got.layers.iter().zip(&want.layers) {
                assert_eq!((&g.layer_id, &g.content, g.opacity), (&w.layer_id, &w.content, w.opacity));
                near(&g.matrix, &w.matrix);
            }
        }
        // Hand check at frame 10: the child's anchor (6, 4) sits at its position (2, 1) in the
        // parent's space; the parent scales (1.5, 0.75), turns 90 deg clockwise and moves to
        // (16, 12): (2 * 1.5, 1 * 0.75) = (3, 0.75) -> (-0.75, 3) -> (15.25, 15).
        let m = src.scene_at(f(10) as i64, &cancel).unwrap().layers[0].matrix;
        let (x, y) = m.apply(6.0, 4.0);
        assert!((x - 15.25).abs() < 1e-12 && (y - 15.0).abs() < 1e-12, "({x}, {y})");
        // Sheared: the columns of the linear part are not orthogonal.
        let [a, b, _, d, e, _] = m.m;
        assert!((a * b + d * e).abs() > 1e-3, "{:?}", m.m);

        // lower_frame over the document graph honours the parent through
        // TemporalSource::layer_matrix: layer 3 parented to layer 2 moves with it.
        let mut rec = records();
        rec["compositions"][0]["layers"][0]["parent"] = json!(identity("SLYR", 2));
        let doc = document();
        let snap = open(&doc);
        let parented = source_from(&doc, &rec, &pixels).unwrap();
        let scene = parented.scene_at(f(5) as i64, &cancel).unwrap();
        // Layer 2's local transform: anchor (4, 3), position (12, 9).
        let alone = src_scene_matrix(&pixels, f(5) as i64);
        near(&scene.layers[1].matrix, &alone.then(Affine2::from_parts(4.0, 3.0, 1.0, 1.0, 0.0, 8.0, 6.0)));
        let root = addr(1, "mix", "out");
        let timings = parented.timings();
        let req = FrameRequest {
            snapshot: &snap,
            expected_revision: 7,
            blend_space: BlendSpace::Encoded,
            limits: Limits::default(),
            budget: Budget::default(),
            resolver: &Available,
            root: &root,
            target: OutputTarget {
                extent: hsk_studio_pigment::Rect { x: 0, y: 0, width: 32, height: 24 },
                format: SampleFormat::RgbF32Le,
                alpha: AlphaConvention::Straight,
                space: scenes::space(),
            },
            info: &Info(&pixels),
            timings: &timings,
        };
        let frame = lower_frame(&req, &parented, f(5) as i64, &cancel).unwrap();
        near(&frame.extensions.layers[0].transform.unwrap().affine, &scene.layers[1].matrix);

        // Source time: floor((tick - start) / stretch), equal to Folio's rule; reverse; overflow.
        let mut rec = records();
        let l7 = &mut rec["compositions"][1]["layers"][1]["timing"];
        l7["start_time"] = json!(f(2));
        l7["stretch"] = json!({"numerator":2,"denominator":1});
        let stretched = source_of(&document(), &rec, &pixels, &scenes::composition2_id()).unwrap();
        let id7 = identity("SLYR", 7);
        assert_eq!(stretched.source_tick(&id7, f(6) as i64).unwrap(), f(2) as i64);
        assert_eq!(stretched.source_tick(&id7, 3).unwrap(), (3 - f(2) as i64).div_euclid(2));
        let folio: StudioCompositionRecords = serde_json::from_value(rec.clone()).unwrap();
        let timing7 = folio.compositions[1].layers[1].timing;
        for tick in [0u64, 1, 7, f(2) + 1, f(9) + 13] {
            assert_eq!(Some(stretched.source_tick(&id7, tick as i64).unwrap()), timing7.source_tick(tick));
        }
        rec["compositions"][1]["layers"][1]["timing"]["stretch"] = json!({"numerator":-1,"denominator":1});
        let reversed = source_of(&document(), &rec, &pixels, &scenes::composition2_id()).unwrap();
        assert_eq!(reversed.source_tick(&id7, f(5) as i64).unwrap(), -(f(3) as i64));
        rec["compositions"][1]["layers"][1]["timing"] = json!({"in_point":0,"out_point":f(48),"start_time":0,"stretch":{"numerator":1,"denominator":3}});
        let slow = source_of(&document(), &rec, &pixels, &scenes::composition2_id()).unwrap();
        assert_eq!(slow.source_tick(&id7, f(1) as i64).unwrap(), 3 * f(1) as i64);
        assert!(matches!(slow.source_tick(&id7, i64::MAX), Err(TemporalError::Composite(CompositeError::Overflow))));
        assert!(matches!(slow.source_tick("missing", 0), Err(TemporalError::Composite(CompositeError::DanglingInput(_)))));
    });
}

/// Layer 3's own matrix at `tick` (composition 1, no parent).
fn src_scene_matrix(pixels: &Pixels, tick: i64) -> Affine2 {
    let src = source_from(&document(), &records(), pixels).unwrap();
    src.scene_at(tick, &CancellationToken::default()).unwrap().layers[1].matrix
}

/// Compares two scenes field by field (matrices to 1e-9), precompositions recursively.
fn same_scene(got: &hsk_studio_composite::SceneComposition, want: &hsk_studio_composite::SceneComposition, what: &str) {
    assert_eq!((&got.composition_id, got.width, got.height, got.background, got.filter),
        (&want.composition_id, want.width, want.height, want.background, want.filter), "{what}");
    assert_eq!(got.layers.len(), want.layers.len(), "{what}");
    for (g, w) in got.layers.iter().zip(&want.layers) {
        assert_eq!((&g.layer_id, g.mode, g.fill_opacity), (&w.layer_id, w.mode, w.fill_opacity), "{what}");
        assert!((g.opacity - w.opacity).abs() < 1e-12, "{what}");
        near(&g.matrix, &w.matrix);
        match (&g.content, &w.content) {
            (SceneContent::Precomp(a), SceneContent::Precomp(b)) => same_scene(a, b, what),
            (a, b) => assert_eq!(a, b, "{what}"),
        }
    }
}

#[test]
fn precomposition_plays_the_nested_composition_at_its_source_time() {
    bounded_case("precomposition_plays_the_nested_composition_at_its_source_time", || {
        let pixels = Pixels::fixture();
        let cancel = CancellationToken::default();
        let src = source_of(&document(), &records(), &pixels, &scenes::composition3_id()).unwrap();
        for frame in scenes::FRAMES3 {
            let got = src.scene_at(f(frame) as i64, &cancel).unwrap();
            same_scene(&got, &scenes::expected_scene3(frame, &pixels), &format!("comp3 frame {frame}"));
        }
        // The nested composition renders into its own surface first (counted), then the surface
        // is placed with the layer's matrix and the declared filter.
        let pixels_ref = &pixels;
        let ctx = SceneContext {
            backend: &NoBackend,
            pixels: pixels_ref,
            request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
            space: scenes::space(),
            limits: SceneLimits::default(),
        };
        let scene = src.scene_at(f(6) as i64, &cancel).unwrap();
        // NoBackend refuses to render, so the nested surface cannot be produced: typed, not dropped.
        let e = lower_scene(&scene, &ctx, &cancel).unwrap_err();
        assert!(matches!(&e, TemporalError::Render { .. }), "{e:?}");
        // Bounds: depth 0 refuses any nesting; one surface allowed refuses the nested one.
        let no_depth = SceneContext { limits: SceneLimits { max_depth: 0, ..SceneLimits::default() }, ..ctx };
        assert!(matches!(lower_scene(&scene, &no_depth, &cancel), Err(TemporalError::Composite(CompositeError::BudgetExceeded))));
        let one_surface = SceneContext { limits: SceneLimits { max_surfaces: 1, ..SceneLimits::default() }, ..ctx };
        assert!(matches!(hsk_studio_composite::render_scene(&scene, &one_surface, &cancel), Err(TemporalError::Composite(CompositeError::BudgetExceeded))));
        let small = SceneContext { limits: SceneLimits { max_surface_bytes: 32 * 24 * 16, ..SceneLimits::default() }, ..ctx };
        assert!(matches!(hsk_studio_composite::render_scene(&scene, &small, &cancel), Err(TemporalError::Composite(CompositeError::BudgetExceeded))));
        // The document-graph path cannot render a precomposition: refused, not drawn empty.
        let e = hsk_studio_composite::TemporalSource::layer_state(&src, &identity("SLYR", 8), 0, &cancel).unwrap_err();
        assert!(matches!(e, TemporalError::Unsupported { feature: COMPOSITION_PRECOMP_ON_DOCUMENT_GRAPH, .. }), "{e:?}");
    });
}
