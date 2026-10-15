//! SMB-HARNESS-DEVELOP-EXPORT (feature `viewer`): open a camera raw / DNG, adjust develop sliders
//! (hsk-studio-develop recipe fields, one chronicle `tiles` step each, undoable), run
//! `develop_to_output` and export PNG / JPEG with the sRGB-v4 profile embedded. Driven headlessly
//! through the `Viewer` methods the develop panel calls and through `cli::run` (no window,
//! CX-SAFE-002). The synthetic DNG below is assembled byte by byte in this file.
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, two_layer_psd};
use hsk_studio_accord::CancellationToken;
use hsk_studio_develop::{DevelopError, Limits, OutputDepth, RangePolicy, Threads};
use hsk_studio_folio::Payload;
use hsk_studio_harness::cli;
use hsk_studio_harness::view::develop::{DevelopExport, SRGB_V4_ICC};
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::{Viewer, widget};
use hsk_studio_interop_raster as raster;
use serde_json::Value;
use std::path::{Path, PathBuf};

// ---- synthetic DNG (little-endian TIFF, raw in IFD0, one uncompressed 16-bit strip) -------------

/// sRGB -> XYZ D50 colorants (ICC sRGB / Lindbloom D50), literal.
const SRGB_D50: [f64; 9] = [0.4360747, 0.3850649, 0.1430804, 0.2225045, 0.7168786, 0.0606169, 0.0139322, 0.0971045, 0.7141733];

fn entry(out: &mut Vec<u8>, extra: &mut Vec<u8>, extra_at: usize, tag: u16, typ: u16, count: u32, data: Vec<u8>) {
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&typ.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    if data.len() <= 4 {
        let mut field = data;
        field.resize(4, 0);
        out.extend_from_slice(&field);
    } else {
        out.extend_from_slice(&((extra_at + extra.len()) as u32).to_le_bytes());
        extra.extend_from_slice(&data);
        if extra.len() % 2 == 1 {
            extra.push(0);
        }
    }
}

fn srational(v: &[f64]) -> Vec<u8> {
    v.iter().flat_map(|x| [((x * 1e7).round() as i32).to_le_bytes(), 10_000_000i32.to_le_bytes()].concat()).collect()
}

/// A `width` x `height` RGGB DNG 1.4 (black 64, white 4095, as-shot neutral, D50 forward matrix)
/// with a smooth per-channel gradient, so every slider visibly changes pixels. Levels stay low (at
/// most about a quarter of white before the as-shot gains) so the developed values, also after
/// exposure +1, stay inside 0..1: render-cpu refuses samples outside the unit range
/// (`sample_range_outside_unit`) and the viewer test reads the composite.
fn synthetic_dng(width: u32, height: u32) -> Vec<u8> {
    let mut samples = Vec::with_capacity((width * height * 2) as usize);
    for y in 0..height {
        for x in 0..width {
            let base = match (x % 2, y % 2) {
                (0, 0) => 120 + (x * 3) % 700,   // R
                (1, 1) => 100 + (y * 5) % 800,   // B
                _ => 150 + ((x + y) * 2) % 900,  // G
            };
            samples.extend_from_slice(&(base as u16).to_le_bytes());
        }
    }
    let inverse = hsk_studio_develop::color::invert(&SRGB_D50).unwrap();
    let tags: Vec<(u16, u16, u32, Vec<u8>)> = vec![
        (254, 4, 1, 0u32.to_le_bytes().to_vec()),
        (256, 4, 1, width.to_le_bytes().to_vec()),
        (257, 4, 1, height.to_le_bytes().to_vec()),
        (258, 3, 1, 16u16.to_le_bytes().to_vec()),
        (259, 3, 1, 1u16.to_le_bytes().to_vec()),
        (262, 3, 1, 32803u16.to_le_bytes().to_vec()),
        (273, 4, 1, vec![0; 4]), // strip offset, patched below
        (277, 3, 1, 1u16.to_le_bytes().to_vec()),
        (278, 4, 1, height.to_le_bytes().to_vec()),
        (279, 4, 1, (samples.len() as u32).to_le_bytes().to_vec()),
        (33421, 3, 2, [2u16.to_le_bytes(), 2u16.to_le_bytes()].concat()),
        (33422, 1, 4, vec![0, 1, 1, 2]),
        (50706, 1, 4, vec![1, 4, 0, 0]),
        (50707, 1, 4, vec![1, 1, 0, 0]),
        (50708, 2, 12, b"Studio Test\0".to_vec()),
        (50714, 3, 1, 64u16.to_le_bytes().to_vec()),
        (50717, 4, 1, 4095u32.to_le_bytes().to_vec()),
        (50721, 10, 9, srational(&inverse)),
        (50728, 5, 3, [0.62f64, 1.0, 0.81].iter().flat_map(|v| [((v * 1e6).round() as u32).to_le_bytes(), 1_000_000u32.to_le_bytes()].concat()).collect()),
        (50778, 3, 1, 23u16.to_le_bytes().to_vec()),
        (50964, 10, 9, srational(&SRGB_D50)),
    ];
    let ifd_at = 8usize;
    let extra_at = ifd_at + 2 + 12 * tags.len() + 4;
    let mut ifd = (tags.len() as u16).to_le_bytes().to_vec();
    let mut extra = Vec::new();
    let mut strip_entry = 0usize;
    for (tag, typ, count, data) in tags {
        if tag == 273 {
            strip_entry = ifd.len() + 8;
        }
        entry(&mut ifd, &mut extra, extra_at, tag, typ, count, data);
    }
    ifd.extend_from_slice(&0u32.to_le_bytes());
    let strip_at = (extra_at + extra.len()) as u32;
    ifd[strip_entry..strip_entry + 4].copy_from_slice(&strip_at.to_le_bytes());
    let mut out = b"II*\0".to_vec();
    out.extend_from_slice(&(ifd_at as u32).to_le_bytes());
    out.extend_from_slice(&ifd);
    out.extend_from_slice(&extra);
    out.extend_from_slice(&samples);
    out
}

// ---- helpers ---------------------------------------------------------------------------------------

fn digests(v: &Viewer, layer: &str) -> Vec<String> {
    let snap = v.run().snapshot().unwrap();
    match &snap.document().layers.iter().find(|l| l.layer_id == layer).unwrap().payload {
        Payload::Raster { tiles } => tiles.iter().map(|t| t.content_digest.digest.clone()).collect(),
        _ => panic!("not raster"),
    }
}

fn revision(v: &Viewer) -> u64 {
    v.run().snapshot().unwrap().document().revision
}

fn value(v: &Viewer, id: &str) -> f64 {
    v.develop_json()["values"][id].as_f64().unwrap_or_else(|| panic!("no {id}: {}", v.develop_json()))
}

fn undo_len(v: &Viewer) -> usize {
    v.run().history()["undo"].as_array().unwrap().len()
}

fn mean_rgb(v: &mut Viewer) -> f64 {
    v.render_if_dirty();
    let f = v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json()));
    let sum: u64 = f.rgba8.chunks_exact(4).flat_map(|p| p[..3].iter().map(|c| u64::from(*c))).sum();
    sum as f64 / (f.rgba8.len() / 4 * 3) as f64
}

fn cli(args: &[&str]) -> (i32, Value) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let o = cli::run(&args);
    (o.exit, serde_json::from_str(&o.stdout).unwrap_or_else(|e| panic!("stdout not JSON ({e}): {}", o.stdout)))
}

fn decode(path: &Path) -> raster::RasterImage {
    raster::decode(&std::fs::read(path).unwrap(), &raster::Limits::default(), &CancellationToken::default()).unwrap()
}

fn corpus_root() -> Option<PathBuf> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config = std::fs::read_to_string(workspace.join(".config/oracle-corpus.toml")).ok()?;
    let value = |key: &str| {
        config.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
        })
    };
    let search = value("search")?;
    let max: usize = value("max-ancestors")?.parse().ok()?;
    let mut at = workspace.canonicalize().ok()?;
    for _ in 0..=max {
        let candidate = at.join(&search);
        if candidate.is_dir() {
            return Some(candidate);
        }
        at = at.parent()?.to_path_buf();
    }
    None
}

// ---- tests -------------------------------------------------------------------------------------------

/// Slider -> chronicle step -> undo/redo -> develop_to_output export, through the Viewer methods the
/// develop panel calls, plus one headless egui frame that draws the panel.
#[test]
fn viewer_develop_sliders_undo_and_export_headlessly() {
    let scratch = Scratch::new("develop");
    let source = scratch.0.join("synthetic.dng");
    let bytes = synthetic_dng(300, 260);
    std::fs::write(&source, &bytes).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap_or_else(|e| panic!("open: {e}"));
    assert!(v.develop_available(), "{}", v.develop_json());
    let state = v.develop_json();
    assert_eq!(state["values_known"], true, "the imported pixels are the identity develop");
    let layer = state["layer_id"].as_str().unwrap().to_string();
    let (r0, imported) = (revision(&v), digests(&v, &layer));
    assert!(imported.len() >= 4, "300x260 spans 2x2 tiles of 256");
    let dark = mean_rgb(&mut v);

    // The identity recipe re-develops to exactly the imported tiles: no step, no new revision.
    assert_eq!(v.set_develop("develop.exposure", 0.0), Ok(r0));
    assert_eq!((digests(&v, &layer), undo_len(&v)), (imported.clone(), 0));

    // Exposure +1: one chronicle step (op `develop`), every tile changed, brighter frame.
    let r1 = v.set_develop("develop.exposure", 1.0).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(r1, r0 + 1);
    let one = digests(&v, &layer);
    assert!(one.iter().zip(&imported).all(|(a, b)| a != b), "every tile re-developed");
    assert_eq!(v.run().ops().last().unwrap()["op"], "develop");
    assert!(mean_rgb(&mut v) > dark + 10.0, "exposure +1 brightens the composite");

    // Two sliders as one step; vibrance switches its group on (develop's gate).
    let r2 = v.set_develop_many(&[("develop.contrast", 30.0), ("develop.vibrance", 25.0)]).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!((r2, undo_len(&v)), (r1 + 1, 2));
    assert_eq!(v.develop_json()["enables"]["enable_colour_adjustments"], true);
    let two = digests(&v, &layer);
    assert_ne!(two, one);

    // Undo/redo are the chronicle history; the slider values follow the pixels.
    v.undo().unwrap();
    assert_eq!((digests(&v, &layer), value(&v, "develop.exposure"), value(&v, "develop.contrast")), (one.clone(), 1.0, 0.0));
    v.undo().unwrap();
    assert_eq!((digests(&v, &layer), value(&v, "develop.exposure")), (imported.clone(), 0.0));
    assert_eq!(v.develop_json()["values_known"], true);
    v.redo().unwrap();
    v.redo().unwrap();
    assert_eq!((digests(&v, &layer), value(&v, "develop.vibrance"), value(&v, "develop.contrast")), (two.clone(), 25.0, 30.0));

    // One egui frame draws the develop panel with stable ids and commits nothing.
    let ops = v.run().ops().len();
    let ctx = egui::Context::default();
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0))), ..Default::default() };
    let _ = ctx.run(input, |ctx| v.ui(ctx));
    for name in ["develop.exposure", "develop.contrast", "develop.sharpen_amount", widget::DEVELOP_PANEL, widget::DEVELOP_EXPORT_PNG, widget::DEVELOP_EXPORT_JPEG] {
        assert!(v.drawn().iter().any(|d| d == name), "{name} not drawn: {:?}", v.drawn());
    }
    assert_eq!(v.run().ops().len(), ops);

    // Export: the PNG samples are exactly develop_to_output of the shown recipe (independent call).
    let png = scratch.0.join("out.png");
    let report = v.export_develop(&png, DevelopExport::Png { depth: OutputDepth::Sixteen }, false).unwrap_or_else(|e| panic!("{e}"));
    let recipe = v.develop_recipe().unwrap();
    let raw = hsk_studio_develop::decode_container(&bytes).unwrap();
    let (plane, _) = raw.cfa_plane(raw.default_window(), RangePolicy::ClampReported, 1, Limits::LARGE).unwrap();
    let (direct, _) = hsk_studio_develop::develop_to_output(&plane, &recipe, r2, OutputDepth::Sixteen, SRGB_V4_ICC, Threads::single(), &CancellationToken::default()).unwrap();
    let decoded = decode(&png);
    assert_eq!((decoded.width, decoded.height, decoded.depth(), decoded.icc.as_deref()), (direct.width, direct.height, 16, Some(SRGB_V4_ICC)));
    let raster::Samples::U16(samples) = &decoded.samples else { panic!("16-bit samples") };
    let expected: Vec<u16> = direct.data.chunks_exact(2).map(|b| u16::from_be_bytes([b[0], b[1]])).collect();
    assert!(samples == &expected, "exported PNG == develop_to_output");
    assert_eq!(report["values"]["develop.contrast"], 30.0);
    let jpeg = scratch.0.join("out.jpg");
    v.export_develop(&jpeg, DevelopExport::Jpeg { quality: 92 }, false).unwrap_or_else(|e| panic!("{e}"));
    let decoded = decode(&jpeg);
    assert_eq!((decoded.width, decoded.height, decoded.icc.as_deref()), (direct.width, direct.height, Some(SRGB_V4_ICC)));
    // The opened source is never overwritten without the flag.
    assert_eq!(v.export_develop(&source, DevelopExport::Jpeg { quality: 90 }, false).unwrap_err().code, "export_over_source");
}

/// CLI round trip on the smallest corpus DNG: open, three sliders as one step, develop_to_output,
/// PNG 16-bit (byte-identical for 1 and 4 develop threads) and JPEG. That the sliders change the
/// pixels is proven on the synthetic DNG (`viewer_develop_sliders_undo_and_export_headlessly`).
#[test]
fn cli_develop_round_trip_on_a_corpus_dng_is_thread_invariant() {
    const FILE: &str = "dng/canon-eos-5d3-mlv-14bit-lossless.dng";
    const SHA256: &str = "1d77dcc6cdb839b10c1948f0b4471984ab48cf15e973fb77520584cee72b603e";
    let Some(root) = corpus_root() else {
        println!("NOT_PROVEN: cli_develop_round_trip reason=corpus_root_absent");
        return;
    };
    let source = root.join(FILE);
    let Ok(bytes) = std::fs::read(&source) else {
        println!("NOT_PROVEN: cli_develop_round_trip reason=file_absent {FILE}");
        return;
    };
    let digest = hsk_studio_package::sha256_hex_of(&bytes);
    if digest != SHA256 {
        println!("NOT_PROVEN: cli_develop_round_trip reason=sha256_mismatch {FILE} {digest}");
        return;
    }
    let scratch = Scratch::new("develop-cli");
    let src = source.to_string_lossy().into_owned();
    let out = |name: &str| scratch.0.join(name).to_string_lossy().into_owned();
    let adjust = ["--adjust", "develop.exposure", "0.75", "--adjust", "develop.contrast", "25", "--adjust", "develop.vibrance", "15"];
    let run = |threads: &str, format: &[&str], name: &str| {
        let path = out(name);
        let mut args = vec!["develop", src.as_str(), path.as_str()];
        args.extend_from_slice(format);
        args.extend_from_slice(&adjust);
        args.extend_from_slice(&["--threads", threads]);
        let (exit, report) = cli(&args);
        assert_eq!(exit, 0, "{report}");
        (std::fs::read(&path).unwrap(), report)
    };
    let (png1, r1) = run("1", &["--png"], "t1.png");
    let (png4, r4) = run("4", &["--png"], "t4.png");
    assert_eq!((r1["export"]["threads"].as_u64(), r4["export"]["threads"].as_u64()), (Some(1), Some(4)));
    assert!(png1 == png4, "PNG identical for 1 and 4 threads");
    assert_eq!(r1["export"]["samples_sha256"], r4["export"]["samples_sha256"]);
    assert_eq!(r1["step"]["history"]["undo"].as_array().map(Vec::len), Some(1), "the three sliders are one chronicle step: {}", r1["step"]);
    assert_eq!(r1["develop"]["values"]["develop.exposure"], 0.75);
    let decoded = decode(Path::new(&out("t1.png")));
    assert_eq!((decoded.width, decoded.height, decoded.depth(), decoded.icc.as_deref()), (1920, 818, 16, Some(SRGB_V4_ICC)));
    let (jpg1, rj) = run("4", &["--jpeg", "--quality", "85"], "t4.jpg");
    let decoded = decode(Path::new(&out("t4.jpg")));
    assert_eq!((decoded.width, decoded.height, decoded.icc.as_deref()), (1920, 818, Some(SRGB_V4_ICC)));
    assert_eq!((rj["export"]["depth_bits"].as_u64(), rj["export"]["subsampling"].as_str()), (Some(8), Some("4:2:0")));
    println!("cli develop: png {} bytes, jpeg {} bytes, samples {}", png1.len(), jpg1.len(), r1["export"]["samples_sha256"]);
}

/// Hostile input: a truncated raw is develop's own decode refusal (viewer and CLI); out-of-range,
/// non-finite and unknown slider values are typed refusals that commit nothing; a non-raw document
/// has no develop route.
#[test]
fn hostile_develop_inputs_are_typed_refusals() {
    let scratch = Scratch::new("develop-hostile");
    let bytes = synthetic_dng(64, 48);
    let truncated = &bytes[..bytes.len() / 3];
    let Err(refused) = hsk_studio_develop::decode_container(truncated) else { panic!("truncated raw decoded") };
    let direct = refused.code();
    let path = scratch.0.join("truncated.dng");
    std::fs::write(&path, truncated).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    assert!(v.open(&path).is_err());
    assert_eq!(v.run().ops().last().unwrap()["error"], direct, "the import passes develop's code through");
    assert!(!v.develop_available());
    let (p, t) = (path.to_string_lossy().into_owned(), scratch.0.join("t.png").to_string_lossy().into_owned());
    let (exit, report) = cli(&["develop", &p, &t, "--png"]);
    assert_eq!((exit, report["error"].as_str()), (1, Some(direct)), "{report}");

    let good = scratch.0.join("good.dng");
    std::fs::write(&good, &bytes).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&good).unwrap_or_else(|e| panic!("{e}"));
    let (r0, ops) = (revision(&v), v.run().ops().len());
    let refusals = [
        ("develop.contrast", 250.0, DevelopError::InvalidAdjustment.code()),
        ("develop.sharpen_amount", -5.0, DevelopError::InvalidAdjustment.code()),
        ("develop.exposure", f64::NAN, DevelopError::NonFinite.code()),
        ("develop.exposure", f64::INFINITY, DevelopError::NonFinite.code()),
        ("develop.exposure", 5000.0, DevelopError::StageOverflow.code()),
        ("develop.temperature", -100.5, DevelopError::InvalidAdjustment.code()),
        ("develop.nope", 1.0, "unknown_control"),
    ];
    for (id, value, code) in refusals {
        assert_eq!(v.set_develop(id, value).map_err(|r| r.code), Err(code), "{id} = {value}");
    }
    assert_eq!(v.set_develop_many(&[]).unwrap_err().code, "nothing_to_change");
    assert_eq!(v.set_develop_threads(0).unwrap_err().code, DevelopError::InvalidLimits.code());
    assert_eq!((revision(&v), v.run().ops().len(), undo_len(&v)), (r0, ops, 0), "refusals commit nothing");
    assert_eq!(value(&v, "develop.contrast"), 0.0);
    let g = good.to_string_lossy().into_owned();
    let o = scratch.0.join("o.png").to_string_lossy().into_owned();
    let (exit, report) = cli(&["develop", &g, &o, "--png", "--adjust", "develop.contrast", "250"]);
    assert_eq!((exit, report["error"].as_str()), (1, Some("invalid_adjustment")), "{report}");
    assert!(!Path::new(&o).exists(), "a refused develop writes nothing");
    assert_eq!(cli(&["develop", &g, &o, "--png", "--adjust", "develop.contrast", "abc"]).0, 2);
    assert_eq!(cli(&["develop", &g, &o, "--jpeg", "--depth", "16"]).0, 2);
    assert_eq!(cli(&["develop", &g, &g, "--png"]).0, 2, "OUT over SOURCE without --overwrite");

    // A PSD has no develop route.
    let psd = scratch.0.join("two.psd");
    std::fs::write(&psd, two_layer_psd()).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&psd).unwrap();
    assert!(!v.develop_available());
    assert_eq!(v.set_develop("develop.exposure", 1.0).unwrap_err().code, "not_a_raw_document");
    assert_eq!(v.export_develop(&scratch.0.join("p.png"), DevelopExport::Png { depth: OutputDepth::Eight }, false).unwrap_err().code, "not_a_raw_document");
    let p = psd.to_string_lossy().into_owned();
    let (exit, report) = cli(&["develop", &p, &o, "--png"]);
    assert_eq!((exit, report["error"].as_str()), (1, Some("not_a_raw_document")), "{report}");
}
