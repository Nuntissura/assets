//! Lane SMB-COMPOSITE-COMPOSITION-2 slices 1-2: composition layers with masks, raster records,
//! vector paint and adjustment layers. The adapter routes such a layer through composite's static
//! lowering (opacity 100 % and blend normal inside, both applied by the composition after the
//! transform) into `SceneContent::Static`, and an adjustment layer into `SceneContent::Adjustment`.
//! Pixel proofs: render-cpu / render-gpu `tests/composition_frames.rs` (composition 6).
#![cfg(feature = "motion")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::temporal::composition::{CompositionInput, CompositionTemporalSource};
use hsk_studio_composite::vector::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_composite::*;
use hsk_studio_folio::{Budget, Resolution, Resolver, StudioCompositionRecords, StudioRasterRecords, TileRef};
use hsk_studio_pigment::adjust_math::AdjustmentOp;
use hsk_studio_pigment::render_plan::{AlphaConvention, ExecuteRequest, PlanNode, SampleFormat};
use hsk_studio_pigment::transform_math::Affine2;
use serde_json::{Value, json};

mod common;
use common::{address, bounded_case, composite, edge, identity, layer, source, tile};
#[path = "support/composition_scenes.rs"]
mod scenes;

struct Res;
impl Resolver for Res {
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
        kind == "adjustment" && schema_id == "hsk.studio.adjustment@1"
    }
}
struct Info;
impl TileInfoSource for Info {
    fn tile_info(&self, t: &TileRef) -> Option<TileInfo> {
        Some(TileInfo {
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: [t.object_key.len() as u8; 32],
            alpha_sha256: [3; 32],
        })
    }
    fn mask_sha256(&self, tiles: &[TileRef]) -> Option<[u8; 32]> {
        Some([tiles.len() as u8 + 40; 32])
    }
}

/// Group 1 composites raster 2 (tile t2 8x6 with an enabled mask m2) and adjustment 4 (an image
/// source by input ordinal); vector 3 is a standalone vector source. Document unit px.
fn document() -> Value {
    let mut raster = layer(2, "raster", json!({"payload_kind":"raster","tiles":[tile(2, "t2", 0, 0, 8, 6)]}));
    let mut m = tile(2, "m2", 0, 0, 8, 6);
    m["format"] = json!("coverage_u8");
    raster["appearance"] = json!({"opacity":60.0,"fill_opacity":100.0,"blend_mode":5,"clipped":false,
        "isolation":"isolated","mask":{"tiles":[m],"enabled":true,"linked":true}});
    let adjustment = layer(4, "adjustment", json!({"payload_kind":"primitive_reference",
        "schema_id":"hsk.studio.adjustment@1","object_id":identity("SLYR",4)}));
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"px",
        "colour_mode":3,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
        "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[layer(1, "group", json!({"payload_kind":"group"})), raster,
            layer(3, "vector", json!({"payload_kind":"vector","path_ids":[identity("SVPT",1)]})), adjustment],
        "graph":{"schema_id":"hsk.studio.layer_graph@1",
            "operations":[source(2, "image"), source(3, "vector"), source(4, "image"), composite(1)],
            "edges":[edge(address(2, "source", "out"), address(1, "mix", "inputs"), 0),
                     edge(address(4, "source", "out"), address(1, "mix", "inputs"), 1)],
            "outputs":[address(1, "mix", "out"), address(3, "source", "out")]}
    });
    common::assign_all_profiles(&mut v);
    v
}
fn timing() -> Value {
    json!({"in_point":0,"out_point":10_584_000_000u64 * 48,"start_time":0,"stretch":{"numerator":1,"denominator":1}})
}
fn records() -> StudioCompositionRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),
        "compositions":[{"schema_id":"hsk.studio.composition@1","composition_id":identity("SCOM",7),"name":"c",
            "width":32,"height":24,"pixel_aspect":{"numerator":1,"denominator":1},
            "frame_rate":{"numerator":24,"denominator":1},"duration":10_584_000_000u64 * 48,
            "layers":[
                {"layer_id":identity("SLYR",4),"timing":timing(),"streams":[{"property":"opacity","static_value":[70.0]}]},
                {"layer_id":identity("SLYR",3),"timing":timing(),"streams":[{"property":"rotation","static_value":[10.0]}]},
                {"layer_id":identity("SLYR",2),"timing":timing()}]}]}))
    .unwrap()
}
fn raster_records(with_adjustment: bool) -> StudioRasterRecords {
    let mut layers = vec![json!({"layer_id":identity("SLYR",2),"mask":{"density":50.0,"feather":0.0,"inverted":false}})];
    if with_adjustment {
        layers.push(json!({"layer_id":identity("SLYR",4),"adjustment":{"kind":"invert"}}));
    }
    serde_json::from_value(json!({"schema_id":"hsk.studio.raster_records@1","document_id":identity("SDOC",1),"layers":layers})).unwrap()
}
fn triangle() -> OwnedPath {
    let p = PathPoint::new;
    OwnedPath {
        elements: vec![
            PathElement::MoveTo(p(3.0, 2.0)),
            PathElement::LineTo(p(20.5, 4.0)),
            PathElement::LineTo(p(9.0, 18.0)),
            PathElement::Close,
        ],
        fill_rule: FillRule::NonZero,
    }
}
fn vectors() -> PreparedVectors {
    let spec = VectorLayerSpec {
        layer_id: identity("SLYR", 3),
        geometry: VectorGeometry::Paths(vec![triangle()]),
        fills: vec![FillSpec { paint: FillPaint::Solid([0.1, 0.5, 0.9, 1.0]), mode: StudioBlendMode::Normal, opacity: 1.0 }],
    };
    let res = VectorResolution { coverage: VectorCoverage::Exact, ..VectorResolution::default() };
    prepare_vector_layers(&[spec], &res, &CancellationToken::default()).unwrap()
}
fn source_with(inputs: RenderInputs<'_>) -> Result<CompositionTemporalSource, TemporalError> {
    let snap = common::open_bytes(&serde_json::to_vec(&document()).unwrap(), &Res);
    let rec = records();
    let id = identity("SCOM", 7);
    let input = CompositionInput {
        snapshot: &snap,
        expected_revision: 7,
        records: &rec,
        composition_id: &id,
        budget: Budget::default(),
        resolver: &Res,
        blend_space: BlendSpace::Encoded,
        info: &Info,
        space: scenes::space(),
        inputs,
    };
    CompositionTemporalSource::new(&input, &CancellationToken::default())
}

#[test]
fn masked_vector_and_adjustment_layers_lower_through_the_static_path() {
    bounded_case("masked_vector_and_adjustment_layers_lower_through_the_static_path", || {
        let cancel = CancellationToken::default();
        let recs = raster_records(true);
        let vecs = vectors();
        let src = source_with(RenderInputs { records: Some(&recs), text: None, vectors: Some(&vecs) }).unwrap();
        let scene = src.scene_at(0, &cancel).unwrap();
        let ids: Vec<_> = scene.layers.iter().map(|l| l.layer_id.clone()).collect();
        assert_eq!(ids, [identity("SLYR", 2), identity("SLYR", 3), identity("SLYR", 4)]);

        // Raster 2: static subtree with the mask (density 50 % from the raster records), the
        // layer's opacity (60 %) and multiply left to the composition.
        let l2 = &scene.layers[0];
        assert_eq!((l2.mode, l2.opacity), (StudioBlendMode::Multiply, 0.6));
        let SceneContent::Static(c2) = &l2.content else { panic!("static raster: {:?}", l2.content) };
        assert_eq!(c2.rect, hsk_studio_pigment::Rect { x: 0, y: 0, width: 8, height: 6 });
        assert_eq!(c2.masks.values().cloned().collect::<Vec<_>>(), vec![format!("{}/m2", identity("SLYR", 2))]);
        assert_eq!(c2.tiles.values().cloned().collect::<Vec<_>>(), vec![format!("{}/t2", identity("SLYR", 2))]);
        let densities: Vec<f32> = c2.plan.nodes.iter().filter_map(|n| match n {
            PlanNode::Group(g) => g.layers.iter().find_map(|l| l.mask.map(|m| m.density)),
            _ => None,
        }).collect();
        assert_eq!(densities, vec![0.5]);
        let neutral = c2.plan.nodes.iter().all(|n| match n {
            PlanNode::Group(g) => g.layers.iter().all(|l| l.opacity == 1.0 && l.mode == StudioBlendMode::Normal),
            _ => true,
        });
        assert!(neutral, "the subtree carries no opacity or blend of its own");
        assert_eq!(l2.matrix, Affine2::IDENTITY);

        // Vector 3: the static plan is the standalone vector plan; the coverage plane is owned.
        let l3 = &scene.layers[1];
        let SceneContent::Static(c3) = &l3.content else { panic!("static vector") };
        let plane = vecs.plane(&identity("SLYR", 3)).unwrap();
        assert_eq!(c3.rect, plane.placement.unwrap());
        let alone = vector_layer_render_plan(&vecs, &identity("SLYR", 3), 7, c3.plan.target).unwrap();
        assert_eq!(c3.plan.nodes, alone.plan.nodes);
        assert_eq!(c3.owned_masks.values().map(|m| m.sha256).collect::<Vec<_>>(), vec![plane.sha256]);
        assert!(c3.owned_masks.values().all(|m| m.bytes == plane.coverage));

        // Adjustment 4: the pigment op from the raster records, opacity stream 70 %.
        let l4 = &scene.layers[2];
        assert_eq!(l4.content, SceneContent::Adjustment(AdjustmentOp::Invert));
        assert!((l4.opacity - 0.7).abs() < 1e-12 && l4.matrix == Affine2::IDENTITY);

        // Refusals: an adjustment layer without its record; without prepared vectors the static
        // path's own refusal; a transformed adjustment layer at lowering.
        let only_mask = raster_records(false);
        let e = source_with(RenderInputs { records: Some(&only_mask), text: None, vectors: Some(&vecs) }).unwrap_err();
        assert!(matches!(e, TemporalError::Unsupported { feature: "adjustment_record_missing", .. }), "{e:?}");
        let e = source_with(RenderInputs { records: Some(&recs), text: None, vectors: None }).unwrap_err();
        assert!(matches!(&e, TemporalError::Composite(CompositeError::Unavailable(t)) if t.ends_with("vector_to_image_conversion")), "{e:?}");
        let mut moved = scene.clone();
        moved.layers = vec![SceneLayer { matrix: Affine2::translate(1.0, 0.0), ..scene.layers[2].clone() }];
        let pixels = scenes::Pixels::fixture();
        let ctx = SceneContext {
            backend: &NoBackend,
            pixels: &pixels,
            request: ExecuteRequest { expected_revision: 7, min_exactness: hsk_studio_pigment::render_plan::Exactness::Approximate, deadline: None, max_working_bytes: 1 << 26 },
            space: scenes::space(),
            limits: SceneLimits::default(),
        };
        let e = lower_scene(&moved, &ctx, &cancel).unwrap_err();
        assert!(matches!(e, TemporalError::Unsupported { feature: "composition_adjustment_transform", .. }), "{e:?}");
        // An untransformed adjustment lowers to a transparent placeholder with the envelope op.
        moved.layers[0].matrix = Affine2::IDENTITY;
        let low = lower_scene(&moved, &ctx, &cancel).unwrap();
        assert_eq!(low.extensions.layers.len(), 1);
        assert_eq!(low.extensions.layers[0].adjustment, Some(AdjustmentOp::Invert));
    });
}

/// Lowering-only backend.
struct NoBackend;
impl hsk_studio_pigment::render_plan::RenderBackend for NoBackend {
    fn identity(&self) -> hsk_studio_pigment::render_plan::BackendIdentity {
        hsk_studio_pigment::render_plan::BackendIdentity {
            name: "none",
            version: "0",
            device: hsk_studio_pigment::render_plan::DeviceClass::Cpu,
            adapter: String::new(),
            generation: 0,
        }
    }
    fn exactness(&self, _: hsk_studio_pigment::render_plan::Operator) -> hsk_studio_pigment::render_plan::Exactness {
        hsk_studio_pigment::render_plan::Exactness::Unsupported
    }
    fn execute(
        &self,
        _: &hsk_studio_pigment::render_plan::RenderPlan,
        _: &ExecuteRequest,
        _: &dyn hsk_studio_pigment::render_plan::PlanSources,
        _: &CancellationToken,
        _: &mut dyn hsk_studio_pigment::render_plan::TileSink,
    ) -> Result<hsk_studio_pigment::render_plan::RenderReceipt, hsk_studio_pigment::render_plan::BackendError> {
        Err(hsk_studio_pigment::render_plan::BackendError::Unavailable("lowering_only"))
    }
}
