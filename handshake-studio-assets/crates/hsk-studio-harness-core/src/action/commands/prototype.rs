//! Prototype play rows of the command corpus (lane SMB-DRIVE-TYPED-2, STU-PRO-124): the in-editor
//! player over a Figma `.fig` (its first flow starting point), driven by frame-local clicks, hovers and
//! a virtual clock. Play mode changes no document authority.

use super::{CommandEntry, index as i, number as n, path as file, typed};

pub(super) const ROWS: &[CommandEntry] = &[
    typed("prototype.open", "Viewer::open_prototype", "Open prototype player", false, &[file("path")]),
    typed("prototype.click", "PrototypePlay::click", "Click in prototype", false, &[n("x"), n("y")]),
    typed("prototype.hover", "PrototypePlay::hover", "Hover in prototype", false, &[n("x"), n("y")]),
    typed("prototype.advance", "PrototypePlay::advance", "Advance prototype clock", false, &[i("ms")]),
    typed("prototype.restart", "PrototypePlay::restart", "Restart prototype", false, &[]),
    typed("prototype.render", "PrototypePlay::render", "Render prototype screen", false, &[]),
    typed("prototype.state", "PrototypePlay::state_json", "Prototype state", false, &[]),
];
