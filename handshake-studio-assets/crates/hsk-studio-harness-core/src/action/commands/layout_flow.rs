//! Layout flow rows of the command corpus (lane SMB-LAYOUT-DELETE-CASCADE-1): autoflow of a loaded text cursor
//! by flow mode and one smart text reflow pass ([STU-LAY-020], [STU-LAY-123]; harness-view
//! `view/commands/layout_flow.rs`), and the `Run` commit of a layer delete that layout records follow
//! (`Run::delete_layout_layers`, driven as `layer.delete_layer`).

use super::{CommandEntry, entry};

pub(super) const ROWS: &[CommandEntry] = &[
    entry("layer.delete_layout_layers", "delete_layout_layers", "Delete layout layers", false, true, &[]),
];
