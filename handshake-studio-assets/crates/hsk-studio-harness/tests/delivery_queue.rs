//! Lane SMB-DELIVERY-QUEUE-1 (feature `media`; STU-VID-063, STU-VID-064, STU-MOT-122, STU-MOT-124,
//! STU-MOT-125, STU-IO-110): one headless delivery queue for sequence recipes and composition render
//! queue items, user export recipes stored in the document entry `export_recipes`, and a PNG image
//! sequence with alpha. Every row runs through `Viewer::run_command` (the typed surface the process
//! drives) or `Viewer::motion_command`; records are JSON. Media: reel's committed DNxHR LB fixture
//! (264x120, 25 fps, 3 frames; provenance in `hsk-studio-reel/tests/fixtures/dnx/`), read through the
//! crate tree; every file lives under `std::env::temp_dir()` in a per-test folder, no-clobber.
//! Oracles: frame and sample counts from the range, the fixture's pinned export digest, hand-computed
//! 8-bit PNG samples of a half-transparent fill (straight 0.8 -> 204, premultiplied 0.4 -> 102, alpha
//! 0.5 -> 128). After Effects / Media Encoder parity of naming, padding and template mapping is
//! UNVERIFIED (D5).
#![cfg(feature = "media")]

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness::cli;
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::timeline::ExportSequenceRequest;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// 25 fps ticks per frame; one second.
const F: u64 = 10_160_640_000;
const S: u64 = 254_016_000_000;
const DNX: &[u8] = include_bytes!("../../hsk-studio-reel/tests/fixtures/dnx/dnxhr-lb-264x120.mov");
const PRESET: &str = "studio.dnxhr.lb.mov";

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-harness-delivery-queue-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Any;
impl folio::Resolver for Any {
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
    fn media(&self, _: &str, _: &ContentDigest) -> Resolution {
        Resolution::Available
    }
}

/// The fixture's clip source as `Viewer::bind_program_media` names it.
fn dnx_source() -> ClipSource {
    let digest = hsk_studio_reel::full_digest(&mut std::io::Cursor::new(DNX), 1 << 20, &CancellationToken::default()).unwrap();
    let hex = hsk_studio_reel::hex(&digest);
    ClipSource::Media { artifact_manifest_id: format!("sha256:{hex}"), content_digest: ContentDigest { algorithm: "sha256".into(), digest: hex }, asset_id: None }
}

/// `seq-main` (and its copy `seq-b`): V1 plays the fixture's 3 frames at timeline frames [0, 3); no audio.
fn package(dir: &Path) -> PathBuf {
    let clip = StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: "SCLP-019abcde-0000-7000-8000-000000000001".into(),
        source_ref: dnx_source(),
        source_in: Ticks::new(0),
        source_out: Ticks::new(3 * F),
        timeline_start: Ticks::new(0),
        duration: Ticks::new(3 * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
        retime: Default::default(),
    };
    let track = StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: "STRK-019abcde-0000-7000-8000-000000000001".into(),
        kind: TrackKind::Video,
        index: 0,
        name: "V1".into(),
        enabled: true,
        locked: false,
        sync_locked: true,
        targeted: true,
        muted: false,
        soloed: false,
        clips: vec![clip.clone()],
        volume: 1.0,
        pan: 0.0,
    };
    let sequence = |id: &str, n: u32| StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: id.into(),
        name: id.into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(3 * F),
        tracks: vec![StudioTrack {
            track_id: format!("STRK-019abcde-0000-7000-8000-00000000001{n}"),
            clips: vec![StudioClip { clip_id: format!("SCLP-019abcde-0000-7000-8000-00000000001{n}"), ..clip.clone() }],
            ..track.clone()
        }],
        markers: Vec::new(),
        work_area: None,
    };
    let doc = json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000009","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[serde_json::to_value(sequence("seq-main", 1)).unwrap(), serde_json::to_value(sequence("seq-b", 2)).unwrap()]
    });
    let token = CancellationToken::default();
    let snapshot = match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), Budget::default(), &token, &Any).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read-only fixture"),
    };
    let assets = hsk_studio_package::MemoryAssetSource::new();
    let mut bytes = Vec::new();
    hsk_studio_package::write_package_with(
        &snapshot,
        &assets as &dyn hsk_studio_package::AssetSource,
        &hsk_studio_package::Extras { quarantined: &[], previews: &[] },
        &hsk_studio_package::WriteOptions::default(),
        &hsk_studio_package::Limits::default(),
        &token,
        &mut bytes,
    )
    .unwrap();
    let path = dir.join("delivery.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

/// The package open (relative outputs land beside it), the fixture bound to the program monitor at 64x32.
fn viewer(s: &Scratch) -> Viewer {
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&package(&s.0)).unwrap_or_else(|e| panic!("{e}"));
    v.bind_program_media("seq-main", vec![DNX.to_vec()], 64, 32).unwrap_or_else(|e| panic!("{} {}", e.code, e.detail));
    v
}

fn params(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

/// A typed row: its result, or its refusal code.
fn row(v: &mut Viewer, id: &str, p: Value) -> Result<Value, String> {
    v.run_command(id, &params(p)).map_err(|r| r.code)
}
fn ok(v: &mut Viewer, id: &str, p: Value) -> Value {
    v.run_command(id, &params(p.clone())).unwrap_or_else(|r| panic!("{id} {p}: {} {}", r.code, r.detail))
}
fn refused(v: &mut Viewer, id: &str, p: Value) -> String {
    match row(v, id, p.clone()) {
        Ok(r) => panic!("{id} {p} was accepted: {r}"),
        Err(code) => code,
    }
}
fn motion(v: &mut Viewer, id: &str, p: Value) {
    v.motion_command(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"));
}

/// Composition `T01` (64x32, 25 fps, 3 frames, transparent background) holding one half-transparent red
/// 16x16 square at (0, 0), and render queue item `rq` with one PNG sequence module `png` at
/// `renders/title.png`.
fn author(v: &mut Viewer, colour: &str) {
    motion(v, "composition.new", json!({"composition_id": "T01", "name": "T01", "width": 64, "height": 32, "frame_rate": {"numerator": 25, "denominator": 1}, "duration": 3 * F, "background": {"r": 0, "g": 0, "b": 0, "a": 0, "colour_profile_id": null}}));
    motion(v, "composition.add_layer", json!({"composition_id": "T01", "kind": "shape", "root": {"items": [
        {"kind": "rect", "position": [8.0, 8.0], "size": [16.0, 16.0]},
        {"kind": "fill", "rgba": [0.8, 0.1, 0.1, 0.5], "opacity": 100.0, "rule": "non_zero"}]}}));
    motion(v, "composition.queue_add", json!({"item_id": "rq", "composition_id": "T01", "module_id": "png", "format": "png_sequence", "output_path": "renders/title.png"}));
    motion(v, "composition.queue_set_module", json!({"item_id": "rq", "module_id": "png", "channels": "rgb_alpha", "color": colour, "depth": 8}));
}

fn decode_png(path: &Path) -> hsk_studio_interop_raster::RasterImage {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    hsk_studio_interop_raster::decode(&bytes, &hsk_studio_interop_raster::Limits::default(), &CancellationToken::default()).unwrap()
}
fn rgba8(img: &hsk_studio_interop_raster::RasterImage, x: usize, y: usize) -> [u8; 4] {
    let hsk_studio_interop_raster::Samples::U8(s) = &img.samples else { panic!("8-bit expected") };
    let at = (y * img.width as usize + x) * 4;
    [s[at], s[at + 1], s[at + 2], s[at + 3]]
}

/// Item 2: one queue, a sequence recipe item and a composition render queue item, one receipt each
/// naming recipe, span, artifacts and outcome; the jobs settle through the job admission.
#[test]
fn one_queue_runs_a_sequence_recipe_and_a_composition_item_with_one_receipt_each() {
    let s = Scratch::new("both");
    std::fs::create_dir_all(s.0.join("renders")).unwrap();
    let mut v = viewer(&s);
    author(&mut v, "straight");
    let a = ok(&mut v, "delivery.add", json!({"kind": "sequence", "sequence_id": "seq-main", "recipe_id": PRESET, "output": "main.mov", "width": 64, "height": 32}));
    let b = ok(&mut v, "delivery.add", json!({"kind": "composition", "item_id": "rq"}));
    assert_ne!(a["delivery_id"], b["delivery_id"], "{a} {b}");
    let listed = ok(&mut v, "delivery.list", json!({}));
    assert_eq!(listed["items"].as_array().map(Vec::len), Some(2), "{listed}");
    let jobs_before = v.job_counts();
    let run = ok(&mut v, "delivery.run", json!({}));
    let receipts = run["receipts"].as_array().unwrap_or_else(|| panic!("{run}"));
    assert_eq!(receipts.len(), 2, "one receipt per item and module: {run}");
    let seq = &receipts[0];
    assert_eq!((seq["kind"].as_str(), seq["outcome"].as_str(), seq["recipe_id"].as_str()), (Some("sequence"), Some("complete"), Some(PRESET)), "{seq}");
    assert_eq!((seq["span"]["start_ticks"].as_u64(), seq["span"]["end_ticks"].as_u64()), (Some(0), Some(3 * F)), "{seq}");
    assert_eq!(seq["receipt"]["frames_encoded"].as_u64(), Some(3), "{seq}");
    assert!(s.0.join("main.mov").is_file());
    let comp = &receipts[1];
    assert_eq!((comp["kind"].as_str(), comp["module_id"].as_str(), comp["outcome"].as_str()), (Some("composition"), Some("png"), Some("complete")), "{comp}");
    let files: Vec<&str> = comp["artifacts"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert_eq!(files, ["renders/title_0000000.png", "renders/title_0000001.png", "renders/title_0000002.png"], "{comp}");
    for f in &files {
        assert!(s.0.join(f).is_file(), "{f}");
    }
    for r in receipts {
        assert_eq!(r["job"]["outcome"].as_str(), Some("completed"), "settled through the admission: {r}");
    }
    let jobs = v.job_counts();
    assert_eq!(jobs.completed.saturating_sub(jobs_before.completed), 2, "{jobs:?}");
    assert_eq!(ok(&mut v, "delivery.receipts", json!({}))["receipts"], run["receipts"]);
}

/// STU-VID-064 "one queue with per-item receipts": cancelling item 2 leaves items 1 and 3.
#[test]
fn cancelling_one_item_leaves_the_others() {
    let s = Scratch::new("cancel");
    let mut v = viewer(&s);
    let mut ids = Vec::new();
    for (seq, out) in [("seq-main", "one.mov"), ("seq-main", "two.mov"), ("seq-b", "three.mov")] {
        ids.push(ok(&mut v, "delivery.add", json!({"kind": "sequence", "sequence_id": seq, "recipe_id": PRESET, "output": out, "width": 64, "height": 32}))["delivery_id"].clone());
    }
    ok(&mut v, "delivery.cancel", json!({"delivery_id": ids[1]}));
    let run = ok(&mut v, "delivery.run", json!({}));
    let outcomes: Vec<&str> = run["receipts"].as_array().unwrap().iter().map(|r| r["outcome"].as_str().unwrap_or("?")).collect();
    assert_eq!(outcomes, ["complete", "canceled", "complete"], "{run}");
    assert!(s.0.join("one.mov").is_file() && s.0.join("three.mov").is_file());
    assert!(!s.0.join("two.mov").exists(), "a canceled item writes nothing");
    assert_eq!(run["receipts"][1]["job"]["outcome"].as_str(), Some("canceled"), "{run}");
}

/// `export_sequence` keeps `preset_id` (a recipe id alias) and writes the same bytes through the one
/// queue path: digest pinned from the output at the lane's base eb17fad6.
#[test]
fn export_sequence_keeps_its_preset_id_and_its_bytes() {
    let s = Scratch::new("pin");
    let mut v = viewer(&s);
    let mut request = ExportSequenceRequest::new(PRESET, s.0.join("pinned.mov"));
    request.width = 64;
    request.height = 32;
    let result = v.export_sequence("seq-main", request).unwrap_or_else(|e| panic!("{} {}", e.code, e.detail));
    assert_eq!(result["receipt"]["outcome"], "complete", "{result}");
    assert_eq!(result["preset_id"], PRESET);
    assert_eq!(result["receipt"]["output_sha256"].as_str(), Some(PINNED_SHA256), "{result}");
}
const PINNED_SHA256: &str = "afbdd389d74c3093cfb79120c6453c249aa0f1ba4e5013585f0a05f32a4e4e58";

/// Item 4: a PNG sequence with alpha, straight and premultiplied colour, 8 bit, seven-digit names.
#[test]
fn png_sequence_writes_straight_or_premultiplied_rgba() {
    for (colour, want) in [("straight", [204u8, 26, 26, 128]), ("premultiplied", [102, 13, 13, 128])] {
        let s = Scratch::new(&format!("png-{colour}"));
        std::fs::create_dir_all(s.0.join("renders")).unwrap();
        let mut v = viewer(&s);
        author(&mut v, colour);
        ok(&mut v, "delivery.add", json!({"kind": "composition", "item_id": "rq"}));
        let run = ok(&mut v, "delivery.run", json!({}));
        assert_eq!(run["receipts"][0]["outcome"], "complete", "{run}");
        let img = decode_png(&s.0.join("renders/title_0000001.png"));
        assert_eq!((img.width, img.height), (64, 32));
        let inside = rgba8(&img, 8, 8);
        for c in 0..4 {
            assert!((i32::from(inside[c]) - i32::from(want[c])).abs() <= 2, "{colour}: {inside:?} vs {want:?}");
        }
        assert_eq!(rgba8(&img, 40, 24), [0, 0, 0, 0], "{colour}: transparent background");
    }
}

/// A PNG module is no-clobber per file: an entry already at frame 1's name stops the item there, the
/// receipt names the frames written (frame 0) and the code; nothing is replaced.
#[test]
fn png_sequence_names_the_frames_written_when_a_write_fails() {
    let s = Scratch::new("png-blocked");
    std::fs::create_dir_all(s.0.join("renders/title_0000001.png")).unwrap();
    let mut v = viewer(&s);
    author(&mut v, "straight");
    ok(&mut v, "delivery.add", json!({"kind": "composition", "item_id": "rq"}));
    let run = ok(&mut v, "delivery.run", json!({}));
    let r = &run["receipts"][0];
    assert_eq!(r["outcome"], "failed", "{run}");
    assert_eq!(r["artifacts"], json!(["renders/title_0000000.png"]), "{run}");
    assert!(s.0.join("renders/title_0000001.png").is_dir(), "the blocking entry is untouched");
}

/// Item 3: user recipes are the same record as shipped ones, stored in the document entry
/// `export_recipes`, saved with the package; every value is checked by reel's `set_parameter`.
#[test]
fn user_recipes_save_list_reopen_and_delete() {
    let s = Scratch::new("recipes");
    let mut v = viewer(&s);
    let saved = ok(&mut v, "export.recipe_save", json!({"from": PRESET, "recipe_id": "user.lb.64x32", "name": "LB 64x32", "parameters": {"ADBEVideoWidth": 64, "ADBEVideoHeight": 32}}));
    assert_eq!((saved["recipe_id"].as_str(), saved["does_video"].as_bool(), saved["does_audio"].as_bool()), (Some("user.lb.64x32"), Some(true), Some(true)), "{saved}");
    let listed = ok(&mut v, "export.recipes", json!({}));
    assert!(listed["shipped"].as_array().unwrap().iter().any(|r| r["recipe_id"] == PRESET), "{listed}");
    assert_eq!(listed["user"].as_array().map(Vec::len), Some(1), "{listed}");
    assert_eq!(refused(&mut v, "export.recipe_save", json!({"from": PRESET, "recipe_id": "user.lb.64x32"})), "export_recipe_exists");
    assert_eq!(refused(&mut v, "export.recipe_save", json!({"from": PRESET, "recipe_id": "user.x", "parameters": {"NoSuchIdentifier": 1}})), "export_parameter_unknown");
    assert_eq!(refused(&mut v, "export.recipe_save", json!({"from": "no.such.recipe", "recipe_id": "user.y"})), "unknown_preset");
    // The user recipe drives a sequence item; its declared raster must equal the source's.
    ok(&mut v, "delivery.add", json!({"kind": "sequence", "sequence_id": "seq-main", "recipe_id": "user.lb.64x32", "output": "user.mov"}));
    let run = ok(&mut v, "delivery.run", json!({}));
    assert_eq!(run["receipts"][0]["outcome"], "complete", "{run}");
    assert_eq!(run["receipts"][0]["recipe_id"], "user.lb.64x32");
    // Saved with the package and read again on reopen.
    let saved_path = s.0.join("with-recipes.handshake");
    v.save(&saved_path, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&saved_path).unwrap();
    assert_eq!(ok(&mut w, "export.recipes", json!({}))["user"].as_array().map(Vec::len), Some(1));
    ok(&mut w, "export.recipe_delete", json!({"recipe_id": "user.lb.64x32"}));
    assert_eq!(ok(&mut w, "export.recipes", json!({}))["user"].as_array().map(Vec::len), Some(0));
    assert_eq!(refused(&mut w, "export.recipe_delete", json!({"recipe_id": "user.lb.64x32"})), "unknown_recipe");
    assert!(w.run().document_entry("export_recipes").is_none(), "the last delete removes the entry");
}

/// Item 5: reorder, duplicate and apply an output-module template; render-settings templates refuse typed.
#[test]
fn render_queue_reorder_duplicate_and_templates() {
    let s = Scratch::new("queue-commands");
    let mut v = viewer(&s);
    author(&mut v, "straight");
    motion(&mut v, "composition.queue_add", json!({"item_id": "rq2", "composition_id": "T01", "module_id": "master", "format": "mov_vc3", "output_path": "renders/master.mov"}));
    motion(&mut v, "composition.queue_reorder", json!({"item_id": "rq2", "index": 0}));
    motion(&mut v, "composition.queue_duplicate", json!({"item_id": "rq", "new_item_id": "rq-copy"}));
    let records: Value = serde_json::from_slice(v.run().document_entry("motion").unwrap()).unwrap();
    let ids: Vec<&str> = records["render_queue"].as_array().unwrap().iter().filter_map(|q| q["item_id"].as_str()).collect();
    assert_eq!(ids, ["rq2", "rq", "rq-copy"]);
    ok(&mut v, "export.recipe_save", json!({"from": "studio.dnxhr.hq.mov", "recipe_id": "user.hq"}));
    motion(&mut v, "composition.queue_apply_template", json!({"item_id": "rq2", "module_id": "master", "template": "output_module", "recipe_id": "user.hq"}));
    let records: Value = serde_json::from_slice(v.run().document_entry("motion").unwrap()).unwrap();
    let module = &records["render_queue"][0]["output_modules"][0];
    assert_eq!((module["format"].as_str(), module["codec_option"].as_str(), module["audio"]["output"].as_str()), (Some("mov_vc3"), Some("dnxhr_hq"), Some("on")), "{module}");
    let e = v.motion_command("composition.queue_apply_template", &json!({"item_id": "rq2", "template": "render_settings", "recipe_id": "user.hq"})).unwrap_err();
    assert!(e.contains("render_settings_template_unbuilt"), "{e}");
}

/// Hostile input: queue bound, relative-path rule, shared outputs, MOV alpha and post-render actions.
#[test]
fn hostile_items_refuse_before_any_frame() {
    let s = Scratch::new("hostile");
    std::fs::create_dir_all(s.0.join("renders")).unwrap();
    let mut v = viewer(&s);
    author(&mut v, "straight");
    for out in ["../escape.mov", "/abs.mov", "a/../b.mov"] {
        assert_eq!(refused(&mut v, "delivery.add", json!({"kind": "sequence", "sequence_id": "seq-main", "recipe_id": PRESET, "output": out})), "delivery_output_path", "{out}");
    }
    let absolute = s.0.join("abs.mov").to_string_lossy().into_owned();
    assert_eq!(refused(&mut v, "delivery.add", json!({"kind": "sequence", "sequence_id": "seq-main", "recipe_id": PRESET, "output": absolute})), "delivery_output_path");
    assert_eq!(refused(&mut v, "delivery.add", json!({"kind": "composition", "item_id": "nope"})), "render_queue_item");
    // Two items naming one output: refused before any frame, nothing written.
    ok(&mut v, "delivery.add", json!({"kind": "sequence", "sequence_id": "seq-main", "recipe_id": PRESET, "output": "same.mov", "width": 64, "height": 32}));
    ok(&mut v, "delivery.add", json!({"kind": "sequence", "sequence_id": "seq-b", "recipe_id": PRESET, "output": "same.mov", "width": 64, "height": 32}));
    assert_eq!(refused(&mut v, "delivery.run", json!({})), "delivery_output_collision");
    assert!(!s.0.join("same.mov").exists());
    let listed = ok(&mut v, "delivery.list", json!({}));
    for item in listed["items"].as_array().unwrap() {
        ok(&mut v, "delivery.remove", json!({"delivery_id": item["delivery_id"]}));
    }
    // 4096 items fit; the 4097th is refused.
    for n in 0..4096 {
        ok(&mut v, "delivery.add", json!({"kind": "sequence", "sequence_id": "seq-main", "recipe_id": PRESET, "output": format!("q/{n}.mov")}));
    }
    assert_eq!(refused(&mut v, "delivery.add", json!({"kind": "sequence", "sequence_id": "seq-main", "recipe_id": PRESET, "output": "q/x.mov"})), "delivery_queue_full");
    // MOV with alpha and post-render actions stay refused, typed, before any frame.
    motion(&mut v, "composition.queue_add", json!({"item_id": "rq-alpha", "composition_id": "T01", "module_id": "m", "format": "mov_vc3", "output_path": "renders/alpha.mov"}));
    motion(&mut v, "composition.queue_set_module", json!({"item_id": "rq-alpha", "module_id": "m", "channels": "rgb_alpha"}));
    assert_eq!(refused(&mut v, "delivery.add", json!({"kind": "composition", "item_id": "rq-alpha"})), "output_alpha_codec");
    motion(&mut v, "composition.queue_set_module", json!({"item_id": "rq-alpha", "module_id": "m", "channels": "rgb", "post_render_action": "import"}));
    assert_eq!(refused(&mut v, "delivery.add", json!({"kind": "composition", "item_id": "rq-alpha"})), "output_post_render");
    assert!(!s.0.join("renders/alpha.mov").exists());
}

/// Item 6: the headless CLI verb beside export-sequence (STU-IO-164).
#[test]
fn deliver_cli_runs_the_queue_headless() {
    let s = Scratch::new("cli");
    let pkg = package(&s.0);
    let media = s.0.join("dnxhr-lb.mov");
    std::fs::write(&media, DNX).unwrap();
    let item = json!({"kind": "sequence", "sequence_id": "seq-main", "recipe_id": PRESET, "output": "cli.mov", "width": 64, "height": 32}).to_string();
    let args: Vec<String> = ["deliver", pkg.to_str().unwrap(), "--item", &item, "--media", media.to_str().unwrap(), "--json"].iter().map(|a| (*a).to_string()).collect();
    let out = cli::run(&args);
    let json: Value = serde_json::from_str(&out.stdout).unwrap_or_else(|_| panic!("JSON: {}", out.stdout));
    assert_eq!(out.exit, 0, "{json}");
    assert_eq!(json["receipts"][0]["outcome"], "complete", "{json}");
    assert!(s.0.join("cli.mov").is_file());
    let _ = S;
}
