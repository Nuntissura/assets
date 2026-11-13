//! Studio's typed commands by id (lane SMB-COMMAND-SURFACE-1), egui-free and drawing nothing: a row of
//! `action::RUN_COMMANDS` with surface [`Surface::Viewer`] plus its decoder here. [`Viewer::run_command`]
//! is the one call: the process drive surface makes it, and a panel will make it too, naming a command
//! id and never a private method. Parameters are JSON; a missing, undeclared or undecodable one is a
//! typed refusal before anything runs. Attribution is the viewer's run context, the caller's. A command
//! that reaches no `Run` operation still ends in one terminal observe frame.
//!
//! A new command: its row in `hsk-studio-harness-core/src/action/commands/<domain>.rs`, its arm in the
//! decoder of that domain here, and a JSONL test through the process. A domain whose engine is behind a
//! feature answers `feature_unavailable` in a build without it.
#![deny(clippy::arithmetic_side_effects)]

mod audio;
mod component;
mod decode;
mod design;
mod export;
mod layout;
mod layout_flow;
mod mixer;
mod monitor;
mod panel;
mod photo;
mod place;
mod prototype;
mod seed;
mod variable;
mod vector;
mod waveform;

use super::Viewer;
use decode::Fields;
use hsk_studio_harness_core::action::{self, CommandEntry, Surface};
use serde_json::{Map, Value, json};
use std::path::Path;

/// A command refused: a stable code and a JSON detail, never a free-form error alone.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandRefusal {
    pub code: String,
    pub detail: Value,
}

impl CommandRefusal {
    pub(crate) fn new(code: &str, detail: Value) -> Self {
        Self { code: code.to_string(), detail }
    }
}

/// A file a command reads whole: its size is checked before any byte is held.
/// Only a regular file, and never more than `limit` bytes held, whatever the metadata says.
pub(super) fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, CommandRefusal> {
    use std::io::Read;
    let unreadable = |e: std::io::Error| CommandRefusal::new("resource_unreadable", json!({"path": path.to_string_lossy(), "kind": format!("{:?}", e.kind())}));
    let too_large = || CommandRefusal::new("resource_too_large", json!({"path": path.to_string_lossy(), "limit_bytes": limit}));
    let meta = std::fs::metadata(path).map_err(unreadable)?;
    if !meta.is_file() {
        return Err(CommandRefusal::new("resource_unreadable", json!({"path": path.to_string_lossy(), "kind": "not_a_regular_file"})));
    }
    if meta.len() > limit {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path).map_err(unreadable)?.take(limit.saturating_add(1)).read_to_end(&mut bytes).map_err(unreadable)?;
    if u64::try_from(bytes.len()).map_or(true, |n| n > limit) {
        return Err(too_large());
    }
    Ok(bytes)
}

/// A method's refusal text as a stable code: the run's newest record's when the method reached the run.
fn method_refusal(viewer: &Viewer, before: usize, message: String) -> CommandRefusal {
    let code = viewer.run().ops().get(before..).and_then(|new| new.iter().rev().find_map(|op| op["error"].as_str().map(str::to_string)));
    CommandRefusal { code: code.unwrap_or_else(|| "command_refused".to_string()), detail: json!({"message": message}) }
}

/// An engine's refusal text as a stable code (lane SMB-DRIVE-TYPED-2): the run's newest record's when
/// the method reached the run, else the text's own code (`lane_unavailable track_volume`,
/// `mixer: rate_off_tick_grid`, `fig_import: decode refused not_fig`), else `command_refused`; the
/// text stays in `detail.message`.
fn engine_refusal(viewer: &Viewer, before: usize, message: String) -> CommandRefusal {
    let mut refusal = method_refusal(viewer, before, message);
    if refusal.code == "command_refused"
        && let Some(code) = refusal.detail["message"].as_str().and_then(leading_code)
    {
        refusal.code = code;
    }
    refusal
}

/// Engine prefixes holding an underscore: the text after them is the message, not a code
/// (`fig_import: ...`); any other `<snake_case>: ` prefix is itself the code.
const ENGINE_PREFIXES: &[&str] = &["fig_import"];

/// The snake_case code a message names, after at most one `<engine>: ` prefix: its leading token, or
/// the token after `refused ` (`fig_import: decode refused not_fig`). A leading token followed by
/// ` refused` names the op that was refused (`mixer: mixer_master_volume refused: ...`), not a reason:
/// `None` (review F1 on 2bbe2207).
pub(in crate::view) fn leading_code(message: &str) -> Option<String> {
    let rest = match message.split_once(": ") {
        Some((engine, rest)) if engine.bytes().all(|b| b.is_ascii_lowercase()) || ENGINE_PREFIXES.contains(&engine) => rest,
        _ => message,
    };
    let coded = |token: &str| token.len() <= 64 && token.contains('_') && token.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    let token = rest.split([' ', ':', '(']).next()?;
    // The token ends at an ASCII delimiter or the end, so the slice after it is on a char boundary.
    if rest.get(token.len()..).is_some_and(|after| after.starts_with(" refused")) {
        return None;
    }
    if coded(token) {
        return Some(token.to_string());
    }
    let (_, after) = rest.split_once("refused ")?;
    let code = after.split([' ', ':', '(', ')']).next()?;
    coded(code).then(|| code.to_string())
}

/// Every given key is declared, every required key is given.
fn check_declared(row: &CommandEntry, params: &Map<String, Value>) -> Result<(), CommandRefusal> {
    if let Some(key) = params.keys().find(|k| !row.parameters.iter().any(|p| p.key == k.as_str())) {
        return Err(CommandRefusal::new("parameter_unknown", json!({"key": key})));
    }
    if let Some(p) = row.parameters.iter().find(|p| p.required && !params.get(p.key).is_some_and(|v| !v.is_null())) {
        return Err(decode::missing(p.key));
    }
    Ok(())
}

impl Viewer {
    /// Runs one typed command by id. The refusal names why; on success the value is the command's own
    /// result. A panic is the caller's to guard, as with every Viewer method.
    pub fn run_command(&mut self, command_id: &str, params: &Map<String, Value>) -> Result<Value, CommandRefusal> {
        let Some(row) = action::command(command_id).filter(|r| r.surface == Surface::Viewer) else {
            return Err(CommandRefusal::new("unknown_command", json!({"command_id": command_id})));
        };
        let before = self.run().ops().len();
        let result = check_declared(row, params).and_then(|()| self.run_row(row, Fields(params), before));
        if self.run().ops().len() == before {
            let outcome = match &result {
                Ok(_) => Ok(Value::Null),
                Err(_) => Err("command_refused"),
            };
            self.run.command_outcome(row.command_id, outcome);
        }
        result
    }

    fn run_row(&mut self, row: &CommandEntry, f: Fields<'_>, before: usize) -> Result<Value, CommandRefusal> {
        let id = row.command_id;
        let done = match id.split('.').next().unwrap_or_default() {
            "catalog" => return seed::run(self, id, f),
            "asset" => return place::run_asset(self, id, f),
            "transform" => return place::run_transform(self, id, f),
            "audio" => return audio::run(self, id, f),
            // Lane SMB-DRIVE-TYPED-2: audio post, the monitors and product design.
            "mixer" | "multitrack" => return mixer::run(self, id, f, before),
            "waveform" | "spectral" => return waveform::run(self, id, f, before),
            "monitor" => return monitor::run(self, id, f),
            "prototype" => return prototype::run(self, id, f),
            "design" if design::owns(id) => return design::run(self, id, f, before),
            // Lane SMB-DS-COMPONENTS-AUTHOR-1: component, variant set, property and instance authoring.
            "component" | "component_set" | "instance" => return component::run(self, id, f, before),
            // Lane SMB-LAYOUT-DELETE-CASCADE-1: autoflow and smart text reflow (rows of action/commands/layout_flow.rs).
            "layout" if layout_flow::owns(id) => return layout_flow::run(self, id, f),
            // Lane SMB-DS-VARIABLES-AUTHOR-1: design variables and explicit modes ([STU-DS-132], [STU-DS-133]).
            "variable" | "variable_collection" | "node" => return variable::run(self, id, f, before),
            "layout" => return layout::run(self, id, f),
            "panel" => return panel::run(self, id, f),
            "vector" => vector::run_vector(self, id, f)?,
            "proof" => vector::run_proof(self, id, f)?,
            "design" => vector::run_design(self, id, f)?,
            "photo" => photo::run(self, id, f)?,
            // Lane SMB-PS-FILTER-ADJUST-COMMANDS-1: smart filter effects and adjustment layers (`view::raster_effects`).
            "effect" => return self.effect_command(id, f.0),
            "adjustment" => return self.adjustment_command(id, f.0),
            "export" | "delivery" => return export::run(self, id, f),
            _ => return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": id}))),
        };
        done.map_err(|message| method_refusal(self, before, message))
    }
}
