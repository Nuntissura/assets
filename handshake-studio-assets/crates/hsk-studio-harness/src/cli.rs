//! Command-line surface as a pure function: arguments in, (exit code, stdout text) out. The
//! example binary only forwards process arguments and prints the result, so every command is
//! testable without spawning a process, opening a window or touching the environment.
//!
//! Two refusals keep a green exit honest (tests: `verify_checks_something_and_save_never_clobbers_the_input`
//! in `tests/harness_headless.rs`): `verify` with no expectation is a usage error (exit 2), and
//! `edit --save OUT` where OUT is the input PACKAGE is a usage error unless `--overwrite` is given.
#![deny(clippy::arithmetic_side_effects)]

use crate::harness::{Harness, HarnessError, References, Selection, default_actor};
use crate::manual::{manual_json, manual_text};
use hsk_studio_accord::CancellationToken;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub struct CliOutput {
    pub exit: i32,
    pub stdout: String,
}

enum EditOp {
    Rename(String, String),
    Visible(String, bool),
    Set(String, String, String),
}

#[derive(Default)]
struct Parsed {
    positional: Vec<String>,
    providers: Vec<String>,
    references: References,
    layer: Option<String>,
    expect_revision: Option<u64>,
    save: Option<PathBuf>,
    overwrite: bool,
    cancel: bool,
    json: bool,
    edits: Vec<EditOp>,
    expect_names: Vec<(String, String)>,
    expect_visible: Vec<(String, bool)>,
    png: bool,
    jpeg: bool,
    /// develop: `--tiff` and its options (lane SMB-DEVELOP-EXPORT-TIFF).
    tiff: bool,
    compression: Option<String>,
    predictor: bool,
    byte_order: Option<String>,
    bigtiff: bool,
    dpi: Option<u32>,
    colour_space: Option<String>,
    /// develop: `--dng` and its options (lane SMB-DEVELOP-DNG-WRITER).
    dng: bool,
    dng_compression: Option<String>,
    dng_preview: Option<String>,
    dng_xmp: Option<PathBuf>,
    dng_allow_dropped: bool,
    quality: Option<u8>,
    /// develop: `--adjust CONTROL VALUE` (value kept as text; parsed as a number by `develop`).
    adjust: Vec<(String, String)>,
    threads: Option<usize>,
    depth: Option<u8>,
}

fn boolean(s: &str) -> Result<bool, String> {
    match s {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!("expected true or false, got `{other}`")),
    }
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut p = Parsed::default();
    let mut i = 0;
    let take = |i: &mut usize, what: &str| -> Result<String, String> {
        let v = args.get(*i).ok_or_else(|| format!("missing {what}"))?;
        *i = i.saturating_add(1);
        Ok(v.clone())
    };
    while let Some(arg) = args.get(i).cloned() {
        i = i.saturating_add(1);
        match arg.as_str() {
            "--provider" => p.providers.push(take(&mut i, "provider id")?),
            "--references" => {
                p.references = match take(&mut i, "references mode")?.as_str() {
                    "unresolved" => References::Unresolved,
                    "trust_all" => References::TrustAll,
                    other => return Err(format!("unknown references mode `{other}`")),
                }
            }
            "--layer" => p.layer = Some(take(&mut i, "layer id")?),
            "--expect-revision" => {
                p.expect_revision =
                    Some(take(&mut i, "revision")?.parse::<u64>().map_err(|_| "revision must be an unsigned integer".to_string())?)
            }
            "--save" => p.save = Some(PathBuf::from(take(&mut i, "output path")?)),
            "--overwrite" => p.overwrite = true,
            "--cancel" => p.cancel = true,
            "--json" => p.json = true,
            "--png" => p.png = true,
            "--jpeg" => p.jpeg = true,
            "--tiff" => p.tiff = true,
            "--dng" => p.dng = true,
            "--dng-compression" => p.dng_compression = Some(take(&mut i, "dng compression (none|ljpeg)")?),
            "--dng-preview" => p.dng_preview = Some(take(&mut i, "dng preview (none|medium|full)")?),
            "--dng-xmp" => p.dng_xmp = Some(PathBuf::from(take(&mut i, "XMP packet path")?)),
            "--dng-allow-dropped" => p.dng_allow_dropped = true,
            "--compression" => p.compression = Some(take(&mut i, "compression (none|lzw|zip)")?),
            "--predictor" => p.predictor = true,
            "--byte-order" => p.byte_order = Some(take(&mut i, "byte order (le|be)")?),
            "--bigtiff" => p.bigtiff = true,
            "--dpi" => p.dpi = Some(take(&mut i, "dpi")?.parse::<u32>().map_err(|_| "dpi must be an unsigned integer".to_string())?),
            "--colour-space" => p.colour_space = Some(take(&mut i, "colour space (srgb|display-p3|adobe-rgb|prophoto)")?),
            "--quality" => {
                p.quality = Some(
                    take(&mut i, "quality")?
                        .parse::<u8>()
                        .ok()
                        .filter(|q| (1..=100).contains(q))
                        .ok_or_else(|| "quality must be an integer in 1..=100".to_string())?,
                )
            }
            "--adjust" => {
                let control = take(&mut i, "develop control id")?;
                p.adjust.push((control, take(&mut i, "value")?));
            }
            "--threads" => {
                p.threads = Some(take(&mut i, "thread count")?.parse::<usize>().map_err(|_| "threads must be an unsigned integer".to_string())?)
            }
            "--depth" => {
                p.depth = Some(match take(&mut i, "depth")?.as_str() {
                    "8" => 8,
                    "16" => 16,
                    other => return Err(format!("depth must be 8 or 16, got `{other}`")),
                })
            }
            "--rename" => {
                let layer = take(&mut i, "layer id")?;
                p.edits.push(EditOp::Rename(layer, take(&mut i, "name")?));
            }
            "--visible" => {
                let layer = take(&mut i, "layer id")?;
                p.edits.push(EditOp::Visible(layer, boolean(&take(&mut i, "true|false")?)?));
            }
            "--set" => {
                let layer = take(&mut i, "layer id")?;
                let control = take(&mut i, "control id")?;
                p.edits.push(EditOp::Set(layer, control, take(&mut i, "value")?));
            }
            "--expect-name" => {
                let layer = take(&mut i, "layer id")?;
                p.expect_names.push((layer, take(&mut i, "name")?));
            }
            "--expect-visible" => {
                let layer = take(&mut i, "layer id")?;
                p.expect_visible.push((layer, boolean(&take(&mut i, "true|false")?)?));
            }
            flag if flag.starts_with("--") => return Err(format!("unknown flag `{flag}`")),
            _ => p.positional.push(arg),
        }
    }
    Ok(p)
}

fn out(exit: i32, value: &Value) -> CliOutput {
    CliOutput { exit, stdout: format!("{}\n", serde_json::to_string_pretty(value).unwrap_or_default()) }
}

fn usage(message: &str) -> CliOutput {
    out(2, &json!({"schema":crate::SCHEMA,"ok":false,"error":"usage","message":message,"hint":"run `help`"}))
}

fn refused(command: &str, error: &HarnessError) -> CliOutput {
    out(1, &json!({"schema":crate::SCHEMA,"command":command,"ok":false,"verdict":"fail","error":error.code(),"detail":error.to_string()}))
}

/// Runs one command line (without the program name).
pub fn run(args: &[String]) -> CliOutput {
    let Some((command, rest)) = args.split_first() else {
        return help(&Parsed::default());
    };
    let command = command.as_str();
    if matches!(command, "--help" | "-h") {
        return help(&Parsed::default());
    }
    let parsed = match parse(rest) {
        Ok(p) => p,
        Err(m) => return usage(&m),
    };
    match command {
        "help" => help(&parsed),
        "descriptors" | "providers" => with_harness(command, &parsed, |h| {
            if command == "descriptors" {
                match h.collect_descriptors() {
                    Ok(d) => out(0, &json!({"schema":crate::SCHEMA,"command":command,"ok":true,"descriptors":d})),
                    Err(e) => refused(command, &e),
                }
            } else {
                out(
                    0,
                    &json!({"schema":crate::SCHEMA,"command":command,"ok":true,"available":crate::AVAILABLE_PROVIDERS,"selected":h.selection().providers}),
                )
            }
        }),
        "open" | "panel" | "edit" | "verify" => {
            let Some(package) = parsed.positional.first().map(PathBuf::from) else {
                return usage("missing PACKAGE path");
            };
            if parsed.positional.len() > 1 {
                return usage("only one PACKAGE path is accepted");
            }
            with_harness(command, &parsed, |h| execute(command, h, &parsed, &package))
        }
        "import" => {
            let Some(input) = parsed.positional.first().map(PathBuf::from) else {
                return usage("missing SOURCE path");
            };
            if parsed.positional.len() > 1 {
                return usage("only one SOURCE path is accepted");
            }
            if let Some(save) = &parsed.save
                && !parsed.overwrite
                && same_file(save, &input)
            {
                return usage("--save names the SOURCE file; pass --overwrite to replace it, or save to another path");
            }
            with_harness(command, &parsed, |h| execute_import(h, &parsed, &input))
        }
        "export" => {
            let [source, output] = parsed.positional.as_slice() else {
                return usage("export needs SOURCE and OUT paths");
            };
            let format = match (parsed.png, parsed.jpeg, parsed.quality) {
                (true, false, None) => ExportChoice::Png,
                (true, false, Some(_)) => return usage("--quality applies to --jpeg only"),
                (false, true, q) => ExportChoice::Jpeg(q.unwrap_or(DEFAULT_JPEG_QUALITY)),
                _ => return usage("export needs exactly one of --png, --jpeg"),
            };
            let (source, output) = (PathBuf::from(source), PathBuf::from(output));
            if !parsed.overwrite && same_file(&output, &source) {
                return usage("OUT names the SOURCE file; pass --overwrite to replace it, or export to another path");
            }
            execute_export(&source, &output, format, parsed.overwrite)
        }
        "develop" => {
            let [source, output] = parsed.positional.as_slice() else {
                return usage("develop needs SOURCE and OUT paths");
            };
            let tiff_options = parsed.compression.is_some()
                || parsed.predictor
                || parsed.byte_order.is_some()
                || parsed.bigtiff
                || parsed.dpi.is_some()
                || parsed.colour_space.is_some();
            let dng_options = parsed.dng_compression.is_some() || parsed.dng_preview.is_some() || parsed.dng_xmp.is_some() || parsed.dng_allow_dropped;
            let format = match (parsed.png, parsed.jpeg, parsed.quality, parsed.depth) {
                (false, false, None, None) if parsed.dng && !parsed.tiff && !tiff_options => match dng_choice(&parsed) {
                    Ok(choice) => choice,
                    Err(message) => return usage(&message),
                },
                _ if parsed.dng => return usage("--dng is exclusive with --png, --jpeg, --tiff, --quality, --depth and the TIFF options"),
                _ if dng_options => return usage("--dng-compression, --dng-preview, --dng-xmp and --dng-allow-dropped apply to --dng only"),
                (false, false, q, depth) if parsed.tiff => {
                    if q.is_some() {
                        return usage("--quality applies to --jpeg only");
                    }
                    match tiff_choice(&parsed, depth) {
                        Ok(choice) => choice,
                        Err(message) => return usage(&message),
                    }
                }
                _ if parsed.tiff => return usage("develop needs exactly one of --png, --jpeg, --tiff, --dng"),
                _ if tiff_options => return usage("--compression, --predictor, --byte-order, --bigtiff, --dpi and --colour-space apply to --tiff only"),
                (true, false, None, depth) => DevelopChoice::Png(depth.unwrap_or(16)),
                (true, false, Some(_), _) => return usage("--quality applies to --jpeg only"),
                (false, true, _, Some(_)) => return usage("--depth applies to --png only (JPEG is 8-bit)"),
                (false, true, q, None) => DevelopChoice::Jpeg(q.unwrap_or(DEFAULT_JPEG_QUALITY)),
                _ => return usage("develop needs exactly one of --png, --jpeg, --tiff, --dng"),
            };
            let mut adjust = Vec::with_capacity(parsed.adjust.len());
            for (control, text) in &parsed.adjust {
                // A number that is out of range (or NaN/inf) is develop's typed refusal, not a usage error.
                match text.parse::<f64>() {
                    Ok(v) => adjust.push((control.clone(), v)),
                    Err(_) => return usage(&format!("--adjust {control}: `{text}` is not a number")),
                }
            }
            let (source, output) = (PathBuf::from(source), PathBuf::from(output));
            if !parsed.overwrite && same_file(&output, &source) {
                return usage("OUT names the SOURCE file; pass --overwrite to replace it, or develop to another path");
            }
            execute_develop(&source, &output, format, &adjust, parsed.threads, parsed.overwrite)
        }
        other => usage(&format!("unknown command `{other}`")),
    }
}

#[cfg_attr(not(feature = "viewer"), allow(dead_code))]
enum DevelopChoice {
    Png(u8),
    Jpeg(u8),
    /// depth, compression (0 none, 1 lzw, 2 zip), predictor, colour space (0 srgb, 1 display-p3, 2 adobe-rgb, 3 prophoto),
    /// big-endian, BigTIFF, dpi.
    Tiff { depth: u8, compression: u8, predictor: bool, colour_space: u8, big_endian: bool, big_tiff: bool, dpi: Option<u32> },
    /// compression (0 none, 1 lossless JPEG), preview (0 none, 1 medium, 2 full size), XMP packet path, allow dropped features.
    Dng { compression: u8, preview: u8, xmp: Option<PathBuf>, allow_dropped: bool },
}

/// `--dng [--dng-compression none|ljpeg] [--dng-preview none|medium|full] [--dng-xmp FILE] [--dng-allow-dropped]`;
/// defaults uncompressed, medium preview. The XMP packet (the develop recipe as `crs:` keys, written by the lrcat
/// XMP codec) is embedded verbatim; develop validates it and every other refusal.
fn dng_choice(parsed: &Parsed) -> Result<DevelopChoice, String> {
    let compression = match parsed.dng_compression.as_deref() {
        None | Some("none") => 0,
        Some("ljpeg") => 1,
        Some(other) => return Err(format!("--dng-compression must be none or ljpeg, got `{other}`")),
    };
    let preview = match parsed.dng_preview.as_deref() {
        None | Some("medium") => 1,
        Some("none") => 0,
        Some("full") => 2,
        Some(other) => return Err(format!("--dng-preview must be none, medium or full, got `{other}`")),
    };
    Ok(DevelopChoice::Dng { compression, preview, xmp: parsed.dng_xmp.clone(), allow_dropped: parsed.dng_allow_dropped })
}

/// `--tiff [--depth 8|16] [--compression none|lzw|zip] [--predictor] [--byte-order le|be] [--bigtiff] [--dpi N]
/// [--colour-space srgb|display-p3|adobe-rgb|prophoto]`; defaults 16-bit, uncompressed, little-endian, sRGB.
/// The predictor needs a compression; develop decides every other refusal (profile missing for Adobe RGB / ProPhoto).
fn tiff_choice(parsed: &Parsed, depth: Option<u8>) -> Result<DevelopChoice, String> {
    let compression = match parsed.compression.as_deref() {
        None | Some("none") => 0,
        Some("lzw") => 1,
        Some("zip") => 2,
        Some(other) => return Err(format!("--compression must be none, lzw or zip, got `{other}`")),
    };
    let colour_space = match parsed.colour_space.as_deref() {
        None | Some("srgb") => 0,
        Some("display-p3") => 1,
        Some("adobe-rgb") => 2,
        Some("prophoto") => 3,
        Some(other) => return Err(format!("--colour-space must be srgb, display-p3, adobe-rgb or prophoto, got `{other}`")),
    };
    let big_endian = match parsed.byte_order.as_deref() {
        None | Some("le") => false,
        Some("be") => true,
        Some(other) => return Err(format!("--byte-order must be le or be, got `{other}`")),
    };
    if parsed.predictor && compression == 0 {
        return Err("--predictor needs --compression lzw or zip".to_string());
    }
    Ok(DevelopChoice::Tiff { depth: depth.unwrap_or(16), compression, predictor: parsed.predictor, colour_space, big_endian, big_tiff: parsed.bigtiff, dpi: parsed.dpi })
}

/// `develop SOURCE OUT (--png [--depth 8|16] | --jpeg [--quality N]) [--adjust CONTROL VALUE]... [--threads N]`
/// (lane SMB-HARNESS-DEVELOP-EXPORT): open the camera raw / DNG like the viewer, apply every
/// `--adjust` as ONE develop step through the viewer (chronicle `tiles` commit, undoable), then
/// `develop_to_output` and encode with the sRGB-v4 profile embedded. Refusals carry their stable code.
#[cfg(feature = "viewer")]
fn execute_develop(source: &Path, output: &Path, format: DevelopChoice, adjust: &[(String, f64)], threads: Option<usize>, overwrite: bool) -> CliOutput {
    use crate::view::{Viewer, develop::DevelopExport, render::Prefer};
    use hsk_studio_develop::OutputDepth;
    let fail = |error: &str, detail: Value| out(1, &json!({"schema":crate::SCHEMA,"command":"develop","ok":false,"verdict":"fail","error":error,"detail":detail}));
    let mut viewer = Viewer::new(Prefer::Cpu);
    if let Err(detail) = viewer.open(source) {
        let code = viewer.run().ops().last().and_then(|op| op["error"].as_str().map(str::to_string)).unwrap_or_else(|| "open_failed".into());
        return fail(&code, json!({"stage":"open","message":detail}));
    }
    if !viewer.develop_available() {
        return fail("not_a_raw_document", json!({"stage":"open","source_format":viewer.receipt().map(|r| r["source_format"].clone())}));
    }
    if let Some(n) = threads
        && let Err(r) = viewer.set_develop_threads(n)
    {
        return fail(r.code, json!({"stage":"threads","message":r.detail}));
    }
    let mut step = Value::Null;
    if !adjust.is_empty() {
        let changes: Vec<(&str, f64)> = adjust.iter().map(|(c, v)| (c.as_str(), *v)).collect();
        match viewer.set_develop_many(&changes) {
            Ok(revision) => step = json!({"revision": revision, "history": viewer.run().history()}),
            Err(r) => return fail(r.code, json!({"stage":"adjust","message":r.detail})),
        }
    }
    if let DevelopChoice::Dng { xmp: Some(path), .. } = &format {
        // Read within the DNG XMP budget (checked before reading); develop validates the packet itself.
        let packet = match std::fs::metadata(path) {
            Ok(m) if m.len() <= hsk_studio_develop::dng_write::MAX_XMP_BYTES as u64 => std::fs::read(path).ok(),
            Ok(_) => return fail("limit_xmp_bytes", json!({"stage":"xmp"})),
            Err(_) => None,
        };
        let Some(packet) = packet else {
            return fail("xmp_unreadable", json!({"stage":"xmp"}));
        };
        if let Err(r) = viewer.set_develop_dng_xmp(Some(packet)) {
            return fail(r.code, json!({"stage":"xmp","message":r.detail}));
        }
    }
    let format = match format {
        DevelopChoice::Dng { compression, preview, allow_dropped, .. } => {
            use hsk_studio_develop::{DngCompression, DngPreview};
            DevelopExport::Dng {
                compression: if compression == 1 { DngCompression::LosslessJpeg } else { DngCompression::None },
                preview: match preview {
                    0 => DngPreview::None,
                    2 => DngPreview::FullSize,
                    _ => DngPreview::Medium,
                },
                allow_dropped,
            }
        }
        DevelopChoice::Png(16) => DevelopExport::Png { depth: OutputDepth::Sixteen },
        DevelopChoice::Png(_) => DevelopExport::Png { depth: OutputDepth::Eight },
        DevelopChoice::Jpeg(quality) => DevelopExport::Jpeg { quality },
        DevelopChoice::Tiff { depth, compression, predictor, colour_space, big_endian, big_tiff, dpi } => {
            use hsk_studio_develop::{ExportColourSpace, TiffCompression};
            DevelopExport::Tiff {
                depth: if depth == 8 { OutputDepth::Eight } else { OutputDepth::Sixteen },
                compression: match compression {
                    1 => TiffCompression::Lzw,
                    2 => TiffCompression::Deflate,
                    _ => TiffCompression::None,
                },
                predictor,
                colour_space: match colour_space {
                    1 => ExportColourSpace::DisplayP3,
                    2 => ExportColourSpace::AdobeRgb,
                    3 => ExportColourSpace::ProPhoto,
                    _ => ExportColourSpace::Srgb,
                },
                big_endian,
                big_tiff,
                dpi,
            }
        }
    };
    match viewer.export_develop(output, format, overwrite) {
        Ok(report) => out(
            0,
            &json!({"schema":crate::SCHEMA,"command":"develop","ok":true,"verdict":"pass","source":source.to_string_lossy(),"step":step,"develop":viewer.develop_json(),"export":report}),
        ),
        Err(r) => fail(r.code, json!({"stage":"export","message":r.detail})),
    }
}

#[cfg(not(feature = "viewer"))]
fn execute_develop(_: &Path, _: &Path, _: DevelopChoice, _: &[(String, f64)], _: Option<usize>, _: bool) -> CliOutput {
    out(1, &json!({"schema":crate::SCHEMA,"command":"develop","ok":false,"verdict":"fail","error":"feature_unavailable","detail":{"feature":"viewer"}}))
}

/// Reads a source file within [`crate::import::MAX_SOURCE_BYTES`] (checked before reading).
fn read_source(command: &str, input: &Path) -> Result<Vec<u8>, CliOutput> {
    let fail = |code: &str| out(1, &json!({"schema":crate::SCHEMA,"command":command,"ok":false,"verdict":"fail","error":code}));
    let len = std::fs::metadata(input).map_err(|_| fail("source_unreadable"))?.len();
    if len > crate::import::MAX_SOURCE_BYTES {
        return Err(fail("source_too_large"));
    }
    std::fs::read(input).map_err(|_| fail("source_unreadable"))
}

/// `import SOURCE [--save OUT]`: open directly, then save (or checkpoint in memory), reopen in a second
/// session and verify, exactly like `edit`.
#[cfg(feature = "import")]
fn execute_import(harness: &Harness, parsed: &Parsed, input: &Path) -> CliOutput {
    let bytes = match read_source("import", input) {
        Ok(b) => b,
        Err(o) => return o,
    };
    let cancel = CancellationToken::default();
    if parsed.cancel {
        cancel.cancel();
    }
    let mut run = harness.run(default_actor(), &cancel);
    if run.open_import(&bytes) {
        run.summary();
        let reopened = match &parsed.save {
            Some(path) => run.save_to(path) && run.reopen_path(path),
            None => match run.checkpoint() {
                Some(b) => run.reopen_bytes(&b),
                None => false,
            },
        };
        if reopened {
            run.verify_reopened();
        }
    }
    let report = run.finish("import");
    out(i32::from(report["ok"] != true), &report)
}

/// Without an importer feature the command still recognises the source and says what is missing.
#[cfg(not(feature = "import"))]
fn execute_import(_: &Harness, _: &Parsed, input: &Path) -> CliOutput {
    let bytes = match read_source("import", input) {
        Ok(b) => b,
        Err(o) => return o,
    };
    let (error, detail) = match crate::import::sniff(&bytes) {
        None => ("format_unrecognised", json!({})),
        // An Affinity file is refused the same way with or without importers (names the export route).
        Some(crate::import::SourceFormat::Affinity) => {
            let refusal = crate::import::affinity_refusal(&bytes);
            (refusal.code(), refusal.detail())
        }
        Some(f) => ("format_unavailable", json!({"format":f.name(),"feature":f.feature(),"reason":"feature_not_enabled"})),
    };
    out(1, &json!({"schema":crate::SCHEMA,"command":"import","ok":false,"verdict":"fail","error":error,"detail":{"import":detail}}))
}

/// Default JPEG quality of `export --jpeg` (the viewer button uses the same value).
const DEFAULT_JPEG_QUALITY: u8 = 90;

#[cfg_attr(not(feature = "viewer"), allow(dead_code))]
enum ExportChoice {
    Png,
    Jpeg(u8),
}

/// `export SOURCE --png|--jpeg [--quality N] OUT`: open like the viewer (import or native package), render
/// with render-cpu, encode through interop-raster, decode back, write atomically to exactly OUT.
#[cfg(feature = "viewer")]
fn execute_export(source: &Path, output: &Path, format: ExportChoice, overwrite: bool) -> CliOutput {
    use crate::view::{Viewer, export::ExportFormat, render::Prefer};
    let format = match format {
        ExportChoice::Png => ExportFormat::Png,
        ExportChoice::Jpeg(quality) => ExportFormat::Jpeg { quality },
    };
    let fail = |error: &str, detail: String| {
        out(1, &json!({"schema":crate::SCHEMA,"command":"export","ok":false,"verdict":"fail","error":error,"detail":detail}))
    };
    let mut viewer = Viewer::new(Prefer::Cpu);
    if let Err(detail) = viewer.open(source) {
        return fail("open_failed", detail);
    }
    match viewer.export(output, format, overwrite) {
        Ok(report) => out(
            0,
            &json!({"schema":crate::SCHEMA,"command":"export","ok":true,"verdict":"pass","source":source.to_string_lossy(),"export":report,"receipt_source_format":viewer.receipt().map(|r| r["source_format"].clone())}),
        ),
        Err(detail) => fail("export_failed", detail),
    }
}

#[cfg(not(feature = "viewer"))]
fn execute_export(_: &Path, _: &Path, _: ExportChoice, _: bool) -> CliOutput {
    out(1, &json!({"schema":crate::SCHEMA,"command":"export","ok":false,"verdict":"fail","error":"feature_unavailable","detail":{"feature":"viewer"}}))
}

fn with_harness(command: &str, parsed: &Parsed, f: impl FnOnce(&Harness) -> CliOutput) -> CliOutput {
    match Harness::new(Selection { providers: parsed.providers.clone(), references: parsed.references }) {
        Ok(h) => f(&h),
        Err(e) => refused(command, &e),
    }
}

fn help(parsed: &Parsed) -> CliOutput {
    with_harness("help", parsed, |h| {
        if parsed.json {
            out(0, &manual_json(h))
        } else {
            CliOutput { exit: 0, stdout: manual_text(h) }
        }
    })
}

/// Whether two paths name the same file (resolved when both exist, literal comparison otherwise).
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn execute(command: &str, harness: &Harness, parsed: &Parsed, package: &Path) -> CliOutput {
    // A verify that checks nothing would report a pass for nothing: refuse it before opening.
    if command == "verify" && parsed.expect_revision.is_none() && parsed.expect_names.is_empty() && parsed.expect_visible.is_empty() {
        return usage("verify needs at least one of --expect-revision, --expect-name, --expect-visible (a verify that checks nothing is refused)");
    }
    // The input package is replaced in place only on explicit request.
    if let Some(save) = &parsed.save
        && command == "edit"
        && !parsed.overwrite
        && same_file(save, package)
    {
        return usage("--save names the input PACKAGE; pass --overwrite to replace it in place, or save to another path");
    }
    let cancel = CancellationToken::default();
    if parsed.cancel {
        cancel.cancel();
    }
    let mut run = harness.run(default_actor(), &cancel);
    let opened = run.open_path(package);
    match command {
        "open" => {
            if opened {
                run.summary();
            }
        }
        "panel" => match &parsed.layer {
            None => return usage("panel needs --layer ID"),
            Some(layer) => {
                if opened {
                    run.panel(layer);
                }
            }
        },
        "verify" => {
            if opened {
                let mut expectations: Vec<(String, &'static str, Value)> = Vec::new();
                expectations.extend(parsed.expect_names.iter().map(|(l, n)| (l.clone(), "name", json!(n))));
                expectations.extend(parsed.expect_visible.iter().map(|(l, v)| (l.clone(), "visible", json!(v))));
                run.verify_live(parsed.expect_revision, expectations);
            }
        }
        _ => {
            if parsed.edits.is_empty() {
                return usage("edit needs at least one of --rename, --visible, --set");
            }
            if opened {
                run.summary();
                edit_flow(&mut run, parsed);
            }
        }
    }
    let report = run.finish(command);
    out(i32::from(report["ok"] != true), &report)
}

fn edit_flow(run: &mut crate::Run<'_>, parsed: &Parsed) {
    let expect = parsed.expect_revision;
    for edit in &parsed.edits {
        let ok = match edit {
            EditOp::Rename(layer, name) => run.rename(layer, name, expect),
            EditOp::Visible(layer, visible) => run.set_visible(layer, *visible, expect),
            EditOp::Set(layer, control, value) => run.panel_set(layer, control, value, expect),
        };
        if !ok {
            return;
        }
    }
    let reopened = match &parsed.save {
        Some(path) => run.save_to(path) && run.reopen_path(path),
        None => match run.checkpoint() {
            Some(bytes) => run.reopen_bytes(&bytes),
            None => false,
        },
    };
    if reopened {
        run.verify_reopened();
    }
}
