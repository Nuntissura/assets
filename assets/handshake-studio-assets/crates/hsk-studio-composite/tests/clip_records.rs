//! Lane SMB-COMPOSITE-CLIP-RECORDS slice 1: Folio vector clips (`StudioVectorClip` +
//! `StudioMaskAttachment::clip_id`, e.g. Figma frame clipping from interop-fig) cut the clipped
//! layers: the clip source's geometry under the clip rule, nib outline + nib rasterize, as each
//! clipped layer's coverage mask over its extent united with the clip bounds. Expected planes are
//! nib called directly. Pixel proofs (CPU and GPU against one oracle): render-cpu / render-gpu
//! `tests/vector_nib.rs` `clipped_*`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::vector::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_composite::*;
use hsk_studio_folio::{StudioDesignRecords, StudioImageSource, StudioVectorRecords, TileRef};
use hsk_studio_nib::raster::{PixelFrame, RasterOptions, rasterize};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::{AlphaConvention, ColourTag, MaskFormat, OutputTarget, PlanNode, SampleFormat};
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
/// SVPT 1: the vector layer's triangle; SVPT 2: the clip source's diamond (authored even-odd, the
/// clip rule NONZERO overrides it).
struct Assets;
impl VectorAssets for Assets {
    fn path(&self, id: &str) -> Option<OwnedPath> {
        if id == identity("SVPT", 1) {
            Some(poly(&[(2.0, 2.0), (40.0, 6.0), (10.0, 36.0)], FillRule::NonZero))
        } else if id == identity("SVPT", 2) {
            Some(poly(&[(20.0, 3.5), (36.25, 20.0), (20.0, 36.5), (3.5, 20.0)], FillRule::EvenOdd))
        } else {
            None
        }
    }
    fn image(&self, _: &StudioImageSource) -> Option<ImageInfo> {
        None
    }
}
/// Group 1 (a frame) composites raster 2 (tile 48x48) and vector 3 (image-typed port); vector 4
/// is the hidden clip source (no graph operation). Document unit px.
fn document() -> Value {
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"px",
        "colour_mode":3,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,
        "working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
        "layers":[layer(1, "group", json!({"payload_kind":"group"})),
            layer(2, "raster", json!({"payload_kind":"raster","tiles":[tile(2, "t2", 0, 0, 48, 48)]})),
            layer(3, "vector", json!({"payload_kind":"vector","path_ids":[identity("SVPT",1)]})),
            {"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",4),"name":"clip","parent":null,"kind":"vector",
             "payload":{"payload_kind":"vector","path_ids":[identity("SVPT",2)]},"visible":false}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[source(2, "image"), source(3, "image"), composite(1)],
            "edges":[edge(address(2, "source", "out"), address(1, "mix", "inputs"), 0),
                     edge(address(3, "source", "out"), address(1, "mix", "inputs"), 1)],
            "outputs":[address(1, "mix", "out")]}
    });
    assign_all_profiles(&mut v);
    v
}
fn design() -> StudioDesignRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.design_records@1","document_id":identity("SDOC",1),
        "nodes":[{"node":{"node_kind":"layer","node_id":identity("SLYR",3)},"style_refs":{"fill_style_id":identity("SSTY",1)}}],
        "paint_styles":[{"schema_id":"hsk.studio.style_registry@1","style_id":identity("SSTY",1),"kind":"PAINT",
            "name":"f","publishing":{"description":"","description_markdown":"","documentation_links":[],"publish_key":"f","is_remote":false},
            "rows":[{"row_kind":"fill","paint":{"paint_kind":"solid","colour":{"r":0.2,"g":0.4,"b":0.6,"colour_profile_id":null}},
                "opacity":100.0,"blend_mode":2,"visible":true}]}]})).unwrap()
}
fn records(attachments: Value) -> StudioVectorRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),
        "clips":[{"clip_id":"fig-clip-0","source_layer_id":identity("SLYR",4),"clip_rule":"NONZERO"}],
        "attachments":attachments})).unwrap()
}
fn lowered(doc: &Value, recs: &StudioVectorRecords) -> (PreparedVectors, Result<InputsLowering, CompositeError>) {
    let snap = open(doc);
    let token = CancellationToken::default();
    let res = VectorResolution { coverage: VectorCoverage::Exact, ..VectorResolution::default() };
    let vectors = prepare_vectors(&snap, &design(), recs, &Assets, BlendSpace::Encoded, &res, &token).unwrap();
    let plan = lower(&snap, 7, BlendSpace::Encoded, &Limits::default(), &token).unwrap();
    let inputs = RenderInputs { vectors: Some(&vectors), ..RenderInputs::default() };
    let report = evaluate_with_inputs(&plan, &snap, &inputs, &token).unwrap();
    let low = lower_with_inputs(&plan, &report, &snap, &inputs, &addr(1, "mix", "out"), target(), &info);
    (vectors, low)
}

#[test]
fn frame_clips_cut_their_child_layers_with_the_nib_clip_plane() {
    bounded_case("frame_clips_cut_their_child_layers_with_the_nib_clip_plane", || {
        let attach = json!([{"layer_id":identity("SLYR",2),"clip_id":"fig-clip-0"},{"layer_id":identity("SLYR",3),"clip_id":"fig-clip-0"}]);
        let (vectors, low) = lowered(&document(), &records(attach));
        let low = low.unwrap();
        let PlanNode::Group(root) = &low.plan.nodes[low.plan.root.0 as usize] else { panic!("root group") };
        assert_eq!(root.layers.len(), 2);
        // The clip outline under NONZERO (the authored even-odd rule of the source is overridden).
        let mut diamond = Assets.path(&identity("SVPT", 2)).unwrap();
        diamond.fill_rule = FillRule::NonZero;
        for (i, layer) in root.layers.iter().enumerate() {
            let m = layer.mask.expect("clip mask");
            assert_eq!((m.format, m.inverted, m.density), (MaskFormat::CoverageU8, false, 1.0));
            let served = low.vector_mask(m.key, &vectors).unwrap();
            assert_eq!(served.sha256, m.sha256);
            // The plane covers the layer's extent and the clip bounds.
            let r = m.placement;
            assert!(r.x <= 2 && r.y <= 2 && r.x + i64::from(r.width) >= 37 && r.y + i64::from(r.height) >= 37, "layer {i}: {r:?}");
            let frame = PixelFrame { x: r.x as i32, y: r.y as i32, width: r.width, height: r.height };
            let (want, _) = rasterize(&diamond, frame, &RasterOptions::default()).unwrap();
            assert_eq!(served.bytes, &want.data[..], "layer {i}: nib plane");
        }
        // Without attachments nothing is cut (unchanged plans).
        let (_, plain) = lowered(&document(), &records(json!([])));
        let plain = plain.unwrap();
        let PlanNode::Group(proot) = &plain.plan.nodes[plain.plan.root.0 as usize] else { panic!() };
        assert!(proot.layers[0].mask.is_none());
        assert!(plain.clip_planes.is_empty());
        // Typed refusals: a clipped layer with its own mask; an attachment to an unknown clip.
        let mut masked = document();
        let mut m = tile(2, "m2", 0, 0, 48, 48);
        m["format"] = json!("coverage_u8");
        masked["layers"][1]["appearance"] = json!({"opacity":100.0,"fill_opacity":100.0,"blend_mode":2,"clipped":false,
            "isolation":"isolated","mask":{"tiles":[m],"enabled":true,"linked":true}});
        let (_, e) = lowered(&masked, &records(json!([{"layer_id":identity("SLYR",2),"clip_id":"fig-clip-0"}])));
        assert!(e.unwrap_err().to_string().contains("vector_clip_with_mask"));
        let (_, e) = lowered(&document(), &records(json!([{"layer_id":identity("SLYR",2),"clip_id":"nope"}])));
        assert!(e.unwrap_err().to_string().contains("vector_clip_unresolved"));
    });
}
