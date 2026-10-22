//! Raster fill-layer records (lane SMB-PS-FILL-LAYERS; spec STU-RAS-007 layer kind `fill`: solid
//! colour, gradient or pattern, STU-RAS-025/160/161). A layer of kind `fill` carries
//! `PrimitiveReference { schema_id: <swatch | gradient | pattern schema of its fill>, object_id: <its
//! own layer id> }` (like an adjustment layer, the record lives beside the document in
//! `StudioRasterLayerRecord::fill_layer`), and the layer is an image source of the composition graph
//! like any pixel layer (STU-CMP-092).
//!
//! Folio stores and validates only. The fill covers `bounds` (the canvas rectangle the source
//! document gave the layer); its mask, opacity, fill opacity and blend mode are the layer's own.
//!
//! Choices the spec does not make (declared, not invented as parity):
//! - The gradient is carried by value as the stop list of `StudioGradient` (STU-VEC-136
//!   [`crate::StudioGradientStop`]: position percent, midpoint, opacity percent, colour) beside the
//!   raster controls of the STU-RAS-160 table (`style`, `angle`, `scale`, `reverse`, `dither`,
//!   `align_with_layer`, `offset_x`, `offset_y`), exactly like the gradient overlay effect: the vector
//!   `StudioGradient` (linear and radial geometry plus an affine) cannot hold the angle, reflected
//!   and diamond styles. Noise gradients and a separate transparency stop list (STU-RAS-160) are not
//!   stored.
//! - The pattern is the pattern primitive reference plus its resolved tile (the pattern overlay's
//!   `pattern_tile` form), so a renderer needs no pattern resolver.
//! - Colours carry the document colour binding of [`StudioColor`] (`unassigned` = the document's
//!   blending space).
//!
//! The records arrive from untrusted wire JSON: integer arithmetic is denied here (workflow V4-01).
#![deny(clippy::arithmetic_side_effects)]
use super::{
    Code, GeometryUnit, Result, StudioGradientStop, TileRef, err,
    raster::StudioEffectFill,
    values::{Ctx, StudioColor, percent},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Largest side of a fill rectangle in pixels (a Folio ceiling; the PSD format allows 300000).
pub const MAX_FILL_SIDE: u32 = 300_000;
/// Largest absolute origin coordinate of a fill rectangle (a Folio ceiling).
pub const MAX_FILL_ORIGIN: i64 = 1 << 31;
/// Gradient stops of a gradient fill (a Folio ceiling, equal to the gradient map's).
pub const MAX_FILL_GRADIENT_STOPS: usize = super::MAX_GRADIENT_MAP_STOPS;

/// Gradient geometry style (STU-RAS-160 `geometry`; `shape_burst` is not stored).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioFillGradientStyle {
    Linear,
    Radial,
    Angle,
    Reflected,
    Diamond,
}

/// The pixel rectangle a fill layer covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioFillBounds {
    pub x: i64,
    pub y: i64,
    pub width: u32,
    pub height: u32,
}

/// The content of a fill layer (tag `fill_kind`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "fill_kind", rename_all = "snake_case", deny_unknown_fields)]
#[allow(clippy::large_enum_variant)]
pub enum StudioFill {
    /// A flat colour.
    Solid { colour: StudioColor },
    /// A gradient (STU-RAS-160): `angle` degrees `-180..=180`, `scale` percent (> 0; the engine's
    /// domain is 10..=150), `offset_x`/`offset_y` percent of the fill size.
    Gradient {
        style: StudioFillGradientStyle,
        stops: Vec<StudioGradientStop>,
        angle: f64,
        scale: f64,
        reverse: bool,
        dither: bool,
        align_with_layer: bool,
        offset_x: f64,
        offset_y: f64,
    },
    /// A tiled pattern (STU-RAS-161): `scale` percent (> 0), `phase_x`/`phase_y` pixels,
    /// `link_with_layer` anchors the tiling at the fill's top-left (otherwise the document origin).
    Pattern {
        pattern: StudioEffectFill,
        /// The pattern's pixels beside the reference (one `rgb_f32le` tile of at most 4096 x 4096
        /// px); absent = unresolved. Not encoded when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pattern_tile: Option<TileRef>,
        scale: f64,
        phase_x: f64,
        phase_y: f64,
        link_with_layer: bool,
    },
}

impl StudioFill {
    /// The primitive schema the hosting `fill` layer's payload names for this content.
    pub fn schema_id(&self) -> &'static str {
        match self {
            Self::Solid { .. } => hsk_studio_accord::SCHEMA_STUDIO_SWATCH,
            Self::Gradient { .. } => hsk_studio_accord::SCHEMA_STUDIO_GRADIENT,
            Self::Pattern { .. } => hsk_studio_accord::SCHEMA_STUDIO_PATTERN,
        }
    }
}

/// The three primitive schemas a `fill` layer payload may name.
pub const FILL_PAYLOAD_SCHEMAS: [&str; 3] = [
    hsk_studio_accord::SCHEMA_STUDIO_SWATCH,
    hsk_studio_accord::SCHEMA_STUDIO_GRADIENT,
    hsk_studio_accord::SCHEMA_STUDIO_PATTERN,
];

/// The fill record of one `fill` layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioFillLayer {
    pub bounds: StudioFillBounds,
    pub fill: StudioFill,
}

fn check(ok: bool, target: &str, field: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(err(Code::InvalidField, format!("{target}/fill_layer/{field}")))
    }
}

/// Validates one fill record against its hosting layer's id `target`.
pub(crate) fn validate_fill_layer(f: &StudioFillLayer, target: &str, cx: &mut Ctx) -> Result<()> {
    let b = &f.bounds;
    check(
        b.width > 0
            && b.height > 0
            && b.width <= MAX_FILL_SIDE
            && b.height <= MAX_FILL_SIDE
            && b.x.unsigned_abs() <= MAX_FILL_ORIGIN.unsigned_abs()
            && b.y.unsigned_abs() <= MAX_FILL_ORIGIN.unsigned_abs(),
        target,
        "bounds",
    )?;
    match &f.fill {
        StudioFill::Solid { colour } => colour.validate(target, cx),
        StudioFill::Gradient {
            stops,
            angle,
            scale,
            offset_x,
            offset_y,
            ..
        } => {
            check(
                angle.is_finite() && (-180.0..=180.0).contains(angle),
                target,
                "angle",
            )?;
            check(
                scale.is_finite() && *scale > 0.0 && offset_x.is_finite() && offset_y.is_finite(),
                target,
                "scale_offset",
            )?;
            check(
                (2..=MAX_FILL_GRADIENT_STOPS).contains(&stops.len()),
                target,
                "stops",
            )?;
            cx.nodes(stops.len(), target)?;
            for s in stops {
                cx.tick()?;
                check(
                    percent(s.position)
                        && percent(s.opacity)
                        && s.midpoint
                            .is_none_or(|m| m.is_finite() && (13.0..=87.0).contains(&m)),
                    target,
                    "stop",
                )?;
                s.colour.validate(target, cx)?;
            }
            check(
                stops.windows(2).all(|w| match w {
                    [a, b] => a.position <= b.position,
                    _ => true,
                }),
                target,
                "stop_order",
            )
        }
        StudioFill::Pattern {
            pattern,
            pattern_tile,
            scale,
            phase_x,
            phase_y,
            ..
        } => {
            check(
                pattern.schema_id == hsk_studio_accord::SCHEMA_STUDIO_PATTERN
                    && !pattern.object_id.is_empty(),
                target,
                "pattern",
            )?;
            check(
                scale.is_finite() && *scale > 0.0 && phase_x.is_finite() && phase_y.is_finite(),
                target,
                "scale_phase",
            )?;
            if let Some(tile) = pattern_tile {
                let px = |v: f64| v.is_finite() && (1.0..=4096.0).contains(&v) && v.fract() == 0.0;
                check(
                    px(tile.width.value)
                        && px(tile.height.value)
                        && tile.width.unit == GeometryUnit::Px
                        && tile.height.unit == GeometryUnit::Px
                        && tile.format == "rgb_f32le"
                        && !tile.artifact_manifest_id.is_empty()
                        && !tile.content_digest.digest.is_empty(),
                    target,
                    "pattern_tile",
                )?;
            }
            Ok(())
        }
    }
}
