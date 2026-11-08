//! Placed-asset rows of the command corpus (lane SMB-PLACE-ASSET-1): [STU-ASSET-010] `asset.place`,
//! `asset.update_all_modified` and `asset.list_placements`, and the menu leaf Embed Link of [STU-SHL-023]
//! (FILE > Place); the viewer method each reaches is in `hsk-studio-harness-view` `view::place`.
//!
//! Layout graphic frames (lane SMB-LAYOUT-PLACE-1; [STU-LAY-028], [STU-LAY-029], [STU-LAY-131]): on a
//! document with pages and no artboard, `asset.place` puts the picture into a graphic frame and needs
//! `page` and the frame `frame_x`, `frame_y`, `frame_width`, `frame_height` (pt, page coordinates); `shape`
//! (`rectangle` or `ellipse`), `fit`, `frame_fitting` (the stored rule) and `anchor` are the frame's.
//! `asset.fit` re-fits a graphic frame (`fit`, optional `anchor`, crop amounts in pt and the stored rule);
//! the id is a Studio reading: no spec id is verified for fitting.

use super::{CommandEntry, entry, number as n, one_of as o, path, text as t, typed};

/// [STU-RAS-141] resampling methods the transform maths implements (members 2, 3, 4).
pub(super) const RESAMPLE: &str = "nearest_neighbour|bilinear|bicubic";
/// [STU-LAY-131] fit members legal on a graphic frame (`honour_existing_style` is data merge only and refuses).
pub(super) const FIT: &str = "content_to_frame|center_content|proportionally|content_aware|frame_to_content|fill_proportionally|apply_frame_fitting_options|preserve_sizes|honour_existing_style";
/// [STU-LAY-029] anchor point of placed content (Folio `StudioFitAnchor`).
pub(super) const ANCHOR: &str = "top_left|top_center|top_right|center_left|center|center_right|bottom_left|bottom_center|bottom_right";

pub(super) const ROWS: &[CommandEntry] = &[
    typed("asset.place", "Viewer::place_asset", "Place", true, &[
        path("path"), o("placement_kind", "linked|embedded"), n("x").opt(), n("y").opt(), n("scale_pct").opt(), t("name").opt(), n("opacity_pct").opt(), o("resample", RESAMPLE).opt(),
        t("page").opt(), n("frame_x").opt(), n("frame_y").opt(), n("frame_width").opt(), n("frame_height").opt(), o("shape", "rectangle|ellipse|path").opt(), o("fit", FIT).opt(),
        o("frame_fitting", FIT).opt(), o("anchor", ANCHOR).opt(),
    ]),
    typed("asset.fit", "Viewer::fit_graphic", "Fitting", true, &[
        t("layer"), o("fit", FIT), o("anchor", ANCHOR).opt(), n("crop_top").opt(), n("crop_left").opt(), n("crop_bottom").opt(), n("crop_right").opt(), o("frame_fitting", FIT).opt(),
    ]),
    typed("asset.list_placements", "Viewer::list_placements", "Link Info", false, &[]),
    typed("asset.update_all_modified", "Viewer::update_all_modified", "Update All Links", true, &[]),
    typed("asset.embed_link", "Viewer::embed_link", "Embed Link", true, &[t("layer")]),
    // The Run method Update All Links commits through: several layers' tiles and the entry, one step.
    entry("layer.set_layers_tiles_and_entry", "set_layers_tiles_and_entry", "Replace raster tiles of layers with document entry", false, true, &[]),
    // The Run method a layout placement and fit commit through: the layout step and the raster records entry, one step.
    entry("document.submit_layout_and_entry", "submit_layout_and_entry", "Commit layout edit with document entry", false, true, &[]),
];
