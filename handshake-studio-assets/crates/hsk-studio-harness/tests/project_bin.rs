//! Lane SMB-PROJECT-BIN-1 (feature `media`): New Project for video and audio work, headless. A sequence
//! from a shipped preset, add tracks (submix included), bin folders, a batch import with a typed refusal
//! per file, placement from the bin, undo and redo, and a saved project whose bin binds the program
//! monitor, the export sound and (under `audio-batch`) the timeline and mixer after reopen, from the
//! media bytes the package carries. Scenario steps: workflows/scenario-video-edit.json 2-6 and 25,
//! scenario-audio-post.json 6, 9-11, 19 and 56.
#![cfg(feature = "media")]

mod support_audio;

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use serde_json::Value;
use std::path::{Path, PathBuf};
use support_audio::{WavFormat, signal, wav};

const PRESET: &str = "legacy-arri-1080p-arri-1080p-24";
const TPF_24: u64 = 10_584_000_000;

/// A package holding one empty document (no sequence): what New Project starts from.
fn empty_package(dir: &Path) -> PathBuf {
    let doc = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-0000000000b1","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],"layers":[],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[]
    });
    let token = CancellationToken::default();
    let snapshot = match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), Budget::default(), &token, &Unresolved).unwrap() {
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
    let path = dir.join("last_ferry.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

/// A 16x16 4:2:0 Y4M file of `frames` frames at 24 fps.
fn y4m(frames: usize) -> Vec<u8> {
    let mut out = b"YUV4MPEG2 W16 H16 F24:1 Ip A1:1 C420\n".to_vec();
    for f in 0..frames {
        out.extend_from_slice(b"FRAME\n");
        out.extend(std::iter::repeat_n(u8::try_from(f % 200).unwrap(), 16 * 16 * 3 / 2));
    }
    out
}

/// The media of the scenario: camera (Y4M), dialogue at 48 kHz, music at 44.1 kHz, one file Studio
/// cannot read.
fn media(dir: &Path) -> Vec<PathBuf> {
    let files: [(&str, Vec<u8>); 4] = [
        ("cam_a.y4m", y4m(12)),
        ("boom.wav", wav(2, 48_000, WavFormat::Int16, None, &signal(48_000, 2))),
        ("music_bed.wav", wav(1, 44_100, WavFormat::Int16, None, &signal(44_100, 1))),
        ("unknown.bin", b"definitely not media at all".to_vec()),
    ];
    files
        .into_iter()
        .map(|(name, bytes)| {
            let p = dir.join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        })
        .collect()
}

fn undo_labels(v: &Viewer) -> Vec<String> {
    v.run().history()["undo"].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_owned()).collect()
}

fn revision(v: &Viewer) -> u64 {
    v.run().snapshot().unwrap().document().revision
}

fn sequence<'a>(v: &'a Viewer, id: &str) -> &'a StudioSequence {
    v.run().snapshot().unwrap().document().sequences.iter().find(|s| s.sequence_id == id).unwrap()
}

fn item_id(state: &Value, name: &str) -> String {
    state["project_bin"]["media_items"].as_array().unwrap().iter().find(|i| i["name"] == name).unwrap()["item_id"].as_str().unwrap().to_owned()
}

#[test]
fn new_project_sequence_tracks_bin_import_and_placement_are_named_undo_steps() {
    let dir = support_audio::scratch("project-bin-steps");
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&empty_package(&dir)).unwrap_or_else(|e| panic!("{e}"));
    let start = revision(&v);

    // Step 2-3: sequence from the preset; its V1-V3, A1-A4 and settings come with it, one step.
    let seq = v.project_new_sequence("main", PRESET).unwrap();
    let s = sequence(&v, &seq);
    assert_eq!(s.frame_rate.ticks_per_frame, TPF_24);
    let kinds: Vec<(TrackKind, &str)> = s.tracks.iter().map(|t| (t.kind, t.name.as_str())).collect();
    assert_eq!(kinds, [(TrackKind::Video, "V1"), (TrackKind::Video, "V2"), (TrackKind::Video, "V3"), (TrackKind::Audio, "A1"), (TrackKind::Audio, "A2"), (TrackKind::Audio, "A3"), (TrackKind::Audio, "A4")]);
    assert_eq!(revision(&v), start + 1);
    let settings: StudioSequenceSettingsRecords = serde_json::from_slice(v.run().document_entry("sequence_settings").unwrap()).unwrap();
    assert_eq!(settings.entry(&seq).unwrap().settings.frame_rate.ticks_per_frame, TPF_24);
    assert_eq!(v.project_new_sequence("x", "no-such-preset").unwrap_err(), "unknown_preset");

    // Audio-post 56: three submix tracks after the audio tracks, one step.
    let subs = v.project_add_tracks(&seq, TrackKind::Submix, &["SUB-DX".into(), "SUB-MX".into(), "SUB-FX".into()]).unwrap();
    let s = sequence(&v, &seq);
    assert_eq!(s.tracks.len(), 10);
    assert_eq!(s.tracks.iter().filter(|t| t.kind == TrackKind::Submix).map(|t| t.index).collect::<Vec<_>>(), [0, 1, 2]);
    assert_eq!(s.tracks[9].track_id, subs[2]);

    // Step 4-6: a folder, then one batch import; the unreadable file is a typed row, the rest imports.
    let sound = v.project_new_folder("Sound", None).unwrap();
    let paths = media(&dir);
    let result = v.project_import(&paths, Some(&sound)).unwrap();
    assert_eq!(result["imported"].as_array().unwrap().len(), 3, "{result}");
    let refused = &result["refused"][0];
    assert_eq!((refused["name"].as_str(), refused["code"].as_str()), (Some("unknown.bin"), Some("MEDIA_IMPORT_UNSUPPORTED")), "{result}");
    assert_eq!(refused["detail"]["detected"], "unknown");
    let again = v.project_import(&paths[1..2], None).unwrap();
    assert_eq!(again["refused"][0]["code"], "already_imported", "{again}");
    let state = v.state_json();
    let cam = item_id(&state, "cam_a.y4m");
    let boom = item_id(&state, "boom.wav");
    let rows = state["project_bin"]["media_items"].as_array().unwrap();
    let row = |id: &str| rows.iter().find(|r| r["item_id"] == id).unwrap().clone();
    assert_eq!((row(&cam)["kind"].as_str(), row(&cam)["width"].as_u64(), row(&cam)["ticks_per_frame"].as_u64()), (Some("video"), Some(16), Some(TPF_24)));
    assert_eq!((row(&boom)["kind"].as_str(), row(&boom)["sample_rate_hz"].as_u64(), row(&boom)["channels"].as_u64()), (Some("audio"), Some(48_000), Some(2)));
    assert_eq!(row(&boom)["duration"].as_u64(), Some(hsk_studio_accord::TICKS_PER_SECOND));
    assert_eq!(row(&boom)["folder_id"], sound.as_str());

    // Audio-post 10 / video-edit placement: the item's whole length at its start, revision +1 each.
    let s = sequence(&v, &seq);
    let (v1, a1) = (s.tracks[0].track_id.clone(), s.tracks[3].track_id.clone());
    let before = revision(&v);
    let audio_clip = v.project_place_item(&seq, &a1, &boom, Ticks::new(0), None, None).unwrap();
    v.project_place_item(&seq, &v1, &cam, Ticks::new(0), None, None).unwrap();
    assert_eq!(revision(&v), before + 2);
    assert_eq!(v.project_place_item(&seq, &v1, &boom, Ticks::new(0), None, None).unwrap_err(), "track_kind_mismatch");
    assert_eq!(v.project_place_item(&seq, &a1, &boom, Ticks::new(1), None, None).unwrap_err(), "clip_overlap");
    assert_eq!(sequence(&v, &seq).duration.value(), hsk_studio_accord::TICKS_PER_SECOND);
    // An item a clip cuts from stays in the bin.
    assert_eq!(v.project_remove_items(std::slice::from_ref(&boom)).unwrap_err(), "item_in_use");

    let labels = undo_labels(&v);
    let named: Vec<&str> = labels.iter().map(|l| l.split(' ').next().unwrap()).collect();
    assert_eq!(named, ["sequence.new", "sequence.add_tracks", "project_bin.new_folder", "project_bin.import", "sequence.place_clip", "sequence.place_clip"], "{labels:?}");
    let steps = v.run().history()["undo_steps"].clone();
    assert!(steps.as_array().unwrap().iter().all(|s| s["actor"]["account_id"].is_string() && s["command_id"].is_string()), "{steps}");

    // Undo the placements and the import, then redo: the bin and the clip come back.
    for _ in 0..3 {
        v.undo().unwrap();
    }
    assert!(v.state_json()["project_bin"]["media_items"].as_array().is_none_or(Vec::is_empty));
    assert!(sequence(&v, &seq).tracks[3].clips.is_empty());
    for _ in 0..3 {
        v.redo().unwrap();
    }
    assert_eq!(v.state_json()["project_bin"]["media_items"].as_array().unwrap().len(), 3);
    assert_eq!(sequence(&v, &seq).tracks[3].clips[0].clip_id, audio_clip);
}

#[test]
fn a_saved_project_binds_monitor_export_and_mix_from_its_own_bin() {
    let dir = support_audio::scratch("project-bin-reopen");
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&empty_package(&dir)).unwrap_or_else(|e| panic!("{e}"));
    let seq = v.project_new_sequence("main", PRESET).unwrap();
    v.project_import(&media(&dir), None).unwrap();
    let state = v.state_json();
    let (cam, boom) = (item_id(&state, "cam_a.y4m"), item_id(&state, "boom.wav"));
    let s = sequence(&v, &seq);
    let (v1, a1) = (s.tracks[0].track_id.clone(), s.tracks[3].track_id.clone());
    v.project_place_item(&seq, &v1, &cam, Ticks::new(0), None, None).unwrap();
    v.project_place_item(&seq, &a1, &boom, Ticks::new(0), None, None).unwrap();
    let saved = dir.join("saved.handshake");
    v.save(&saved, false).unwrap();
    // The media files are gone: only the package holds the bytes now.
    for p in media(&dir) {
        std::fs::remove_file(p).unwrap();
    }

    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&saved).unwrap_or_else(|e| panic!("{e}"));
    let bin = w.project_bin().unwrap().unwrap();
    assert_eq!(bin.media_items.len(), 3);
    assert_eq!(sequence(&w, &seq).tracks[3].clips.len(), 1);
    let bound = w.project_bind(&seq, 64, 36, 48_000).unwrap();
    assert_eq!((bound["video"].as_u64(), bound["audio"].as_u64()), (Some(1), Some(1)), "{bound}");
    let not_bound = bound["not_bound"].as_array().unwrap();
    assert_eq!(not_bound.len(), 1, "{bound}");
    assert_eq!(not_bound[0]["code"], "audio_rate_mismatch");
    assert_eq!(w.monitor().unwrap().sequence().sequence_id, seq);
    #[cfg(feature = "audio-batch")]
    {
        let s = w.state_json();
        assert_eq!(s["timeline"]["sequence_id"], seq.as_str(), "{}", s["timeline"]);
        assert!(s["mixer"].is_object(), "{}", s["mixer"]);
    }
}

/// The built `studio-harness` (the example without `desktop`, as tests/harness_drive.rs finds it).
fn binary() -> PathBuf {
    #[cfg(feature = "desktop")]
    return PathBuf::from(env!("CARGO_BIN_EXE_studio-harness"));
    #[cfg(not(feature = "desktop"))]
    {
        let exe = std::env::current_exe().unwrap();
        let dir = exe.parent().and_then(Path::parent).expect("target directory").to_path_buf();
        let example = dir.join("examples").join(format!("studio-harness{}", std::env::consts::EXE_SUFFIX));
        assert!(example.is_file(), "the studio-harness example was not built at {}", example.display());
        example
    }
}

/// One quiet `command PACKAGE --stdin` session (no console, piped streams, bounded wait): exit code
/// and one JSON value per stdout line.
fn drive(package: &Path, input: &[Value], dir: &Path) -> (i32, Vec<Value>) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let args = ["command", package.to_str().unwrap(), "--actor", "model-a", "--session", "s-1", "--stdin"];
    let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    assert!(!matches!(hsk_studio_harness::cli::window_args(&owned, &|p| p.is_file()), hsk_studio_harness::cli::WindowArgs::Window { .. }));
    let mut command = Command::new(binary());
    command.args(args).current_dir(dir).env("STUDIO_DIAG_DIR", dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().expect("spawn studio-harness");
    let bytes: Vec<u8> = input.iter().flat_map(|v| format!("{v}\n").into_bytes()).collect();
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&bytes);
    });
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut out);
        out
    });
    let mut stderr = child.stderr.take().unwrap();
    let errors = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stderr, &mut out);
        out
    });
    let start = std::time::Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if start.elapsed() > std::time::Duration::from_secs(180) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the drive session did not end");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    writer.join().unwrap();
    let out = String::from_utf8(reader.join().unwrap()).unwrap();
    let _ = errors.join();
    (status.code().expect("an exit code"), out.lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}"))).collect())
}

/// Through the process, the way a model drives it: New Sequence, import, a folder, a placement, save.
/// The minted ids are deterministic (sequence `seq-1`, items in import order, A1 the fourth track).
#[test]
fn a_model_builds_the_project_through_the_drive_session() {
    let dir = support_audio::scratch("project-bin-drive");
    let package = empty_package(&dir);
    let paths: Vec<String> = media(&dir).iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let saved = dir.join("drive_saved.handshake");
    let a1 = "STRK-019abcde-0000-7e1e-9002-000000000004";
    let line = |id: &str, parameters: Value| serde_json::json!({"command": "command", "command_id": id, "parameters": parameters});
    let input = [
        line("viewer.project_new_sequence", serde_json::json!({"name": "ep12", "preset_id": PRESET})),
        line("viewer.project_import", serde_json::json!({"paths": paths})),
        line("viewer.project_new_folder", serde_json::json!({"name": "Sound"})),
        line("viewer.project_move_items", serde_json::json!({"items": "item-2,item-3", "folder": "bin-1"})),
        line("viewer.project_add_tracks", serde_json::json!({"sequence_id": "seq-1", "kind": "submix", "names": "SUB-DX"})),
        line("viewer.project_place_item", serde_json::json!({"sequence_id": "seq-1", "track_id": a1, "item_id": "item-2", "start": 0})),
        line("document.save", serde_json::json!({"path": saved.to_string_lossy()})),
    ];
    let (exit, replies) = drive(&package, &input, &dir);
    let failed: Vec<&Value> = replies.iter().filter(|r| r["ok"] == false).collect();
    assert!(failed.is_empty() && exit == 0, "exit {exit}: {failed:?}");
    let import = replies.iter().find(|r| r["command_id"] == "viewer.project_import").unwrap();
    assert_eq!(import["detail"]["result"]["refused"][0]["code"], "MEDIA_IMPORT_UNSUPPORTED", "{import}");
    let last = &replies.iter().rev().find(|r| r["command_id"] == "viewer.project_place_item").unwrap()["state"];
    assert_eq!(last["project_bin"]["media_items"].as_array().unwrap().len(), 3);

    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&saved).unwrap_or_else(|e| panic!("{e}"));
    let bin = w.project_bin().unwrap().unwrap();
    assert_eq!(bin.item("item-2").unwrap().folder_id.as_deref(), Some("bin-1"));
    let s = sequence(&w, "seq-1");
    assert_eq!((s.tracks.len(), s.tracks[3].track_id.as_str(), s.tracks[3].clips.len()), (8, a1, 1));
    assert_eq!(s.tracks[3].clips[0].source_ref, bin.item("item-2").unwrap().source);
}
