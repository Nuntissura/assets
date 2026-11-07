//! Prototype play rows of the command corpus (lane SMB-DRIVE-TYPED-2, STU-PRO-138/139): the in-editor
//! player over a Figma `.fig` (its first flow starting point), driven by named triggers on named nodes,
//! frame-local clicks and hovers and a virtual clock; the spec's `prototype.start_session`,
//! `dispatch_trigger`, `read_runtime_variables`, `capture_frame` and `end_session`, plus the play panel's
//! pointer rows. Play mode changes no document authority. One session per process (no session id yet).

use super::{CommandEntry, flag as b, index as i, number as n, one_of as o, output_path as out, path as file, text as t, typed};

pub(super) const ROWS: &[CommandEntry] = &[
    typed("prototype.start_session", "Viewer::open_prototype", "Start prototype session", false, &[file("path")]),
    typed("prototype.dispatch_trigger", "PrototypePlay::dispatch_trigger", "Fire a prototype trigger", false, &[
        t("node"), o("trigger", "click|hover_enter|hover_leave|mouse_down|mouse_up|press"),
    ]),
    typed("prototype.click", "PrototypePlay::click", "Click in prototype", false, &[n("x"), n("y")]),
    typed("prototype.hover", "PrototypePlay::hover", "Hover in prototype", false, &[n("x"), n("y")]),
    typed("prototype.advance", "PrototypePlay::advance", "Advance prototype clock", false, &[i("ms")]),
    typed("prototype.restart", "PrototypePlay::restart", "Restart prototype", false, &[]),
    typed("prototype.read_runtime_variables", "PlayerSession::state", "Read prototype variables", false, &[]),
    typed("prototype.capture_frame", "PrototypePlay::render", "Capture prototype frame", false, &[out("path").opt(), b("overwrite").opt()]),
    typed("prototype.state", "PrototypePlay::state_json", "Prototype state", false, &[]),
    typed("prototype.end_session", "Viewer::prototype", "End prototype session", false, &[]),
];
