//! SMB-LR-EXPORT-GUI-1 (feature `viewer`): the develop export stage of hsk-studio-develop (image
//! sizing, output sharpening, graphic watermark; lane SMB-LR-EXPORT-SIZING-1) as viewer state, run by
//! the window's develop exports, and the export dialog. Driven through the real `Viewer` (headless egui
//! frames, CX-SAFE-002); the DNG is the synthetic one of `common`. Lightroom's export dialog is the
//! reference and is UNVERIFIED (D5).
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, synthetic_dng};
use hsk_studio_accord::CancellationToken;
use hsk_studio_develop::{
    ExportStage, MarkTransfer, OutputDepth, OutputSizing, ResizeType, SharpenMedia, WatermarkAnchor, WatermarkGraphic, WatermarkSettings,
    WatermarkSize, WatermarkSource, sharpen_row,
};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::develop::DevelopExport;
use hsk_studio_harness::view::develop::export_dialog::{DNG_BUTTON, TIFF_BUTTON};
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_interop_raster as raster;
use serde_json::Value;
use std::path::Path;

/// File digests of today's develop exports of `synthetic_dng(64, 48)` (identity edit), captured at
/// e1e06156 before the stage existed (log `pins-e1e06156.log` of the lane): PNG 16-bit, PNG 8-bit,
/// JPEG quality 90, TIFF (window button defaults), DNG (window button defaults).
const PINS: [(&str, &str); 5] = [
    ("png16", "5081f034f684ec42dc7efa55e3b945c39bc41ffd8c1be578fce62044245b60a7"),
    ("png8", "c2d0d08e0e2bd6a95c4a0eca5e1df5ee55a790dfdb45cd7845e846c134b46ef9"),
    ("jpeg", "4b547d89e9df736f327d8ec07702ea2bc1a63086606e4190133d5e12cf8f8719"),
    ("tiff", "539130c73835a8ca79b892dadff02f58e507b688fbf3c928556439aaa22c03af"),
    ("dng", "cfdff6f168dd6956d1474e7eddb995c1db32803459b7bf0fe106179cae244e5e"),
];

fn format(name: &str) -> DevelopExport {
    match name {
        "png16" => DevelopExport::Png { depth: OutputDepth::Sixteen },
        "png8" => DevelopExport::Png { depth: OutputDepth::Eight },
        "jpeg" => DevelopExport::Jpeg { quality: 90 },
        "tiff" => TIFF_BUTTON,
        _ => DNG_BUTTON,
    }
}

fn open_dng(scratch: &Scratch, width: u32, height: u32) -> Viewer {
    let source = scratch.0.join(format!("synthetic-{width}x{height}.dng"));
    std::fs::write(&source, synthetic_dng(width, height)).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap_or_else(|e| panic!("open: {e}"));
    assert!(v.develop_available(), "{}", v.develop_json());
    v
}

fn export(v: &mut Viewer, path: &Path, f: DevelopExport) -> Value {
    v.export_develop(path, f, false).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn sha_of(path: &Path) -> String {
    hsk_studio_package::sha256_hex_of(&std::fs::read(path).unwrap())
}

/// PNG IHDR width and height.
fn png_size(path: &Path) -> (u32, u32) {
    let b = std::fs::read(path).unwrap();
    assert_eq!(&b[..8], b"\x89PNG\r\n\x1a\n");
    (u32::from_be_bytes(b[16..20].try_into().unwrap()), u32::from_be_bytes(b[20..24].try_into().unwrap()))
}

/// The stage with sizing off (unconstrained), sharpening and watermark at develop's defaults (off).
fn unconstrained() -> ExportStage {
    ExportStage { sizing: OutputSizing { constrain_size: false, ..OutputSizing::default() }, ..ExportStage::default() }
}

// ---- slice 1: headless stage ------------------------------------------------------------------

/// No stage (the default) exports today's bytes for every format; setting a stage and clearing it
/// again returns to them; the DNG export ignores a set stage (its preview is not staged) and says so.
#[test]
fn empty_stage_exports_stay_byte_identical_to_the_pinned_digests() {
    let scratch = Scratch::new("export-stage-pins");
    let mut v = open_dng(&scratch, 64, 48);
    assert_eq!(v.develop_export_stage(), None);
    assert_eq!(v.develop_json()["export_stage"]["on"], false);
    for (name, pin) in PINS {
        let p = scratch.0.join(format!("plain.{name}"));
        let report = export(&mut v, &p, format(name));
        assert_eq!(sha_of(&p), pin, "{name}: unstaged export changed");
        assert!(report["export_stage"].is_null(), "{name}: {report}");
    }
    v.set_develop_export_stage(ExportStage::default()).unwrap();
    assert!(v.develop_export_stage().is_some());
    let p = scratch.0.join("staged.dng");
    let report = export(&mut v, &p, DNG_BUTTON);
    assert_eq!(sha_of(&p), PINS[4].1, "the DNG export does not run the stage");
    assert_eq!(report["export_stage"]["applied"], false, "{report}");
    assert_eq!(report["export_stage"]["reason"], "dng_preview_not_staged");
    v.clear_develop_export_stage().unwrap();
    for (name, pin) in PINS {
        let p = scratch.0.join(format!("cleared.{name}"));
        export(&mut v, &p, format(name));
        assert_eq!(sha_of(&p), pin, "{name}: a cleared stage is the unstaged export");
    }
}

/// Long edge 64 on the 300 x 260 raw: 64 x round_half_up(260 * 64 / 300 = 55.47) = 64 x 55, in the
/// PNG header, the TIFF page and the report; the receipt carries sizes, rule, filter and resolution.
#[test]
fn long_edge_64_gives_the_hand_computed_size_in_the_header_and_the_report() {
    let scratch = Scratch::new("export-stage-long-edge");
    let mut v = open_dng(&scratch, 300, 260);
    assert_eq!(v.develop_json()["window"], serde_json::json!([0, 0, 300, 260]));
    let stage = ExportStage { sizing: OutputSizing { resize_type: ResizeType::LongEdge, max_width: 64.0, ..OutputSizing::default() }, ..ExportStage::default() };
    v.set_develop_export_stage(stage).unwrap();
    let p = scratch.0.join("long.png");
    let r = export(&mut v, &p, DevelopExport::Png { depth: OutputDepth::Sixteen });
    assert_eq!(png_size(&p), (64, 55));
    assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(64), Some(55)), "{r}");
    let s = &r["export_stage"];
    assert_eq!(s["applied"], true);
    assert_eq!((s["source_size"].clone(), s["output_size"].clone()), (serde_json::json!([300, 260]), serde_json::json!([64, 55])));
    assert_eq!((s["fit_rule"].as_str(), s["resized"].as_bool(), s["filter"].as_str()), (Some("long_edge"), Some(true), Some(hsk_studio_develop::RESAMPLE_FILTER)));
    assert_eq!(s["resolution_ppi"].as_f64(), Some(240.0));
    assert!(s["sharpening"].is_null() && s["watermark"].is_null(), "{s}");
    let t = scratch.0.join("long.tif");
    let r = export(&mut v, &t, TIFF_BUTTON);
    let file = hsk_studio_interop_tiff::decode(&std::fs::read(&t).unwrap(), &hsk_studio_interop_tiff::Limits::default(), &CancellationToken::default()).unwrap();
    assert_eq!((file.pages[0].width, file.pages[0].height), (64, 55));
    assert_eq!(r["export_stage"]["output_size"], serde_json::json!([64, 55]));
    let res = file.pages[0].resolution.expect("the staged TIFF writes the stage resolution");
    assert_eq!((f64::from(res.x.0) / f64::from(res.x.1), res.unit), (240.0, 2), "{r}");
    assert!(v.state_json()["status"].to_string().contains("long_edge"), "the status log names the stage: {}", v.state_json()["status"]);
}

/// Output sharpening Screen / Standard changes the pixels against the unstaged export; the receipt
/// row is develop's table row (`sharpen_row(Screen, 2, 240 ppi)`).
#[test]
fn sharpen_screen_standard_changes_pixels_and_reports_the_engine_row() {
    let scratch = Scratch::new("export-stage-sharpen");
    let mut v = open_dng(&scratch, 64, 48);
    let plain = export(&mut v, &scratch.0.join("plain.png"), DevelopExport::Png { depth: OutputDepth::Sixteen });
    let mut stage = unconstrained();
    stage.sharpening.enabled = true;
    stage.sharpening.media = SharpenMedia::Screen;
    stage.sharpening.level = 2;
    v.set_develop_export_stage(stage).unwrap();
    let sharp = export(&mut v, &scratch.0.join("sharp.png"), DevelopExport::Png { depth: OutputDepth::Sixteen });
    assert_ne!(sharp["samples_sha256"], plain["samples_sha256"], "sharpening moved no pixel");
    let s = &sharp["export_stage"];
    assert_eq!((s["resized"].as_bool(), s["output_size"].clone()), (Some(false), serde_json::json!([64, 48])));
    let row = sharpen_row(SharpenMedia::Screen, 2, 240.0).unwrap();
    let got = &s["sharpening"];
    assert_eq!((got["media"].as_str(), got["level"].as_u64(), got["amount"].as_str()), (Some("Screen"), Some(2), Some("Standard")), "{got}");
    assert_eq!((got["sigma_px"].as_f64(), got["kernel_radius"].as_u64(), got["strength"].as_f64()), (Some(row.sigma_px), Some(u64::from(row.kernel_radius)), Some(row.strength)));
    let clip = &s["sharpen_out_of_range"];
    assert!(clip["below_zero"].is_u64() && clip["above_one"].is_u64(), "{s}");
}

/// A 2 x 2 opaque green mark (linear light), proportional 3.125 % of the 64 px width = 2 px, bottom
/// right without insets: the receipt reports (62, 46, 2, 2) and exactly those PNG pixels are green.
#[test]
fn watermark_2x2_lands_at_the_anchor_rect_the_receipt_reports() {
    let scratch = Scratch::new("export-stage-watermark");
    let mut v = open_dng(&scratch, 64, 48);
    let graphic = WatermarkGraphic { width: 2, height: 2, rgba: [0.0, 1.0, 0.0, 1.0].repeat(4), transfer: MarkTransfer::Linear };
    let stage = ExportStage {
        watermark: WatermarkSettings {
            use_watermark: true,
            source: WatermarkSource::Graphic(graphic),
            size: WatermarkSize::Proportional(3.125),
            anchor: WatermarkAnchor::BottomRight,
            ..WatermarkSettings::default()
        },
        ..unconstrained()
    };
    v.set_develop_export_stage(stage).unwrap();
    let p = scratch.0.join("mark.png");
    let r = export(&mut v, &p, DevelopExport::Png { depth: OutputDepth::Eight });
    let w = &r["export_stage"]["watermark"];
    assert_eq!((w["x"].as_i64(), w["y"].as_i64(), w["width"].as_u64(), w["height"].as_u64()), (Some(62), Some(46), Some(2), Some(2)), "{w}");
    assert_eq!((w["anchor"].as_str(), w["size_rule"].as_str()), (Some("bottom_right"), Some("proportional")));
    let img = raster::decode(&std::fs::read(&p).unwrap(), &raster::Limits::default(), &CancellationToken::default()).unwrap();
    let raster::Samples::U8(px) = &img.samples else { panic!("not 8-bit") };
    let at = |x: usize, y: usize| px[(y * 64 + x) * 3..(y * 64 + x) * 3 + 3].to_vec();
    for (x, y) in [(62, 46), (63, 46), (62, 47), (63, 47)] {
        assert_eq!(at(x, y), vec![0, 255, 0], "mark pixel ({x}, {y})");
    }
    for (x, y) in [(61, 46), (62, 45), (61, 47)] {
        assert_ne!(at(x, y), vec![0, 255, 0], "pixel ({x}, {y}) outside the mark");
    }
}
