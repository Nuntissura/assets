//! Stale-frame revision gate of the canvas preview (lane SMB-JOB-ADMISSION-1, RF-08 phase 0;
//! STU-ARC-013, GUI-DESKTOP-035). No toolkit code.
//!
//! At display time [`Viewer::gate_preview`] compares the rendered frame's document revision with
//! the document head. A frame that is not the head's (older) is recorded as typed
//! `JobOutcome::Superseded { revision: head }`: counted ([`Viewer::preview_superseded`], also the
//! session job counters), emitted through observe (codes only), named in a status log line, and
//! never shown as current; the viewer stays dirty so the next frame renders the head. Previews are
//! synchronous replaceable work and are not admitted through the `JobPort` (module
//! `job_admission`). Today's renderer reads the head it renders, so a superseded frame needs a
//! frame produced before an edit (the renderer swap's asynchronous path is the case this gate is
//! for); test `viewer_job_admission.rs` `preview_frame_older_than_the_head_is_superseded_and_not_shown`.
use super::Viewer;
use super::render::{Frame, RenderFail};
use serde_json::{Value, json};

/// Superseded-preview counter and the last superseded pair.
#[derive(Debug, Default)]
pub(in crate::view) struct PreviewGate {
    superseded: u64,
    /// (frame revision, head revision) of the last superseded frame.
    last: Option<(u64, u64)>,
}

impl Viewer {
    /// The display-time gate (module docs): `None` when the frame was superseded (nothing to
    /// show), else the result unchanged (render failures pass through).
    pub fn gate_preview(&mut self, result: Result<Frame, RenderFail>) -> Option<Result<Frame, RenderFail>> {
        let frame_revision = match &result {
            Ok(frame) => frame.revision,
            Err(_) => return Some(result),
        };
        let head = self.document_head();
        if frame_revision == head {
            return Some(result);
        }
        let _ = self.record_superseded_preview(frame_revision, head);
        self.jobs.previews.superseded = self.jobs.previews.superseded.saturating_add(1);
        self.jobs.previews.last = Some((frame_revision, head));
        self.dirty = true;
        self.say(format!("preview: frame of revision {frame_revision} superseded by revision {head} (not shown)"));
        None
    }

    /// Preview frames superseded since the viewer started.
    pub fn preview_superseded(&self) -> u64 {
        self.jobs.previews.superseded
    }

    /// `{superseded, last: {frame_revision, head_revision} | null}`.
    pub fn preview_gate_json(&self) -> Value {
        json!({
            "superseded": self.jobs.previews.superseded,
            "last": self.jobs.previews.last.map(|(f, h)| json!({"frame_revision": f, "head_revision": h})),
        })
    }
}
