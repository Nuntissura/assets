//! Lane SMB-DRIVE-TYPED-2, the third drive slice: the mixer, multitrack automation, the waveform editor
//! and the spectral display (feature `audio-batch`), the monitors and source edit (feature `media`) and
//! product design's instance commands and prototype player (feature `viewer`) run as typed rows through
//! the real studio-harness process, one JSONL line each. A mutating row names its undo step, carries the
//! caller's attribution and survives save and reopen; a refused row answers a typed code. Oracles: the
//! package reopened by the library viewer, the in-process player, hand values of the fixtures. Scenario
//! steps (workflows/): audio-post 11, 21, 22, 33-35, 41, 42, 46-48, 51, 55, 57-59, 64-66, 69-71;
//! video-edit 13, 15, 18, 19, 76, 77; product-design 43-45, 63, 65-68, 79 (STU-PRO-138/139: the spec's
//! prototype.start_session, dispatch_trigger, read_runtime_variables, capture_frame and end_session).
//! Also the lead additions: SMB-DOC-NEW-1's rows mark optional parameters as their drive rows do, and
//! its viewer rows are document rows.

use hsk_studio_harness_core::action::{self, Surface};
use hsk_studio_harness_core::manual;

/// Every typed row of the slice.
const SLICE_ROWS: &[&str] = &[
    "mixer.open", "mixer.volume", "mixer.pan", "mixer.mute", "mixer.solo", "mixer.output", "mixer.add_send", "mixer.send", "mixer.remove_send", "mixer.master_volume",
    "mixer.add_effect", "mixer.remove_effect", "mixer.move_effect", "mixer.effect_enabled", "mixer.effect_param", "mixer.render", "mixer.state",
    "multitrack.open", "multitrack.lane_show", "multitrack.lane_cycle", "multitrack.lane_point_add", "multitrack.lane_point_move", "multitrack.lane_point_delete",
    "multitrack.clip_gain", "multitrack.waveforms", "multitrack.lane",
    "waveform.select", "waveform.snap", "waveform.copy", "waveform.cut", "waveform.paste", "waveform.delete", "waveform.silence", "waveform.insert_silence", "waveform.fade",
    "waveform.gain_envelope", "waveform.amplify", "waveform.normalize", "waveform.invert", "waveform.reverse", "waveform.effect", "waveform.peaks", "waveform.state",
    "spectral.open", "spectral.set_region", "spectral.apply", "spectral.state",
    "monitor.open", "monitor.bind_program_media", "monitor.open_source_media", "monitor.source_mark", "monitor.key", "monitor.source_edit",
    "prototype.start_session", "prototype.dispatch_trigger", "prototype.click", "prototype.hover", "prototype.advance", "prototype.restart",
    "prototype.read_runtime_variables", "prototype.capture_frame", "prototype.state", "prototype.end_session",
    "design.import_fig", "design.instance.inspect", "design.instance.switch_variant", "design.instance.set_property", "design.instance.set_override",
    "design.instance.reset_override", "design.instance.reset_all", "design.instance.swap_component",
];

/// The slice's rows are typed catalog rows; none takes an `audio.*` id, and the one catalog still
/// builds with them (an author id collision is a refusal of the whole catalog).
#[test]
fn the_slice_rows_are_typed_catalog_rows() {
    let catalog = action::ui_seed().expect("the catalog builds");
    for id in SLICE_ROWS {
        let row = action::command(id).unwrap_or_else(|| panic!("{id} has no row"));
        assert_eq!(row.surface, Surface::Viewer, "{id}");
        assert!(!row.recordable && !row.command_id.starts_with("audio."), "{id}");
        assert!(catalog.resolve(id).is_some(), "{id} resolves in the catalog");
    }
    for id in ["mixer.volume", "waveform.amplify", "multitrack.lane_point_add", "spectral.apply", "monitor.source_edit", "design.instance.switch_variant"] {
        assert!(action::command(id).unwrap().mutates_authority, "{id}");
    }
    for id in ["mixer.state", "waveform.copy", "multitrack.lane_show", "prototype.click", "design.instance.inspect"] {
        assert!(!action::command(id).unwrap().mutates_authority, "{id}");
    }
}

/// Lead addition 1: a catalog id the drive routes to a viewer row (SMB-DOC-NEW-1's document.new,
/// artboard.* and page.*) marks a parameter optional exactly where the viewer row does.
#[test]
fn routed_document_rows_mark_optional_parameters_as_their_drive_rows() {
    for (id, method) in manual::DRIVE_CATALOG_ROUTES {
        let Some(entry) = action::command(id) else { continue };
        let row = manual::DRIVE_VIEWER.iter().find(|r| r.method == *method).unwrap_or_else(|| panic!("viewer.{method}"));
        let mut catalog: Vec<(&str, bool)> = entry.parameters.iter().map(|p| (p.key, p.required)).collect();
        let mut drive: Vec<(&str, bool)> = row.params.iter().map(|p| (p.key, p.required)).collect();
        catalog.sort_unstable();
        drive.sort_unstable();
        assert_eq!(catalog, drive, "{id} -> viewer.{method}");
    }
}

/// Lead addition 2: New Document, artboard and page viewer rows are document rows.
#[test]
fn document_viewer_rows_have_the_document_domain() {
    let methods = [
        "new_document", "add_artboard", "rename_artboard", "resize_artboard", "move_artboard", "delete_artboard", "set_artboard_options", "go_to_artboard", "add_page",
        "rename_page", "resize_page", "delete_page", "go_to_page",
    ];
    for method in methods {
        assert_eq!(manual::viewer_domain(method), "document", "{method}");
        assert_eq!(manual::viewer_row_of(&format!("document.{method}")).map(|r| r.method), Some(method));
        assert!(manual::viewer_row_of(&format!("vector.{method}")).is_none(), "{method}");
    }
    // The catalog ids keep their routes.
    assert!(manual::DRIVE_CATALOG_ROUTES.contains(&("artboard.add", "add_artboard")));
}

// ---- the process ----------------------------------------------------------------------------------

#[cfg(feature = "viewer")]
mod process {
    use std::path::{Path, PathBuf};

    /// The built binary: `studio-harness` under feature `desktop`, else the example beside the test
    /// target (run with no target flags so cargo builds the real example).
    #[cfg(feature = "desktop")]
    fn binary() -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_studio-harness"))
    }

    #[cfg(not(feature = "desktop"))]
    fn binary() -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let dir = exe.parent().and_then(Path::parent).expect("target directory").to_path_buf();
        let example = dir.join("examples").join(format!("studio-harness{}", std::env::consts::EXE_SUFFIX));
        assert!(example.is_file(), "the studio-harness example was not built at {}", example.display());
        example
    }

    /// A scratch directory under the cargo target tree, removed on drop.
    pub struct Scratch(pub PathBuf);
    impl Scratch {
        pub fn new(label: &str) -> Self {
            let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("drive-typed-2-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        pub fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// One quiet live session: no console, piped streams, diagnostics under the scratch dir; the window
    /// classifier guards the spawn.
    pub struct Live {
        child: std::process::Child,
        stdin: std::process::ChildStdin,
        stdout: std::io::BufReader<std::process::ChildStdout>,
    }

    impl Live {
        pub fn open(package: &Path, actor: &str, diag: &Path) -> Self {
            use hsk_studio_harness::cli::{self, WindowArgs};
            let args = ["command", package.to_str().unwrap(), "--actor", actor, "--session", "s-drive-typed-2", "--stdin"];
            let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
            assert!(!matches!(cli::window_args(&owned, &|p| p.is_file()), WindowArgs::Window { .. }), "{args:?} would open a window; not spawned");
            let mut command = std::process::Command::new(binary());
            command.args(args).current_dir(diag).env("STUDIO_DIAG_DIR", diag).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x0800_0000);
            }
            let mut child = command.spawn().expect("spawn studio-harness");
            let stdin = child.stdin.take().expect("stdin");
            let stdout = std::io::BufReader::new(child.stdout.take().expect("stdout"));
            let mut live = Self { child, stdin, stdout };
            let opened = live.read();
            assert_eq!(opened["ok"], true, "{opened}");
            live
        }

        fn read(&mut self) -> serde_json::Value {
            let mut line = String::new();
            std::io::BufRead::read_line(&mut self.stdout, &mut line).expect("a reply line");
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("reply is not JSON ({e}): {line}"))
        }

        pub fn call(&mut self, id: &str, parameters: serde_json::Value) -> serde_json::Value {
            use std::io::Write;
            writeln!(self.stdin, "{}", serde_json::json!({"command":"command","command_id":id,"parameters":parameters})).unwrap();
            self.stdin.flush().unwrap();
            self.read()
        }

        /// The row's result; the reply passed.
        pub fn ok(&mut self, id: &str, parameters: serde_json::Value) -> serde_json::Value {
            let reply = self.call(id, parameters);
            assert_eq!(reply["ok"], true, "{id}: {} {}", reply["error"], reply["detail"]);
            reply["detail"]["result"].clone()
        }

        /// A mutating row passed and named `step` as its undo step.
        pub fn step(&mut self, id: &str, parameters: serde_json::Value, step: &str) -> serde_json::Value {
            let reply = self.call(id, parameters);
            assert_eq!(reply["ok"], true, "{id}: {} {}", reply["error"], reply["detail"]);
            assert_eq!(reply["detail"]["undo_name"], step, "{id}: {}", reply["detail"]);
            reply["detail"]["result"].clone()
        }

        pub fn refused(&mut self, id: &str, parameters: serde_json::Value, code: &str) -> serde_json::Value {
            let reply = self.call(id, parameters);
            assert_eq!(reply["error"], code, "{id}: {reply}");
            reply
        }

        pub fn close(mut self) -> i32 {
            drop(self.stdin);
            let start = std::time::Instant::now();
            loop {
                if let Some(status) = self.child.try_wait().expect("wait") {
                    return status.code().expect("an exit code");
                }
                if start.elapsed() > std::time::Duration::from_secs(120) {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    panic!("the session did not end");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }

    /// A package holding `doc` (a Folio document JSON), every reference available.
    pub fn package(path: &Path, doc: &serde_json::Value) {
        use hsk_studio_folio::{self as folio, Budget, ContentDigest, Inspection, Resolution, TileRef};
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
        let token = hsk_studio_accord::CancellationToken::default();
        let snapshot = match folio::inspect_bytes(&serde_json::to_vec(doc).unwrap(), Budget::default(), &token, &Any).unwrap() {
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
        std::fs::write(path, bytes).unwrap();
    }

    /// A document JSON with `sequences`.
    pub fn document(sequences: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-0000000000d2","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
            "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
            "artboards":[],"page_spreads":[],
            "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":"SLYR-019abcde-0000-7000-8000-0000000000d2","name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
            "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
            "sequences": sequences
        })
    }
}

// ---- audio post: mixer, multitrack, waveform editor, spectral display ------------------------------

#[cfg(feature = "audio-batch")]
mod support_audio;

#[cfg(feature = "audio-batch")]
mod audio_post {
    use super::process::{Live, Scratch, document, package};
    use super::support_audio::{WavFormat, wav};
    use hsk_studio_accord::{CancellationToken, Ticks};
    use hsk_studio_folio::*;
    use hsk_studio_harness::view::Viewer;
    use hsk_studio_harness::view::render::Prefer;
    use serde_json::json;
    use std::io::Cursor;

    /// One frame at 25 fps in ticks, and the samples of one frame at 48 kHz.
    const F: u64 = 10_160_640_000;
    const SPF: usize = 1920;
    const RATE: u32 = 48_000;
    const CLIP1: &str = "SCLP-019abcde-0000-7000-8000-000000000001";

    fn id(n: u32) -> String {
        format!("STRK-019abcde-0000-7000-8000-{n:012x}")
    }

    /// The clip source a sound file binds as: `sha256:<hex>` of its bytes.
    fn media_source(bytes: &[u8]) -> ClipSource {
        use hsk_studio_reel as reel;
        let hex = reel::hex(&reel::full_digest(&mut Cursor::new(bytes), 1 << 20, &CancellationToken::default()).unwrap());
        ClipSource::Media { artifact_manifest_id: format!("sha256:{hex}"), content_digest: ContentDigest { algorithm: "sha256".into(), digest: hex }, asset_id: None }
    }

    fn clip(n: u32, source: ClipSource, frames: u64) -> StudioClip {
        StudioClip {
            schema_id: ClipSchema::V1,
            clip_id: format!("SCLP-019abcde-0000-7000-8000-{n:012x}"),
            source_ref: source,
            source_in: Ticks::new(0),
            source_out: Ticks::new(frames * F),
            timeline_start: Ticks::new(0),
            duration: Ticks::new(frames * F),
            speed: Speed::UNIT,
            enabled: true,
            link_group: None,
            markers: Vec::new(),
            transitions: ClipTransitions::default(),
            retime: Default::default(),
        }
    }

    fn track(n: u32, kind: TrackKind, clips: Vec<StudioClip>) -> StudioTrack {
        StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: id(n),
            kind,
            index: if kind == TrackKind::Submix { 0 } else { n - 1 },
            name: format!("T{n}"),
            enabled: true,
            locked: false,
            sync_locked: true,
            targeted: true,
            muted: false,
            soloed: false,
            clips,
            volume: 1.0,
            pan: 0.0,
        }
    }

    /// Dialogue (dc 0.25 stereo, ten frames) and music (an impulse, mono) over the DX submix.
    #[test]
    fn mixer_multitrack_waveform_and_spectral_rows_run_through_the_process_and_survive_reopen() {
        let scratch = Scratch::new("audio");
        let n = 12 * SPF;
        let dc_wav = wav(2, RATE, WavFormat::Int16, None, &vec![0.25; 2 * n]);
        let mut imp = vec![0.0f32; n];
        imp[1000] = 0.5;
        let imp_wav = wav(1, RATE, WavFormat::Int16, None, &imp);
        let (dc_path, imp_path) = (scratch.path("dx.wav"), scratch.path("mx.wav"));
        std::fs::write(&dc_path, &dc_wav).unwrap();
        std::fs::write(&imp_path, &imp_wav).unwrap();
        let sequence = StudioSequence {
            schema_id: SequenceSchema::V1,
            sequence_id: "seq-ep".into(),
            name: "ep".into(),
            frame_rate: SequenceFrameRate { ticks_per_frame: F },
            duration: Ticks::new(10 * F),
            tracks: vec![
                track(1, TrackKind::Audio, vec![clip(1, media_source(&dc_wav), 10)]),
                track(2, TrackKind::Audio, vec![clip(2, media_source(&imp_wav), 10)]),
                track(3, TrackKind::Submix, vec![]),
            ],
            markers: Vec::new(),
            work_area: None,
        };
        let source = scratch.path("ep.handshake");
        package(&source, &document(json!([serde_json::to_value(&sequence).unwrap()])));

        let mut s = Live::open(&source, "lane-drive-audio", &scratch.0);
        // No sound port yet: typed refusals, nothing opens.
        s.refused("mixer.open", json!({"sequence_id": "seq-ep"}), "audio_sources_missing");
        s.refused("mixer.volume", json!({"track_id": id(1), "linear": 0.5}), "mixer_closed");
        s.refused("multitrack.lane", json!({"track_id": id(1), "lane": "track_volume"}), "no_timeline");
        s.refused("waveform.amplify", json!({"start": 0, "end": 10, "db": -6}), "waveform_closed");
        s.ok("audio.bind_sound", json!({"files": [dc_path.to_str().unwrap(), imp_path.to_str().unwrap()]}));

        // Step 11 and 55: the timeline's sound and the mixer on the sequence.
        let bound = s.ok("multitrack.open", json!({}));
        assert_eq!((bound["bound_rate"].as_u64(), bound["mixer_open"].as_bool()), (Some(u64::from(RATE)), Some(true)), "{bound}");
        s.refused("multitrack.open", json!({"block_frames": 0}), "parameter_invalid");

        // Steps 57-59 and 69-70: routing, faders, solo and mute, bus and master effects.
        let state = s.step("mixer.output", json!({"track_id": id(1), "bus": id(3)}), "mixer_output");
        assert_eq!(state["strips"][0]["output"], id(3), "{state}");
        let state = s.step("mixer.volume", json!({"track_id": id(2), "linear": 0.5}), "mixer_volume");
        assert_eq!(state["strips"][1]["volume"], 0.5);
        let reply = s.refused("mixer.volume", json!({"track_id": id(2), "linear": 99}), "command_refused");
        assert!(reply["detail"]["message"].as_str().is_some_and(|m| m.contains("volume")), "{reply}");
        s.refused("mixer.volume", json!({"track_id": id(2), "linear": "loud"}), "parameter_invalid");
        s.step("mixer.solo", json!({"track_id": id(1), "soloed": true}), "mixer_solo");
        s.step("mixer.solo", json!({"track_id": id(1), "soloed": false}), "mixer_solo");
        s.step("mixer.mute", json!({"track_id": id(2), "muted": true}), "mixer_mute");
        s.step("mixer.mute", json!({"track_id": id(2), "muted": false}), "mixer_mute");
        let state = s.step("mixer.add_effect", json!({"bus": id(3), "template": "equalizer"}), "mixer_add_effect");
        assert_eq!(state["strips"][2]["effects"][0]["instance_id"], "fx-0", "{state}");
        s.step("mixer.add_effect", json!({"bus": id(3), "template": "dynamics"}), "mixer_add_effect");
        let state = s.step("mixer.move_effect", json!({"bus": id(3), "instance_id": "fx-1", "to": 0}), "mixer_move_effect");
        assert_eq!(state["strips"][2]["effects"][0]["instance_id"], "fx-1");
        let state = s.step("mixer.effect_enabled", json!({"bus": id(3), "instance_id": "fx-0", "enabled": false}), "mixer_effect_enabled");
        assert_eq!(state["strips"][2]["effects"][1]["enabled"], false);
        let state = s.step("mixer.add_effect", json!({"template": "lookahead_limiter"}), "mixer_add_effect");
        assert_eq!(state["master"]["effects"].as_array().map(Vec::len), Some(1), "the master's limiter (step 80)");
        s.refused("mixer.add_effect", json!({"bus": id(3)}), "parameter_missing");
        s.refused("mixer.add_effect", json!({"bus": id(3), "template": "gain", "effect": {"instance_id": "x"}}), "parameter_conflict");
        s.refused("mixer.add_effect", json!({"template": "reverb"}), "parameter_invalid");
        s.refused("mixer.remove_effect", json!({"bus": id(3), "instance_id": "fx-9"}), "command_refused");
        // Step 71: a pass renders through the buses and fills the meters.
        let state = s.ok("mixer.render", json!({"frames": 4800}));
        assert!(state["master"]["peak"][0].as_f64().unwrap() > 0.01, "{state}");
        s.refused("mixer.render", json!({"frames": 10_000_000}), "parameter_invalid");

        // Steps 21, 22, 64-66: lanes, points, clip gain.
        s.ok("multitrack.lane_show", json!({"track_id": id(1), "lane": "track_volume"}));
        s.refused("multitrack.lane_show", json!({"track_id": id(1), "lane": "loudness"}), "parameter_invalid");
        s.step("multitrack.lane_point_add", json!({"track_id": id(1), "lane": "track_volume", "at": 0, "value": -6.0}), "timeline_lane_point_add");
        s.step("multitrack.lane_point_add", json!({"track_id": id(1), "lane": "track_volume", "at": 5 * F, "value": -12.0}), "timeline_lane_point_add");
        s.step("multitrack.lane_point_move", json!({"track_id": id(1), "lane": "track_volume", "point_offset_ticks": 5 * F, "at": 6 * F, "value": -9.0}), "timeline_lane_point_move");
        s.step("multitrack.lane_point_delete", json!({"track_id": id(1), "lane": "track_volume", "point_offset_ticks": 0}), "timeline_lane_point_delete");
        let lane = s.ok("multitrack.lane", json!({"track_id": id(1), "lane": "track_volume"}));
        let points = lane["points"].as_array().unwrap();
        assert_eq!(points.len(), 1, "{lane}");
        assert_eq!(points[0]["at"].as_u64(), Some(6 * F));
        assert!((points[0]["value"].as_f64().unwrap() + 9.0).abs() < 1e-6, "{lane}");
        s.step("multitrack.lane_point_add", json!({"track_id": id(2), "lane": "track_pan", "at": 0, "value": -0.5}), "timeline_lane_point_add");
        s.step("multitrack.clip_gain", json!({"clip_id": CLIP1, "delta_db": -2.0}), "timeline_clip_gain");
        s.refused("multitrack.clip_gain", json!({"clip_id": CLIP1, "delta_db": 0}), "no_change");
        s.ok("multitrack.waveforms", json!({}));

        // Steps 33, 34, 46-48: the boom source in the waveform editor.
        s.ok("audio.waveform_open_clip", json!({"clip_id": CLIP1}));
        s.ok("waveform.select", json!({"start": 0, "end": 480}));
        s.ok("waveform.copy", json!({"start": 0, "end": 480}));
        s.step("waveform.paste", json!({"at": 9600, "mode": "overwrite"}), "waveform_paste");
        s.refused("waveform.paste", json!({"at": 9600, "mode": "sideways"}), "parameter_invalid");
        s.step("waveform.fade", json!({"start": 0, "end": 960, "direction": "in", "curve": "equal_power"}), "waveform_fade");
        s.step("waveform.gain_envelope", json!({"start": 2000, "end": 4400, "points": [[0, 0.0], [1200, -4.0], [2400, 0.0]]}), "waveform_gain_envelope");
        s.step("waveform.amplify", json!({"start": 4800, "end": 9600, "db": -3.0}), "waveform_amplify");
        s.step("waveform.normalize", json!({"start": 4800, "end": 9600, "target_dbfs": -1.0, "linked": true}), "waveform_normalize");
        s.refused("waveform.amplify", json!({"start": 9600, "end": 4800, "db": -3.0}), "waveform_out_of_range");
        s.refused("waveform.gain_envelope", json!({"start": 0, "end": 10, "points": "[[0, \"loud\"]]"}), "parameter_invalid");
        let peaks = s.ok("waveform.peaks", json!({"start": 0, "end": 4800, "columns": 8}));
        assert_eq!(peaks["result"]["channels"].as_array().map(Vec::len), Some(2), "{peaks}");
        // Steps 35, 41, 42: the spectral display and region edits.
        s.refused("spectral.apply", json!({"op": "heal"}), "spectral_closed");
        let display = s.ok("spectral.open", json!({"fft_size": 1024, "hop": 256, "window": "hann", "scale": "log", "min_hz": 40}));
        assert_eq!(display["fft_size"], 1024, "{display}");
        s.refused("spectral.apply", json!({"op": "heal"}), "spectral_no_selection");
        s.ok("spectral.set_region", json!({"start": 0, "end": 4800, "low_hz": 100.0, "high_hz": 2000.0}));
        s.refused("spectral.apply", json!({"op": "attenuate"}), "parameter_missing");
        s.step("spectral.apply", json!({"op": "attenuate", "gain_db": -20.0}), "waveform_spectral");
        s.refused("spectral.set_region", json!({"start": 0, "end": 4800}), "parameter_missing");
        s.ok("spectral.set_region", json!({"lasso": [[100.0, 200.0], [4000.0, 200.0], [4000.0, 3000.0]]}));
        s.step("spectral.apply", json!({"op": "heal"}), "waveform_spectral");
        // Step 51: the last repair undone through the drive's history.
        let editor = s.ok("waveform.state", json!({}));
        let ops = editor["editor"]["ops"].as_array().unwrap().len();
        let undo = s.call("history.undo", json!({}));
        assert_eq!(undo["ok"], true, "{undo}");
        let editor = s.ok("waveform.state", json!({}));
        assert_eq!(editor["editor"]["ops"].as_array().unwrap().len(), ops - 1, "{editor}");
        let reply = s.call("mixer.state", json!({}));
        assert_eq!(reply["attribution"]["actor"]["account_id"], "lane-drive-audio", "the caller's attribution: {reply}");

        let saved = scratch.path("saved.handshake");
        let reply = s.call("document.save", json!({"path": saved.to_str().unwrap()}));
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(s.close(), 1, "the session refused rows, so it ends with exit 1");

        // Reopened by the library viewer: the routing, fader, lanes, clip gain and edit list are in the package.
        let mut w = Viewer::new(Prefer::Cpu);
        w.open(&saved).unwrap();
        let doc = w.run().snapshot().unwrap().document().clone();
        let seq = doc.sequences.iter().find(|q| q.sequence_id == "seq-ep").unwrap();
        assert_eq!(seq.tracks[1].volume, 0.5);
        let routing = w.audio_routing().unwrap().expect("the routing entry");
        let r = routing.routings.iter().find(|r| r.sequence_id == "seq-ep").unwrap();
        assert_eq!(r.routes.iter().find(|x| x.track_id == id(1)).and_then(|x| x.output.clone()), Some(id(3)));
        assert_eq!(r.bus_effects.iter().find(|b| b.track_id == id(3)).map(|b| b.effects.len()), Some(2));
        assert_eq!(r.master.effects.len(), 1);
        assert_eq!(r.track_lane(&id(1), StudioTrackParam::Volume).map(|a| a.points.len()), Some(1));
        assert!(r.track_lane(&id(2), StudioTrackParam::Pan).is_some());
        assert!(r.clip_gains.iter().any(|g| g.clip_id == CLIP1), "clip gain");
        let edits = w.waveform_document().unwrap().expect("the waveform edit list");
        let names: Vec<&str> = edits.ops.iter().map(|o| o.name()).collect();
        assert_eq!(names, ["paste", "fade", "gain_envelope", "amplify", "normalize", "spectral"], "the heal was undone");
    }
}

// ---- video edit: the monitors and source edit ---------------------------------------------------

#[cfg(feature = "media")]
mod monitors {
    use super::process::{Live, Scratch, document, package};
    use hsk_studio_accord::Ticks;
    use hsk_studio_folio::*;
    use hsk_studio_harness::view::Viewer;
    use hsk_studio_harness::view::render::Prefer;
    use serde_json::json;

    /// One frame at 24 fps in ticks.
    const TPF_24: u64 = 10_584_000_000;

    /// A 16x16 4:2:0 Y4M file of `frames` frames at 24 fps.
    fn y4m(frames: usize) -> Vec<u8> {
        let mut out = b"YUV4MPEG2 W16 H16 F24:1 Ip A1:1 C420\n".to_vec();
        for f in 0..frames {
            out.extend_from_slice(b"FRAME\n");
            out.extend(std::iter::repeat_n(u8::try_from(f % 200).unwrap(), 16 * 16 * 3 / 2));
        }
        out
    }

    /// Steps 13, 15, 18 and 19 of the video edit: a take screened in source, marked and inserted, then
    /// overwritten on the program sequence, each one undo step that the reopened package holds.
    #[test]
    fn monitor_rows_screen_mark_and_edit_a_take_into_the_program() {
        let scratch = Scratch::new("monitors");
        let take = scratch.path("take.y4m");
        std::fs::write(&take, y4m(24)).unwrap();
        // An empty program sequence (Folio: its duration is its content's end, 0).
        let sequence = StudioSequence {
            schema_id: SequenceSchema::V1,
            sequence_id: "seq-cut".into(),
            name: "cut".into(),
            frame_rate: SequenceFrameRate { ticks_per_frame: TPF_24 },
            duration: Ticks::new(0),
            tracks: vec![StudioTrack {
                schema_id: TrackSchema::V1,
                track_id: "STRK-019abcde-0000-7000-8000-000000000011".into(),
                kind: TrackKind::Video,
                index: 0,
                name: "V1".into(),
                enabled: true,
                locked: false,
                sync_locked: true,
                targeted: true,
                muted: false,
                soloed: false,
                clips: Vec::new(),
                volume: 1.0,
                pan: 0.0,
            }],
            markers: Vec::new(),
            work_area: None,
        };
        let source = scratch.path("cut.handshake");
        package(&source, &document(json!([serde_json::to_value(&sequence).unwrap()])));

        let mut s = Live::open(&source, "lane-drive-monitor", &scratch.0);
        s.refused("monitor.source_edit", json!({"mode": "insert"}), "no_source_monitor");
        s.refused("monitor.source_mark", json!({"mark_in": 2}), "no_source_monitor");
        s.ok("monitor.open", json!({"sequence_id": "seq-cut", "width": 64, "height": 36}));
        s.refused("monitor.open", json!({"sequence_id": "seq-cut", "width": 0}), "parameter_invalid");
        let opened = s.ok("monitor.open_source_media", json!({"path": take.to_str().unwrap(), "width": 64, "height": 36}));
        assert!(opened["source"].as_str().is_some_and(|id| id.starts_with("sha256:")), "{opened}");
        s.refused("monitor.open_source_media", json!({"path": scratch.path("missing.y4m").to_str().unwrap()}), "resource_unreadable");
        s.ok("monitor.key", json!({"target": "source", "key": "step_forward", "frames": 3}));
        s.refused("monitor.key", json!({"target": "source", "key": "rewind"}), "parameter_invalid");
        s.ok("monitor.key", json!({"target": "source", "key": "mark_in"}));
        let marked = s.ok("monitor.source_mark", json!({"mark_out": 10}));
        assert_eq!((marked["source"]["mark_in"].as_u64(), marked["source"]["mark_out"].as_u64()), (Some(3), Some(10)), "{marked}");
        let reply = s.call("monitor.source_edit", json!({"mode": "insert"}));
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["detail"]["undo_name"], "source_insert", "{reply}");
        s.step("monitor.source_edit", json!({"mode": "overwrite"}), "source_overwrite");
        s.refused("monitor.source_edit", json!({"mode": "replace"}), "parameter_invalid");
        let bound = s.ok("monitor.bind_program_media", json!({"sequence_id": "seq-cut", "files": [take.to_str().unwrap()], "width": 64, "height": 36}));
        assert_eq!(bound["sources"][0], opened["source"], "{bound}");
        let saved = scratch.path("cut-saved.handshake");
        assert_eq!(s.call("document.save", json!({"path": saved.to_str().unwrap()}))["ok"], true);
        assert_eq!(s.close(), 1);

        let mut w = Viewer::new(Prefer::Cpu);
        w.open(&saved).unwrap();
        let doc = w.run().snapshot().unwrap().document().clone();
        let clips = &doc.sequences[0].tracks[0].clips;
        assert!(!clips.is_empty(), "the edits reached the package");
        assert!(clips.iter().all(|c| matches!(&c.source_ref, ClipSource::Media { artifact_manifest_id, .. } if Some(artifact_manifest_id.as_str()) == opened["source"].as_str())));
    }
}

// ---- product design: instances and the prototype player -------------------------------------------

#[cfg(feature = "viewer")]
mod product_design {
    use super::process::{Live, Scratch, document, package};
    use hsk_studio_harness::view::Viewer;
    use hsk_studio_harness::view::prototype::PrototypePlay;
    use hsk_studio_harness::view::render::Prefer;
    use serde_json::{Value, json};

    fn fixture(name: &str) -> std::path::PathBuf {
        // The fixtures live with the importer (repository-relative, never machine-local).
        std::path::PathBuf::from(format!("{}/../hsk-studio-interop-fig/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR")))
    }

    /// The frame-local centre of the text layer `name` on the player's screen (its glyph bounds).
    fn label_centre(play: &PrototypePlay, name: &str) -> (f64, f64) {
        let d = play.proposal().snapshot.document();
        let screen = play.session().state().screen;
        let text = d.layers.iter().find(|l| l.name == name && l.kind == "text").expect("label");
        let layout = play.proposal().text_layouts.iter().find(|t| t.layer_id == text.layer_id).unwrap();
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for g in layout.glyphs.iter().filter_map(|g| g.path.as_ref()) {
            for e in &g.elements {
                if let hsk_studio_nib::PathElement::MoveTo(p) | hsk_studio_nib::PathElement::LineTo(p) = e {
                    (x0, y0, x1, y1) = (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y));
                }
            }
        }
        let bg = play.proposal().frame_backgrounds.iter().find(|b| b.frame.node_id == screen).unwrap();
        let path = play.proposal().paths.iter().find(|p| p.layer_id == bg.layer_id).unwrap();
        let (mut fx, mut fy) = (f64::MAX, f64::MAX);
        for e in &path.geometry.elements {
            if let hsk_studio_nib::PathElement::MoveTo(p) | hsk_studio_nib::PathElement::LineTo(p) = e {
                (fx, fy) = (fx.min(p.x), fy.min(p.y));
            }
        }
        ((x0 + x1) / 2.0 - fx, (y0 + y1) / 2.0 - fy)
    }

    /// Steps 43-45 and 79: instance variant, text override and reset through the process, one step
    /// each, kept by the reopened package; steps 63, 65-68: the player clicked, hovered and clocked,
    /// in step with the in-process player.
    #[test]
    fn design_and_prototype_rows_run_through_the_process() {
        let scratch = Scratch::new("design");
        let start = scratch.path("start.handshake");
        package(&start, &document(json!([])));
        let fig = scratch.path("components.fig");
        std::fs::copy(fixture("fig-components-autolayout.fig"), &fig).unwrap();
        let dest = scratch.path("components.handshake");

        let mut s = Live::open(&start, "lane-drive-design", &scratch.0);
        s.refused("design.instance.reset_all", json!({"layer": "SLYR-019abcde-0000-7000-8000-00000000ffff"}), "command_refused");
        let report = s.ok("design.import_fig", json!({"source": fig.to_str().unwrap(), "dest": dest.to_str().unwrap()}));
        // The imported package is now the session's: the drive never writes over it.
        s.refused("design.import_fig", json!({"source": fig.to_str().unwrap(), "dest": dest.to_str().unwrap(), "overwrite": true}), "export_over_source");
        s.refused("design.import_fig", json!({"source": fig.to_str().unwrap(), "dest": start.to_str().unwrap()}), "overwrite_required");
        let layers: Vec<String> = report["instance_layers"].as_array().unwrap().iter().filter_map(Value::as_str).map(str::to_owned).collect();
        assert!(!layers.is_empty(), "{report}");
        // An instance whose set has a `state` variant with a second value.
        let (layer, view) = layers
            .iter()
            .map(|l| (l.clone(), s.ok("design.instance.inspect", json!({"layer": l}))))
            .find(|(_, v)| v["variants"]["state"]["values"].as_array().is_some_and(|vals| vals.len() > 1))
            .expect("an instance with a state variant");
        let current = view["variants"]["state"]["current"].clone();
        let other = view["variants"]["state"]["values"].as_array().unwrap().iter().find(|v| **v != current).unwrap().as_str().unwrap().to_owned();
        let switched = s.step("design.instance.switch_variant", json!({"layer": layer, "property": "state", "value": other}), "design_switch_variant");
        assert!(switched.is_object(), "{switched}");
        let view = s.ok("design.instance.inspect", json!({"layer": layer}));
        assert_eq!(view["variants"]["state"]["current"], other.as_str(), "{view}");
        s.refused("design.instance.inspect", json!({"layer": "SLYR-019abcde-0000-7000-8000-00000000ffff"}), "not_an_instance");
        s.refused("design.instance.set_override", json!({"layer": layer, "node_id": "x", "field": "fills", "value": "red"}), "parameter_invalid");
        let node = view["texts"][0]["node_id"].as_str().expect("a text node").to_owned();
        s.step("design.instance.set_override", json!({"layer": layer, "node_id": node, "field": "characters", "value": "Book the vet"}), "design_set_override");
        let view = s.ok("design.instance.inspect", json!({"layer": layer}));
        assert_eq!(view["texts"][0]["text"], "Book the vet", "{view}");
        s.step("design.instance.reset_override", json!({"layer": layer, "node_id": node, "field": "characters"}), "design_reset_override");
        s.step("design.instance.set_override", json!({"layer": layer, "node_id": node, "field": "visible", "value": false}), "design_set_override");
        let saved = scratch.path("components-saved.handshake");
        assert_eq!(s.call("document.save", json!({"path": saved.to_str().unwrap()}))["ok"], true);

        // The player beside the document; the in-process player is the oracle.
        let pages = fixture("fig-prototype-pages.fig");
        let mut oracle = PrototypePlay::open(&std::fs::read(&pages).unwrap(), None).unwrap();
        s.refused("prototype.click", json!({"x": 1, "y": 1}), "prototype_closed");
        let opened = s.ok("prototype.start_session", json!({"path": pages.to_str().unwrap()}));
        assert_eq!(opened["state"], oracle.state_json(), "{opened}");
        let (x, y) = label_centre(&oracle, "Click: navigate to details");
        let hovered = s.ok("prototype.hover", json!({"x": x, "y": y}));
        oracle.hover(x, y);
        assert_eq!(hovered["state"], oracle.state_json());
        let clicked = s.ok("prototype.click", json!({"x": x, "y": y}));
        let hit = oracle.click(x, y);
        assert_eq!(clicked["hit"], json!(hit), "{clicked}");
        assert_eq!(clicked["state"], oracle.state_json());
        assert_eq!(clicked["state"]["screen_name"], "detail-screen");
        let (x, y) = label_centre(&oracle, "Click: smart animate");
        s.ok("prototype.click", json!({"x": x, "y": y}));
        oracle.click(x, y);
        let mid = s.ok("prototype.advance", json!({"ms": 175}));
        oracle.advance(175);
        assert_eq!(mid["state"], oracle.state_json(), "the clock reached the same tween");
        // Step 67: the mid frame captured to bytes on disk, the same size as the in-process render.
        let frame = scratch.path("mid.png");
        let rendered = s.ok("prototype.capture_frame", json!({"path": frame.to_str().unwrap()}));
        let screen = oracle.render().unwrap();
        assert_eq!((rendered["screen"]["width"].as_u64(), rendered["screen"]["height"].as_u64()), (Some(u64::from(screen.width)), Some(u64::from(screen.height))));
        assert!(std::fs::read(&frame).unwrap().starts_with(b"\x89PNG"), "a PNG was written");
        s.refused("prototype.capture_frame", json!({"path": frame.to_str().unwrap()}), "overwrite_required");
        s.refused("prototype.advance", json!({"ms": 3_600_001}), "parameter_invalid");
        s.ok("prototype.advance", json!({"ms": 175}));
        oracle.advance(175);
        // Step 65 by name: a named trigger on a named node, no pointer.
        let (x, y) = label_centre(&oracle, "Click: back to start");
        let node = oracle.hit(x, y).expect("the label node");
        let fired = s.ok("prototype.dispatch_trigger", json!({"node": node, "trigger": "click"}));
        oracle.dispatch_trigger(&hsk_studio_components::PlayerInput::Click(node.clone()));
        assert_eq!(fired["state"], oracle.state_json(), "{fired}");
        assert_eq!(fired["state"]["screen_name"], "start-screen");
        s.refused("prototype.dispatch_trigger", json!({"node": node, "trigger": "tap"}), "parameter_invalid");
        // Step 68: runtime variables as the session holds them.
        let vars = s.ok("prototype.read_runtime_variables", json!({}));
        assert_eq!(vars["variables"], serde_json::to_value(&oracle.session().state().variables).unwrap(), "{vars}");
        let restarted = s.ok("prototype.restart", json!({}));
        assert_eq!(restarted["state"]["history_depth"], 0);
        s.ok("prototype.end_session", json!({}));
        s.refused("prototype.state", json!({}), "prototype_closed");
        assert_eq!(s.close(), 1);

        // The edited instance is kept: the reopened package draws the switched variant without the label.
        let mut w = Viewer::new(Prefer::Cpu);
        w.open(&saved).unwrap();
        let view = w.instance_view(&layer).expect("the instance").json();
        assert_eq!(view["variants"]["state"]["current"], other.as_str(), "{view}");
        assert_eq!(view["state"], "edited", "{view}");
    }
}
