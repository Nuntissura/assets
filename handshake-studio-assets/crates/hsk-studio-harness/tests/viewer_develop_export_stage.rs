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

// ---- slice 2: the export dialog ---------------------------------------------------------------

fn frame(v: &mut Viewer, ctx: &egui::Context, events: Vec<egui::Event>) {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 4000.0))),
        events,
        ..Default::default()
    };
    let _ = ctx.run(input, |ctx| v.ui(ctx));
}

fn drew(v: &Viewer, name: &str) -> bool {
    v.drawn().iter().any(|d| d == name)
}

fn stage_state(v: &Viewer) -> Value {
    v.develop_json()["export_stage"].clone()
}

fn field(v: &mut Viewer, key: &str, value: &str) {
    v.set_develop_export_field(&ew::key(key), value).unwrap_or_else(|e| panic!("{key} = {value}: {e}"));
}

use hsk_studio_harness::view::develop::export_dialog::widget as ew;

/// The switch shows the three STU-IO-135 groups with every key row (class and default source shown),
/// the preview follows the fields: develop's default box 1000 x 1000 fits 300 x 260 to 1000 x 867;
/// LongEdge plus 64 typed into the max_width field through egui gives 64 x 55, the exported size.
#[test]
fn dialog_fields_drive_the_size_preview() {
    let scratch = Scratch::new("export-dialog-preview");
    let mut v = open_dng(&scratch, 300, 260);
    let ctx = egui::Context::default();
    frame(&mut v, &ctx, vec![]);
    assert!(drew(&v, ew::STAGE) && drew(&v, ew::EXPORT_TIFF) && drew(&v, ew::EXPORT_DNG), "{:?}", v.drawn());
    assert!(!drew(&v, &ew::key("max_width")), "rows are hidden while the stage is off");
    field(&mut v, "stage", "true");
    assert_eq!(v.develop_export_stage(), Some(&ExportStage::default()), "switching on starts from develop's defaults");
    frame(&mut v, &ctx, vec![]);
    for k in hsk_studio_develop::EXPORT_STAGE_KEYS {
        assert!(drew(&v, &ew::key(k.key)), "{} not drawn", k.key);
    }
    for g in ["image_sizing", "watermarking", "output_sharpening"] {
        assert!(drew(&v, &ew::group(g)), "group {g} not drawn");
    }
    let s = stage_state(&v);
    assert_eq!(s["groups"], serde_json::json!(["image_sizing", "watermarking", "output_sharpening"]), "STU-IO-135 order");
    let row = |key: &str| s["keys"].as_array().unwrap().iter().find(|r| r["key"] == key).unwrap().clone();
    assert_eq!((row("constrain_size")["storage"].as_str(), row("constrain_size")["default_source"].as_str()), (Some("stored_setting"), Some("studio_declared")));
    assert_eq!(row("megapixels")["storage"], "unclassified");
    assert_eq!((s["preview"].as_str(), s["fit_rule"].as_str(), s["export_enabled"].as_bool()), (Some("1000 x 867 px"), Some("width_height"), Some(true)), "{s}");
    assert!(drew(&v, ew::PREVIEW));

    field(&mut v, "resize_type", "LongEdge");
    ctx.memory_mut(|m| m.request_focus(hsk_studio_harness::view::widget::id(&ew::key("max_width"))));
    let select_all = egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND };
    frame(&mut v, &ctx, vec![select_all, egui::Event::Text("64".into())]);
    frame(&mut v, &ctx, vec![]);
    assert_eq!(v.develop_export_stage().map(|s| s.sizing.max_width), Some(64.0), "typed into the field: {}", stage_state(&v));
    let s = stage_state(&v);
    assert_eq!((s["preview"].as_str(), s["preview_size"].clone()), (Some("64 x 55 px"), serde_json::json!([64, 55])));
    let p = scratch.0.join("preview.png");
    export(&mut v, &p, DevelopExport::Png { depth: OutputDepth::Eight });
    assert_eq!(png_size(&p), (64, 55), "the preview is the exported size");
}

/// A percentage of 0 is develop's typed refusal shown inline: no preview, export disabled and refused
/// with the same code (DNG still exports, unstaged); text that is not a number is refused typed and
/// kept; 50 % gives 32 x 24. Nothing is clamped in the view.
#[test]
fn invalid_percentage_shows_the_typed_error_and_disables_export() {
    let scratch = Scratch::new("export-dialog-invalid");
    let mut v = open_dng(&scratch, 64, 48);
    let ctx = egui::Context::default();
    field(&mut v, "stage", "true");
    field(&mut v, "resize_type", "Percentage");
    field(&mut v, "resize_percentage", "0");
    assert_eq!(v.develop_export_stage().map(|s| s.sizing.percentage), Some(0.0), "stored unclamped");
    let s = stage_state(&v);
    assert_eq!((s["error"]["code"].as_str(), s["error"]["detail"].as_str()), (Some("export_setting_out_of_range"), Some("resize_percentage")), "{s}");
    assert_eq!((s["export_enabled"].as_bool(), s["preview"].is_null()), (Some(false), true));
    frame(&mut v, &ctx, vec![]);
    assert!(drew(&v, ew::ERROR), "the typed error is shown inline");
    for f in [DevelopExport::Png { depth: OutputDepth::Sixteen }, TIFF_BUTTON] {
        let r = v.export_develop(&scratch.0.join(format!("refused.{}", f.name())), f, false).unwrap_err();
        assert_eq!(r.code, "export_setting_out_of_range", "{}", f.name());
    }
    let dng = scratch.0.join("still.dng");
    export(&mut v, &dng, DNG_BUTTON);
    assert_eq!(sha_of(&dng), PINS[4].1);

    let r = v.set_develop_export_field(&ew::key("resize_percentage"), "fifty").unwrap_err();
    assert_eq!(r.code, "export_field_not_a_number");
    let s = stage_state(&v);
    assert_eq!((s["error"]["code"].as_str(), s["unparsed"]["resize_percentage"].as_str(), s["export_enabled"].as_bool()), (Some("export_field_not_a_number"), Some("fifty"), Some(false)));
    let key = s["keys"].as_array().unwrap().iter().find(|r| r["key"] == "resize_percentage").unwrap()["value"].clone();
    assert_eq!(key, "fifty", "the field keeps the typed text");
    assert_eq!(v.export_develop(&scratch.0.join("nan.png"), DevelopExport::Png { depth: OutputDepth::Eight }, false).unwrap_err().code, "export_field_not_a_number");
    field(&mut v, "resize_percentage", "50");
    let s = stage_state(&v);
    assert_eq!((s["preview"].as_str(), s["export_enabled"].as_bool(), s["error"].is_null()), (Some("32 x 24 px"), Some(true), true), "{s}");
    assert_eq!(v.set_develop_export_field("develop.export.no_such_key", "1").unwrap_err().code, "unknown_export_field");
}

/// The window's TIFF and DNG buttons (drawn beside PNG and JPEG) write files whose headers parse: the
/// TIFF through interop-tiff (16-bit, sRGB profile, staged to long edge 32 = 32 x 24), the DNG through
/// develop's own container reader (full raw, stage not applied).
#[test]
fn export_tiff_and_dng_buttons_write_files_whose_headers_parse() {
    let scratch = Scratch::new("export-dialog-buttons");
    let mut v = open_dng(&scratch, 64, 48);
    let ctx = egui::Context::default();
    frame(&mut v, &ctx, vec![]);
    for name in [hsk_studio_harness::view::widget::DEVELOP_EXPORT_PNG, hsk_studio_harness::view::widget::DEVELOP_EXPORT_JPEG, ew::EXPORT_TIFF, ew::EXPORT_DNG] {
        assert!(drew(&v, name), "{name} not drawn");
    }
    field(&mut v, "stage", "true");
    field(&mut v, "resize_type", "LongEdge");
    field(&mut v, "max_width", "32");
    let base = std::path::PathBuf::from(v.save_input.trim());
    let tif = base.with_extension("develop.tif");
    let r = export(&mut v, &tif, TIFF_BUTTON);
    let file = hsk_studio_interop_tiff::decode(&std::fs::read(&tif).unwrap(), &hsk_studio_interop_tiff::Limits::default(), &CancellationToken::default()).unwrap();
    let page = &file.pages[0];
    assert_eq!((page.width, page.height, page.bits_per_sample), (32, 24, 16), "{r}");
    assert_eq!(page.icc.as_deref(), Some(hsk_studio_harness::view::develop::SRGB_V4_ICC));
    let dng = base.with_extension("develop.dng");
    let r = export(&mut v, &dng, DNG_BUTTON);
    assert_eq!(r["export_stage"]["applied"], false);
    let bytes = std::fs::read(&dng).unwrap();
    assert!(bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*"), "TIFF/DNG header");
    hsk_studio_develop::decode_container(&bytes).unwrap_or_else(|e| panic!("DNG does not parse: {e:?}"));
    assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(64), Some(48)));
}

/// A text watermark is develop's typed refusal (needs hsk-studio-type), inline and at export; a PNG
/// graphic loaded through the dialog's path replaces it and exports with its placement.
#[test]
fn text_watermark_is_refused_typed_and_a_graphic_file_is_placed() {
    let scratch = Scratch::new("export-dialog-watermark");
    let mut v = open_dng(&scratch, 64, 48);
    let ctx = egui::Context::default();
    assert_eq!(v.load_develop_watermark(&scratch.0.join("none.png")).unwrap_err().code, "export_stage_off");
    field(&mut v, "stage", "true");
    field(&mut v, "constrain_size", "false");
    field(&mut v, "use_watermark", "true");
    field(&mut v, "watermark_source", "text");
    field(&mut v, "watermark_text", "Handshake Studio");
    let s = stage_state(&v);
    assert_eq!((s["error"]["code"].as_str(), s["error"]["detail"].as_str(), s["export_enabled"].as_bool()), (Some("text_watermark_unsupported"), Some("hsk-studio-type"), Some(false)), "{s}");
    frame(&mut v, &ctx, vec![]);
    assert!(drew(&v, ew::WATERMARK_TEXT) && drew(&v, ew::ERROR));
    assert_eq!(v.export_develop(&scratch.0.join("text.png"), DevelopExport::Png { depth: OutputDepth::Eight }, false).unwrap_err().code, "text_watermark_unsupported");

    let samples = raster::Samples::U8([255, 0, 0, 255].repeat(4));
    let req = raster::PngEncodeRequest { width: 2, height: 2, colour: raster::Colour::Rgba, samples: &samples, icc: None, compression: raster::PngCompression::Balanced };
    let mark = scratch.0.join("mark.png");
    std::fs::write(&mark, raster::encode_png(&req).unwrap()).unwrap();
    v.load_develop_watermark(&mark).unwrap();
    field(&mut v, "watermark_anchor", "top_left");
    field(&mut v, "watermark_size", "proportional");
    field(&mut v, "watermark_size_value", "3.125");
    let s = stage_state(&v);
    assert!(s["error"].is_null(), "{s}");
    assert_eq!((s["watermark_graphic"]["width"].as_u64(), s["watermark_graphic"]["height"].as_u64()), (Some(2), Some(2)));
    frame(&mut v, &ctx, vec![]);
    assert!(drew(&v, ew::WATERMARK_GRAPHIC));
    let r = export(&mut v, &scratch.0.join("graphic.png"), DevelopExport::Png { depth: OutputDepth::Eight });
    let w = &r["export_stage"]["watermark"];
    assert_eq!((w["x"].as_i64(), w["y"].as_i64(), w["width"].as_u64(), w["height"].as_u64(), w["anchor"].as_str()), (Some(0), Some(0), Some(2), Some(2), Some("top_left")), "{w}");
}
