//! SMB-COMPOSITE-NEW-KINDS slice 4 (Folio G1): a text source whose `out` port is image-typed feeds
//! a group's image inputs and lowers like a prepared text plane (Solid + coverage_u8 mask) inside
//! that group. Expected shapes are derived by hand from the fixture; the text plane is Type's, the
//! same one the text-root path pins (tests/text_lowering.rs).
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::*;
use hsk_studio_folio::TileRef;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, ColourTag, MaskFormat, OutputTarget, PlanNode, SampleFormat,
};
use hsk_studio_type::FontBytes;
use serde_json::{Value, json};

mod common;
use common::*;
#[path = "common/text_shaping.rs"]
mod shaping;
use shaping::{FontFile, Seg, shape};

const FILL: [f32; 4] = [0.8, 0.1, 0.2, 1.0];

/// A mask input without its source key.
#[derive(Debug, PartialEq)]
struct MaskInputView(Rect, MaskFormat, [u8; 32], bool, f32);
impl MaskInputView {
    fn of(m: hsk_studio_pigment::render_plan::MaskInput) -> Self {
        Self(m.placement, m.format, m.sha256, m.inverted, m.density)
    }
}

fn target() -> OutputTarget {
    OutputTarget {
        extent: Rect { x: 0, y: 0, width: 128, height: 128 },
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        space: ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded },
    }
}
fn info(tile: &TileRef) -> Option<TileInfo> {
    Some(TileInfo {
        format: SampleFormat::RgbF32Le,
        alpha: AlphaConvention::Straight,
        colour_sha256: [tile.object_key.len() as u8; 32],
        alpha_sha256: [9; 32],
    })
}
/// Group 1 composites raster 2 (ordinal 0, bottom) then text 3 (ordinal 1, multiply 50 %), whose
/// source declares its `out` port `image` (G1).
fn doc() -> Value {
    let mut text = layer(3, "text", json!({"payload_kind":"text","story_id":identity("STXT",1)}));
    text["appearance"] = json!({"opacity":50.0,"fill_opacity":100.0,"blend_mode":
        hsk_studio_folio::BLEND_NORMAL + 3,"clipped":false,"isolation":"isolated"});
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"mm","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[layer(1, "group", json!({"payload_kind":"group"})), layer(2, "raster", json!({"payload_kind":"raster","tiles":[tile(2, "t00", 0, 0, 64, 64)]})), text],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[source(2, "image"), composite(1), source(3, "image")],
            "edges":[edge(address(2, "source", "out"), address(1, "mix", "inputs"), 0),
                     edge(address(3, "source", "out"), address(1, "mix", "inputs"), 1)],
            "outputs":[address(1, "mix", "out")]}
    });
    assign_all_profiles(&mut v);
    v
}
fn hi() -> PreparedText {
    let files = [FontFile::inter()];
    let segs = vec![Seg { start: 0, end: 2, font: 0, script: *b"Latn", language: "en", axes: vec![] }];
    let res = TextResolution::at_ppi(192.0);
    shape(&files, "Hi", &segs, false, 12.0, |p| {
        let mut x = 10.25;
        let mut runs = Vec::new();
        for run in p.paragraphs()[0].runs() {
            let file = files.iter().find(|f| f.identity == run.resolved().identity).unwrap();
            runs.push(TextRun { run, font: FontBytes { bytes: &file.bytes, resource: run.resolved() }, origin_px: [x, 40.0] });
            x += run.glyphs().iter().map(|g| g.x_advance_pt).sum::<f64>() * res.px_per_pt;
        }
        let id = identity("SLYR", 3);
        prepare_text(&[TextLayerInput { layer_id: &id, runs: &runs, fill_rgba: FILL }], &res, &CancellationToken::default())
    })
    .unwrap()
}

#[test]
fn image_typed_text_composites_inside_a_group_over_a_raster() {
    bounded_case("image_typed_text_composites_inside_a_group_over_a_raster", || {
        let token = CancellationToken::default();
        let text = hi();
        let plane = text.plane(&identity("SLYR", 3)).unwrap().clone();
        let rect = plane.placement.unwrap();
        let snap = open(&doc());
        let plan = plan_of(&snap);
        let root = addr(1, "mix", "out");

        // Without prepared text the image-typed text source is a typed refusal, never dropped.
        let plain = evaluate(&plan, &snap, &token).unwrap();
        assert_eq!(
            plain.step(&addr(3, "source", "out")).unwrap().status,
            StepStatus::Unavailable("text_to_image_conversion".into())
        );
        assert!(lower_to_render_plan(&plan, &plain, &snap, &root, target(), &info).is_err());

        let report = evaluate_with_text(&plan, &snap, &text, &token).unwrap();
        report.require_ready().unwrap();
        // Group extent = raster tile union text plane.
        let ext = report.step(&root).unwrap().extent.unwrap();
        let x1 = (rect.x + i64::from(rect.width)).max(64);
        let y1 = (rect.y + i64::from(rect.height)).max(64);
        assert_eq!(ext, Rect { x: 0, y: 0, width: x1 as u32, height: y1 as u32 });

        let low = lower_to_render_plan_with_text(&plan, &report, &snap, &root, target(), &info, &text).unwrap();
        let nodes = &low.lowering.plan.nodes;
        let PlanNode::Group(top) = &nodes[low.lowering.plan.root.0 as usize] else { panic!("root group") };
        assert!(top.isolated);
        assert_eq!(top.layers.len(), 2, "raster bottom, text top");
        assert!(matches!(nodes[top.layers[0].input.0 as usize], PlanNode::Tile(_)));
        let text_layer = &top.layers[1];
        assert_eq!(text_layer.mode, StudioBlendMode::Multiply);
        assert_eq!(text_layer.opacity, 0.5);
        let PlanNode::Group(content) = &nodes[text_layer.input.0 as usize] else { panic!("text content group") };
        let [inner] = content.layers.as_slice() else { panic!("one Solid layer") };
        assert_eq!(nodes[inner.input.0 as usize], PlanNode::Solid { rect, rgba: FILL });
        let mask = inner.mask.unwrap();
        assert_eq!((mask.format, mask.placement, mask.sha256), (MaskFormat::CoverageU8, rect, plane.sha256));
        // The text content node is exactly the standalone text plan's root (the renderer-proven one).
        let alone = text_layer_render_plan(&text, &identity("SLYR", 3), 7, target()).unwrap();
        let alone_plan = &alone.lowering.plan;
        let PlanNode::Group(alone_root) = &alone_plan.nodes[alone_plan.root.0 as usize] else { panic!() };
        // Same mask but for its key (the tile below took key 0 in the document plan).
        let alone_mask = alone_root.layers[0].mask.unwrap();
        assert_eq!(MaskInputView::of(alone_mask), MaskInputView::of(mask));
        assert_eq!(alone_plan.nodes[alone_root.layers[0].input.0 as usize], nodes[inner.input.0 as usize]);
        // The mask is served from the prepared plane.
        assert_eq!(low.text_mask(mask.key, &text).unwrap().sha256, plane.sha256);
        assert_eq!(low.lowering.sources.len(), 1);
    });
}
