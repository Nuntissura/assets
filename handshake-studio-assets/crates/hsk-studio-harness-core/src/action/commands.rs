//! The command corpus of the harness [`crate::Run`] ([STU-AUT-147] subset, [`RUN_COMMANDS`]), one
//! file per domain so that lanes add rows without touching each other's tables. A row goes in the
//! `ROWS` of its domain's file; a file holding no row yet is ready for the lane that needs it. A new
//! domain is a new file, one `mod` line and one [`DOMAINS`] line.

use super::{UnitToken, ValueType};
use serde_json::{Value, json};

mod audio;
mod automation;
mod board;
mod design;
mod document;
mod history;
mod layer;
mod layout;
mod motion;
mod photo;
mod text;
mod vector;
mod video;

pub use layer::{CMD_PANEL_SET, CMD_RENAME, CMD_RESTRUCTURE, CMD_SET_BLEND_MODE, CMD_SET_OPACITY, CMD_SET_VISIBLE};
pub use layout::{CMD_LAYOUT_EXPORT_IDML, CMD_LAYOUT_EXPORT_PDF, CMD_LAYOUT_OPEN, CMD_LAYOUT_PREFLIGHT, CMD_LAYOUT_RENDER_PDF_PAGE};

/// One declared parameter of a corpus command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParamSpec {
    pub key: &'static str,
    pub value_type: ValueType,
    /// The unit a `measured` parameter is recorded in.
    pub unit: Option<UnitToken>,
}

/// One [STU-AUT-147] corpus entry (subset: no `input_schema`/`output_schema`, schemars is not an
/// approved harness-core dependency; no capability, enabler, receipt shape, argus targets or
/// manual anchor). `dry_runnable` is false for every entry: `Run` has no dry-run mode (gap).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandEntry {
    pub command_id: &'static str,
    /// The public `Run` method the command runs through, or `viewer.<method>`: a row of
    /// `manual::DRIVE_VIEWER` for a command the viewer runs (lane SMB-LAYOUT-PDF-HOST-1).
    pub run_method: &'static str,
    pub display_name: &'static str,
    pub recordable: bool,
    pub mutates_authority: bool,
    pub dry_runnable: bool,
    pub parameters: &'static [ParamSpec],
}

const fn p(key: &'static str, value_type: ValueType) -> ParamSpec {
    ParamSpec { key, value_type, unit: None }
}

const fn entry(command_id: &'static str, run_method: &'static str, display_name: &'static str, recordable: bool, mutates_authority: bool, parameters: &'static [ParamSpec]) -> CommandEntry {
    CommandEntry { command_id, run_method, display_name, recordable, mutates_authority, dry_runnable: false, parameters }
}

/// Every domain's rows, in the order of the corpus.
const DOMAINS: &[&[CommandEntry]] = &[
    layer::ROWS,
    document::ROWS,
    history::ROWS,
    automation::ROWS,
    audio::ROWS,
    board::ROWS,
    design::ROWS,
    layout::ROWS,
    motion::ROWS,
    photo::ROWS,
    text::ROWS,
    vector::ROWS,
    video::ROWS,
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
            "command_id": c.command_id, "run_method": c.run_method, "display_name": c.display_name,
            "recordable": c.recordable, "mutates_authority": c.mutates_authority, "dry_runnable": c.dry_runnable,
            "parameters": c.parameters.iter().map(|p| json!({"key": p.key, "value_type": p.value_type.token(), "unit": p.unit.map(UnitToken::token)})).collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>())
}
