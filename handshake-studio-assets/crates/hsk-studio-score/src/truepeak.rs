//! 4x oversampling true-peak estimator shared by the lookahead limiter and the loudness meter.
//!
//! Filter: the 48-tap, 4-phase FIR interpolator given as the example in ITU-R BS.1770-5
//! Annex 2 (identical to the BS.1770-4 Annex 2 example; fetched from
//! `https://www.itu.int/dms_pubrec/itu-r/rec/bs/R-REC-BS.1770-5-202311-I!!PDF-E.pdf`; the -4
//! PDF path returned HTTP 404). Coefficients are transcribed exactly (dyadic rationals; per-phase DC
//! gain is 1.0016 / 0.9730 / 0.9730 / 1.0016, as given by the standard). Phase `p` tap `k` multiplies `x[m - k]` and yields the
//! 4x sample at position `m - 5.875 + p / 4` (the filter centre is between taps 23 and 24), so a
//! value pushed at time `m` describes the region `[m - 6, m - 5)` ([`TRUE_PEAK_DELAY`] = 6).
//! Annex 2 states the example "would satisfy the requirements"; accuracy is measured in
//! `tests/truepeak.rs`.
use std::sync::OnceLock;

/// Oversampling factor.
pub(crate) const OVERSAMPLE: usize = 4;
/// Taps per interpolating polyphase branch.
pub(crate) const BRANCH_TAPS: usize = 12;
/// Delay of the interpolator in original-rate samples.
pub(crate) const TRUE_PEAK_DELAY: usize = 6;

/// BS.1770-5 Annex 2 coefficients, one row per phase (phase 0..=3), 12 taps each.
const ITU_PHASES: [[f64; BRANCH_TAPS]; OVERSAMPLE] = [
    [
        0.0017089843750, 0.0109863281250, -0.0196533203125, 0.0332031250000, -0.0594482421875, 0.1373291015625,
        0.9721679687500, -0.1022949218750, 0.0476074218750, -0.0266113281250, 0.0148925781250, -0.0083007812500,
    ],
    [
        -0.0291748046875, 0.0292968750000, -0.0517578125000, 0.0891113281250, -0.1665039062500, 0.4650878906250,
        0.7797851562500, -0.2003173828125, 0.1015625000000, -0.0582275390625, 0.0330810546875, -0.0189208984375,
    ],
    [
        -0.0189208984375, 0.0330810546875, -0.0582275390625, 0.1015625000000, -0.2003173828125, 0.7797851562500,
        0.4650878906250, -0.1665039062500, 0.0891113281250, -0.0517578125000, 0.0292968750000, -0.0291748046875,
    ],
    [
        -0.0083007812500, 0.0148925781250, -0.0266113281250, 0.0476074218750, -0.1022949218750, 0.9721679687500,
        0.1373291015625, -0.0594482421875, 0.0332031250000, -0.0196533203125, 0.0109863281250, 0.0017089843750,
    ],
];

fn branches() -> &'static [[f64; BRANCH_TAPS]; OVERSAMPLE] {
    static TABLE: OnceLock<[[f64; BRANCH_TAPS]; OVERSAMPLE]> = OnceLock::new();
    TABLE.get_or_init(|| ITU_PHASES)
}

/// Per-channel streaming estimator. `push` returns the largest magnitude among the four
/// oversampled values and the original sample that describe the region `[m - 6, m - 5)`.
#[derive(Clone, Debug)]
pub(crate) struct TruePeakFilter {
    history: [f64; BRANCH_TAPS],
}

impl TruePeakFilter {
    pub(crate) fn new() -> Self {
        Self {
            history: [0.0; BRANCH_TAPS],
        }
    }
    pub(crate) fn reset(&mut self) {
        self.history = [0.0; BRANCH_TAPS];
    }
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        self.history.copy_within(0..BRANCH_TAPS - 1, 1);
        self.history[0] = x;
        // The Annex 2 phase-0 branch is not an identity, so the raw sample is included: the
        // true peak can never be below the sample peak (also keeps the limiter clamp idle).
        let mut peak = self.history[TRUE_PEAK_DELAY].abs();
        for row in branches() {
            let mut acc = 0.0;
            for (tap, h) in row.iter().zip(&self.history) {
                acc += tap * h;
            }
            peak = peak.max(acc.abs());
        }
        peak
    }
}
