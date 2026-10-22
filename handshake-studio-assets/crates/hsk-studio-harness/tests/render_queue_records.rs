//! Lane SMB-AE-MOTION-RECORDS slice 2b (feature `media`): a saved render-queue item (Folio
//! `StudioRenderQueueItem`: composition + render settings + two output modules) renders a saved
//! composition whose layer is a saved SHAPE LAYER (Folio motion records, keyframed rect position)
//! with a saved composition audio layer, through composite's composition evaluator and reel's
//! queue. Both files are decoded back: frame times, colours at hand-computed points, placed PCM.
#![cfg(feature = "media")]

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_composite::temporal::composition::{CompositionInput, CompositionTemporalSource};
use hsk_studio_composite::{BlendSpace, RenderInputs, ScenePixels, TileInfo};
use hsk_studio_folio::*;
use hsk_studio_harness::view::timeline_export::render_queue::{TemporalScenes, render_queue_jobs};
use hsk_studio_pigment::render_plan::{ColourTag, TilePlanes};
use hsk_studio_reel::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;

const S: u64 = 254_016_000_000;
const F: u64 = 10_160_640_000; // 25 fps
const SPF: u64 = 1920;
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

fn snapshot() -> Snapshot {
    match inspect_bytes(&serde_json::to_vec(&document()).unwrap(), Budget::default(), &CancellationToken::default(), &Available).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read-only"),
    }
}

fn compositions() -> StudioCompositionRecords {
    serde_json::from_value(json!({"schema_id":"hsk.studio.composition_records@1","document_id":identity("SDOC",1),
        "compositions":[{"schema_id":"hsk.studio.composition@1","composition_id":identity("SCOM",9),"name":"c","width":64,"height":32,
            "pixel_aspect":{"numerator":1,"denominator":1},"frame_rate":{"numerator":25,"denominator":1},"duration":S,
            "layers":[{"layer_id":identity("SLYR",2),"timing":{"in_point":0,"out_point":S,"start_time":0,"stretch":{"numerator":1,"denominator":1}}}]}]}))
    .unwrap()
}

fn module(id: &str, profile: &str, audio: &str, path: &str) -> Value {
    json!({"module_id":id,"format":"mov_vc3","codec_option":profile,"include_project_link":false,"post_render_action":"none",
        "include_source_metadata":false,"channels":"rgb","depth":8,"color":"straight","starting_number":0,"use_comp_frame_number":false,
        "audio":{"output":audio,"sample_rate":48000,"bit_depth":32,"float":true,"channels":2},
        "colour":{"preserve_rgb":false,"output_color_space":"rec709","embed_color_space":true},"output_path":path})
}

fn motion() -> Value {
    json!({"schema_id":"hsk.studio.motion_records@1","document_id":identity("SDOC",1),
        "shape_layers":[{"layer_id":identity("SLYR",2),"root":{"items":[
            {"kind":"rect","position":[8.0,8.0],"size":[16.0,16.0]},
            {"kind":"fill","rgba":[0.8,0.1,0.1,1.0],"opacity":100.0,"rule":"non_zero"}]},
            "streams":[{"path":[0],"field":"rect_position","value":{"static_value":[8.0,8.0],"keyframes":[
                {"time":0,"value":[8.0,8.0],"in_interp":"linear","out_interp":"linear"},
                {"time":S,"value":[40.0,8.0],"in_interp":"linear","out_interp":"linear"}]}}]}],
        "composition_audio":[{"composition_id":identity("SCOM",9),"layers":[{"audio_id":"tone","start_tick":2 * F,"gain":0.5}]}],
        "render_queue":[{"item_id":"queue-1","composition_id":identity("SCOM",9),
            "settings":{"quality":"best","resolution":"full","disk_cache":"current_settings","proxy_use":"current_settings","effects":"current_settings",
                "solo_switches":"current_settings","guide_layers":"current_settings","color_depth":"8_bits_per_channel","frame_blending":"current_settings",
                "field_render":"off","pulldown_3_2":"off","motion_blur":"current_settings","time_span":{"custom":{"start_tick":0,"end_tick":5 * F}}},
            "output_modules":[module("master","dnxhr_hq","on","renders/master.mov"),module("proxy","dnxhr_lb","off","renders/proxy.mov")]}]})
}

fn tone() -> Arc<Vec<f32>> {
    Arc::new((0..20_000u32).flat_map(|j| {
        let v = (j % 50) as f32 / 100.0 - 0.25;
        [v, -v]
    }).collect())
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

type Jobs = Result<Vec<(String, ExportJob)>, hsk_studio_harness::view::monitor::MonitorFail>;

fn jobs(motion_records: &StudioMotionRecords, root: &std::path::Path) -> Jobs {
    let snap = snapshot();
    let comps = compositions();
    let comp = comps.compositions[0].clone();
    let item = motion_records.render_queue[0].clone();
    let mut scenes = || {
        let id = identity("SCOM", 9);
        let source = CompositionTemporalSource::new(
            &CompositionInput {
                snapshot: &snap, expected_revision: 7, records: &comps, composition_id: &id, budget: Budget::default(),
                resolver: &Available, blend_space: BlendSpace::Encoded, info: &info, space: SPACE, inputs: RenderInputs::default(),
            },
            &CancellationToken::default(),
        )
        .and_then(|s| s.with_motion_records(motion_records, &BTreeMap::new(), SPACE, 7, None))
        .map_err(|e| hsk_studio_harness::view::monitor::MonitorFail { code: "scenes", detail: format!("{e:?}") })?;
        Ok(TemporalScenes { source, pixels: NoPixels })
    };
    let audio = |id: &str, rate: u32| (id == "tone" && rate == 48_000).then(tone);
    render_queue_jobs(&item, &comp, Some(motion_records), &mut scenes, &audio, SPACE, root, false)
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

fn read(bytes: &[u8], track: u32, count: u64) -> Collect {
    let request = SegmentRequest {
        codec: "isobmff".to_owned(),
        start_pts: Ticks::new(0),
        count,
        track: Some(track),
        output: OutputMode::Decoded,
        prefer: SourcePreference::Original,
        purpose: Purpose::Export,
        grant: Grant { content_id: ContentId::parse("render-queue").unwrap(), revision: 1, expected_len: bytes.len() as u64 },
        expected_revision: 1,
    };
    let mut sink = Collect::default();
    Registry::standard()
        .decode_segment(&mut Cursor::new(bytes.to_vec()), None, &request, &Limits::default(), &CancellationToken::default(), &mut sink)
        .expect("reel reads the export");
    sink
}

fn rgb(y: u8, cb: u8, cr: u8) -> [f32; 3] {
    let y = (f32::from(y) - 16.0) / 219.0;
    let cb = (f32::from(cb) - 128.0) / 224.0;
    let cr = (f32::from(cr) - 128.0) / 224.0;
    let r = y + 1.5748 * cr;
    let b = y + 1.8556 * cb;
    let g = (y - 0.2126 * r - 0.0722 * b) / 0.7152;
    [r, g, b]
}

#[test]
fn saved_render_queue_item_renders_a_saved_shape_layer_composition() {
    let records: StudioMotionRecords = serde_json::from_value(motion()).unwrap();
    validate_motion_records(&records, snapshot().document(), &compositions(), Budget::default(), &CancellationToken::default(), &Available)
        .expect("valid records");
    let root = std::env::temp_dir().join(format!("harness-render-queue-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("renders")).unwrap();
    let mut queue = ExportQueue::new();
    let built = jobs(&records, &root).unwrap();
    assert_eq!(built.iter().map(|(m, _)| m.as_str()).collect::<Vec<_>>(), vec!["master", "proxy"]);
    for (_, job) in built {
        queue.submit(job);
    }
    let receipts = queue.run();
    for r in &receipts {
        assert_eq!(r.outcome, ExportOutcome::Complete, "{r:?}");
        assert_eq!((r.span.start_ticks, r.span.end_ticks, r.frames_encoded), (0, 5 * F, 5));
    }
    assert_eq!((receipts[0].audio_frames, receipts[1].audio_frames), (5 * SPF, 0), "audio on / off");

    let (w, h) = (64usize, 32usize);
    for name in ["master.mov", "proxy.mov"] {
        let bytes = std::fs::read(root.join("renders").join(name)).unwrap();
        let video = read(&bytes, 1, 5);
        assert_eq!(video.chunks.len(), 5, "{name}");
        let mut worst = 0f32;
        for (n, (pts, frame)) in video.chunks.iter().enumerate() {
            assert_eq!(*pts, (n as u64 * F) as i64, "{name} frame {n} time");
            // rect_position x = 8 + 32 * n / 25 at frame n: centre pixel red, far corner black.
            let cx = (8.0 + 32.0 * n as f64 / 25.0).floor() as usize;
            let (yp, cb, cr) = (&frame[..w * h], &frame[w * h..w * h + w / 2 * h], &frame[w * h + w / 2 * h..]);
            for ((x, y), want) in [((cx, 8usize), [0.8f32, 0.1, 0.1]), ((60, 28), [0.0; 3])] {
                let got = rgb(yp[y * w + x], cb[y * (w / 2) + x / 2], cr[y * (w / 2) + x / 2]);
                for c in 0..3 {
                    worst = worst.max((got[c] - want[c]).abs());
                }
            }
        }
        println!("{name}: worst R'G'B' error {worst:.4}");
        // 8-bit limited range + VC-3 coding (LB is the coarsest profile).
        assert!(worst <= 0.05, "{name}: worst R'G'B' error {worst}");
    }
    // Master audio: the tone layer starts at composition sample 2 * 1920 with gain 0.5.
    let bytes = std::fs::read(root.join("renders/master.mov")).unwrap();
    let audio = read(&bytes, 2, 5 * SPF);
    let got: Vec<f32> = audio.chunks.iter().flat_map(|c| c.1.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect::<Vec<_>>()).collect();
    let t = tone();
    let want: Vec<f32> = (0..(5 * SPF) as usize)
        .flat_map(|k| if k < 3840 { [0.0, 0.0] } else { [0.5 * t[2 * (k - 3840)], 0.5 * t[2 * (k - 3840) + 1]] })
        .collect();
    assert_eq!(got.len(), want.len());
    assert_eq!(got.iter().zip(&want).position(|(a, b)| a != b), None, "placed PCM");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unsupported_render_settings_are_refused_before_rendering() {
    let root = std::env::temp_dir();
    let code = |edit: &dyn Fn(&mut Value)| {
        let mut v = motion();
        edit(&mut v);
        let r: StudioMotionRecords = serde_json::from_value(v).unwrap();
        jobs(&r, &root).err().map(|e| e.code)
    };
    assert_eq!(code(&|v| v["render_queue"][0]["settings"]["field_render"] = json!("upper_field_first")), Some("render_fields"));
    assert_eq!(code(&|v| v["render_queue"][0]["settings"]["time_span"] = json!({"custom":{"start_tick":0,"end_tick":100 * S}})), Some("render_span_range"));
    assert_eq!(code(&|v| v["render_queue"][0]["settings"]["frame_rate"] = json!({"numerator":30,"denominator":1})), Some("render_frame_rate_conversion"));
    assert_eq!(code(&|v| v["render_queue"][0]["output_modules"][0]["channels"] = json!("rgb_alpha")), Some("output_alpha_codec"));
    assert_eq!(code(&|v| v["render_queue"][0]["output_modules"][1]["colour"]["output_color_space"] = json!("rec2020")), Some("output_color_space"));
    assert_eq!(code(&|v| v["render_queue"][0]["output_modules"][0]["crop"] = json!({"top":0,"left":40,"bottom":0,"right":24})), Some("output_crop"));
    assert_eq!(code(&|v| v["composition_audio"][0]["layers"][0]["audio_id"] = json!("missing")), Some("output_audio_unavailable"));
}
