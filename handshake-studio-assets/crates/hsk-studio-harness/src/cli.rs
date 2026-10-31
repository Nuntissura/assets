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
    /// develop: export stage flags in command-line order (lane SMB-LR-EXPORT-GUI-1).
    stage: Vec<StageArg>,
    threads: Option<usize>,
    depth: Option<u8>,
    /// export-sequence (lane SMB-PR-SEQ-EXPORT-UI-1): `--sequence`, `--preset`, `--in` / `--out` (ticks, kept as
    /// text and checked by the command), `--output`, `--media FILE`..., `--audio FILE`... (R44 F-R44-2), `--width`,
    /// `--height`.
    sequence: Option<String>,
    preset: Option<String>,
    in_tick: Option<String>,
    out_tick: Option<String>,
    output: Option<PathBuf>,
    media: Vec<PathBuf>,
    audio: Vec<PathBuf>,
    width: Option<u32>,
    height: Option<u32>,
}

/// One develop export stage flag (lane SMB-LR-EXPORT-GUI-1), mapped one to one onto the export
/// dialog's fields (`Viewer::set_develop_export_field`, ids `develop.export.<key>`), so a command line
/// and the window build the same `ExportStage`:
/// `--export-set KEY VALUE` writes field KEY (the stage starts with every group off, as in the dialog);
/// `--long-edge N` = stage on, constrain_size, resize_type LongEdge, max_width N; `--sharpen MEDIA:AMOUNT` (screen|glossy|matte : low|standard|high) = stage on,
/// output sharpening on with that media and level 1|2|3; `--watermark FILE` = stage on, use_watermark,
/// the PNG / JPEG graphic loaded like the dialog's file dialog does.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "viewer"), allow(dead_code))]
enum StageArg {
    Set(String, String),
    Watermark(PathBuf),
}

fn stage_on() -> StageArg {
    StageArg::Set("stage".into(), "true".into())
}

fn sharpen_args(value: &str) -> Result<Vec<StageArg>, String> {
    let bad = || format!("--sharpen must be screen|glossy|matte:low|standard|high, got `{value}`");
    let (media, amount) = value.split_once(':').ok_or_else(bad)?;
    let media = match media {
        "screen" => "Screen",
        "glossy" => "Glossy",
        "matte" => "Matte",
        _ => return Err(bad()),
    };
    let level = match amount {
        "low" => "1",
        "standard" => "2",
        "high" => "3",
        _ => return Err(bad()),
    };
    let set = |k: &str, v: &str| StageArg::Set(k.into(), v.into());
    Ok(vec![stage_on(), set("output_sharpening_enabled", "true"), set("output_sharpening_media", media), set("output_sharpening_level", level)])
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
            "--export-set" => {
                let key = take(&mut i, "export setting key")?;
                p.stage.push(StageArg::Set(key, take(&mut i, "value")?));
            }
            "--long-edge" => {
                let edge = take(&mut i, "long edge")?;
                p.stage.extend([
                    stage_on(),
                    StageArg::Set("constrain_size".into(), "true".into()),
                    StageArg::Set("resize_type".into(), "LongEdge".into()),
                    StageArg::Set("max_width".into(), edge),
                ]);
            }
            "--sharpen" => p.stage.extend(sharpen_args(&take(&mut i, "sharpening (media:amount)")?)?),
            "--watermark" => {
                let file = PathBuf::from(take(&mut i, "watermark graphic path")?);
                p.stage.extend([stage_on(), StageArg::Set("use_watermark".into(), "true".into()), StageArg::Watermark(file)]);
            }
            "--sequence" => p.sequence = Some(take(&mut i, "sequence id")?),
            "--preset" => p.preset = Some(take(&mut i, "export preset id")?),
            "--in" => p.in_tick = Some(take(&mut i, "in tick")?),
            "--out" => p.out_tick = Some(take(&mut i, "out tick")?),
            "--output" => p.output = Some(PathBuf::from(take(&mut i, "output path")?)),
            "--media" => p.media.push(PathBuf::from(take(&mut i, "media file path")?)),
            "--audio" => p.audio.push(PathBuf::from(take(&mut i, "audio file path")?)),
            "--width" => p.width = Some(take(&mut i, "width")?.parse::<u32>().ok().filter(|w| *w > 0).ok_or_else(|| "width must be a positive integer".to_string())?),
            "--height" => p.height = Some(take(&mut i, "height")?.parse::<u32>().ok().filter(|h| *h > 0).ok_or_else(|| "height must be a positive integer".to_string())?),
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
            execute_develop(&source, &output, format, &adjust, &parsed.stage, parsed.threads, parsed.overwrite)
        }
        "export-sequence" => {
            let [package] = parsed.positional.as_slice() else {
                return usage("export-sequence needs exactly one PACKAGE path");
            };
            let Some(sequence) = parsed.sequence.clone() else {
                return usage("export-sequence needs --sequence ID");
            };
            let Some(preset) = parsed.preset.clone() else {
                return usage("export-sequence needs --preset ID");
            };
            let Some(output) = parsed.output.clone() else {
                return usage("export-sequence needs --output PATH");
            };
            let tick = |text: Option<&str>, flag: &str| -> Result<Option<u64>, CliOutput> {
                text.map(|t| t.parse::<u64>().map_err(|_| usage(&format!("{flag} must be a tick count (unsigned integer, 254016000000 per second), got `{t}`")))).transpose()
            };
            let (in_tick, out_tick) = match (tick(parsed.in_tick.as_deref(), "--in"), tick(parsed.out_tick.as_deref(), "--out")) {
                (Ok(a), Ok(b)) => (a, b),
                (Err(o), _) | (_, Err(o)) => return o,
            };
            if in_tick.is_some() != out_tick.is_some() {
                return usage("--in and --out go together");
            }
            let args = SequenceExport {
                sequence,
                preset,
                in_tick,
                out_tick,
                output,
                overwrite: parsed.overwrite,
                media: parsed.media.clone(),
                audio: parsed.audio.clone(),
                width: parsed.width.unwrap_or(1920),
                height: parsed.height.unwrap_or(1080),
            };
            execute_export_sequence(Path::new(package), &args)
        }
        "batch" => {
            let [job] = parsed.positional.as_slice() else {
                return usage("batch needs exactly one JOB.json path");
            };
            execute_batch(&parsed, Path::new(job))
        }
        "action" => match parsed.positional.as_slice() {
            [sub, set] if sub == "check" => execute_action_check(Path::new(set)),
            _ => usage("action needs `check SET.json`"),
        },
        other => usage(&format!("unknown command `{other}`")),
    }
}

/// A typed failure of `batch` / `action` before any operation ran (exit 1).
fn failed(command: &str, error: &str, detail: Value) -> CliOutput {
    out(1, &json!({"schema":crate::SCHEMA,"command":command,"ok":false,"verdict":"fail","error":error,"detail":detail}))
}

/// `batch JOB.json` (lane SMB-PS-ACTIONS-1): decode the portable job strictly, run it headless
/// through `hsk_studio_harness_core::batch::run_batch` and print the receipt set.
fn execute_batch(parsed: &Parsed, job_path: &Path) -> CliOutput {
    use hsk_studio_harness_core::batch::{BatchJob, BatchPorts, run_batch};
    let bytes = match std::fs::read(job_path) {
        Ok(b) => b,
        Err(_) => return failed("batch", "job_unreadable", Value::Null),
    };
    let job = match BatchJob::decode(&bytes) {
        Ok(j) => j,
        Err(e) => return failed("batch", e.code, json!({"path": e.path})),
    };
    with_harness("batch", parsed, |h| {
        let cancel = CancellationToken::default();
        if parsed.cancel {
            cancel.cancel();
        }
        match run_batch(h, &job, BatchPorts::default(), &cancel) {
            Err(refusal) => failed("batch", refusal.code, refusal.detail),
            Ok(report) => {
                let ran = !report.receipts.is_empty();
                let ok = ran && report.stopped.is_none() && report.receipts.iter().all(|r| r.outcome == "succeeded");
                let verdict = if !ran {
                    "nothing_checked"
                } else if ok {
                    "pass"
                } else {
                    "fail"
                };
                out(i32::from(!ok), &json!({"schema":crate::SCHEMA,"command":"batch","ok":ok,"verdict":verdict,"report":report.to_json()}))
            }
        }
    })
}

/// `action check SET.json` (lane SMB-PS-ACTIONS-1): strict decode, then the automation validators.
fn execute_action_check(set_path: &Path) -> CliOutput {
    use hsk_studio_harness_core::action::{ActionSet, Severity, validate_set};
    let bytes = match std::fs::read(set_path) {
        Ok(b) => b,
        Err(_) => return failed("action check", "action_set_unreadable", Value::Null),
    };
    let set = match ActionSet::decode(&bytes) {
        Ok(s) => s,
        Err(e) => return failed("action check", e.code, json!({"path": e.path})),
    };
    let diagnostics = validate_set(&set);
    let ok = diagnostics.iter().all(|d| d.severity != Severity::Error);
    let unresolvable: Vec<Value> = set
        .actions
        .iter()
        .flat_map(|a| a.steps.iter().filter(|s| s.command().is_none()).map(|s| json!({"action_id": a.action_id, "step_index": s.index, "command_id": s.command_id})))
        .collect();
    out(
        i32::from(!ok),
        &json!({
            "schema":crate::SCHEMA,"command":"action check","ok":ok,"verdict":if ok { "pass" } else { "fail" },
            "action_set_id": set.action_set_id, "actions": set.actions.len(),
            "steps": set.actions.iter().map(|a| a.steps.len()).sum::<usize>(),
            "diagnostics": diagnostics.iter().map(|d| d.to_json()).collect::<Vec<_>>(),
            "unresolvable": unresolvable,
        }),
    )
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
fn execute_develop(source: &Path, output: &Path, format: DevelopChoice, adjust: &[(String, f64)], stage: &[StageArg], threads: Option<usize>, overwrite: bool) -> CliOutput {
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
    // Export stage flags, in order, through the dialog's own field path (lane SMB-LR-EXPORT-GUI-1).
    for arg in stage {
        let (result, what) = match arg {
            StageArg::Set(key, value) => (viewer.set_develop_export_field(&format!("develop.export.{key}"), value), key.clone()),
            StageArg::Watermark(path) => (viewer.load_develop_watermark(path), "watermark".to_owned()),
        };
        if let Err(r) = result {
            return fail(r.code, json!({"stage":"export_stage","setting":what,"message":r.detail}));
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
fn execute_develop(_: &Path, _: &Path, _: DevelopChoice, _: &[(String, f64)], _: &[StageArg], _: Option<usize>, _: bool) -> CliOutput {
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

/// `export-sequence PACKAGE --sequence ID --preset ID [--in T --out T] --output OUT [--overwrite] [--media FILE]...
/// [--audio FILE]... [--width W --height H] [--json]` (lane SMB-PR-SEQ-EXPORT-UI-1; `--audio`: R44 F-R44-2).
#[cfg_attr(not(feature = "media"), allow(dead_code))]
struct SequenceExport {
    sequence: String,
    preset: String,
    in_tick: Option<u64>,
    out_tick: Option<u64>,
    output: PathBuf,
    overwrite: bool,
    media: Vec<PathBuf>,
    audio: Vec<PathBuf>,
    width: u32,
    height: u32,
}

/// Total bytes of all `--media` and `--audio` files one `export-sequence` reads into memory, checked from the file
/// sizes before any file is read (R44 F-R44-15; Studio reading: the in-memory limit of ONE media item,
/// `view::media::MAX_MEDIA_BYTES`, 2 GiB, also bounds the sum, so N files never hold N x 2 GiB). Over it:
/// `media_too_large` (stage `media` or `audio`, with `total_limit`).
pub const MAX_EXPORT_INPUT_BYTES: u64 = 2_147_483_648;

/// The `--audio` files of an `export-sequence` as the export's sound port (R44 F-R44-2): each file decoded by
/// reel's audio decoder (`hsk_studio_reel::decode_audio_file`, as the viewer's audio import) and keyed as the clip
/// source `sha256:<hex>` of its bytes, exactly as `Viewer::bind_program_media` keys video. Port adapter only: no
/// codec here.
#[cfg(feature = "media")]
struct CliAudio(Vec<(hsk_studio_folio::ClipSource, hsk_studio_accord::Ticks, Vec<Vec<f32>>)>);
#[cfg(feature = "media")]
impl hsk_studio_pulse::AudioSources for CliAudio {
    fn audio(&self, source: &hsk_studio_folio::ClipSource) -> Option<&[Vec<f32>]> {
        self.0.iter().find(|(s, _, _)| s == source).map(|(_, _, p)| p.as_slice())
    }
}
#[cfg(feature = "media")]
impl hsk_studio_pulse::SourceExtents for CliAudio {
    fn extent(&self, source: &hsk_studio_folio::ClipSource) -> Option<hsk_studio_accord::Ticks> {
        self.0.iter().find(|(s, _, _)| s == source).map(|(_, t, _)| *t)
    }
}

/// A typed refusal of [`cli_audio`]: error code and detail JSON.
#[cfg(feature = "media")]
type AudioRefusal = (String, Value);

/// Decodes the `--audio` files (bytes already read) into one port at their shared rate; `None` without files.
/// Refusals (stage `audio`): `audio_decode` (reel's message), `audio_empty`, `audio_rate_mismatch` (files at
/// different rates: this path has no resampler).
#[cfg(feature = "media")]
fn cli_audio(files: Vec<(PathBuf, Vec<u8>)>, cancel: &CancellationToken) -> Result<Option<(CliAudio, u32)>, AudioRefusal> {
    let limits = crate::view::audio_import::ImportLimits::default();
    let decode_fail = |name: &str, message: String| ("audio_decode".to_string(), json!({"stage":"audio","path":name,"message":message}));
    let mut port = Vec::with_capacity(files.len());
    let mut rate: Option<u32> = None;
    for (path, bytes) in files {
        let name = path.to_string_lossy().into_owned();
        let digest = hsk_studio_reel::full_digest(&mut std::io::Cursor::new(&bytes[..]), 1 << 20, cancel).map_err(|e| decode_fail(&name, e.to_string()))?;
        let decoded = hsk_studio_reel::decode_audio_file(&mut std::io::Cursor::new(&bytes[..]), &hsk_studio_reel::Limits::default(), limits.max_frames, cancel)
            .map_err(|e| decode_fail(&name, e.to_string()))?;
        let frames = u64::try_from(decoded.frames()).map_err(|_| decode_fail(&name, "frame count".into()))?;
        if frames == 0 || decoded.channels == 0 {
            return Err(("audio_empty".into(), json!({"stage":"audio","path":name})));
        }
        match rate {
            Some(r) if r != decoded.sample_rate => {
                return Err(("audio_rate_mismatch".into(), json!({"stage":"audio","path":name,"rate":decoded.sample_rate,"expected":r})));
            }
            _ => rate = Some(decoded.sample_rate),
        }
        let extent = hsk_studio_pulse::ticks_from_samples(frames, decoded.sample_rate).map_err(|e| decode_fail(&name, format!("{e:?}")))?;
        let hex = hsk_studio_reel::hex(&digest);
        let source = hsk_studio_folio::ClipSource::Media {
            artifact_manifest_id: format!("sha256:{hex}"),
            content_digest: hsk_studio_folio::ContentDigest { algorithm: "sha256".into(), digest: hex },
            asset_id: None,
        };
        port.push((source, extent, decoded.planar()));
    }
    Ok(rate.map(|r| (CliAudio(port), r)))
}

/// Opens the package headlessly like the viewer, binds every `--media` file to the program monitor through
/// reel's decoders (`Viewer::bind_program_media`: clip sources `sha256:<hex>`) and every `--audio` file as the
/// export's sound port (`Viewer::set_export_audio`, [`cli_audio`]), and runs `Viewer::export_sequence` (the window's
/// Export Media runs the same method): same refusals, same result JSON (`export.receipt` is reel's receipt). Exit 0
/// only when the receipt's outcome is `complete`. A relative OUT is relative to the current directory. Before any
/// file is read (R44 F-R44-15): an `--out` past the sequence end (its duration rounded up to a whole frame) is
/// `export_range` (stage `range`), the summed `--media` and `--audio` sizes are checked against
/// [`MAX_EXPORT_INPUT_BYTES`], and an OUT that cannot be made absolute is `output_unresolved` (stage `output`).
/// Tests: `tests/cli_export_sequence.rs`.
#[cfg(feature = "media")]
fn execute_export_sequence(package: &Path, a: &SequenceExport) -> CliOutput {
    use crate::view::{Viewer, render::Prefer, timeline::ExportSequenceRequest};
    let fail = |error: &str, detail: Value| out(1, &json!({"schema":crate::SCHEMA,"command":"export-sequence","ok":false,"verdict":"fail","error":error,"detail":detail}));
    let mut viewer = Viewer::new(Prefer::Cpu);
    if let Err(message) = viewer.open(package) {
        return fail("open_failed", json!({"stage":"open","message":message}));
    }
    // --out past the sequence end: refused before any media is read.
    if let Some(out_tick) = a.out_tick
        && let Some(sequence) = viewer.run().snapshot().and_then(|s| s.document().sequences.iter().find(|q| q.sequence_id == a.sequence))
    {
        let tpf = sequence.frame_rate.ticks_per_frame.max(1);
        let end = sequence.duration.value().div_ceil(tpf).checked_mul(tpf).unwrap_or(u64::MAX);
        if out_tick > end {
            return fail("export_range", json!({"stage":"range","out_tick":out_tick,"sequence_end":end}));
        }
    }
    let output = match std::path::absolute(&a.output) {
        Ok(p) => p,
        Err(e) => return fail("output_unresolved", json!({"stage":"output","path":a.output.to_string_lossy(),"message":e.to_string()})),
    };
    // Sizes first, every file, before reading any (the total bound and reel's per-item limit).
    let mut total = 0u64;
    for (path, stage) in a.media.iter().map(|p| (p, "media")).chain(a.audio.iter().map(|p| (p, "audio"))) {
        match std::fs::metadata(path) {
            Ok(m) if m.len() <= crate::view::media::MAX_MEDIA_BYTES as u64 => {
                total = total.saturating_add(m.len());
                if total > MAX_EXPORT_INPUT_BYTES {
                    return fail("media_too_large", json!({"stage":stage,"path":path.to_string_lossy(),"total_limit":MAX_EXPORT_INPUT_BYTES}));
                }
            }
            Ok(_) => return fail("media_too_large", json!({"stage":stage,"path":path.to_string_lossy()})),
            Err(_) => return fail("media_unreadable", json!({"stage":stage,"path":path.to_string_lossy()})),
        }
    }
    let read = |path: &PathBuf, stage: &str| std::fs::read(path).map_err(|_| fail("media_unreadable", json!({"stage":stage,"path":path.to_string_lossy()})));
    let mut files = Vec::with_capacity(a.media.len());
    for path in &a.media {
        match read(path, "media") {
            Ok(b) => files.push(b),
            Err(o) => return o,
        }
    }
    let mut sounds = Vec::with_capacity(a.audio.len());
    for path in &a.audio {
        match read(path, "audio") {
            Ok(b) => sounds.push((path.clone(), b)),
            Err(o) => return o,
        }
    }
    if !files.is_empty()
        && let Err(e) = viewer.bind_program_media(&a.sequence, files, a.width, a.height)
    {
        return fail(e.code, json!({"stage":"media","message":e.detail}));
    }
    match cli_audio(sounds, &CancellationToken::default()) {
        Ok(Some((port, rate))) => {
            let port = std::sync::Arc::new(port);
            let extents: std::sync::Arc<dyn hsk_studio_pulse::SourceExtents + Send + Sync> = port.clone();
            if let Err(message) = viewer.set_export_audio(SharedCliAudio(port), extents, rate) {
                return fail("audio_port", json!({"stage":"audio","message":message}));
            }
        }
        Ok(None) => {}
        Err((code, detail)) => return fail(&code, detail),
    }
    let mut request = ExportSequenceRequest::new(&a.preset, output);
    request.in_tick = a.in_tick;
    request.out_tick = a.out_tick;
    request.width = a.width;
    request.height = a.height;
    request.overwrite = a.overwrite;
    match viewer.export_sequence(&a.sequence, request) {
        Ok(export) => {
            let ok = export["receipt"]["outcome"] == "complete";
            out(i32::from(!ok), &json!({"schema":crate::SCHEMA,"command":"export-sequence","ok":ok,"verdict":if ok { "pass" } else { "fail" },"export":export}))
        }
        Err(e) if e.code == "audio_sources_missing" => fail(e.code, json!({"stage":"export","message":e.detail,"hint":"pass the clip's sound file with --audio FILE"})),
        Err(e) => fail(e.code, json!({"stage":"export","message":e.detail})),
    }
}

/// [`CliAudio`] shared between the sound port and its extents.
#[cfg(feature = "media")]
struct SharedCliAudio(std::sync::Arc<CliAudio>);
#[cfg(feature = "media")]
impl hsk_studio_pulse::AudioSources for SharedCliAudio {
    fn audio(&self, source: &hsk_studio_folio::ClipSource) -> Option<&[Vec<f32>]> {
        self.0.audio(source)
    }
}

#[cfg(not(feature = "media"))]
fn execute_export_sequence(_: &Path, _: &SequenceExport) -> CliOutput {
    out(1, &json!({"schema":crate::SCHEMA,"command":"export-sequence","ok":false,"verdict":"fail","error":"feature_unavailable","detail":{"feature":"media"}}))
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
