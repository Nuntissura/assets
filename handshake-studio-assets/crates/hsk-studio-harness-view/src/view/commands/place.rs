//! `asset.*` and `transform.*` (lane SMB-PLACE-ASSET-1): the placed-asset and transform commands of
//! `view::place`, whose module docs are their manual.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::super::place::{Anchor, FreeTransform, PlaceOptions, PlaceRefusal, PlacementKind};
use super::CommandRefusal;
use super::decode::Fields;
use hsk_studio_pigment::transform_math::ResampleFilter;
use serde_json::{Value, json};

const RESAMPLE: [(&str, ResampleFilter); 3] =
    [("nearest_neighbour", ResampleFilter::NearestNeighbour), ("bilinear", ResampleFilter::Bilinear), ("bicubic", ResampleFilter::Bicubic)];

fn refusal(r: PlaceRefusal) -> CommandRefusal {
    CommandRefusal::new(&r.code, json!({"message": r.detail}))
}

fn resample(f: Fields<'_>) -> Result<Option<ResampleFilter>, CommandRefusal> {
    if f.has("resample") { f.token("resample", &RESAMPLE).map(Some) } else { Ok(None) }
}

pub(super) fn run_asset(v: &mut Viewer, id: &str, f: Fields<'_>) -> Result<Value, CommandRefusal> {
    match id {
        "asset.place" => {
            let kind = f.token("placement_kind", &[("linked", PlacementKind::Linked), ("embedded", PlacementKind::Embedded)])?;
            let opts = PlaceOptions {
                x: f.opt_number("x")?,
                y: f.opt_number("y")?,
                scale_pct: f.opt_number("scale_pct")?,
                name: f.opt_text("name")?,
                opacity_pct: f.opt_number("opacity_pct")?,
                resample: resample(f)?,
            };
            v.place_asset(&f.path("path")?, kind, &opts).map_err(refusal)
        }
        "asset.list_placements" => Ok(v.list_placements()),
        "asset.update_all_modified" => v.update_all_modified().map_err(refusal),
        "asset.embed_link" => v.embed_link(&f.text("layer")?).map_err(refusal),
        other => Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
    }
}

pub(super) fn run_transform(v: &mut Viewer, id: &str, f: Fields<'_>) -> Result<Value, CommandRefusal> {
    let layer = f.text("layer")?;
    match id {
        "transform.free_transform" => {
            let base = FreeTransform::default();
            let t = FreeTransform {
                scale_x: f.number_or("scale_x", base.scale_x)?,
                scale_y: f.number_or("scale_y", base.scale_y)?,
                angle: f.number_or("angle", base.angle)?,
                skew_x: f.number_or("skew_x", base.skew_x)?,
                skew_y: f.number_or("skew_y", base.skew_y)?,
                translate_x: f.number_or("translate_x", base.translate_x)?,
                translate_y: f.number_or("translate_y", base.translate_y)?,
                reference_point: if f.has("reference_point") { f.token("reference_point", &Anchor::ALL)? } else { base.reference_point },
                resample: resample(f)?,
            };
            v.free_transform(&layer, &t).map_err(refusal)
        }
        "transform.move" => v.move_content(&layer, f.number("dx")?, f.number("dy")?).map_err(refusal),
        "transform.reset" => v.reset_transform(&layer).map_err(refusal),
        other => Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
    }
}
