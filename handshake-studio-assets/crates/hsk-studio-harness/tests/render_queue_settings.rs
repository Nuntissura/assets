//! Lane SMB-AE-RENDER-QUEUE-2 (feature `media`): the STU-MOT-121 render settings that used to be
//! refused now render. Spec basis: STU-MOT-121 (`resolution` full / half / third / quarter / custom
//! divisor pair, `quality`, `color_depth` 8 / 16 / 32, `frame_blending`, `field_render`, `time_span`
//! `work_area_only`), STU-MOT-007a (frame blending is one engine with two entry points), STU-MOT-122
//! (`channels` RGB / alpha / RGB+alpha, `color` premultiplied / straight), STU-COL-120 (32-bit is
//! floating point, unbounded). After Effects parity of every setting is UNVERIFIED (no D5 oracle case).
//!
//! Frames are examined through `FrameRenderer` / `SharedFrames` (straight float frames, exact
//! values); one test encodes a real file to prove the span and the reduced frame size reach reel.
#![cfg(feature = "media")]

use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::temporal::composition::{CompositionInput, CompositionTemporalSource};
use hsk_studio_composite::vector_nib::shape_layer::{GroupTransform, ShapeFill, ShapeGroup, ShapeItem, shape_layer_content};
use hsk_studio_composite::{BlendSpace, RenderInputs, SceneComposition, SceneContent, SceneLayer, ScenePixels, TileInfo};
use hsk_studio_folio::*;
use hsk_studio_harness::view::monitor::MonitorFail;
use hsk_studio_harness::view::timeline_export::composition::{
    CompositionScenes, Frame, FrameRenderer, OutputChannels, OutputPost, RenderDepth, RenderOptions,
};
use hsk_studio_harness::view::timeline_export::render_queue::{
    RenderQueueBuild, TemporalScenes, apply_frame_blending_switch, item_span, render_queue_build, render_settings,
};
use hsk_studio_nib::FillRule;
use hsk_studio_nib::raster::RasterOptions;
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::render_plan::{ColourTag, TilePlanes};
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};
use hsk_studio_reel::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const S: u64 = 254_016_000_000;
const F: u64 = 10_160_640_000; // 25 fps
const SPACE: ColourTag = ColourTag { profile_sha256: [0; 32], blend_space: BlendSpace::Encoded };
const RED: [f32; 3] = [0.8, 0.1, 0.1];

fn identity(prefix: &str, n: u8) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}

struct NoPixels;
impl ScenePixels for NoPixels {
    fn tile(&self, _: &str) -> Option<TilePlanes<'_>> {
        None
    }
}

/// One rectangle of a synthetic scene.
#[derive(Clone)]
struct Rect {
    id: &'static str,
    rgba: [f32; 4],
    position: [f64; 2],
    size: [f64; 2],
    mode: StudioBlendMode,
}

fn rect(id: &'static str, rgba: [f32; 4], position: [f64; 2], size: [f64; 2]) -> Rect {
    Rect { id, rgba, position, size, mode: StudioBlendMode::Normal }
}

/// A 64 x 32 scene over an opaque black background, the rectangles bottom-first.
fn scene(rects: &[Rect], cancel: &CancellationToken) -> SceneComposition {
    let layers = rects
        .iter()
        .map(|r| {
            let root = ShapeGroup {
                items: vec![
                    ShapeItem::Rect { position: r.position, size: r.size, roundness: 0.0, reversed: false },
                    ShapeItem::Fill(ShapeFill { rgba: r.rgba, opacity: 100.0, rule: FillRule::NonZero }),
                ],
                transform: GroupTransform::IDENTITY,
            };
            let content = shape_layer_content(r.id, &root, 0.0, SPACE, 0, &RasterOptions::default(), cancel).unwrap().expect("content");
            SceneLayer {
                layer_id: r.id.into(),
                content: SceneContent::Static(Box::new(content)),
                mode: r.mode,
                opacity: 1.0,
                fill_opacity: 1.0,
                matrix: Affine2::IDENTITY,
                matte: None,
                clipped: false,
            }
        })
        .collect();
    SceneComposition {
        composition_id: identity("SCOM", 1),
        width: 64,
        height: 32,
        background: Some([0.0, 0.0, 0.0, 1.0]),
        filter: ResampleFilter::Bilinear,
        layers,
    }
}

struct Fixed(Vec<Rect>, NoPixels);
impl CompositionScenes for Fixed {
    fn scene_at(&mut self, _: i64, cancel: &CancellationToken) -> Result<SceneComposition, String> {
        Ok(scene(&self.0, cancel))
    }
    fn pixels(&self) -> &dyn ScenePixels {
        &self.1
    }
}

fn renderer(rects: Vec<Rect>, options: RenderOptions) -> FrameRenderer<Fixed> {
    FrameRenderer::new(Fixed(rects, NoPixels), SPACE, 64, 32, F, None).unwrap().with_options(options).unwrap()
}

fn render(rects: &[Rect], options: RenderOptions) -> Frame {
    renderer(rects.to_vec(), options).frame_at(0, &CancellationToken::default()).unwrap()
}

fn px(f: &Frame, x: u32, y: u32) -> [f32; 3] {
    let i = ((y * f.width + x) * 3) as usize;
    [f.rgb[i], f.rgb[i + 1], f.rgb[i + 2]]
}

fn near(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= tol)
}

fn depth(depth: RenderDepth) -> RenderOptions {
    RenderOptions { divisor: (1, 1), depth }
}

// --- resolution -------------------------------------------------------------------------------

#[test]
fn reduced_resolutions_render_the_scene_at_the_reduced_size() {
    let square = [rect("SLYR-a", [RED[0], RED[1], RED[2], 1.0], [8.0, 8.0], [16.0, 16.0])];
    let full = render(&square, RenderOptions::default());
    assert_eq!((full.width, full.height), (64, 32));
    assert!(near(px(&full, 16, 16), RED, 1e-5) && near(px(&full, 60, 28), [0.0; 3], 0.0));
    // Divisor pairs: the size is ceil(side / divisor), the layers resample into it.
    for ((x, y), (w, h)) in [((2, 2), (32, 16)), ((3, 3), (22, 11)), ((4, 4), (16, 8)), ((5, 3), (13, 11)), ((1, 4), (64, 8))] {
        let f = render(&square, RenderOptions { divisor: (x, y), depth: RenderDepth::Float });
        assert_eq!((f.width, f.height), (w, h), "divisor {x}x{y}");
        assert_eq!((f.rgb.len(), f.alpha.len()), ((w * h * 3) as usize, (w * h) as usize));
    }
    // Half: the square covers source [8, 24) = reduced [4, 12) on both axes; its interior is the
    // colour, the far corner is the background, and the render is repeatable.
    let half = render(&square, depth_pair(2));
    let tol = 1e-4;
    assert!(near(px(&half, 8, 8), RED, tol), "interior {:?}", px(&half, 8, 8));
    assert!(near(px(&half, 30, 14), [0.0; 3], 0.0), "corner {:?}", px(&half, 30, 14));
    assert!(near(px(&half, 1, 1), [0.0; 3], 0.0));
    assert_eq!(half, render(&square, depth_pair(2)), "deterministic");
    // Quarter: reduced [2, 6).
    let quarter = render(&square, depth_pair(4));
    assert!(near(px(&quarter, 4, 4), RED, tol), "interior {:?}", px(&quarter, 4, 4));
    assert!(near(px(&quarter, 14, 7), [0.0; 3], 0.0));
}

fn depth_pair(d: u32) -> RenderOptions {
    RenderOptions { divisor: (d, d), depth: RenderDepth::Float }
}

#[test]
fn hostile_resolution_divisors_are_refused() {
    for divisor in [(0, 1), (1, 0), (100, 1), (1, 100), (u32::MAX, 2)] {
        let e = RenderOptions { divisor, depth: RenderDepth::Float }.size(64, 32).unwrap_err();
        assert_eq!(e.code, "render_resolution", "{divisor:?}");
        let built = FrameRenderer::new(Fixed(vec![], NoPixels), SPACE, 64, 32, F, None).unwrap().with_options(RenderOptions { divisor, depth: RenderDepth::Float });
        assert!(built.is_err(), "{divisor:?}");
    }
    // A side never drops below one pixel.
    assert_eq!(RenderOptions { divisor: (99, 99), depth: RenderDepth::Float }.size(3, 2).unwrap(), (1, 1));
    // A scene of another raster than the renderer's source is refused per frame, not scaled.
    let mut r = FrameRenderer::new(Fixed(vec![], NoPixels), SPACE, 32, 32, F, None).unwrap();
    assert!(r.frame_at(0, &CancellationToken::default()).is_err());
}

// --- bit depth --------------------------------------------------------------------------------

#[test]
fn bit_depths_quantise_to_their_levels_and_32_bit_is_unbounded() {
    let v = [0.123_456_f32, 0.5, 0.9];
    let cover = [rect("SLYR-a", [v[0], v[1], v[2], 1.0], [0.0, 0.0], [64.0, 32.0])];
    let float = render(&cover, depth(RenderDepth::Float));
    assert!(near(px(&float, 32, 16), v, 1e-5), "{:?}", px(&float, 32, 16));
    let b8 = px(&render(&cover, depth(RenderDepth::Bits8)), 32, 16);
    let b16 = px(&render(&cover, depth(RenderDepth::Bits16)), 32, 16);
    let b32 = render(&cover, depth(RenderDepth::Bits32));
    for c in 0..3 {
        let want = px(&float, 32, 16)[c];
        // 8 bits: whole 1/255 levels within half a level of the float value.
        let l8 = b8[c] * 255.0;
        assert!((l8 - l8.round()).abs() < 1e-3 && (b8[c] - want).abs() <= 0.5 / 255.0 + 1e-5, "8 bpc channel {c}: {} vs {want}", b8[c]);
        // 16 bits: whole 1/65535 levels within half a level.
        let l16 = b16[c] * 65535.0;
        assert!((l16 - l16.round()).abs() < 1e-2 && (b16[c] - want).abs() <= 0.5 / 65535.0 + 1e-5, "16 bpc channel {c}: {} vs {want}", b16[c]);
        // 32 bits keeps the value (in range, the unbounded stage changes nothing).
        assert!((px(&b32, 32, 16)[c] - want).abs() <= 1e-6, "32 bpc channel {c}");
    }
    assert_ne!(b8, b16, "8 and 16 bits differ on a value between 8-bit levels");
    // The quantiser itself: hand-computed levels, NaN and out-of-range samples.
    assert_eq!(RenderDepth::Bits8.quantise(0.5), 128.0 / 255.0);
    assert_eq!(RenderDepth::Bits16.quantise(1.0 / 3.0), 21845.0 / 65535.0);
    assert_eq!((RenderDepth::Bits8.quantise(f32::NAN), RenderDepth::Bits8.quantise(7.0), RenderDepth::Bits16.quantise(-3.0)), (0.0, 1.0, 0.0));
    assert_eq!((RenderDepth::Bits32.quantise(7.5), RenderDepth::Float.quantise(0.1234)), (7.5, 0.1234));
}

#[test]
fn thirty_two_bit_rendering_keeps_values_above_one() {
    // 0.8 + 0.8 with linear dodge (add): the unit-range stages clamp at 1.0, 32 bpc keeps 1.6.
    let mut add = rect("SLYR-b", [0.8, 0.8, 0.8, 1.0], [0.0, 0.0], [64.0, 32.0]);
    add.mode = StudioBlendMode::LinearDodgeAdd;
    let layers = [rect("SLYR-a", [0.8, 0.1, 0.1, 1.0], [0.0, 0.0], [64.0, 32.0]), add];
    for d in [RenderDepth::Float, RenderDepth::Bits8, RenderDepth::Bits16] {
        assert!(near(px(&render(&layers, depth(d)), 32, 16), [1.0, 0.9, 0.9], 5e-3), "{d:?}");
    }
    let hdr = px(&render(&layers, depth(RenderDepth::Bits32)), 32, 16);
    assert!(near(hdr, [1.6, 0.9, 0.9], 1e-4), "32 bpc {hdr:?}");
}

// --- alpha output laws --------------------------------------------------------------------------

fn alpha_frame() -> Frame {
    // 2 x 1: a half-transparent pixel with straight colour (0.8, 0.4, 0.2), then a transparent one.
    Frame { width: 2, height: 1, rgb: vec![0.8, 0.4, 0.2, 0.9, 0.9, 0.9], alpha: vec![0.5, 0.0] }
}

fn floats(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

#[test]
fn alpha_output_modes_obey_the_straight_premultiplied_laws() {
    let f = alpha_frame();
    let post = |channels, premultiplied| OutputPost { channels, premultiplied, ..OutputPost::default() };
    let straight = floats(&post(OutputChannels::Rgb, false).apply(&f));
    let premult = floats(&post(OutputChannels::Rgb, true).apply(&f));
    let matte = floats(&post(OutputChannels::Alpha, true).apply(&f));
    assert_eq!(straight, vec![0.8, 0.4, 0.2, 0.0, 0.0, 0.0], "straight colour, zero where alpha is zero");
    assert_eq!(premult, vec![0.4, 0.2, 0.1, 0.0, 0.0, 0.0], "premultiplied = straight x alpha");
    assert_eq!(matte, vec![0.5, 0.5, 0.5, 0.0, 0.0, 0.0], "alpha matte in all three channels");
    // RGB+alpha: the colour part equals the RGB-only frame, the alpha part equals the matte.
    for premultiplied in [false, true] {
        let p = post(OutputChannels::Rgb, premultiplied);
        let rgba = floats(&p.apply_rgba(&f));
        let rgb = floats(&p.apply(&f));
        assert_eq!(rgba.len(), 8);
        for px in 0..2 {
            assert_eq!(rgba[px * 4..px * 4 + 3], rgb[px * 3..px * 3 + 3], "colour part (premultiplied {premultiplied})");
            assert_eq!(rgba[px * 4 + 3], matte[px * 3], "alpha part");
        }
        // Straight and premultiplied agree where alpha is 1 and round-trip where it is not.
    }
    let (s, p) = (floats(&post(OutputChannels::Rgb, false).apply_rgba(&f)), floats(&post(OutputChannels::Rgb, true).apply_rgba(&f)));
    for c in 0..3 {
        assert!((p[c] - s[c] * s[3]).abs() < 1e-7 && (p[c] / p[3] - s[c]).abs() < 1e-6, "round trip channel {c}");
    }
    let opaque = Frame { width: 1, height: 1, rgb: vec![0.3, 0.6, 0.9], alpha: vec![1.0] };
    assert_eq!(post(OutputChannels::Rgb, false).apply_rgba(&opaque), post(OutputChannels::Rgb, true).apply_rgba(&opaque), "opaque: straight == premultiplied");
    // Resize and crop apply to the RGBA frame like the RGB one (size = out_w x out_h x 16 bytes).
    let resized = OutputPost { resize: Some((4, 2)), ..post(OutputChannels::Rgb, true) };
    assert_eq!(resized.apply_rgba(&f).len(), 4 * 2 * 16);
}

// --- record-driven settings -----------------------------------------------------------------------

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

fn document() -> Value {
    let p = identity("SCPF", 1);
    json!({
        "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":7,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":p,"working_space_cmyk":p,"working_space_gray":p,"working_space_spot":p,"blending_space":p,
        "artboards":[],"page_spreads":[],
        "layers":[
            {"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",1),"name":"holder","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true},
            {"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",2),"name":"shape","parent":null,"kind":"raster","payload":{"payload_kind":"raster","tiles":[]},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]}
    })
}

fn snapshot() -> Snapshot {
    match inspect_bytes(&serde_json::to_vec(&document()).unwrap(), Budget::default(), &CancellationToken::default(), &Available).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read-only"),
    }
}

fn timing(a: u64, b: u64) -> Value {
    json!({"in_point":a,"out_point":b,"start_time":0,"stretch":{"numerator":1,"denominator":1}})
}

/// SCOM-1 (outer, 10 frames at 25 fps): layer 1 is the precomposition of SCOM-2 (the source, 25
/// frames) whose layer 2 is a shape layer moving one pixel per frame; `outer` is merged into layer 1.
fn compositions(outer: Value) -> StudioCompositionRecords {
    let mut l1 = json!({"layer_id":identity("SLYR",1),"timing":timing(0,10 * F),"precomp":{"composition_id":identity("SCOM",2),"collapse_transformations":false}});
    for (k, v) in outer.as_object().cloned().unwrap_or_default() {
        l1[k] = v;
    }
    serde_json::from_value(json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),"compositions":[
        {"schema_id":"hsk.studio.composition@1","composition_id":identity("SCOM",1),"name":"outer","width":64,"height":32,
         "pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":25,"denominator":1},"duration":10 * F,"layers":[l1]},
        {"schema_id":"hsk.studio.composition@1","composition_id":identity("SCOM",2),"name":"source","width":64,"height":32,
         "pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":25,"denominator":1},"duration":S,
         "layers":[{"layer_id":identity("SLYR",2),"timing":timing(0,S)}]}]}))
    .unwrap()
}

fn module(id: &str) -> Value {
    json!({"module_id":id,"format":"mov_vc3","codec_option":"dnxhr_hq","include_project_link":false,"post_render_action":"none",
        "include_source_metadata":false,"channels":"rgb","depth":8,"color":"straight","starting_number":0,"use_comp_frame_number":false,
        "audio":{"output":"off","sample_rate":48000,"bit_depth":32,"float":true,"channels":2},
        "colour":{"preserve_rgb":false,"output_color_space":"rec709","embed_color_space":true},"output_path":format!("renders/{id}.mov")})
}

fn settings(edit: Value) -> Value {
    let mut s = json!({"quality":"best","resolution":"full","disk_cache":"current_settings","proxy_use":"current_settings","effects":"current_settings",
        "solo_switches":"current_settings","guide_layers":"current_settings","color_depth":"8_bits_per_channel","frame_blending":"current_settings",
        "field_render":"off","pulldown_3_2":"off","motion_blur":"off_for_all_layers","time_span":"length_of_comp"});
    for (k, v) in edit.as_object().unwrap() {
        s[k] = v.clone();
    }
    s
}

/// The shape layer moves from x = 8 to x = 33 over the source's 25 frames: one pixel per frame.
fn motion(settings: Value, extra: Value) -> StudioMotionRecords {
    let mut m = json!({"schema_id":"hsk.studio.motion_records@1","document_id":identity("SDOC",1),
        "shape_layers":[{"layer_id":identity("SLYR",2),"root":{"items":[
            {"kind":"rect","position":[8.0,8.0],"size":[16.0,16.0]},
            {"kind":"fill","rgba":[0.8,0.1,0.1,1.0],"opacity":100.0,"rule":"non_zero"}]},
            "streams":[{"path":[0],"field":"rect_position","value":{"static_value":[8.0,8.0],"keyframes":[
                {"time":0,"value":[8.0,8.0],"in_interp":"linear","out_interp":"linear"},
                {"time":S,"value":[33.0,8.0],"in_interp":"linear","out_interp":"linear"}]}}]}],
        "render_queue":[{"item_id":"queue-1","composition_id":identity("SCOM",1),"settings":settings,"output_modules":[module("master")]}]});
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        m[k] = v;
    }
    serde_json::from_value(m).unwrap()
}

fn info(_: &TileRef) -> Option<TileInfo> {
    None
}

type Built = Result<RenderQueueBuild<TemporalScenes<NoPixels>>, MonitorFail>;

fn build(records: &StudioCompositionRecords, motion: &StudioMotionRecords, root: &std::path::Path) -> Built {
    let snap = snapshot();
    let item = motion.render_queue[0].clone();
    let mut records = records.clone();
    apply_frame_blending_switch(&mut records, item.settings.frame_blending);
    let comp = records.composition(&item.composition_id).unwrap().clone();
    let mut scenes = || {
        let id = identity("SCOM", 1);
        let source = CompositionTemporalSource::new(
            &CompositionInput {
                snapshot: &snap, expected_revision: 7, records: &records, composition_id: &id, budget: Budget::default(),
                resolver: &Available, blend_space: BlendSpace::Encoded, info: &info, space: SPACE, inputs: RenderInputs::default(),
            },
            &CancellationToken::default(),
        )
        .and_then(|s| s.with_motion_records(motion, &BTreeMap::new(), SPACE, 7, None))
        .map_err(|e| MonitorFail { code: "scenes", detail: format!("{e:?}") })?;
        Ok(TemporalScenes { source, pixels: NoPixels })
    };
    let audio = |_: &str, _: u32| None;
    render_queue_build(&item, &comp, Some(motion), Some(8), &mut scenes, &audio, SPACE, root, false)
}

fn code(b: Built) -> Option<&'static str> {
    b.err().map(|e| e.code)
}

#[test]
fn fixture_records_validate() {
    let motion = motion(settings(json!({})), json!({}));
    validate_motion_records(&motion, snapshot().document(), &compositions(json!({})), Budget::default(), &CancellationToken::default(), &Available)
        .expect("valid motion records");
}

#[test]
fn spans_whole_composition_work_area_and_custom_in_out() {
    let comps = compositions(json!({}));
    let comp = comps.compositions[0].clone();
    let item = |span: Value| motion(settings(json!({"time_span": span})), json!({})).render_queue[0].clone();
    // Whole composition: 10 frames.
    assert_eq!(item_span(&item(json!("length_of_comp")), &comp, None).unwrap(), (0, 10 * F, F));
    // Work area: the stored span, whole frames (a partial first frame rounds down, a partial last up).
    assert_eq!(item_span(&item(json!("work_area_only")), &comp, Some((2 * F, 5 * F))).unwrap(), (2 * F, 5 * F, F));
    assert_eq!(item_span(&item(json!("work_area_only")), &comp, Some((2 * F + 1, 5 * F + 1))).unwrap(), (2 * F, 6 * F, F), "partial frames render whole");
    // No stored work area: the whole composition.
    assert_eq!(item_span(&item(json!("work_area_only")), &comp, None).unwrap(), (0, 10 * F, F));
    // Custom in / out; the boundaries are exact.
    assert_eq!(item_span(&item(json!({"custom":{"start_tick":F,"end_tick":2 * F}})), &comp, None).unwrap(), (F, 2 * F, F));
    assert_eq!(item_span(&item(json!({"custom":{"start_tick":0,"end_tick":10 * F}})), &comp, None).unwrap(), (0, 10 * F, F));
    // Hostile spans: empty, reversed, past the end (also via the work area argument).
    for span in [json!({"custom":{"start_tick":F,"end_tick":F}}), json!({"custom":{"start_tick":3 * F,"end_tick":F}}), json!({"custom":{"start_tick":0,"end_tick":10 * F + 1}})] {
        assert_eq!(item_span(&item(span), &comp, None).unwrap_err().code, "render_span_range");
    }
    assert_eq!(item_span(&item(json!("work_area_only")), &comp, Some((0, 11 * F))).unwrap_err().code, "render_span_range");
    assert_eq!(item_span(&item(json!("work_area_only")), &comp, Some((4 * F, 4 * F))).unwrap_err().code, "render_span_range");
}

#[test]
fn work_area_records_validate_and_roundtrip() {
    let comps = compositions(json!({}));
    let ok = |areas: Value| {
        let m = motion(settings(json!({})), json!({"composition_work_areas": areas}));
        validate_motion_records(&m, snapshot().document(), &comps, Budget::default(), &CancellationToken::default(), &Available).map(|_| m)
    };
    let m = ok(json!([{"composition_id":identity("SCOM",1),"start_tick":F,"end_tick":4 * F}])).unwrap();
    assert_eq!(m.work_area(&identity("SCOM", 1)), Some((F, 4 * F)));
    assert_eq!(m.work_area(&identity("SCOM", 2)), None);
    let back: StudioMotionRecords = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
    assert_eq!(back, m);
    // Records without work areas serialise without the member (byte-identical to earlier sets).
    assert!(!serde_json::to_string(&motion(settings(json!({})), json!({}))).unwrap().contains("work_area"));
    // Hostile: dangling composition, duplicate, empty span, span past the end.
    let bad = |areas: Value| ok(areas).err().map(|e| e.code);
    assert_eq!(bad(json!([{"composition_id":identity("SCOM",7),"start_tick":0,"end_tick":F}])), Some(Code::DanglingReference));
    let a = json!({"composition_id":identity("SCOM",1),"start_tick":0,"end_tick":F});
    assert_eq!(bad(json!([a, a])), Some(Code::DuplicateIdentity));
    assert_eq!(bad(json!([{"composition_id":identity("SCOM",1),"start_tick":F,"end_tick":F}])), Some(Code::InvalidField));
    assert_eq!(bad(json!([{"composition_id":identity("SCOM",1),"start_tick":0,"end_tick":10 * F + 1}])), Some(Code::InvalidField));
}

#[test]
fn the_queue_renders_the_work_area_at_reduced_resolution_and_depth() {
    let root = std::env::temp_dir().join(format!("harness-render-settings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("renders")).unwrap();
    let work_area = json!({"composition_work_areas":[{"composition_id":identity("SCOM",1),"start_tick":2 * F,"end_tick":5 * F}]});
    let m = motion(settings(json!({"time_span":"work_area_only","resolution":"half","color_depth":"16_bits_per_channel"})), work_area);
    let mut built = build(&compositions(json!({})), &m, &root).expect("builds");
    assert!(built.notes.is_empty(), "{:?}", built.notes);
    let (id, job) = built.jobs.remove(0);
    assert_eq!(id, "master");
    assert_eq!((job.span.start_ticks, job.span.end_ticks), (2 * F, 5 * F));
    let video = job.video.as_ref().unwrap();
    assert_eq!((video.frame_count(), video.format().width, video.format().height), (3, 32, 16), "3 frames at half size");
    // The frame at the work area's start: source frame 2 = the square at x = 10 -> reduced x = 5.
    let cancel = CancellationToken::default();
    let f0 = built.shared.frame(0, &cancel).unwrap();
    assert_eq!((f0.width, f0.height), (32, 16));
    assert!(near(px(&f0, 10, 8), RED, 2e-3), "interior {:?}", px(&f0, 10, 8));
    assert!(near(px(&f0, 2, 8), [0.0; 3], 0.0), "left of the square {:?}", px(&f0, 2, 8));
    let l16 = px(&f0, 10, 8)[0] * 65535.0;
    assert!((l16 - l16.round()).abs() < 1e-2, "16-bit levels");
    // The file: encode the item and read the receipt.
    let mut queue = ExportQueue::new();
    queue.submit(job);
    let receipts = queue.run();
    assert_eq!(receipts[0].outcome, ExportOutcome::Complete, "{:?}", receipts[0]);
    assert_eq!((receipts[0].span.start_ticks, receipts[0].span.end_ticks, receipts[0].frames_encoded), (2 * F, 5 * F, 3));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn settings_that_render_differently_from_their_name_say_so() {
    let comps = compositions(json!({}));
    let root = std::env::temp_dir();
    let notes = |edit: Value, extra: Value| build(&comps, &motion(settings(edit), extra), &root).map(|b| b.notes.into_iter().map(|n| n.code).collect::<Vec<_>>());
    // Draft renders as best, noted.
    assert_eq!(notes(json!({"quality":"draft"}), json!({})).unwrap(), vec!["render_quality_draft_is_best"]);
    // Work area only without a stored work area renders the whole composition, noted.
    assert_eq!(notes(json!({"time_span":"work_area_only"}), json!({})).unwrap(), vec!["render_work_area_whole_comp"]);
    // `current_settings` colour depth takes the document's bits (the builder passes 8 here); without
    // one of 8 / 16 / 32 the default pipeline renders, noted.
    assert!(notes(json!({"color_depth":"current_settings"}), json!({})).unwrap().is_empty());
    let item = motion(settings(json!({"color_depth":"current_settings"})), json!({})).render_queue[0].clone();
    let comp = comps.compositions[0].clone();
    let (options, n) = render_settings(&item, &comp, Some(32)).unwrap();
    assert_eq!((options.depth, n.len()), (RenderDepth::Bits32, 0));
    let (options, n) = render_settings(&item, &comp, Some(1)).unwrap();
    assert_eq!((options.depth, n[0].code), (RenderDepth::Float, "render_color_depth_current_unset"));
    let (options, _) = render_settings(&item, &comp, Some(16)).unwrap();
    assert_eq!(options.depth, RenderDepth::Bits16);
}

#[test]
fn settings_still_refused_are_refused_before_rendering() {
    let comps = compositions(json!({}));
    let root = std::env::temp_dir();
    let refused = |edit: Value| code(build(&comps, &motion(settings(edit), json!({})), &root));
    // Fields and pulldown: no reel encoder writes field flags, so the output could not say which field is first.
    assert_eq!(refused(json!({"field_render":"upper_field_first"})), Some("render_fields"));
    assert_eq!(refused(json!({"field_render":"lower_field_first"})), Some("render_fields"));
    assert_eq!(refused(json!({"pulldown_3_2":"WSSWW"})), Some("render_fields"));
    assert_eq!(refused(json!({"quality":"wireframe"})), Some("render_quality"));
    assert_eq!(refused(json!({"effects":"all_off"})), Some("render_effects_switch"));
    assert_eq!(refused(json!({"frame_rate":{"numerator":30,"denominator":1}})), Some("render_frame_rate_conversion"));
    // The settings that were refused before now build.
    for edit in [
        json!({"resolution":"half"}), json!({"resolution":"third"}), json!({"resolution":"quarter"}), json!({"resolution":{"custom":{"x":5,"y":3}}}),
        json!({"color_depth":"16_bits_per_channel"}), json!({"color_depth":"32_bits_per_channel"}), json!({"quality":"draft"}),
        json!({"frame_blending":"on_for_checked_layers"}), json!({"frame_blending":"off_for_all_layers"}),
    ] {
        assert_eq!(refused(edit.clone()), None, "{edit}");
    }
    // RGB+alpha is still refused: no reel encoder carries alpha (OutputPost::apply_rgba is ready for one).
    let mut m = motion(settings(json!({})), json!({}));
    m.render_queue[0].output_modules[0].channels = StudioOutputChannels::RgbAlpha;
    assert_eq!(code(build(&comps, &m, &root)), Some("output_alpha_codec"));
    // Hostile custom divisors straight from a record (bypassing Folio validation) are refused by the queue.
    let mut m = motion(settings(json!({})), json!({}));
    m.render_queue[0].settings.resolution = StudioRenderResolution::Custom { x: 0, y: 1 };
    assert_eq!(code(build(&comps, &m, &root)), Some("render_resolution"));
    m.render_queue[0].settings.resolution = StudioRenderResolution::Custom { x: 1, y: 100 };
    assert_eq!(code(build(&comps, &m, &root)), Some("render_resolution"));
}

// --- frame blending ---------------------------------------------------------------------------

/// Half speed: outer frame k shows source time k / 2 frames (remap keys in seconds).
fn half_speed() -> Value {
    json!({"static_value":[0.0],"keyframes":[
        {"time":0,"value":[0.0],"in_interp":"linear","out_interp":"linear"},
        {"time":10 * F,"value":[5.0 / 25.0],"in_interp":"linear","out_interp":"linear"}]})
}

fn frames_of(outer: Value, switch: &str) -> Vec<std::sync::Arc<Frame>> {
    let root = std::env::temp_dir();
    let m = motion(settings(json!({"frame_blending":switch})), json!({}));
    let b = build(&compositions(outer), &m, &root).unwrap_or_else(|e| panic!("{switch}: {e:?}"));
    let cancel = CancellationToken::default();
    (0..10).map(|k| b.shared.frame(k, &cancel).unwrap()).collect()
}

#[test]
fn frame_blending_renders_per_layer_and_off_for_all_layers_forces_it_off() {
    let layer = json!({"time_remap": half_speed(), "frame_blending": "frame_mix"});
    // The square covers [8 + a, 24 + a) at source frame a. Outer frame 3 = source 1.5: floor frame 1
    // is [9, 25), frame 2 is [10, 26).
    let on = frames_of(layer.clone(), "current_settings");
    let checked = frames_of(layer.clone(), "on_for_checked_layers");
    let off = frames_of(layer.clone(), "off_for_all_layers");
    let unblended = frames_of(json!({"time_remap": half_speed()}), "current_settings");
    assert_eq!(on[3], checked[3], "current_settings and on_for_checked_layers use each layer's own switch");
    // Whole-frame outer frames (even k) show an integral source frame: blending changes nothing.
    assert_eq!(on[2], unblended[2]);
    // Frame 3: blended = half red where only one of the two source frames covers the column.
    let blended = &on[3];
    assert!(near(px(blended, 9, 16), [RED[0] * 0.5, RED[1] * 0.5, RED[2] * 0.5], 2e-3), "column only in frame 1 {:?}", px(blended, 9, 16));
    assert!(near(px(blended, 25, 16), [RED[0] * 0.5, RED[1] * 0.5, RED[2] * 0.5], 2e-3), "column only in frame 2 {:?}", px(blended, 25, 16));
    assert!(near(px(blended, 17, 16), RED, 2e-3), "column in both {:?}", px(blended, 17, 16));
    // Off for all layers: the floor frame, whatever the layer record says; equal to a layer without blending.
    assert_eq!(off[3], unblended[3]);
    assert!(near(px(&off[3], 9, 16), RED, 2e-3) && near(px(&off[3], 25, 16), [0.0; 3], 0.0), "floor frame");
    assert_ne!(on[3], off[3], "blending is visible");
    // The refusal that guards a caller who forgot the switch: the unforced composition is refused.
    let comps = compositions(layer);
    let item = motion(settings(json!({"frame_blending":"off_for_all_layers"})), json!({})).render_queue[0].clone();
    let comp = comps.compositions[0].clone();
    assert_eq!(render_settings(&item, &comp, Some(8)).unwrap_err().code, "render_frame_blending_off_unapplied");
    let mut forced = comps.clone();
    apply_frame_blending_switch(&mut forced, StudioLayerSwitch::OffForAllLayers);
    assert!(forced.compositions.iter().flat_map(|c| &c.layers).all(|l| l.frame_blending.is_off()));
    assert!(render_settings(&item, &forced.compositions[0], Some(8)).is_ok());
    // Other switches leave the layers alone.
    let mut kept = comps.clone();
    apply_frame_blending_switch(&mut kept, StudioLayerSwitch::OnForCheckedLayers);
    assert_eq!(kept, comps);
}
