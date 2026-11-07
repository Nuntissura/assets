//! Layout rows of the command corpus.
//!
//! Lane SMB-LAYOUT-PDF-HOST-1: the Layout Editor's output commands. They run on the viewer, not on
//! `Run` (harness-view owns the layout editor and the interop-layout edge), so each `run_method`
//! names its `viewer.<method>` drive row (`manual::DRIVE_VIEWER`), and the drive surface runs the
//! catalog id through that row with the same parameter keys. None changes the document: opening
//! the editor and exporting are view state and derived deliverables.

use super::super::ValueType;
use super::{CommandEntry, ParamSpec, entry, p};

pub const CMD_LAYOUT_OPEN: &str = "layout.open";
pub const CMD_LAYOUT_PREFLIGHT: &str = "layout.preflight";
pub const CMD_LAYOUT_EXPORT_PDF: &str = "layout.export_pdf";
pub const CMD_LAYOUT_EXPORT_IDML: &str = "layout.export_idml";
pub const CMD_LAYOUT_RENDER_PDF_PAGE: &str = "layout.render_pdf_page";

const PATH: ParamSpec = p("path", ValueType::String);
const OVERWRITE: ParamSpec = p("overwrite", ValueType::Boolean);
const PAGE_WIDTH_PT: ParamSpec = p("page_width_pt", ValueType::Number);
const PAGE_HEIGHT_PT: ParamSpec = p("page_height_pt", ValueType::Number);
const PAGES: ParamSpec = p("pages", ValueType::Number);
const PDFX: ParamSpec = p("pdfx", ValueType::Enumerated);
const CREATED: ParamSpec = p("created", ValueType::String);
const OUTPUT_PROFILE: ParamSpec = p("output_profile", ValueType::String);
const OUTPUT_CONDITION_IDENTIFIER: ParamSpec = p("output_condition_identifier", ValueType::String);
const OUTPUT_CONDITION: ParamSpec = p("output_condition", ValueType::String);
const REGISTRY: ParamSpec = p("registry", ValueType::String);
const TITLE: ParamSpec = p("title", ValueType::String);
const SUBSET_FONTS: ParamSpec = p("subset_fonts", ValueType::Boolean);
const IGNORE_PREFLIGHT: ParamSpec = p("ignore_preflight", ValueType::Boolean);
const SOURCE: ParamSpec = p("source", ValueType::String);
const PAGE: ParamSpec = p("page", ValueType::Number);
const PX_PER_PT: ParamSpec = p("px_per_pt", ValueType::Number);

pub(super) const ROWS: &[CommandEntry] = &[
    entry(CMD_LAYOUT_OPEN, "viewer.layout_open", "Open the layout editor", false, false, &[PAGE_WIDTH_PT, PAGE_HEIGHT_PT, PAGES]),
    entry(CMD_LAYOUT_PREFLIGHT, "viewer.layout_preflight", "Preflight the layout", false, false, &[]),
    entry(CMD_LAYOUT_EXPORT_PDF, "viewer.export_layout_pdf", "Export layout as PDF", false, false, &[
        PATH, OVERWRITE, PDFX, CREATED, OUTPUT_PROFILE, OUTPUT_CONDITION_IDENTIFIER, OUTPUT_CONDITION, REGISTRY, TITLE, SUBSET_FONTS, IGNORE_PREFLIGHT,
    ]),
    entry(CMD_LAYOUT_EXPORT_IDML, "viewer.export_layout_idml", "Export layout as IDML", false, false, &[PATH, OVERWRITE]),
    entry(CMD_LAYOUT_RENDER_PDF_PAGE, "viewer.render_pdf_page", "Render a PDF page to PNG", false, false, &[SOURCE, PAGE, PX_PER_PT, PATH, OVERWRITE]),
];
