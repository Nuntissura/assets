//! `StudioWidthProfile` (lane SMB-VECTOR-STYLES; STU-VEC-122): the variable-width profile of the
//! stroke of one `SVPT` path, held in `StudioVectorRecords::width_profiles` (absent and empty are
//! the same set; empty is not encoded, so record sets written before it are byte-identical).
//!
//! A profile is a list of width points; each carries `position` (`0..=1`, the arc-length ratio
//! along the path), the incoming `width` (document unit) and, when it differs, an independent
//! outgoing `width_out` (absent = the uniform case). Positions are strictly increasing; there are
//! at least two points. The three captured editing booleans of STU-VEC-122
//! (`adjust_adjoining_width_points`, `single_width_only_incoming`, `single_width_only_outgoing`,
//! default `false`) are carried and skipped at their default. The widths replace the stroke row's
//! weight along the path (consumers: composite's Folio adapter).
#![deny(clippy::arithmetic_side_effects)]
use super::{Code, Result, err, id};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Most width points of one profile.
pub const WIDTH_PROFILE_MAX_POINTS: usize = 256;
/// Largest width (document units).
pub const WIDTH_PROFILE_MAX_WIDTH: f64 = 1.0e6;

/// One width point (STU-VEC-122).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioWidthPoint {
    /// Arc-length ratio along the path, `0..=1`.
    pub position: f64,
    /// Incoming width (document unit, >= 0).
    pub width: f64,
    /// Outgoing width when it differs from `width` (absent = equal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width_out: Option<f64>,
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// The width profile of the stroke of path `path_id`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioWidthProfile {
    /// `SVPT` id of the stroked path.
    pub path_id: String,
    pub points: Vec<StudioWidthPoint>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub adjust_adjoining_width_points: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub single_width_only_incoming: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub single_width_only_outgoing: bool,
}

impl StudioWidthProfile {
    /// True when every point has equal incoming and outgoing width.
    pub fn is_symmetric(&self) -> bool {
        self.points.iter().all(|p| p.width_out.is_none_or(|w| w == p.width))
    }
}

/// Path id grammar, point count (checked before the walk), positions finite in `0..=1` and
/// strictly increasing, widths finite in `0..=WIDTH_PROFILE_MAX_WIDTH`.
pub fn validate_width_profile(p: &StudioWidthProfile) -> Result<()> {
    id(&p.path_id, "SVPT", "path_id")?;
    let t = p.path_id.as_str();
    if p.points.len() > WIDTH_PROFILE_MAX_POINTS {
        return Err(err(Code::BudgetExceeded, t));
    }
    if p.points.len() < 2 {
        return Err(err(Code::InvalidField, format!("{t}/points")));
    }
    let width_ok = |w: f64| w.is_finite() && (0.0..=WIDTH_PROFILE_MAX_WIDTH).contains(&w);
    let mut last = f64::NEG_INFINITY;
    for (i, q) in p.points.iter().enumerate() {
        let ok = q.position.is_finite()
            && (0.0..=1.0).contains(&q.position)
            && q.position > last
            && width_ok(q.width)
            && q.width_out.is_none_or(width_ok);
        if !ok {
            return Err(err(Code::InvalidField, format!("{t}/points/{i}")));
        }
        last = q.position;
    }
    Ok(())
}
