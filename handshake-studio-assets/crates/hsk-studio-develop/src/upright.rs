//! Automatic perspective correction (STU-RAW-118 `auto_perspective_*`). Slice 1 placeholder: only
//! stored candidate transforms run; frame analysis is refused until slice 2 lands.
use crate::DevelopError;
use crate::geometry::Geometry;

/// Result of an upright analysis: the six candidates (off, auto, level, vertical, full, guided).
#[derive(Clone, Debug, PartialEq)]
pub struct UprightAnalysis {
    pub transforms: [[f64; 9]; 6],
}

pub fn analyse(_rgb: &[f32], _width: u32, _height: u32, _geometry: &Geometry, _luma: [f64; 3]) -> Result<UprightAnalysis, DevelopError> {
    Err(DevelopError::UnsupportedContribution("upright_analysis"))
}
