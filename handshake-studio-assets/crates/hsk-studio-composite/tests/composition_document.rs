//! Lane SMB-COMPOSITE-COMPOSITION-2 slice 4: precompositions and track mattes on the DOCUMENT
//! graph path (`CompositionTemporalSource::lower_document_frame`). Proof: the document-graph frame
//! of a document whose graph mirrors the composition renders pixel-identical to the scene path
//! (`render_scene`, CPU/GPU-proven in the renderer tests) under one small reference executor
//! (`Ref`: tiles, solids, isolated groups, coverage masks, pigment `composite_over`), and the
//! nested surface equals the shared oracle of the nested composition.
#![cfg(feature = "motion")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::temporal::composition::{
    COMPOSITION_COLLAPSE_ON_DOCUMENT_GRAPH, CompositionInput, CompositionTemporalSource, PRECOMP_TILE_KEY,
};
use hsk_studio_composite::*;
use hsk_studio_folio::{Budget, StudioCompositionRecords, TileRef};
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, BackendError, BackendIdentity, DeviceClass, Exactness, ExecuteRequest, NoVector, Operator,
    OutputTarget, PlanExtensions, PlanNode, PlanSources, RenderBackend, RenderPlan, RenderReceipt, SampleFormat,
    SourceKey, TilePlanes, TileSink,
};
use hsk_studio_pigment::{Mask, Rect};
use serde_json::{Value, json};

mod common;
use common::{Available, address, bounded_case, edge, identity, layer, open, source, tile};
#[path = "support/composition_scenes.rs"]
mod scenes;
use scenes::{Pixels, TICKS_PER_FRAME};

/// Reference executor for plans without an envelope: per pixel, groups composite their layers
/// bottom-up over transparent with pigment's `composite_over`, a coverage mask multiplies opacity.
struct Ref;
fn f32_at(b: &[u8], i: usize) -> f32 {
    f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}
fn coverage(m: &Mask<'_>, x: i64, y: i64) -> f32 {
    let (cx, cy) = (x - m.bounds.x, y - m.bounds.y);
    if cx < 0 || cy < 0 || cx >= i64::from(m.bounds.width) || cy >= i64::from(m.bounds.height) {
        return 0.0;
    }
    let row = cy as usize * m.stride_bytes as usize;
    match m.sample_format {
        "coverage_u8" => (f64::from(m.bytes[row + cx as usize]) / 255.0) as f32,
        _ => {
            let i = row + cx as usize * 2;
            (f64::from(u16::from_le_bytes([m.bytes[i], m.bytes[i + 1]])) / 65535.0) as f32
        }
    }
}
fn eval(plan: &RenderPlan, id: usize, src: &dyn PlanSources, x: i64, y: i64) -> Rgba {
    match &plan.nodes[id] {
        PlanNode::Tile(t) => {
            let (cx, cy) = (x - t.placement.x, y - t.placement.y);
            if cx < 0 || cy < 0 || cx >= i64::from(t.placement.width) || cy >= i64::from(t.placement.height) {
                return Rgba::TRANSPARENT;
            }
            let p: TilePlanes<'_> = src.tile(t.key).expect("tile served");
            let c = cy as usize * p.colour_stride_bytes as usize + cx as usize * 12;
            let a = cy as usize * p.alpha_stride_bytes as usize + cx as usize * 4;
            Rgba::new([f32_at(p.colour, c), f32_at(p.colour, c + 4), f32_at(p.colour, c + 8)], f32_at(p.alpha, a))
        }
        PlanNode::Solid { rect, rgba } => {
            let inside = x >= rect.x && y >= rect.y && x < rect.x + i64::from(rect.width) && y < rect.y + i64::from(rect.height);
            if inside { Rgba::new([rgba[0], rgba[1], rgba[2]], rgba[3]) } else { Rgba::TRANSPARENT }
        }
        PlanNode::Group(g) => {
            let mut acc = Rgba::TRANSPARENT;
            for l in &g.layers {
                let s = eval(plan, l.input.0 as usize, src, x, y);
                let mut k = l.opacity * l.fill_opacity;
                if let Some(m) = l.mask {
                    let c = coverage(&src.mask(m.key).expect("mask served"), x, y);
                    k *= if m.inverted { 1.0 - c } else { c };
                }
                acc = composite_over(l.mode, acc, s, k, PixelSite { x, y, seed: 0 }).unwrap();
            }
            acc
        }
        PlanNode::Vector(v) => match *v {},
    }
}
impl RenderBackend for Ref {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity { name: "ref", version: "0", device: DeviceClass::Cpu, adapter: String::new(), generation: 0 }
    }
    fn exactness(&self, _: Operator) -> Exactness {
        Exactness::Exact
    }
    fn execute_extended(&self, plan: &RenderPlan<NoVector>, ext: &PlanExtensions, r: &ExecuteRequest,
        s: &dyn PlanSources, c: &CancellationToken, sink: &mut dyn TileSink) -> Result<RenderReceipt, BackendError> {
        assert!(ext.is_empty(), "the reference executor takes plain plans only");
        self.execute(plan, r, s, c, sink)
    }
    fn execute(&self, plan: &RenderPlan<NoVector>, _: &ExecuteRequest, src: &dyn PlanSources, _: &CancellationToken,
        sink: &mut dyn TileSink) -> Result<RenderReceipt, BackendError> {
        let e = plan.target.extent;
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in e.y..e.y + i64::from(e.height) {
            for x in e.x..e.x + i64::from(e.width) {
                let p = eval(plan, plan.root.0 as usize, src, x, y);
                for v in p.rgb {
                    colour.extend_from_slice(&v.to_le_bytes());
                }
                alpha.extend_from_slice(&p.a.to_le_bytes());
            }
        }
        sink.accept(e, &colour, &alpha).map_err(BackendError::Sink)?;
        Ok(RenderReceipt {
            plan_revision: plan.revision,
            plan_digest: plan.digest().unwrap(),
            backend: self.identity(),
            region: e,
            target: plan.target,
            exactness: Default::default(),
            chunks: 1,
            nodes_executed: plan.nodes.len() as u64,
            peak_working_bytes: 0,
        })
    }
}
struct Grid(Vec<[f32; 4]>, u32);
impl TileSink for Grid {
    fn accept(&mut self, r: Rect, c: &[u8], a: &[u8]) -> Result<(), hsk_studio_pigment::render_plan::SinkError> {
        for row in 0..r.height as usize {
            for col in 0..r.width as usize {
                let i = row * r.width as usize + col;
                let at = (r.y as usize + row) * self.1 as usize + r.x as usize + col;
                self.0[at] = [f32_at(c, i * 12), f32_at(c, i * 12 + 4), f32_at(c, i * 12 + 8), f32_at(a, i * 4)];
            }
        }
        Ok(())
    }
}

struct Info<'a>(&'a Pixels);
impl TileInfoSource for Info<'_> {
    fn tile_info(&self, t: &TileRef) -> Option<TileInfo> {
        let p = self.0.get(&format!("{}/{}", t.layer_id, t.object_key))?;
        Some(TileInfo { format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight,
            colour_sha256: p.colour_sha256, alpha_sha256: p.alpha_sha256 })
    }
}
/// The caller's sources of a document frame: raster tiles by Folio record.
struct Caller<'a> {
    pixels: &'a Pixels,
    table: &'a [(SourceKey, TileRef)],
}
impl PlanSources for Caller<'_> {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        let (_, t) = self.table.iter().find(|(k, _)| *k == key)?;
        self.pixels.tile(&format!("{}/{}", t.layer_id, t.object_key))
    }
    fn mask(&self, _: SourceKey) -> Option<Mask<'_>> {
        None
    }
}

fn f(frame: u64) -> u64 {
    frame * TICKS_PER_FRAME
}
fn timing(a: u64, b: u64) -> Value {
    json!({"in_point":f(a),"out_point":f(b),"start_time":0,"stretch":{"numerator":1,"denominator":1}})
}
fn composite_op(n: u8, key: &str) -> Value {
    json!({"layer_id":identity("SLYR",n),"operation_key":key,"operation_kind":"composite",
        "inputs":[{"port_key":"inputs","value_type":"image","cardinality":"many","semantic":"ordered_composite"}],
        "outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"composite_result"}]})
}
/// Group 1 composites `inputs` (layer numbers, bottom first); every layer listed in `layers`.
fn document(layers: Vec<Value>, ops: Vec<Value>, inputs: &[(u8, &str)]) -> Value {
    let edges: Vec<Value> = inputs.iter().enumerate()
        .map(|(i, (n, op))| edge(address(*n, op, "out"), address(1, "mix", "inputs"), i as u64)).collect();
    let mut all = vec![layer(1, "group", json!({"payload_kind":"group"}))];
    all.extend(layers);
    let mut operations = ops;
    operations.push(composite_op(1, "mix"));
    let mut v = json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"px",
        "colour_mode":3,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,
        "working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],"layers":all,
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":operations,"edges":edges,"outputs":[address(1, "mix", "out")]}
    });
    common::assign_all_profiles(&mut v);
    v
}
fn comp(id: u8, w: u32, h: u32, layers: Value) -> Value {
    json!({"schema_id":"hsk.studio.composition@1","composition_id":identity("SCOM",id),"name":"c","width":w,"height":h,
        "pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":24,"denominator":1},"duration":f(48),"layers":layers})
}
fn ctx<'a>(pixels: &'a Pixels) -> SceneContext<'a> {
    SceneContext {
        backend: &Ref,
        pixels,
        request: ExecuteRequest { expected_revision: 7, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: 1 << 28 },
        space: scenes::space(),
        limits: SceneLimits::default(),
    }
}
fn comp_source(snap: &hsk_studio_folio::Snapshot, rec: &StudioCompositionRecords, id: u8, pixels: &Pixels) -> CompositionTemporalSource {
    let cid = identity("SCOM", id);
    let input = CompositionInput {
        snapshot: snap, expected_revision: 7, records: rec, composition_id: &cid, budget: Budget::default(),
        resolver: &Available, blend_space: BlendSpace::Encoded, info: &Info(pixels), space: scenes::space(),
        inputs: RenderInputs::default(),
    };
    CompositionTemporalSource::new(&input, &CancellationToken::default()).unwrap()
}
/// Renders the document-graph frame and the scene of the same tick with `Ref`.
fn both(snap: &hsk_studio_folio::Snapshot, src: &CompositionTemporalSource, tick: i64, pixels: &Pixels)
    -> (Vec<[f32; 4]>, Vec<[f32; 4]>, hsk_studio_composite::temporal::composition::DocumentFrame) {
    let cancel = CancellationToken::default();
    let root = common::addr(1, "mix", "out");
    let timings = src.timings();
    let req = FrameRequest {
        snapshot: snap, expected_revision: 7, blend_space: BlendSpace::Encoded, limits: Limits::default(),
        budget: Budget::default(), resolver: &Available, root: &root,
        target: OutputTarget { extent: Rect { x: 0, y: 0, width: 32, height: 24 }, format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight, space: scenes::space() },
        info: &Info(pixels), timings: &timings,
    };
    let doc = src.lower_document_frame(&req, &RenderInputs::default(), tick, &ctx(pixels), &cancel).unwrap();
    let caller = Caller { pixels, table: &doc.frame.lowering.sources };
    let mut grid = Grid(vec![[0.0; 4]; 32 * 24], 32);
    Ref.execute_extended(&doc.frame.lowering.plan, &doc.frame.extensions, &ctx(pixels).request, &doc.sources(&caller), &cancel, &mut grid).unwrap();
    let scene = render_scene(&src.scene_at(tick, &cancel).unwrap(), &ctx(pixels), &cancel).unwrap();
    let px = (0..24u32).flat_map(|y| (0..32u32).map(move |x| (x, y))).map(|(x, y)| scene.pixel(x, y).unwrap()).collect();
    (grid.0, px, doc)
}

#[test]
fn precomposition_on_the_document_graph_equals_the_scene_path() {
    bounded_case("precomposition_on_the_document_graph_equals_the_scene_path", || {
        let pixels = Pixels::fixture();
        // Layer 8: a content-less group whose graph operation is a composite with no inputs.
        let v = document(
            vec![layer(8, "group", json!({"payload_kind":"group"})),
                 layer(9, "raster", json!({"payload_kind":"raster","tiles":[tile(9, "td", 0, 0, 6, 6)]}))],
            vec![composite_op(8, "nest")],
            &[(8, "nest")],
        );
        let snap = open(&v);
        let rec: StudioCompositionRecords = serde_json::from_value(json!({"schema_id":"hsk.studio.composition_records@1",
            "document_id":identity("SDOC",1),"compositions":[
                comp(3, 32, 24, json!([{"layer_id":identity("SLYR",8),
                    "timing":{"in_point":0,"out_point":f(48),"start_time":f(2),"stretch":{"numerator":2,"denominator":1}},
                    "precomp":{"composition_id":identity("SCOM",4),"collapse_transformations":false},
                    "streams":[{"property":"position","static_value":[16.0,12.0]}]}])),
                comp(4, 12, 10, json!([{"layer_id":identity("SLYR",9),"timing":timing(0, 12),"streams":[
                    {"property":"position","static_value":[0.0,0.0],"keyframes":[
                        {"time":0,"value":[3.0,3.0],"in_interp":"linear","out_interp":"linear"},
                        {"time":f(10),"value":[9.0,7.0],"in_interp":"linear","out_interp":"linear"}]}]}]))]})).unwrap();
        let src = comp_source(&snap, &rec, 3, &pixels);
        // Outer frame 22 = inner frame 10 (integer placements: the reference executor's domain).
        let (doc_px, scene_px, doc) = both(&snap, &src, f(22) as i64, &pixels);
        assert_eq!(doc_px, scene_px, "document graph == scene path");
        // The synthetic tile: nested size at (16 - 6, 12 - 5), served from the rendered surface,
        // which equals the oracle of the nested composition at inner frame 10.
        let (key, t) = doc.frame.lowering.sources.iter().find(|(_, t)| t.object_key == PRECOMP_TILE_KEY).unwrap();
        assert_eq!(t.layer_id, identity("SLYR", 8));
        let tile = doc.frame.lowering.plan.nodes.iter().find_map(|n| match n {
            PlanNode::Tile(t) if t.key == *key => Some(t.placement),
            _ => None,
        }).unwrap();
        assert_eq!(tile, Rect { x: 10, y: 7, width: 12, height: 10 });
        let surface = &doc.surfaces[key];
        let want = scenes::oracle(&scenes::inner_scene4(10, &pixels), &pixels);
        for (i, w) in want.iter().enumerate() {
            let got = [f32_at(&surface.colour, i * 12), f32_at(&surface.colour, i * 12 + 4), f32_at(&surface.colour, i * 12 + 8), f32_at(&surface.alpha, i * 4)];
            assert_eq!(got, *w, "nested pixel {i}");
        }
        // A collapsed precomposition is refused on this path (the scene path expands it).
        let mut rec2 = rec.clone();
        rec2.compositions[0].layers[0].precomp.as_mut().unwrap().collapse_transformations = true;
        let src2 = comp_source(&snap, &rec2, 3, &pixels);
        let root = common::addr(1, "mix", "out");
        let req = FrameRequest {
            snapshot: &snap, expected_revision: 7, blend_space: BlendSpace::Encoded, limits: Limits::default(),
            budget: Budget::default(), resolver: &Available, root: &root,
            target: OutputTarget { extent: Rect { x: 0, y: 0, width: 32, height: 24 }, format: SampleFormat::RgbF32Le,
                alpha: AlphaConvention::Straight, space: scenes::space() },
            info: &Info(&pixels), timings: &[],
        };
        let e = src2.lower_document_frame(&req, &RenderInputs::default(), 0, &ctx(&pixels), &CancellationToken::default()).unwrap_err();
        assert!(matches!(e, TemporalError::Unsupported { feature: COMPOSITION_COLLAPSE_ON_DOCUMENT_GRAPH, .. }), "{e:?}");
    });
}

#[test]
fn track_mattes_on_the_document_graph_equal_the_scene_path() {
    bounded_case("track_mattes_on_the_document_graph_equal_the_scene_path", || {
        let pixels = Pixels::fixture();
        let v = document(
            vec![layer(10, "raster", json!({"payload_kind":"raster","tiles":[tile(10, "te", 0, 0, 10, 8)]})),
                 layer(11, "raster", json!({"payload_kind":"raster","tiles":[tile(11, "tf", 0, 0, 20, 14)]})),
                 layer(12, "raster", json!({"payload_kind":"raster","tiles":[tile(12, "tg", 0, 0, 8, 8)]}))],
            vec![source(10, "image"), source(11, "image"), source(12, "image")],
            &[(10, "source"), (11, "source"), (12, "source")],
        );
        let snap = open(&v);
        let rec: StudioCompositionRecords = serde_json::from_value(json!({"schema_id":"hsk.studio.composition_records@1",
            "document_id":identity("SDOC",1),"compositions":[comp(5, 32, 24, json!([
                {"layer_id":identity("SLYR",12),"timing":timing(0, 48),
                 "track_matte":{"source_layer_id":identity("SLYR",10),"mode":"luma","inverted":true},
                 "streams":[{"property":"position","static_value":[6.0,6.0]}]},
                {"layer_id":identity("SLYR",11),"timing":timing(0, 48),
                 "track_matte":{"source_layer_id":identity("SLYR",10),"mode":"alpha","inverted":false},
                 "streams":[{"property":"position","static_value":[16.0,12.0]}]},
                {"layer_id":identity("SLYR",10),"timing":timing(0, 8),"streams":[{"property":"position","static_value":[8.0,12.0]}]}]))]}))
            .unwrap();
        let src = comp_source(&snap, &rec, 5, &pixels);
        let cancel = CancellationToken::default();
        for frame in [0u64, 10] {
            let (doc_px, scene_px, doc) = both(&snap, &src, f(frame) as i64, &pixels);
            assert_eq!(doc_px, scene_px, "frame {frame}: document graph == scene path");
            // Two matte planes (one per matted layer); the matte source renders nothing itself.
            assert_eq!(doc.mattes.len(), 2, "frame {frame}");
            assert!(doc.frame.hidden.contains(&identity("SLYR", 10)), "frame {frame}");
        }
        // Plain lower_frame (no renderer) still refuses matted layers and matte sources.
        let e = TemporalSource::layer_state(&src, &identity("SLYR", 11), 0, &cancel).unwrap_err();
        assert!(matches!(e, TemporalError::Unsupported { .. }), "{e:?}");
    });
}
