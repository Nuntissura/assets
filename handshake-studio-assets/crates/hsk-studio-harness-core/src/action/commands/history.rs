//! History rows of the command corpus.

use super::{CommandEntry, entry};

pub(super) const ROWS: &[CommandEntry] = &[
    // Lane SMB-DS-VARIABLES-AUTHOR-1 (review F5): a rollback is an undo that leaves no redo entry.
    entry("history.undo", "undo / rollback_newest_step", "Undo", false, true, &[]),
    entry("history.redo", "redo", "Redo", false, true, &[]),
];
