//! Multitrack rows of the command corpus (lane SMB-DRIVE-TYPED-2): every `AutomationCommand` by its
//! key through `Viewer::multitrack_command` (point and clip gain edits are one entry-only undo step
//! each, named `timeline_<key>`; lane choice and waveforms are view settings), the timeline's sound
//! binding and a lane's points. `lane` is `clip_volume`, `track_volume`, `track_pan`, `clip_pan` or
//! `bus.<instance_id>.<key>`; `at` is a timeline tick; a point is its clip (absent: a track or bus
//! point) and its offset in ticks. Feature `audio-batch`; elsewhere `feature_unavailable`.

use super::{CommandEntry, index as i, number as n, text as t, typed};

const RUN: &str = "Viewer::multitrack_command";

pub(super) const ROWS: &[CommandEntry] = &[
    typed("multitrack.open", "Viewer::open_multitrack", "Bind the timeline's sound", false, &[i("block_frames").opt()]),
    typed("multitrack.lane_show", RUN, "Show automation lane", false, &[t("track_id"), t("lane").opt()]),
    typed("multitrack.lane_cycle", RUN, "Next automation lane", false, &[t("track_id")]),
    typed("multitrack.lane_point_add", RUN, "Add automation point", true, &[t("track_id"), t("lane"), i("at"), n("value")]),
    typed("multitrack.lane_point_move", RUN, "Move automation point", true, &[t("track_id"), t("lane"), t("point_clip_id").opt(), i("point_offset_ticks"), i("at"), n("value")]),
    typed("multitrack.lane_point_delete", RUN, "Delete automation point", true, &[t("track_id"), t("lane"), t("point_clip_id").opt(), i("point_offset_ticks")]),
    typed("multitrack.clip_gain", RUN, "Clip gain", true, &[t("clip_id"), n("delta_db")]),
    typed("multitrack.waveforms", RUN, "Waveforms on or off", false, &[]),
    typed("multitrack.lane", "Viewer::timeline_lane", "Automation lane points", false, &[t("track_id"), t("lane")]),
];
