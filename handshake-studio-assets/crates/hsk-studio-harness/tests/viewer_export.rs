//! SMB-CONVERT-RASTER slice 2 (feature `viewer`): export of the composited document as PNG / JPEG,
//! headless (render-cpu; no window, no GPU device). A PNG source with straight alpha (written by
//! interop-raster's encoder) is imported through the viewer, composited, exported, and the exported file
//! is decoded by interop-raster and compared with the source pixels (PNG: within 1 per channel where
//! alpha > 0; JPEG: against the source flattened on white, PSNR bound). The CLI `export` command and its
//! refusals are driven through `cli::run`.
#![cfg(feature = "viewer")]

use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::cli;
use hsk_studio_harness::view::export::ExportFormat;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::{Viewer, widget};
use hsk_studio_interop_raster as raster;
use serde_json::Value;
use std::path::PathBuf;

/// Test scratch directory under TMP, removed on drop. TMP must be on D: (studio-env.sh sets it).
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let base = std::env::temp_dir();
        let text = base.to_string_lossy().to_ascii_uppercase();
        assert!(!(cfg!(windows) && text.starts_with("C:")), "TMP is on C: ({}); run through studio-env.sh", base.display());
        let dir = base.join(format!("hsk-harness-export-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const W: u32 = 19;
const H: u32 = 13;

/// RGBA 8-bit source: smooth colour ramps, alpha 255 on the left half, a ramp down to 0 on the right.
fn source_rgba() -> Vec<u8> {
    let mut v = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let a = if x < W / 2 { 255 } else { 255 - (x - W / 2) * 25 };
            v.extend([(x * 13) as u8, (y * 19) as u8, (x * 5 + y * 7) as u8, a as u8]);
        }
    }
    v
}

fn source_png() -> Vec<u8> {
    let samples = raster::Samples::U8(source_rgba());
    let req = raster::PngEncodeRequest {
        width: W,
        height: H,
        colour: raster::Colour::Rgba,
        samples: &samples,
        icc: None,
        compression: raster::PngCompression::Fast,
    };
    raster::encode_png(&req).unwrap()
}

fn decode(path: &std::path::Path) -> raster::RasterImage {
    raster::decode(&std::fs::read(path).unwrap(), &raster::Limits::default(), &CancellationToken::default()).unwrap()
}

fn cli_json(args: &[&str]) -> (i32, Value) {
    let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let out = cli::run(&args);
    (out.exit, serde_json::from_str(&out.stdout).unwrap_or(Value::Null))
}

#[test]
fn viewer_exports_the_composite_as_png_and_jpeg() {
    let scratch = Scratch::new("viewer");
    let source = scratch.0.join("src.png");
    std::fs::write(&source, source_png()).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(v.receipt().unwrap()["source_format"], "png");
    let src = source_rgba();

    // PNG: transparency present -> RGBA; sRGB 8-bit -> linear working space -> sRGB 8-bit within 1.
    let png = scratch.0.join("out.png");
    let report = v.export(&png, ExportFormat::Png, false).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!((report["format"].as_str(), report["colour"].as_str(), report["backend"].as_str()), (Some("png"), Some("Rgba"), Some("cpu")), "{report}");
    let img = decode(&png);
    assert_eq!((img.width, img.height, img.colour), (W, H, raster::Colour::Rgba));
    let raster::Samples::U8(got) = &img.samples else { panic!("8-bit") };
    let mut worst = 0u8;
    for (s, g) in src.chunks_exact(4).zip(got.chunks_exact(4)) {
        worst = worst.max(s[3].abs_diff(g[3]));
        if s[3] > 0 {
            for c in 0..3 {
                worst = worst.max(s[c].abs_diff(g[c]));
            }
        }
    }
    println!("EXPORT png max abs diff vs source {worst}");
    assert!(worst <= 1, "png export differs by {worst}");

    // JPEG: flattened on white (note), 4:4:4 at q95; compared with the source flattened the same way.
    let jpg = scratch.0.join("out.jpg");
    let report = v.export(&jpg, ExportFormat::Jpeg { quality: 95 }, false).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(report["subsampling"], "4:4:4");
    assert!(report["notes"].as_array().unwrap().iter().any(|n| n == "alpha_flattened_on_white"), "{report}");
    let img = decode(&jpg);
    assert_eq!((img.width, img.height, img.colour), (W, H, raster::Colour::Rgb));
    let raster::Samples::U8(got) = &img.samples else { panic!("8-bit") };
    let flat: Vec<f64> = src
        .chunks_exact(4)
        .flat_map(|p| {
            let a = f64::from(p[3]) / 255.0;
            [0, 1, 2].map(|c| (f64::from(p[c]) * a + 255.0 * (1.0 - a)).round())
        })
        .collect();
    let mse = flat.iter().zip(got).map(|(a, b)| (a - f64::from(*b)).powi(2)).sum::<f64>() / flat.len() as f64;
    let psnr = 10.0 * (255.0f64 * 255.0 / mse).log10();
    println!("EXPORT jpeg q95 PSNR vs flattened source {psnr:.2} dB");
    assert!(psnr > 38.0, "jpeg export PSNR {psnr:.2}");

    // The opened source is never replaced without overwrite; the UI draws the export buttons.
    let before = std::fs::read(&source).unwrap();
    assert!(v.export(&source, ExportFormat::Png, false).is_err());
    assert_eq!(std::fs::read(&source).unwrap(), before);
    let ctx = egui::Context::default();
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 800.0))), ..Default::default() };
    let _ = ctx.run(input, |ctx| v.ui(ctx));
    for name in [widget::EXPORT_PNG_BUTTON, widget::EXPORT_JPEG_BUTTON] {
        assert!(v.drawn().iter().any(|d| d == name), "{name} not drawn");
    }
    // Only the files this test wrote exist (atomic write leaves no temp file).
    let mut names: Vec<String> = std::fs::read_dir(&scratch.0).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, ["out.jpg", "out.png", "src.png"]);
}

#[test]
fn cli_export_writes_png_and_jpeg_and_refuses_bad_requests() {
    let scratch = Scratch::new("cli");
    let source = scratch.0.join("src.png");
    std::fs::write(&source, source_png()).unwrap();
    let s = source.to_str().unwrap();
    let png = scratch.0.join("a.png");
    let (exit, report) = cli_json(&["export", s, "--png", png.to_str().unwrap()]);
    assert_eq!((exit, report["verdict"].as_str()), (0, Some("pass")), "{report}");
    assert_eq!(decode(&png).width, W);
    let jpg = scratch.0.join("a.jpg");
    let (exit, report) = cli_json(&["export", s, "--jpeg", "--quality", "70", jpg.to_str().unwrap()]);
    assert_eq!((exit, report["export"]["subsampling"].as_str(), report["export"]["quality"].as_u64()), (0, Some("4:2:0"), Some(70)), "{report}");
    assert_eq!(decode(&jpg).colour, raster::Colour::Rgb);
    // Usage errors (exit 2): no format, both formats, quality on PNG, quality out of range, OUT == SOURCE.
    let out = scratch.0.join("b.png");
    let o = out.to_str().unwrap();
    for args in [
        vec!["export", s, o],
        vec!["export", s, "--png", "--jpeg", o],
        vec!["export", s, "--png", "--quality", "50", o],
        vec!["export", s, "--jpeg", "--quality", "101", o],
        vec!["export", s, "--png", s],
        vec!["export", s, "--png"],
    ] {
        let (exit, report) = cli_json(&args);
        assert_eq!((exit, report["error"].as_str()), (2, Some("usage")), "{args:?}: {report}");
    }
    assert!(!out.exists());
    // An unopenable source is a typed failure (exit 1), nothing written.
    let junk = scratch.0.join("junk.bin");
    std::fs::write(&junk, b"not an image").unwrap();
    let (exit, report) = cli_json(&["export", junk.to_str().unwrap(), "--png", o]);
    assert_eq!((exit, report["error"].as_str()), (1, Some("open_failed")), "{report}");
    assert!(!out.exists());
}
