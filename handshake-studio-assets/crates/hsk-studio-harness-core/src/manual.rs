//! The built-in manual (UserManual pillar): generated from the same descriptors the commands run
//! on (the command table below drives both parsing-help and this text; the panel topics come from
//! the controls crate's descriptors; providers and descriptors come from the live registries).
#![deny(clippy::arithmetic_side_effects)]

use crate::diag_sink::{self, DIAG_DIR_ENV, MAX_CONSECUTIVE_FAILURES, MAX_FILE_BYTES, MAX_FILES, MAX_LINE_BYTES};
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
    CommandSpec { name: "edit", usage: "edit PACKAGE --provider chronicle.patch (--rename LAYER NAME | --visible LAYER true|false | --set LAYER CONTROL VALUE)... [--expect-revision N] [--save OUT [--overwrite]] [--cancel]", summary: "Open, apply edits through the selected provider (revision-checked), save (or checkpoint in memory), reopen in a second session and verify. One record per operation. --save OUT naming the input PACKAGE needs both --overwrite and --expect-revision N (usage error, exit 2): N is the revision the package was opened at, checked on the first edit. The save compares the file with what this process read: another writer's save since then is destination_changed, a removed file destination_missing, and nothing is written. --save over another existing file is destination_exists unless --overwrite is given. A read-only file or a directory that takes no new file is destination_read_only; save_busy means another save of that file held its lock for the whole wait. Every refused save record carries detail.recovery: run again from the current revision (`verify` or `open` prints it), or save to another path." },
    CommandSpec { name: "import", usage: "import SOURCE [--references unresolved|trust_all] [--save OUT.handshake [--overwrite]] [--cancel]", summary: "--save over an existing file other than SOURCE is destination_exists unless --overwrite is given. Open directly / convert to native: recognise SOURCE by its magic, run the selected importer (PSD/PSB with feature import-psd; DNG and develop-decodable camera raw with feature import-raw, developed into one linear-light layer; plain TIFF with feature import-tiff, first page as one raster layer, Photoshop layers in tag 37724 (big-endian) imported as layers through interop-psd with the composite as typed loss, other pages recorded as typed loss; PDF with feature import-pdf is carried, not imported: an empty document, the original PDF and a receipt with page count, media boxes and one pdf_pages_not_imported loss per page; embedded ICC profiles are converted by Prism into the linear-light sRGB working space, a missing or unusable profile is a typed loss entry), write a native package (document, tiles, typed import entries: records, receipt with loss report and provenance, original bytes), open it as a session, then save to OUT (or checkpoint in memory), reopen and verify. Unknown bytes are format_unrecognised. An Affinity file (.af/.afphoto/.afdesign/.afpub, magic 00 FF 4B 41) is always refused with affinity_native_use_export: export PSD (layers kept), PDF or SVG from Affinity and import that file; the library call import::import_affinity_preview (feature import-affinity) opens only its embedded preview as one flattened, lossy layer." },
    CommandSpec { name: "export", usage: "export SOURCE (--png | --jpeg [--quality 1..100]) OUT [--overwrite]", summary: "Export the composited document as PNG or JPEG (feature viewer): SOURCE (a .handshake package or any importable source) is opened like the viewer opens it, composited with render-cpu, encoded by hsk-studio-interop-raster (PNG 8-bit RGB/RGBA untagged sRGB; JPEG baseline, quality default 90, 4:4:4 at >= 90 else 4:2:0, transparency flattened on white), decoded back to check, then written atomically to exactly OUT. OUT equal to SOURCE is refused unless --overwrite. Without feature viewer: error feature_unavailable." },
    CommandSpec { name: "develop", usage: "develop SOURCE OUT (--png [--depth 8|16] | --jpeg [--quality 1..100]) [--adjust CONTROL VALUE]... [--threads N] [--overwrite]", summary: "Develop a camera raw or DNG and export it (feature viewer): SOURCE is opened like the viewer opens it (import-raw route), every --adjust is applied as ONE develop step (controls develop.exposure, develop.temperature, develop.tint, develop.contrast, develop.highlights, develop.shadows, develop.whites, develop.blacks, develop.vibrance, develop.saturation, develop.clarity, develop.texture, develop.dehaze, develop.sharpen_amount, develop.luminance_noise, develop.colour_noise, develop.sharpen_radius, develop.sharpen_detail, develop.sharpen_masking, develop.grading.<shadows|midtones|highlights|global>.<hue|saturation|luminance>, develop.grading.blending, develop.grading.balance, develop.wb_kelvin and develop.wb_tint (switch to raw-form white balance): fields of hsk-studio-develop's recipe, validated by develop; an out-of-range or non-finite value is develop's typed refusal, e.g. invalid_adjustment, non_finite, stage_overflow) committed through the session's chronicle provider like every viewer edit, then hsk_studio_develop::develop_to_output renders sRGB-transfer samples (PNG 16-bit by default or 8-bit; JPEG 8-bit, quality default 90) that interop-raster encodes with the CC0 sRGB-v4 profile embedded, decodes back and writes atomically to exactly OUT. --threads N (1..64) only changes speed: the output is identical for every count. A source that is not a camera raw is not_a_raw_document; a truncated raw is develop's own decode refusal." },
    CommandSpec { name: "export-sequence", usage: "export-sequence PACKAGE --sequence ID --preset ID [--in T --out T] --output OUT [--overwrite] [--media FILE]... [--audio FILE]... [--width W --height H] [--json]", summary: "Export a document sequence, nested sequence clips included, to a media file (feature media; lane SMB-PR-SEQ-EXPORT-UI-1): PACKAGE is opened like the viewer opens it, every --media file is bound to the program monitor through reel's decoders as clip source sha256:<hex>, every --audio file (WAV, MP3, FLAC, Ogg, AAC, AIFF; decoded by reel's audio decoder, all at one sample rate, no resampling) is the export's sound source as clip source sha256:<hex> of its bytes, and the viewer's Viewer::export_sequence (the same path as the timeline's Export Media form) renders the frames, mixes the sound of the --audio files as the mixer plays it (with feature audio-batch, the document's stored audio routing: clip gain and pan envelopes, track and master automation, buses, sends and effects; a build without it neither reads the routing nor plays it) and encodes through reel's export queue with Studio preset --preset (e.g. studio.dnxhr.hq.mov; H.264/HEVC need the OS encoder, feature os-encode). --in/--out are ticks (254016000000 per second, whole frames, together or not at all; default the work area, else the whole sequence); --width/--height default 1920x1080; a relative OUT is relative to the current directory. Before any file is read: an --out past the sequence end (its duration rounded up to a whole frame) is export_range, more than 16 --audio files is audio_too_many (detail limit), an --audio file over 1 GiB (1073741824 bytes, the viewer audio import's file limit) is media_too_large (detail limit), and the sizes of all --media and --audio files together are capped at 2 GiB (media_too_large, detail total_limit). The decoded audio of all --audio files shares one budget of 4147200000 bytes (frames x channels x 4-byte samples; each decode is refused before it holds more than the remainder): over it audio_too_large (detail decoded_limit). Output: export.receipt is reel's receipt (frames_encoded, audio_frames, output_sha256, outcome); exit 0 only for outcome complete. Refusals (exit 1): unknown_preset, codec_unavailable, export_range, frame_decoder_missing (no --media), audio_sources_missing (an audio clip and no --audio: never silent; the detail's hint names --audio), audio_decode, audio_empty, audio_rate_mismatch (--audio files at different rates), audio_too_many, audio_too_large, audio_port (the viewer refused the decoded sound port), mixdown (an --audio set that lacks a clip's sound), media_too_large, media_unreadable, output_unresolved, nest_unresolved, unknown_sequence, open_failed; missing --sequence/--preset/--output or a non-integer --in/--out is a usage error (exit 2). Without feature media: error feature_unavailable." },
    CommandSpec { name: "verify", usage: "verify PACKAGE (--expect-revision N | --expect-name LAYER NAME | --expect-visible LAYER true|false)...", summary: "Open a package and check revision and layer values. At least one expectation is required: a verify that would check nothing is a usage error (exit 2), never a pass." },
    CommandSpec { name: "batch", usage: "batch JOB.json [--provider chronicle.patch] [--references unresolved|trust_all] [--cancel]", summary: "Run a portable batch job (STU-AUT-126..136; JOB.json is BatchJob::export: job_id, source_set explicit_file_list|folder (recurse, include/exclude * and ? patterns on the file name), applied_automation action|command_sequence|none, output_targets (format_id handshake = native package; other formats refused per target), filename_template tokens, error_policy stop_on_first_error|skip_and_continue|retry_then_skip|quarantine (copies, never moves), concurrency 1, post_processing []). Headless and quiet: an enabled always_show step, an ask overwrite policy, an unsupported source kind or template token refuses the job before any file is touched (exit 1, typed code). Prints the receipt set (one receipt per input: path, bytes, sha256, automation and parameters, target paths, outcome, attempts) and the job state. pass: at least one input and every input succeeded; nothing_checked: no input. Edits need --provider chronicle.patch." },
    CommandSpec { name: "state", usage: "state PACKAGE --actor ID --session ID [--owner ID] [--access-space ID]", summary: "Open PACKAGE on the CPU path, no window, and print the revision, the resolved attribution and the viewer state (state_json: layers, frame, receipt, edit and history, develop, ops, status, shell, timeline, proof, design, info). Feature viewer; without it: error feature_unavailable." },
    CommandSpec { name: "command", usage: "command PACKAGE --actor ID --session ID [--owner ID] [--access-space ID] (--command-id ID [--param KEY VALUE]... [--expect-revision N] [--save OUT [--overwrite]] | --stdin)", summary: "Run one command id from DRIVE COMMANDS on PACKAGE and print the same response as state, with command_id and detail. Nothing is written without --save OUT; --save over PACKAGE needs both --overwrite and --expect-revision. --stdin is the live session: one process, one document, one undo history, JSONL in and out (see DRIVE SESSION). Feature viewer." },
    CommandSpec { name: "undo", usage: "undo PACKAGE --actor ID --session ID [--owner ID] [--access-space ID]", summary: "Always refuses undo_needs_live_session, exit 1: the undo history lives in the process that made the edits, and a package carries none. Send {\"command\":\"undo\"} in a `command --stdin` session. Feature viewer." },
    CommandSpec { name: "health", usage: "health [--diag-dir DIR]", summary: "Is this build able to work and leave a record? Parses the foundation descriptors and writes one line to the diag dir, then prints one JSON document: checks[], previous_runs (the runs in the diag dir that never reached run_end: a process that died, or one still running; panic files) and not_proven. exit 0 when every check passed, else the first failure: 3 diag_not_configured, 4 diag_unavailable, 5 descriptor_mismatch. See DIAGNOSTICS." },
    CommandSpec { name: "action", usage: "action check SET.json", summary: "Decode an action set artifact strictly (STU-AUT-102..107, 114: unknown, duplicate or missing fields and out-of-domain values are typed refusals, exit 1) and run the automation validators: action_declared_step_count_mismatch, action_shortcut_collision, action_step_command_unresolvable (step kept, listed in unresolvable), action_step_parameter_key_unknown (warning), action_measured_value_missing_unit. Exit 0 when no error-severity diagnostic, else 1." },
];

pub const FLAGS: &[(&str, &str)] = &[
    ("--diag-dir DIR", "Every command: also leave what this run did as JSONL in DIR (see DIAGNOSTICS); the binary then uses DIR instead of its default. A failure to write there never changes the command's result."),
    ("--provider ID", "Select a provider explicitly (repeatable). Nothing is selected by default; edits need one. Available: chronicle.patch."),
    ("--references MODE", "unresolved (default, honest: nothing is available) or trust_all (development stand-in, recorded in the output)."),
    ("--cancel", "Run with an already-cancelled token: every operation is refused or canceled and nothing is written."),
    ("--expect-revision N", "edit: the revision PACKAGE is expected at when opened, checked on the first edit and refused stale_revision otherwise; the later edits of the command line follow it. Required for --save over the input PACKAGE. verify: the revision the file must hold."),
    ("--save OUT", "edit and import: write the edited package atomically to exactly OUT. The harness picks no path and restricts no root: the caller owns the destination. OUT equal to the input PACKAGE is refused unless --overwrite and, for edit, --expect-revision are given; another existing OUT is destination_exists unless --overwrite is given."),
    ("--overwrite", "edit: allow --save OUT to replace the input PACKAGE in place (still refused destination_changed when another writer changed it since this process read it) or another existing file (replaced without a compare, last writer wins: the one case the caller asks for it); import: --save over SOURCE or another existing file; export: OUT over SOURCE."),
    ("--png | --jpeg", "export: output format (exactly one)."),
    ("--quality N", "export --jpeg and develop --jpeg: JPEG quality 1..100 (default 90)."),
    ("--adjust CONTROL VALUE", "develop: set a develop slider (repeatable; all of them form one undoable step). VALUE must parse as a number (else usage, exit 2); develop decides whether it is in range."),
    ("--depth 8|16", "develop --png: bits per sample (default 16)."),
    ("--threads N", "develop: worker threads of every develop run (default: available parallelism, capped); the output does not depend on it."),
    ("--actor ID", "state, command, undo (required, no default): account and principal of every command of this process."),
    ("--session ID", "state, command, undo (required, no default): session and client session of every command of this process."),
    ("--owner ID", "state, command, undo: owner account and principal. Default: the actor, a stated limit: a default records the actor as its own owner. Every response prints the resolved attribution."),
    ("--access-space ID", "state, command, undo: access space. Default: the actor, the same limit."),
    ("--command-id ID", "command: a DRIVE COMMANDS id."),
    ("--param KEY VALUE", "command: one declared parameter of the command id (repeatable); VALUE is text and decodes by the declared type."),
    ("--stdin", "command: read JSONL commands from stdin in one process (DRIVE SESSION)."),
];

/// One typed parameter of a driveable timeline command. `kind`: text, text_list (comma separated or
/// a JSON list of strings), ticks (u64), frames (i64), count (u64), boolean.
pub struct DriveParam {
    pub key: &'static str,
    pub kind: &'static str,
    pub required: bool,
}

const fn need(key: &'static str, kind: &'static str) -> DriveParam {
    DriveParam { key, kind, required: true }
}

const fn may(key: &'static str, kind: &'static str) -> DriveParam {
    DriveParam { key, kind, required: false }
}

/// One driveable `timeline.<key>` command: `key` is `TimelineCommand::key()`, `spec_commands` every
/// [STU-VID-041] command id the variant names, where the tables name one (harness-view has no edge
/// from this crate; the harness crate's tests pin this table to the enum).
pub struct DriveTimeline {
    pub key: &'static str,
    pub params: &'static [DriveParam],
    pub spec_commands: &'static [&'static str],
}

const fn row(key: &'static str, params: &'static [DriveParam], spec_commands: &'static [&'static str]) -> DriveTimeline {
    DriveTimeline { key, params, spec_commands }
}

const CLIP_DELTA: &[DriveParam] = &[need("clip_id", "text"), need("delta_frames", "frames")];
const RIGHT_MANY: &[DriveParam] = &[need("right", "boolean"), need("many", "boolean")];

/// The `RUN_COMMANDS` rows the process drives; their parameters are the rows' own. Any other row
/// refuses `command_not_driveable`.
pub const DRIVE_RUN_COMMANDS: &[&str] = &["layer.rename", "layer.set_visible", "layer.set_opacity", "layer.set_blend_mode", "layer.panel_set", "history.undo", "history.redo", "document.save"];

/// The timeline commands the process drives, addressed `timeline.<key>`.
pub const DRIVE_TIMELINE: &[DriveTimeline] = &[
    row("select", &[may("clip_id", "text")], &[]),
    row("move_clip", &[need("clip_id", "text"), need("start", "ticks"), may("track_id", "text")], &[]),
    row("trim_in", &[need("clip_id", "text"), need("head", "ticks")], &[]),
    row("trim_out", &[need("clip_id", "text"), need("tail", "ticks")], &[]),
    row("razor", &[], &["cmd.sequence.razorateditline"]),
    row("lift", &[], &[]),
    row("ripple_delete", &[], &["cmd.timeline.ripple.delete"]),
    row("marker_add", &[], &[]),
    row("marker_next", &[], &["cmd.marker.gotomarker.next"]),
    row("marker_previous", &[], &["cmd.marker.gotomarker.previous"]),
    row("edit_next", &[], &["cmd.tlnav.next.edit"]),
    row("edit_previous", &[], &["cmd.tlnav.prev.edit"]),
    row("snap", &[], &["cmd.sequence.snap"]),
    row("zoom_in", &[], &[]),
    row("zoom_out", &[], &[]),
    row("zoom_fit", &[], &[]),
    row("scroll", &[need("first_frame", "count")], &[]),
    row("scrub", &[need("tick", "ticks")], &[]),
    row("ripple_trim_in", CLIP_DELTA, &[]),
    row("ripple_trim_out", CLIP_DELTA, &[]),
    row("roll", CLIP_DELTA, &[]),
    row("slip", CLIP_DELTA, &[]),
    row("slide", CLIP_DELTA, &[]),
    row("trim_nudge", &[need("forward", "boolean"), need("many", "boolean")], &["cmd.trim.plus.minor", "cmd.trim.plus.major", "cmd.trim.minus.minor", "cmd.trim.minus.major"]),
    row("slip_nudge", RIGHT_MANY, &["cmd.timeline.slip.left.one", "cmd.timeline.slip.left.several", "cmd.timeline.slip.right.one", "cmd.timeline.slip.right.several"]),
    row("slide_nudge", RIGHT_MANY, &["cmd.timeline.slide.left.one", "cmd.timeline.slide.left.several", "cmd.timeline.slide.right.one", "cmd.timeline.slide.right.several"]),
    row("clip_nudge", RIGHT_MANY, &["cmd.timeline.nudge.left.one", "cmd.timeline.nudge.left.several", "cmd.timeline.nudge.right.one", "cmd.timeline.nudge.right.several"]),
    row("clip_track_nudge", &[need("up", "boolean")], &["cmd.timeline.nudge.up", "cmd.timeline.nudge.down"]),
    row("extend_edit", &[], &[]),
    row("multi_frame_step", &[need("step", "count")], &[]),
    row("trim_session_enter", &[], &[]),
    row("trim_session_type", &[], &[]),
    row("trim_session_commit", &[], &[]),
    row("trim_session_rollback", &[], &[]),
    row("composite_preview", &[], &["cmd.timeline.composite.preview.during.trim"]),
    row("nest", &[may("clips", "text_list")], &["cmd.clip.nestify"]),
    row("make_subsequence", &[may("clips", "text_list")], &["cmd.sequence.makesubsequence"]),
    row("open_nested", &[may("clip_id", "text")], &["cmd.tlnav.reveal.nested.sequence"]),
    row("back_to_parent", &[], &[]),
];

/// Timeline keys that exist and refuse `command_not_driveable`, with the reason.
pub const DRIVE_TIMELINE_NOT_DRIVEABLE: &[(&str, &str)] = &[
    ("edit_select", "takes an edit mode with no text form yet"),
    ("edit_point", "takes an edit mode with no text form yet"),
    ("tool", "takes a trim tool with no text form yet"),
    ("trim_session_focus", "takes a trim focus with no text form yet"),
    ("rate_stretch", "takes a clip edge with no text form yet"),
    ("speed_duration", "takes a speed target with no text form yet"),
    ("speed_duration_open", "a form of the panel, not yet driven"),
    ("speed_duration_apply", "a form of the panel, not yet driven"),
    ("speed_duration_cancel", "a form of the panel, not yet driven"),
    ("export_media_open", "an export job; use export-sequence"),
    ("export_media_run", "an export job; use export-sequence"),
    ("export_media_cancel", "an export job; use export-sequence"),
];

/// One driveable `viewer.<method>` command: `method` is the Viewer method's own name.
pub struct DriveViewer {
    pub method: &'static str,
    pub params: &'static [DriveParam],
}

const fn method(method: &'static str, params: &'static [DriveParam]) -> DriveViewer {
    DriveViewer { method, params }
}

const LAYER: &[DriveParam] = &[need("layer", "text")];
const XY: &[DriveParam] = &[need("x", "number"), need("y", "number")];
const RECT: &[DriveParam] = &[need("x", "number"), need("y", "number"), need("width", "number"), need("height", "number")];
const POINTS: &[DriveParam] = &[need("points", "points")];
const DRAG: &[DriveParam] = &[need("from", "point"), need("to", "point"), need("radius", "number")];
const OUT: &[DriveParam] = &[need("path", "path"), may("overwrite", "boolean")];
const LAYER_STYLE: &[DriveParam] = &[need("layer", "text"), need("style", "text")];
const SWATCH_ROW: &str = "one_of:fill|stroke";

/// The Viewer's non-draw methods the process drives, addressed `viewer.<method>`, with the same
/// typed refusals and history as the window.
pub const DRIVE_VIEWER: &[DriveViewer] = &[
    method("new_raster_layer", &[need("name", "text")]),
    method("duplicate_layer", LAYER),
    method("delete_layer", LAYER),
    method("shift_layer", &[need("layer", "text"), need("up", "boolean")]),
    method("ungroup", &[need("group", "text")]),
    method("group_layers", &[need("layers", "text_list"), need("name", "text")]),
    method("move_layer", &[need("layer", "text"), need("place", "one_of:inside|at_beginning|at_end|before|after"), may("target", "text")]),
    method("select", &[may("layer", "text")]),
    method("set_tool", &[need("tool", "text")]),
    method("set_combine", &[need("mode", "one_of:replace|extend|diminish|intersect")]),
    method("set_brush", &[may("diameter_px", "number"), may("hardness_pct", "number"), may("spacing_pct", "number"), may("opacity_pct", "number"), may("rgb8", "rgb8")]),
    method("select_rect", RECT),
    method("select_ellipse", RECT),
    method("select_lasso", POINTS),
    method("clear_selection", &[]),
    method("undo_selection", &[]),
    method("select_wand", &[need("layer", "text"), need("x", "integer"), need("y", "integer"), may("tolerance", "number"), may("contiguous", "boolean")]),
    method("ps_click", XY),
    method("refine_end", &[]),
    method("paint", &[need("layer", "text"), need("samples", "samples")]),
    method("fill_click", XY),
    method("fill_drag", &[need("from", "point"), need("to", "point")]),
    method("info_eyedropper", XY),
    method("set_develop", &[need("control", "text"), need("value", "number")]),
    method("set_develop_threads", &[need("count", "integer")]),
    method("set_develop_export_field", &[need("id", "text"), need("value", "text")]),
    method("load_develop_watermark", &[need("path", "path")]),
    method("clear_develop_export_stage", &[]),
    method("export", &[need("path", "path"), need("format", "one_of:png|jpeg"), may("quality", "integer"), may("overwrite", "boolean")]),
    method("export_vector_svg", OUT),
    method("export_vector_pdf", OUT),
    method("pen_point", &[need("x", "number"), need("y", "number"), may("drag", "point"), need("close_radius", "number")]),
    method("pen_end", &[]),
    method("curvature_point", &[need("x", "number"), need("y", "number"), need("corner", "boolean"), need("close_radius", "number")]),
    method("draw_shape", &[need("kind", "one_of:rectangle|rounded_rectangle|ellipse|polygon|star|line|arc_segment|rectangular_grid|polar_grid|spiral"), need("a", "point"), need("b", "point"), may("constrain", "boolean")]),
    method("pencil_stroke", POINTS),
    method("blob_brush_stroke", POINTS),
    method("knife_cut", POINTS),
    method("direct_drag", &[need("from", "point"), need("to", "point"), need("radius", "number"), may("angle45", "boolean"), may("break_mirroring", "boolean")]),
    method("reshape_drag", DRAG),
    method("width_drag", DRAG),
    method("set_width_point", &[need("layer", "text"), need("t", "number"), need("width", "number")]),
    method("clear_width", LAYER),
    method("expand_shape", LAYER),
    method("promote_network", LAYER),
    method("outline_stroke_layer", LAYER),
    method("offset_path_layer", &[need("layer", "text"), need("offset", "number"), need("join", "one_of:miter|round|bevel")]),
    method("create_outlines", LAYER),
    method("convert_text_kind", LAYER),
    method("bind_live_text", &[need("layer", "text"), need("text", "text"), need("kind", "one_of:path|area"), may("inset_pt", "number")]),
    method("text_click", &[need("p", "point"), need("radius", "number"), need("text", "text")]),
    method("network_paint_bucket", &[need("layer", "text"), need("p", "point"), need("fill", "boolean")]),
    method("network_region_swatch", &[need("layer", "text"), need("p", "point"), may("swatch", "text")]),
    method("network_pen", &[need("layer", "text"), need("x", "number"), need("y", "number"), may("drag", "point"), need("radius", "number")]),
    method("pathfinder_layers", &[need("op", "one_of:union|intersect|exclude|subtract_front|subtract_back|divide|trim|merge|crop|outline|hard_mix|soft_mix|trap"), need("layers", "text_list")]),
    method("shape_builder_layers", &[need("layers", "text_list"), need("gesture", "points"), need("delete", "boolean")]),
    method("make_clip", &[need("target", "text"), need("source", "text"), may("even_odd", "boolean")]),
    method("release_masks", &[need("target", "text")]),
    method("expand_envelope", LAYER),
    method("release_envelope", LAYER),
    method("move_envelope_point", &[need("layer", "text"), need("index", "integer"), need("to", "point")]),
    method("create_gradient_mesh", &[need("layer", "text"), need("rows", "integer"), need("columns", "integer")]),
    method("clear_appearance", LAYER),
    method("break_graphic_style", LAYER),
    method("reduce_to_basic_appearance", LAYER),
    method("add_graphic_style", &[need("name", "text"), need("layer", "text")]),
    method("apply_graphic_style", LAYER_STYLE),
    method("merge_graphic_style", LAYER_STYLE),
    method("redefine_graphic_style", LAYER_STYLE),
    method("apply_brush", &[need("layer", "text"), need("brush", "text"), may("scale", "number")]),
    method("remove_brush", LAYER),
    method("swap_stroke_markers", &[need("layer", "text"), need("row", "integer")]),
    method("apply_swatch", &[need("layer", "text"), need("swatch", "text"), need("kind", SWATCH_ROW)]),
    method("apply_swatch_tinted", &[need("layer", "text"), need("swatch", "text"), need("kind", SWATCH_ROW), may("tint", "number")]),
    method("detach_swatch", &[need("layer", "text"), need("kind", SWATCH_ROW)]),
    method("grid_append_child_at", &[need("container", "text"), need("node", "text"), need("row", "integer"), need("column", "integer")]),
    method("set_grid_gaps", &[need("node", "text"), need("row_gap", "number"), need("column_gap", "number")]),
    method("auto_layout_control", &[need("widget_id", "text"), need("value", "text")]),
    method("help_search", &[need("text", "text")]),
    method("help_close", &[]),
    method("dialog_accept", &[]),
    method("dialog_cancel", &[]),
    method("close", &[]),
    method("proof_apply_draft", &[]),
    method("proof_close_page", &[]),
    method("proof_set_save_name", &[need("name", "text")]),
];

/// Menu leaves that refuse `command_not_driveable`, with the reason.
pub const DRIVE_SHELL_NOT_DRIVEABLE: &[(&str, &str)] = &[
    ("studio.view.render_gpu", "the process stays on the CPU path; it holds no GPU device"),
    ("studio.document.save", "writes a path the caller never named; use document.save"),
];

/// Refusal codes of the drive surface itself; a command's own code passes through unchanged.
pub const DRIVE_REFUSALS: &[&str] = &[
    "feature_unavailable", "actor_invalid", "open_failed", "undo_needs_live_session", "line_malformed", "line_not_utf8", "line_too_long", "unknown_command", "command_not_driveable",
    "parameter_missing", "parameter_unknown", "parameter_invalid", "stale_revision", "overwrite_required", "expect_revision_required", "export_over_source", "no_timeline", "command_refused", "command_panicked", "save_refused", "timeline_refused",
];

/// What the drive surface does not do yet: a model reads these from `help`, not from a surprise.
pub const DRIVE_LIMITS: &[&str] = &[
    "--owner and --access-space default to the actor, which records a model as its own owner; attribution strings are transport identifiers, not proof of a grant",
    "undo works only inside one --stdin process; the history is not in the package",
    "no dry_run, no input or output schema, no capability grant and no promotion gate on any command yet; command ids are RUN_COMMANDS ids, not studio.<domain>.<command>.v<N>",
    "--diag-dir is not available until the observe diagnostics sink lands",
    "every reply carries the whole state and renders a changed document on the CPU path; a long session's reply grows with its ops",
    "not driveable yet: layer.restructure and the staged-payload rows of RUN_COMMANDS; Viewer methods that take a whole typed record (paint, stroke, swatch and envelope specs, path and network edits, design and proof commands, liquify, grid tracks, align and transform)",
    "the coercion is text: a JSON number given for a text parameter arrives as its digits",
    "one terminal observe frame per command holds only for commands that reach a Run operation: a refusal before dispatch and the rows that never reach Run emit none (viewer.select, set_tool, set_brush, select_rect, select_ellipse, select_lasso, clear_selection, the three exports, timeline zoom, scroll and scrub, studio.view.*); a selection reaches the package at the next save with only that save's record",
    "viewer.<method> is a transitional prefix over Viewer method names: these rows have no RUN_COMMANDS entry and no controls registry id yet",
    "a command that panics answers command_panicked and the session goes on; what it had already changed stays changed, read state",
];

/// The drive surface as JSON, from the tables above and the command corpus.
pub fn drive_json() -> Value {
    let params = |p: &[DriveParam]| p.iter().map(|d| json!({"key": d.key, "kind": d.kind, "required": d.required})).collect::<Vec<_>>();
    json!({
        "commands": crate::action::corpus_json().as_array().into_iter().flatten().filter(|c| c["command_id"].as_str().is_some_and(|id| DRIVE_RUN_COMMANDS.contains(&id))).cloned().collect::<Vec<_>>(),
        "motion_commands": crate::action::corpus_json().as_array().into_iter().flatten().filter(|c| c["run_method"] == "motion_command").cloned().collect::<Vec<_>>(),
        "timeline": DRIVE_TIMELINE.iter().map(|r| json!({"command_id": format!("timeline.{}", r.key), "parameters": params(r.params), "spec_commands": r.spec_commands})).collect::<Vec<_>>(),
        "viewer": DRIVE_VIEWER.iter().map(|r| json!({"command_id": format!("viewer.{}", r.method), "parameters": params(r.params)})).collect::<Vec<_>>(),
        "shell": {"command_ids": "every menu leaf id in state.shell.menu (studio.*), through the menu's own availability; no parameters", "not_driveable": DRIVE_SHELL_NOT_DRIVEABLE.iter().map(|(id, why)| json!({"command_id": id, "reason": why})).collect::<Vec<_>>()},
        "parameter_kinds": "text; text_list (comma separated or a JSON list of strings); number; integer; boolean; ticks; frames; count; path; point [x, y]; points [[x, y], ...]; samples [[x, y] or [x, y, pressure], ...]; rgb8 [r, g, b]; one_of:a|b|c. A list given in --param is its JSON text.",
        "timeline_not_driveable": DRIVE_TIMELINE_NOT_DRIVEABLE.iter().map(|(k, why)| json!({"command_id": format!("timeline.{k}"), "reason": why})).collect::<Vec<_>>(),
        "session_lines": ["{\"command\":\"state\"}", "{\"command\":\"undo\",\"expected_revision\":N}", "{\"command\":\"command\",\"command_id\":ID,\"parameters\":{KEY:VALUE},\"expected_revision\":N}"],
        "response": "schema, command, ok, verdict, revision, attribution, command_id, line (session), detail, error (a refusal), state (state_json). The first session line reports the open, and a closed stdin ends with ended, lines, refused. Exit 0 only when nothing was refused.",
        "open_timeline": "the first timeline command opens the timeline on the document's first sequence; a document with none refuses no_timeline",
        "document_save": "overwrite is optional, default false, and no existing file is replaced without it; saving over the opened PACKAGE also needs expected_revision; an export onto the opened PACKAGE always refuses export_over_source",
        "line_limit_bytes": 1_048_576,
        "refusals": DRIVE_REFUSALS,
        "limits": DRIVE_LIMITS,
    })
}

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
        "purpose": "Headless development consumer of the Studio foundations with explicitly selected providers. No window, no network, no process spawn; every command prints one JSON document (except help text; a `command --stdin` session prints one JSON line per input line).",
        "window": "Feature desktop adds the binary's window: studio-harness [--cpu] [--recent LIST] [FILE], a manual Operator step. Any command word above answers with no window; a word that is no command and no existing file prints usage, exit 2, no window.",
        "drive": drive_json(),
        "commands": COMMANDS.iter().map(|c| json!({"name":c.name,"usage":c.usage,"summary":c.summary})).collect::<Vec<_>>(),
        "flags": FLAGS.iter().map(|(f, d)| json!({"flag":f,"summary":d})).collect::<Vec<_>>(),
        "providers": {"available": AVAILABLE_PROVIDERS, "selected": harness.selection().providers},
        "output": {"schema": SCHEMA, "ops": "one record per operation: op, n, ok, revision, observe{frames,status,outcome}, detail|error", "verdicts": {"pass":"at least one operation ran and every operation succeeded","fail":"an operation failed or was refused","nothing_checked":"no operation ran; never a pass"}, "exit_codes": {"0":"every operation succeeded","1":"an operation failed or was refused (JSON still printed)","2":"usage error","3":"health only: diag_not_configured","4":"health only: diag_unavailable","5":"health only: descriptor_mismatch"}},
        "diagnostics": diagnostics_json(),
        "panel_controls": topics,
        "automation": automation_json(),
        "descriptors": harness.collect_descriptors().unwrap_or_default(),
        "not_proven": NOT_PROVEN,
    })
}

/// What the binaries leave behind (lane SMB-OBSERVE-DIAG-SINK-1); the rules are in [`crate::diag_sink`].
pub fn diagnostics_json() -> Value {
    json!({
        "what": format!("Each run of a binary leaves JSONL (schema {}) in the diag dir: run_start, one frame line per observe frame, one op line per operation, run_end with the exit code. Field names are the ring record's and the observe wire's. Ids, closed codes and counters only: never document contents, panic messages or command-line arguments.", diag_sink::DIAG_SCHEMA),
        "dir_flag": "--diag-dir DIR on every command, state, command and undo included; the window takes no flag and uses the variable or the default",
        "dir_variable": DIAG_DIR_ENV,
        "default_dir": format!("With neither --diag-dir nor {DIAG_DIR_ENV}: the per-user local state directory, the first of LOCALAPPDATA, XDG_STATE_HOME, HOME/.local/state that is set, under hsk-studio/diag; with none of them, the temp directory, under hsk-studio/diag."),
        "not_writable": format!("A location that cannot be written never fails or changes a command: there is no fallback to another location, each line is dropped and counted, and after {MAX_CONSECUTIVE_FAILURES} failures in a row the process stops trying. `health` reports it, exit 4 diag_unavailable."),
        "files": "studio-diag-<process birth ms>-<pid>-<instance>-<segment>.jsonl and studio-panic-<process birth ms>-<pid>.jsonl.",
        "bounds": format!("A line is at most {MAX_LINE_BYTES} bytes, a file at most {MAX_FILE_BYTES} bytes, a directory at most {MAX_FILES} of our files. A full file starts the next one; past the count our oldest file is removed. No other file is ever touched."),
        "panic": "A panic in a binary adds a typed panic record: source file tail, line, column, payload kind and length, never the message. The previous hook still runs, so stderr and the exit status do not change.",
        "hard_death": "A stack overflow or an abort runs no hook and leaves no record of itself. It leaves every line written up to then, each one write to the OS, and no run_end; `health` counts such a run in previous_runs.unterminated_or_running. Not power loss: only a panic record is synced to disk.",
        "library_callers": "cli::run in-process writes only with --diag-dir and reads no variable; only a binary's main resolves the default. A host gives Harness a sink with Harness::with_diag, or installs a process sink. state, command, undo and the viewer build their own harness, which takes the process sink: in a binary their frames and ops are all there, in-process only the run's start and end.",
        "tests": format!("A test that spawns a binary sets {DIAG_DIR_ENV} or passes --diag-dir on its Command, under its own temp directory, and removes it."),
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
    out += "\nOUTPUT\n  One JSON document per command: schema, command, ok, verdict, selection, ops[], observe, not_proven.\n  verdict: pass only when at least one operation ran and every operation succeeded; fail when one failed; nothing_checked when no operation ran (never a pass).\n  Exit codes: 0 pass, 1 failed or refused, 2 usage; health also 3 diag_not_configured, 4 diag_unavailable, 5 descriptor_mismatch.\n";
    let drive = drive_json();
    out += "\nWINDOW\n  Feature desktop adds the binary's window: studio-harness [--cpu] [--recent LIST] [FILE], a manual Operator step. Any command word above answers with no window; a word that is no command and no existing file prints usage, exit 2, no window.\n";
    out += "\nDRIVE COMMANDS (state, command, undo; --command-id ID or the command_id of a session line)\n";
    for c in drive["commands"].as_array().into_iter().flatten() {
        let keys: Vec<String> = c["parameters"].as_array().into_iter().flatten().map(|p| format!("{}:{}", p["key"].as_str().unwrap_or(""), p["value_type"].as_str().unwrap_or(""))).collect();
        out += &format!("  {} ({})\n", c["command_id"].as_str().unwrap_or(""), keys.join(", "));
    }
    for r in DRIVE_TIMELINE {
        let keys: Vec<String> = r.params.iter().map(|p| format!("{}:{}{}", p.key, p.kind, if p.required { "" } else { " optional" })).collect();
        let spec = if r.spec_commands.is_empty() { String::new() } else { format!(" spec {}", r.spec_commands.join(" ")) };
        out += &format!("  timeline.{} ({}){spec}\n", r.key, keys.join(", "));
    }
    for (key, why) in DRIVE_TIMELINE_NOT_DRIVEABLE {
        out += &format!("  timeline.{key}: refuses command_not_driveable, {why}\n");
    }
    for r in DRIVE_VIEWER {
        let keys: Vec<String> = r.params.iter().map(|p| format!("{}:{}{}", p.key, p.kind, if p.required { "" } else { " optional" })).collect();
        out += &format!("  viewer.{} ({})\n", r.method, keys.join(", "));
    }
    out += "  studio.* (every menu leaf id in state.shell.menu, no parameters, through the menu's own availability)\n";
    for (id, why) in DRIVE_SHELL_NOT_DRIVEABLE {
        out += &format!("  {id}: refuses command_not_driveable, {why}\n");
    }
    out += "\nDRIVE SESSION (command PACKAGE --actor ID --session ID --stdin)\n";
    for line in drive["session_lines"].as_array().into_iter().flatten() {
        out += &format!("  {}\n", line.as_str().unwrap_or(""));
    }
    for key in ["response", "open_timeline", "document_save", "parameter_kinds"] {
        out += &format!("  {key}: {}\n", drive[key].as_str().unwrap_or(""));
    }
    out += &format!("  A line over {} bytes refuses line_too_long; a refused line never ends the session.\n  Refusals: {}\n  Limits:\n", drive["line_limit_bytes"], DRIVE_REFUSALS.join(", "));
    for limit in DRIVE_LIMITS {
        out += &format!("    {limit}\n");
    }
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
    out += "\nDIAGNOSTICS\n";
    for (k, v) in m["diagnostics"].as_object().into_iter().flatten() {
        out += &format!("  {k}: {}\n", v.as_str().unwrap_or(""));
    }
    out += "\nNOT PROVEN\n";
    for n in NOT_PROVEN {
        out += &format!("  {n}\n");
    }
    out
}
