//! Audio rows of the command corpus (lane SMB-AUDIO-OUT-MIX-1): the sound port, program audio out on
//! the program monitor, the routed audio-only mixdown, stems with a sum check and a timeline clip
//! opened in the waveform editor. They run in a build with features `audio-batch` (mixdown, stems,
//! waveform, sound) and `audio-device` (audio out); elsewhere they refuse `feature_unavailable`. Audio
//! out reaches score-device's software device only, never an operating-system device.

use super::{CommandEntry, flag as b, list, number as n, one_of as o, output_path, path, text as t, typed};

/// Tokens of `MixFormat::parse`.
const FORMAT: &str = "wav16|wav24|wav_float|flac16|flac24|aiff16|aiff24|aiff_float";

pub(super) const ROWS: &[CommandEntry] = &[
    typed("audio.bind_sound", "Viewer::set_export_audio", "Bind sound files", false, &[list("files")]),
    typed("audio.out_devices", "Viewer::audio_out_devices", "List audio devices", false, &[]),
    typed("audio.out_open", "Viewer::audio_out_open", "Open audio out", false, &[t("device").opt(), n("block_frames").opt(), t("sequence_id").opt()]),
    typed("audio.out_transport", "Viewer::audio_out_transport", "Audio transport", false, &[o("action", "play|pause|toggle|scrub|loop_on|loop_off"), n("tick").opt()]),
    typed("audio.out_run", "Viewer::audio_out_run", "Run audio out", false, &[n("ms")]),
    typed("audio.out_state", "Viewer::audio_out_json", "Audio out state", false, &[]),
    typed("audio.out_close", "Viewer::audio_out_close", "Close audio out", false, &[]),
    typed("audio.export_mix", "Viewer::export_sequence_mix", "Export audio mixdown", false, &[t("sequence_id"), o("format", FORMAT), output_path("output"), b("overwrite").opt()]),
    typed("audio.export_stems", "Viewer::export_sequence_stems", "Export stems", false, &[t("sequence_id"), list("buses"), b("rest").opt(), o("format", FORMAT), path("dir"), b("overwrite").opt()]),
    typed("audio.waveform_open_clip", "Viewer::open_waveform_from_clip", "Open clip in waveform editor", false, &[t("clip_id")]),
];
