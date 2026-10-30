//! STU-LAY-119 text-frame auto-size on the threaded reflow (lane SMB-ID-FRAME-AUTOSIZE-1).
use crate::threaded_reflow::LayoutProposal;
use hsk_studio_folio::AutoSizing;

/// Largest width or height an auto-sized frame may take, pt (declared bound, Studio reading).
pub const MAX_AUTOSIZE_EXTENT_PT: f64 = 15_552.0;
/// Bisection stops when the bracket on the frame's larger side is at most this wide, pt.
pub const AUTOSIZE_TOLERANCE_PT: f64 = 0.01;
/// Most compositions one auto-size may run.
pub const MAX_AUTOSIZE_ITERATIONS: u32 = 64;

/// One auto-sized frame: its id, the authored and the new size, and the compositions run.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameResize {
    pub frame_id: String,
    pub frame_index: usize,
    pub auto_sizing: AutoSizing,
    pub before_width_pt: f64,
    pub before_height_pt: f64,
    pub after_width_pt: f64,
    pub after_height_pt: f64,
    pub compositions: u32,
}

/// The reflow with every auto-size applied, and the resizes as its receipt.
#[derive(Clone, Debug, PartialEq)]
pub struct AutosizeProposal {
    pub layout: LayoutProposal,
    pub resizes: Vec<FrameResize>,
}
impl AutosizeProposal {
    pub fn autosized_frames(&self) -> &[FrameResize] {
        &self.resizes
    }
}
