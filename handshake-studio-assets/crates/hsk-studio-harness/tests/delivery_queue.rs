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
use hsk_studio_harness::{Harness, References, Selection, asset_local_test_context};
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
    // A new PNG module is RGB+alpha, straight, 8 bits: only premultiplied colour is a change.
    let records: Value = serde_json::from_slice(v.run().document_entry("motion").unwrap()).unwrap();
    let module = &records["render_queue"][0]["output_modules"][0];
    assert_eq!((module["channels"].as_str(), module["color"].as_str(), module["depth"].as_u64()), (Some("rgb_alpha"), Some("straight"), Some(8)), "{module}");
    if colour != "straight" {
        motion(v, "composition.queue_set_module", json!({"item_id": "rq", "module_id": "png", "color": colour}));
    }
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

/// Solid 4x4 frames for any source; after `cancel_after` requests it cancels the item behind `handle` (the
/// program monitor's port is served by the delivery run's own thread, so this is a cancel mid-run).
struct CancelAfter {
    served: std::sync::atomic::AtomicU64,
    cancel_after: u64,
    handle: std::sync::Mutex<Option<hsk_studio_harness::view::delivery::DeliveryCancel>>,
}
impl CancelAfter {
    fn serve(&self) -> std::sync::Arc<hsk_studio_harness::view::monitor::SourceFrame> {
        let n = self.served.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        if n == self.cancel_after
            && let Some(h) = self.handle.lock().unwrap().as_ref()
        {
            h.cancel();
        }
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for _ in 0..16 {
            for c in [0.8f32, 0.2, 0.1] {
                colour.extend_from_slice(&c.to_le_bytes());
            }
            alpha.extend_from_slice(&1.0f32.to_le_bytes());
        }
        std::sync::Arc::new(hsk_studio_harness::view::monitor::SourceFrame { width: 4, height: 4, colour_sha256: hsk_studio_prism::profile_hash(&colour), alpha_sha256: hsk_studio_prism::profile_hash(&alpha), colour, alpha })
    }
}
struct Shared(std::sync::Arc<CancelAfter>);
impl hsk_studio_harness::view::monitor::FrameSource for Shared {
    fn frame(&self, _: &ClipSource, _: Ticks) -> Option<std::sync::Arc<hsk_studio_harness::view::monitor::SourceFrame>> {
        Some(self.0.serve())
    }
}
struct Long;
impl hsk_studio_pulse::SourceExtents for Long {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(1000 * F))
    }
}

/// A cancel WHILE the queue runs stops that item only: item 2 is canceled at its second frame (reel publishes
/// nothing, so no partial output is named), items 1 and 3 complete.
#[test]
fn cancelling_one_running_item_leaves_the_others() {
    let s = Scratch::new("cancel-running");
    let mut v = viewer(&s);
    let source = std::sync::Arc::new(CancelAfter { served: Default::default(), cancel_after: 5, handle: Default::default() });
    v.set_monitor_sources(Box::new(Shared(source.clone())), Box::new(Long));
    let mut ids = Vec::new();
    for (seq, out) in [("seq-main", "one.mov"), ("seq-main", "two.mov"), ("seq-b", "three.mov")] {
        ids.push(ok(&mut v, "delivery.add", json!({"kind": "sequence", "sequence_id": seq, "recipe_id": PRESET, "output": out, "width": 64, "height": 32}))["delivery_id"].as_u64().unwrap());
    }
    *source.handle.lock().unwrap() = v.delivery_cancel_handle(ids[1]);
    let run = ok(&mut v, "delivery.run", json!({}));
    let outcomes: Vec<&str> = run["receipts"].as_array().unwrap().iter().map(|r| r["outcome"].as_str().unwrap_or("?")).collect();
    assert_eq!(outcomes, ["complete", "canceled", "complete"], "{run}");
    let two = &run["receipts"][1];
    assert_eq!((two["partial"].as_bool(), two["artifacts"].as_array().map(Vec::len)), (Some(false), Some(0)), "{two}");
    assert_eq!(two["job"]["outcome"], "canceled", "{two}");
    assert!(two["receipt"]["frames_encoded"].as_u64().is_some_and(|n| n < 3), "stopped mid-item: {two}");
    assert!(!s.0.join("two.mov").exists() && s.0.join("one.mov").is_file() && s.0.join("three.mov").is_file());
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
    // Every value through reel's checks: a credential is never stored, bounds hold, a parameter the writer does
    // not consume is refused when the recipe is used, before the first frame.
    assert_eq!(refused(&mut v, "export.recipe_save", json!({"from": PRESET, "recipe_id": "user.c", "parameters": {"ADBEStockRefreshToken": "secret"}})), "export_credential_in_recipe_record");
    assert_eq!(refused(&mut v, "export.recipe_save", json!({"from": PRESET, "recipe_id": "user.n", "name": "n".repeat(257)})), "export_recipe_limit");
    assert_eq!(refused(&mut v, "export.recipe_save", json!({"from": PRESET, "recipe_id": "bad id"})), "export_recipe_field_invalid");
    assert_eq!(refused(&mut v, "export.recipe_save", json!({"from": PRESET, "recipe_id": PRESET})), "export_recipe_exists");
    ok(&mut v, "export.recipe_save", json!({"from": PRESET, "recipe_id": "user.fps", "parameters": {"ADBEVideoTargetBitrate": 10.0}}));
    assert_eq!(refused(&mut v, "delivery.add", json!({"kind": "sequence", "sequence_id": "seq-main", "recipe_id": "user.fps", "output": "fps.mov"})), "export_parameter_not_consumed");
    ok(&mut v, "export.recipe_delete", json!({"recipe_id": "user.fps"}));
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
    // MOV with alpha and post-render actions stay refused, typed, before any frame.
    motion(&mut v, "composition.queue_add", json!({"item_id": "rq-alpha", "composition_id": "T01", "module_id": "m", "format": "mov_vc3", "output_path": "renders/alpha.mov"}));
    motion(&mut v, "composition.queue_set_module", json!({"item_id": "rq-alpha", "module_id": "m", "channels": "rgb_alpha"}));
    assert_eq!(refused(&mut v, "delivery.add", json!({"kind": "composition", "item_id": "rq-alpha"})), "output_alpha_codec");
    motion(&mut v, "composition.queue_set_module", json!({"item_id": "rq-alpha", "module_id": "m", "channels": "rgb", "post_render_action": "import"}));
    assert_eq!(refused(&mut v, "delivery.add", json!({"kind": "composition", "item_id": "rq-alpha"})), "output_post_render");
    assert!(!s.0.join("renders/alpha.mov").exists());
    // 4096 items fit; the 4097th is refused.
    for n in 0..4096 {
        ok(&mut v, "delivery.add", json!({"kind": "sequence", "sequence_id": "seq-main", "recipe_id": PRESET, "output": format!("q/{n}.mov")}));
    }
    assert_eq!(refused(&mut v, "delivery.add", json!({"kind": "sequence", "sequence_id": "seq-main", "recipe_id": PRESET, "output": "q/x.mov"})), "delivery_queue_full");
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

// -------------------------------------------------------------------------------------------------
// Review 44da1364 findings F1-F7 (lead rulings 2026-11-11, research/lead-rulings-20261108-by-lane.json).
// -------------------------------------------------------------------------------------------------

/// `pkg` reopened through the harness run with the document entry `export_recipes` replaced by `body`, stored by
/// the session as is (another writer's or a damaged entry: the viewer's producer checks are bypassed), saved as
/// `name` beside it.
fn with_recipes_entry(s: &Scratch, pkg: &Path, name: &str, body: &[u8]) -> PathBuf {
    let harness = Harness::new(Selection { providers: vec![], references: References::TrustAll }).unwrap();
    let cancel = CancellationToken::default();
    let mut run = harness.run(asset_local_test_context(), &cancel);
    assert!(run.open_path(pkg), "{}", run.finish("open"));
    run.set_document_entry("export_recipes", Some(body.to_vec())).unwrap_or_else(|e| panic!("set_document_entry: {e}"));
    let path = s.0.join(name);
    assert!(run.save_to(&path), "{}", run.finish("save"));
    path
}

/// The viewer on `path`, the fixture bound to the program monitor at 64x32.
fn viewer_at(path: &Path) -> Viewer {
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(path).unwrap_or_else(|e| panic!("{e}"));
    v.bind_program_media("seq-main", vec![DNX.to_vec()], 64, 32).unwrap_or_else(|e| panic!("{} {}", e.code, e.detail));
    v
}

/// A recipe entry body (`hsk.studio.export_recipes@1`) of `recipes` (JSON texts).
fn recipes_entry(recipes: &[String]) -> Vec<u8> {
    format!(r#"{{"schema_id":"hsk.studio.export_recipes@1","recipes":[{}]}}"#, recipes.join(",")).into_bytes()
}

/// The JSON of shipped preset `preset` as user recipe `id`, `edit` applied.
fn preset_json(preset: &str, id: &str, edit: &dyn Fn(&mut Value)) -> String {
    let mut v: Value = serde_json::from_str(&hsk_studio_reel::presets::export_preset(preset).unwrap().studio_recipe().to_json()).unwrap();
    v["recipe_id"] = json!(id);
    edit(&mut v);
    serde_json::to_string(&v).unwrap()
}

/// F1: an entry this build cannot read (here a newer writer's schema) never blocks a shipped preset: export_sequence,
/// delivery.add and the run keep working; only a user recipe id refuses `export_recipes_invalid`, and the listing
/// still shows the shipped presets with the entry marked unreadable.
#[test]
fn an_unreadable_recipe_entry_never_blocks_a_shipped_preset() {
    let s = Scratch::new("f1-unreadable");
    let pkg = package(&s.0);
    let path = with_recipes_entry(&s, &pkg, "newer.handshake", br#"{"schema_id":"hsk.studio.export_recipes@2","recipes":[]}"#);
    let mut v = viewer_at(&path);
    let mut request = ExportSequenceRequest::new(PRESET, s.0.join("shipped.mov"));
    request.width = 64;
    request.height = 32;
    let result = v.export_sequence("seq-main", request).unwrap_or_else(|e| panic!("{} {}", e.code, e.detail));
    assert_eq!(result["receipt"]["outcome"], "complete", "{result}");
    ok(&mut v, "delivery.add", json!({"kind": "sequence", "sequence_id": "seq-main", "recipe_id": PRESET, "output": "queued.mov", "width": 64, "height": 32}));
    assert_eq!(refused(&mut v, "delivery.add", json!({"kind": "sequence", "sequence_id": "seq-main", "recipe_id": "user.any", "output": "user.mov"})), "export_recipes_invalid");
    let run = ok(&mut v, "delivery.run", json!({}));
    assert_eq!(run["receipts"][0]["outcome"], "complete", "{run}");
    let listed = ok(&mut v, "export.recipes", json!({}));
    assert!(listed["shipped"].as_array().unwrap().iter().any(|r| r["recipe_id"] == PRESET), "{listed}");
    assert_eq!((listed["user_entry"]["readable"].as_bool(), listed["user_entry"]["code"].as_str()), (Some(false), Some("export_recipes_invalid")), "{listed}");
    // The whole unreadable entry can be removed (one undo step), so the user can repair the document.
    ok(&mut v, "export.recipe_delete", json!({"all": true}));
    assert!(v.run().document_entry("export_recipes").is_none());
    assert_eq!(ok(&mut v, "export.recipes", json!({}))["user_entry"]["readable"], true);
}

/// The text value reel stores up to 4096 bytes with no character rule; control characters serialize as 6 bytes.
fn big_recipe(id: &str) -> String {
    let wide = "\u{1}".repeat(4096);
    preset_json(PRESET, id, &|v| {
        v["parameters"]["0"] = json!(wide);
        v["parameters"]["ADBEAudioChannelConfiguration"] = json!(wide);
    })
}

/// F2: one 8 MiB bound for save and read. A save that would cross it refuses typed and changes nothing; an entry
/// over it (another writer's) still lists and deletes recipe by recipe until it reads again.
#[test]
fn recipe_entry_size_bound_is_one_for_save_and_read_and_an_oversized_entry_is_repairable() {
    const LIMIT: usize = 8 << 20;
    let s = Scratch::new("f2-size");
    let pkg = package(&s.0);
    let one = big_recipe("user.big.000").len();
    let header = recipes_entry(&[]).len();
    let fit = (LIMIT - header + 1) / (one + 1);
    assert!(fit < 255, "{fit} recipes fit");
    let ids: Vec<String> = (0..fit + 2).map(|n| format!("user.big.{n:03}")).collect();
    let under: Vec<String> = ids[..fit].iter().map(|id| big_recipe(id)).collect();
    let body = recipes_entry(&under);
    assert!(body.len() <= LIMIT && body.len() + one + 1 > LIMIT, "{} bytes", body.len());
    let path = with_recipes_entry(&s, &pkg, "full.handshake", &body);
    let mut v = viewer_at(&path);
    assert_eq!(ok(&mut v, "export.recipes", json!({}))["user"].as_array().map(Vec::len), Some(fit));
    assert_eq!(refused(&mut v, "export.recipe_save", json!({"from": "user.big.000", "recipe_id": "user.big.zzz"})), "export_recipes_full");
    assert_eq!(v.run().document_entry("export_recipes").map(<[u8]>::len), Some(body.len()), "a refused save changes nothing");
    // Over the bound: listed (shipped plus the entry state) and repaired by deletes.
    let over: Vec<String> = ids.iter().map(|id| big_recipe(id)).collect();
    let path = with_recipes_entry(&s, &pkg, "over.handshake", &recipes_entry(&over));
    let mut w = viewer_at(&path);
    let listed = ok(&mut w, "export.recipes", json!({}));
    assert_eq!(listed["user_entry"]["readable"], false, "{}", listed["user_entry"]);
    assert!(listed["shipped"].as_array().is_some_and(|l| !l.is_empty()));
    assert_eq!(refused(&mut w, "export.recipe_save", json!({"from": PRESET, "recipe_id": "user.new"})), "export_recipes_invalid");
    ok(&mut w, "export.recipe_delete", json!({"recipe_id": ids[fit + 1]}));
    ok(&mut w, "export.recipe_delete", json!({"recipe_id": ids[fit]}));
    let listed = ok(&mut w, "export.recipes", json!({}));
    assert_eq!((listed["user_entry"]["readable"].as_bool(), listed["user"].as_array().map(Vec::len)), (Some(true), Some(fit)), "{}", listed["user_entry"]);
}

/// A full-frame opaque mid-grey composition (linear 0.5) with render queue item `rq`, PNG module `png` at
/// `renders/grey.png`, its colour group `colour`.
fn author_grey(v: &mut Viewer, colour: Value) {
    motion(v, "composition.new", json!({"composition_id": "G01", "name": "G01", "width": 16, "height": 8, "frame_rate": {"numerator": 25, "denominator": 1}, "duration": F, "background": {"r": 0, "g": 0, "b": 0, "a": 0, "colour_profile_id": null}}));
    motion(v, "composition.add_layer", json!({"composition_id": "G01", "kind": "shape", "root": {"items": [
        {"kind": "rect", "position": [8.0, 4.0], "size": [16.0, 8.0]},
        {"kind": "fill", "rgba": [0.5, 0.5, 0.5, 1.0], "opacity": 100.0, "rule": "non_zero"}]}}));
    motion(v, "composition.queue_add", json!({"item_id": "rq", "composition_id": "G01", "module_id": "png", "format": "png_sequence", "output_path": "renders/grey.png"}));
    motion(v, "composition.queue_set_module", json!({"item_id": "rq", "module_id": "png", "colour": colour}));
}

/// The data of the first `cICP` chunk of a PNG.
fn cicp(bytes: &[u8]) -> Option<[u8; 4]> {
    let at = bytes.windows(4).position(|w| w == b"cICP")?;
    bytes.get(at + 4..at + 8)?.try_into().ok()
}

/// F3: PNG frames apply the transfer of the module's colour space (linear 0.5 -> sRGB 188, BT.709 180) and carry the
/// matching chunk (iCCP sRGB, cICP 1/1/0/1) when `embed_color_space` is set; without it no colour chunk.
#[test]
fn png_frames_apply_the_colour_space_transfer_and_tag_it() {
    let cases = [("srgb", true, 188u8), ("rec709", true, 180), ("srgb", false, 188), ("rec709", false, 180)];
    for (space, embed, want) in cases {
        let s = Scratch::new(&format!("f3-{space}-{embed}"));
        std::fs::create_dir_all(s.0.join("renders")).unwrap();
        let mut v = viewer(&s);
        author_grey(&mut v, json!({"preserve_rgb": false, "output_color_space": space, "embed_color_space": embed}));
        ok(&mut v, "delivery.add", json!({"kind": "composition", "item_id": "rq"}));
        let run = ok(&mut v, "delivery.run", json!({}));
        assert_eq!(run["receipts"][0]["outcome"], "complete", "{run}");
        let file = s.0.join("renders/grey_0000000.png");
        let img = decode_png(&file);
        let px = rgba8(&img, 8, 4);
        for c in 0..3 {
            assert!((i32::from(px[c]) - i32::from(want)).abs() <= 1, "{space} embed={embed}: {px:?} vs {want}");
        }
        assert_eq!(px[3], 255);
        let bytes = std::fs::read(&file).unwrap();
        match (space, embed) {
            ("srgb", true) => assert!(img.icc.is_some() && cicp(&bytes).is_none(), "{space}: iCCP sRGB"),
            ("rec709", true) => assert_eq!((cicp(&bytes), img.icc.is_some()), (Some([1, 1, 0, 1]), false), "{space}: cICP BT.709"),
            _ => assert_eq!((cicp(&bytes), img.icc.is_some()), (None, false), "{space}: embed off, no colour chunk"),
        }
    }
}

/// F4: an output-module template keeps every AV1 setting of the recipe, and the output path's extension follows the
/// template's container.
#[test]
fn an_output_module_template_keeps_av1_settings_and_the_container_extension() {
    let s = Scratch::new("f4-template");
    let pkg = package(&s.0);
    let av1 = preset_json("studio.av1.mp4", "user.av1.slow", &|v| {
        v["video"]["speed"] = json!(2);
        v["video"]["bitrate"] = json!(5_000_000);
        v["video"]["max_keyframe_interval"] = json!(60);
    });
    let quantizer = serde_json::from_str::<Value>(&av1).unwrap()["video"]["quantizer"].as_u64().unwrap();
    let hq = preset_json("studio.dnxhr.hq.mov", "user.hq", &|_| {});
    let path = with_recipes_entry(&s, &pkg, "templates.handshake", &recipes_entry(&[av1, hq]));
    let mut v = viewer_at(&path);
    author(&mut v, "straight");
    motion(&mut v, "composition.queue_apply_template", json!({"item_id": "rq", "module_id": "png", "template": "output_module", "recipe_id": "user.av1.slow"}));
    let records: Value = serde_json::from_slice(v.run().document_entry("motion").unwrap()).unwrap();
    let module = &records["render_queue"][0]["output_modules"][0];
    let want = format!("{quantizer},speed=2,bitrate=5000000,keyframe=60");
    assert_eq!(
        (module["format"].as_str(), module["codec_option"].as_str(), module["output_path"].as_str()),
        (Some("mp4_av1"), Some(want.as_str()), Some("renders/title.mp4")),
        "{module}"
    );
    motion(&mut v, "composition.queue_apply_template", json!({"item_id": "rq", "module_id": "png", "template": "output_module", "recipe_id": "user.hq"}));
    let records: Value = serde_json::from_slice(v.run().document_entry("motion").unwrap()).unwrap();
    let module = &records["render_queue"][0]["output_modules"][0];
    assert_eq!((module["format"].as_str(), module["codec_option"].as_str(), module["output_path"].as_str()), (Some("mov_vc3"), Some("dnxhr_hq"), Some("renders/title.mov")), "{module}");
}

/// F5: outputs equal on Windows are one output: trailing dots and spaces, case, and a sequence output equal to one of a
/// PNG module's frame names refuse the run before any write.
#[test]
fn windows_equivalent_outputs_collide_before_any_write() {
    let s = Scratch::new("f5-collide");
    std::fs::create_dir_all(s.0.join("renders")).unwrap();
    let mut v = viewer(&s);
    author(&mut v, "straight");
    let item = |out: &str| json!({"kind": "sequence", "sequence_id": "seq-main", "recipe_id": PRESET, "output": out, "width": 64, "height": 32, "overwrite": true});
    for (a, b) in [("a.mov", "a.mov."), ("b.mov", "B.MOV "), ("renders/title_0000001.png", "")] {
        ok(&mut v, "delivery.add", item(a));
        if b.is_empty() {
            ok(&mut v, "delivery.add", json!({"kind": "composition", "item_id": "rq", "overwrite": true}));
        } else {
            ok(&mut v, "delivery.add", item(b));
        }
        assert_eq!(refused(&mut v, "delivery.run", json!({})), "delivery_output_collision", "{a} / {b}");
        let listed = ok(&mut v, "delivery.list", json!({}));
        for queued in listed["items"].as_array().unwrap() {
            ok(&mut v, "delivery.remove", json!({"delivery_id": queued["delivery_id"]}));
        }
    }
    let written: Vec<_> = std::fs::read_dir(&s.0).unwrap().filter_map(Result::ok).map(|e| e.file_name()).filter(|n| n.to_string_lossy().to_ascii_lowercase().contains(".mov")).collect();
    assert!(written.is_empty(), "nothing written: {written:?}");
    assert_eq!(std::fs::read_dir(s.0.join("renders")).unwrap().count(), 0);
}

/// F6: a PNG receipt names the frame pattern and the range written, not one entry per frame.
#[test]
fn png_receipt_names_the_pattern_and_range_not_each_frame() {
    let s = Scratch::new("f6-receipt");
    std::fs::create_dir_all(s.0.join("renders")).unwrap();
    let mut v = viewer(&s);
    author(&mut v, "straight");
    ok(&mut v, "delivery.add", json!({"kind": "composition", "item_id": "rq"}));
    let run = ok(&mut v, "delivery.run", json!({}));
    let r = &run["receipts"][0];
    assert_eq!(r["artifacts"], json!([{"pattern": "renders/title_%07d.png", "first_number": 0, "last_number": 2, "frames": 3}]), "{r}");
    assert_eq!((r["receipt"]["pattern"].as_str(), r["receipt"]["frames_written"].as_u64()), (Some("renders/title_%07d.png"), Some(3)), "{r}");
    assert!(!r.to_string().contains("title_0000001.png"), "no per-frame name: {r}");
}
