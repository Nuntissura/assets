//! Colour-vision-deficiency simulation matrices (STU-COL-243).
//!
//! Source: G. M. Machado, M. M. Oliveira, L. A. F. Fernandes, "A Physiologically-based Model for Simulation
//! of Color Vision Deficiency", IEEE TVCG 15(6), 2009 (Proc. IEEE Visualization), simulation matrices for
//! severity 1.0 (dichromacy) as published on the authors' page
//! https://www.inf.ufrgs.br/~oliveira/pubs_files/CVD_Simulation/CVD_Simulation.html (numbers only; no code).
//! The page does not state the encoding of its RGB; Studio applies the matrices to LINEAR-light sRGB proof
//! device values (Studio reading, listed in the lane spec deltas), so the proof device of every CVD and
//! operator condition is Prism's linear-light sRGB working profile (`WORKING_PROFILE_LINEAR_SRGB`). Every
//! row of the three matrices sums to 1 within 2e-6, so the neutral axis is preserved
//! (tests/soft_proof.rs `cvd_neutral_axis_is_preserved`).
use super::ColourVision;

/// Machado et al. 2009, protanopia (severity 1.0).
pub const MACHADO_2009_PROTANOPIA: [[f64; 3]; 3] = [
    [0.152286, 1.052583, -0.204868],
    [0.114503, 0.786281, 0.099216],
    [-0.003882, -0.048116, 1.051998],
];
/// Machado et al. 2009, deuteranopia (severity 1.0).
pub const MACHADO_2009_DEUTERANOPIA: [[f64; 3]; 3] = [
    [0.367322, 0.860646, -0.227968],
    [0.280085, 0.672501, 0.047413],
    [-0.011820, 0.042940, 0.968881],
];
/// Machado et al. 2009, tritanopia (severity 1.0).
pub const MACHADO_2009_TRITANOPIA: [[f64; 3]; 3] = [
    [1.255528, -0.076749, -0.178779],
    [-0.078411, 0.930809, 0.147602],
    [0.004733, 0.691367, 0.303900],
];

/// Id under which the CVD proof device profile (`WORKING_PROFILE_LINEAR_SRGB`) is admitted and reported.
pub const CVD_PROOF_DEVICE_PROFILE_ID: &str = "SCPF-019a2c00-0000-7000-8000-00000000c7d0";

impl ColourVision {
    /// The linear-light sRGB simulation matrix of this condition.
    pub const fn matrix(self) -> [[f64; 3]; 3] {
        match self {
            Self::Protanopia => MACHADO_2009_PROTANOPIA,
            Self::Deuteranopia => MACHADO_2009_DEUTERANOPIA,
            Self::Tritanopia => MACHADO_2009_TRITANOPIA,
        }
    }
}

/// `m * v`, then clamped to the proof device range 0..1.
pub(crate) fn simulate(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    m.map(|r| (r[0] * v[0] + r[1] * v[1] + r[2] * v[2]).clamp(0.0, 1.0))
}
