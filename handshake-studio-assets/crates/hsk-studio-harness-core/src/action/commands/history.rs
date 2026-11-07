//! History rows of the command corpus.

use super::{CommandEntry, entry};

pub(super) const ROWS: &[CommandEntry] = &[
    entry("history.undo", "undo", "Undo", false, true, &[]),
    entry("history.redo", "redo", "Redo", false, true, &[]),
];
