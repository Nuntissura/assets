//! Automation rows of the command corpus.

use super::{CommandEntry, entry};

pub(super) const ROWS: &[CommandEntry] = &[entry("action.play", "play_action", "Play action", false, true, &[])];
