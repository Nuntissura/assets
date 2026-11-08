//! Mixer rows of the command corpus (lane SMB-DRIVE-TYPED-2): every `MixerEdit` member through
//! `Viewer::mixer_edit`, one entry-only undo step each named by its op (`mixer_volume`, ...), plus the
//! mixer's open, render and state. The sound is the bound port (`audio.bind_sound`, `video.project_bind`).
//! A `bus` that is absent names the master. Feature `audio-batch`; elsewhere `feature_unavailable`.

use super::{CommandEntry, flag as b, index as i, number as n, one_of as o, record as r, text as t, typed};

const EDIT: &str = "Viewer::mixer_edit";
/// The panel's effect templates (`mixer::effect_templates`: the four batch kinds, then score's
/// filter-kind table, lane SMB-SCORE-EFFECTS-6); a harness test pins this list to them.
pub(super) const TEMPLATES: &str = "gain|equalizer|dynamics|lookahead_limiter|graphic_eq_10_bands|graphic_eq_20_bands|graphic_eq_30_bands|hard_limiter|flanger|phaser|chorus|click_repair|reverb|feedback_delay|hum_remover|multiband_compressor";
const EFFECT: &str = "a Folio StudioAudioEffectInstance record";

pub(super) const ROWS: &[CommandEntry] = &[
    typed("mixer.open", "Viewer::open_mixer", "Open mixer", false, &[t("sequence_id"), i("block_frames").opt()]),
    typed("mixer.volume", EDIT, "Track volume", true, &[t("track_id"), n("linear")]),
    typed("mixer.pan", EDIT, "Track pan", true, &[t("track_id"), n("pan")]),
    typed("mixer.mute", EDIT, "Mute track", true, &[t("track_id"), b("muted")]),
    typed("mixer.solo", EDIT, "Solo track", true, &[t("track_id"), b("soloed")]),
    typed("mixer.output", EDIT, "Route track output", true, &[t("track_id"), t("bus").opt()]),
    typed("mixer.add_send", EDIT, "Add send", true, &[t("track_id"), t("target"), n("level"), b("pre_fader").opt()]),
    typed("mixer.send", EDIT, "Edit send", true, &[t("track_id"), t("send_id"), n("level"), b("pre_fader"), n("pan")]),
    typed("mixer.remove_send", EDIT, "Remove send", true, &[t("track_id"), t("send_id")]),
    typed("mixer.master_volume", EDIT, "Master volume", true, &[n("linear")]),
    typed("mixer.add_effect", EDIT, "Add effect", true, &[t("bus").opt(), o("template", TEMPLATES).opt(), r("effect", EFFECT).opt(), i("at").opt()]),
    typed("mixer.remove_effect", EDIT, "Remove effect", true, &[t("bus").opt(), t("instance_id")]),
    typed("mixer.move_effect", EDIT, "Move effect", true, &[t("bus").opt(), t("instance_id"), i("to")]),
    typed("mixer.effect_enabled", EDIT, "Bypass effect", true, &[t("bus").opt(), t("instance_id"), b("enabled")]),
    typed("mixer.effect_param", EDIT, "Effect parameter", true, &[t("bus").opt(), t("instance_id"), t("key"), n("value")]),
    typed("mixer.render", "Viewer::mixer_render", "Render a mixer pass", false, &[i("frames")]),
    typed("mixer.state", "Viewer::mixer_json", "Mixer state", false, &[]),
];
