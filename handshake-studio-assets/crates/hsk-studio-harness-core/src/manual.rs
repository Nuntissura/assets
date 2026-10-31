//! The built-in manual (UserManual pillar): generated from the same descriptors the commands run
//! on (the command table below drives both parsing-help and this text; the panel topics come from
//! the controls crate's descriptors; providers and descriptors come from the live registries).
#![deny(clippy::arithmetic_side_effects)]

use crate::harness::{AVAILABLE_PROVIDERS, Harness, NOT_PROVEN, SCHEMA};
use crate::panel::{self, PanelValues};
use serde_json::{Value, json};

pub struct CommandSpec {
    pub name: &'static str,
    pub usage: &'static str,
    pub summary: &'static str,
}

/// Every command, in the order the manual lists them.
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec { name: "help", usage: "help [--json]", summary: "This manual (also --help). With --json the same content as JSON." },
    CommandSpec { name: "descriptors", usage: "descriptors", summary: "Owner and version parsed from the real DESCRIPTOR const of every foundation the harness uses." },
    CommandSpec { name: "providers", usage: "providers [--provider ID]...", summary: "Providers the harness can register and the ones selected on this command line." },
    CommandSpec { name: "open", usage: "open PACKAGE [--references unresolved|trust_all]", summary: "Open a .handshake package and print a summary: document, layers, artboards, page spreads, sequences, stories." },
    CommandSpec { name: "panel", usage: "panel PACKAGE --layer ID", summary: "Print the control panel bound to a layer (agent JSON of the controls descriptors, current values, commit route of every control)." },
    CommandSpec { name: "edit", usage: "edit PACKAGE --provider chronicle.patch (--rename LAYER NAME | --visible LAYER true|false | --set LAYER CONTROL VALUE)... [--expect-revision N] [--save OUT [--overwrite]] [--cancel]", summary: "Open, apply edits through the selected provider (revision-checked), save (or checkpoint in memory), reopen in a second session and verify. One record per operation. --save OUT may not name the input PACKAGE unless --overwrite is given (usage error, exit 2)." },
    CommandSpec { name: "import", usage: "import SOURCE [--references unresolved|trust_all] [--save OUT.handshake [--overwrite]] [--cancel]", summary: "Open directly / convert to native: recognise SOURCE by its magic, run the selected importer (PSD/PSB with feature import-psd; DNG and develop-decodable camera raw with feature import-raw, developed into one linear-light layer; plain TIFF with feature import-tiff, first page as one raster layer, Photoshop layers in tag 37724 (big-endian) imported as layers through interop-psd with the composite as typed loss, other pages recorded as typed loss; PDF with feature import-pdf is carried, not imported: an empty document, the original PDF and a receipt with page count, media boxes and one pdf_pages_not_imported loss per page; embedded ICC profiles are converted by Prism into the linear-light sRGB working space, a missing or unusable profile is a typed loss entry), write a native package (document, tiles, typed import entries: records, receipt with loss report and provenance, original bytes), open it as a session, then save to OUT (or checkpoint in memory), reopen and verify. Unknown bytes are format_unrecognised. An Affinity file (.af/.afphoto/.afdesign/.afpub, magic 00 FF 4B 41) is always refused with affinity_native_use_export: export PSD (layers kept), PDF or SVG from Affinity and import that file; the library call import::import_affinity_preview (feature import-affinity) opens only its embedded preview as one flattened, lossy layer." },
    CommandSpec { name: "export", usage: "export SOURCE (--png | --jpeg [--quality 1..100]) OUT [--overwrite]", summary: "Export the composited document as PNG or JPEG (feature viewer): SOURCE (a .handshake package or any importable source) is opened like the viewer opens it, composited with render-cpu, encoded by hsk-studio-interop-raster (PNG 8-bit RGB/RGBA untagged sRGB; JPEG baseline, quality default 90, 4:4:4 at >= 90 else 4:2:0, transparency flattened on white), decoded back to check, then written atomically to exactly OUT. OUT equal to SOURCE is refused unless --overwrite. Without feature viewer: error feature_unavailable." },
    CommandSpec { name: "develop", usage: "develop SOURCE OUT (--png [--depth 8|16] | --jpeg [--quality 1..100]) [--adjust CONTROL VALUE]... [--threads N] [--overwrite]", summary: "Develop a camera raw or DNG and export it (feature viewer): SOURCE is opened like the viewer opens it (import-raw route), every --adjust is applied as ONE develop step (controls develop.exposure, develop.temperature, develop.tint, develop.contrast, develop.highlights, develop.shadows, develop.whites, develop.blacks, develop.vibrance, develop.saturation, develop.clarity, develop.texture, develop.dehaze, develop.sharpen_amount, develop.luminance_noise, develop.colour_noise, develop.sharpen_radius, develop.sharpen_detail, develop.sharpen_masking, develop.grading.<shadows|midtones|highlights|global>.<hue|saturation|luminance>, develop.grading.blending, develop.grading.balance, develop.wb_kelvin and develop.wb_tint (switch to raw-form white balance): fields of hsk-studio-develop's recipe, validated by develop; an out-of-range or non-finite value is develop's typed refusal, e.g. invalid_adjustment, non_finite, stage_overflow) committed through the session's chronicle provider like every viewer edit, then hsk_studio_develop::develop_to_output renders sRGB-transfer samples (PNG 16-bit by default or 8-bit; JPEG 8-bit, quality default 90) that interop-raster encodes with the CC0 sRGB-v4 profile embedded, decodes back and writes atomically to exactly OUT. --threads N (1..64) only changes speed: the output is identical for every count. A source that is not a camera raw is not_a_raw_document; a truncated raw is develop's own decode refusal." },
    CommandSpec { name: "export-sequence", usage: "export-sequence PACKAGE --sequence ID --preset ID [--in T --out T] --output OUT [--overwrite] [--media FILE]... [--audio FILE]... [--width W --height H] [--json]", summary: "Export a document sequence, nested sequence clips included, to a media file (feature media; lane SMB-PR-SEQ-EXPORT-UI-1): PACKAGE is opened like the viewer opens it, every --media file is bound to the program monitor through reel's decoders as clip source sha256:<hex>, every --audio file (WAV, MP3, FLAC, Ogg, AAC, AIFF; decoded by reel's audio decoder, all at one sample rate, no resampling) is the export's sound source as clip source sha256:<hex> of its bytes, and the viewer's Viewer::export_sequence (the same path as the timeline's Export Media form) renders the frames, mixes the sound of the --audio files as the mixer plays it (with feature audio-batch, the document's stored audio routing: clip gain and pan envelopes, track and master automation, buses, sends and effects; a build without it neither reads the routing nor plays it) and encodes through reel's export queue with Studio preset --preset (e.g. studio.dnxhr.hq.mov; H.264/HEVC need the OS encoder, feature os-encode). --in/--out are ticks (254016000000 per second, whole frames, together or not at all; default the work area, else the whole sequence); --width/--height default 1920x1080; a relative OUT is relative to the current directory. Before any file is read: an --out past the sequence end (its duration rounded up to a whole frame) is export_range, and the sizes of all --media and --audio files together are capped at 2 GiB (media_too_large). Output: export.receipt is reel's receipt (frames_encoded, audio_frames, output_sha256, outcome); exit 0 only for outcome complete. Refusals (exit 1): unknown_preset, codec_unavailable, export_range, frame_decoder_missing (no --media), audio_sources_missing (an audio clip and no --audio: never silent; the detail's hint names --audio), audio_decode, audio_empty, audio_rate_mismatch (--audio files at different rates), mixdown (an --audio set that lacks a clip's sound), media_too_large, media_unreadable, output_unresolved, nest_unresolved, unknown_sequence, open_failed; missing --sequence/--preset/--output or a non-integer --in/--out is a usage error (exit 2). Without feature media: error feature_unavailable." },
    CommandSpec { name: "verify", usage: "verify PACKAGE (--expect-revision N | --expect-name LAYER NAME | --expect-visible LAYER true|false)...", summary: "Open a package and check revision and layer values. At least one expectation is required: a verify that would check nothing is a usage error (exit 2), never a pass." },
    CommandSpec { name: "batch", usage: "batch JOB.json [--provider chronicle.patch] [--references unresolved|trust_all] [--cancel]", summary: "Run a portable batch job (STU-AUT-126..136; JOB.json is BatchJob::export: job_id, source_set explicit_file_list|folder (recurse, include/exclude * and ? patterns on the file name), applied_automation action|command_sequence|none, output_targets (format_id handshake = native package; other formats refused per target), filename_template tokens, error_policy stop_on_first_error|skip_and_continue|retry_then_skip|quarantine (copies, never moves), concurrency 1, post_processing []). Headless and quiet: an enabled always_show step, an ask overwrite policy, an unsupported source kind or template token refuses the job before any file is touched (exit 1, typed code). Prints the receipt set (one receipt per input: path, bytes, sha256, automation and parameters, target paths, outcome, attempts) and the job state. pass: at least one input and every input succeeded; nothing_checked: no input. Edits need --provider chronicle.patch." },
    CommandSpec { name: "action", usage: "action check SET.json", summary: "Decode an action set artifact strictly (STU-AUT-102..107, 114: unknown, duplicate or missing fields and out-of-domain values are typed refusals, exit 1) and run the automation validators: action_declared_step_count_mismatch, action_shortcut_collision, action_step_command_unresolvable (step kept, listed in unresolvable), action_step_parameter_key_unknown (warning), action_measured_value_missing_unit. Exit 0 when no error-severity diagnostic, else 1." },
];

pub const FLAGS: &[(&str, &str)] = &[
    ("--provider ID", "Select a provider explicitly (repeatable). Nothing is selected by default; edits need one. Available: chronicle.patch."),
    ("--references MODE", "unresolved (default, honest: nothing is available) or trust_all (development stand-in, recorded in the output)."),
    ("--cancel", "Run with an already-cancelled token: every operation is refused or canceled and nothing is written."),
    ("--save OUT", "edit and import: write the edited package atomically to exactly OUT. The harness picks no path and restricts no root: the caller owns the destination. OUT equal to the input PACKAGE is refused unless --overwrite is given."),
    ("--overwrite", "edit: allow --save OUT to replace the input PACKAGE in place; import: --save over SOURCE; export: OUT over SOURCE."),
    ("--png | --jpeg", "export: output format (exactly one)."),
    ("--quality N", "export --jpeg and develop --jpeg: JPEG quality 1..100 (default 90)."),
    ("--adjust CONTROL VALUE", "develop: set a develop slider (repeatable; all of them form one undoable step). VALUE must parse as a number (else usage, exit 2); develop decides whether it is in range."),
    ("--depth 8|16", "develop --png: bits per sample (default 16)."),
    ("--threads N", "develop: worker threads of every develop run (default: available parallelism, capped); the output does not depend on it."),
];

/// The manual as JSON.
pub fn manual_json(harness: &Harness) -> Value {
    let topics: Vec<Value> = panel::build(&PanelValues::sample())
        .map(|p| {
            hsk_studio_controls::projection::manual_topics(&p.registry)
                .into_iter()
                .map(|t| json!({"id":t.id,"title":t.title,"body":t.body}))
                .collect()
        })
        .unwrap_or_default();
    json!({
        "schema": SCHEMA,
        "command": "help",
        "ok": true,
        "purpose": "Headless development consumer of the Studio foundations with explicitly selected providers. No window, no network, no process spawn; every command prints one JSON document (except help text).",
        "commands": COMMANDS.iter().map(|c| json!({"name":c.name,"usage":c.usage,"summary":c.summary})).collect::<Vec<_>>(),
        "flags": FLAGS.iter().map(|(f, d)| json!({"flag":f,"summary":d})).collect::<Vec<_>>(),
        "providers": {"available": AVAILABLE_PROVIDERS, "selected": harness.selection().providers},
        "output": {"schema": SCHEMA, "ops": "one record per operation: op, n, ok, revision, observe{frames,status,outcome}, detail|error", "verdicts": {"pass":"at least one operation ran and every operation succeeded","fail":"an operation failed or was refused","nothing_checked":"no operation ran; never a pass"}, "exit_codes": {"0":"every operation succeeded","1":"an operation failed or was refused (JSON still printed)","2":"usage error"}},
        "panel_controls": topics,
        "automation": automation_json(),
        "descriptors": harness.collect_descriptors().unwrap_or_default(),
        "not_proven": NOT_PROVEN,
    })
}

/// The automation vocabulary (STU-AUT-175: every closed enumeration as its literal tokens) and the
/// command corpus of `Run` (lane SMB-PS-ACTIONS-1).
pub fn automation_json() -> Value {
    use crate::action::{DialogOption, UnitToken, ValueType};
    use crate::batch::{OverwritePolicy, TemplateToken};
    json!({
        "dialog_options": DialogOption::ALL.iter().map(|d| json!({"value": d.value(), "token": d.token()})).collect::<Vec<_>>(),
        "value_types": ValueType::ALL.iter().map(|t| t.token()).collect::<Vec<_>>(),
        "unit_tokens": UnitToken::ALL.iter().map(|u| u.token()).collect::<Vec<_>>(),
        "function_key_index": "0..=12 (0 none, 1..=12 F1..F12)",
        "color_index": "0..=7 (0 none)",
        "source_kinds": ["explicit_file_list", "folder", "open_documents (refused)", "selection_within_document (refused)", "catalog_query (refused)"],
        "applied_automation": ["action", "command_sequence", "none"],
        "overwrite_policies": OverwritePolicy::ALL.iter().map(|p| p.token()).collect::<Vec<_>>(),
        "error_policies": ["stop_on_first_error", "skip_and_continue", "retry_then_skip", "quarantine"],
        "template_tokens": [
            TemplateToken::OriginalFilename.token(), TemplateToken::OriginalExtension.token(),
            "sequence (start 0..=2147483647, soft 1..=100000, default 1; padding 0..=10)", TemplateToken::OutputFormat.token(),
            "custom (text)", "date, time, document_width, document_height, scale_suffix (refused)",
        ],
        "validators": [
            crate::action::CODE_STEP_COUNT_MISMATCH, crate::action::CODE_SHORTCUT_COLLISION, crate::action::CODE_COMMAND_UNRESOLVABLE,
            crate::action::CODE_PARAMETER_KEY_UNKNOWN, crate::action::CODE_MEASURED_MISSING_UNIT,
            "batch_always_show_dialog_in_headless", "batch_ask_overwrite_in_headless",
        ],
        "commands": crate::action::corpus_json(),
    })
}

/// The manual as plain text, generated from [`manual_json`].
pub fn manual_text(harness: &Harness) -> String {
    let m = manual_json(harness);
    let mut out = String::from("studio-harness: headless Studio development consumer (explicit providers, no window)\n\nCOMMANDS\n");
    for c in COMMANDS {
        out += &format!("  {}\n      {}\n", c.usage, c.summary);
    }
    out += "\nFLAGS\n";
    for (f, d) in FLAGS {
        out += &format!("  {f}\n      {d}\n");
    }
    out += &format!("\nPROVIDERS available: {}\n", AVAILABLE_PROVIDERS.join(", "));
    out += "\nOUTPUT\n  One JSON document per command: schema, command, ok, verdict, selection, ops[], observe, not_proven.\n  verdict: pass only when at least one operation ran and every operation succeeded; fail when one failed; nothing_checked when no operation ran (never a pass).\n  Exit codes: 0 pass, 1 failed or refused, 2 usage.\n";
    out += "\nPANEL CONTROLS (descriptors from hsk-studio-controls; `panel` prints the same controls with values)\n";
    for t in m["panel_controls"].as_array().into_iter().flatten() {
        out += &format!("  {} ({})\n", t["title"].as_str().unwrap_or(""), t["id"].as_str().unwrap_or(""));
        for line in t["body"].as_str().unwrap_or("").lines() {
            out += &format!("      {line}\n");
        }
    }
    out += "\nAUTOMATION (action records, recording, playback, batch; tokens are literal)\n";
    for (k, v) in m["automation"].as_object().into_iter().flatten() {
        if k != "commands" {
            out += &format!("  {k}: {v}\n");
        }
    }
    for c in crate::action::RUN_COMMANDS {
        out += &format!("  command {} (Run::{}) recordable={} mutates_authority={}\n", c.command_id, c.run_method, c.recordable, c.mutates_authority);
    }
    out += "\nNOT PROVEN\n";
    for n in NOT_PROVEN {
        out += &format!("  {n}\n");
    }
    out
}
