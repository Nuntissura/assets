//! Catalog rows: read the one descriptor catalog through the command loop.

use super::{CommandEntry, text as t, typed};

pub(super) const ROWS: &[CommandEntry] = &[
    typed("catalog.list", "Viewer::ui_catalog", "List the descriptor catalog", false, &[]),
    typed("catalog.resolve", "Viewer::ui_catalog", "Resolve an id in the catalog", false, &[t("id")]),
];
