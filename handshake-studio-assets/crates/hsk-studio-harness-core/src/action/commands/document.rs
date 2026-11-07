//! Document rows of the command corpus.

use super::super::ValueType;
use super::{CommandEntry, ParamSpec, entry, p};

const PATH: ParamSpec = p("path", ValueType::String);
const OVERWRITE: ParamSpec = p("overwrite", ValueType::Boolean);
// Lane SMB-DOC-NEW-1: New Document, artboards and pages. Lengths are numbers in document units
// (pages: points); the drive surface runs these ids through the viewer rows of manual::DRIVE_CATALOG_ROUTES, and a
// parameter is optional here exactly where that viewer row's is (lane SMB-DRIVE-TYPED-2).
const NAME: ParamSpec = p("name", ValueType::String);
const PROFILE: ParamSpec = p("profile", ValueType::Enumerated);
const WIDTH: ParamSpec = p("width", ValueType::Number);
const HEIGHT: ParamSpec = p("height", ValueType::Number);
const UNIT: ParamSpec = p("unit", ValueType::Enumerated);
const COLOUR_MODE: ParamSpec = p("colour_mode", ValueType::Enumerated);
const BITS_PER_CHANNEL: ParamSpec = p("bits_per_channel", ValueType::Number);
const ARTBOARD: ParamSpec = p("artboard", ValueType::Reference);
const X: ParamSpec = p("x", ValueType::Number);
const Y: ParamSpec = p("y", ValueType::Number);
const PAGE: ParamSpec = p("page", ValueType::Reference);
const AFTER: ParamSpec = p("after", ValueType::Reference);
const NEW_SPREAD: ParamSpec = p("new_spread", ValueType::Boolean);
const FILE_NAME: ParamSpec = p("file_name", ValueType::String);
const RULER_ORIGIN: ParamSpec = p("ruler_origin", ValueType::List);
const RULER_PIXEL_ASPECT_RATIO: ParamSpec = p("ruler_pixel_aspect_ratio", ValueType::Number);
const SHOW_CENTER_MARK: ParamSpec = p("show_center_mark", ValueType::Boolean);
const SHOW_CROSS_HAIRS: ParamSpec = p("show_cross_hairs", ValueType::Boolean);
const SHOW_SAFE_AREAS: ParamSpec = p("show_safe_areas", ValueType::Boolean);
const FADE_REGION_OUTSIDE_ARTBOARD: ParamSpec = p("fade_region_outside_artboard", ValueType::Boolean);
const UPDATE_WHILE_DRAGGING: ParamSpec = p("update_while_dragging", ValueType::Boolean);
const BACKGROUND_FILL: ParamSpec = p("background_fill", ValueType::Enumerated);
const PRESET: ParamSpec = p("preset", ValueType::String);

pub(super) const ROWS: &[CommandEntry] = &[
    entry("document.set_document_entry", "set_document_entry", "Set document entry", false, true, &[]),
    entry("document.stage_tile", "stage_tile", "Stage tile body", false, false, &[]),
    entry("document.submit_sequence", "submit_sequence", "Replace sequence", false, true, &[]),
    entry("document.submit_sequence_and_entry", "submit_sequence_and_entry", "Replace sequence with document entry", false, true, &[]),
    entry("document.submit_sequence_edits", "submit_sequence_edits", "Replace, insert or remove sequences", false, true, &[]),
    // Lane SMB-LAYOUT-PERSIST-1: Viewer::layout_apply commits a layout editor edit through it.
    entry("document.submit_layout", "submit_layout", "Commit layout edit", false, true, &[]),
    entry("document.submit_sequence_edits_and_entry", "submit_sequence_edits_and_entry", "Replace, insert or remove sequences with document entry", false, true, &[]),
    entry("document.new", "new_document", "New document", false, true, &[PROFILE, WIDTH.opt(), HEIGHT.opt(), UNIT.opt(), COLOUR_MODE.opt(), BITS_PER_CHANNEL.opt(), NAME.opt(), FILE_NAME.opt()]),
    entry("artboard.add", "add_artboard", "New artboard", false, true, &[NAME.opt(), WIDTH, HEIGHT, X.opt(), Y.opt()]),
    entry("artboard.rename", "rename_artboard", "Rename artboard", false, true, &[ARTBOARD, NAME]),
    entry("artboard.resize", "resize_artboard", "Resize artboard", false, true, &[ARTBOARD, WIDTH, HEIGHT]),
    entry("artboard.move", "move_artboard", "Move artboard", false, true, &[ARTBOARD, X, Y]),
    entry("artboard.delete", "delete_artboard", "Delete artboard", false, true, &[ARTBOARD]),
    entry("artboard.set_options", "set_artboard_options", "Artboard options", false, true, &[
        ARTBOARD, RULER_ORIGIN.opt(), RULER_PIXEL_ASPECT_RATIO.opt(), SHOW_CENTER_MARK.opt(), SHOW_CROSS_HAIRS.opt(), SHOW_SAFE_AREAS.opt(), FADE_REGION_OUTSIDE_ARTBOARD.opt(),
        UPDATE_WHILE_DRAGGING.opt(), BACKGROUND_FILL.opt(), PRESET.opt(),
    ]),
    entry("page.add", "add_page", "Insert page", false, true, &[AFTER.opt(), NEW_SPREAD.opt(), WIDTH.opt(), HEIGHT.opt(), NAME.opt()]),
    entry("page.rename", "rename_page", "Rename page spread", false, true, &[PAGE, NAME]),
    entry("page.resize", "resize_page", "Page size", false, true, &[PAGE, WIDTH, HEIGHT]),
    entry("page.delete", "delete_page", "Delete page", false, true, &[PAGE]),
    entry("document.open", "open_path / open_bytes / expect_on_disk", "Open document", false, false, &[]),
    entry("document.import", "open_import / open_import_detailed", "Open and convert to native", false, false, &[]),
    entry("document.reopen", "reopen_bytes / reopen_path", "Reopen saved document", false, false, &[]),
    entry("document.save", "save_to / save_new / save_replacing", "Save document", false, false, &[PATH, OVERWRITE]),
    entry("document.checkpoint", "checkpoint", "Checkpoint document", false, false, &[]),
    entry("document.verify", "verify_live / verify_reopened", "Verify document", false, false, &[]),
    entry("document.summary", "summary", "Document summary", false, false, &[]),
    // A host-owned read recorded with its frame (lane SMB-DOC-EXPORT-1: the document export); changes no authority.
    entry("document.record_host_outcome", "record_host_outcome", "Record host read", false, false, &[]),
];
