//! Lane SMB-COMPOSITE-DOCGRAPH-MASKS: vector clips, vector masks and clipped layers on the
//! DOCUMENT-graph frame path (`CompositionTemporalSource::lower_document_frame` with the render
//! inputs), driven by Folio records (`StudioVectorRecords` clips, masks and mask parameters,
//! `StudioCompositionRecords`). Proofs: the document-graph frame and the scene path of the same
//! composition render the same pixels under one reference executor (pigment's `composite_over`,
//! `coverage_math` feather and `mask_coverage`, the clip-to-base run rule; alpha bit-identical,
//! colour bit-identical except the scene path's surface rounding, `fixture::path_agreement`); the
//! document-graph
//! frame equals the folio-free fixture `support/docgraph_scenes.rs` structurally (plan, envelope,
//! owned planes), which render-cpu / render-gpu `tests/composition_docgraph_masks.rs` render
//! against the shared oracle.
#![cfg(feature = "motion")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::temporal::composition::{CompositionInput, CompositionTemporalSource, DocumentFrame};
use hsk_studio_composite::vector::OwnedPath;
use hsk_studio_composite::*;
use hsk_studio_folio::{Budget, Snapshot, StudioCompositionRecords, StudioDesignRecords, StudioImageSource, StudioVectorRecords, TileRef};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::render_plan::{AlphaConvention, MaskFormat, OutputTarget, PlanNode, PlanNodeId, SampleFormat};
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
    composition_records_with(|l| json!([{"property":"anchor","static_value":[0.0,0.0]},
        {"property":"position","static_value":[l.position[0] as f64,l.position[1] as f64]}]))
}
/// Composition 9 with the moved variant's transforms (`fixture::MOVES`, lane SMB-COMPOSITE-DOCGRAPH-2).
fn moved_records() -> StudioCompositionRecords {
    composition_records_with(|l| {
        let m = fixture::move_of(l.n);
        json!([{"property":"anchor","static_value":[0.0,0.0]},
            {"property":"position","static_value":[m.position[0],m.position[1]]},
            {"property":"rotation","static_value":[m.rotation]},
            {"property":"scale","static_value":[m.scale,m.scale]}])
    })
}
/// Composition 9 with `streams(layer)` as every layer's streams.
fn composition_records_with(streams: impl Fn(&fixture::LayerSpec) -> Value) -> StudioCompositionRecords {
    let layers: Vec<Value> = fixture::LAYERS.iter().rev().map(|l| json!({"layer_id":identity("SLYR",l.n),
        "timing":{"in_point":0,"out_point":48 * TICKS_PER_FRAME,"start_time":0,"stretch":{"numerator":1,"denominator":1}},
        "streams":streams(l)})).collect();
    serde_json::from_value(json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),
        "compositions":[{"schema_id":"hsk.studio.composition@1","composition_id":identity("SCOM",9),"name":"c","width":scenes::W,"height":scenes::H,
            "pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":24,"denominator":1},"duration":48 * TICKS_PER_FRAME,
            "layers":layers}]})).unwrap()
}
/// `recs` with mask `mask`'s parameters replaced by `params`.
fn with_params(mut recs: StudioVectorRecords, mask: &str, params: Value) -> StudioVectorRecords {
    recs.mask_parameters.retain(|p| p.mask_id != mask);
    recs.mask_parameters.push(serde_json::from_value(params).unwrap());
    recs
}
/// `recs` with the triangle clip `c-tri` also on layer `n`.
fn with_clip(mut recs: StudioVectorRecords, n: u8) -> StudioVectorRecords {
    recs.attachments.push(serde_json::from_value(json!({"layer_id":identity("SLYR",n),"clip_id":"c-tri"})).unwrap());
    recs
}
/// Document graph against scene path where the scene path carries an unlinked mask as a matte
/// (`matte_coverage` stores a matte as `coverage_u16le`, so a feathered mask is rounded to
/// 1/65535 there while the document graph reads it unrounded): alpha and premultiplied colour
/// within `MATTE_ROUNDING` plus the surface rounding. Returns the largest difference.
const MATTE_ROUNDING: f32 = 1.0 / 65535.0;
fn matte_agreement(doc: &[[f32; 4]], scene: &[[f32; 4]]) -> Result<f32, Vec<String>> {
    let worst = fixture::max_delta(doc, scene);
    if worst <= MATTE_ROUNDING + fixture::SURFACE_ROUNDING {
        return Ok(worst);
    }
    Err(doc.iter().zip(scene).enumerate().filter(|(_, (a, s))| fixture::max_delta(&[**a], &[**s]) > MATTE_ROUNDING + fixture::SURFACE_ROUNDING)
        .map(|(i, (a, s))| format!("({}, {}): document graph {a:?} vs scene path {s:?}", i % scenes::W as usize, i / scenes::W as usize)).collect())
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
        // Owned planes and track-matte planes (what `DocumentFrame::sources` serves).
        owned_masks: doc.owned_masks.clone().into_iter().chain(doc.mattes.iter().map(|(k, m)| (*k, OwnedMask {
            bounds: hsk_studio_pigment::Rect { x: 0, y: 0, width: m.width, height: m.height },
            sample_format: "coverage_u16le".into(),
            bytes: m.bytes.clone(),
            stride_bytes: u64::from(m.width) * 2,
            sha256: m.sha256,
        }))).collect(),
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
        // (1) Document graph == scene path under the reference executor (`fixture::path_agreement`:
        // alpha bit-identical; colour bit-identical except under a surfaced layer, within 2^-22).
        let doc_px = fixture::render_frame(&fixture::Ref, &got, &pixels).unwrap();
        let scene_px = fixture::render_scene_px(&fixture::Ref, &scene, &pixels);
        match fixture::path_agreement(&doc_px, &scene_px) {
            Ok(ulp) => println!("document graph vs scene path: {} of {} pixels identical, {ulp} within the surface rounding under a surfaced layer",
                doc_px.len() - ulp, doc_px.len()),
            Err(e) => failures.extend(e),
        }
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
                    println!("scene layer {} differs:\n got  {a:?}\n want {f:?}", a.layer_id);
                }
            }
            panic!("{}", failures.join("\n"));
        }
    });
}

/// Goal 2 (lane SMB-COMPOSITE-VECTOR-MASK-2 left the `CompositionTemporalSource` wiring of
/// unlinked masks without a Folio-records test): from Folio records, the scene path strips the
/// unlinked mask from layer 3's static subtree and carries it as a composition-space alpha matte
/// (identity matrix) while the layer moves; the document-graph path keeps the same plane in plan
/// space; density below 1 and a track matte on the same layer are refused typed. (A clip and an
/// unlinked mask on one moved layer: `clip_and_unlinked_mask_on_one_moved_layer`.)
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
        // Density below 1 on an unlinked mask: refused when the composition is read.
        let dense = with_params(vector_records(), "m-alpha", json!({"mask_id":"m-alpha","linked":false,"density":50.0}));
        let e = source_of(&build(&dense), &rec, &pixels).unwrap_err();
        assert!(e.to_string().contains(vector_mask::REFUSE_MASK_UNLINKED_DENSITY), "{e}");
        // An unlinked mask together with a track matte: refused when the frame is evaluated.
        let mut matted = composition_records();
        let l3r = matted.compositions[0].layers.iter_mut().find(|l| l.layer_id == identity("SLYR", 3)).unwrap();
        l3r.track_matte = Some(serde_json::from_value(json!({"source_layer_id":identity("SLYR",11),"mode":"alpha","inverted":false})).unwrap());
        let e = source_of(&b, &matted, &pixels).unwrap().scene_at(0, &cancel).unwrap_err();
        assert!(matches!(e, TemporalError::Unsupported { feature: vector_mask::REFUSE_MASK_UNLINKED_MATTE, .. }), "{e:?}");
    });
}

/// Stretch (DESCRIPTOR refusal only the document path had): a track matte on a layer that already
/// carries a coverage mask (here the vector clip of layer 2 and the feathered LUMINANCE mask of
/// layer 7, both matted by layer 11) was `composition_matte_with_mask` on the document graph while
/// the scene path rendered it. The layer's mask now moves into an isolated wrapper (its feather
/// with it) and the matte takes the slot; the frame equals the scene path under the reference
/// executor (`fixture::path_agreement`).
#[test]
fn track_mattes_on_masked_layers_on_the_document_graph_equal_the_scene_path() {
    bounded_case("track_mattes_on_masked_layers_on_the_document_graph_equal_the_scene_path", || {
        let pixels = Pixels::fixture();
        let cancel = CancellationToken::default();
        let mut records = vector_records();
        records.mask_parameters.retain(|p| p.mask_id != "m-lum");
        records.mask_parameters.push(serde_json::from_value(json!({"mask_id":"m-lum","reveal_outside":true,"feather":1.0})).unwrap());
        let b = build(&records);
        let mut rec = composition_records();
        for (n, mode, inverted) in [(2u8, "alpha", false), (7, "luma", true)] {
            let l = rec.compositions[0].layers.iter_mut().find(|l| l.layer_id == identity("SLYR", n)).unwrap();
            l.track_matte = Some(serde_json::from_value(json!({"source_layer_id":identity("SLYR",11),"mode":mode,"inverted":inverted})).unwrap());
        }
        let src = source_of(&b, &rec, &pixels).unwrap();
        let doc = doc_frame(&b, &src, &pixels).unwrap();
        assert_eq!(doc.mattes.len(), 2);
        let plan = &doc.frame.lowering.plan;
        // Each matted entry reads an isolated wrapper that carries the layer's own mask.
        let PlanNode::Group(root) = &plan.nodes[plan.root.0 as usize] else { panic!("root group") };
        let mut wrapped = 0;
        for (li, l) in root.layers.iter().enumerate() {
            let Some(m) = l.mask.filter(|m| doc.mattes.contains_key(&m.key)) else { continue };
            assert_eq!(m.placement, fixture::extent());
            let PlanNode::Group(w) = &plan.nodes[l.input.0 as usize] else { panic!("layer {li}: wrapper") };
            assert!(w.isolated && w.layers.len() == 1);
            let inner = w.layers[0].mask.expect("the layer's own mask");
            assert!(doc.owned_masks.contains_key(&inner.key));
            wrapped += 1;
        }
        assert_eq!(wrapped, 2);
        // The feather of layer 7's mask moved with it to the wrapper's layer.
        let feathers = &doc.frame.extensions.layers;
        assert!(feathers.iter().any(|e| e.mask_feather_px == 1.0 && e.layer == 0
            && matches!(&plan.nodes[e.group.0 as usize], PlanNode::Group(w) if w.layers.len() == 1)));
        let doc_px = fixture::render_frame(&fixture::Ref, &plain(&doc), &pixels).unwrap();
        let scene_px = fixture::render_scene_px(&fixture::Ref, &src.scene_at(0, &cancel).unwrap(), &pixels);
        match fixture::path_agreement(&doc_px, &scene_px) {
            Ok(ulp) => println!("matted masked layers: document graph vs scene path, {ulp} pixels within the surface rounding"),
            Err(e) => panic!("{}", e.join("\n")),
        }
        // The matte cuts something: the frame differs from the unmatted one.
        let unmatted_src = source_of(&b, &composition_records(), &pixels).unwrap();
        let unmatted = fixture::render_frame(&fixture::Ref, &plain(&doc_frame(&b, &unmatted_src, &pixels).unwrap()), &pixels).unwrap();
        assert_ne!(doc_px, unmatted);
    });
}

/// The root-group layer of fixture layer `n` (the root lists `fixture::LAYERS` in order).
fn root_layer(plan: &hsk_studio_pigment::render_plan::RenderPlan, n: u8) -> hsk_studio_pigment::render_plan::Layer {
    let PlanNode::Group(root) = &plan.nodes[plan.root.0 as usize] else { panic!("root group") };
    root.layers[fixture::LAYERS.iter().position(|l| l.n == n).unwrap()]
}
/// The frame pixel `(x, y)`.
fn px(frame: &[[f32; 4]], x: i64, y: i64) -> [f32; 4] {
    frame[(y * i64::from(scenes::W) + x) as usize]
}

/// Lane SMB-COMPOSITE-DOCGRAPH-2 goal 4 (`transform_unlinked_vector_mask_with_clip` was refused for
/// every moved layer): layer 3 carries the triangle clip AND its unlinked ALPHA mask and moves by
/// (22, 14). The document graph splits them as the scene path cuts them: the clip moves with the
/// content inside an isolated wrapper (`coverage_u8`, the nib plane of the triangle moved by the
/// layer's translation), the mask stays on the layer in plan space with its feather; the frame
/// renders like the scene path (exact without feather, within the scene matte's u16 rounding with
/// a 1.5 px feather), and the clip is visible.
#[test]
fn clip_and_unlinked_mask_on_one_moved_layer() {
    bounded_case("clip_and_unlinked_mask_on_one_moved_layer", || {
        let pixels = Pixels::fixture();
        let cancel = CancellationToken::default();
        let rec = composition_records();
        let unclipped = build(&vector_records());
        let unclipped_px = fixture::render_frame(&fixture::Ref, &plain(&doc_frame(&unclipped, &source_of(&unclipped, &rec, &pixels).unwrap(), &pixels).unwrap()), &pixels).unwrap();
        for feather in [0.0f32, 1.5] {
            let mut recs = with_clip(vector_records(), 3);
            if feather > 0.0 {
                recs = with_params(recs, "m-alpha", json!({"mask_id":"m-alpha","linked":false,"feather":feather}));
            }
            let b = build(&recs);
            let src = source_of(&b, &rec, &pixels).unwrap();
            let doc = doc_frame(&b, &src, &pixels).unwrap();
            let plan = &doc.frame.lowering.plan;
            let l = root_layer(plan, 3);
            let mask = l.mask.expect("the unlinked mask stays on the layer");
            assert_eq!(mask.format, MaskFormat::CoverageU16Le);
            let PlanNode::Group(w) = &plan.nodes[l.input.0 as usize] else { panic!("feather {feather}: wrapper") };
            assert!(w.isolated && w.layers.len() == 1 && w.layers[0].mode == StudioBlendMode::Normal);
            let clip = w.layers[0].mask.expect("the clip moves with the content");
            assert_eq!((clip.format, clip.inverted, clip.density), (MaskFormat::CoverageU8, false, 1.0));
            let PlanNode::Tile(t) = &plan.nodes[w.layers[0].input.0 as usize] else { panic!("the layer's tile") };
            assert_eq!(t.placement, fixture::moved_rect(fixture::spec(3)));
            // The clip plane: the triangle's nib plane over the static rect, moved by (22, 14).
            let cp = &doc.owned_masks[&clip.key];
            assert_eq!(cp.bounds, clip.placement);
            let unmoved = Rect { x: cp.bounds.x - 22, y: cp.bounds.y - 14, ..cp.bounds };
            assert!(unmoved.x <= 0 && unmoved.y <= 0 && unmoved.width >= 9 && unmoved.height >= 7, "{unmoved:?}");
            assert_eq!(cp.bytes, fixture::cut_plane(fixture::Cut::Clip, unmoved).plane.bytes, "feather {feather}: clip plane");
            // The mask plane: in place, the mask alone (no clip) over the frame and its feather.
            let mp = &doc.owned_masks[&mask.key];
            let alone = fixture::layer_plane(fixture::spec(3)).unwrap().plane;
            let plain_mask = vector_mask(fixture::Cut::AlphaUnlinked, &b);
            for y in mp.bounds.y..mp.bounds.y + i64::from(mp.bounds.height) {
                for x in mp.bounds.x..mp.bounds.x + i64::from(mp.bounds.width) {
                    assert_eq!(fixture::effective(mp, 0.0, false, 1.0, x, y), fixture::effective(&alone, 0.0, false, 1.0, x, y), "({x}, {y})");
                }
            }
            assert!(plain_mask.feather_px == feather);
            let entries = &doc.frame.extensions.layers;
            assert!(entries.iter().all(|e| e.group != PlanNodeId(l.input.0)), "nothing on the wrapper");
            let root = plan.root;
            let li = fixture::LAYERS.iter().position(|o| o.n == 3).unwrap() as u32;
            assert_eq!(entries.iter().find(|e| e.group == root && e.layer == li).map_or(0.0, |e| e.mask_feather_px), feather);
            // Pixels: the scene path, and the clip cuts.
            let doc_px = fixture::render_frame(&fixture::Ref, &plain(&doc), &pixels).unwrap();
            let scene_px = fixture::render_scene_px(&fixture::Ref, &src.scene_at(0, &cancel).unwrap(), &pixels);
            if feather == 0.0 {
                if let Err(e) = fixture::path_agreement(&doc_px, &scene_px) {
                    panic!("{}", e.join("\n"));
                }
            } else {
                let worst = matte_agreement(&doc_px, &scene_px).unwrap_or_else(|e| panic!("{}", e.join("\n")));
                println!("feather {feather}: document graph vs scene path, max |delta| {worst:e}");
            }
            let r = fixture::moved_rect(fixture::spec(3));
            let cut = (r.y..r.y + i64::from(r.height)).flat_map(|y| (r.x..r.x + i64::from(r.width)).map(move |x| (x, y)))
                .any(|(x, y)| px(&doc_px, x, y) != px(&unclipped_px, x, y));
            assert!(feather > 0.0 || cut, "the clip cuts layer 3");
        }
    });
}

/// The ALPHA unlinked mask of `b`'s layer 3 (`prepare_vectors` output).
fn vector_mask(_: fixture::Cut, b: &Built) -> VectorMask {
    b.vectors.mask(&identity("SLYR", 3)).unwrap().unwrap().clone()
}

/// Lane SMB-COMPOSITE-DOCGRAPH-2 goal 3: feathered unlinked masks render the same on the document
/// graph and on the scene path. Layer 3 moves to (25, 17), partly past its ALPHA mask's source
/// rect, with a 2 px feather; layer 7's LUMINANCE reveal_outside mask is unlinked with a 1 px
/// feather while the layer moves far from the mask art. Before this lane the scene path's matte
/// cut the feather at the source rect and was zero outside it (layer 7 vanished), and the document
/// graph clamped the feather taps at a plane border on mask content; both planes now hold the
/// whole feathered mask (`vector_mask::unlinked_rect`) and the reveal_outside matte covers the
/// composition. The paths agree within the scene matte's u16 rounding.
#[test]
fn feathered_unlinked_masks_match_the_scene_path() {
    bounded_case("feathered_unlinked_masks_match_the_scene_path", || {
        let pixels = Pixels::fixture();
        let cancel = CancellationToken::default();
        let rec = composition_records_with(|l| {
            let p = if l.n == 3 { [25.0, 17.0] } else { [l.position[0] as f64, l.position[1] as f64] };
            json!([{"property":"anchor","static_value":[0.0,0.0]},{"property":"position","static_value":p}])
        });
        let recs = with_params(
            with_params(vector_records(), "m-alpha", json!({"mask_id":"m-alpha","linked":false,"feather":2.0})),
            "m-lum",
            json!({"mask_id":"m-lum","reveal_outside":true,"linked":false,"feather":1.0}),
        );
        let b = build(&recs);
        let src = source_of(&b, &rec, &pixels).unwrap();
        let doc = doc_frame(&b, &src, &pixels).unwrap();
        let scene = src.scene_at(0, &cancel).unwrap();
        // Planes: layer 3's matte over the source rect grown by the feather support (3 px), layer
        // 7's matte over the composition; the document graph's planes over the frame and both.
        let support = hsk_studio_pigment::coverage_math::feather_support(2.0).unwrap() as i64;
        let m3 = b.vectors.mask(&identity("SLYR", 3)).unwrap().unwrap();
        let s3 = vector_mask::source_bounds(m3).unwrap().unwrap();
        let grown = Rect { x: s3.x - support, y: s3.y - support, width: s3.width + 2 * support as u32, height: s3.height + 2 * support as u32 };
        assert_eq!(vector_mask::unlinked_rect(m3), Ok(Some(grown)));
        let matte_plane = |n: u8| {
            let l = scene.layers.iter().find(|l| l.layer_id == identity("SLYR", n)).unwrap();
            let ml = l.matte.as_ref().unwrap().layer.as_ref().expect("matte layer");
            let SceneContent::Static(mc) = &ml.content else { panic!("static matte") };
            mc.owned_masks.values().next().unwrap().bounds
        };
        assert_eq!(matte_plane(3), grown);
        let comp = fixture::extent();
        let m7 = matte_plane(7);
        assert!(m7.x <= 0 && m7.y <= 0 && m7.x + i64::from(m7.width) >= i64::from(comp.width) && m7.y + i64::from(m7.height) >= i64::from(comp.height), "{m7:?}");
        let p3 = doc.owned_masks[&root_layer(&doc.frame.lowering.plan, 3).mask.unwrap().key].bounds;
        assert!(p3.x <= grown.x && p3.y <= grown.y && p3.x + i64::from(p3.width) >= grown.x + i64::from(grown.width)
            && p3.y + i64::from(p3.height) >= grown.y + i64::from(grown.height), "{p3:?}");
        // Pixels.
        let doc_px = fixture::render_frame(&fixture::Ref, &plain(&doc), &pixels).unwrap();
        let scene_px = fixture::render_scene_px(&fixture::Ref, &scene, &pixels);
        let worst = matte_agreement(&doc_px, &scene_px).unwrap_or_else(|e| panic!("{}", e.join("\n")));
        println!("feathered unlinked masks: document graph vs scene path, max |delta| {worst:e}");
        // Not vacuous: (29, 18) is past the source rect (x < 29) inside the feather; layer 7
        // shows its tile alpha far from the mask art on both paths.
        assert!(s3.x + i64::from(s3.width) <= 29);
        assert!(px(&doc_px, 29, 18)[3] > 0.0 && px(&scene_px, 29, 18)[3] > 0.0, "the feather spills past the source rect");
        let t7 = fixture::tile_of(&pixels, fixture::spec(7)).at(2, 1);
        assert_eq!(px(&doc_px, 26, 3)[3], t7[3], "reveal_outside: full outside the art");
        assert_eq!(px(&scene_px, 26, 3)[3], t7[3], "reveal_outside: full outside the art");
    });
}

/// Lane SMB-COMPOSITE-DOCGRAPH-2 goal 2 (the docgraph-masks lane left it NOT_PROVEN): fractional,
/// rotated and scaled moves of clipped and masked layers on the document graph, from Folio
/// records (`fixture::MOVES`). The frame is exactly the folio-free `fixture::moved_frame`: tiles
/// at the authored place with pigment transforms (bilinear), a vector clip and a linked
/// LUMINANCE mask resampled with their content on planes refitted to the tile rect, the unlinked
/// ALPHA mask in place, the Folio clipped layer rotated over its masked base. Under the reference
/// executor it equals `fixture::moved_oracle` bit for bit (render-cpu and render-gpu
/// `tests/composition_docgraph_masks.rs` execute the same frame). A fractional move of the
/// feathered linked mask's layer is refused typed by pigment (`PlanError::TransformCombination`).
#[test]
fn fractional_and_rotated_moves_of_clipped_and_masked_layers() {
    bounded_case("fractional_and_rotated_moves_of_clipped_and_masked_layers", || {
        let pixels = Pixels::fixture();
        let b = build(&vector_records());
        let src = source_of(&b, &moved_records(), &pixels).unwrap();
        let doc = doc_frame(&b, &src, &pixels).unwrap();
        let got = plain(&doc);
        let want = fixture::moved_frame(&pixels);
        let mut failures = Vec::new();
        for (what, ok) in [("plan", got.plan == want.plan), ("envelope", got.extensions == want.extensions),
            ("tiles", got.tiles == want.tiles), ("owned planes", got.owned_masks == want.owned_masks)] {
            if !ok {
                failures.push(format!("fixture {what} differs"));
            }
        }
        if !failures.is_empty() {
            println!("got  {:#?}\nwant {:#?}", got.plan, want.plan);
            println!("got  {:#?}\nwant {:#?}", got.extensions, want.extensions);
            for ((gk, g), (wk, w)) in got.owned_masks.iter().zip(&want.owned_masks) {
                println!("plane got {gk:?} {:?} {} | want {wk:?} {:?} {}", g.bounds, g.sample_format, w.bounds, w.sample_format);
            }
            panic!("{}", failures.join("\n"));
        }
        let doc_px = fixture::render_frame(&fixture::Ref, &got, &pixels).unwrap();
        let oracle = fixture::moved_oracle(&pixels);
        let off = doc_px.iter().zip(&oracle).filter(|(a, o)| a != o).count();
        assert_eq!(off, 0, "{off} pixels differ from the oracle, max |delta| {:e}", fixture::max_delta(&doc_px, &oracle));
        // Every resampled layer is sampled off the pixel grid (not a disguised integer move).
        let resampled = fixture::LAYERS.iter().filter(|l| fixture::layer_transform(l).is_some()).count();
        assert_eq!(resampled, 5);
        // The feathered linked mask's layer moved by a fraction: refused typed before rendering.
        let rec = composition_records_with(|l| {
            let p = if l.n == 10 { [12.5, 3.0] } else { [l.position[0] as f64, l.position[1] as f64] };
            json!([{"property":"anchor","static_value":[0.0,0.0]},{"property":"position","static_value":p}])
        });
        let e = doc_frame(&b, &source_of(&b, &rec, &pixels).unwrap(), &pixels).unwrap_err();
        assert!(matches!(e, TemporalError::Composite(CompositeError::Plan(hsk_studio_pigment::render_plan::PlanError::TransformCombination { .. }))), "{e:?}");
    });
}
