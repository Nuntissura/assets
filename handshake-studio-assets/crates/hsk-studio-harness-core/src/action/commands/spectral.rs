//! Spectral display rows of the command corpus (lane SMB-DRIVE-TYPED-2): open the display on the
//! waveform editor's audio, select a time-frequency region and apply a `SpectralMaskOp` to it (one
//! waveform undo step, `waveform_spectral`). Feature `audio-batch`; elsewhere `feature_unavailable`.

use super::{CommandEntry, index as i, number as n, one_of as o, record as r, typed};

pub(super) const ROWS: &[CommandEntry] = &[
    typed("spectral.open", "Viewer::open_spectral_display", "Open spectral display", false, &[
        i("fft_size").opt(), i("hop").opt(), o("window", "hann|hamming|blackman|blackman_harris|rectangular").opt(), o("scale", "linear|log").opt(), n("min_hz").opt(),
        i("rows").opt(), n("floor_db").opt(),
    ]),
    typed("spectral.set_region", "Viewer::set_spectral_region", "Spectral selection", false, &[
        i("start").opt(), i("end").opt(), n("low_hz").opt(), n("high_hz").opt(), r("lasso", "[[sample, hz], ...]").opt(),
    ]),
    typed("spectral.apply", "Viewer::apply_spectral", "Spectral edit", true, &[o("op", "identity|remove|heal|attenuate"), n("gain_db").opt()]),
    typed("spectral.state", "Viewer::spectral_json", "Spectral display state", false, &[]),
];
