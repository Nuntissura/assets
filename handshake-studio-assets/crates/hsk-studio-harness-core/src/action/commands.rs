//! The command corpus of the harness [`crate::Run`] ([STU-AUT-147] subset, [`RUN_COMMANDS`]), one
//! file per domain so that lanes add rows without touching each other's tables. A row goes in the
//! `ROWS` of its domain's file; a file holding no row yet is ready for the lane that needs it. A new
//! domain is a new file, one `mod` line and one [`DOMAINS`] line.

use super::{UnitToken, ValueType};
use serde_json::{Value, json};

mod audio;
mod automation;
mod board;
mod catalog;
mod design;
mod document;
mod history;
mod layer;
mod layout;
mod motion;
mod panel;
mod photo;
mod proof;
mod text;
mod vector;
mod video;
mod web;

pub use layer::{CMD_PANEL_SET, CMD_RENAME, CMD_RESTRUCTURE, CMD_SET_BLEND_MODE, CMD_SET_OPACITY, CMD_SET_VISIBLE};

/// One declared parameter of a corpus command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParamSpec {
    pub key: &'static str,
    pub value_type: ValueType,
    /// The unit a `measured` parameter is recorded in.
    pub unit: Option<UnitToken>,
    /// A `Run` row's parameters are all required; a typed row says which are.
    pub required: bool,
    /// The decoded form of a typed row's parameter (the manual's parameter kinds); empty on a `Run` row.
    pub shape: &'static str,
}

impl ParamSpec {
    /// The parameter may be omitted.
    pub const fn opt(self) -> Self {
        Self { required: false, ..self }
    }
}

/// What runs a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// The public `Run` method `run_method` names.
    Run,
    /// `Viewer::run_command` and its typed decoders; `run_method` names the method it reaches.
    Viewer,
}

/// One [STU-AUT-147] corpus entry (subset: no `input_schema`/`output_schema`, schemars is not an
/// approved harness-core dependency; no capability, enabler, receipt shape, argus targets or
/// manual anchor). `dry_runnable` is false for every entry: `Run` has no dry-run mode (gap).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandEntry {
    pub command_id: &'static str,
    /// The public method the command runs through ([`Surface`]).
    pub run_method: &'static str,
    pub surface: Surface,
    pub display_name: &'static str,
    pub recordable: bool,
    pub mutates_authority: bool,
    pub dry_runnable: bool,
    pub parameters: &'static [ParamSpec],
}

const fn p(key: &'static str, value_type: ValueType) -> ParamSpec {
    ParamSpec { key, value_type, unit: None, required: true, shape: "" }
}

const fn shaped(key: &'static str, value_type: ValueType, shape: &'static str) -> ParamSpec {
    ParamSpec { key, value_type, unit: None, required: true, shape }
}

const fn text(key: &'static str) -> ParamSpec {
    shaped(key, ValueType::String, "text")
}

const fn number(key: &'static str) -> ParamSpec {
    shaped(key, ValueType::Number, "number")
}

const fn index(key: &'static str) -> ParamSpec {
    shaped(key, ValueType::Number, "integer")
}

const fn flag(key: &'static str) -> ParamSpec {
    shaped(key, ValueType::Boolean, "boolean")
}

const fn list(key: &'static str) -> ParamSpec {
    shaped(key, ValueType::List, "text_list")
}

/// A file the command reads.
const fn path(key: &'static str) -> ParamSpec {
    shaped(key, ValueType::String, "path")
}

/// A file the command writes: the drive surface refuses the opened package and an unasked overwrite.
const fn output_path(key: &'static str) -> ParamSpec {
    shaped(key, ValueType::String, OUTPUT_PATH)
}

const fn point(key: &'static str) -> ParamSpec {
    shaped(key, ValueType::List, "point")
}

const fn points(key: &'static str) -> ParamSpec {
    shaped(key, ValueType::List, "points")
}

const fn samples(key: &'static str) -> ParamSpec {
    shaped(key, ValueType::List, "samples")
}

const fn rgb8(key: &'static str) -> ParamSpec {
    shaped(key, ValueType::List, "rgb8")
}

const fn vector3(key: &'static str) -> ParamSpec {
    shaped(key, ValueType::List, "[a, b, c]")
}

/// `tokens` is the list of allowed tokens, joined with `|`.
const fn one_of(key: &'static str, tokens: &'static str) -> ParamSpec {
    shaped(key, ValueType::Enumerated, tokens)
}

/// A nested record; `shape` says what it holds.
const fn record(key: &'static str, shape: &'static str) -> ParamSpec {
    shaped(key, ValueType::Class, shape)
}

/// The shape of a parameter naming a file the command writes.
pub const OUTPUT_PATH: &str = "output_path";

const fn entry(command_id: &'static str, run_method: &'static str, display_name: &'static str, recordable: bool, mutates_authority: bool, parameters: &'static [ParamSpec]) -> CommandEntry {
    CommandEntry { command_id, run_method, surface: Surface::Run, display_name, recordable, mutates_authority, dry_runnable: false, parameters }
}

/// A row run by `Viewer::run_command` (not recordable yet: playback calls `Run` methods).
const fn typed(command_id: &'static str, method: &'static str, display_name: &'static str, mutates_authority: bool, parameters: &'static [ParamSpec]) -> CommandEntry {
    CommandEntry { command_id, run_method: method, surface: Surface::Viewer, display_name, recordable: false, mutates_authority, dry_runnable: false, parameters }
}

/// Every domain's rows, in the order of the corpus.
const DOMAINS: &[&[CommandEntry]] = &[
    layer::ROWS,
    document::ROWS,
    history::ROWS,
    automation::ROWS,
    audio::ROWS,
    board::ROWS,
    catalog::ROWS,
    design::ROWS,
    layout::ROWS,
    motion::ROWS,
    panel::ROWS,
    photo::ROWS,
    proof::ROWS,
    text::ROWS,
    vector::ROWS,
    video::ROWS,
    web::ROWS,
];

const ROW_COUNT: usize = {
    let (mut count, mut d) = (0, 0);
    while d < DOMAINS.len() {
        count += DOMAINS[d].len();
        d += 1;
    }
    count
};

const ALL_ROWS: [CommandEntry; ROW_COUNT] = {
    let mut rows = [entry("", "", "", false, false, &[]); ROW_COUNT];
    let (mut at, mut d) = (0, 0);
    while d < DOMAINS.len() {
        let mut r = 0;
        while r < DOMAINS[d].len() {
            rows[at] = DOMAINS[d][r];
            at += 1;
            r += 1;
        }
        d += 1;
    }
    rows
};

/// The automatable surface of [`crate::Run`]: every public operation, recordable or not. Recording
/// captures exactly the `recordable` ones; any other one run during a recording is reported as
/// `not_recordable` ([STU-AUT-109]: silent omission is forbidden).
pub const RUN_COMMANDS: &[CommandEntry] = &ALL_ROWS;

/// The corpus entry of `command_id`.
pub fn command(command_id: &str) -> Option<&'static CommandEntry> {
    RUN_COMMANDS.iter().find(|c| c.command_id == command_id)
}

/// The corpus as JSON (manual and CLI).
pub fn corpus_json() -> Value {
    json!(RUN_COMMANDS
        .iter()
        .map(|c| json!({
            "command_id": c.command_id, "run_method": c.run_method, "surface": if c.surface == Surface::Run { "run" } else { "viewer" }, "display_name": c.display_name,
            "recordable": c.recordable, "mutates_authority": c.mutates_authority, "dry_runnable": c.dry_runnable,
            "parameters": c.parameters.iter().map(|p| json!({"key": p.key, "value_type": p.value_type.token(), "unit": p.unit.map(UnitToken::token), "required": p.required})).collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>())
}
