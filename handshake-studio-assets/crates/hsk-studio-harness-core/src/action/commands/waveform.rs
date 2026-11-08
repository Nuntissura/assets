//! Waveform editor rows of the command corpus (lane SMB-DRIVE-TYPED-2): every `WaveformEdit` member
//! but the hosted plugin through `Viewer::waveform_edit`, one entry-only undo step each named
//! `waveform_<op>` (copy is clipboard only), plus selection, snap, peaks and state. Samples are frame
//! indices of the editor's audio; `channels` is a bit mask (bit i = channel i), absent = every channel.
//! The editor opens with `audio.waveform_open_clip`; spectral edits are `spectral.apply` (spectral.rs). Feature
//! `audio-batch`; elsewhere `feature_unavailable`.

use super::mixer::TEMPLATES;
use super::{CommandEntry, flag as b, index as i, number as n, one_of as o, record as r, typed};

const EDIT: &str = "Viewer::waveform_edit";
const CURVES: &str = "linear|equal_power|s_curve|logarithmic";
const EFFECT: &str = "a Folio StudioAudioEffectInstance record";

pub(super) const ROWS: &[CommandEntry] = &[
    typed("waveform.select", "Viewer::waveform_select", "Select samples", false, &[i("start").opt(), i("end").opt(), i("channels").opt()]),
    typed("waveform.snap", "Viewer::set_waveform_snap", "Snap to zero crossings", false, &[b("on")]),
    typed("waveform.copy", EDIT, "Copy samples", false, &[i("start"), i("end"), i("channels").opt()]),
    typed("waveform.cut", EDIT, "Cut samples", true, &[i("start"), i("end")]),
    typed("waveform.paste", EDIT, "Paste samples", true, &[i("at"), o("mode", "insert|overwrite|mix"), i("channels").opt()]),
    typed("waveform.delete", EDIT, "Delete samples", true, &[i("start"), i("end")]),
    typed("waveform.silence", EDIT, "Silence", true, &[i("start"), i("end"), i("channels").opt()]),
    typed("waveform.insert_silence", EDIT, "Insert silence", true, &[i("at"), i("frames")]),
    typed("waveform.fade", EDIT, "Fade", true, &[i("start"), i("end"), i("channels").opt(), o("direction", "in|out"), o("curve", CURVES).opt()]),
    typed("waveform.gain_envelope", EDIT, "Gain envelope", true, &[i("start"), i("end"), i("channels").opt(), r("points", "[[offset_frames, db], ...]")]),
    typed("waveform.amplify", EDIT, "Amplify", true, &[i("start"), i("end"), i("channels").opt(), n("db")]),
    typed("waveform.normalize", EDIT, "Normalize", true, &[i("start"), i("end"), i("channels").opt(), n("target_dbfs"), b("linked")]),
    typed("waveform.invert", EDIT, "Invert", true, &[i("start"), i("end"), i("channels").opt()]),
    typed("waveform.reverse", EDIT, "Reverse", true, &[i("start"), i("end"), i("channels").opt()]),
    typed("waveform.effect", EDIT, "Apply effect to selection", true, &[i("start"), i("end"), i("channels").opt(), o("template", TEMPLATES).opt(), r("effect", EFFECT).opt()]),
    // Lane SMB-SCORE-EFFECTS-6: noise reduction from a noise print over print_start .. print_end (same
    // channels); the reduction fields default to score's `NoiseReductionParams::default()`.
    typed(
        "waveform.noise_reduce",
        EDIT,
        "Reduce noise from a noise print",
        true,
        &[
            i("start"),
            i("end"),
            i("channels").opt(),
            i("print_start"),
            i("print_end"),
            i("fft_size"),
            n("reduction_db").opt(),
            n("over_subtraction").opt(),
            n("smoothing_up_ms").opt(),
            n("smoothing_down_ms").opt(),
            i("freq_smoothing_bins").opt(),
        ],
    ),
    typed("waveform.peaks", "Viewer::waveform_peaks", "Waveform peaks", false, &[i("start"), i("end"), i("columns")]),
    typed("waveform.state", "Viewer::waveform_json", "Waveform editor state", false, &[]),
];
