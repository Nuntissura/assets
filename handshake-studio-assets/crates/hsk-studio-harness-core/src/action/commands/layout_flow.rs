//! Layout flow rows of the command corpus (lane SMB-LAYOUT-DELETE-CASCADE-1): autoflow of a loaded text cursor
//! by flow mode and one smart text reflow pass ([STU-LAY-020], [STU-LAY-123]; harness-view
//! `view/commands/layout_flow.rs`; each commits under its own row id, so `layout.undo` takes it), and the `Run` commit of a layer delete that layout records follow
//! (`Run::delete_layout_layers`, driven as `viewer.delete_layer`; the commit runs under this row's id).

use super::{CommandEntry, entry, flag as b, one_of as o, text as t, typed};

const FLOW_MODES: &str = "flow.manual|flow.semi_auto|flow.auto|flow.fixed_page_auto";

pub(super) const ROWS: &[CommandEntry] = &[
    typed("layout.autoflow", "Viewer::layout_autoflow", "Autoflow text", true, &[t("frame"), o("mode", FLOW_MODES), t("page").opt()]),
    typed("layout.smart_reflow", "Viewer::layout_smart_reflow", "Smart text reflow", true, &[t("frame"), b("delete_empty_pages"), b("preserve_facing_page_spreads")]),
    entry("layer.delete_layout_layers", "delete_layout_layers", "Delete layout layers", false, true, &[]),
];
