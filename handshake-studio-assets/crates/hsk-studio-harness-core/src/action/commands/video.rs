//! Video rows of the command corpus.

use super::{CommandEntry, entry};

// Lane SMB-PROJECT-BIN-1 (STU-VID-010/016/021/024/050): the project bin and sequence creation. Each
// carries a typed Rust payload (a Folio command, sequence, track list or clip), so none records; the
// process surface reaches them through the `viewer.project_*` rows, which build the payload.
pub(super) const ROWS: &[CommandEntry] = &[
    entry("project_bin.new_folder", "project_bin_command", "New bin folder", false, true, &[]),
    entry("project_bin.import", "project_bin_command", "Import media into the project bin", false, true, &[]),
    entry("project_bin.move", "project_bin_command", "Move bin items", false, true, &[]),
    entry("project_bin.remove", "project_bin_command", "Remove bin items", false, true, &[]),
    entry("sequence.new", "new_sequence", "New sequence from preset", false, true, &[]),
    entry("sequence.add_tracks", "add_tracks", "Add tracks", false, true, &[]),
    entry("sequence.place_clip", "place_clip", "Place clip from the project bin", false, true, &[]),
];
