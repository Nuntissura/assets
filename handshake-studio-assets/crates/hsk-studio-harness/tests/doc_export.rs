//! Lane SMB-DOC-EXPORT-1 (feature `viewer`): the document export recipe (`view::doc_export`), scenario
//! photo-retouch steps 105-110, 112 and 116. Headless: render-cpu, `cli::run` in process, files under TMP. Each test
//! decodes what was written through the codec owners and compares it with the document or with develop's own
//! sizing rule, never with the report alone.
#![cfg(feature = "viewer")]

use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::cli;
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::develop::SRGB_V4_ICC;
use hsk_studio_harness::view::doc_export::{DocExportRecipe, DocFormat};
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness_core::batch::{Automation, BatchJob, ErrorPolicy, OutputTarget, OverwritePolicy, SourceSet, TemplateToken};
use hsk_studio_interop_psd as psd;
use hsk_studio_interop_raster as raster;
use hsk_studio_interop_tiff as tiff;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Test scratch directory under TMP, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-doc-export-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const W: u32 = 24;
const H: u32 = 16;

/// Opaque RGB ramps.
fn source_rgb() -> Vec<u8> {
    let mut v = Vec::new();
    for y in 0..H {
        for x in 0..W {
            v.extend([(x * 10) as u8, (y * 15) as u8, (x * 3 + y * 7) as u8]);
        }
    }
    v
}

fn png(width: u32, height: u32, colour: raster::Colour, samples: Vec<u8>) -> Vec<u8> {
    let samples = raster::Samples::U8(samples);
    raster::encode_png(&raster::PngEncodeRequest { width, height, colour, samples: &samples, icc: None, compression: raster::PngCompression::Fast }).unwrap()
}

fn opened(s: &Scratch) -> (Viewer, PathBuf) {
    let source = s.path("src.png");
    std::fs::write(&source, png(W, H, raster::Colour::Rgb, source_rgb())).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap_or_else(|e| panic!("{e}"));
    (v, source)
}

fn recipe(v: Value) -> DocExportRecipe {
    DocExportRecipe::from_json(&v, None, &CancellationToken::default()).unwrap_or_else(|r| panic!("{r}"))
}

fn tiff_page(path: &Path) -> tiff::TiffPage {
    let file = tiff::decode(&std::fs::read(path).unwrap(), &tiff::Limits::default(), &CancellationToken::default()).unwrap();
    file.pages.into_iter().next().unwrap()
}

fn revision(v: &Viewer) -> u64 {
    v.run().snapshot().unwrap().document().revision
}

fn cli_json(args: &[&str]) -> (i32, Value) {
    let out = cli::run(&args.iter().map(|s| (*s).to_string()).collect::<Vec<_>>());
    (out.exit, serde_json::from_str(&out.stdout).unwrap_or(Value::Null))
}

/// Step 109: a 16-bit ZIP TIFF of the composite, tagged with the sRGB profile and the resolution, whose decoded
/// samples are the document's (8-bit source, so the high byte is within 1).
#[test]
fn tiff16_is_tagged_and_decodes_to_the_document() {
    let s = Scratch::new("tiff16");
    let (mut v, source) = opened(&s);
    let out = s.path("print.tif");
    let r = recipe(json!({"format": "tiff", "depth": 16, "compression": "zip", "predictor": true, "stage": {"resolution": 300, "resolution_unit": "ppinch"}}));
    let before = revision(&v);
    let report = v.export_document(&out, &r, false).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(revision(&v), before, "an export never edits the document");
    assert_eq!((report["format"].as_str(), report["depth"].as_u64(), report["colour"].as_str()), (Some("tiff"), Some(16), Some("Rgb")), "{report}");
    let page = tiff_page(&out);
    assert_eq!((page.info.width, page.info.height, page.info.bits_per_sample, page.info.samples_per_pixel), (W, H, 16, 3));
    assert_eq!(page.info.icc.as_deref(), Some(SRGB_V4_ICC), "embedded profile");
    assert_eq!(page.info.compression, 8, "ZIP");
    assert_eq!(page.info.predictor, 2);
    let res = page.info.resolution.expect("resolution");
    assert_eq!((res.x, res.unit), ((30000, 100), 2));
    let tiff::Samples::U16(got) = page.pixels() else { panic!("16-bit") };
    let worst = source_rgb().iter().zip(got).map(|(s, g)| s.abs_diff((g >> 8) as u8)).max().unwrap();
    println!("DOC_EXPORT tiff16 max abs diff (high byte) vs source {worst}");
    assert!(worst <= 1, "tiff16 differs by {worst}");
    // The op record carries the run's attribution; nothing is replaced without overwrite.
    let op = v.run().ops().iter().rev().find(|o| o["op"] == "export_document").cloned().unwrap();
    assert!(op["ok"] == true && op["actor"].is_object(), "{op}");
    let bytes = std::fs::read(&out).unwrap();
    let again = v.export_document(&out, &r, false).unwrap_err();
    assert_eq!(again.code, "destination_exists");
    assert_eq!(std::fs::read(&out).unwrap(), bytes);
    assert_eq!(v.export_document(&source, &r, false).unwrap_err().code, "export_over_source");
    // The CLI flags build the same recipe.
    let cli_out = s.path("cli.tif");
    let (exit, report) = cli_json(&["export", source.to_str().unwrap(), cli_out.to_str().unwrap(), "--tiff", "--depth", "16", "--compression", "zip", "--predictor", "--dpi", "300"]);
    assert_eq!(exit, 0, "{report}");
    assert_eq!(std::fs::read(&cli_out).unwrap(), bytes, "CLI and viewer write the same file");
}

/// Steps 105-107: crop, then a print size (3 x 4 cm at 100 ppi through develop's sizing rule) with glossy output
/// sharpening; the output has exactly develop's fitted size and the document is unchanged.
#[test]
fn crop_print_size_and_sharpening_follow_develops_stage() {
    let s = Scratch::new("print");
    let (mut v, _) = opened(&s);
    let out = s.path("hero.png");
    let r = recipe(json!({
        "format": "png", "depth": 16, "crop": {"x": 4, "y": 2, "width": 12, "height": 10},
        "stage": {"constrain_size": true, "resize_type": "WidthHeight", "max_width": 3, "max_height": 4, "size_unit": "cm",
                  "resolution": 100, "resolution_unit": "ppinch", "output_sharpening_enabled": true, "output_sharpening_media": "Glossy", "output_sharpening_level": 2},
    }));
    let stage = r.stage.clone().unwrap();
    let fit = hsk_studio_develop::fit_output_size(&stage.sizing, 12, 10).unwrap();
    let before = revision(&v);
    let report = v.export_document(&out, &r, false).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(revision(&v), before);
    let img = raster::decode(&std::fs::read(&out).unwrap(), &raster::Limits::default(), &CancellationToken::default()).unwrap();
    assert_eq!((img.width, img.height), (fit.width, fit.height), "{report}");
    assert!(img.width > 12, "enlarged to the print size");
    assert_eq!(img.icc.as_deref(), Some(SRGB_V4_ICC));
    assert!(matches!(img.samples, raster::Samples::U16(_)));
    assert_eq!(report["stage"]["sharpening"]["media"], "Glossy", "{report}");
    assert_eq!(report["stage"]["output_size"], json!([fit.width, fit.height]));
    // A crop past the frame is refused before any work.
    let bad = recipe(json!({"format": "png", "crop": {"x": 20, "y": 0, "width": 10, "height": 4}}));
    assert_eq!(v.export_document(&s.path("bad.png"), &bad, false).unwrap_err().code, "export_crop_outside");
    assert!(!s.path("bad.png").exists());
}

/// Step 112: a web JPEG at a long edge with screen sharpening and a graphic watermark, tagged sRGB.
#[test]
fn web_jpeg_long_edge_sharpen_and_watermark() {
    let s = Scratch::new("web");
    let (mut v, _) = opened(&s);
    let mark = s.path("logo.png");
    std::fs::write(&mark, png(4, 4, raster::Colour::Rgba, [255u8, 255, 255, 200].repeat(16))).unwrap();
    let out = s.path("web.jpg");
    let r = recipe(json!({
        "format": "jpeg", "quality": 85, "watermark_graphic": mark.to_str().unwrap(),
        "stage": {"constrain_size": true, "resize_type": "LongEdge", "max_width": 48, "output_sharpening_enabled": true, "output_sharpening_media": "Screen",
                  "output_sharpening_level": 2, "use_watermark": true, "watermark_anchor": "bottom_right"},
    }));
    let report = v.export_document(&out, &r, false).unwrap_or_else(|e| panic!("{e}"));
    let img = raster::decode(&std::fs::read(&out).unwrap(), &raster::Limits::default(), &CancellationToken::default()).unwrap();
    assert_eq!((img.width, img.height), (48, 32), "{report}");
    assert!(img.icc.is_some(), "tagged JPEG");
    assert!(report["stage"]["watermark"].is_object() && report["stage"]["sharpening"]["media"] == "Screen", "{report}");
    assert_eq!(report["subsampling"], "4:2:0");
}

/// Step 110: CMYK TIFF in the printer's profile (Prism wide path), tagged with that profile, with an ink readout.
#[test]
fn cmyk_tiff_is_converted_and_tagged_with_the_printer_profile() {
    let s = Scratch::new("cmyk");
    let (mut v, _) = opened(&s);
    let profile_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../hsk-studio-prism/tests/oracle/profiles/hsk-synth-cmyk-v2.icc");
    let profile = std::fs::read(&profile_path).unwrap();
    let out = s.path("press.tif");
    let r = recipe(json!({"format": "tiff", "depth": 8, "colour": {"space": "cmyk", "profile": profile_path.to_str().unwrap(), "intent": "relative_colorimetric", "black_point_compensation": true}}));
    let report = v.export_document(&out, &r, false).unwrap_or_else(|e| panic!("{e}"));
    let page = tiff_page(&out);
    assert_eq!(page.info.source_photometric, 5, "Separated (CMYK)");
    assert_eq!(page.info.samples_per_pixel, 4);
    assert_eq!(page.info.icc.as_deref(), Some(profile.as_slice()), "tagged with the printer profile");
    let ink = report["cmyk"]["total_ink_max_percent"].as_f64().unwrap();
    println!("DOC_EXPORT cmyk total ink max {ink} %");
    assert!(ink > 0.0 && ink <= 400.0, "{report}");
    let tiff::Samples::U8(got) = page.pixels() else { panic!("8-bit") };
    assert!(got.iter().any(|c| *c > 0), "inks written");
    // CMYK is TIFF only; PNG refuses before anything is written.
    let png_cmyk = recipe(json!({"format": "png", "colour": {"space": "cmyk", "profile": profile_path.to_str().unwrap()}}));
    assert_eq!(v.export_document(&s.path("x.png"), &png_cmyk, false).unwrap_err().code, "cmyk_format_unsupported");
}

/// Step 108: a layered PSD of the document (raster layers and a group with their names), that re-reads through
/// interop-psd with the layer pixels of the document and opens again in the viewer.
#[test]
fn layered_psd_keeps_layers_and_groups_and_reopens() {
    let s = Scratch::new("psd");
    let (mut v, _) = opened(&s);
    let base = v.layers()[0].layer_id.clone();
    let empty = v.new_raster_layer("retouch").unwrap();
    v.group_layers(&[base.clone(), empty], "hero group").unwrap();
    let out = s.path("master.psd");
    let report = v.export_document(&out, &DocExportRecipe::of_format(DocFormat::Psd { depth: 8 }), false).unwrap_or_else(|e| panic!("{e}"));
    let cancel = CancellationToken::default();
    let limits = psd::Limits::default();
    let doc = psd::read_psd(&std::fs::read(&out).unwrap(), &limits, &cancel).unwrap();
    assert_eq!((doc.header.width, doc.header.height, doc.header.depth), (W, H, 8));
    assert_eq!(doc.resources.icc_profile(), Some(SRGB_V4_ICC));
    let layers = &doc.layer_section.layer_info.as_ref().unwrap().layers;
    let names: Vec<&str> = layers.iter().map(|l| l.name.as_str()).collect();
    println!("DOC_EXPORT psd records {names:?} report {}", report["psd"]);
    assert_eq!(layers.len(), 4, "divider, two pixel layers, folder: {names:?}");
    assert!(names.contains(&"hero group") && names.contains(&"retouch"), "{names:?}");
    // The imported layer's red channel is the source's red samples.
    let pixel = layers.iter().find(|l| l.name == "Background").expect("the source layer");
    let (rw, rh) = (pixel.rect.width(), pixel.rect.height());
    println!("DOC_EXPORT psd source layer rect {:?}", pixel.rect);
    assert!(pixel.rect.left <= 0 && pixel.rect.top <= 0 && rw >= W && rh >= H, "{:?}", pixel.rect);
    let red = pixel.channels.iter().find(|c| c.id == 0).unwrap().decode(rw, rh, &doc.header, &limits, &cancel).unwrap();
    let at = |x: u32, y: u32| red[((y as i64 - i64::from(pixel.rect.top)) * i64::from(rw) + x as i64 - i64::from(pixel.rect.left)) as usize];
    let worst = source_rgb().chunks_exact(3).enumerate().map(|(i, p)| p[0].abs_diff(at(i as u32 % W, i as u32 / W))).max().unwrap();
    assert!(worst <= 1, "layer pixels differ by {worst}");
    // The file opens again as a layered document.
    let mut back = Viewer::new(Prefer::Cpu);
    back.open(&out).unwrap_or_else(|e| panic!("{e}"));
    let reopened: Vec<String> = back.layers().iter().map(|l| l.name.clone()).collect();
    assert!(reopened.iter().any(|n| n == "hero group") && reopened.iter().any(|n| n == "retouch"), "{reopened:?}");
    // A layered master takes no flat-output stage.
    let staged = recipe(json!({"format": "psd", "stage": {"constrain_size": true, "resize_type": "LongEdge", "max_width": 8}}));
    assert_eq!(v.export_document(&s.path("staged.psd"), &staged, false).unwrap_err().code, "psd_export_stage_unsupported");
}

/// Hostile recipes: closed keys, bad values and a stage output over develop's limit are typed refusals.
#[test]
fn hostile_recipes_are_typed_refusals() {
    let cancel = CancellationToken::default();
    let code = |v: Value| DocExportRecipe::from_json(&v, None, &cancel).unwrap_err().code;
    assert_eq!(code(json!({"format": "png", "sharpen": 1})), "recipe_key_unknown");
    assert_eq!(code(json!({"format": "bmp"})), "recipe_format_unknown");
    assert_eq!(code(json!({"format": "png", "depth": 12})), "recipe_depth_invalid");
    assert_eq!(code(json!({"format": "png", "quality": 50})), "recipe_key_not_for_format");
    assert_eq!(code(json!({"format": "tiff", "compression": "none", "predictor": true})), "recipe_predictor_needs_compression");
    assert_eq!(code(json!({"format": "png", "crop": {"x": 0, "y": 0, "width": 0, "height": 1}})), "recipe_crop_invalid");
    assert_eq!(code(json!({"format": "png", "stage": {"no_such_key": 1}})), "unknown_export_field");
    assert_eq!(code(json!({"format": "png", "stage": {"max_width": "wide"}})), "export_field_not_a_number");
    assert_eq!(code(json!({"format": "jpeg", "quality": 101})), "recipe_quality_invalid");
    let s = Scratch::new("hostile");
    let (mut v, _) = opened(&s);
    let huge = recipe(json!({"format": "png", "stage": {"constrain_size": true, "resize_type": "LongEdge", "max_width": 1.0e12}}));
    let e = v.export_document(&s.path("huge.png"), &huge, false).unwrap_err();
    assert_eq!(e.code, "export_output_too_large", "{e}");
    assert!(!s.path("huge.png").exists());
}

/// Step 116: the batch runner writes non-native targets through the recipe (a default TIFF target and a recipe
/// JPEG target), and a rerun with the skip policy skips what exists.
#[test]
fn batch_writes_tiff_and_recipe_targets_and_skips_on_rerun() {
    let s = Scratch::new("batch");
    // Batch inputs are native packages: the PNG is imported and saved first.
    let (mut v, _) = opened(&s);
    let input = s.path("doc.handshake");
    v.save(&input, false).unwrap_or_else(|e| panic!("{e}"));
    let recipe_file = s.path("web.json");
    std::fs::write(&recipe_file, serde_json::to_vec(&json!({"schema_id": "hsk.studio.doc_export_recipe@1", "recipe_id": "web-16", "format": "jpeg", "quality": 80,
        "stage": {"constrain_size": true, "resize_type": "LongEdge", "max_width": 16}})).unwrap()).unwrap();
    let out = s.path("out");
    let target = |format: &str, recipe: Option<&str>| OutputTarget {
        format_id: format.into(),
        export_recipe_id: recipe.map(str::to_string),
        destination: out.to_string_lossy().into_owned(),
        colour_profile_policy: None,
        resize_policy: None,
        overwrite_policy: OverwritePolicy::Skip,
    };
    let job = BatchJob {
        job_id: "doc-export".into(),
        source_set: SourceSet::ExplicitFileList(vec![input.to_string_lossy().into_owned()]),
        applied_automation: Automation::None,
        output_targets: vec![target("tiff", None), target("jpeg", Some("web-16")), target("jpeg", Some("no-such-recipe"))],
        filename_template: vec![TemplateToken::OriginalFilename, TemplateToken::Custom("-out".into())],
        error_policy: ErrorPolicy::SkipAndContinue,
        concurrency: 1,
        post_processing: vec![],
    };
    let job_file = s.path("job.json");
    std::fs::write(&job_file, job.export()).unwrap();
    let (_, report) = cli_json(&["batch", job_file.to_str().unwrap(), "--recipe", recipe_file.to_str().unwrap()]);
    let targets = &report["report"]["receipts"][0]["targets"];
    assert_eq!(targets[0]["outcome"], "written", "{report}");
    assert_eq!(targets[1]["outcome"], "written", "{report}");
    assert_eq!((targets[2]["outcome"].as_str(), targets[2]["code"].as_str()), (Some("refused"), Some("batch_export_recipe_unknown")), "{report}");
    let tif = PathBuf::from(targets[0]["path"].as_str().unwrap());
    assert!(tif.ends_with("doc-out.tif"));
    let page = tiff_page(&tif);
    assert_eq!((page.info.width, page.info.bits_per_sample, page.info.icc.as_deref()), (W, 16, Some(SRGB_V4_ICC)));
    let jpg = std::fs::read(targets[1]["path"].as_str().unwrap()).unwrap();
    let img = raster::decode(&jpg, &raster::Limits::default(), &CancellationToken::default()).unwrap();
    assert_eq!((img.width, img.height), (16, 11));
    // No temp file is left beside the outputs.
    let mut names: Vec<String> = std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, ["doc-out.jpg", "doc-out.tif"]);
    let (_, rerun) = cli_json(&["batch", job_file.to_str().unwrap(), "--recipe", recipe_file.to_str().unwrap()]);
    let again = &rerun["report"]["receipts"][0]["targets"];
    assert_eq!((again[0]["outcome"].as_str(), again[1]["outcome"].as_str()), (Some("skipped_existing"), Some("skipped_existing")), "{rerun}");
}
