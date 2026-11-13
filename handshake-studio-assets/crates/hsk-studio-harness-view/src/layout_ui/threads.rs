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
use hsk_studio_accord::CancellationToken;
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

/// [STU-LAY-123] flow modes of a loaded text cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlowMode {
    /// `flow.manual`: places one frame's worth; the cursor reloads with the remainder.
    Manual,
    /// `flow.semi_auto`: places a frame and reloads the cursor to continue, without adding pages.
    SemiAuto,
    /// `flow.auto`: adds frames and pages until the story ends.
    Auto,
    /// `flow.fixed_page_auto`: flows into existing pages only, without adding pages.
    FixedPageAuto,
}

impl FlowMode {
    /// The spec's member tokens.
    pub const ALL: [(&'static str, FlowMode); 4] = [("flow.manual", Self::Manual), ("flow.semi_auto", Self::SemiAuto), ("flow.auto", Self::Auto), ("flow.fixed_page_auto", Self::FixedPageAuto)];

    pub fn token(self) -> &'static str {
        Self::ALL.iter().find(|(_, m)| *m == self).map_or("", |(t, _)| t)
    }
}

/// Most frames one autoflow or smart reflow places; past it the flow stops with the cursor loaded
/// (`frame_limit`), so a hostile story never adds pages without bound.
pub const MAX_AUTOFLOW_FRAMES: usize = 256;

/// What [`LayoutEditor::autoflow`] and [`LayoutEditor::smart_reflow`] changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlowReport {
    /// New frame layers in flow order, each threaded after the one before.
    pub frames: Vec<String>,
    /// Pages added.
    pub pages_added: Vec<String>,
    /// Pages removed (smart reflow with `delete_empty_pages`).
    pub pages_removed: Vec<String>,
    /// Text is still overset: the cursor stays loaded with the remainder.
    pub cursor_loaded: bool,
    /// Why the flow stopped with text left: `no_page` (no page after the frame and the mode adds none) or
    /// `frame_limit` ([`MAX_AUTOFLOW_FRAMES`]).
    pub stopped: Option<&'static str>,
}

impl FlowReport {
    fn changed(&self) -> bool {
        !(self.frames.is_empty() && self.pages_added.is_empty() && self.pages_removed.is_empty())
    }
}

/// The smart text reflow controls this pass takes ([STU-LAY-123]); every one is given, none assumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReflowControls {
    /// `delete_empty_pages`: trailing pages that hold only empty frames of the story go.
    pub delete_empty_pages: bool,
    /// `preserve_facing_page_spreads`: on a facing-pages document pages are added and removed in pairs.
    pub preserve_facing_page_spreads: bool,
}

/// Autoflow and smart text reflow (lane SMB-LAYOUT-DELETE-CASCADE-1; [STU-LAY-020], [STU-LAY-123]). A new
/// frame fills its page's margin box and threads after the frame before it (`LayoutEdit::DrawFrame` and
/// `LayoutEdit::Thread` of the frame tool); a new page follows the document's last page with that page's size,
/// margins and applied parent (`PageEdit::InsertPage`): a left-hand page alone on its spread takes its partner,
/// else a new spread. Each call is ONE undo step (`layout.autoflow`, `layout.smart_reflow`) or, when nothing
/// changes, none. Bounded: at most [`MAX_AUTOFLOW_FRAMES`] frames, one composition per placed frame.
/// Declared gaps: the six reflow controls are not a stored document preference (Folio has no record for them),
/// so reflow runs when called, not on every edit (`smart_text_reflow`, `smart_text_reflow_sync`), and
/// `limit_to_primary_text_frames` and `link_text_files_when_importing` are not taken; `flow.manual` and
/// `flow.semi_auto` place the same frame here, they differ in the window's cursor only. InDesign parity of the
/// frame geometry, the page pairing and the empty-page rule is UNVERIFIED (D5).
impl LayoutEditor {
    /// Places the overset text of `from`'s story by `mode`, starting on `page` (default: the page after
    /// `from`'s page). `from` must end its thread (`already_threaded`); `unknown_frame`, `unknown_page`, and the
    /// frame tool's codes refuse with nothing changed. Text that fits already places nothing (no step).
    pub fn autoflow(&mut self, from: &str, mode: FlowMode, page: Option<&str>, _cancel: &CancellationToken) -> Result<FlowReport, LayoutUiError> {
        let story = super::story_id_of(&self.doc, from)?;
        if self.frame_link(from).and_then(|(_, n)| n).is_some() {
            return Err(LayoutUiError::AlreadyThreaded(from.to_owned()));
        }
        if let Some(p) = page
            && self.doc.document.locate_page(p).is_none()
        {
            return Err(LayoutUiError::UnknownPage(p.to_owned()));
        }
        self.as_one_step("layout.autoflow", |ed| {
            let mut report = FlowReport::default();
            ed.flow_into_frames(&story, from, mode, page.map(str::to_owned), false, &mut report)?;
            Ok(report)
        })
    }

    /// Smart text reflow of `frame`'s story, run once: while text is overset, frames and pages are added after
    /// the thread's last frame (as `flow.auto`); else, with `delete_empty_pages`, the trailing pages that hold
    /// nothing but frames of the story with no composed line are removed with their frames (the thread closes
    /// over them; never the page of the story's first frame, never the last page of the document). With
    /// `preserve_facing_page_spreads` on a facing-pages document pages come and go in pairs.
    pub fn smart_reflow(&mut self, frame: &str, controls: ReflowControls, _cancel: &CancellationToken) -> Result<FlowReport, LayoutUiError> {
        let story = super::story_id_of(&self.doc, frame)?;
        let info = self.thread_info(frame)?;
        self.as_one_step("layout.smart_reflow", |ed| {
            let mut report = FlowReport::default();
            if ed.compose(&story)?.overset.is_some() {
                ed.flow_into_frames(&story, &info.end_text_frame, FlowMode::Auto, None, controls.preserve_facing_page_spreads, &mut report)?;
            } else if controls.delete_empty_pages {
                ed.remove_empty_pages(&story, &info.start_text_frame, controls.preserve_facing_page_spreads, &mut report)?;
            }
            Ok(report)
        })
    }

    /// Runs `change` on the editor's state as ONE undo step named `name`; a refusal or a change of nothing
    /// leaves the state and the history as they were.
    fn as_one_step(&mut self, name: &'static str, change: impl FnOnce(&mut Self) -> Result<FlowReport, LayoutUiError>) -> Result<FlowReport, LayoutUiError> {
        let before = (self.doc.clone(), self.rec.clone());
        match change(self) {
            Ok(report) if report.changed() => {
                self.undo.push(before);
                self.undo_names.push(name);
                if self.undo.len() > super::MAX_HISTORY {
                    self.undo.remove(0);
                    self.undo_names.remove(0);
                }
                self.redo.clear();
                self.redo_names.clear();
                self.last_error = None;
                self.after_edit();
                Ok(report)
            }
            outcome => {
                (self.doc, self.rec) = before;
                outcome
            }
        }
    }

    /// Frames threaded after `last` while the story is overset (one only for `flow.manual` and
    /// `flow.semi_auto`), on `first_page` then the following pages; `flow.auto` adds pages past the last.
    fn flow_into_frames(&mut self, story: &str, last: &str, mode: FlowMode, first_page: Option<String>, pairs: bool, report: &mut FlowReport) -> Result<(), LayoutUiError> {
        if self.compose(story)?.overset.is_none() {
            return Ok(());
        }
        let mut last = last.to_owned();
        let mut target = first_page;
        loop {
            if report.frames.len() >= MAX_AUTOFLOW_FRAMES {
                report.stopped = Some("frame_limit");
                break;
            }
            let on = match target.take().or_else(|| self.page_after(&last)) {
                Some(p) => p,
                None if mode == FlowMode::Auto => {
                    let added = self.add_page()?;
                    report.pages_added.push(added.clone());
                    added
                }
                None => {
                    report.stopped = Some("no_page");
                    break;
                }
            };
            last = self.margin_frame(&on, &last)?;
            report.frames.push(last.clone());
            if matches!(mode, FlowMode::Manual | FlowMode::SemiAuto) || self.compose(story)?.overset.is_none() {
                break;
            }
        }
        // A facing pair stays whole: a page added alone gets its partner, with its frame of the thread.
        if pairs && self.facing() && report.pages_added.len() & 1 == 1 && report.frames.len() < MAX_AUTOFLOW_FRAMES {
            let added = self.add_page()?;
            report.pages_added.push(added.clone());
            report.frames.push(self.margin_frame(&added, &last)?);
        }
        report.cursor_loaded = self.compose(story)?.overset.is_some();
        if !report.cursor_loaded {
            report.stopped = None;
        }
        Ok(())
    }

    /// The document's pages in order.
    fn page_order(&self) -> Vec<String> {
        self.doc.document.page_spreads.iter().flat_map(|s| &s.pages).map(|p| p.page_id.clone()).collect()
    }

    /// Any page is left- or right-hand.
    fn facing(&self) -> bool {
        self.doc.document.page_spreads.iter().flat_map(|s| &s.pages).any(|p| p.side != hsk_studio_folio::PageSide::SingleSided)
    }

    fn page_of_frame(&self, frame: &str) -> Option<String> {
        self.doc.document.layers.iter().find(|l| l.layer_id == frame)?.text_frame.as_ref().map(|f| f.page_id.clone())
    }

    /// The page after `frame`'s page in document order.
    fn page_after(&self, frame: &str) -> Option<String> {
        let page = self.page_of_frame(frame)?;
        let order = self.page_order();
        let at = order.iter().position(|p| *p == page)?;
        order.get(at.checked_add(1)?).cloned()
    }

    /// A page after the document's last page (module docs): its key.
    fn add_page(&mut self) -> Result<String, LayoutUiError> {
        let spread = self.doc.document.page_spreads.last().ok_or(LayoutUiError::InvalidGeometry)?;
        let page = spread.pages.last().ok_or(LayoutUiError::InvalidGeometry)?;
        let new_spread = !(page.side == hsk_studio_folio::PageSide::LeftHand && spread.pages.len() == 1);
        let after = page.page_id.clone();
        let parent = self.rec.parents.binding(&after).and_then(|b| b.applied_parent.clone());
        let held: std::collections::BTreeSet<String> = self.page_order().into_iter().collect();
        let (doc, rec) = super::pages::transform(&self.doc, &self.rec, &super::PageEdit::InsertPage { after, new_spread, parent })?;
        (self.doc, self.rec) = (doc, rec);
        self.page_order().into_iter().find(|p| !held.contains(p)).ok_or(LayoutUiError::InvalidGeometry)
    }

    /// A frame filling `page`'s margin box, threaded after `after`: its layer id.
    fn margin_frame(&mut self, page: &str, after: &str) -> Result<String, LayoutUiError> {
        let p = self.doc.document.locate_page(page).ok_or_else(|| LayoutUiError::UnknownPage(page.to_owned()))?.page;
        let m = &p.margins;
        let draw = super::LayoutEdit::DrawFrame {
            page_id: page.to_owned(),
            x: m.left.value,
            y: m.top.value,
            w: p.width.value - m.left.value - m.right.value,
            h: p.height.value - m.top.value - m.bottom.value,
        };
        self.doc = super::transform(&self.doc, &draw)?;
        let frame = self.doc.document.layers.last().map(|l| l.layer_id.clone()).ok_or(LayoutUiError::InvalidGeometry)?;
        self.doc = super::transform(&self.doc, &super::LayoutEdit::Thread { from: after.to_owned(), to: frame.clone() })?;
        Ok(frame)
    }

    /// The trailing pages of `story` that hold nothing but its frames with no composed line, removed
    /// (`PageEdit::DeletePage`: the frames go, the thread closes).
    fn remove_empty_pages(&mut self, story: &str, first: &str, pairs: bool, report: &mut FlowReport) -> Result<(), LayoutUiError> {
        let proposal = self.compose(story)?;
        let filled: std::collections::BTreeSet<&str> = proposal.frames.iter().filter(|f| !f.lines.is_empty()).map(|f| f.frame_id.as_str()).collect();
        let keep = self.page_of_frame(first);
        let graphic: std::collections::BTreeSet<&str> = self.rec.longdoc.graphic_frames.iter().map(|g| g.page_id.as_str()).collect();
        let shows = |l: &hsk_studio_folio::StudioLayer| matches!(&l.payload, Payload::Text { story_id } if story_id == story);
        let order = self.page_order();
        let mut doomed: Vec<String> = Vec::new();
        for page in order.iter().rev() {
            if Some(page) == keep.as_ref() || doomed.len().saturating_add(1) >= order.len() {
                break;
            }
            let on: Vec<&hsk_studio_folio::StudioLayer> = self.doc.document.layers.iter().filter(|l| l.text_frame.as_ref().is_some_and(|f| f.page_id == *page)).collect();
            let empty = !on.is_empty() && on.iter().all(|&l| shows(l) && !filled.contains(l.layer_id.as_str())) && !graphic.contains(page.as_str());
            if !empty {
                break;
            }
            doomed.push(page.clone());
        }
        if pairs && self.facing() && doomed.len() & 1 == 1 {
            doomed.pop();
        }
        for page in doomed {
            let (doc, rec) = super::pages::transform(&self.doc, &self.rec, &super::PageEdit::DeletePage { page: page.clone() })?;
            (self.doc, self.rec) = (doc, rec);
            report.pages_removed.push(page);
        }
        Ok(())
    }
}
