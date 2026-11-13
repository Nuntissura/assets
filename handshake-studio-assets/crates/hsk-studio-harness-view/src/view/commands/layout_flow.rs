//! `layout_flow.*` (lane SMB-LAYOUT-DELETE-CASCADE-1; [STU-LAY-020], [STU-LAY-123]): autoflow of a loaded text
//! cursor by flow mode and one smart text reflow pass, each committed to the open document as ONE undo step
//! (`layout.autoflow`, `layout.smart_reflow`; `Viewer::layout_commit`), saved with the package; nothing placed
//! or removed is no step. The editor rules are `LayoutEditor::autoflow` and `LayoutEditor::smart_reflow`.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::super::shell::Panel;
use super::CommandRefusal;
use super::decode::Fields;
use crate::layout_ui::LayoutUiError;
use crate::layout_ui::threads::{FlowMode, FlowReport, ReflowControls};
use serde_json::{Value, json};

impl Viewer {
    /// `layout_flow.autoflow`: [`crate::layout_ui::LayoutEditor::autoflow`] on the document's layout, one document
    /// step `layout.autoflow`. Refusals change nothing: `not_open`, `layout_editor_unavailable`, the editor's codes
    /// (`unknown_frame`, `unknown_page`, `already_threaded`, ...), the session's (`value_conflict`, `folio_rejected`, ...).
    pub fn layout_autoflow(&mut self, frame: &str, mode: FlowMode, page: Option<&str>) -> Result<FlowReport, String> {
        self.layout_flow_step("layout.autoflow", |ed| ed.autoflow(frame, mode, page))
    }

    /// `layout_flow.smart_reflow`: [`crate::layout_ui::LayoutEditor::smart_reflow`], one document step
    /// `layout.smart_reflow`; refusals as [`Viewer::layout_autoflow`].
    pub fn layout_smart_reflow(&mut self, frame: &str, controls: ReflowControls) -> Result<FlowReport, String> {
        self.layout_flow_step("layout.smart_reflow", |ed| ed.smart_reflow(frame, controls))
    }

    fn layout_flow_step(&mut self, command: &'static str, edit: impl FnOnce(&mut crate::layout_ui::LayoutEditor) -> Result<FlowReport, LayoutUiError>) -> Result<FlowReport, String> {
        if self.run.snapshot().is_none() {
            return Err("not_open".into());
        }
        self.layout_sync();
        let Some(ed) = self.layout_editor() else {
            return Err("layout_editor_unavailable".into());
        };
        let report = edit(ed).map_err(|e| e.code().to_owned())?;
        if report.frames.is_empty() && report.pages_added.is_empty() && report.pages_removed.is_empty() {
            return Ok(report);
        }
        if let Err(code) = self.layout_commit(command, |_| {}) {
            if let Some(ed) = self.layout_editor() {
                ed.undo();
            }
            return Err(code);
        }
        Ok(report)
    }
}

fn reply(viewer: &Viewer, r: &FlowReport) -> Value {
    let mut out = json!({
        "frames": r.frames, "pages_added": r.pages_added, "pages_removed": r.pages_removed,
        "cursor_loaded": r.cursor_loaded, "stopped": r.stopped,
        "changed": !(r.frames.is_empty() && r.pages_added.is_empty() && r.pages_removed.is_empty()),
    });
    if let (Value::Object(o), Value::Object(h)) = (&mut out, viewer.layout_history()) {
        o.extend(h);
    }
    out
}

pub(super) fn run(viewer: &mut Viewer, id: &str, f: Fields<'_>) -> Result<Value, CommandRefusal> {
    // Decoded first: a refused row opens no editor.
    let frame = f.text("frame")?;
    let step = |r: Result<FlowReport, String>| r.map_err(|code| CommandRefusal::new(&code, Value::Null));
    let done = match id {
        "layout_flow.autoflow" => {
            let mode = f.token("mode", &FlowMode::ALL)?;
            let page = f.opt_text("page")?;
            open(viewer)?;
            step(viewer.layout_autoflow(&frame, mode, page.as_deref()))?
        }
        "layout_flow.smart_reflow" => {
            let controls = ReflowControls { delete_empty_pages: f.flag("delete_empty_pages")?, preserve_facing_page_spreads: f.flag("preserve_facing_page_spreads")? };
            open(viewer)?;
            step(viewer.layout_smart_reflow(&frame, controls))?
        }
        other => return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
    };
    Ok(reply(viewer, &done))
}

/// The layout editor, opened as the window's panel opens it.
fn open(viewer: &mut Viewer) -> Result<(), CommandRefusal> {
    if viewer.layout_editor().is_none() {
        viewer.toggle_panel(Panel::LayoutEditor).map_err(|message| CommandRefusal::new("layout_editor_unavailable", json!({"message": message})))?;
    }
    Ok(())
}
