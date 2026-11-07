//! The web document host (lane SMB-WEB-HOST-1): RED. The rows exist and refuse until the host lands.
#![deny(clippy::arithmetic_side_effects)]
use super::Run;
use crate::action::{self, CommandEntry};
use serde_json::{Value, json};

pub const MANIFEST_FILE: &str = "studio-export-manifest.json";
pub const RECEIPT_FILE: &str = "studio-publish-receipt.json";

/// The web row of `command_id`.
pub fn row(command_id: &str) -> Option<&'static CommandEntry> {
    action::RUN_COMMANDS.iter().find(|c| c.run_method == "web_command" && c.command_id == command_id)
}

/// No host yet.
#[derive(Default)]
pub struct WebHost;

impl WebHost {
    pub fn revision(&self) -> u64 {
        0
    }

    pub fn state_json(&self) -> Value {
        json!({"open": false, "revision": 0})
    }
}

/// Refuses every web command (red).
pub(super) fn run(run: &mut Run<'_>, command_id: &str, _params: &Value) -> bool {
    let before = run.frames.count();
    let op = row(command_id).map_or("web_command", |r| r.command_id);
    run.own_or_push(op, before, Err("unknown_command"))
}
