//! Standalone asset desktop/headless development consumers with explicit selected providers; no
//! unconditional all-domain facade, no separately shipped suite app.
//!
//! Slice 1 is HEADLESS ONLY: a desktop window needs eframe/winit, which the dependency probe
//! forbids (lead decision pending). The library drives the real session (open, revision-checked
//! provider submit, atomic save, reopen) and the controls descriptors; the `studio-harness`
//! example is a thin forwarder to [`cli::run`].
#![forbid(unsafe_code)]

pub mod cli;
mod harness;
pub mod manual;
pub mod panel;

pub use harness::{
    AVAILABLE_PROVIDERS, Harness, HarnessError, NOT_PROVEN, References, Run, SCHEMA, Selection, default_actor,
};

/// Machine-readable crate descriptor (same convention as the other Studio crates).
pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-HARNESS","version":1,"slice":1,"mode":"headless_only","desktop_window":"not_provided (eframe/winit are probe-forbidden; lead decision pending)","api":"Harness::new(Selection) -> Run: open_path/open_bytes, summary, panel, rename, set_visible, panel_set, save_to, checkpoint, reopen_path/reopen_bytes, verify_reopened/verify_live, finish; cli::run(args); manual::manual_json/manual_text; Harness::collect_descriptors","consumer":"studio-harness help | descriptors | providers | open PACKAGE | panel PACKAGE --layer ID | edit PACKAGE --provider chronicle.patch (--rename LAYER NAME | --visible LAYER BOOL | --set LAYER CONTROL VALUE)... [--save OUT [--overwrite]] | verify PACKAGE (--expect-revision N | --expect-name LAYER NAME | --expect-visible LAYER BOOL)...","input":"a .handshake package file, an explicit provider selection (nothing is selected by default), layer ids from the summary","output":"one JSON document per command (schema hsk.studio.harness@1): per-operation records with revision, one Observe frame count and outcome, detail or stable error code; verdict pass only if at least one operation ran and every operation succeeded (fail when one did not, nothing_checked when none ran); verify without any expectation and edit --save naming the input package without --overwrite are usage errors (exit 2)","selection":{"providers_available":["chronicle.patch"],"default":"none","unknown_provider":"refused before any step","references":"unresolved (default) | trust_all (development stand-in, recorded)"},"controls":"panel for a layer is built only from hsk-studio-controls descriptors; name and visible commit through chronicle, appearance values are read-only projections (no chronicle property)","observe":"one terminal frame per operation on an addressed document (an operation before the document is identified reports unaddressed with 0 frames): session operations through the session report, harness operations through a harness-emitted frame that sets no private text","quiet":"this crate's sources use no window, process, network or environment API (source scan); file paths are exactly the ones given on the command line (use the artifact root)","proven_by":{"verdict_needs_evidence":"tests/harness_headless.rs verify_checks_something_and_save_never_clobbers_the_input, an_empty_run_is_nothing_checked_not_a_pass","nothing_selected_by_default":"refusals_are_typed_and_nothing_is_committed","one_frame_per_operation":"open_edit_save_reopen_verify_end_to_end, refusals_are_typed_and_nothing_is_committed","quiet_sources":"quiet_sources_use_no_process_network_or_environment_api","given_paths_only":"files_are_saved_atomically_and_failed_edits_keep_the_last_good_file"},"not_proven":["visual_render_proof","appearance_commit","desktop_window_consumer"]}"#;
