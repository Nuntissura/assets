//! Automation rows of the command corpus.

use super::{CommandEntry, entry};

pub(super) const ROWS: &[CommandEntry] = &[
    entry("action.play", "play_action", "Play action", false, true, &[]),
    // The terminal frame of a typed command that reached no other `Run` operation.
    entry("command.outcome", "command_outcome", "Report a command outcome", false, false, &[]),
];
