//! Lane SMB-COMPOSITE-CLIP-RECORDS item 2: a composition with a TEXT layer. The text layer (Type
//! glyph coverage prepared by `prepare_text`) takes the static path: its subtree is exactly the
//! standalone text plan (`text_layer_render_plan`, renderer-proven in render-cpu / render-gpu
//! `tests/text_render.rs`), the coverage plane is owned by the scene, and the composition animates
//! it like any layer (rotation stream, opacity stream, window).
#![cfg(feature = "motion")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::temporal::composition::{CompositionInput, CompositionTemporalSource};
use hsk_studio_composite::*;
use hsk_studio_folio::{Budget, StudioCompositionRecords, TileRef};
use hsk_studio_pigment::render_plan::ColourTag;
use hsk_studio_pigment::transform_math::Affine2;
use hsk_studio_type::FontBytes;
use serde_json::{Value, json};

mod common;
use common::*;
#[path = "common/text_shaping.rs"]
mod shaping;
use shaping::{FontFile, Seg, shape};

const FILL: [f32; 4] = [0.9, 0.2, 0.3, 1.0];
const TPF: u64 = 10_584_000_000;

fn info(_: &TileRef) -> Option<TileInfo> {
    None
}
/// Text layer 5 as a standalone text source (`mixed()`), document unit px.
fn document() -> Value {
    let mut v = mixed();
    assign_all_profiles(&mut v);
    v["geometry_unit"] = json!("px");
    v
}
fn prepared() -> PreparedText {
    let files = [FontFile::inter()];
    let segs = vec![Seg { start: 0, end: 2, font: 0, script: *b"Latn", language: "en", axes: vec![] }];
    let res = TextResolution::at_ppi(144.0);
    shape(&files, "Hi", &segs, false, 12.0, |p| {
        let mut x = 4.25;
        let mut runs = Vec::new();
        for run in p.paragraphs()[0].runs() {
            let file = files.iter().find(|f| f.identity == run.resolved().identity).unwrap();
            runs.push(TextRun { run, font: FontBytes { bytes: &file.bytes, resource: run.resolved() }, origin_px: [x, 20.0] });
            x += run.glyphs().iter().map(|g| g.x_advance_pt).sum::<f64>() * res.px_per_pt;
        }
        let id = identity("SLYR", 5);
        prepare_text(&[TextLayerInput { layer_id: &id, runs: &runs, fill_rgba: FILL }], &res, &CancellationToken::default())
    })
    .unwrap()
}
fn records() -> StudioCompositionRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),
        "compositions":[{"schema_id":"hsk.studio.composition@1","composition_id":identity("SCOM",9),"name":"title",
            "width":64,"height":48,"pixel_aspect":{"numerator":1,"denominator":1},
            "frame_rate":{"numerator":24,"denominator":1},"duration":TPF * 48,
            "layers":[{"layer_id":identity("SLYR",5),
                "timing":{"in_point":TPF * 2,"out_point":TPF * 40,"start_time":0,"stretch":{"numerator":1,"denominator":1}},
                "streams":[
                    {"property":"rotation","static_value":[0.0],"keyframes":[
                        {"time":0,"value":[0.0],"in_interp":"linear","out_interp":"linear"},
                        {"time":TPF * 10,"value":[20.0],"in_interp":"linear","out_interp":"linear"}]},
                    {"property":"opacity","static_value":[60.0]}]}]}]}))
    .unwrap()
}

#[test]
fn text_layer_plays_in_a_composition_through_the_static_path() {
    bounded_case("text_layer_plays_in_a_composition_through_the_static_path", || {
        let text = prepared();
        let plane = text.plane(&identity("SLYR", 5)).unwrap().clone();
        let rect = plane.placement.unwrap();
        let snap = open(&document());
        let rec = records();
        let id = identity("SCOM", 9);
        let space = ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded };
        let make = |inputs: RenderInputs<'_>| {
            CompositionTemporalSource::new(&CompositionInput {
                snapshot: &snap, expected_revision: 7, records: &rec, composition_id: &id, budget: Budget::default(),
                resolver: &Available, blend_space: BlendSpace::Encoded, info: &info, space, inputs,
            }, &CancellationToken::default())
        };
        // Without prepared text: the static path's typed refusal.
        let e = make(RenderInputs::default()).unwrap_err();
        assert!(matches!(&e, TemporalError::Composite(CompositeError::Unavailable(t)) if t.ends_with("text_to_image_conversion")), "{e:?}");

        let src = make(RenderInputs { records: None, text: Some(&text), vectors: None }).unwrap();
        let cancel = CancellationToken::default();
        assert!(src.scene_at(0, &cancel).unwrap().layers.is_empty(), "before its window");
        let scene = src.scene_at((TPF * 5) as i64, &cancel).unwrap();
        let [l] = scene.layers.as_slice() else { panic!("one layer") };
        assert!((l.opacity - 0.6).abs() < 1e-12);
        let SceneContent::Static(c) = &l.content else { panic!("static text: {:?}", l.content) };
        // The subtree is the standalone text plan (renderer-proven), the plane owned by the scene.
        assert_eq!(c.rect, rect);
        let alone = text_layer_render_plan(&text, &identity("SLYR", 5), 7, c.plan.target).unwrap();
        assert_eq!(c.plan.nodes, alone.lowering.plan.nodes);
        let owned: Vec<_> = c.owned_masks.values().collect();
        assert_eq!(owned.len(), 1);
        assert_eq!((owned[0].sha256, &owned[0].bytes, owned[0].bounds), (plane.sha256, &plane.coverage, rect));
        // Layer space = the text plane: rotation 10 deg about its centre at frame 5.
        let (cx, cy) = (rect.x as f64 + f64::from(rect.width) / 2.0, rect.y as f64 + f64::from(rect.height) / 2.0);
        let want = Affine2::from_parts(cx, cy, 1.0, 1.0, 10.0, 0.0, 0.0);
        for (a, b) in l.matrix.m.iter().zip(want.m) {
            assert!((a - b).abs() < 1e-9, "{:?} vs {:?}", l.matrix.m, want.m);
        }
        // Lowering places the rendered text surface with the declared bilinear filter.
        assert_eq!(scene.filter, hsk_studio_pigment::transform_math::ResampleFilter::Bilinear);
    });
}
