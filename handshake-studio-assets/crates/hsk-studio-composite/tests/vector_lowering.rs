//! SMB-COMPOSITE-NEW-KINDS slices 2-3: vector layers with solid and image fills (Folio
//! `StudioImagePaint`) and live boolean groups are drawn into coverage planes on the host and lowered
//! through pigment's Solid, Tile, coverage-mask and layer-transform routes. Expected planes, nodes and
//! affines are derived by hand from the fixture.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::*;
use hsk_studio_folio::{StudioDesignRecords, StudioImageSource, StudioVectorRecords, TileRef};
use hsk_studio_nib::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, ColourTag, Group, Layer, LayerExtension, MaskFormat, MaskInput, OutputTarget,
    PlanNode, PlanNodeId, SampleFormat, SourceKey, TileInput,
};
use hsk_studio_pigment::transform_math::{Affine2, EdgeMode, LayerTransform, ResampleFilter};
use serde_json::{Value, json};

mod common;
use common::*;

fn target() -> OutputTarget {
    OutputTarget {
        extent: Rect { x: 0, y: 0, width: 32, height: 32 },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
    }
}
fn no_tiles(_: &TileRef) -> Option<TileInfo> {
    None
}
fn rect_path(x0: f64, y0: f64, x1: f64, y1: f64) -> OwnedPath {
    let p = PathPoint::new;
    OwnedPath {
        elements: vec![
            PathElement::MoveTo(p(x0, y0)),
            PathElement::LineTo(p(x1, y0)),
            PathElement::LineTo(p(x1, y1)),
            PathElement::LineTo(p(x0, y1)),
            PathElement::Close,
        ],
        fill_rule: FillRule::NonZero,
    }
}
const INFO: ImageInfo = ImageInfo {
    width: 4,
    height: 4,
    format: SampleFormat::RgbF32Le,
    alpha: AlphaConvention::Straight,
    colour_sha256: [5; 32],
    alpha_sha256: [6; 32],
};
/// SVPT 1: the square (2, 3)-(12, 13); SVPT 2: (8, 3)-(18, 13); SVPT 9: a 3x3 cached outline.
struct Assets;
impl VectorAssets for Assets {
    fn path(&self, path_id: &str) -> Option<OwnedPath> {
        if path_id == identity("SVPT", 1) {
            Some(rect_path(2.0, 3.0, 12.0, 13.0))
        } else if path_id == identity("SVPT", 2) {
            Some(rect_path(8.0, 3.0, 18.0, 13.0))
        } else if path_id == identity("SVPT", 9) {
            Some(rect_path(1.0, 1.0, 4.0, 4.0))
        } else {
            None
        }
    }
    fn image(&self, source: &StudioImageSource) -> Option<ImageInfo> {
        (image_source_id(source) == format!("sha256:{}", "a".repeat(64))).then_some(INFO)
    }
}
fn doc() -> Value {
    let mut v = mixed();
    assign_all_profiles(&mut v);
    v
}
fn style(rows: Vec<Value>) -> Value {
    json!({"schema_id":"hsk.studio.style_registry@1","style_id":identity("SSTY",1),"kind":"PAINT",
        "name":"fills","publishing":{"description":"","description_markdown":"","documentation_links":[],
        "publish_key":"fills","is_remote":false},"rows":rows})
}
fn solid_row(r: f64, g: f64, b: f64, opacity: f64, mode: u16) -> Value {
    json!({"row_kind":"fill","paint":{"paint_kind":"solid","colour":{"r":r,"g":g,"b":b,"colour_profile_id":null}},
        "opacity":opacity,"blend_mode":mode,"visible":true})
}
fn design(layer: u8, rows: Vec<Value>) -> StudioDesignRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.design_records@1","document_id":identity("SDOC",1),
        "nodes":[{"node":{"node_kind":"layer","node_id":identity("SLYR",layer)},
            "style_refs":{"fill_style_id":identity("SSTY",1)}}],
        "paint_styles":[style(rows)]}))
    .expect("design records shape")
}
fn image_paint(row: u32, scale: &str, transform: [f64; 6], digest: char) -> Value {
    json!({"style_id":identity("SSTY",1),"row":row,"image":{"source_kind":"package_asset",
        "content_digest":{"algorithm":"sha256","digest":digest.to_string().repeat(64)}},
        "scale_mode":scale,"transform":transform,"scaling_factor":1.0,"rotation":0.0,"opacity":80.0})
}
fn vector_records(image_paints: Vec<Value>, boolean_groups: Vec<Value>) -> StudioVectorRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),
        "image_paints":image_paints,"boolean_groups":boolean_groups}))
    .expect("vector records shape")
}
fn prepared(v: &Value, d: &StudioDesignRecords, r: &StudioVectorRecords) -> (hsk_studio_folio::Snapshot, PreparedVectors) {
    let snap = open(v);
    let p = prepare_vectors(&snap, d, r, &Assets, BlendSpace::Encoded,
        // The supersampled rule and its Folio refusals (the default is nib Exact since SMB-COMPOSITE-CLIP-RECORDS).
        &VectorResolution { coverage: VectorCoverage::Supersampled, ..VectorResolution::default() },
        &CancellationToken::default()).unwrap();
    (snap, p)
}
fn lowered(
    snap: &hsk_studio_folio::Snapshot,
    vectors: &PreparedVectors,
    root: &NodeAddr,
) -> (EvalReport, Result<InputsLowering, CompositeError>) {
    let token = CancellationToken::default();
    let plan = lower(snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
    let inputs = RenderInputs { vectors: Some(vectors), ..RenderInputs::default() };
    let report = evaluate_with_inputs(&plan, snap, &inputs, &token).unwrap();
    let low = lower_with_inputs(&plan, &report, snap, &inputs, root, target(), &no_tiles);
    (report, low)
}

#[test]
fn vector_layer_with_solid_and_image_fills_lowers_to_pigment_routes() {
    bounded_case("vector_layer_with_solid_and_image_fills_lowers_to_pigment_routes", || {
        // Vector layer 4 (SVPT 1): row 0 solid (0.25, 0.5, 0.75) normal 100 %; row 1 an image
        // (CROP, transform x' = 0.5 x + 0.25) multiply at 50 % row opacity x 80 % image opacity.
        let d = design(4, vec![solid_row(0.25, 0.5, 0.75, 100.0, 2), solid_row(0.0, 0.0, 0.0, 50.0, 5)]);
        let r = vector_records(vec![image_paint(1, "CROP", [0.5, 0.0, 0.0, 0.5, 0.25, 0.25], 'a')], vec![]);
        let (snap, vectors) = prepared(&doc(), &d, &r);
        let plane = vectors.plane(&identity("SLYR", 4)).expect("vector 4 prepared");
        // The square covers whole pixels: every sample of every pixel is inside.
        let square = Rect { x: 2, y: 3, width: 10, height: 10 };
        assert_eq!(plane.placement, Some(square));
        assert_eq!(plane.coverage, vec![255u8; 100]);
        assert_eq!(plane.sha256, hsk_studio_prism::profile_hash(&[255u8; 100]));
        // Pixel -> unit (1/4), CROP transform, unit -> bounds (10, origin (2, 3)): x' = 1.25 x + 4.5.
        let affine = Affine2 { m: [1.25, 0.0, 4.5, 0.0, 1.25, 5.5] };
        let source_id = format!("sha256:{}", "a".repeat(64));
        assert_eq!(
            plane.fills,
            vec![
                PreparedFill { paint: PreparedPaint::Solid([0.25, 0.5, 0.75, 1.0]), mode: StudioBlendMode::Normal, opacity: 1.0 },
                PreparedFill {
                    paint: PreparedPaint::Image { source_id: source_id.clone(), info: INFO, placements: vec![affine] },
                    mode: StudioBlendMode::Multiply,
                    opacity: 0.5 * 0.8,
                },
            ]
        );
        let (report, low) = lowered(&snap, &vectors, &addr(4, "source", "out"));
        assert_eq!(report.step(&addr(4, "source", "out")).unwrap().extent, Some(square));
        let low = low.unwrap();
        let layer = |input: u32, mode, opacity: f32, mask| Layer { input: PlanNodeId(input), mode, opacity, fill_opacity: 1.0, mask };
        assert_eq!(
            low.plan.nodes,
            vec![
                PlanNode::Solid { rect: square, rgba: [0.25, 0.5, 0.75, 1.0] },
                PlanNode::Tile(TileInput {
                    key: SourceKey(0),
                    placement: Rect { x: 0, y: 0, width: 4, height: 4 },
                    format: SampleFormat::RgbF32Le,
                    alpha: AlphaConvention::Straight,
                    colour_sha256: [5; 32],
                    alpha_sha256: [6; 32],
                }),
                PlanNode::Group(Group { isolated: true, layers: vec![
                    layer(0, StudioBlendMode::Normal, 1.0, None),
                    layer(1, StudioBlendMode::Multiply, 0.5 * 0.8, None)] }),
                PlanNode::Group(Group { isolated: true, layers: vec![layer(2, StudioBlendMode::Normal, 1.0, Some(MaskInput {
                    key: SourceKey(1), placement: square, format: MaskFormat::CoverageU8,
                    sha256: plane.sha256, inverted: false, density: 1.0 }))] }),
            ]
        );
        assert_eq!(low.plan.root, PlanNodeId(3));
        assert_eq!(
            low.extensions.layers,
            vec![LayerExtension::new(PlanNodeId(2), 1).with_transform(
                LayerTransform::new(affine, ResampleFilter::Bilinear).with_edge(EdgeMode::Clamp))]
        );
        assert_eq!(low.image_source(SourceKey(0)), Some(source_id.as_str()));
        assert_eq!(low.vector_mask(SourceKey(1), &vectors).unwrap().sha256, plane.sha256);
        for ledger in [&report.receipt.exactness, &low.exactness] {
            assert!(ledger.operators.iter().any(|o| o.key == "transform_bilinear"), "{ledger:?}");
        }
        // The folio-free plan of the same layer is the same plan (what the renderer tests run).
        let alone = vector_layer_render_plan(&vectors, &identity("SLYR", 4), 7, target()).unwrap();
        assert_eq!((alone.plan, alone.extensions), (low.plan, low.extensions));
        // Without prepared vectors the static path is unchanged: the vector layer stays refused.
        let plain = evaluate(&lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &CancellationToken::default()).unwrap(),
            &snap, &CancellationToken::default()).unwrap();
        assert_eq!(plain.step(&addr(4, "source", "out")).unwrap().status,
            StepStatus::Unavailable("vector_to_image_conversion".into()));
    });
}

#[test]
fn vector_refusals_are_typed_and_never_dropped() {
    bounded_case("vector_refusals_are_typed_and_never_dropped", || {
        let refused = |d: StudioDesignRecords, r: StudioVectorRecords, code: &str| {
            let (snap, vectors) = prepared(&doc(), &d, &r);
            assert_eq!(vectors.refusal(&identity("SLYR", 4)), Some(code), "{code}");
            let (report, low) = lowered(&snap, &vectors, &addr(4, "source", "out"));
            assert_eq!(report.step(&addr(4, "source", "out")).unwrap().status, StepStatus::Unavailable(code.into()));
            assert!(low.unwrap_err().to_string().contains(code), "{code}");
        };
        let none = || vector_records(vec![], vec![]);
        let gradient = json!({"row_kind":"fill","paint":{"paint_kind":"gradient","gradient":{
            "geometry":{"gradient_kind":"linear","x1":0.0,"y1":0.0,"x2":1.0,"y2":0.0},
            "transform":[1.0,0.0,0.0,1.0,0.0,0.0],"spread":"pad","interpolation":"classic","dither":false,
            "stops":[{"position":0.0,"midpoint":null,"opacity":100.0,"colour":{"r":0.0,"g":0.0,"b":0.0,"colour_profile_id":null}},
                     {"position":100.0,"midpoint":null,"opacity":100.0,"colour":{"r":1.0,"g":1.0,"b":1.0,"colour_profile_id":null}}]}},
            "opacity":100.0,"blend_mode":2,"visible":true});
        refused(design(4, vec![gradient]), none(), "paint_gradient");
        let stroke = json!({"row_kind":"stroke","paint":{"paint_kind":"solid","colour":{"r":0.0,"g":0.0,"b":0.0,"colour_profile_id":null}},
            "stroke":{"weight":{"value":1.0,"unit":"px"},"cap":"butt","join":"miter","align":"CENTER","miter_limit":4.0,
                "dash":[],"dash_offset":{"value":0.0,"unit":"px"}},"opacity":100.0,"blend_mode":2,"visible":true});
        refused(design(4, vec![solid_row(1.0, 1.0, 1.0, 100.0, 2), stroke]), none(), "vector_stroke_not_rendered");
        let solid = || design(4, vec![solid_row(1.0, 0.0, 0.0, 100.0, 2)]);
        refused(solid(), vector_records(vec![image_paint(0, "CROP", [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 'b')], vec![]),
            "image_paint_unresolved");
        refused(solid(), vector_records(vec![image_paint(0, "FILL", [0.5, 0.0, 0.0, 0.5, 0.0, 0.0], 'a')], vec![]),
            "image_paint_transform");
        let mut tiny = image_paint(0, "TILE", [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 'a');
        tiny["scaling_factor"] = json!(0.05); // 0.2 px copies over a 10 px square: 2500 copies
        refused(solid(), vector_records(vec![tiny], vec![]), "image_paint_tile_count");
        // An unresolved path.
        let mut v = doc();
        v["layers"][3]["payload"]["path_ids"] = json!([identity("SVPT", 7)]);
        let (_, vectors) = prepared(&v, &solid(), &none());
        assert_eq!(vectors.refusal(&identity("SLYR", 4)), Some("vector_path_unresolved"));
        // FILL / FIT / TILE placements (hand values over the 10x10 square, 4x4 image).
        let placed = |scale: &str, factor: f64| {
            let mut p = image_paint(0, scale, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 'a');
            p["scaling_factor"] = json!(factor);
            let (_, vectors) = prepared(&doc(), &solid(), &vector_records(vec![p], vec![]));
            match &vectors.plane(&identity("SLYR", 4)).unwrap().fills[0].paint {
                PreparedPaint::Image { placements, .. } => placements.clone(),
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(placed("FILL", 1.0), vec![Affine2 { m: [2.5, 0.0, 2.0, 0.0, 2.5, 3.0] }]);
        assert_eq!(placed("FIT", 1.0), vec![Affine2 { m: [2.5, 0.0, 2.0, 0.0, 2.5, 3.0] }]);
        // TILE at 2: 8 px copies from (2, 3): 2 x 2 copies.
        assert_eq!(
            placed("TILE", 2.0),
            vec![
                Affine2 { m: [2.0, 0.0, 2.0, 0.0, 2.0, 3.0] },
                Affine2 { m: [2.0, 0.0, 10.0, 0.0, 2.0, 3.0] },
                Affine2 { m: [2.0, 0.0, 2.0, 0.0, 2.0, 11.0] },
                Affine2 { m: [2.0, 0.0, 10.0, 0.0, 2.0, 11.0] },
            ]
        );
    });
}
