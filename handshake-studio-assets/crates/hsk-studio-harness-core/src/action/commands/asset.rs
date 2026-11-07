//! Placed-asset rows of the command corpus (lane SMB-PLACE-ASSET-1): [STU-ASSET-010] `asset.place`,
//! `asset.update_all_modified` and `asset.list_placements`, and the menu leaf Embed Link of [STU-SHL-023]
//! (FILE > Place); the viewer method each reaches is in `hsk-studio-harness-view` `view::place`.

use super::{CommandEntry, entry, number as n, one_of as o, path, text as t, typed};

/// [STU-RAS-141] resampling methods the transform maths implements (members 2, 3, 4).
pub(super) const RESAMPLE: &str = "nearest_neighbour|bilinear|bicubic";

pub(super) const ROWS: &[CommandEntry] = &[
    typed("asset.place", "Viewer::place_asset", "Place", true, &[
        path("path"), o("placement_kind", "linked|embedded"), n("x").opt(), n("y").opt(), n("scale_pct").opt(), t("name").opt(), n("opacity_pct").opt(), o("resample", RESAMPLE).opt(),
    ]),
    typed("asset.list_placements", "Viewer::list_placements", "Link Info", false, &[]),
    typed("asset.update_all_modified", "Viewer::update_all_modified", "Update All Links", true, &[]),
    typed("asset.embed_link", "Viewer::embed_link", "Embed Link", true, &[t("layer")]),
    // The Run method Update All Links commits through: several layers' tiles and the entry, one step.
    entry("layer.set_layers_tiles_and_entry", "set_layers_tiles_and_entry", "Replace raster tiles of layers with document entry", false, true, &[]),
];
