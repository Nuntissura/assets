//! Lane SMB-COMPOSITE-VECTOR-IN-GROUP slice 2: vector layers drawn through nib (CX-CAP-001) —
//! exact fill coverage (`nib::raster::rasterize` of the united outline / nib boolean), strokes
//! (`nib::stroke::outline_stroke` then `rasterize`), gradient and pattern rows (`paint_coverage`) —
//! and the Folio adapter's nib mode (`VectorCoverage::Exact`): strokes and gradients mapped,
//! typed refusals, the default mode unchanged. Expected planes are computed by calling nib
//! directly on the same inputs. Pixel proofs: render-cpu / render-gpu `tests/vector_nib.rs`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::*;
use hsk_studio_composite::vector::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_folio::{StudioDesignRecords, StudioImageSource, StudioVectorRecords, TileRef};
use hsk_studio_nib::boolean::{BooleanOp as NibOp, BooleanOptions, boolean};
use hsk_studio_nib::raster::{CoveragePlane, Paint, PixelFrame, RasterOptions, paint_coverage, rasterize};
use hsk_studio_nib::stroke::{StrokeOptions, outline_stroke};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{AlphaConvention, ColourTag, OutputTarget, PlanNode, SampleFormat};
use serde_json::{Value, json};

mod common;
use common::*;
#[path = "support/vector_scenes.rs"]
mod scenes;

fn target() -> OutputTarget {
    OutputTarget {
        extent: Rect { x: 0, y: 0, width: 32, height: 24 },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
    }
}
fn exact() -> VectorResolution {
    VectorResolution { coverage: VectorCoverage::Exact, ..VectorResolution::default() }
}
fn frame(r: Rect) -> PixelFrame {
    PixelFrame { x: r.x as i32, y: r.y as i32, width: r.width, height: r.height }
}

#[test]
fn nib_planes_equal_nib_called_directly() {
    bounded_case("nib_planes_equal_nib_called_directly", || {
        let spec = scenes::spec();
        let v = prepare_vector_layers(std::slice::from_ref(&spec), &exact(), &CancellationToken::default()).unwrap();
        let plane = v.plane(scenes::LAYER).unwrap();
        let rect = plane.placement.unwrap();
        let VectorGeometry::Paths(paths) = &spec.geometry else { panic!() };
        // Fill: the united outline rasterised by nib over the layer plane.
        let union = boolean(paths, NibOp::Union, &BooleanOptions::default()).unwrap().path;
        let (want, _) = rasterize(&union, frame(rect), &RasterOptions::default()).unwrap();
        assert_eq!(plane.coverage, want.data);
        assert!(plane.coverage.iter().any(|c| *c > 0 && *c < 255), "anti-aliased edges");
        // Strokes: rows 2 and 3, each its own nib outline (paths stroked one by one, united).
        assert_eq!(plane.strokes.iter().map(|s| s.row).collect::<Vec<_>>(), vec![2, 3]);
        for s in &plane.strokes {
            let FillPaint::Stroke { style, .. } = &spec.fills[s.row].paint else { panic!() };
            let outs: Vec<_> = paths.iter().map(|p| outline_stroke(p, style, &StrokeOptions::default()).unwrap().path).collect();
            let united = boolean(&outs, NibOp::Union, &BooleanOptions::default()).unwrap().path;
            let (want, _) = rasterize(&united, frame(rect), &RasterOptions::default()).unwrap();
            assert_eq!(s.coverage, want.data, "stroke row {}", s.row);
        }
        // The plane covers the strokes (which reach beyond the fill bounds).
        assert!(rect.x <= 2 && rect.y <= 1, "{rect:?}");
        // Gradient row 0: nib's premultiplied paint stored straight.
        let FillPaint::Gradient(g) = &spec.fills[0].paint else { panic!() };
        let full = CoveragePlane { frame: frame(rect), data: vec![255; (rect.width * rect.height) as usize] };
        let painted = paint_coverage(&full, &Paint::Gradient(g.clone())).unwrap();
        let PreparedPaint::Painted(t) = &plane.fills[0].paint else { panic!() };
        for (i, p) in painted.data.iter().enumerate() {
            let a = f32::from_le_bytes(t.alpha[i * 4..i * 4 + 4].try_into().unwrap());
            let r = f32::from_le_bytes(t.colour[i * 12..i * 12 + 4].try_into().unwrap());
            assert_eq!(a, p[3]);
            assert!((r * a - p[0]).abs() <= 1e-6, "pixel {i}");
        }
        // Lowering: content = [fills group masked by the fill plane, two stroke layers masked by
        // their planes]; painted rows read tiles served from the prepared layer.
        let low = vector_layer_render_plan(&v, scenes::LAYER, 7, target()).unwrap();
        let nodes = &low.plan.nodes;
        let PlanNode::Group(content) = &nodes[low.plan.root.0 as usize] else { panic!() };
        assert_eq!(content.layers.len(), 3);
        let PlanNode::Group(rows) = &nodes[content.layers[0].input.0 as usize] else { panic!() };
        assert_eq!(rows.layers.len(), 2);
        assert_eq!((rows.layers[1].mode, rows.layers[1].opacity), (StudioBlendMode::Multiply, 0.6));
        assert_eq!((content.layers[2].mode, content.layers[2].opacity), (StudioBlendMode::Screen, 0.8));
        for (i, layer) in content.layers.iter().enumerate() {
            let m = layer.mask.unwrap();
            let served = low.vector_mask(m.key, &v).unwrap();
            assert_eq!(served.sha256, m.sha256, "layer {i}");
        }
        let PlanNode::Tile(tile) = &nodes[rows.layers[0].input.0 as usize] else { panic!("painted tile") };
        assert_eq!(low.vector_paint(tile.key, &v, scenes::space()).unwrap().colour_sha256, t.colour_sha256);
        assert!(low.exactness.operators.iter().any(|o| o.key == "tile"));
    });
}

/// Folio fixture: vector layer 4 (SVPT 1, the square (2, 3)-(12, 13)), document geometry unit px.
fn doc(unit: &str) -> Value {
    let mut v = mixed();
    assign_all_profiles(&mut v);
    v["geometry_unit"] = json!(unit);
    v
}
struct Assets;
impl VectorAssets for Assets {
    fn path(&self, path_id: &str) -> Option<OwnedPath> {
        (path_id == identity("SVPT", 1)).then(|| {
            let p = PathPoint::new;
            OwnedPath {
                elements: vec![
                    PathElement::MoveTo(p(2.0, 3.0)),
                    PathElement::LineTo(p(12.0, 3.0)),
                    PathElement::LineTo(p(12.0, 13.0)),
                    PathElement::LineTo(p(2.0, 13.0)),
                    PathElement::Close,
                ],
                fill_rule: FillRule::NonZero,
            }
        })
    }
    fn image(&self, _: &StudioImageSource) -> Option<ImageInfo> {
        None
    }
}
fn solid(r: f64) -> Value {
    json!({"paint_kind":"solid","colour":{"r":r,"g":0.5,"b":0.25,"colour_profile_id":null}})
}
fn fill_row(paint: Value) -> Value {
    json!({"row_kind":"fill","paint":paint,"opacity":100.0,"blend_mode":2,"visible":true})
}
fn stroke_row(paint: Value, unit: &str) -> Value {
    json!({"row_kind":"stroke","paint":paint,
        "stroke":{"weight":{"value":2.0,"unit":unit},"cap":"round","join":"bevel","align":"CENTER","miter_limit":4.0,
            "dash":[],"dash_offset":{"value":0.0,"unit":unit}},"opacity":50.0,"blend_mode":2,"visible":true})
}
fn gradient(interpolation: &str) -> Value {
    json!({"paint_kind":"gradient","gradient":{
        "geometry":{"gradient_kind":"linear","x1":2.0,"y1":3.0,"x2":12.0,"y2":13.0},
        "transform":[1.0,0.0,0.0,1.0,0.0,0.0],"spread":"pad","interpolation":interpolation,"dither":false,
        "stops":[{"position":0.0,"midpoint":40.0,"opacity":100.0,"colour":{"r":0.0,"g":0.0,"b":0.0,"colour_profile_id":null}},
                 {"position":100.0,"midpoint":null,"opacity":50.0,"colour":{"r":1.0,"g":1.0,"b":1.0,"colour_profile_id":null}}]}})
}
fn design(rows: Vec<Value>) -> StudioDesignRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.design_records@1","document_id":identity("SDOC",1),
        "nodes":[{"node":{"node_kind":"layer","node_id":identity("SLYR",4)},"style_refs":{"fill_style_id":identity("SSTY",1)}}],
        "paint_styles":[{"schema_id":"hsk.studio.style_registry@1","style_id":identity("SSTY",1),"kind":"PAINT",
            "name":"fills","publishing":{"description":"","description_markdown":"","documentation_links":[],
            "publish_key":"fills","is_remote":false},"rows":rows}]}))
    .expect("design records shape")
}
fn records() -> StudioVectorRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1)})).unwrap()
}
fn prepared(unit: &str, rows: Vec<Value>, res: &VectorResolution) -> PreparedVectors {
    let snap = open(&doc(unit));
    prepare_vectors(&snap, &design(rows), &records(), &Assets, BlendSpace::Encoded, res, &CancellationToken::default()).unwrap()
}
fn no_tiles(_: &TileRef) -> Option<TileInfo> {
    None
}

#[test]
fn folio_nib_mode_maps_strokes_and_gradients_and_refuses_typed() {
    bounded_case("folio_nib_mode_maps_strokes_and_gradients_and_refuses_typed", || {
        let id = identity("SLYR", 4);
        let rows = || vec![fill_row(gradient("classic")), fill_row(solid(0.8)), stroke_row(solid(0.1), "px")];
        let v = prepared("px", rows(), &exact());
        let plane = v.plane(&id).unwrap_or_else(|| panic!("{:?}", v.refusal(&id)));
        assert!(matches!(plane.fills[0].paint, PreparedPaint::Painted(_)));
        assert_eq!(plane.fills[2].opacity, 0.5);
        assert_eq!(plane.strokes.len(), 1);
        // The stroke row maps Folio's style: 2 px, round cap, bevel join.
        let style = hsk_studio_composite::vector::StrokeStyle {
            width: 2.0,
            cap: hsk_studio_composite::vector::LineCap::Round,
            join: hsk_studio_composite::vector::LineJoin::Bevel,
            miter_limit: 4.0,
            dash: None,
            align: hsk_studio_composite::vector::StrokeAlign::Center,
        };
        let square = Assets.path(&identity("SVPT", 1)).unwrap();
        let outline = outline_stroke(&square, &style, &StrokeOptions::default()).unwrap().path;
        let (want, _) = rasterize(&outline, frame(plane.placement.unwrap()), &RasterOptions::default()).unwrap();
        assert_eq!(plane.strokes[0].coverage, want.data);
        // The document plan composites it (lowering through the Folio graph).
        let snap = open(&doc("px"));
        let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &CancellationToken::default()).unwrap();
        let inputs = RenderInputs { vectors: Some(&v), ..RenderInputs::default() };
        let report = evaluate_with_inputs(&plan, &snap, &inputs, &CancellationToken::default()).unwrap();
        let root = addr(4, "source", "out");
        assert_eq!(report.step(&root).unwrap().status, StepStatus::Ready);
        lower_with_inputs(&plan, &report, &snap, &inputs, &root, target(), &no_tiles).unwrap();

        // Supersampled mode: the earlier refusals, unchanged.
        let refusal = |unit: &str, rows: Vec<Value>, res: &VectorResolution| prepared(unit, rows, res).refusal(&id);
        let supersampled = VectorResolution { coverage: VectorCoverage::Supersampled, ..VectorResolution::default() };
        assert_eq!(refusal("px", rows(), &supersampled), Some("paint_gradient"));
        assert_eq!(refusal("px", vec![fill_row(solid(0.8)), stroke_row(solid(0.1), "px")], &supersampled),
            Some("vector_stroke_not_rendered"));
        // nib mode refusals.
        assert_eq!(refusal("mm", rows(), &exact()), Some(REFUSE_GEOMETRY_UNIT_KEY));
        assert_eq!(refusal("px", vec![stroke_row(solid(0.1), "mm")], &exact()), Some(REFUSE_GEOMETRY_UNIT_KEY));
        assert_eq!(refusal("px", vec![stroke_row(solid(0.1), "px"), fill_row(solid(0.8))], &exact()), Some("vector_stroke_order"));
        assert_eq!(refusal("px", vec![fill_row(gradient("perceptual"))], &exact()), Some("vector_paint"));
    });
}
const REFUSE_GEOMETRY_UNIT_KEY: &str = "vector_geometry_unit";
