//! Lane SMB-DEVELOP-COMMANDS-1 (feature `viewer`): every develop operation of the Viewer is a typed
//! `photo.develop.*` row. Each row is checked against a direct `Viewer::develop_op` (or `set_develop`)
//! call with the same operation on a second viewer of the same raw: same tile digests, same
//! `develop_json`, one undo step each (a differential, so a decoder cannot drift from the op). One
//! process test runs scenario photo-retouch steps 27-30, 40, 41, 46 and 51 through `command --stdin`.
//! Headless (no window, CX-SAFE-002). Lightroom and Camera Raw parity UNVERIFIED (D5).
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, synthetic_dng};
use hsk_studio_develop::{Brush, ColourRange, LookDomain, LuminanceRange, MaskSource, RadialGradient, UprightMode, VignetteStyle};
use hsk_studio_folio::Payload;
use hsk_studio_harness::cli::{self, WindowArgs};
use hsk_studio_harness::view::commands::CommandRefusal;
use hsk_studio_harness::view::develop::{DevelopOp, Refusal, default_linear};
use hsk_studio_harness::view::develop_more::FamilyOp;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::Viewer;
use serde_json::{Map, Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn digests(v: &Viewer) -> Vec<String> {
    let layer = v.develop_json()["layer_id"].as_str().unwrap().to_string();
    let snap = v.run().snapshot().unwrap();
    match &snap.document().layers.iter().find(|l| l.layer_id == layer).unwrap().payload {
        Payload::Raster { tiles } => tiles.iter().map(|t| t.content_digest.digest.clone()).collect(),
        _ => panic!("not raster"),
    }
}

fn open(path: &Path) -> Viewer {
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    assert!(v.develop_available(), "{}", v.develop_json());
    v
}

fn undo_len(v: &Viewer) -> usize {
    v.run().history()["undo"].as_array().unwrap().len()
}

fn row(v: &mut Viewer, id: &str, params: Value) -> Result<Value, CommandRefusal> {
    let map: Map<String, Value> = params.as_object().cloned().unwrap_or_default();
    v.run_command(id, &map)
}

/// The row on `a` and the direct call on `b` both succeed, make one undo step each, and leave the
/// same pixels and the same develop state.
fn same(a: &mut Viewer, b: &mut Viewer, id: &str, params: Value, direct: impl FnOnce(&mut Viewer) -> Result<u64, Refusal>) {
    let steps = undo_len(a);
    let typed = row(a, id, params);
    let called = direct(b);
    assert!(typed.is_ok(), "{id}: the row refused {typed:?} (direct {called:?})");
    assert!(called.is_ok(), "{id}: the direct call refused {called:?}");
    assert_eq!(undo_len(a), steps + 1, "{id} is one undo step");
    assert_eq!(undo_len(a), undo_len(b), "{id}: undo steps differ");
    assert_eq!(digests(a), digests(b), "{id}: tiles differ from the direct call");
    assert_eq!(a.develop_json(), b.develop_json(), "{id}: develop state differs from the direct call");
}

const PROFILE: &str = "hsk-lens-profile 1\nname Test 35\nsample 35 - inf\nperspective 1 1 0.5 0.5 -0.08 0 0\nvignette -0.3 0 0\nca_red_green 1.003 0 0 0\n";
const PRESET: &str = "hsk-develop-preset 1\nmeta name Punch\nset contrast 35\nset effects.grain.size 60\n";

fn invert_cube() -> String {
    let mut s = String::from("TITLE \"negative\"\nLUT_3D_SIZE 3\n");
    for b in 0..3 {
        for g in 0..3 {
            for r in 0..3 {
                s.push_str(&format!("{} {} {}\n", 1.0 - r as f64 / 2.0, 1.0 - g as f64 / 2.0, 1.0 - b as f64 / 2.0));
            }
        }
    }
    s
}

struct Files {
    profile: PathBuf,
    cube: PathBuf,
    preset: PathBuf,
}

fn files(scratch: &Scratch) -> Files {
    let f = Files { profile: scratch.0.join("lens.txt"), cube: scratch.0.join("negative.cube"), preset: scratch.0.join("punch.preset") };
    std::fs::write(&f.profile, PROFILE).unwrap();
    std::fs::write(&f.cube, invert_cube()).unwrap();
    std::fs::write(&f.preset, PRESET).unwrap();
    f
}

fn text(p: &Path) -> String {
    p.to_str().unwrap().to_string()
}

/// Every row over an existing DevelopOp / FamilyOp member and the two slider rows equal the direct call.
#[test]
fn every_existing_develop_op_has_a_typed_row_equal_to_the_direct_call() {
    let scratch = Scratch::new("develop-commands-existing");
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(160, 120)).unwrap();
    let f = files(&scratch);
    let (mut a, mut b) = (open(&source), open(&source));

    same(&mut a, &mut b, "photo.develop.set", json!({"control":"develop.exposure","value":0.5}), |v| v.set_develop("develop.exposure", 0.5));
    same(&mut a, &mut b, "photo.develop.set_many", json!({"changes":{"develop.contrast":20,"develop.shadows":30}}), |v| v.set_develop_many(&[("develop.contrast", 20.0), ("develop.shadows", 30.0)]));
    same(&mut a, &mut b, "photo.develop.add_colour_range", json!({"samples":[[0.5,0.4,0.3]],"breadth":0.3}), |v| v.develop_op(DevelopOp::AddColourRange { samples: vec![[0.5, 0.4, 0.3]], breadth: 0.3 }));
    same(&mut a, &mut b, "photo.develop.add_correction", json!({"source":"linear"}), |v| v.develop_op(DevelopOp::AddCorrection(default_linear())));
    let radial = RadialGradient { left: 0.2, top: 0.25, right: 0.75, bottom: 0.75, angle: 0.0, midpoint: 50.0, roundness: 0.0, feather: 30.0, flipped: false };
    same(&mut a, &mut b, "photo.develop.add_correction", json!({"source":"radial","left":0.2,"feather":30}), |v| v.develop_op(DevelopOp::AddCorrection(MaskSource::RadialGradient(radial))));
    let brush = Brush { dabs: vec![(0.3, 0.3), (0.4, 0.4)], radius: 0.05, flow: 1.0, feather: 0.5 };
    same(&mut a, &mut b, "photo.develop.add_correction", json!({"source":"brush","dabs":[[0.3,0.3],[0.4,0.4]],"radius":0.05}), |v| v.develop_op(DevelopOp::AddCorrection(MaskSource::Brush(brush))));
    same(&mut a, &mut b, "photo.develop.set", json!({"control":"develop.local.3.exposure","value":0.5}), |v| v.set_develop("develop.local.3.exposure", 0.5));
    let lum = MaskSource::LuminanceRange(LuminanceRange { lo: 0.5, hi: 1.0, feather: 0.1 });
    same(&mut a, &mut b, "photo.develop.add_mask", json!({"correction":3,"source":"luminance_range","lo":0.5,"hi":1.0,"feather":0.1,"subtract":true}), |v| v.develop_op(DevelopOp::AddMask { correction: 3, source: lum, subtract: true }));
    let colour = MaskSource::ColourRange(ColourRange { samples: vec![[0.2, 0.3, 0.4]], breadth: 0.5 });
    same(&mut a, &mut b, "photo.develop.add_mask", json!({"correction":1,"source":"colour_range","samples":[[0.2,0.3,0.4]],"breadth":0.5}), |v| v.develop_op(DevelopOp::AddMask { correction: 1, source: colour, subtract: false }));
    let lens = FamilyOp::LensProfile { profile: PROFILE.as_bytes().to_vec(), focal_mm: 35.0, aperture_value: None, distance_m: None };
    same(&mut a, &mut b, "photo.develop.lens_profile", json!({"path":text(&f.profile),"focal_mm":35}), |v| v.develop_op(DevelopOp::Family(lens)));
    // Both toggles are on by default (develop `Optics::default`): turn lateral CA removal off.
    same(&mut a, &mut b, "photo.develop.lens_toggles", json!({"profile_enable":true,"auto_ca":false}), |v| v.develop_op(DevelopOp::Family(FamilyOp::LensToggles { profile_enable: true, auto_ca: false })));
    same(&mut a, &mut b, "photo.develop.upright_guides", json!({"guides":[[[0.1,0.1],[0.12,0.9]],[[0.9,0.1],[0.88,0.9]]]}), |v| {
        v.develop_op(DevelopOp::Family(FamilyOp::UprightGuides(vec![[[0.1, 0.1], [0.12, 0.9]], [[0.9, 0.1], [0.88, 0.9]]])))
    });
    same(&mut a, &mut b, "photo.develop.upright", json!({"mode":"level"}), |v| v.develop_op(DevelopOp::Family(FamilyOp::Upright(UprightMode::Level))));
    same(&mut a, &mut b, "photo.develop.set", json!({"control":"develop.effects.grain.amount","value":40}), |v| v.set_develop("develop.effects.grain.amount", 40.0));
    same(&mut a, &mut b, "photo.develop.grain_seed", json!({"seed":12345}), |v| v.develop_op(DevelopOp::Family(FamilyOp::GrainSeed(12_345))));
    same(&mut a, &mut b, "photo.develop.vignette_style", json!({"style":"colour_priority"}), |v| v.develop_op(DevelopOp::Family(FamilyOp::VignetteStyle(VignetteStyle::ColourPriority))));
    let look = FamilyOp::Look { name: "negative".into(), cube: invert_cube().into_bytes(), domain: LookDomain::SrgbEncoded };
    same(&mut a, &mut b, "photo.develop.look", json!({"path":text(&f.cube),"name":"negative","domain":"srgb_encoded"}), |v| v.develop_op(DevelopOp::Family(look)));
    same(&mut a, &mut b, "photo.develop.apply_preset", json!({"path":text(&f.preset),"amount":0.8}), |v| v.develop_op(DevelopOp::Family(FamilyOp::Preset { preset: PRESET.as_bytes().to_vec(), amount: 0.8 })));
    same(&mut a, &mut b, "photo.develop.clear_look", json!({}), |v| v.develop_op(DevelopOp::Family(FamilyOp::ClearLook)));
    same(&mut a, &mut b, "photo.develop.clear_lens_profile", json!({}), |v| v.develop_op(DevelopOp::Family(FamilyOp::ClearLensProfile)));
    assert!(a.develop_json()["local"].as_array().is_some_and(|l| l.len() == 4), "{}", a.develop_json());
}

/// A file parameter is read under a named cap before parsing; a token, a count or a number out of shape
/// is a typed refusal before anything runs, and nothing is committed.
#[test]
fn develop_rows_refuse_hostile_parameters_typed_and_commit_nothing() {
    let scratch = Scratch::new("develop-commands-hostile");
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(64, 48)).unwrap();
    let f = files(&scratch);
    let mut v = open(&source);
    let (pixels, steps) = (digests(&v), undo_len(&v));
    // A .cube one byte over Prism's cap: refused on the size, no byte read (a sparse length, no write).
    let huge = scratch.0.join("huge.cube");
    std::fs::File::create(&huge).unwrap().set_len(hsk_studio_prism::MAX_CUBE_BYTES as u64 + 1).unwrap();
    let preset_over = scratch.0.join("over.preset");
    std::fs::File::create(&preset_over).unwrap().set_len(hsk_studio_develop::MAX_PRESET_BYTES as u64 + 1).unwrap();
    let lens_over = scratch.0.join("over.lens");
    std::fs::File::create(&lens_over).unwrap().set_len(hsk_studio_develop::MAX_LENS_PROFILE_BYTES as u64 + 1).unwrap();
    let unknown_key = scratch.0.join("unknown.preset");
    std::fs::write(&unknown_key, "hsk-develop-preset 1\nset no.such.key 1\n").unwrap();
    let missing = text(&scratch.0.join("absent.cube"));
    let many_guides: Vec<Value> = (0..100_000).map(|_| json!([[0.1, 0.1], [0.1, 0.9]])).collect();
    let cases: Vec<(&str, Value, &str)> = vec![
        ("photo.develop.look", json!({"path":text(&huge),"name":"x","domain":"linear"}), "resource_too_large"),
        ("photo.develop.look", json!({"path":missing,"name":"x","domain":"linear"}), "resource_unreadable"),
        ("photo.develop.look", json!({"path":text(&f.cube),"name":"x","domain":"log"}), "parameter_invalid"),
        ("photo.develop.apply_preset", json!({"path":text(&preset_over),"amount":1}), "resource_too_large"),
        ("photo.develop.apply_preset", json!({"path":text(&unknown_key),"amount":1}), "invalid_preset"),
        ("photo.develop.lens_profile", json!({"path":text(&lens_over),"focal_mm":35}), "resource_too_large"),
        ("photo.develop.lens_profile", json!({"path":text(&f.profile),"focal_mm":"NaN"}), "parameter_invalid"),
        ("photo.develop.upright_guides", json!({"guides":many_guides}), "develop_limit"),
        ("photo.develop.upright", json!({"mode":"sideways"}), "parameter_invalid"),
        ("photo.develop.vignette_style", json!({"style":"loud"}), "parameter_invalid"),
        ("photo.develop.grain_seed", json!({"seed":-1}), "parameter_invalid"),
        ("photo.develop.set", json!({"control":"develop.exposure","value":"inf"}), "parameter_invalid"),
        ("photo.develop.set", json!({"control":"develop.no_such","value":1}), "unknown_control"),
        ("photo.develop.set_many", json!({"changes":{}}), "nothing_to_change"),
        ("photo.develop.set_many", json!({"changes":{"develop.exposure":"x"}}), "parameter_invalid"),
        ("photo.develop.add_correction", json!({"source":"radial","zero":[0.1,0.1]}), "parameter_invalid"),
        ("photo.develop.add_correction", json!({"source":"wormhole"}), "parameter_invalid"),
        ("photo.develop.add_correction", json!({"source":"subject"}), "model_unavailable"),
        ("photo.develop.add_mask", json!({"correction":9,"source":"linear"}), "unknown_item"),
        ("photo.develop.add_colour_range", json!({"samples":[[0.1,0.2]],"breadth":0.3}), "parameter_invalid"),
    ];
    for (id, params, code) in cases {
        let refused = row(&mut v, id, params).expect_err(id);
        assert_eq!(refused.code, code, "{id}: {refused:?}");
    }
    assert_eq!((digests(&v), undo_len(&v)), (pixels, steps), "a refused row commits nothing");
}

// ---- process test --------------------------------------------------------------------------------

const PROCESS_LIMIT: Duration = Duration::from_secs(300);

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

struct Live {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: std::io::BufReader<std::process::ChildStdout>,
}

impl Live {
    fn open(source: &Path, diag: &Path) -> Self {
        let args = ["command", source.to_str().unwrap(), "--actor", "lane-develop", "--session", "s-develop", "--stdin"];
        let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        assert!(!matches!(cli::window_args(&owned, &|p| p.is_file()), WindowArgs::Window { .. }), "{args:?} would open a window; not spawned");
        let mut command = Command::new(binary());
        command.args(args).current_dir(diag).env("STUDIO_DIAG_DIR", diag).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().expect("spawn studio-harness");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = std::io::BufReader::new(child.stdout.take().expect("stdout"));
        let mut live = Self { child, stdin, stdout };
        let opened = live.read();
        assert_eq!(opened["ok"], true, "{opened}");
        live
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        std::io::BufRead::read_line(&mut self.stdout, &mut line).expect("a reply line");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("reply is not JSON ({e}): {line}"))
    }

    fn ok(&mut self, id: &str, parameters: Value) -> Value {
        writeln!(self.stdin, "{}", json!({"command":"command","command_id":id,"parameters":parameters})).unwrap();
        self.stdin.flush().unwrap();
        let reply = self.read();
        assert_eq!(reply["ok"], true, "{id}: {} {}", reply["error"], reply["detail"]);
        reply
    }

    /// One step: ok, one named undo step.
    fn step(&mut self, id: &str, parameters: Value) -> Value {
        let reply = self.ok(id, parameters);
        assert!(reply["detail"]["undo_name"].is_string(), "{id} names its undo step: {reply}");
        reply
    }

    fn close(mut self) -> i32 {
        drop(self.stdin);
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().expect("wait") {
                return status.code().expect("an exit code, not a crash");
            }
            if start.elapsed() > PROCESS_LIMIT {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("the session did not end within {PROCESS_LIMIT:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Scenario photo-retouch steps 27 (look), 28 (lens profile), 29 (CA and defringe), 30 (Upright), 40
/// (radial dodge), 41 (brush with a luminance range mask; intersect is not a develop blend, so the range
/// is added), 46 (vignette and grain) and 51 (preset at 80 percent), in order, through the process.
#[test]
fn photo_retouch_develop_steps_run_through_the_process() {
    let scratch = Scratch::new("develop-commands-process");
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(96, 64)).unwrap();
    let f = files(&scratch);
    let mut s = Live::open(&source, &scratch.0);
    s.step("photo.develop.look", json!({"path":text(&f.cube),"name":"negative","domain":"srgb_encoded"}));
    s.step("photo.develop.set", json!({"control":"develop.look.amount","value":0.6}));
    s.step("photo.develop.lens_profile", json!({"path":text(&f.profile),"focal_mm":35}));
    // Profile correction and lateral CA removal are on by default: the row confirms them, no step.
    let toggles = s.ok("photo.develop.lens_toggles", json!({"profile_enable":true,"auto_ca":true}));
    assert!(toggles["detail"]["undo_name"].is_null(), "{toggles}");
    s.step("photo.develop.set_many", json!({"changes":{"develop.optics.defringe.purple_amount":5,"develop.optics.defringe.green_amount":3}}));
    s.step("photo.develop.upright", json!({"mode":"level"}));
    s.step("photo.develop.add_correction", json!({"source":"radial","left":0.3,"top":0.2,"right":0.7,"bottom":0.6}));
    s.step("photo.develop.set", json!({"control":"develop.local.0.exposure","value":0.4}));
    s.step("photo.develop.add_correction", json!({"source":"brush","dabs":[[0.4,0.35],[0.6,0.35]],"radius":0.04}));
    s.step("photo.develop.add_mask", json!({"correction":1,"source":"luminance_range","lo":0.4,"hi":1.0}));
    s.step("photo.develop.set_many", json!({"changes":{"develop.effects.vignette.amount":-40,"develop.effects.grain.amount":25}}));
    s.step("photo.develop.grain_seed", json!({"seed":7}));
    let last = s.step("photo.develop.apply_preset", json!({"path":text(&f.preset),"amount":0.8}));
    assert_eq!(last["state"]["develop"]["local"].as_array().map(Vec::len), Some(2), "{last}");
    assert_eq!(s.close(), 0);
}
