//! Stroke arrowheads in the viewer (lane SMB-AI-ARROWHEADS-2; STU-VEC-146). Red stub: the API the
//! tests drive, not yet built.
#![deny(clippy::arithmetic_side_effects)]
use super::super::Viewer;
use hsk_studio_folio::{StudioArrowAlignment, StudioArrowhead};

/// The markers of one stroke row: start and end tokens, their scales (percent of the stroke
/// weight, `1..=1000`) and the alignment of both ends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkerSpec {
    pub start: StudioArrowhead,
    pub end: StudioArrowhead,
    pub start_scale: f64,
    pub end_scale: f64,
    pub alignment: StudioArrowAlignment,
}

impl MarkerSpec {
    /// Both scales at Folio's `DEFAULT_MARKER_SCALE` (100, `studio_declared`).
    pub fn new(start: StudioArrowhead, end: StudioArrowhead, alignment: StudioArrowAlignment) -> Self {
        Self { start, end, start_scale: hsk_studio_folio::DEFAULT_MARKER_SCALE, end_scale: hsk_studio_folio::DEFAULT_MARKER_SCALE, alignment }
    }
}

impl Viewer {
    /// The markers of stroke row `row` of `layer_id`'s appearance.
    pub fn stroke_markers(&self, _layer_id: &str, _row: u32) -> Result<Option<MarkerSpec>, String> {
        Err("stroke_markers: not built".into())
    }

    /// Sets (or with `None` removes) the markers of stroke row `row`.
    pub fn set_stroke_markers(&mut self, _layer_id: &str, _row: u32, _spec: Option<MarkerSpec>) -> Result<String, String> {
        Err("set_stroke_markers: not built".into())
    }

    /// The swap command.
    pub fn swap_stroke_markers(&mut self, _layer_id: &str, _row: u32) -> Result<String, String> {
        Err("swap_stroke_markers: not built".into())
    }
}
