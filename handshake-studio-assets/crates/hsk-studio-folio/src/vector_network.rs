//! `StudioVectorNetwork` record (`hsk.studio.vector_network@1`, lane SMB-ILLUSTRATOR-PEN-3;
//! STU-VEC-001, STU-VEC-004, STU-VEC-108, STU-VEC-109, STU-VEC-110): the wire form of a vector
//! network, three arrays plus an index topology:
//! - `vertices[]`: `x`, `y` (document units) and the optional per-vertex overrides `stroke_cap`,
//!   `stroke_join`, `corner_radius` (>= 0) and `handle_mirroring` (`NONE`, `ANGLE`,
//!   `ANGLE_AND_LENGTH`); absent = inherit from the object;
//! - `segments[]`: `start`, `end` (vertex indices) and the optional `tangent_start` /
//!   `tangent_end` vectors relative to their endpoint (both absent or zero = straight);
//! - `regions[]`: `winding_rule` (`NONZERO` or `EVENODD`; a region never carries `NONE`), `loops`
//!   (arrays of segment indices, each a closed walk), the REQUIRED fill state `fills` and an
//!   optional `fill_style_id`.
//!
//! The record names the `SVPT` path whose geometry it is (`path_id`): Payload::Vector keeps naming
//! paths, so a consumer that ignores networks draws the path's flattened form. Indices out of range
//! are `DanglingReference` (STU-VEC-108: never clamped); an unclosed loop is `InvalidField`;
//! counts are bounded before the walk ([`NETWORK_MAX_ELEMENTS`], [`NETWORK_MAX_REGIONS`],
//! [`NETWORK_MAX_LOOP_REFERENCES`]).
//!
//! Declared gaps: accord gives `hsk.studio.vector_network@1` no identity prefix, so the record is
//! keyed by its path id; the cap and join member names are Studio's (`butt`, `round`, `projecting`;
//! `miter`, `round`, `bevel`), the captured tokens are not recovered here; `fill_style_id` is checked
//! as an `SSTY` id only (resolution against design records is the caller's).
#![deny(clippy::arithmetic_side_effects)]
use super::{Code, Result, err, id};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

schema_token!(VectorNetworkSchema, "hsk.studio.vector_network@1");

/// Most vertices or segments of one network record.
pub const NETWORK_MAX_ELEMENTS: usize = 65_536;
/// Most regions of one network record.
pub const NETWORK_MAX_REGIONS: usize = 4096;
/// Most segment references over all loops of one record.
pub const NETWORK_MAX_LOOP_REFERENCES: usize = 262_144;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioStrokeCap {
    Butt,
    Round,
    Projecting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioStrokeJoin {
    Miter,
    Round,
    Bevel,
}

/// STU-VEC-109 captured tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum StudioHandleMirroring {
    #[serde(rename = "NONE")]
    None,
    #[serde(rename = "ANGLE")]
    Angle,
    #[serde(rename = "ANGLE_AND_LENGTH")]
    AngleAndLength,
}

/// STU-VEC-110 region winding (a region never carries `NONE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum StudioRegionWinding {
    #[serde(rename = "NONZERO")]
    NonZero,
    #[serde(rename = "EVENODD")]
    EvenOdd,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioNetworkVertex {
    pub x: f64,
    pub y: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_cap: Option<StudioStrokeCap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_join: Option<StudioStrokeJoin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corner_radius: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle_mirroring: Option<StudioHandleMirroring>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioNetworkSegment {
    pub start: u32,
    pub end: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tangent_start: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tangent_end: Option<[f64; 2]>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioNetworkRegion {
    pub winding_rule: StudioRegionWinding,
    pub loops: Vec<Vec<u32>>,
    pub fills: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_style_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioVectorNetwork {
    pub schema_id: VectorNetworkSchema,
    /// `SVPT` id of the path this network is the geometry of.
    pub path_id: String,
    pub vertices: Vec<StudioNetworkVertex>,
    pub segments: Vec<StudioNetworkSegment>,
    pub regions: Vec<StudioNetworkRegion>,
}

/// JSON schema (draft 2020-12) of the network record.
pub fn vector_network_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12().into_generator().into_root_schema_for::<StudioVectorNetwork>()
}

fn finite_point(x: f64, y: f64) -> bool {
    x.is_finite() && y.is_finite() && x.abs() <= 1.0e9 && y.abs() <= 1.0e9
}

/// Validates one network record: path id, budgets before the walk, finite coordinates (|v| <= 1e9),
/// non-negative corner radii, indices in range (`DanglingReference`, never clamped), no zero-length
/// self segment, every loop a closed walk (`InvalidField`), at least one loop per region, style ids
/// `SSTY`.
pub fn validate_vector_network(n: &StudioVectorNetwork) -> Result<()> {
    id(&n.path_id, "SVPT", "path_id")?;
    let t = n.path_id.as_str();
    if n.vertices.len() > NETWORK_MAX_ELEMENTS || n.segments.len() > NETWORK_MAX_ELEMENTS || n.regions.len() > NETWORK_MAX_REGIONS {
        return Err(err(Code::BudgetExceeded, t));
    }
    let refs = n.regions.iter().flat_map(|r| r.loops.iter()).fold(0_usize, |s, l| s.saturating_add(l.len()));
    if refs > NETWORK_MAX_LOOP_REFERENCES {
        return Err(err(Code::BudgetExceeded, t));
    }
    for (i, v) in n.vertices.iter().enumerate() {
        if !finite_point(v.x, v.y) || v.corner_radius.is_some_and(|r| !r.is_finite() || r < 0.0) {
            return Err(err(Code::InvalidField, format!("{t}/vertices/{i}")));
        }
    }
    let vertex = |i: u32| usize::try_from(i).ok().filter(|i| *i < n.vertices.len());
    for (i, s) in n.segments.iter().enumerate() {
        let target = format!("{t}/segments/{i}");
        let (Some(_), Some(_)) = (vertex(s.start), vertex(s.end)) else {
            return Err(err(Code::DanglingReference, target));
        };
        for tan in [s.tangent_start, s.tangent_end].into_iter().flatten() {
            if !finite_point(tan[0], tan[1]) {
                return Err(err(Code::InvalidField, target));
            }
        }
        let zero = |t: Option<[f64; 2]>| t.is_none_or(|t| t == [0.0, 0.0]);
        if s.start == s.end && zero(s.tangent_start) && zero(s.tangent_end) {
            return Err(err(Code::InvalidField, target));
        }
    }
    for (i, r) in n.regions.iter().enumerate() {
        let target = format!("{t}/regions/{i}");
        if r.loops.is_empty() {
            return Err(err(Code::InvalidField, target));
        }
        if let Some(style) = &r.fill_style_id {
            id(style, "SSTY", &target)?;
        }
        for l in &r.loops {
            let seg = |k: u32| usize::try_from(k).ok().and_then(|k| n.segments.get(k));
            let Some(first) = l.first().and_then(|k| seg(*k)) else {
                return Err(err(Code::InvalidField, target));
            };
            let closed = [first.start, first.end].into_iter().any(|start| {
                let mut at = start;
                for k in l {
                    match seg(*k) {
                        Some(s) if s.start == at => at = s.end,
                        Some(s) if s.end == at => at = s.start,
                        _ => return false,
                    }
                }
                at == start
            });
            if l.iter().any(|k| seg(*k).is_none()) {
                return Err(err(Code::DanglingReference, target));
            }
            if !closed {
                return Err(err(Code::InvalidField, target));
            }
        }
    }
    Ok(())
}
