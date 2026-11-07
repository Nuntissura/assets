//! Monitor rows of the command corpus (lane SMB-DRIVE-TYPED-2, STU-VID-024/025): the program monitor on
//! a sequence, media bound to it, the source monitor on a media file, its marks, the monitor keys and
//! the source-to-program edit (`Viewer::source_edit`, one undo step: `source_insert` or
//! `source_overwrite`). Opening media needs feature `media`; elsewhere those two refuse
//! `feature_unavailable`.

use super::{CommandEntry, index as i, list as l, one_of as o, path as file, text as t, typed};

const KEYS: &str = "shuttle_left|shuttle_stop|shuttle_right|slow_left|slow_right|toggle_play|step_back|step_forward|mark_in|mark_out|insert|overwrite";

pub(super) const ROWS: &[CommandEntry] = &[
    typed("monitor.open", "Viewer::open_monitor", "Open program monitor", false, &[t("sequence_id"), i("width").opt(), i("height").opt()]),
    typed("monitor.bind_program_media", "Viewer::bind_program_media", "Bind media to the program monitor", false, &[t("sequence_id"), l("files"), i("width").opt(), i("height").opt()]),
    typed("monitor.open_source_media", "Viewer::open_source_media", "Open media in the source monitor", false, &[file("path"), i("width").opt(), i("height").opt()]),
    typed("monitor.source_mark", "SourceMonitor::mark_in / mark_out", "Mark source in and out", false, &[i("mark_in").opt(), i("mark_out").opt()]),
    typed("monitor.key", "Viewer::monitor_key", "Monitor key", true, &[o("target", "source|program"), o("key", KEYS), i("frames").opt()]),
    typed("monitor.source_edit", "Viewer::source_edit", "Insert or overwrite from source", true, &[
        o("mode", "insert|overwrite"), l("targets").opt(), o("fit", "change_speed|trim_head|trim_tail|ignore_sequence_out").opt(),
    ]),
];
