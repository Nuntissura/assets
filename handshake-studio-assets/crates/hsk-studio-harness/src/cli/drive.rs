//! The process drive surface (lane SMB-HARNESS-DRIVE-1, feature `viewer`; [STU-CON-007] (a) and
//! [STU-MDL-006], first slice): `state`, `command`, `undo` and the `--stdin` JSONL session, over the
//! viewer's non-draw methods. No second command catalog: a command id is a `RUN_COMMANDS` row (a typed
//! row runs `Viewer::run_command`), `timeline.<TimelineCommand::key()>`, `<domain>.<Viewer method>` or
//! its old form `viewer.<Viewer method>` (module `drive_viewer`) or a menu leaf `studio.*`
//! (`ShellCommand::id`), and the manual's drive tables say which of them this slice drives. Every response carries the revision, `Viewer::state_json` and the resolved attribution;
//! a mutating command never runs without the caller's `--actor` and `--session`.
//!
//! This module draws nothing and never opens a window (tests/harness_drive.rs
//! a_live_session_owns_no_window, drive_sources_name_no_window_focus_or_os_input); the viewer renders
//! on the CPU path. Saves go through `Viewer::save`, the one path every save takes (`Run::save_to`).
#![deny(clippy::arithmetic_side_effects)]

use super::{CliOutput, Parsed, out, same_file, usage};
use crate::RunContext;
use crate::view::shell::ShellCommand;
use crate::view::timeline::TimelineCommand;
use crate::view::{Viewer, render::Prefer};
use hsk_studio_accord::{ActorContext, ClientSessionId, Ticks, TransientCorrelation};
use hsk_studio_harness_core::action::{self, Surface, ValueType};
use hsk_studio_harness_core::harness::{motion_commands, web_commands};
use crate::view::menu_tree::{self, CatalogRefusal, MenuContext};
use hsk_studio_harness_core::manual::{DRIVE_CATALOG_ROUTES, DRIVE_RUN_COMMANDS, DRIVE_SHELL_NOT_DRIVEABLE, DRIVE_TIMELINE, DRIVE_TIMELINE_NOT_DRIVEABLE, DRIVE_VIEWER, DriveTimeline, viewer_command_id, viewer_row_of};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The longest session line in bytes; past it the line is refused and skipped, and the bytes past
/// the limit are never held in memory.
pub(super) const MAX_LINE_BYTES: usize = 1_048_576;

pub(super) struct Refusal {
    code: String,
    detail: Value,
}

pub(super) fn refuse(code: &str, detail: Value) -> Refusal {
    Refusal { code: code.to_string(), detail }
}

/// Parameter values as text, whatever JSON they arrived as (a list or object as its JSON text), so a
/// `--param` and a session line decode alike.
pub(super) type Params = BTreeMap<String, String>;

/// One command the session reads from a line, or a one-shot's arguments.
enum Request {
    State,
    Undo { expected: Option<u64> },
    Command { id: String, params: Params, expected: Option<u64> },
}

struct Live {
    viewer: Viewer,
    source: PathBuf,
}

/// The arguments every drive command needs: one PACKAGE, `--actor` and `--session`.
fn package_of(parsed: &Parsed) -> Result<PathBuf, CliOutput> {
    let [package] = parsed.positional.as_slice() else {
        return Err(usage("this command needs exactly one PACKAGE path"));
    };
    if parsed.actor.is_none() || parsed.session.is_none() {
        return Err(usage("--actor ID and --session ID are required; there is no default actor"));
    }
    Ok(PathBuf::from(package))
}

/// `--actor` is account and principal; `--session` is the session and the client session; `--owner`
/// (owner account and principal) and `--access-space` default to the actor, a stated limit: nothing
/// here proves a grant.
fn context_of(parsed: &Parsed) -> Result<RunContext, Refusal> {
    let invalid = || refuse("actor_invalid", json!({"limit":"1..=128 characters, no control character"}));
    let (Some(actor), Some(session)) = (parsed.actor.as_deref(), parsed.session.as_deref()) else {
        return Err(invalid());
    };
    let owner = parsed.owner.as_deref().unwrap_or(actor);
    let space = parsed.access_space.as_deref().unwrap_or(actor);
    let actor = ActorContext::new(actor, actor, owner, owner, space, session).map_err(|_| invalid())?;
    let client_session = ClientSessionId::parse(session).map_err(|_| invalid())?;
    Ok(RunContext::new(actor, client_session, Arc::new(TransientCorrelation::new())))
}

fn attribution_json(context: &RunContext) -> Value {
    let a = context.actor();
    json!({
        "actor": {
            "account_id": a.account_id(), "principal_id": a.principal_id(), "owner_account_id": a.owner_account_id(),
            "owner_principal_id": a.owner_principal_id(), "access_space_id": a.access_space_id(), "session_id": a.session_id(),
        },
        "client_session": context.client_session().as_str(),
    })
}

impl Live {
    fn new(context: RunContext, source: PathBuf) -> Self {
        Self { viewer: Viewer::with_context(Prefer::Cpu, hsk_studio_render_gpu::DeviceOptions::default(), context), source }
    }

    fn open(&mut self) -> Result<Value, Refusal> {
        let before = self.viewer.run().ops().len();
        let source = self.source.clone();
        self.viewer.open(&source).map_err(|message| self.failure(before, "open_failed", message))?;
        Ok(json!({"source": source.to_string_lossy()}))
    }

    fn revision(&self) -> Option<u64> {
        self.viewer.run().snapshot().map(|s| s.document().revision)
    }

    /// A refused viewer method's stable code: the run's newest record's when the method reached the
    /// run, otherwise `fallback`.
    fn failure(&self, ops_before: usize, fallback: &str, message: String) -> Refusal {
        let code = self.viewer.run().ops().get(ops_before..).and_then(|new| new.iter().rev().find_map(|op| op["error"].as_str().map(str::to_string)));
        Refusal { code: code.unwrap_or_else(|| fallback.to_string()), detail: json!({"message": message}) }
    }

    /// One response: the outcome over the state after it. The frame is rendered on the CPU path
    /// first, so `state.frame` belongs to the revision reported.
    fn reply(&mut self, command: &str, command_id: Option<&str>, line: Option<u64>, result: Result<Value, Refusal>) -> Value {
        self.viewer.render_if_dirty();
        let state = self.viewer.state_json();
        let mut reply = json!({
            "schema": crate::SCHEMA, "command": command, "ok": result.is_ok(), "verdict": if result.is_ok() { "pass" } else { "fail" },
            "revision": state["revision"], "attribution": attribution_json(self.viewer.run().context()),
        });
        if let Some(id) = command_id {
            reply["command_id"] = json!(id);
        }
        if let Some(n) = line {
            reply["line"] = json!(n);
        }
        match result {
            Ok(detail) => reply["detail"] = detail,
            Err(r) => {
                reply["error"] = json!(r.code);
                reply["detail"] = r.detail;
            }
        }
        reply["state"] = state;
        reply
    }

    /// Runs one command id. Fail closed: an unknown id, a row this slice cannot drive, an undeclared,
    /// missing or mistyped parameter and a stale revision all refuse before anything runs.
    fn dispatch(&mut self, id: &str, params: &Params, expected: Option<u64>) -> Result<Value, Refusal> {
        if let Some((_, method)) = DRIVE_CATALOG_ROUTES.iter().find(|(catalog, _)| *catalog == id) {
            return self.viewer_method(method, params, expected);
        }
        if let Some(key) = id.strip_prefix("timeline.") {
            return self.timeline(key, params, expected);
        }
        if let Some(method) = id.strip_prefix("viewer.") {
            return self.viewer_method(method, params, expected);
        }
        if id.starts_with("studio.") {
            return self.shell(id, params, expected);
        }
        if let Some(row) = motion_commands::row(id) {
            return self.motion(row, params, expected);
        }
        if let Some(row) = viewer_row_of(id).filter(|_| action::command(id).is_none()) {
            return self.viewer_method(row.method, params, expected);
        }
        if let Some(row) = web_commands::row(id) {
            return self.web(row, params, expected);
        }
        let Some(entry) = action::command(id) else {
            return Err(refuse("unknown_command", json!({"command_id": id, "hint": "`help --json` lists drive.commands, drive.typed, drive.viewer and drive.timeline"})));
        };
        if entry.surface == Surface::Viewer {
            return self.typed(entry, params, expected);
        }
        if !DRIVE_RUN_COMMANDS.contains(&id) {
            return Err(refuse("command_not_driveable", json!({"command_id": id, "reason": "needs a payload this process surface does not carry yet"})));
        }
        let mut params = params.clone();
        if id == "document.save" {
            // The one optional parameter of a driveable row: a save never overwrites by default.
            params.entry("overwrite".to_string()).or_insert_with(|| "false".to_string());
        }
        check_declared(entry.parameters.iter().map(|p| (p.key, p.value_type)), &params)?;
        self.check_revision(expected)?;
        let text = |key: &str| params.get(key).map_or("", String::as_str);
        let flag = |key: &str| text(key) == "true";
        let number = |key: &str| text(key).parse::<f64>().unwrap_or(f64::NAN);
        let before = self.viewer.run().ops().len();
        let result = match id {
            "layer.rename" => self.viewer.rename_layer(text("layer"), text("name")),
            // The menus' shared executor (lane SMB-GUI-MENU-1): one code path for the leaf and this row.
            "layer.set_visible" => match menu_tree::run_catalog(&mut self.viewer, id, &params) {
                Some(Ok(r)) => r.map(|v| v.as_u64().unwrap_or_default()),
                Some(Err(e)) => return Err(catalog_refusal(e)),
                None => return Err(refuse("command_not_driveable", json!({"command_id": id}))),
            },
            "layer.set_opacity" => self.viewer.set_opacity(text("layer"), number("opacity")),
            "layer.set_blend_mode" => match text("blend_mode").parse::<u16>() {
                Ok(mode) if mode >= 1 => self.viewer.set_blend_mode(text("layer"), mode),
                _ => return Err(refuse("parameter_invalid", json!({"key": "blend_mode", "expected": "integer in 1..=65535"}))),
            },
            "layer.panel_set" => self.viewer.panel_set(text("layer"), text("control"), text("value")),
            "history.undo" => self.viewer.undo(),
            "history.redo" => self.viewer.redo(),
            "document.save" => return self.save(Path::new(text("path")), flag("overwrite"), expected),
            _ => return Err(refuse("command_not_driveable", json!({"command_id": id}))),
        };
        match result {
            Ok(revision) => Ok(json!({"revision": revision})),
            Err(message) => Err(self.failure(before, "command_refused", message)),
        }
    }

    /// One typed motion command (lane SMB-MOTION-COMMANDS-1): its parameters are optional per row, so
    /// `check_declared` does not apply; the run refuses a missing or mistyped one with its own code.
    fn motion(&mut self, row: &action::CommandEntry, params: &Params, expected: Option<u64>) -> Result<Value, Refusal> {
        let value = motion_commands::params_from_text(row, params).map_err(|(code, key)| refuse(code, json!({"key": key})))?;
        self.check_revision(expected)?;
        let before = self.viewer.run().ops().len();
        match self.viewer.motion_command(row.command_id, &value) {
            Ok(revision) => {
                let detail = self.viewer.run().ops().last().map_or(Value::Null, |op| op["detail"].clone());
                Ok(json!({"revision": revision, "motion": detail}))
            }
            Err(message) => {
                let mut refusal = self.failure(before, "command_refused", message);
                refusal.detail["target"] = self.viewer.run().ops().last().map_or(Value::Null, |op| op["target"].clone());
                Err(refusal)
            }
        }
    }

    /// One web command (lane SMB-WEB-HOST-1): `expected` is the web source revision; a save never replaces the
    /// opened package.
    fn web(&mut self, row: &action::CommandEntry, params: &Params, expected: Option<u64>) -> Result<Value, Refusal> {
        let value = motion_commands::params_from_text(row, params).map_err(|(code, key)| refuse(code, json!({"key": key})))?;
        let current = self.viewer.web_json()["revision"].as_u64();
        if let Some(n) = expected
            && current != Some(n)
        {
            return Err(refuse("stale_revision", json!({"expected": n, "current": current, "revision_of": "web"})));
        }
        if row.command_id == "web.save"
            && let Some(path) = params.get("path").map(Path::new)
            && same_file(path, &self.source)
        {
            return Err(refuse("export_over_source", json!({"path": path.to_string_lossy()})));
        }
        match self.viewer.web_command(row.command_id, &value) {
            Ok(detail) => Ok(detail),
            Err(code) => {
                let record = self.viewer.run().ops().last().cloned().unwrap_or(Value::Null);
                Err(refuse(&code, json!({"target": record["target"], "detail": record["detail"]})))
            }
        }
    }

    fn check_revision(&self, expected: Option<u64>) -> Result<(), Refusal> {
        match expected {
            Some(n) if self.revision() != Some(n) => Err(refuse("stale_revision", json!({"expected": n, "current": self.revision()}))),
            _ => Ok(()),
        }
    }

    /// An existing file is replaced only with `overwrite`; the opened source also needs the revision
    /// the caller read.
    fn save(&mut self, path: &Path, overwrite: bool, expected: Option<u64>) -> Result<Value, Refusal> {
        if !overwrite && path.exists() {
            return Err(refuse("overwrite_required", json!({"path": path.to_string_lossy()})));
        }
        if same_file(path, &self.source) && expected.is_none() {
            return Err(refuse("expect_revision_required", json!({"path": path.to_string_lossy()})));
        }
        let before = self.viewer.run().ops().len();
        match self.viewer.save(path, overwrite) {
            Ok(()) => Ok(json!({"saved": path.to_string_lossy(), "revision": self.revision()})),
            Err(message) => Err(self.failure(before, "save_refused", message)),
        }
    }

    /// A typed row: `Viewer::run_command`, the call a panel makes. A file it writes is guarded as an
    /// export is. The reply names the undo step the command made.
    fn typed(&mut self, entry: &action::CommandEntry, params: &Params, expected: Option<u64>) -> Result<Value, Refusal> {
        self.check_revision(expected)?;
        for declared in entry.parameters.iter().filter(|p| p.shape == action::OUTPUT_PATH) {
            let Some(path) = params.get(declared.key).map(Path::new) else { continue };
            if same_file(path, &self.source) {
                return Err(refuse("export_over_source", json!({"path": path.to_string_lossy()})));
            }
            if params.get("overwrite").map(String::as_str) != Some("true") && path.exists() {
                return Err(refuse("overwrite_required", json!({"path": path.to_string_lossy()})));
            }
        }
        let map: serde_json::Map<String, Value> = params.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
        // The step this command made, if any: the newest step's correlation changes only then (a count
        // can stay level under the history cap).
        let newest = |viewer: &Viewer| viewer.run().history()["undo_steps"].as_array().and_then(|s| s.last()).map(|s| s["correlation"].clone());
        let step_before = newest(&self.viewer);
        let result = self.viewer.run_command(entry.command_id, &map).map_err(|r| Refusal { code: r.code, detail: r.detail })?;
        if matches!(entry.command_id, "panel.vector_import.import_vector" | "design.import_fig")
            && let Some(dest) = params.get("dest")
        {
            // The viewer now holds the imported package: saves and exports guard that one.
            self.source = PathBuf::from(dest);
        }
        // A layout row's result names its step when it has one; a row whose result carries no such key
        // (layout.open) is read like every other row (lane SMB-R60-FOLLOWUPS-1, review F1 of c43f2d90).
        let undo_name = if entry.command_id.starts_with("layout.") && result.get("undo_name").is_some() {
            result["undo_name"].clone()
        } else if newest(&self.viewer) != step_before {
            self.viewer.run().history()["undo"].as_array().and_then(|steps| steps.last()).cloned().unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        Ok(json!({"result": result, "revision": self.revision(), "undo_name": if entry.mutates_authority { undo_name } else { Value::Null }}))
    }

    fn viewer_method(&mut self, method: &str, params: &Params, expected: Option<u64>) -> Result<Value, Refusal> {
        let Some(row) = DRIVE_VIEWER.iter().find(|r| r.method == method) else {
            return Err(refuse("unknown_command", json!({"command_id": format!("viewer.{method}"), "hint": "`help --json` lists drive.viewer"})));
        };
        self.check_revision(expected)?;
        if matches!(method, "export" | "export_document" | "export_vector_svg" | "export_vector_pdf" | "export_layout_pdf" | "export_layout_idml")
            && let Some(path) = params.get("path").map(Path::new)
        {
            // An export replaces bytes with no Run op: never the opened package, whatever the flags.
            if same_file(path, &self.source) {
                return Err(refuse("export_over_source", json!({"path": path.to_string_lossy()})));
            }
            if params.get("overwrite").map(String::as_str) != Some("true") && path.exists() {
                return Err(refuse("overwrite_required", json!({"path": path.to_string_lossy()})));
            }
        }
        let before = self.viewer.run().ops().len();
        // The menus' shared executor runs its rows (lane SMB-GUI-MENU-1): a menu leaf and this row are one code path.
        let outcome = match menu_tree::run_catalog(&mut self.viewer, &viewer_command_id(method), params) {
            Some(r) => r.map_err(catalog_refusal)?,
            None => super::drive_viewer::call(&mut self.viewer, row, params)?,
        };
        match outcome {
            Ok(result) => Ok(json!({"result": result, "revision": self.revision()})),
            Err(message) => Err(self.failure(before, "command_refused", message)),
        }
    }

    /// A menu leaf: its availability gate and its route are the window's own (`Viewer::shell_command`).
    fn shell(&mut self, id: &str, params: &Params, expected: Option<u64>) -> Result<Value, Refusal> {
        if let Some((_, reason)) = DRIVE_SHELL_NOT_DRIVEABLE.iter().find(|(leaf, _)| *leaf == id) {
            return Err(refuse("command_not_driveable", json!({"command_id": id, "reason": reason})));
        }
        let Some(command) = ShellCommand::from_id(id) else {
            if id.is_empty() {
                return self.menu_leaf(id, params, expected);
            }
            return Err(refuse("unknown_command", json!({"command_id": id, "hint": "state.shell.menu lists the leaves"})));
        };
        if let Some(key) = params.keys().next() {
            return Err(refuse("parameter_unknown", json!({"key": key})));
        }
        self.check_revision(expected)?;
        let before = self.viewer.run().ops().len();
        match self.viewer.shell_command(command) {
            Ok(()) => Ok(json!({"revision": self.revision()})),
            Err(message) => Err(self.failure(before, "command_refused", message)),
        }
    }

    /// A designed menu leaf that is not a shell command (lane SMB-GUI-MENU-1): the one evaluation the native menu
    /// renders (`Viewer::studio_menu`, every shell command shown) gates it, and it runs through the shared executor.
    /// A greyed leaf answers its reason code with its availability record as the detail.
    fn menu_leaf(&mut self, id: &str, params: &Params, expected: Option<u64>) -> Result<Value, Refusal> {
        if !menu_tree::template().meta.contains_key(id) {
            return Err(refuse("unknown_command", json!({"command_id": id, "hint": "state.shell.menu lists the leaves"})));
        }
        if let Some(key) = params.keys().next() {
            return Err(refuse("parameter_unknown", json!({"key": key})));
        }
        self.check_revision(expected)?;
        match self.viewer.run_menu_leaf(&MenuContext::everything(), id) {
            Ok(run) => Ok(json!({"revision": self.revision(), "command": run.command, "params": run.params, "result": run.result, "undo_name": run.undo_name})),
            Err(r) => Err(Refusal { code: r.code, detail: json!({"command": r.command, "availability": r.availability.map(|a| a.json()), "detail": r.detail}) }),
        }
    }

    fn timeline(&mut self, key: &str, params: &Params, expected: Option<u64>) -> Result<Value, Refusal> {
        let Some(row) = DRIVE_TIMELINE.iter().find(|r| r.key == key) else {
            let id = format!("timeline.{key}");
            return Err(match DRIVE_TIMELINE_NOT_DRIVEABLE.iter().find(|(k, _)| *k == key) {
                Some((_, reason)) => refuse("command_not_driveable", json!({"command_id": id, "reason": reason})),
                None => refuse("unknown_command", json!({"command_id": id, "hint": "`help --json` lists drive.timeline"})),
            });
        };
        let call = timeline_call(row, params)?;
        self.check_revision(expected)?;
        if self.viewer.timeline_mut().is_none() {
            self.viewer.open_timeline().map_err(|message| refuse("no_timeline", json!({"message": message})))?;
        }
        let before = self.viewer.run().ops().len();
        match self.viewer.timeline_command(call) {
            Ok(()) => Ok(json!({"revision": self.revision()})),
            Err(message) => Err(self.failure(before, "timeline_refused", message)),
        }
    }
}

/// A parameter refusal of the menus' shared executor, as the drive surface's refusal.
fn catalog_refusal(e: CatalogRefusal) -> Refusal {
    refuse(e.code, e.detail)
}

/// Every given key is declared, every declared key is given and each value has the declared type.
fn check_declared<'a>(declared: impl Iterator<Item = (&'a str, ValueType)>, params: &Params) -> Result<(), Refusal> {
    let declared: Vec<(&str, ValueType)> = declared.collect();
    if let Some(key) = params.keys().find(|k| !declared.iter().any(|(d, _)| d == k)) {
        return Err(refuse("parameter_unknown", json!({"key": key})));
    }
    for (key, value_type) in declared {
        let Some(value) = params.get(key) else {
            return Err(refuse("parameter_missing", json!({"key": key})));
        };
        let fits = match value_type {
            ValueType::Boolean => value == "true" || value == "false",
            ValueType::Number | ValueType::Measured => value.parse::<f64>().is_ok_and(f64::is_finite),
            ValueType::Reference | ValueType::String => true,
            _ => false,
        };
        if !fits {
            return Err(refuse("parameter_invalid", json!({"key": key, "expected": value_type.token()})));
        }
    }
    Ok(())
}

/// The typed timeline command of one manual row, from text parameters.
fn timeline_call(row: &DriveTimeline, params: &Params) -> Result<TimelineCommand, Refusal> {
    if let Some(key) = params.keys().find(|k| !row.params.iter().any(|p| p.key == *k)) {
        return Err(refuse("parameter_unknown", json!({"key": key})));
    }
    if let Some(p) = row.params.iter().find(|p| p.required && !params.contains_key(p.key)) {
        return Err(refuse("parameter_missing", json!({"key": p.key})));
    }
    let optional = |key: &str| params.get(key).cloned();
    let text = |key: &str| params.get(key).cloned().ok_or_else(|| refuse("parameter_missing", json!({"key": key})));
    fn decode<T: std::str::FromStr>(params: &Params, key: &str, expected: &str) -> Result<T, Refusal> {
        let value = params.get(key).ok_or_else(|| refuse("parameter_missing", json!({"key": key})))?;
        value.parse().map_err(|_| refuse("parameter_invalid", json!({"key": key, "expected": expected})))
    }
    let ticks = |key: &str| decode::<u64>(params, key, "ticks").map(Ticks::new);
    let frames = |key: &str| decode::<i64>(params, key, "frames");
    let flag = |key: &str| decode::<bool>(params, key, "boolean");
    // A JSON list of strings (`[]` empty) or comma-separated text.
    let list = |key: &str| optional(key).map_or_else(Vec::new, |v| match serde_json::from_str::<Vec<String>>(&v) {
        Ok(items) if v.trim_start().starts_with('[') => items,
        _ => v.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect::<Vec<_>>(),
    });
    Ok(match row.key {
        "select" => TimelineCommand::Select(optional("clip_id")),
        "move_clip" => TimelineCommand::MoveClip { clip_id: text("clip_id")?, start: ticks("start")?, track_id: optional("track_id") },
        "trim_in" => TimelineCommand::TrimIn { clip_id: text("clip_id")?, head: ticks("head")? },
        "trim_out" => TimelineCommand::TrimOut { clip_id: text("clip_id")?, tail: ticks("tail")? },
        "razor" => TimelineCommand::RazorAtPlayhead,
        "lift" => TimelineCommand::Lift,
        "ripple_delete" => TimelineCommand::RippleDelete,
        "marker_add" => TimelineCommand::AddMarker,
        "marker_next" => TimelineCommand::NextMarker,
        "marker_previous" => TimelineCommand::PreviousMarker,
        "edit_next" => TimelineCommand::NextEdit,
        "edit_previous" => TimelineCommand::PreviousEdit,
        "snap" => TimelineCommand::ToggleSnap,
        "zoom_in" => TimelineCommand::ZoomIn,
        "zoom_out" => TimelineCommand::ZoomOut,
        "zoom_fit" => TimelineCommand::ZoomToFit,
        "scroll" => TimelineCommand::ScrollTo(decode::<u64>(params, "first_frame", "frame index")?),
        "scrub" => TimelineCommand::Scrub(ticks("tick")?),
        "ripple_trim_in" => TimelineCommand::RippleTrimIn { clip_id: text("clip_id")?, delta_frames: frames("delta_frames")? },
        "ripple_trim_out" => TimelineCommand::RippleTrimOut { clip_id: text("clip_id")?, delta_frames: frames("delta_frames")? },
        "roll" => TimelineCommand::Roll { clip_id: text("clip_id")?, delta_frames: frames("delta_frames")? },
        "slip" => TimelineCommand::Slip { clip_id: text("clip_id")?, delta_frames: frames("delta_frames")? },
        "slide" => TimelineCommand::Slide { clip_id: text("clip_id")?, delta_frames: frames("delta_frames")? },
        "trim_nudge" => TimelineCommand::TrimNudge { forward: flag("forward")?, many: flag("many")? },
        "slip_nudge" => TimelineCommand::SlipNudge { right: flag("right")?, many: flag("many")? },
        "slide_nudge" => TimelineCommand::SlideNudge { right: flag("right")?, many: flag("many")? },
        "clip_nudge" => TimelineCommand::ClipNudge { right: flag("right")?, many: flag("many")? },
        "clip_track_nudge" => TimelineCommand::ClipTrackNudge { up: flag("up")? },
        "extend_edit" => TimelineCommand::ExtendEditToPlayhead,
        "multi_frame_step" => TimelineCommand::SetMultiFrameStep(decode::<u32>(params, "step", "frame count")?),
        "trim_session_enter" => TimelineCommand::TrimSessionEnter,
        "trim_session_type" => TimelineCommand::TrimSessionToggleType,
        "trim_session_commit" => TimelineCommand::TrimSessionCommit,
        "trim_session_rollback" => TimelineCommand::TrimSessionRollback,
        "composite_preview" => TimelineCommand::ToggleCompositePreview,
        "nest" => TimelineCommand::Nest(list("clips")),
        "make_subsequence" => TimelineCommand::MakeSubsequence(list("clips")),
        "open_nested" => TimelineCommand::OpenNested(optional("clip_id")),
        "back_to_parent" => TimelineCommand::BackToParent,
        other => return Err(refuse("command_not_driveable", json!({"command_id": format!("timeline.{other}")}))),
    })
}

/// `state`, `undo`, `command --command-id ...`, and `command --stdin` over no input.
pub(super) fn one_shot(command: &str, parsed: &Parsed) -> CliOutput {
    if command == "command" && parsed.stdin {
        let mut output = Vec::new();
        let exit = session(parsed, &mut std::io::empty(), &mut output);
        return CliOutput { exit, stdout: String::from_utf8_lossy(&output).into_owned() };
    }
    let package = match package_of(parsed) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let acting = parsed.command_id.is_some() || !parsed.params.is_empty() || parsed.save.is_some() || parsed.expect_revision.is_some();
    if command == "command" {
        if parsed.command_id.is_none() {
            return usage("command needs --command-id ID or --stdin");
        }
    } else if acting || parsed.stdin {
        return usage("--command-id, --param, --expect-revision, --save and --stdin belong to `command`");
    }
    if parsed.overwrite && parsed.save.is_none() {
        return usage("--overwrite goes with --save");
    }
    if let Some(save) = &parsed.save
        && same_file(save, &package)
    {
        if !parsed.overwrite {
            return usage("--save names the input PACKAGE; pass --overwrite and --expect-revision to replace it, or save to another path");
        }
        if parsed.expect_revision.is_none() {
            return usage("--save over the input PACKAGE needs --expect-revision: the revision you read");
        }
    }
    let mut params = Params::new();
    for (key, value) in &parsed.params {
        if params.insert(key.clone(), value.clone()).is_some() {
            return usage(&format!("--param {key} is given twice"));
        }
    }
    let context = match context_of(parsed) {
        Ok(c) => c,
        Err(r) => return out(1, &json!({"schema":crate::SCHEMA,"command":command,"ok":false,"verdict":"fail","error":r.code,"detail":r.detail})),
    };
    let mut live = Live::new(context, package);
    let opened = live.open();
    let command_id = parsed.command_id.as_deref();
    let result = opened.and_then(|opened| match command {
        "state" => Ok(opened),
        "undo" => Err(refuse(
            "undo_needs_live_session",
            json!({"hint": "the undo history lives in one process: run `command PACKAGE --actor ID --session ID --stdin` and send {\"command\":\"undo\"}"}),
        )),
        _ => {
            let id = command_id.unwrap_or_default();
            let done = live.dispatch(id, &params, parsed.expect_revision)?;
            match &parsed.save {
                Some(path) => live.save(path, parsed.overwrite, parsed.expect_revision).map(|saved| json!({"command": done, "save": saved})),
                None => Ok(done),
            }
        }
    });
    let exit = i32::from(result.is_err());
    out(exit, &live.reply(command, command_id, None, result))
}

/// One decoded session line.
enum Line {
    End,
    Text(String),
    TooLong,
    NotUtf8,
}

/// Reads one line of at most `limit` bytes (the newline excluded), holding no more than that: the
/// rest of a longer line is consumed and dropped.
fn read_line(input: &mut dyn BufRead, limit: usize) -> std::io::Result<Line> {
    let mut held = Vec::new();
    let (mut over, mut seen) = (false, false);
    loop {
        let available = input.fill_buf()?;
        if available.is_empty() {
            break;
        }
        seen = true;
        let newline = available.iter().position(|b| *b == b'\n');
        let part = &available[..newline.unwrap_or(available.len())];
        if !over {
            if held.len().saturating_add(part.len()) > limit {
                over = true;
                held = Vec::new();
            } else {
                held.extend_from_slice(part);
            }
        }
        let used = newline.map_or(available.len(), |n| n.saturating_add(1));
        input.consume(used);
        if newline.is_some() {
            break;
        }
    }
    Ok(if !seen {
        Line::End
    } else if over {
        Line::TooLong
    } else {
        String::from_utf8(held).map_or(Line::NotUtf8, Line::Text)
    })
}

fn malformed(message: impl Into<String>) -> Refusal {
    refuse("line_malformed", json!({"message": message.into()}))
}

/// A JSON value as parameter text: a scalar as itself, a list of strings joined with commas, any
/// other list or object as its JSON text. Null refuses.
fn parameter_text(key: &str, value: &Value) -> Result<String, Refusal> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Array(items) if !items.is_empty() && items.iter().all(Value::is_string) => Ok(items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(",")),
        Value::Array(_) | Value::Object(_) => Ok(value.to_string()),
        Value::Null => Err(refuse("parameter_invalid", json!({"key": key}))),
    }
}

/// Decodes one session line strictly; serde_json's recursion limit bounds a deep value.
fn parse_line(text: &str) -> Result<Request, Refusal> {
    let value: Value = serde_json::from_str(text).map_err(|e| malformed(e.to_string()))?;
    let Value::Object(fields) = value else {
        return Err(malformed("a line is one JSON object"));
    };
    let Some(command) = fields.get("command").and_then(Value::as_str) else {
        return Err(malformed("`command` is required"));
    };
    let allowed: &[&str] = match command {
        "state" => &["command"],
        "undo" => &["command", "expected_revision"],
        "command" => &["command", "command_id", "parameters", "expected_revision"],
        other => return Err(refuse("unknown_command", json!({"command": other, "known": ["state", "undo", "command"]}))),
    };
    if let Some(field) = fields.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(malformed(format!("`{field}` is not a field of `{command}`")));
    }
    let expected = match fields.get("expected_revision") {
        None => None,
        Some(v) => Some(v.as_u64().ok_or_else(|| malformed("`expected_revision` is an unsigned integer"))?),
    };
    Ok(match command {
        "state" => Request::State,
        "undo" => Request::Undo { expected },
        _ => {
            let Some(id) = fields.get("command_id").and_then(Value::as_str) else {
                return Err(malformed("`command_id` is required"));
            };
            let mut params = Params::new();
            match fields.get("parameters") {
                None => {}
                Some(Value::Object(given)) => {
                    for (key, value) in given {
                        params.insert(key.clone(), parameter_text(key, value)?);
                    }
                }
                Some(_) => return Err(malformed("`parameters` is an object")),
            }
            Request::Command { id: id.to_string(), params, expected }
        }
    })
}

/// Runs one line's work and answers a panic as a typed refusal, so one bad line cannot end a session.
/// What the panicking method had already changed stays changed; the reply's state shows it.
fn guarded<T>(work: impl FnOnce() -> Result<T, Refusal>) -> Result<T, Refusal> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or_else(|_| Err(refuse("command_panicked", json!({"note": "the command panicked; it may have partly applied, read state"}))))
}

/// Writes one compact JSON line and flushes it, so a caller on a pipe reads the reply at once.
fn emit(output: &mut dyn Write, value: &Value) -> bool {
    writeln!(output, "{value}").and_then(|()| output.flush()).is_ok()
}

/// The `--stdin` session: one process, one document, one history. The first line reports the open;
/// each input line gets one reply; a closed stdin ends with a summary line. Exit 0 only when nothing
/// was refused; a refused line never ends the session, a failed open or a closed stdout does.
pub(super) fn session(parsed: &Parsed, input: &mut dyn BufRead, output: &mut dyn Write) -> i32 {
    let package = match package_of(parsed) {
        Ok(p) => p,
        Err(o) => {
            let _ = output.write_all(o.stdout.as_bytes());
            return o.exit;
        }
    };
    if parsed.command_id.is_some() || !parsed.params.is_empty() || parsed.save.is_some() || parsed.expect_revision.is_some() {
        let o = usage("--stdin takes its commands from stdin: no --command-id, --param, --save or --expect-revision");
        let _ = output.write_all(o.stdout.as_bytes());
        return o.exit;
    }
    let context = match context_of(parsed) {
        Ok(c) => c,
        Err(r) => {
            let _ = emit(output, &json!({"schema":crate::SCHEMA,"command":"command","ok":false,"verdict":"fail","error":r.code,"detail":r.detail}));
            return 1;
        }
    };
    let mut live = Live::new(context, package);
    let opened = live.open();
    let open_ok = opened.is_ok();
    let reply = live.reply("open", None, None, opened);
    if !emit(output, &reply) || !open_ok {
        return 1;
    }
    let (mut lines, mut refused) = (0u64, 0u64);
    let ended = loop {
        let text = match read_line(input, MAX_LINE_BYTES) {
            Err(_) => break "stdin_error",
            Ok(Line::End) => break "stdin_closed",
            Ok(Line::Text(t)) if t.trim().is_empty() => continue,
            Ok(Line::Text(t)) => Ok(t),
            Ok(Line::TooLong) => Err(refuse("line_too_long", json!({"limit_bytes": MAX_LINE_BYTES}))),
            Ok(Line::NotUtf8) => Err(refuse("line_not_utf8", Value::Null)),
        };
        lines = lines.saturating_add(1);
        let request = text.and_then(|t| parse_line(&t));
        let (command, id, result) = match request {
            Err(r) => ("command", None, Err(r)),
            Ok(Request::State) => ("state", None, Ok(Value::Null)),
            Ok(Request::Undo { expected }) => ("undo", None, guarded(|| live.dispatch("history.undo", &Params::new(), expected))),
            Ok(Request::Command { id, params, expected }) => {
                let result = guarded(|| live.dispatch(&id, &params, expected));
                ("command", Some(id), result)
            }
        };
        if result.is_err() {
            refused = refused.saturating_add(1);
        }
        // The state itself may be what panics: the line still gets its one reply.
        let reply = guarded(|| Ok(live.reply(command, id.as_deref(), Some(lines), result))).unwrap_or_else(|r| {
            refused = refused.saturating_add(1);
            json!({"schema": crate::SCHEMA, "command": command, "line": lines, "ok": false, "verdict": "fail", "error": r.code, "detail": r.detail})
        });
        if !emit(output, &reply) {
            return 1;
        }
    };
    let end = json!({
        "schema": crate::SCHEMA, "command": "command", "ok": refused == 0 && ended == "stdin_closed", "ended": ended, "lines": lines, "refused": refused,
        "revision": live.revision(), "attribution": attribution_json(live.viewer.run().context()),
    });
    let written = emit(output, &end);
    i32::from(!written || refused > 0 || ended != "stdin_closed")
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_in_a_line_is_a_typed_refusal() {
        let r: Result<(), Refusal> = guarded(|| panic!("a bad line"));
        assert_eq!(r.err().map(|r| r.code).as_deref(), Some("command_panicked"));
        assert_eq!(guarded(|| Ok(7)).ok(), Some(7));
    }

    #[test]
    fn a_line_is_bounded_exactly_and_a_long_one_is_skipped() {
        let mut bytes = vec![b'a'; 16];
        bytes.push(b'\n');
        bytes.extend(vec![b'b'; 17]);
        bytes.push(b'\n');
        bytes.extend_from_slice(b"last");
        // A one-byte buffer: the limit holds over every chunking of the pipe.
        for capacity in [1, 7, 4096] {
            let mut input = std::io::BufReader::with_capacity(capacity, bytes.as_slice());
            assert!(matches!(read_line(&mut input, 16).unwrap(), Line::Text(t) if t == "a".repeat(16)));
            assert!(matches!(read_line(&mut input, 16).unwrap(), Line::TooLong));
            assert!(matches!(read_line(&mut input, 16).unwrap(), Line::Text(t) if t == "last"));
            assert!(matches!(read_line(&mut input, 16).unwrap(), Line::End));
        }
    }

    /// The manual's timeline table against the enum: every row builds the command of its own key
    /// with the spec ids the enum names, and the refused keys are real keys of no row.
    #[test]
    fn the_manual_timeline_rows_match_the_typed_commands() {
        for row in DRIVE_TIMELINE {
            let booleans: Vec<&str> = row.params.iter().filter(|p| p.kind == "boolean").map(|p| p.key).collect();
            let combinations = 1usize << booleans.len();
            let mut seen: Vec<&str> = Vec::new();
            for bits in 0..combinations {
                let mut params = Params::new();
                for p in row.params {
                    let value = match p.kind {
                        "boolean" => {
                            let index = booleans.iter().position(|b| *b == p.key).unwrap();
                            (bits >> index & 1 == 1).to_string()
                        }
                        "ticks" | "frames" | "count" => "1".to_string(),
                        _ => "x".to_string(),
                    };
                    params.insert(p.key.to_string(), value);
                }
                let Ok(call) = timeline_call(row, &params) else { panic!("row {} does not build", row.key) };
                assert_eq!(call.key(), row.key);
                if let Some(id) = call.spec_command() {
                    assert!(row.spec_commands.contains(&id), "{}: {id} is not listed", row.key);
                    seen.push(id);
                }
            }
            for id in row.spec_commands {
                assert!(seen.contains(id), "{}: {id} is listed but never named by the enum", row.key);
            }
        }
        for (key, _) in DRIVE_TIMELINE_NOT_DRIVEABLE {
            assert!(!DRIVE_TIMELINE.iter().any(|r| r.key == *key), "{key} is in both tables");
        }
    }
}
