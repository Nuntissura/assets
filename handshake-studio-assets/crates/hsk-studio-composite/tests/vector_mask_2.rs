//! Lane SMB-COMPOSITE-VECTOR-MASK-2: Folio `StudioVectorMaskParameters` (density, feather, link,
//! Illustrator's `reveal_outside`) lower to pigment's mask density and feather; a vector clip on a
//! pass-through group takes pigment's isolation rule; raster sources are rendered by the caller
//! (`pending_mask_sources` -> `mask_source_lowerings` -> `resolve_mask_source`); an unlinked mask
//! becomes a composition-space alpha matte. Pixel proofs: render-cpu / render-gpu
//! `tests/vector_mask_2.rs`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::vector::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_composite::*;
use hsk_studio_folio::{Snapshot, StudioDesignRecords, StudioImageSource, StudioVectorRecords, TileRef};
use hsk_studio_nib::raster::{PixelFrame, RasterOptions, rasterize};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, ColourTag, MaskFormat, OutputTarget, PlanNode, SampleFormat,
};
use serde_json::{Value, json};

mod common;
use common::*;

fn space() -> ColourTag {
    ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded }
}
fn target() -> OutputTarget {
    OutputTarget {
        extent: Rect { x: 0, y: 0, width: 64, height: 64 },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: space(),
    }
}
fn info(t: &TileRef) -> Option<TileInfo> {
    Some(TileInfo { format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight,
        colour_sha256: [t.object_key.len() as u8; 32], alpha_sha256: [9; 32] })
}
fn poly(points: &[(f64, f64)]) -> OwnedPath {
    let mut elements = vec![PathElement::MoveTo(PathPoint::new(points[0].0, points[0].1))];
    elements.extend(points[1..].iter().map(|p| PathElement::LineTo(PathPoint::new(p.0, p.1))));
    elements.push(PathElement::Close);
    OwnedPath { elements, fill_rule: FillRule::NonZero }
}
fn diamond() -> OwnedPath {
    poly(&[(20.0, 3.5), (36.25, 20.0), (20.0, 36.5), (3.5, 20.0)])
}
struct Assets;
impl VectorAssets for Assets {
    fn path(&self, id: &str) -> Option<OwnedPath> {
        if id == identity("SVPT", 1) {
            Some(poly(&[(2.0, 2.0), (40.0, 6.0), (10.0, 36.0)]))
        } else if id == identity("SVPT", 2) {
            Some(diamond())
        } else {
            None
        }
    }
    fn image(&self, _: &StudioImageSource) -> Option<ImageInfo> {
        None
    }
}
/// Group 1 (pass-through) composites raster 2 (48x48) and vector 3; vector 4 (hidden diamond) is
/// a VECTOR / LUMINANCE source; raster 2 is also an ALPHA source for vector 3.
fn document() -> Value {
    let mut hidden = layer(4, "vector", json!({"payload_kind":"vector","path_ids":[identity("SVPT",2)]}));
    hidden["visible"] = json!(false);
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"px",
        "colour_mode":3,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,
        "working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[layer(1, "group", json!({"payload_kind":"group"})),
            layer(2, "raster", json!({"payload_kind":"raster","tiles":[tile(2, "t2", 0, 0, 48, 48)]})),
            layer(3, "vector", json!({"payload_kind":"vector","path_ids":[identity("SVPT",1)]})),
            hidden],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[source(2, "image"), source(3, "image"), composite(1)],
            "edges":[edge(address(2, "source", "out"), address(1, "mix", "inputs"), 0),
                     edge(address(3, "source", "out"), address(1, "mix", "inputs"), 1)],
            "outputs":[address(1, "mix", "out")]}
    });
    assign_all_profiles(&mut v);
    v
}
fn design() -> StudioDesignRecords {
    let style = |n: u8, rgb: [f64; 3]| json!({"schema_id":"hsk.studio.style_registry@1","style_id":identity("SSTY",n),"kind":"PAINT",
        "name":format!("f{n}"),"publishing":{"description":"","description_markdown":"","documentation_links":[],"publish_key":format!("f{n}"),"is_remote":false},
        "rows":[{"row_kind":"fill","paint":{"paint_kind":"solid","colour":{"r":rgb[0],"g":rgb[1],"b":rgb[2],"colour_profile_id":null}},
            "opacity":100.0,"blend_mode":2,"visible":true}]});
    let node = |l: u8, s: u8| json!({"node":{"node_kind":"layer","node_id":identity("SLYR",l)},"style_refs":{"fill_style_id":identity("SSTY",s)}});
    serde_json::from_value(json!({"schema_id":"hsk.studio.design_records@1","document_id":identity("SDOC",1),
        "nodes":[node(3, 1), node(4, 2)], "paint_styles":[style(1, [0.2, 0.4, 0.6]), style(2, [0.9, 0.8, 0.3])]})).unwrap()
}
fn records(attachments: Value, params: Value) -> StudioVectorRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),
        "clips":[{"clip_id":"c0","source_layer_id":identity("SLYR",4),"clip_rule":"NONZERO"}],
        "masks":[
            {"mask_id":"m-vec","source_layer_id":identity("SLYR",4),"mode":"VECTOR"},
            {"mask_id":"m-lum","source_layer_id":identity("SLYR",4),"mode":"LUMINANCE"},
            {"mask_id":"m-alpha","source_layer_id":identity("SLYR",4),"mode":"ALPHA"},
            {"mask_id":"m-raster","source_layer_id":identity("SLYR",2),"mode":"ALPHA"}],
        "mask_parameters":params,
        "attachments":attachments})).unwrap()
}
struct Run {
    vectors: PreparedVectors,
    report: EvalReport,
    low: Result<InputsLowering, CompositeError>,
}
fn run(recs: &StudioVectorRecords, resolve: impl FnOnce(&mut PreparedVectors, &LoweredPlan, &Snapshot)) -> Run {
    let snap = open(&document());
    let token = CancellationToken::default();
    let res = VectorResolution { coverage: VectorCoverage::Exact, ..VectorResolution::default() };
    let mut vectors = prepare_vectors(&snap, &design(), recs, &Assets, BlendSpace::Encoded, &res, &token).unwrap();
    let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
    resolve(&mut vectors, &plan, &snap);
    let inputs = RenderInputs { vectors: Some(&vectors), ..RenderInputs::default() };
    let report = evaluate_with_inputs(&plan, &snap, &inputs, &token).unwrap();
    let low = lower_with_inputs(&plan, &report, &snap, &inputs, &addr(1, "mix", "out"), target(), &info);
    Run { vectors, report, low }
}
fn status(r: &Run, n: u8) -> StepStatus {
    r.report.steps.iter().find(|s| s.addr.layer_id == identity("SLYR", n)).unwrap().status.clone()
}
fn u16s(b: &[u8]) -> Vec<u16> {
    b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
}
fn att(pairs: &[(u8, &str)]) -> Value {
    Value::Array(pairs.iter().map(|(l, m)| json!({"layer_id":identity("SLYR",*l),"mask_id":m})).collect())
}

#[test]
fn mask_parameters_pass_through_clips_rendered_sources_and_unlinked_mattes() {
    bounded_case("mask_parameters_pass_through_clips_rendered_sources_and_unlinked_mattes", || {
        let none = |_: &mut PreparedVectors, _: &LoweredPlan, _: &Snapshot| {};
        // (1) density 40 % and feather 2.5 px: pigment's MaskInput.density and envelope feather.
        let r = run(&records(att(&[(2, "m-vec")]), json!([{"mask_id":"m-vec","density":40.0,"feather":2.5}])), none);
        let low = r.low.unwrap();
        let PlanNode::Group(root) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("root") };
        let m = root.layers[0].mask.unwrap();
        assert_eq!((m.format, m.density), (MaskFormat::CoverageU16Le, 0.4));
        let e = low.extensions.layers.iter().find(|e| e.group == low.plan.root && e.layer == 0).expect("feather entry");
        assert_eq!(e.mask_feather_px, 2.5);
        let has = |l: &InputsLowering, k: &str| l.exactness.operators.iter().any(|o| o.key == k);
        assert!(has(&low, "mask_density") && has(&low, "mask_feather"), "both envelope operators in the ledger");
        // Defaults: no parameters = the lane-1 plan (density 1, no envelope entry).
        let plain = run(&records(att(&[(2, "m-vec")]), json!([])), none).low.unwrap();
        assert!(plain.extensions.layers.is_empty());

        // (2) reveal_outside (Illustrator Clip off): LUMINANCE over white, full outside the art.
        let r = run(&records(att(&[(2, "m-lum")]), json!([{"mask_id":"m-lum","reveal_outside":true}])), none);
        let low = r.low.unwrap();
        let PlanNode::Group(root) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!() };
        let m = root.layers[0].mask.unwrap();
        let got = u16s(low.vector_mask(m.key, &r.vectors).unwrap().bytes);
        let at = |x: i64, y: i64| got[((y - m.placement.y) * i64::from(m.placement.width) + (x - m.placement.x)) as usize];
        // Outside the diamond: white = luma 1; at its centre: the fill colour's luma.
        assert_eq!(at(1, 1), 65535);
        let luma = 0.2126f32 * 0.9 + 0.7152 * 0.8 + 0.0722 * 0.3;
        assert!(at(20, 20).abs_diff((f64::from(luma) * 65535.0).round() as u16) <= 1, "{}", at(20, 20));
        let r = run(&records(att(&[(2, "m-alpha")]), json!([{"mask_id":"m-alpha","reveal_outside":true}])), none);
        assert_eq!(status(&r, 2), StepStatus::Unavailable("vector_mask_reveal_outside_mode".into()));

        // (3) A vector clip on the pass-through group (root, wrapped): pigment's isolation rule.
        let mut clipped = records(json!([]), json!([]));
        clipped.attachments.push(serde_json::from_value(json!({"layer_id":identity("SLYR",1),"clip_id":"c0"})).unwrap());
        let low = run(&clipped, none).low.unwrap();
        let PlanNode::Group(wrap) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!() };
        assert_eq!(wrap.layers[0].mode, StudioBlendMode::Normal);
        let PlanNode::Group(inner) = &low.plan.nodes[wrap.layers[0].input.0 as usize] else { panic!() };
        assert!(inner.isolated && !low.plan.has_pass_through_mask());
        assert!(low.notes.iter().any(|n| n.key == "mask_isolates_pass_through"));
        assert_eq!(low.exactness.operators.iter().find(|o| o.key == "mask_isolates_pass_through").map(|o| o.exactness), Some(Exactness::Approximate));

        // (4) A raster ALPHA source: pending, lowered alone, rendered by the caller, resolved.
        let recs = records(att(&[(3, "m-raster")]), json!([]));
        let r = run(&recs, none);
        assert_eq!(status(&r, 3), StepStatus::Unavailable("vector_mask_source_not_rendered".into()));
        let [p] = r.vectors.pending_mask_sources() else { panic!("one pending") };
        assert_eq!((p.mask_id.as_str(), p.layers.as_slice()), ("m-raster", &[identity("SLYR", 3)][..]));
        let rect = Rect { x: 0, y: 0, width: 48, height: 48 };
        // The caller's "render" of raster 2: a horizontal alpha ramp (what its tile holds).
        let frame: Vec<[f32; 4]> = (0..48 * 48).map(|i| [0.5, 0.5, 0.5, (i % 48) as f32 / 47.0]).collect();
        let r = run(&recs, |v, plan, snap| {
            let inputs = RenderInputs { vectors: Some(v), ..RenderInputs::default() };
            let lowered = mask_source_lowerings(plan, snap, &inputs, space(), &info, &CancellationToken::default()).unwrap();
            let [one] = lowered.as_slice() else { panic!("one lowering") };
            assert_eq!((one.mask_id.as_str(), one.rect), ("m-raster", Some(rect)));
            let source = one.lowering.as_ref().unwrap().as_ref().unwrap();
            // The source subtree alone: its own tile, nothing else.
            assert!(source.sources.iter().all(|(_, t)| t.layer_id == identity("SLYR", 2)) && source.sources.len() == 1);
            assert_eq!(v.resolve_mask_source("m-raster", Some(rect), &frame[..10]), Err("vector_mask_budget"));
            v.resolve_mask_source("m-raster", Some(rect), &frame).unwrap();
            assert!(v.pending_mask_sources().is_empty());
        });
        assert_eq!(status(&r, 3), StepStatus::Ready);
        let low = r.low.unwrap();
        let PlanNode::Group(root) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!() };
        let m = root.layers[1].mask.unwrap();
        let got = u16s(low.vector_mask(m.key, &r.vectors).unwrap().bytes);
        for (i, g) in got.iter().enumerate() {
            let (x, y) = (m.placement.x + (i % m.placement.width as usize) as i64, m.placement.y + (i / m.placement.width as usize) as i64);
            let want = if (0..48).contains(&x) && (0..48).contains(&y) { (f64::from(x as f32 / 47.0) * 65535.0).round() as u16 } else { 0 };
            assert!(g.abs_diff(want) <= 1, "({x},{y}) {g} vs {want}");
        }
        // A failed source render: the layer carries the code.
        let r = run(&recs, |v, _, _| v.fail_mask_source("m-raster", "vector_mask_source_unavailable"));
        assert_eq!(status(&r, 3), StepStatus::Unavailable("vector_mask_source_unavailable".into()));

        // (5) Unlinked: a composition-space alpha matte of the same plane; density refused there.
        let r = run(&records(att(&[(2, "m-vec")]), json!([{"mask_id":"m-vec","linked":false,"feather":1.5}])), none);
        let mask = r.vectors.mask(&identity("SLYR", 2)).unwrap().unwrap();
        assert!(!mask.linked);
        let matte = unlinked_mask_matte(mask, "L2", space(), 7).unwrap();
        assert_eq!((matte.mode, matte.inverted), (SceneMatteMode::Alpha, false));
        let layer = matte.layer.expect("matte layer");
        assert_eq!(layer.matrix, hsk_studio_pigment::transform_math::Affine2::IDENTITY);
        let SceneContent::Static(content) = &layer.content else { panic!("static matte") };
        let plane = content.owned_masks.values().next().unwrap();
        let r0 = plane.bounds;
        let frame = PixelFrame { x: r0.x as i32, y: r0.y as i32, width: r0.width, height: r0.height };
        let want: Vec<u16> = rasterize(&diamond(), frame, &RasterOptions::default()).unwrap().0.data.iter().map(|c| u16::from(*c) * 257).collect();
        assert_eq!(u16s(&plane.bytes), want, "the matte is the nib plane");
        assert_eq!(content.extensions.layers[0].mask_feather_px, 1.5);
        let dense = VectorMask { density: 0.5, ..mask.clone() };
        let e = unlinked_mask_matte(&dense, "L2", space(), 7).unwrap_err();
        assert!(e.to_string().contains("vector_mask_unlinked_density"), "{e}");
    });
}
