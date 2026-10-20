//! Text-frame threading display and story editor rows (lane SMB-LAYOUT-UI-2 slice 2;
//! STU-LAY-122, STU-LAY-026). [`LayoutEditor::thread_info`] exposes the five thread references of
//! STU-LAY-122 (`parent_story`, `start_text_frame`, `end_text_frame`, `previous_text_frame`,
//! `next_text_frame`), `text_frame_index` and `overflows`, read from the Folio links and the
//! composed proposal (`hsk_studio_layout::reflow_flow`) without rendering; the page view draws an
//! in port (top left) and an out port (bottom right) per frame: empty, threaded, or the overset
//! marker when the story does not fit; a threads-view overlay joins frames in flow order.
//! [`LayoutEditor::story_lines`] is the story editor's line list: one row per composed line with its
//! frame ordinal, column and depth (baseline from the frame top, pt), then the overset rows after an
//! overset marker. Threading itself stays the frame tool's (`LayoutEdit::Thread`/`Unthread`).
//!
//! Declared gaps: inline display of notes, tracked changes, tables and structure tags in the story
//! editor (STU-LAY-026) is not drawn; ports are not drag targets (click the frame with the thread
//! tool). InDesign port/story-editor behaviour UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use super::{LayoutEditor, LayoutUiError, story_text, widget};
use hsk_studio_folio::Payload;
use serde_json::{Value, json};

/// State of a frame's in or out port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Port {
    /// No thread at this end.
    Empty,
    /// Threaded to a previous (in) or next (out) frame.
    Threaded,
    /// Out port of the last frame of a story with overset text.
    Overset,
}
impl Port {
    pub fn code(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Threaded => "threaded",
            Self::Overset => "overset",
        }
    }
}

/// STU-LAY-122 thread references of one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadInfo {
    pub frame_id: String,
    pub parent_story: String,
    pub start_text_frame: String,
    pub end_text_frame: String,
    pub previous_text_frame: Option<String>,
    pub next_text_frame: Option<String>,
    /// Ordinal of this frame in its story's chain (0-based).
    pub text_frame_index: usize,
    /// The story has overset text.
    pub overflows: bool,
    pub in_port: Port,
    pub out_port: Port,
    /// Story bytes composed into this frame (`None` = no line).
    pub source: Option<(u64, u64)>,
}

/// One story-editor line.
#[derive(Clone, Debug, PartialEq)]
pub struct StoryLine {
    pub paragraph: usize,
    /// Ordinal of the frame in the chain; `None` for overset rows.
    pub frame: Option<usize>,
    pub column: usize,
    /// Baseline from the frame top, pt; `None` for overset rows.
    pub depth_pt: Option<f64>,
    pub start: u64,
    pub end: u64,
    pub text: String,
    pub overset: bool,
}

/// Port colours of the page view (also the headless visual assertions).
pub mod colour {
    pub const PORT_EMPTY: egui::Color32 = egui::Color32::from_rgb(250, 250, 250);
    pub const PORT_THREADED: egui::Color32 = egui::Color32::from_rgb(0, 90, 200);
    pub const PORT_OVERSET: egui::Color32 = egui::Color32::from_rgb(220, 0, 0);
}
/// Port square size, pt.
pub const PORT_SIZE: f32 = 6.0;

impl LayoutEditor {
    fn frame_link(&self, id: &str) -> Option<(Option<String>, Option<String>)> {
        let l = self.doc.document.layers.iter().find(|l| l.layer_id == id)?;
        let f = l.text_frame.as_ref()?;
        Some((f.previous_text_frame.clone(), f.next_text_frame.clone()))
    }

    /// The thread references of a text frame (STU-LAY-122), without rendering.
    pub fn thread_info(&self, frame: &str) -> Result<ThreadInfo, LayoutUiError> {
        let layer = self
            .doc
            .document
            .layers
            .iter()
            .find(|l| l.layer_id == frame && l.text_frame.is_some())
            .ok_or_else(|| LayoutUiError::UnknownFrame(frame.to_owned()))?;
        let Payload::Text { story_id } = &layer.payload else { return Err(LayoutUiError::UnknownFrame(frame.to_owned())) };
        let (previous, next) = self.frame_link(frame).ok_or_else(|| LayoutUiError::UnknownFrame(frame.to_owned()))?;
        let bound = self.doc.document.layers.len();
        // Walk to the chain start, counting this frame's index; then to the end.
        let (mut start, mut index, mut steps) = (frame.to_owned(), 0usize, 0usize);
        while let Some(p) = self.frame_link(&start).and_then(|(p, _)| p) {
            steps = steps.saturating_add(1);
            if steps > bound {
                return Err(LayoutUiError::ThreadCycle);
            }
            start = p;
            index = index.saturating_add(1);
        }
        let (mut end, mut steps) = (frame.to_owned(), 0usize);
        while let Some(n) = self.frame_link(&end).and_then(|(_, n)| n) {
            steps = steps.saturating_add(1);
            if steps > bound {
                return Err(LayoutUiError::ThreadCycle);
            }
            end = n;
        }
        let proposal = self.compose(story_id)?;
        let overflows = proposal.overset.is_some();
        let source = proposal.frames.iter().find(|f| f.frame_id == frame).and_then(|f| {
            let s = f.lines.iter().map(|l| l.source.start).min()?;
            let e = f.lines.iter().map(|l| l.source.end).max()?;
            Some((s, e))
        });
        Ok(ThreadInfo {
            frame_id: frame.to_owned(),
            parent_story: story_id.clone(),
            start_text_frame: start,
            end_text_frame: end,
            in_port: if previous.is_some() { Port::Threaded } else { Port::Empty },
            out_port: match (&next, overflows) {
                (Some(_), _) => Port::Threaded,
                (None, true) => Port::Overset,
                (None, false) => Port::Empty,
            },
            previous_text_frame: previous,
            next_text_frame: next,
            text_frame_index: index,
            overflows,
            source,
        })
    }

    /// Story-editor lines: composed lines with frame, column and depth, then the overset rows.
    pub fn story_lines(&self, story_id: &str) -> Result<Vec<StoryLine>, LayoutUiError> {
        let st = self
            .doc
            .stories
            .iter()
            .find(|s| s.story_id == story_id)
            .ok_or_else(|| LayoutUiError::UnknownStory(story_id.to_owned()))?;
        let text = story_text(st);
        let slice = |s: u64, e: u64| -> String {
            text.get(usize::try_from(s).unwrap_or(usize::MAX)..usize::try_from(e).unwrap_or(usize::MAX))
                .unwrap_or_default()
                .trim_end_matches('\n')
                .to_owned()
        };
        let p = self.compose(story_id)?;
        let mut out = Vec::new();
        for (fi, f) in p.frames.iter().enumerate() {
            for l in &f.lines {
                out.push(StoryLine {
                    paragraph: l.paragraph,
                    frame: Some(fi),
                    column: l.column,
                    depth_pt: Some(l.baseline_y_pt),
                    start: l.source.start,
                    end: l.source.end,
                    text: slice(l.source.start, l.source.end),
                    overset: false,
                });
            }
        }
        if let Some(o) = p.overset {
            for (pi, para) in st.paragraphs.iter().enumerate() {
                let (s, e) = (para.start.max(o.start), para.end.min(o.end));
                if s < e || (s == e && para.start == para.end && para.start >= o.start && o.start < o.end) {
                    out.push(StoryLine { paragraph: pi, frame: None, column: 0, depth_pt: None, start: s, end: e, text: slice(s, e), overset: true });
                }
            }
        }
        Ok(out)
    }

    pub(crate) fn threads_json(&self, frame: &str) -> Value {
        match self.thread_info(frame) {
            Ok(t) => json!({
                "parent_story": t.parent_story, "start_text_frame": t.start_text_frame, "end_text_frame": t.end_text_frame,
                "text_frame_index": t.text_frame_index, "overflows": t.overflows,
                "in_port": t.in_port.code(), "out_port": t.out_port.code(), "source": t.source.map(|(s, e)| [s, e]),
            }),
            Err(e) => json!({"refusal": e.code()}),
        }
    }

    /// In and out port squares of one frame (screen rectangle `r`).
    #[allow(clippy::arithmetic_side_effects)]
    pub(crate) fn draw_ports(&self, painter: &egui::Painter, r: egui::Rect, frame: &str) {
        let Ok(t) = self.thread_info(frame) else { return };
        let fill = |p: Port| match p {
            Port::Empty => colour::PORT_EMPTY,
            Port::Threaded => colour::PORT_THREADED,
            Port::Overset => colour::PORT_OVERSET,
        };
        let size = egui::vec2(PORT_SIZE, PORT_SIZE);
        let inp = egui::Rect::from_min_size(r.min + egui::vec2(2.0, 2.0), size);
        let outp = egui::Rect::from_min_size(r.max - egui::vec2(2.0 + PORT_SIZE, 2.0 + PORT_SIZE), size);
        for (rect, port) in [(inp, t.in_port), (outp, t.out_port)] {
            painter.rect_filled(rect, 0.0, fill(port));
            painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.0_f32, egui::Color32::DARK_GRAY), egui::StrokeKind::Inside);
            if port == Port::Overset {
                let c = rect.center();
                let w = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);
                painter.line_segment([c - egui::vec2(2.0, 0.0), c + egui::vec2(2.0, 0.0)], w);
                painter.line_segment([c - egui::vec2(0.0, 2.0), c + egui::vec2(0.0, 2.0)], w);
            }
        }
    }

    /// Thread facts of the selected frame and the story editor's line list with depth.
    pub(crate) fn thread_panel(&mut self, ui: &mut egui::Ui, frame: &str, drawn: &mut Vec<&'static str>) {
        ui.checkbox(&mut self.show_threads, "Show text threads");
        drawn.push(widget::SHOW_THREADS);
        ui.push_id(widget::THREAD_INFO, |ui| match self.thread_info(frame) {
            Ok(t) => {
                ui.label(format!(
                    "frame {} of chain {}..{} | in {} | out {}{}",
                    t.text_frame_index.saturating_add(1),
                    t.start_text_frame,
                    t.end_text_frame,
                    t.in_port.code(),
                    t.out_port.code(),
                    if t.overflows { " | story overset" } else { "" }
                ));
            }
            Err(e) => {
                ui.colored_label(egui::Color32::RED, e.code());
            }
        });
        drawn.push(widget::THREAD_INFO);
        let Some(story) = self.story_of_frame(frame).map(|s| s.story_id.clone()) else { return };
        let lines = self.story_lines(&story).unwrap_or_default();
        egui::Grid::new(widget::STORY_LINES).striped(true).show(ui, |ui| {
            let mut marked = false;
            for l in &lines {
                if l.overset && !marked {
                    marked = true;
                    ui.push_id(widget::STORY_OVERSET, |ui| ui.colored_label(colour::PORT_OVERSET, "---- overset ----"));
                    ui.end_row();
                }
                ui.label(match (l.frame, l.depth_pt) {
                    (Some(f), Some(d)) => format!("f{} c{} {:.1} pt", f.saturating_add(1), l.column.saturating_add(1), d),
                    _ => "overset".to_owned(),
                });
                ui.label(&l.text);
                ui.end_row();
            }
        });
        drawn.push(widget::STORY_LINES);
        if lines.iter().any(|l| l.overset) {
            drawn.push(widget::STORY_OVERSET);
        }
    }
}
