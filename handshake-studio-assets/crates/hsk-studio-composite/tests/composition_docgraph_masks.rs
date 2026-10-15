//! Lane SMB-COMPOSITE-DOCGRAPH-MASKS: vector clips, vector masks and clipped layers on the
//! DOCUMENT-graph frame path (`CompositionTemporalSource::lower_document_frame` with the render
//! inputs), driven by Folio records (`StudioVectorRecords` clips, masks and mask parameters,
//! `StudioCompositionRecords`). Proofs: the document-graph frame and the scene path of the same
//! composition render pixel-identical under one reference executor (pigment's `composite_over`,
//! `coverage_math` feather and `mask_coverage`, the clip-to-base run rule); the document-graph
//! frame equals the folio-free fixture `support/docgraph_scenes.rs` structurally (plan, envelope,
//! owned planes), which render-cpu / render-gpu `tests/composition_docgraph_masks.rs` render
//! against the shared oracle.
#![cfg(feature = "motion")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::temporal::composition::{CompositionInput, CompositionTemporalSource, DocumentFrame};
use hsk_studio_composite::vector::OwnedPath;
use hsk_studio_composite::*;
use hsk_studio_folio::{Budget, Snapshot, StudioCompositionRecords, StudioDesignRecords, StudioImageSource, StudioVectorRecords, TileRef};
use hsk_studio_pigment::render_plan::{AlphaConvention, OutputTarget, SampleFormat};
use serde_json::{Value, json};

mod common;
use common::{Available, address, bounded_case, edge, identity, layer, open, source, tile};
#[path = "support/docgraph_scenes.rs"]
mod fixture;
use fixture::scenes::{self, Pixels, TICKS_PER_FRAME};

struct Info<'a>(&'a Pixels);
impl TileInfoSource for Info<'_> {
    fn tile_info(&self, t: &TileRef) -> Option<TileInfo> {
        let p = self.0.get(&format!("{}/{}", t.layer_id, t.object_key))?;
        Some(TileInfo { format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight,
            colour_sha256: p.colour_sha256, alpha_sha256: p.alpha_sha256 })
    }
}
struct Assets;
impl VectorAssets for Assets {
    fn path(&self, id: &str) -> Option<OwnedPath> {
        fixture::SOURCES.iter().find(|(n, _)| identity("SVPT", *n) == id).map(|(n, _)| fixture::source_path(*n))
    }
    fn image(&self, _: &StudioImageSource) -> Option<ImageInfo> {
        None
    }
}

fn raster(n: u8, key: &str, w: u32, h: u32, clipped: bool) -> Value {
    let mut l = layer(n, "raster", json!({"payload_kind":"raster","tiles":[tile(n, key, 0, 0, w, h)]}));
    if clipped {
        l["appearance"] = json!({"opacity":100.0,"fill_opacity":100.0,"blend_mode":2,"clipped":true,"isolation":"isolated"});
    }
    l
}
/// Group 1 composites the raster layers of `fixture::LAYERS` (bottom first); the hidden vector
/// layers of `fixture::SOURCES` are the clip and mask sources (no graph operation).
fn document() -> Value {
    let mut layers = vec![layer(1, "group", json!({"payload_kind":"group"}))];
    let mut ops = Vec::new();
    let mut edges = Vec::new();
    for (i, l) in fixture::LAYERS.iter().enumerate() {
        layers.push(raster(l.n, l.key, l.w, l.h, l.clipped));
        ops.push(source(l.n, "image"));
        edges.push(edge(address(l.n, "source", "out"), address(1, "mix", "inputs"), i as u64));
    }
    for (n, _) in fixture::SOURCES {
        let mut v = layer(n, "vector", json!({"payload_kind":"vector","path_ids":[identity("SVPT",n)]}));
        v["visible"] = json!(false);
        layers.push(v);
    }
    ops.push(common::composite(1));
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"px",
        "colour_mode":3,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,
        "working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],"layers":layers,
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":ops,"edges":edges,"outputs":[address(1, "mix", "out")]}
    });
    common::assign_all_profiles(&mut v);
    v
}
fn design() -> StudioDesignRecords {
    let style = |n: u8, rgba: [f32; 4]| json!({"schema_id":"hsk.studio.style_registry@1","style_id":identity("SSTY",n),"kind":"PAINT",
        "name":format!("f{n}"),"publishing":{"description":"","description_markdown":"","documentation_links":[],"publish_key":format!("f{n}"),"is_remote":false},
        "rows":[{"row_kind":"fill","paint":{"paint_kind":"solid","colour":{"r":rgba[0],"g":rgba[1],"b":rgba[2],"colour_profile_id":null}},
            "opacity":f64::from(rgba[3]) * 100.0,"blend_mode":2,"visible":true}]});
    let nodes: Vec<Value> = fixture::SOURCES.iter().map(|(n, _)| json!({"node":{"node_kind":"layer","node_id":identity("SLYR",*n)},
        "style_refs":{"fill_style_id":identity("SSTY",*n)}})).collect();
    let styles: Vec<Value> = fixture::SOURCES.iter().map(|(n, c)| style(*n, *c)).collect();
    serde_json::from_value(json!({"schema_id":"hsk.studio.design_records@1","document_id":identity("SDOC",1),
        "nodes":nodes,"paint_styles":styles})).unwrap()
}
/// Clips, masks and parameters as `fixture::LAYERS` describes them.
fn vector_records() -> StudioVectorRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),
        "clips":[{"clip_id":"c-tri","source_layer_id":identity("SLYR",fixture::CLIP_SOURCE),"clip_rule":"NONZERO"}],
        "masks":[
            {"mask_id":"m-vec","source_layer_id":identity("SLYR",fixture::VECTOR_SOURCE),"mode":"VECTOR","invert_mask":true},
            {"mask_id":"m-alpha","source_layer_id":identity("SLYR",fixture::ALPHA_SOURCE),"mode":"ALPHA"},
            {"mask_id":"m-lum","source_layer_id":identity("SLYR",fixture::LUMA_SOURCE),"mode":"LUMINANCE"}],
        "mask_parameters":[
            {"mask_id":"m-vec","density":60.0,"feather":1.5},
            {"mask_id":"m-alpha","linked":false},
            {"mask_id":"m-lum","reveal_outside":true}],
        "attachments":[
            {"layer_id":identity("SLYR",fixture::VECTOR_MASKED),"mask_id":"m-vec"},
            {"layer_id":identity("SLYR",fixture::CLIPPED_BY_VECTOR),"clip_id":"c-tri"},
            {"layer_id":identity("SLYR",fixture::UNLINKED_MASKED),"mask_id":"m-alpha"},
            {"layer_id":identity("SLYR",fixture::LUMA_MASKED),"mask_id":"m-lum"}]})).unwrap()
}
/// Composition 9 (32x24, no background): every layer of `fixture::LAYERS` placed at its position
/// with anchor (0, 0), so its content moves by `position - content origin`.
fn composition_records() -> StudioCompositionRecords {
    let layers: Vec<Value> = fixture::LAYERS.iter().rev().map(|l| json!({"layer_id":identity("SLYR",l.n),
        "timing":{"in_point":0,"out_point":48 * TICKS_PER_FRAME,"start_time":0,"stretch":{"numerator":1,"denominator":1}},
        "streams":[{"property":"anchor","static_value":[0.0,0.0]},
                   {"property":"position","static_value":[l.position[0] as f64,l.position[1] as f64]}]})).collect();
    serde_json::from_value(json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),
        "compositions":[{"schema_id":"hsk.studio.composition@1","composition_id":identity("SCOM",9),"name":"c","width":scenes::W,"height":scenes::H,
            "pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":24,"denominator":1},"duration":48 * TICKS_PER_FRAME,
            "layers":layers}]})).unwrap()
}

struct Built {
    snap: Snapshot,
    vectors: PreparedVectors,
}
fn build(records: &StudioVectorRecords) -> Built {
    let snap = open(&document());
    let res = VectorResolution { coverage: VectorCoverage::Exact, ..VectorResolution::default() };
    let vectors = prepare_vectors(&snap, &design(), records, &Assets, BlendSpace::Encoded, &res, &CancellationToken::default()).unwrap();
    Built { snap, vectors }
}
fn source_of(b: &Built, rec: &StudioCompositionRecords, pixels: &Pixels) -> Result<CompositionTemporalSource, TemporalError> {
    let cid = identity("SCOM", 9);
    let input = CompositionInput {
        snapshot: &b.snap, expected_revision: 7, records: rec, composition_id: &cid, budget: Budget::default(),
        resolver: &Available, blend_space: BlendSpace::Encoded, info: &Info(pixels), space: scenes::space(),
        inputs: RenderInputs { vectors: Some(&b.vectors), ..RenderInputs::default() },
    };
    CompositionTemporalSource::new(&input, &CancellationToken::default())
}
fn doc_frame(b: &Built, src: &CompositionTemporalSource, pixels: &Pixels) -> Result<DocumentFrame, TemporalError> {
    let root = common::addr(1, "mix", "out");
    let timings = src.timings();
    let req = FrameRequest {
        snapshot: &b.snap, expected_revision: 7, blend_space: BlendSpace::Encoded, limits: Limits::default(),
        budget: Budget::default(), resolver: &Available, root: &root,
        target: OutputTarget { extent: fixture::extent(), format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight, space: scenes::space() },
        info: &Info(pixels), timings: &timings,
    };
    let inputs = RenderInputs { vectors: Some(&b.vectors), ..RenderInputs::default() };
    src.lower_document_frame(&req, &inputs, 0, &fixture::context(&fixture::Ref, pixels), &CancellationToken::default())
}

/// The document frame without Folio types (plan keys -> fixture tile ids).
fn plain(doc: &DocumentFrame) -> fixture::DocFrame {
    fixture::DocFrame {
        plan: doc.frame.lowering.plan.clone(),
        extensions: doc.frame.extensions.clone(),
        tiles: doc.frame.lowering.sources.iter().map(|(k, t)| (*k, format!("{}/{}", t.layer_id, t.object_key))).collect(),
        owned_masks: doc.owned_masks.clone(),
    }
}

#[test]
fn document_graph_frames_with_vector_clips_masks_and_clipped_layers_equal_the_scene_path() {
    bounded_case("document_graph_frames_with_vector_clips_masks_and_clipped_layers_equal_the_scene_path", || {
        let pixels = Pixels::fixture();
        let b = build(&vector_records());
        let rec = composition_records();
        let src = source_of(&b, &rec, &pixels).unwrap();
        let doc = doc_frame(&b, &src, &pixels).unwrap();
        let cancel = CancellationToken::default();
        let scene = src.scene_at(0, &cancel).unwrap();
        let got = plain(&doc);
        let want = fixture::document_frame(&pixels);
        let mut failures = Vec::new();
        // (1) Document graph == scene path under the reference executor: every alpha and every
        // colour bit-identical, except colour inside the clip run (layer 12 over the masked base
        // 10) by at most 1 ULP: the scene path draws the masked base into an intermediate
        // straight-alpha surface first (`SceneContent::Static`), whose colour is recovered by a
        // division, while the document graph clips to the base tile directly.
        let doc_px = fixture::render_frame(&fixture::Ref, &got, &pixels).unwrap();
        let scene_px = fixture::render_scene_px(&fixture::Ref, &scene, &pixels);
        let run = fixture::moved_rect(fixture::spec(12));
        let mut ulp_pixels = 0;
        for (i, (a, s)) in doc_px.iter().zip(&scene_px).enumerate() {
            if a == s {
                continue;
            }
            let (x, y) = ((i % scenes::W as usize) as i64, (i / scenes::W as usize) as i64);
            let in_run = x >= run.x && y >= run.y && x < run.x + i64::from(run.width) && y < run.y + i64::from(run.height);
            let ulp = a[3] == s[3] && (0..3).all(|c| a[c].to_bits().abs_diff(s[c].to_bits()) <= 1);
            if in_run && ulp {
                ulp_pixels += 1;
            } else {
                failures.push(format!("doc != scene at ({x}, {y}): {a:?} vs {s:?}"));
            }
        }
        println!("document graph vs scene path: {} of {} pixels identical, {ulp_pixels} within 1 ULP of colour in the clip run",
            doc_px.len() - ulp_pixels, doc_px.len());
        // Every cut is visible in the frame (no silent no-op): the cut layers differ from the same
        // frame without the vector records.
        let uncut = build(&serde_json::from_value(json!({"schema_id":"hsk.studio.vector_records@1",
            "document_id":identity("SDOC",1)})).unwrap());
        let plain_src = source_of(&uncut, &rec, &pixels).unwrap();
        let plain_px = fixture::render_frame(&fixture::Ref, &plain(&doc_frame(&uncut, &plain_src, &pixels).unwrap()), &pixels).unwrap();
        for l in fixture::LAYERS.iter().filter(|l| l.cut.is_some()) {
            let r = fixture::moved_rect(l);
            let changed = (r.y..r.y + i64::from(r.height)).flat_map(|y| (r.x..r.x + i64::from(r.width)).map(move |x| (x, y)))
                .any(|(x, y)| { let i = (y * i64::from(scenes::W) + x) as usize; doc_px[i] != plain_px[i] });
            if !changed {
                failures.push(format!("layer {} is not cut on the document graph", l.n));
            }
        }
        // (2) The frame is the folio-free fixture (renderer tests render that one).
        for (what, ok) in [("plan", got.plan == want.plan), ("envelope", got.extensions == want.extensions),
            ("tiles", got.tiles == want.tiles), ("owned planes", got.owned_masks == want.owned_masks)] {
            if !ok {
                failures.push(format!("fixture {what} differs"));
            }
        }
        if scene != fixture::scene(&pixels) {
            failures.push("fixture scene differs".into());
        }
        // (3) The oracle (independent of the plan) agrees bit for bit.
        let oracle = fixture::oracle(&pixels);
        let off = doc_px.iter().zip(&oracle).filter(|(a, o)| a != o).count();
        if off != 0 {
            failures.push(format!("{off} pixels differ from the oracle, max |delta| {:e}", fixture::max_delta(&doc_px, &oracle)));
        }
        if !failures.is_empty() {
            println!("{:#?}", got.plan);
            println!("{:#?}", got.extensions);
            println!("{:?}", got.tiles);
            for (k, m) in &got.owned_masks { println!("got {k:?} {:?} {} {}", m.bounds, m.sample_format, scenes::hex(&m.sha256)); }
            for l in &fixture::LAYERS {
                if let Some(c) = fixture::layer_plane(l) {
                    println!("fixture {} {:?} {} {} inv {} dens {} feather {}", l.n, c.plane.bounds, c.plane.sample_format, scenes::hex(&c.plane.sha256), c.input.inverted, c.input.density, c.feather_px);
                }
                println!("{} {:?}", l.n, TemporalSource::layer_matrix(&src, &identity("SLYR", l.n), 0, &cancel));
            }
            for s in &scene.layers {
                let what = match &s.content {
                    SceneContent::Static(c) => format!("static rect {:?} nodes {:?} ext {:?}", c.rect, c.plan.nodes, c.extensions),
                    other => format!("{other:?}"),
                };
                println!("scene {} m {:?} clipped {} matte {} {what}", s.layer_id, s.matrix.m, s.clipped, s.matte.is_some());
            }
            let fs = fixture::scene(&pixels);
            for (a, f) in scene.layers.iter().zip(&fs.layers) {
                if a != f {
                    println!("scene layer {} differs:
 got  {a:?}
 want {f:?}", a.layer_id);
                }
            }
            panic!("{}", failures.join("
"));
        }
    });
}

/// Goal 2 (lane SMB-COMPOSITE-VECTOR-MASK-2 left the `CompositionTemporalSource` wiring of
/// unlinked masks without a Folio-records test): from Folio records, the scene path strips the
/// unlinked mask from layer 3's static subtree and carries it as a composition-space alpha matte
/// (identity matrix) while the layer moves; the document-graph path keeps the same plane in plan
/// space; density below 1 and a track matte on the same layer are refused typed; a moved layer
/// whose clip and unlinked mask share one plane is refused on the document graph.
#[test]
fn unlinked_masks_in_compositions_from_folio_records() {
    bounded_case("unlinked_masks_in_compositions_from_folio_records", || {
        let pixels = Pixels::fixture();
        let cancel = CancellationToken::default();
        let rec = composition_records();
        let b = build(&vector_records());
        let src = source_of(&b, &rec, &pixels).unwrap();
        let scene = src.scene_at(0, &cancel).unwrap();
        let l3 = scene.layers.iter().find(|l| l.layer_id == identity("SLYR", 3)).unwrap();
        assert_eq!(l3.matrix.m, [1.0, 0.0, 22.0, 0.0, 1.0, 14.0], "the layer moves");
        let SceneContent::Static(content) = &l3.content else { panic!("static subtree") };
        assert!(content.owned_masks.is_empty(), "the unlinked mask left the subtree");
        let matte = l3.matte.as_ref().expect("unlinked mask matte");
        assert_eq!((matte.mode, matte.inverted), (SceneMatteMode::Alpha, false));
        let ml = matte.layer.as_ref().expect("matte layer");
        assert_eq!(ml.matrix, hsk_studio_pigment::transform_math::Affine2::IDENTITY, "the mask stays");
        let SceneContent::Static(mc) = &ml.content else { panic!("static matte") };
        // The matte's plane is the document plane of the mask (same values where both exist).
        let fixture_plane = fixture::layer_plane(fixture::spec(3)).unwrap().plane;
        let m = mc.owned_masks.values().next().unwrap();
        for y in m.bounds.y..m.bounds.y + i64::from(m.bounds.height) {
            for x in m.bounds.x..m.bounds.x + i64::from(m.bounds.width) {
                assert_eq!(fixture::effective(m, 0.0, false, 1.0, x, y), fixture::effective(&fixture_plane, 0.0, false, 1.0, x, y), "({x}, {y})");
            }
        }
        let linked = |recs: &mut StudioVectorRecords, mask: &str, params: Value| {
            recs.mask_parameters.retain(|p| p.mask_id != mask);
            recs.mask_parameters.push(serde_json::from_value(params).unwrap());
        };
        // Density below 1 on an unlinked mask: refused when the composition is read.
        let mut dense = vector_records();
        linked(&mut dense, "m-alpha", json!({"mask_id":"m-alpha","linked":false,"density":50.0}));
        let e = source_of(&build(&dense), &rec, &pixels).unwrap_err();
        assert!(e.to_string().contains(vector_mask::REFUSE_MASK_UNLINKED_DENSITY), "{e}");
        // An unlinked mask together with a track matte: refused when the frame is evaluated.
        let mut matted = composition_records();
        let l3r = matted.compositions[0].layers.iter_mut().find(|l| l.layer_id == identity("SLYR", 3)).unwrap();
        l3r.track_matte = Some(serde_json::from_value(json!({"source_layer_id":identity("SLYR",11),"mode":"alpha","inverted":false})).unwrap());
        let e = source_of(&b, &matted, &pixels).unwrap().scene_at(0, &cancel).unwrap_err();
        assert!(matches!(e, TemporalError::Unsupported { feature: vector_mask::REFUSE_MASK_UNLINKED_MATTE, .. }), "{e:?}");
        // A moved layer with a clip AND an unlinked mask (one plane): refused on the document graph.
        let mut split = vector_records();
        split.attachments.push(serde_json::from_value(json!({"layer_id":identity("SLYR",3),"clip_id":"c-tri"})).unwrap());
        let bs = build(&split);
        let srcs = source_of(&bs, &rec, &pixels).unwrap();
        let e = doc_frame(&bs, &srcs, &pixels).unwrap_err();
        assert!(matches!(e, TemporalError::Unsupported { feature: temporal::TRANSFORM_UNLINKED_MASK_WITH_CLIP, .. }), "{e:?}");
    });
}
