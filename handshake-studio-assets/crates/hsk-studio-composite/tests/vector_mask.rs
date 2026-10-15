//! Lane SMB-COMPOSITE-VECTOR-MASK: Folio vector masks (`StudioVectorMask` +
//! `StudioMaskAttachment::mask_id`) lower to the masked layer's `coverage_u16le` plane
//! (`crate::vector_mask`): VECTOR = nib coverage of the source outline, ALPHA / LUMINANCE = the
//! source group rendered alone (pigment `composite_over`), invert, region, refusals. Expected planes
//! are computed here from nib called directly and pigment's scalar compositing. Pixel proofs (CPU
//! and GPU against one oracle): render-cpu / render-gpu `tests/vector_mask.rs`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::vector::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_composite::*;
use hsk_studio_folio::{StudioDesignRecords, StudioImageSource, StudioVectorRecords, TileRef};
use hsk_studio_nib::raster::{PixelFrame, RasterOptions, rasterize};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, ColourTag, MaskFormat, OutputTarget, PlanNode, SampleFormat,
};
use serde_json::{Value, json};

mod common;
use common::*;

fn target() -> OutputTarget {
    OutputTarget {
        extent: Rect { x: 0, y: 0, width: 64, height: 64 },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
    }
}
fn info(t: &TileRef) -> Option<TileInfo> {
    Some(TileInfo { format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight,
        colour_sha256: [t.object_key.len() as u8; 32], alpha_sha256: [9; 32] })
}
fn poly(points: &[(f64, f64)], rule: FillRule) -> OwnedPath {
    let mut elements = vec![PathElement::MoveTo(PathPoint::new(points[0].0, points[0].1))];
    elements.extend(points[1..].iter().map(|p| PathElement::LineTo(PathPoint::new(p.0, p.1))));
    elements.push(PathElement::Close);
    OwnedPath { elements, fill_rule: rule }
}
fn path_id(n: u32) -> String {
    format!("SVPT-019abcde-0000-7000-8000-{n:012x}")
}
/// SVPT 1: vector 3's triangle; 2: the diamond of VECTOR source 4; 3 and 4: the square and the
/// triangle of group source 5's members 6 and 7; 5: a degenerate (collinear) path; 6: non-finite;
/// 100..: tiny squares (the huge-count source).
struct Assets;
impl VectorAssets for Assets {
    fn path(&self, id: &str) -> Option<OwnedPath> {
        let n = u32::from_str_radix(id.rsplit('-').next()?, 16).ok()?;
        Some(match n {
            1 => poly(&[(2.0, 2.0), (40.0, 6.0), (10.0, 36.0)], FillRule::NonZero),
            2 => poly(&[(20.0, 3.5), (36.25, 20.0), (20.0, 36.5), (3.5, 20.0)], FillRule::NonZero),
            3 => poly(&[(6.5, 6.0), (30.0, 6.0), (30.0, 28.5), (6.5, 28.5)], FillRule::NonZero),
            4 => poly(&[(16.0, 10.0), (44.5, 18.0), (20.0, 44.0)], FillRule::NonZero),
            5 => poly(&[(4.0, 4.0), (20.0, 20.0), (36.0, 36.0)], FillRule::NonZero),
            6 => poly(&[(f64::NAN, 4.0), (20.0, 20.0), (4.0, 36.0)], FillRule::NonZero),
            n if n >= 100 => {
                let (x, y) = (f64::from(n % 40), f64::from((n / 40) % 40));
                poly(&[(x, y), (x + 0.5, y), (x + 0.5, y + 0.5), (x, y + 0.5)], FillRule::NonZero)
            }
            _ => return None,
        })
    }
    fn image(&self, _: &StudioImageSource) -> Option<ImageInfo> {
        None
    }
}
fn child(n: u8, parent: u8, path: u32, appearance: Value) -> Value {
    let mut l = layer(n, "vector", json!({"payload_kind":"vector","path_ids":[path_id(path)]}));
    l["parent"] = json!({"node_kind":"layer","node_id":identity("SLYR",parent)});
    l["appearance"] = appearance;
    l
}
fn hidden(mut l: Value) -> Value {
    l["visible"] = json!(false);
    l
}
fn appearance(opacity: f64, mode: u16) -> Value {
    json!({"opacity":opacity,"fill_opacity":100.0,"blend_mode":mode,"clipped":false,"isolation":"isolated"})
}
/// Group 1 composites raster 2 (48x48) and vector 3; hidden mask sources: vector 4 (diamond),
/// group 5 (members 6: red square 50 %, 7: blue triangle multiply), vector 8 (degenerate path),
/// vector 9 (non-finite path), vector 10 (1100 tiny squares). Document unit px.
fn document() -> Value {
    let many: Vec<String> = (100..1200).map(path_id).collect();
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"px",
        "colour_mode":3,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,
        "working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[layer(1, "group", json!({"payload_kind":"group"})),
            layer(2, "raster", json!({"payload_kind":"raster","tiles":[tile(2, "t2", 0, 0, 48, 48)]})),
            layer(3, "vector", json!({"payload_kind":"vector","path_ids":[path_id(1)]})),
            hidden(layer(4, "vector", json!({"payload_kind":"vector","path_ids":[path_id(2)]}))),
            hidden(layer(5, "group", json!({"payload_kind":"group"}))),
            child(6, 5, 3, appearance(50.0, 2)),
            child(7, 5, 4, appearance(100.0, 5)),
            hidden(layer(8, "vector", json!({"payload_kind":"vector","path_ids":[path_id(5)]}))),
            hidden(layer(9, "vector", json!({"payload_kind":"vector","path_ids":[path_id(6)]}))),
            hidden(layer(10, "vector", json!({"payload_kind":"vector","path_ids":many})))],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[source(2, "image"), source(3, "image"), composite(1)],
            "edges":[edge(address(2, "source", "out"), address(1, "mix", "inputs"), 0),
                     edge(address(3, "source", "out"), address(1, "mix", "inputs"), 1)],
            "outputs":[address(1, "mix", "out")]}
    });
    assign_all_profiles(&mut v);
    v
}
const RED: [f32; 3] = [0.9, 0.1, 0.1];
const BLUE: [f32; 3] = [0.1, 0.2, 0.9];
fn style(n: u8, rgb: [f32; 3]) -> Value {
    json!({"schema_id":"hsk.studio.style_registry@1","style_id":identity("SSTY",n),"kind":"PAINT",
        "name":format!("f{n}"),"publishing":{"description":"","description_markdown":"","documentation_links":[],"publish_key":format!("f{n}"),"is_remote":false},
        "rows":[{"row_kind":"fill","paint":{"paint_kind":"solid","colour":{"r":rgb[0],"g":rgb[1],"b":rgb[2],"colour_profile_id":null}},
            "opacity":100.0,"blend_mode":2,"visible":true}]})
}
fn design() -> StudioDesignRecords {
    let node = |layer: u8, s: u8| json!({"node":{"node_kind":"layer","node_id":identity("SLYR",layer)},"style_refs":{"fill_style_id":identity("SSTY",s)}});
    serde_json::from_value(json!({"schema_id":"hsk.studio.design_records@1","document_id":identity("SDOC",1),
        "nodes":[node(3, 1), node(6, 2), node(7, 3), node(10, 1)],
        "paint_styles":[style(1, [0.2, 0.4, 0.6]), style(2, RED), style(3, BLUE)]})).unwrap()
}
fn px(v: f64) -> Value {
    json!({"value":v,"unit":"px"})
}
fn records(attachments: Value) -> StudioVectorRecords {
    let region = json!({"x":px(8.0),"y":px(8.5),"width":px(24.0),"height":px(19.25)});
    serde_json::from_value(json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),
        "clips":[{"clip_id":"c0","source_layer_id":identity("SLYR",4),"clip_rule":"NONZERO"}],
        "masks":[
            {"mask_id":"m-vec","source_layer_id":identity("SLYR",4),"mode":"VECTOR"},
            {"mask_id":"m-vinv","source_layer_id":identity("SLYR",4),"mode":"VECTOR","invert_mask":true},
            {"mask_id":"m-alpha","source_layer_id":identity("SLYR",5),"mode":"ALPHA"},
            {"mask_id":"m-lum","source_layer_id":identity("SLYR",5),"mode":"LUMINANCE","invert_mask":true,"region":region},
            {"mask_id":"m-pt","source_layer_id":identity("SLYR",4),"mode":"ALPHA",
                "region":{"x":{"value":1.0,"unit":"pt"},"y":px(0.0),"width":px(4.0),"height":px(4.0)}},
            {"mask_id":"m-raster","source_layer_id":identity("SLYR",2),"mode":"ALPHA"},
            {"mask_id":"m-flat","source_layer_id":identity("SLYR",8),"mode":"VECTOR"},
            {"mask_id":"m-nan","source_layer_id":identity("SLYR",9),"mode":"VECTOR"},
            {"mask_id":"m-many","source_layer_id":identity("SLYR",10),"mode":"VECTOR"},
            {"mask_id":"m-many-alpha","source_layer_id":identity("SLYR",10),"mode":"ALPHA"}],
        "attachments":attachments})).unwrap()
}
fn attach(pairs: &[(u8, &str)]) -> Value {
    Value::Array(pairs.iter().map(|(l, m)| json!({"layer_id":identity("SLYR",*l),"mask_id":m})).collect())
}
struct Lowered {
    vectors: PreparedVectors,
    report: EvalReport,
    low: Result<InputsLowering, CompositeError>,
}
fn lowered(doc: &Value, recs: &StudioVectorRecords, root: u8) -> Lowered {
    let snap = open(doc);
    let token = CancellationToken::default();
    let res = VectorResolution { coverage: VectorCoverage::Exact, ..VectorResolution::default() };
    let vectors = prepare_vectors(&snap, &design(), recs, &Assets, BlendSpace::Encoded, &res, &token).unwrap();
    let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
    let inputs = RenderInputs { vectors: Some(&vectors), ..RenderInputs::default() };
    let report = evaluate_with_inputs(&plan, &snap, &inputs, &token).unwrap();
    let low = lower_with_inputs(&plan, &report, &snap, &inputs, &addr(root, "mix", "out"), target(), &info);
    Lowered { vectors, report, low }
}
fn status(l: &Lowered, n: u8) -> StepStatus {
    l.report.steps.iter().find(|s| s.addr.layer_id == identity("SLYR", n)).unwrap().status.clone()
}
fn u16s(bytes: &[u8]) -> Vec<u16> {
    bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
}
fn nib(path: u32, r: Rect) -> Vec<u8> {
    let frame = PixelFrame { x: r.x as i32, y: r.y as i32, width: r.width, height: r.height };
    rasterize(&Assets.path(&path_id(path)).unwrap(), frame, &RasterOptions::default()).unwrap().0.data
}
/// Group source 5 rendered alone at plane pixel `i` (independent of composite: nib coverage and
/// pigment `composite_over`): member 6 red at 50 %, member 7 blue multiply.
fn group_source(c6: u8, c7: u8, site: PixelSite) -> Rgba {
    let k = |c: u8| (f64::from(c) / 255.0) as f32;
    let m6 = composite_over(StudioBlendMode::Normal, Rgba::TRANSPARENT, Rgba::new(RED, 1.0), k(c6), site).unwrap();
    let m7 = composite_over(StudioBlendMode::Normal, Rgba::TRANSPARENT, Rgba::new(BLUE, 1.0), k(c7), site).unwrap();
    let acc = composite_over(StudioBlendMode::Normal, Rgba::TRANSPARENT, m6, 0.5, site).unwrap();
    composite_over(StudioBlendMode::Multiply, acc, m7, 1.0, site).unwrap()
}

#[test]
fn vector_alpha_and_luminance_masks_lower_to_the_expected_planes() {
    bounded_case("vector_alpha_and_luminance_masks_lower_to_the_expected_planes", || {
        // VECTOR on raster 2, ALPHA (group source) on vector 3.
        let l = lowered(&document(), &records(attach(&[(2, "m-vec"), (3, "m-alpha")])), 1);
        let low = l.low.unwrap();
        let PlanNode::Group(root) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("root group") };
        let m = root.layers[0].mask.expect("vector mask");
        assert_eq!((m.format, m.inverted, m.density), (MaskFormat::CoverageU16Le, false, 1.0));
        let served = low.vector_mask(m.key, &l.vectors).unwrap();
        assert_eq!((served.sha256, served.sample_format), (m.sha256, "coverage_u16le"));
        // The plane covers the raster extent and the diamond; it is nib's coverage x 257.
        let r = m.placement;
        assert!(r.x <= 0 && r.y <= 0 && r.x + i64::from(r.width) >= 48 && r.y + i64::from(r.height) >= 48, "{r:?}");
        let want: Vec<u16> = nib(2, r).iter().map(|c| u16::from(*c) * 257).collect();
        assert_eq!(u16s(served.bytes), want, "VECTOR = nib coverage");
        // ALPHA of group 5 rendered alone.
        let m = root.layers[1].mask.expect("alpha mask");
        assert_eq!((m.format, m.inverted), (MaskFormat::CoverageU16Le, false));
        let r = m.placement;
        let (c6, c7) = (nib(3, r), nib(4, r));
        let got = u16s(low.vector_mask(m.key, &l.vectors).unwrap().bytes);
        let mut worst = 0u16;
        for (i, g) in got.iter().enumerate() {
            let site = PixelSite { x: r.x + (i % r.width as usize) as i64, y: r.y + (i / r.width as usize) as i64, seed: 0 };
            let a = group_source(c6[i], c7[i], site).a;
            worst = worst.max(g.abs_diff((f64::from(a) * 65535.0).round() as u16));
        }
        assert!(worst <= 1, "alpha plane off by {worst}");
        assert!(got.iter().any(|v| *v > 0 && *v < 65535) && got.contains(&0));
        assert!(low.notes.iter().any(|n| n.key == "vector_mask_content" && n.layer_id == identity("SLYR", 3)));

        // LUMINANCE, inverted, with a region (fractional edges): baked on the host (no pigment invert).
        let l = lowered(&document(), &records(attach(&[(2, "m-lum")])), 1);
        let low = l.low.unwrap();
        let PlanNode::Group(root) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!() };
        let m = root.layers[0].mask.unwrap();
        assert!(!m.inverted);
        let r = m.placement;
        let (c6, c7) = (nib(3, r), nib(4, r));
        let got = u16s(low.vector_mask(m.key, &l.vectors).unwrap().bytes);
        let mut worst = 0u16;
        for (i, g) in got.iter().enumerate() {
            let (x, y) = (r.x + (i % r.width as usize) as i64, r.y + (i / r.width as usize) as i64);
            let c = group_source(c6[i], c7[i], PixelSite { x, y, seed: 0 });
            let luma = (0.2126 * c.rgb[0] + 0.7152 * c.rgb[1] + 0.0722 * c.rgb[2]) * c.a;
            let area = |p: f64, lo: f64, hi: f64| (hi.min(p + 1.0) - lo.max(p)).max(0.0);
            let region = area(x as f64, 8.0, 32.0) * area(y as f64, 8.5, 27.75);
            let want = ((1.0 - f64::from(luma)) * region * 65535.0).round() as u16;
            worst = worst.max(g.abs_diff(want));
        }
        assert!(worst <= 1, "luminance plane off by {worst}");
        let at = |x: i64, y: i64| got[((y - r.y) * i64::from(r.width) + (x - r.x)) as usize];
        assert_eq!((at(7, 12), at(32, 12), at(12, 28)), (0, 0, 0), "zero outside the region, inverted or not");
        assert_eq!(at(31, 9), 65535, "inside the region, outside the source: inverted zero = full");

        // VECTOR inverted alone: pigment's mask invert (full outside the plane), same plane bytes.
        let inv = lowered(&document(), &records(attach(&[(2, "m-vinv")])), 1).low.unwrap();
        let PlanNode::Group(iroot) = &inv.plan.nodes[inv.plan.root.0 as usize] else { panic!() };
        let mi = iroot.layers[0].mask.unwrap();
        assert!(mi.inverted);
        let plain = lowered(&document(), &records(attach(&[(2, "m-vec")])), 1).low.unwrap();
        let PlanNode::Group(proot) = &plain.plan.nodes[plain.plan.root.0 as usize] else { panic!() };
        assert_eq!(mi.sha256, proot.layers[0].mask.unwrap().sha256);

        // A vector mask on the pass-through root group: wrapped, isolated, Normal, noted.
        let l = lowered(&document(), &records(attach(&[(1, "m-vec")])), 1);
        let low = l.low.unwrap();
        let PlanNode::Group(wrap) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!() };
        assert_eq!(wrap.layers.len(), 1);
        assert_eq!(wrap.layers[0].mode, StudioBlendMode::Normal);
        assert_eq!(wrap.layers[0].mask.unwrap().format, MaskFormat::CoverageU16Le);
        let PlanNode::Group(inner) = &low.plan.nodes[wrap.layers[0].input.0 as usize] else { panic!() };
        assert!(inner.isolated);
        assert!(low.notes.iter().any(|n| n.key == "vector_mask_isolates_pass_through"));

        // A clip and a mask on one layer: one u16 plane = mask x clip; no attachments: unchanged.
        let mut both = records(attach(&[(2, "m-alpha")]));
        both.attachments[0].clip_id = Some("c0".into());
        let low = lowered(&document(), &both, 1).low.unwrap();
        let PlanNode::Group(root) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!() };
        assert_eq!(root.layers[0].mask.unwrap().format, MaskFormat::CoverageU16Le);
        assert_eq!(low.clip_planes.len(), 1);
        let none = lowered(&document(), &records(json!([])), 1).low.unwrap();
        assert!(none.clip_planes.is_empty() && none.notes.is_empty());

        // Typed refusals in evaluation (the step is Unavailable with the code; never ignored).
        let un = |code: &str| StepStatus::Unavailable(code.into());
        for (pairs, layer_n, code) in [
            (attach(&[(2, "nope")]), 2, "vector_mask_unresolved"),
            (attach(&[(3, "m-raster")]), 3, "vector_mask_source_kind"),
            // A layer masked by its own content: the source carries a mask (itself).
            (attach(&[(2, "m-raster")]), 2, "vector_mask_source_nested"),
            (attach(&[(2, "m-pt")]), 2, "vector_mask_region"),
            (attach(&[(3, "m-alpha"), (6, "m-vec")]), 3, "vector_mask_source_nested"),
        ] {
            let l = lowered(&document(), &records(pairs), 1);
            assert_eq!(status(&l, layer_n), un(code), "{code}");
            assert!(l.low.is_err(), "{code}: lowering refused too");
        }
        let mut masked = document();
        let mut t = tile(2, "m2", 0, 0, 48, 48);
        t["format"] = json!("coverage_u8");
        masked["layers"][1]["appearance"] = json!({"opacity":100.0,"fill_opacity":100.0,"blend_mode":2,"clipped":false,
            "isolation":"isolated","mask":{"tiles":[t],"enabled":true,"linked":true}});
        let l = lowered(&masked, &records(attach(&[(2, "m-vec")])), 1);
        assert_eq!(status(&l, 2), un("vector_mask_with_mask"));
    });
}

#[test]
fn hostile_mask_sources_are_refused_or_bounded_before_allocation() {
    bounded_case("hostile_mask_sources_are_refused_or_bounded_before_allocation", || {
        let un = |code: &str| StepStatus::Unavailable(code.into());
        // 1100 paths in one VECTOR source: nib's boolean operand budget refuses (typed); as an ALPHA
        // source the source layer itself is refused, so the mask is.
        let l = lowered(&document(), &records(attach(&[(2, "m-many")])), 1);
        assert_eq!(status(&l, 2), un("vector_nib_budget"));
        let l = lowered(&document(), &records(attach(&[(2, "m-many-alpha")])), 1);
        assert_eq!(status(&l, 2), un("vector_mask_source_unavailable"));
        // Degenerate (collinear) source: an empty region, the layer masked away completely.
        let low = lowered(&document(), &records(attach(&[(2, "m-flat")])), 1).low.unwrap();
        let PlanNode::Group(root) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!() };
        assert_eq!(root.layers[0].mask.unwrap().format, MaskFormat::CoverageU16Le);
        let [(_, plane)] = low.clip_planes.as_slice() else { panic!("one plane") };
        assert!(plane.wide && plane.coverage.iter().all(|b| *b == 0));
        // A non-finite source: typed refusal, no plane.
        let l = lowered(&document(), &records(attach(&[(2, "m-nan")])), 1);
        assert!(matches!(status(&l, 2), StepStatus::Unavailable(c) if c.starts_with("vector_")), "{:?}", status(&l, 2));
        assert!(l.low.is_err());

        // Folio-free bounds: a source of 5000 members and an exhausted aggregate budget are refused
        // before any plane is allocated (the budget is not charged on refusal).
        let res = VectorResolution { coverage: VectorCoverage::Exact, ..VectorResolution::default() };
        let token = CancellationToken::default();
        let spec = VectorLayerSpec {
            layer_id: "L".into(),
            geometry: VectorGeometry::Paths(vec![Assets.path(&path_id(3)).unwrap()]),
            fills: vec![FillSpec { paint: FillPaint::Solid([1.0, 0.0, 0.0, 1.0]), mode: StudioBlendMode::Normal, opacity: 1.0 }],
        };
        let vectors = prepare_vector_layers(&[spec], &res, &token).unwrap();
        let leaf = MaskNode::Leaf { layer_id: "L".into(), mode: StudioBlendMode::Normal, opacity: 1.0 };
        let wide = MaskNode::Group { mode: StudioBlendMode::Normal, opacity: 1.0, children: vec![leaf.clone(); 5000] };
        let mut budget = u64::MAX;
        assert_eq!(mask_content(&vectors, &wide, MaskContentMode::Alpha, &mut budget), Err("vector_mask_budget"));
        let mut budget = 10;
        assert_eq!(mask_content(&vectors, &leaf, MaskContentMode::Alpha, &mut budget), Err("vector_mask_budget"));
        assert_eq!(budget, 10);
        let mut budget = 1 << 20;
        assert!(mask_content(&vectors, &leaf, MaskContentMode::Luminance, &mut budget).is_ok());
        assert!(budget < 1 << 20, "charged");
        // A mask whose source lies far away: the plane over extent + source bounds is over the
        // pixel budget and refused at lowering (typed), never allocated.
        let far = VectorMask {
            source: VectorMaskSource::Outline(Some(poly(&[(9.0e6, 9.0e6), (9.0e6 + 4.0, 9.0e6), (9.0e6, 9.0e6 + 4.0)], FillRule::NonZero))),
            invert: false,
            region: None,
        };
        let vectors = vectors.with_mask("L", far);
        let t = OutputTarget { extent: Rect { x: 0, y: 0, width: 64, height: 64 }, ..target() };
        let e = vector_layer_render_plan(&vectors, "L", 1, t).unwrap_err();
        assert!(e.to_string().contains("vector_mask_budget"), "{e}");
    });
}
