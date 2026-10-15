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
        // (1) Document graph == scene path, every pixel, under the reference executor.
        let doc_px = fixture::render_frame(&fixture::Ref, &got, &pixels).unwrap();
        let scene_px = fixture::render_scene_px(&fixture::Ref, &scene, &pixels);
        for (i, (a, s)) in doc_px.iter().zip(&scene_px).enumerate() {
            if a != s {
                failures.push(format!("doc != scene at ({}, {}): {a:?} vs {s:?}", i % scenes::W as usize, i / scenes::W as usize));
            }
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
            panic!("{}", failures.join("
"));
        }
    });
}
