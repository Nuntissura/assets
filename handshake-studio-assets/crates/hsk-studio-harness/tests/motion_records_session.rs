//! Lane SMB-AE-MOTION-RECORDS-2 (feature `media`): saved documents carry their composition and
//! motion records (session document entries), a reopened document renders its saved render-queue
//! item ONCE for two output modules (one cropped, resized alpha matte), motion blur samples the
//! STU-MOT-004 shutter window, and text-animator glyphs come from Type shaping. Expected values are
//! hand-computed in the comments.
#![cfg(feature = "media")]

mod common;

use hsk_studio_accord::{ActorContext, CancellationToken, Ticks};
use hsk_studio_composite::vector_nib::shape_layer::{GroupTransform, ShapeFill, ShapeGroup, ShapeItem, shape_layer_content};
use hsk_studio_composite::{BlendSpace, SceneComposition, SceneContent, SceneLayer, ScenePixels, TileInfo};
use hsk_studio_folio::*;
use hsk_studio_harness::view::timeline_export::composition::{CompositionScenes, FrameRenderer, MotionBlur, SharedFrames};
use hsk_studio_harness::view::timeline_export::render_queue::session_render_queue_jobs;
use hsk_studio_nib::FillRule;
use hsk_studio_nib::raster::RasterOptions;
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_package::{MemoryAssetSource, write_package};
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::render_plan::{ColourTag, TilePlanes};
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};
use hsk_studio_reel::*;
use hsk_studio_session::{Session, SessionConfig, SessionPorts};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::sync::Arc;

const S: u64 = 254_016_000_000;
const F: u64 = 10_160_640_000; // 25 fps
const SPACE: ColourTag = ColourTag { profile_sha256: [0; 32], blend_space: BlendSpace::Encoded };

fn identity(prefix: &str, n: u8) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}

struct Available;
impl Resolver for Available {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}

struct Quiet;
impl SinkPort for Quiet {
    fn try_send(&mut self, _: DeliveryClass, _: &[u8]) -> Result<(), DeliveryError> {
        Ok(())
    }
}

struct NoPixels;
impl ScenePixels for NoPixels {
    fn tile(&self, _: &str) -> Option<TilePlanes<'_>> {
        None
    }
}

fn info(_: &TileRef) -> Option<TileInfo> {
    None
}

fn document() -> Value {
    let p = identity("SCPF", 1);
    json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":p,"working_space_cmyk":p,"working_space_gray":p,"working_space_spot":p,"blending_space":p,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",2),"name":"shape","parent":null,"kind":"raster","payload":{"payload_kind":"raster","tiles":[]},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[{"layer_id":identity("SLYR",2),"operation_key":"source","operation_kind":"source","inputs":[],
            "outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"source"}]}],"edges":[],
            "outputs":[{"layer_id":identity("SLYR",2),"operation_key":"source","port_key":"out"}]}
    })
}

fn compositions() -> StudioCompositionRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),
        "compositions":[{"schema_id":"hsk.studio.composition@1","composition_id":identity("SCOM",9),"name":"c","width":64,"height":32,
            "pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":25,"denominator":1},"duration":S,
            "layers":[{"layer_id":identity("SLYR",2),"timing":{"in_point":0,"out_point":S,"start_time":0,"stretch":{"numerator":1,"denominator":1}}}]}]}))
    .unwrap()
}

fn module(id: &str, extra: Value) -> Value {
    let mut m = json!({"module_id":id,"format":"mov_vc3","codec_option":"dnxhr_hq","include_project_link":false,"post_render_action":"none",
        "include_source_metadata":false,"channels":"rgb","depth":8,"color":"premultiplied","starting_number":0,"use_comp_frame_number":false,
        "audio":{"output":"off","sample_rate":48000,"bit_depth":32,"float":true,"channels":2},
        "colour":{"preserve_rgb":false,"output_color_space":"rec709","embed_color_space":true},"output_path":format!("renders/{id}.mov")});
    for (k, v) in extra.as_object().unwrap() {
        m[k] = v.clone();
    }
    m
}

fn motion() -> StudioMotionRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.motion_records@1","document_id":identity("SDOC",1),
        "shape_layers":[{"layer_id":identity("SLYR",2),"root":{"items":[
            {"kind":"rect","position":[8.0,8.0],"size":[16.0,16.0]},
            {"kind":"fill","rgba":[0.8,0.1,0.1,1.0],"opacity":100.0,"rule":"non_zero"}]},
            "streams":[{"path":[0],"field":"rect_position","value":{"static_value":[8.0,8.0],"keyframes":[
                {"time":0,"value":[8.0,8.0],"in_interp":"linear","out_interp":"linear"},
                {"time":S,"value":[40.0,8.0],"in_interp":"linear","out_interp":"linear"}]}}]}],
        "composition_settings":[{"composition_id":identity("SCOM",9),"motion_blur":true,"shutter_angle":180.0,"shutter_phase":0.0,
            "samples_per_frame":4,"adaptive_sample_limit":128,"motion_blur_layers":[identity("SLYR",2)]}],
        "render_queue":[{"item_id":"queue-1","composition_id":identity("SCOM",9),
            "settings":{"quality":"best","resolution":"full","disk_cache":"current_settings","proxy_use":"current_settings","effects":"current_settings",
                "solo_switches":"current_settings","guide_layers":"current_settings","color_depth":"8_bits_per_channel","frame_blending":"current_settings",
                "field_render":"off","pulldown_3_2":"off","motion_blur":"current_settings","time_span":{"custom":{"start_tick":0,"end_tick":5 * F}}},
            "output_modules":[module("master", json!({})),
                module("matte", json!({"channels":"alpha","crop":{"top":0,"left":0,"bottom":0,"right":32},"resize":{"width":16,"height":16,"lock_aspect":true}}))]}]}))
    .unwrap()
}

fn ports() -> SessionPorts {
    let mut p = SessionPorts::new(Box::new(Quiet));
    p.resolver = Box::new(Available);
    p
}

fn actor() -> ActorContext {
    ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap()
}

#[derive(Default)]
struct Collect {
    chunks: Vec<(i64, Vec<u8>)>,
}
impl ChunkSink for Collect {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
        self.chunks.push((c.pts, c.bytes.to_vec()));
        Ok(())
    }
}

fn read(bytes: &[u8], count: u64) -> Collect {
    let request = SegmentRequest {
        codec: "isobmff".to_owned(),
        start_pts: Ticks::new(0),
        count,
        track: Some(1),
        output: OutputMode::Decoded,
        prefer: SourcePreference::Original,
        purpose: Purpose::Export,
        grant: Grant { content_id: ContentId::parse("motion-session").unwrap(), revision: 1, expected_len: bytes.len() as u64 },
        expected_revision: 1,
    };
    let mut sink = Collect::default();
    Registry::standard()
        .decode_segment(&mut Cursor::new(bytes.to_vec()), None, &request, &Limits::default(), &CancellationToken::default(), &mut sink)
        .expect("reel reads the export");
    sink
}

#[test]
fn saved_records_reopen_and_render_once_into_two_modules() {
    let cancel = CancellationToken::default();
    let snap = match inspect_bytes(&serde_json::to_vec(&document()).unwrap(), Budget::default(), &cancel, &Available).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read-only"),
    };
    let mut package = Vec::new();
    write_package(&snap, &MemoryAssetSource::new(), &hsk_studio_package::Limits::default(), &cancel, &mut package).unwrap();
    let mut session = Session::open_bytes(&package, SessionConfig::new(actor()), ports(), &cancel).unwrap();
    // Motion records need the composition records first; a refusal changes nothing.
    assert!(session.set_motion_records(Some(&motion()), &Available, &cancel).is_err());
    assert_eq!(session.motion_records(&Available, &cancel).unwrap(), None);
    session.set_composition_records(Some(&compositions()), &Available, &cancel).unwrap();
    session.set_motion_records(Some(&motion()), &Available, &cancel).unwrap();

    let root = std::env::temp_dir().join(format!("harness-motion-session-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("renders")).unwrap();
    let path = root.join("doc.handshake");
    session.save_to(&path, &cancel).unwrap();
    let reopened = Session::open_path(&path, SessionConfig::new(actor()), ports(), &cancel).unwrap();
    assert_eq!(reopened.composition_records(&Available, &cancel).unwrap(), Some(compositions()));
    assert_eq!(reopened.motion_records(&Available, &cancel).unwrap(), Some(motion()));
    assert!(reopened.loss().is_empty(), "typed entries are no loss: {:?}", reopened.loss());

    let audio = |_: &str, _: u32| None;
    let (jobs, shared) = session_render_queue_jobs(&reopened, "queue-1", &Available, &info, &|| NoPixels, &BTreeMap::new(), &audio, SPACE, &root, false, &cancel).unwrap();
    let mut queue = ExportQueue::new();
    for (_, job) in jobs {
        queue.submit(job);
    }
    for r in queue.run() {
        assert_eq!((r.outcome.clone(), r.frames_encoded), (ExportOutcome::Complete, 5), "{r:?}");
    }
    assert_eq!(shared.renders(), 5, "five frames rendered once for two output modules");

    // Master: 64 x 32; the rect (x 1.28 n .. 16 + 1.28 n, y 0..16, motion-blurred along x) is red
    // at its centre (8 + 1.28 n, 8), black far away.
    let master = read(&std::fs::read(root.join("renders/master.mov")).unwrap(), 5);
    assert_eq!(master.chunks.iter().map(|c| c.0).collect::<Vec<_>>(), (0..5).map(|n| (n * F) as i64).collect::<Vec<_>>());
    for (n, (_, frame)) in master.chunks.iter().enumerate() {
        let cx = (8.0 + 1.28 * n as f64) as usize;
        let (yc, yfar) = (frame[8 * 64 + cx], frame[28 * 64 + 60]);
        // Y' of (0.8, 0.1, 0.1): 16 + 219 * (0.2126*0.8 + 0.7152*0.1 + 0.0722*0.1) = 70.48.
        assert!((i32::from(yc) - 70).abs() <= 3, "frame {n} centre luma {yc}");
        assert!(yfar <= 18, "frame {n} background luma {yfar}");
    }
    // Matte: crop to x 0..32, resize 32x32 -> 16x16 (output (x, y) samples source (2x + 0.5, 2y + 0.5)):
    // (4, 4) -> (8.5, 8.5) inside the rect: alpha 1 = white luma 235; (12, 12) -> y 24.5 below it: 16.
    let matte = read(&std::fs::read(root.join("renders/matte.mov")).unwrap(), 5);
    assert_eq!(matte.chunks.len(), 5);
    for (n, (_, frame)) in matte.chunks.iter().enumerate() {
        let (inside, below) = (frame[4 * 16 + 4], frame[12 * 16 + 12]);
        assert!(inside >= 230, "frame {n} matte inside {inside}");
        assert!(below <= 20, "frame {n} matte below {below}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// A red 16 x 16 square moving 16 px per frame (layer 1) over a static blue square at x 40..48
/// (layer 2); `tick` may be negative.
struct Moving(NoPixels);
fn square(id: &str, rgba: [f32; 4], x: f64, cancel: &CancellationToken) -> SceneLayer {
    let root = ShapeGroup {
        items: vec![
            ShapeItem::Rect { position: [8.0, 8.0], size: [16.0, 16.0], roundness: 0.0, reversed: false },
            ShapeItem::Fill(ShapeFill { rgba, opacity: 100.0, rule: FillRule::NonZero }),
        ],
        transform: GroupTransform::IDENTITY,
    };
    let content = shape_layer_content(id, &root, 0.0, SPACE, 0, &RasterOptions::default(), cancel).unwrap().unwrap();
    SceneLayer {
        layer_id: id.into(),
        content: SceneContent::Static(Box::new(content)),
        mode: StudioBlendMode::Normal,
        opacity: 1.0,
        fill_opacity: 1.0,
        matrix: Affine2 { m: [1.0, 0.0, x, 0.0, 1.0, 8.0] },
        matte: None,
        clipped: false,
    }
}
impl CompositionScenes for Moving {
    fn scene_at(&mut self, tick: i64, cancel: &CancellationToken) -> Result<SceneComposition, String> {
        Ok(SceneComposition {
            composition_id: "SCOM-019abcde-0000-7000-8000-0000000000c2".into(),
            width: 64,
            height: 32,
            background: None,
            filter: ResampleFilter::Bilinear,
            layers: vec![square("blue", [0.0, 0.0, 1.0, 1.0], 40.0, cancel), square("red", [1.0, 0.0, 0.0, 1.0], 16.0 * tick as f64 / F as f64, cancel)],
        })
    }
    fn pixels(&self) -> &dyn ScenePixels {
        &self.0
    }
}

#[test]
fn motion_blur_samples_the_shutter_window_of_checked_layers() {
    let cancel = CancellationToken::default();
    // 180 deg, phase 0, 4 samples at frame 0: ticks F/16 * (1, 3, 5, 7) => red x offsets 1, 3, 5, 7.
    let blur = MotionBlur { shutter_angle: 180.0, shutter_phase: 0.0, samples_per_frame: 4, layers: BTreeSet::from(["red".to_owned()]) };
    assert_eq!(blur.sample_ticks(0, F), vec![(F / 16) as i64, (3 * F / 16) as i64, (5 * F / 16) as i64, (7 * F / 16) as i64]);
    let window = MotionBlur { shutter_angle: 360.0, shutter_phase: -180.0, samples_per_frame: 2, layers: BTreeSet::new() };
    assert_eq!(window.sample_ticks(F, F), vec![(3 * F / 4) as i64, (5 * F / 4) as i64], "phase -180 centres the window on the frame");
    let mut r = FrameRenderer::new(Moving(NoPixels), SPACE, 64, 32, F, Some(blur.clone())).unwrap();
    let f = r.frame_at(0, &cancel).unwrap();
    // Row 8: coverage count of [s, s + 16) over s in {1, 3, 5, 7}, divided by 4.
    let want = [0.0, 0.25, 0.25, 0.5, 0.5, 0.75, 0.75, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.75, 0.75, 0.5, 0.5, 0.25, 0.25, 0.0];
    for (x, w) in want.iter().enumerate() {
        let a = f.alpha[8 * 64 + x];
        assert!((a - w).abs() < 1e-5, "x {x}: alpha {a} want {w}");
        if *w > 0.0 {
            assert!((f.rgb[(8 * 64 + x) * 3] - 1.0).abs() < 1e-5, "straight red at x {x}");
        }
    }
    // The unchecked blue layer stays crisp: x 40..56 opaque, 39 and 56 empty.
    assert_eq!((f.alpha[8 * 64 + 39], f.alpha[8 * 64 + 40], f.alpha[8 * 64 + 55], f.alpha[8 * 64 + 56]), (0.0, 1.0, 1.0, 0.0));
    assert_eq!(r.renders(), 1);

    // Render once: two consumers of one frame render it once.
    let shared = SharedFrames::new(FrameRenderer::new(Moving(NoPixels), SPACE, 64, 32, F, None).unwrap(), 0, 2, 2, 1 << 30).unwrap();
    let a = shared.frame(0, &cancel).unwrap();
    let b = shared.frame(0, &cancel).unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(shared.renders(), 1);
    // Hostile settings and budgets are refused before anything renders.
    for bad in [MotionBlur { samples_per_frame: 1, ..blur.clone() }, MotionBlur { shutter_angle: 721.0, ..blur.clone() }, MotionBlur { shutter_phase: f64::NAN, ..blur.clone() }] {
        assert_eq!(FrameRenderer::new(Moving(NoPixels), SPACE, 64, 32, F, Some(bad)).err().map(|e| e.code), Some("render_motion_blur_settings"));
    }
    let big = SharedFrames::new(FrameRenderer::new(Moving(NoPixels), SPACE, 64, 32, F, None).unwrap(), 0, 1_000_000, 2, 1 << 20);
    assert_eq!(big.err().map(|e| e.code), Some("render_once_cache_budget"));
}

#[test]
fn text_animator_glyphs_come_from_type_shaping() {
    use hsk_studio_harness::view::Viewer;
    use hsk_studio_harness::view::render::Prefer;
    use hsk_studio_harness::view::text::{FontFile, animated_glyphs, prepare};
    use hsk_studio_composite::text::animate::motion::{Animator, AnimatorProperties, RangeSelector, RangeUnits, Selector, evaluate_text_animators};
    let scratch = common::Scratch::new("motion-text");
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, common::solid_psd(96, 48, "paper", [255, 255, 255])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    v.text_size_pt = 24.0;
    let id = v.place_text(4.0, 34.0, "H i").unwrap();
    let doc = v.run().snapshot().unwrap().document().clone();
    let entry = v.text_entry().unwrap();
    let fonts = [FontFile::bundled().unwrap()];
    let cancel = CancellationToken::default();
    let glyphs = animated_glyphs(&doc, &entry, &fonts, &cancel).unwrap();
    let (g, fill) = &glyphs[&id];
    // Three shaped glyphs (H, space, i); the space has no outline; anchors advance left to right
    // from the record's pen origin (4, 34) on the baseline.
    assert_eq!(g.len(), 3);
    assert_eq!(g.iter().map(|x| x.is_space).collect::<Vec<_>>(), vec![false, true, false]);
    assert!(g[0].anchor_px[0] > 4.0 && g[0].anchor_px[0] < g[1].anchor_px[0] && g[1].anchor_px[0] < g[2].anchor_px[0]);
    assert!(g.iter().all(|x| x.anchor_px[1] == 34.0));
    assert_eq!(fill[3], 1.0, "the character style fill (the viewer brush colour), opaque");
    // Identity animators: nib's per-glyph coverage covers the same box as Type's own rasteriser of
    // the same shaped runs (two independent rasterisers, within one pixel).
    let chars = hsk_studio_composite::text::animate::text_chars(g);
    let ident = evaluate_text_animators(&chars, &[], [0.0, 0.0, 0.0, 1.0], 0.0).unwrap();
    let content = hsk_studio_composite::text::animate::animated_text_content(&id, g, &ident, *fill, SPACE, 1, &RasterOptions::default(), &cancel).unwrap().unwrap();
    let plane = prepare(&doc, &entry, &fonts, &cancel).unwrap();
    let rect = plane.plane(&id).unwrap().placement.unwrap();
    for (a, b) in [(content.rect.x, rect.x), (content.rect.y, rect.y), (content.rect.x + i64::from(content.rect.width), rect.x + i64::from(rect.width)), (content.rect.y + i64::from(content.rect.height), rect.y + i64::from(rect.height))] {
        assert!((a - b).abs() <= 1, "nib {:?} vs type {:?}", content.rect, rect);
    }
    // An animator on character index 2 ("i") moves only that glyph up 10 px.
    let up = Animator {
        properties: AnimatorProperties { position: Some([0.0, -10.0]), ..AnimatorProperties::default() },
        selectors: vec![Selector::Range(RangeSelector { start: 2.0, end: 3.0, units: RangeUnits::Index, ..RangeSelector::default() })],
    };
    let moved = evaluate_text_animators(&chars, &[up], [0.0, 0.0, 0.0, 1.0], 0.0).unwrap();
    let after = hsk_studio_composite::text::animate::animated_text_content(&id, g, &moved, *fill, SPACE, 1, &RasterOptions::default(), &cancel).unwrap().unwrap();
    let bounds = |c: &hsk_studio_composite::StaticContent, k: u32| c.owned_masks[&hsk_studio_pigment::render_plan::SourceKey(k)].bounds;
    assert_eq!(bounds(&after, 0), bounds(&content, 0), "H stays");
    let (b0, b1) = (bounds(&content, 1), bounds(&after, 1));
    assert_eq!((b1.x, b1.y, b1.width, b1.height), (b0.x, b0.y - 10, b0.width, b0.height), "i moves up 10 px");
}
