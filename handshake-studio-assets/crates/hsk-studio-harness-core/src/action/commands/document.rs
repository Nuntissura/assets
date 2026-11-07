//! Document rows of the command corpus.

use super::super::ValueType;
use super::{CommandEntry, ParamSpec, entry, p};

const PATH: ParamSpec = p("path", ValueType::String);
const OVERWRITE: ParamSpec = p("overwrite", ValueType::Boolean);

pub(super) const ROWS: &[CommandEntry] = &[
    entry("document.set_document_entry", "set_document_entry", "Set document entry", false, true, &[]),
    entry("document.stage_tile", "stage_tile", "Stage tile body", false, false, &[]),
    entry("document.submit_sequence", "submit_sequence", "Replace sequence", false, true, &[]),
    entry("document.submit_sequence_and_entry", "submit_sequence_and_entry", "Replace sequence with document entry", false, true, &[]),
    entry("document.submit_sequence_edits", "submit_sequence_edits", "Replace, insert or remove sequences", false, true, &[]),
    entry("document.submit_sequence_edits_and_entry", "submit_sequence_edits_and_entry", "Replace, insert or remove sequences with document entry", false, true, &[]),
    entry("document.open", "open_path / open_bytes / expect_on_disk", "Open document", false, false, &[]),
    entry("document.import", "open_import / open_import_detailed", "Open and convert to native", false, false, &[]),
    entry("document.reopen", "reopen_bytes / reopen_path", "Reopen saved document", false, false, &[]),
    entry("document.save", "save_to / save_new / save_replacing", "Save document", false, false, &[PATH, OVERWRITE]),
    entry("document.checkpoint", "checkpoint", "Checkpoint document", false, false, &[]),
    entry("document.verify", "verify_live / verify_reopened", "Verify document", false, false, &[]),
    entry("document.summary", "summary", "Document summary", false, false, &[]),
];
