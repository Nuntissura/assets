//! Transform rows of the command corpus (lane SMB-PLACE-ASSET-1): the [STU-RAS-029] Move and Free transform
//! primitives with the [STU-RAS-142] parameter names and the [STU-RAS-141] reference point, and [STU-RAS-014]'s
//! reset-transforms command, on a raster or placed-asset layer (`view::place` in `hsk-studio-harness-view`).

use super::asset::RESAMPLE;
use super::{CommandEntry, number as n, one_of as o, text as t, typed};

/// [STU-RAS-141] nine-member reference point.
const ANCHOR: &str = "top_left|top_centre|top_right|middle_left|middle_centre|middle_right|bottom_left|bottom_centre|bottom_right";

pub(super) const ROWS: &[CommandEntry] = &[
    typed("transform.free_transform", "Viewer::free_transform", "Free Transform", true, &[
        t("layer"), n("scale_x").opt(), n("scale_y").opt(), n("angle").opt(), n("skew_x").opt(), n("skew_y").opt(), n("translate_x").opt(), n("translate_y").opt(),
        o("reference_point", ANCHOR).opt(), o("resample", RESAMPLE).opt(),
    ]),
    typed("transform.move", "Viewer::move_content", "Move", true, &[t("layer"), n("dx"), n("dy")]),
    typed("transform.reset", "Viewer::reset_transform", "Reset Transform", true, &[t("layer")]),
];
