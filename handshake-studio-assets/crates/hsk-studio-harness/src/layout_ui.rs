//! Layout editing surface (lane SMB-LAYOUT-UI slice 4; STU-LAY-017/018/021/026/122): a page view of
//! the spreads with their text frames, columns, composed lines and thread connections; a text-frame
//! tool (draw a frame, thread two frames, set columns and gutter); a story editor (the story as
//! plain text, one row per paragraph with the paragraph style name in the margin and an overset
//! marker); paragraph and character style application. Feature `layout-ui` (part of `viewer`),
//! window-free: [`LayoutEditor::ui`] draws into any `egui::Context` and every action is a method
//! taking a [`LayoutEdit`], so tests drive it headlessly (`tests/viewer_layout_ui.rs`).
//!
//! Lane SMB-LAYOUT-UI-2 adds the InDesign-parity panels on the same editor (each in its own
//! submodule, each action an edit of the same undo history, each dispatching the existing Folio and
//! layout engine commands): pages and parent pages ([`pages`]), thread ports and the story
//! editor's line list with depth and overset rows ([`threads`]), paragraph and character styles
//! with GREP rules ([`styles`]), tables ([`tables`]) and find/change in text and GREP modes
//! ([`find`]).
//!
//! History: every successful [`LayoutEditor::apply`] is exactly ONE undo step (a snapshot of the
//! whole layout state: document, stories, layout-flow and type-style records); a refused edit
//! leaves the state and the history untouched. Undo/redo restore the snapshots. The history is
//! bounded (`MAX_HISTORY`, oldest steps dropped). Persisting layout edits through the session and
//! chronicle is a declared gap of this slice (the state is the caller's to save).
//!
//! Composition preview: paragraphs are shaped with a fixed advance of 0.5 em, line height 1.25 em
//! and ascent 0.75 em of the paragraph style's point size (default 12 pt): a placeholder for the
//! editor only (PDF export shapes through hsk-studio-type); flow, columns, threads, styles' letter
//! spacing and overset are layout's own (`hsk_studio_layout::reflow_flow`).
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    AutoSizing, Bounds, FirstBaseline, GeometryUnit, LayerSchema, LayoutEdges, Length, NodeKind, NodeRef, Parent,
    Payload, StoryContent, StoryDirection, StoryParagraph, StoryRange, StoryTextKind, StudioDocument, StudioLayer,
    StudioLayoutFlowRecords, StudioLongDocRecords, StudioParentPageRecords, StudioTextFrame, StudioTextStory, StudioTypeStyleRecords,
    TextStorySchema, TypeStyleScope, VerticalJustification,
};
use hsk_studio_layout::{
    Composer, LayoutError, LayoutProposal, Limits, ReflowRequest, ShapedParagraph, SourceSpan, SpaceParams, StyleSource,
    TypesetOptions, reflow_flow, replace_story_text, shift_flow_records,
};
use serde_json::{Value, json};

pub mod pages;
pub use pages::{PageEdit, PageRow};
pub mod threads;
pub use threads::{Port, StoryLine, ThreadInfo};
pub mod styles;
pub use styles::{StyleEdit, StylePatch, StyleRow};
pub mod tables;
pub use tables::TableEdit;
pub mod find;
pub use find::{FindQuery, FindScope, StoryHit};

/// Undo steps kept (the oldest is dropped beyond this).
pub const MAX_HISTORY: usize = 256;
/// Columns the frame tool accepts.
pub const MAX_COLUMNS: u32 = 64;

/// Stable widget ids of the layout panel.
pub mod widget {
    pub const PANEL: &str = "layout";
    pub const PAGE_VIEW: &str = "layout.pages";
    pub const TOOL_SELECT: &str = "layout.tool.select";
    pub const TOOL_FRAME: &str = "layout.tool.frame";
    pub const TOOL_THREAD: &str = "layout.tool.thread";
    pub const UNDO: &str = "layout.undo";
    pub const REDO: &str = "layout.redo";
    pub const COLUMNS: &str = "layout.frame.columns";
    pub const GUTTER: &str = "layout.frame.gutter";
    pub const APPLY_COLUMNS: &str = "layout.frame.apply_columns";
    pub const UNTHREAD: &str = "layout.frame.unthread";
    pub const STORY_EDITOR: &str = "layout.story";
    pub const STORY_TEXT: &str = "layout.story.text";
    pub const COMMIT_TEXT: &str = "layout.story.commit";
    pub const STYLE_COLUMN: &str = "layout.story.styles";
    pub const CHAR_STYLE: &str = "layout.style.character";
    pub const APPLY_CHAR_STYLE: &str = "layout.style.character.apply";
    pub const ERROR: &str = "layout.error";
    // Lane SMB-LAYOUT-UI-2: pages and parent pages panel.
    pub const PAGES_PANEL: &str = "layout.pages_panel";
    pub const PAGE_INSERT: &str = "layout.pages_panel.insert";
    pub const PAGE_INSERT_SPREAD: &str = "layout.pages_panel.insert_spread";
    pub const PAGE_DELETE: &str = "layout.pages_panel.delete";
    pub const PAGE_MOVE_UP: &str = "layout.pages_panel.move_up";
    pub const PAGE_MOVE_DOWN: &str = "layout.pages_panel.move_down";
    pub const PARENT_LIST: &str = "layout.pages_panel.parents";
    pub const PARENT_NEW: &str = "layout.pages_panel.parent_new";
    pub const PARENT_APPLY: &str = "layout.pages_panel.parent_apply";
    pub const PARENT_ITEMS: &str = "layout.pages_panel.parent_items";
    // Slice 2: threads and story editor lines.
    pub const SHOW_THREADS: &str = "layout.threads.show";
    pub const THREAD_INFO: &str = "layout.frame.thread_info";
    pub const STORY_LINES: &str = "layout.story.lines";
    pub const STORY_OVERSET: &str = "layout.story.overset";
    // Slice 3: styles panels.
    pub const STYLES_PANEL: &str = "layout.styles";
    pub const PARA_STYLES: &str = "layout.styles.paragraph";
    pub const CHAR_STYLES: &str = "layout.styles.character";
    pub const STYLE_EDIT: &str = "layout.styles.edit";
    pub const GREP_RULES: &str = "layout.styles.grep";
    pub const CLEAR_OVERRIDES: &str = "layout.styles.clear_overrides";
    // Slice 4: tables and find/change.
    pub const TABLES_PANEL: &str = "layout.tables";
    pub const TABLE_INSERT: &str = "layout.tables.insert";
    pub const TABLE_CELL: &str = "layout.tables.cell";
    pub const FIND_PANEL: &str = "layout.find";
    pub const FIND_TEXT: &str = "layout.find.text";
    pub const CHANGE_TEXT: &str = "layout.find.change_text";
    pub const FIND_MODE: &str = "layout.find.mode";
    pub const FIND_NEXT: &str = "layout.find.next";
    pub const CHANGE: &str = "layout.find.change";
    pub const CHANGE_ALL: &str = "layout.find.change_all";
    pub const FIND_HITS: &str = "layout.find.hits";
}

/// Everything the editor edits.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutDoc {
    pub document: StudioDocument,
    pub stories: Vec<StudioTextStory>,
    pub flow: StudioLayoutFlowRecords,
    pub styles: StudioTypeStyleRecords,
}

/// Record sets beside the document that the SMB-LAYOUT-UI-2 panels edit (same undo history).
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutRecords {
    /// Parent pages and per-page parent bindings (Folio `hsk.studio.parent_page_records@1`).
    pub parents: StudioParentPageRecords,
    /// Tables (and the other long-document records) beside the document (Folio
    /// `hsk.studio.longdoc_records@1`).
    pub longdoc: StudioLongDocRecords,
}
impl LayoutRecords {
    /// Empty record sets for `document_id`.
    pub fn new(document_id: &str) -> Self {
        Self { parents: StudioParentPageRecords::new(document_id), longdoc: StudioLongDocRecords::new(document_id) }
    }
}

/// One editing command = one undo step.
#[derive(Clone, Debug, PartialEq)]
pub enum LayoutEdit {
    /// A new empty text frame (and its new empty story) on a page, page coordinates in pt.
    DrawFrame { page_id: String, x: f64, y: f64, w: f64, h: f64 },
    /// Thread `from` (end of its chain) into `to` (an empty, unthreaded frame); `to` joins the story.
    Thread { from: String, to: String },
    /// Cut the thread after `from`; the frames after the cut get a new empty story.
    Unthread { from: String },
    SetColumns { frame: String, count: u32, gutter: f64 },
    ReplaceText { story: String, start: u64, end: u64, text: String },
    /// `None` clears the paragraph's style.
    ApplyParagraphStyle { story: String, paragraph: usize, style: Option<String> },
    /// `None` clears the range's character style.
    ApplyCharacterStyle { story: String, start: u64, end: u64, style: Option<String> },
    /// Pages and parent pages panel (SMB-LAYOUT-UI-2).
    Page(PageEdit),
    /// Paragraph and character styles panels (SMB-LAYOUT-UI-2).
    Style(StyleEdit),
    /// Tables panel (SMB-LAYOUT-UI-2).
    Table(TableEdit),
    /// Find/change: `only` = the hit starting at (story, byte), else every hit (one undo step).
    Change { query: FindQuery, replacement: String, only: Option<(String, u64)> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutUiError {
    Layout(LayoutError),
    UnknownPage(String),
    UnknownFrame(String),
    UnknownStory(String),
    UnknownStyle(String),
    WrongStyleScope(String),
    /// The link would close a loop.
    ThreadCycle,
    /// `from` already has a next frame or `to` a previous one.
    AlreadyThreaded(String),
    /// Threading into a frame whose story has text (merging stories is a declared gap).
    TargetNotEmpty(String),
    NotThreaded(String),
    InvalidGeometry,
    IdSpaceExhausted,
    /// A Folio record validator refused the result (`folio_<code>`, target).
    Folio(&'static str, String),
    /// The parent adapter refused the records (its code).
    Parent(&'static str),
    /// Deleting the last page of the document.
    LastPage,
    UnknownParent(String),
    /// The page applies no parent holding this item.
    UnknownParentItem(String),
    /// "cannot unlink an item that was not overridden" (STU-LAY-007).
    ItemNotOverridden(String),
    /// The item is already overridden on this page.
    AlreadyOverridden(String),
    /// A parent named empty, too long or with control characters.
    InvalidName,
    /// A STU-LAY-143 style constraint (its own code).
    Style(&'static str),
    /// The bounded pattern engine refused a pattern (its code).
    Grep(&'static str),
    /// STU-LAY-129 "object contains no text for find/change" (never "no matches").
    NoTextForFind,
    UnknownTable(String),
    /// A table grid or composition refusal (its code).
    Table(&'static str),
}
impl LayoutUiError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Layout(e) => e.code(),
            Self::UnknownPage(_) => "unknown_page",
            Self::UnknownFrame(_) => "unknown_frame",
            Self::UnknownStory(_) => "unknown_story",
            Self::UnknownStyle(_) => "unknown_style",
            Self::WrongStyleScope(_) => "wrong_style_scope",
            Self::ThreadCycle => "thread_cycle",
            Self::AlreadyThreaded(_) => "already_threaded",
            Self::TargetNotEmpty(_) => "thread_target_not_empty",
            Self::NotThreaded(_) => "not_threaded",
            Self::InvalidGeometry => "invalid_geometry",
            Self::IdSpaceExhausted => "id_space_exhausted",
            Self::Folio(code, _) => code,
            Self::Parent(code) => code,
            Self::LastPage => "last_page",
            Self::UnknownParent(_) => "unknown_parent",
            Self::UnknownParentItem(_) => "unknown_parent_item",
            Self::ItemNotOverridden(_) => "item_not_overridden",
            Self::AlreadyOverridden(_) => "already_overridden",
            Self::InvalidName => "invalid_name",
            Self::Style(code) | Self::Grep(code) | Self::Table(code) => code,
            Self::NoTextForFind => "no_text_for_find",
            Self::UnknownTable(_) => "unknown_table",
        }
    }
}
impl From<LayoutError> for LayoutUiError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}

/// The active tool of the page view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    #[default]
    Select,
    Frame,
    Thread,
}

/// One story-editor row.
#[derive(Clone, Debug, PartialEq)]
pub struct StoryRow {
    pub paragraph: usize,
    pub start: u64,
    pub end: u64,
    /// Name of the paragraph style (the margin column), `None` = no style.
    pub style_name: Option<String>,
    pub text: String,
    /// Part of the paragraph is overset.
    pub overset: bool,
}

pub(crate) fn len(v: f64) -> Length {
    Length { value: v, unit: GeometryUnit::Pt }
}
pub(crate) fn story_text(s: &StudioTextStory) -> &str {
    match &s.content {
        StoryContent::Inline { text_utf8 } => text_utf8,
        StoryContent::Stored { .. } => "",
    }
}

/// (width, height, [(page id, page x)]) of one spread.
type SpreadGeo = (f64, f64, Vec<(String, f64)>);

/// Window-free layout editor state.
pub struct LayoutEditor {
    doc: LayoutDoc,
    rec: LayoutRecords,
    undo: Vec<(LayoutDoc, LayoutRecords)>,
    redo: Vec<(LayoutDoc, LayoutRecords)>,
    pub tool: Tool,
    pub selected: Option<String>,
    thread_from: Option<String>,
    drag_start: Option<(usize, f64, f64)>,
    columns: u32,
    gutter: f64,
    draft: String,
    draft_story: Option<String>,
    char_range: (u64, u64),
    char_style: Option<String>,
    pub last_error: Option<String>,
    /// Pages panel selection (page key).
    pub selected_page: Option<String>,
    pub(crate) pages_ui: pages::PagesUi,
    /// Threads-view overlay (flow order lines between frames).
    pub show_threads: bool,
    pub(crate) styles_ui: styles::StylesUi,
    pub(crate) tables_ui: tables::TablesUi,
    pub(crate) find_ui: find::FindUi,
}

impl LayoutEditor {
    pub fn new(doc: LayoutDoc) -> Self {
        let rec = LayoutRecords::new(&doc.document.document_id);
        Self::with_records(doc, rec)
    }
    /// An editor over a document and its SMB-LAYOUT-UI-2 record sets.
    pub fn with_records(doc: LayoutDoc, rec: LayoutRecords) -> Self {
        Self {
            doc,
            rec,
            undo: Vec::new(),
            redo: Vec::new(),
            tool: Tool::Select,
            selected: None,
            thread_from: None,
            drag_start: None,
            columns: 1,
            gutter: 12.0,
            draft: String::new(),
            draft_story: None,
            char_range: (0, 0),
            char_style: None,
            last_error: None,
            selected_page: None,
            pages_ui: pages::PagesUi::default(),
            show_threads: true,
            styles_ui: styles::StylesUi::default(),
            tables_ui: tables::TablesUi { rows: 2, columns: 2, ..tables::TablesUi::default() },
            find_ui: find::FindUi::default(),
        }
    }
    pub fn doc(&self) -> &LayoutDoc {
        &self.doc
    }
    pub fn records(&self) -> &LayoutRecords {
        &self.rec
    }
    /// (undo steps, redo steps).
    pub fn history_len(&self) -> (usize, usize) {
        (self.undo.len(), self.redo.len())
    }

    /// Applies one edit as ONE undo step; a refusal changes nothing.
    pub fn apply(&mut self, edit: LayoutEdit) -> Result<(), LayoutUiError> {
        let (next, next_rec) = match &edit {
            LayoutEdit::Page(e) => pages::transform(&self.doc, &self.rec, e)?,
            LayoutEdit::Style(e) => (styles::transform(&self.doc, e)?, self.rec.clone()),
            LayoutEdit::Table(e) => tables::transform(&self.doc, &self.rec, e)?,
            LayoutEdit::Change { query, replacement, only } => find::transform_change(&self.doc, &self.rec, query, replacement, only.as_ref())?,
            _ => {
                let mut rec = self.rec.clone();
                if let LayoutEdit::ReplaceText { story, start, end, text } = &edit {
                    tables::shift_anchors(&mut rec, story, *start, *end, text.len() as u64);
                }
                (transform(&self.doc, &edit)?, rec)
            }
        };
        let before = std::mem::replace(&mut self.doc, next);
        let before_rec = std::mem::replace(&mut self.rec, next_rec);
        self.undo.push((before, before_rec));
        if self.undo.len() > MAX_HISTORY {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.last_error = None;
        if let LayoutEdit::DrawFrame { .. } = edit {
            self.selected = self.doc.document.layers.last().map(|l| l.layer_id.clone());
        }
        self.after_edit();
        Ok(())
    }
    pub fn undo(&mut self) -> bool {
        match self.undo.pop() {
            Some((prev, prev_rec)) => {
                let now = std::mem::replace(&mut self.doc, prev);
                let now_rec = std::mem::replace(&mut self.rec, prev_rec);
                self.redo.push((now, now_rec));
                self.draft_story = None;
                self.after_edit();
                true
            }
            None => false,
        }
    }
    pub fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some((next, next_rec)) => {
                let now = std::mem::replace(&mut self.doc, next);
                let now_rec = std::mem::replace(&mut self.rec, next_rec);
                self.undo.push((now, now_rec));
                self.draft_story = None;
                self.after_edit();
                true
            }
            None => false,
        }
    }

    /// Drops selections that no longer exist after an edit, undo or redo.
    fn after_edit(&mut self) {
        if self.selected.as_ref().is_some_and(|f| !self.doc.document.layers.iter().any(|l| l.layer_id == *f)) {
            self.selected = None;
        }
        if self.selected_page.as_ref().is_some_and(|p| self.doc.document.locate_page(p).is_none()) {
            self.selected_page = None;
        }
    }

    /// Story of a frame layer.
    pub fn story_of_frame(&self, frame: &str) -> Option<&StudioTextStory> {
        let layer = self.doc.document.layers.iter().find(|l| l.layer_id == frame)?;
        let Payload::Text { story_id } = &layer.payload else { return None };
        self.doc.stories.iter().find(|s| s.story_id == *story_id)
    }

    /// Composes one story with the preview shaping (module docs).
    pub fn compose(&self, story_id: &str) -> Result<LayoutProposal, LayoutUiError> {
        if self.rec.longdoc.tables.iter().any(|t| t.story_id == story_id) {
            return Ok(self.compose_tables(story_id)?.text);
        }
        let st = self
            .doc
            .stories
            .iter()
            .find(|s| s.story_id == story_id)
            .ok_or_else(|| LayoutUiError::UnknownStory(story_id.to_owned()))?;
        let paras = preview_paragraphs(st, &self.doc.styles)?;
        let req = ReflowRequest {
            document: &self.doc.document,
            story: st,
            resolved_text: None,
            paragraphs: &paras,
            obstacles: &[],
            composer: Composer::FirstFit,
            space: SpaceParams::default(),
            expected_revision: self.doc.document.revision,
            limits: Limits::default(),
        };
        let opts = TypesetOptions {
            styles: Some(StyleSource { records: &self.doc.styles, default_font_size_pt: 12.0 }),
            ..TypesetOptions::default()
        };
        Ok(reflow_flow(&req, &opts, &self.doc.flow, &CancellationToken::default())?)
    }

    /// Story-editor rows (paragraph style names in the margin, overset marker).
    pub fn story_rows(&self, story_id: &str) -> Result<Vec<StoryRow>, LayoutUiError> {
        let st = self
            .doc
            .stories
            .iter()
            .find(|s| s.story_id == story_id)
            .ok_or_else(|| LayoutUiError::UnknownStory(story_id.to_owned()))?;
        let overset = self.compose(story_id).ok().and_then(|p| p.overset);
        let text = story_text(st);
        Ok(st
            .paragraphs
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let body = text
                    .get(usize::try_from(p.start).unwrap_or(usize::MAX)..usize::try_from(p.end).unwrap_or(usize::MAX))
                    .unwrap_or_default()
                    .trim_end_matches('\n')
                    .to_owned();
                StoryRow {
                    paragraph: i,
                    start: p.start,
                    end: p.end,
                    style_name: p
                        .style_ref
                        .as_deref()
                        .and_then(|s| self.doc.styles.style(s))
                        .map(|s| s.name.clone()),
                    text: body,
                    overset: overset.is_some_and(|o| o.start < p.end.max(p.start.saturating_add(1))),
                }
            })
            .collect())
    }

    /// Structured projection for models and tests.
    pub fn state_json(&self) -> Value {
        let frames: Vec<Value> = self
            .doc
            .document
            .layers
            .iter()
            .filter_map(|l| {
                let f = l.text_frame.as_ref()?;
                let story = match &l.payload {
                    Payload::Text { story_id } => story_id.clone(),
                    _ => String::new(),
                };
                Some(json!({
                    "frame_id": l.layer_id, "page_id": f.page_id, "story_id": story,
                    "bounds": [f.bounds.x.value, f.bounds.y.value, f.bounds.width.value, f.bounds.height.value],
                    "columns": f.text_column_count, "gutter": f.text_column_gutter.value,
                    "previous": f.previous_text_frame, "next": f.next_text_frame,
                    "thread": self.threads_json(&l.layer_id),
                }))
            })
            .collect();
        let stories: Vec<Value> = self
            .doc
            .stories
            .iter()
            .map(|s| {
                let placed = self.compose(&s.story_id);
                json!({
                    "story_id": s.story_id,
                    "bytes": story_text(s).len(),
                    "paragraphs": s.paragraphs.len(),
                    "overset": placed.as_ref().ok().and_then(|p| p.overset.map(|o| [o.start, o.end])),
                    "refusal": placed.as_ref().err().map(|e| e.code()),
                    "lines": placed.as_ref().map_or(0, |p| p.frames.iter().map(|f| f.lines.len()).sum::<usize>()),
                })
            })
            .collect();
        json!({
            "schema": "hsk.studio.harness.layout_ui@1",
            "tool": format!("{:?}", self.tool).to_lowercase(),
            "selected": self.selected,
            "history": {"undo": self.undo.len(), "redo": self.redo.len()},
            "frames": frames,
            "stories": stories,
            "pages": self.pages_json(),
            "parents": self.parents_json(),
            "styles": self.styles_json(),
            "tables": self.tables_json(),
            "find": self.find_json(),
            "error": self.last_error,
        })
    }

    pub(crate) fn act(&mut self, edit: LayoutEdit) {
        if let Err(e) = self.apply(edit) {
            self.last_error = Some(e.code().to_owned());
        }
    }

    /// Draws one frame of the panel; returns the widget ids drawn.
    pub fn ui(&mut self, ctx: &egui::Context) -> Vec<&'static str> {
        let mut drawn = vec![widget::PANEL];
        egui::Window::new("Layout").id(egui::Id::new(widget::PANEL)).show(ctx, |ui| {
            ui.horizontal(|ui| {
                for (tool, id, label) in [
                    (Tool::Select, widget::TOOL_SELECT, "Select"),
                    (Tool::Frame, widget::TOOL_FRAME, "Text frame"),
                    (Tool::Thread, widget::TOOL_THREAD, "Thread"),
                ] {
                    if ui.push_id(id, |ui| ui.selectable_label(self.tool == tool, label)).inner.clicked() {
                        self.tool = tool;
                        self.thread_from = None;
                    }
                    drawn.push(id);
                }
                if ui.push_id(widget::UNDO, |ui| ui.button("Undo")).inner.clicked() {
                    self.undo();
                }
                drawn.push(widget::UNDO);
                if ui.push_id(widget::REDO, |ui| ui.button("Redo")).inner.clicked() {
                    self.redo();
                }
                drawn.push(widget::REDO);
            });
            self.pages_panel(ui, &mut drawn);
            self.page_view(ui);
            drawn.push(widget::PAGE_VIEW);
            if let Some(frame) = self.selected.clone() {
                ui.horizontal(|ui| {
                    ui.push_id(widget::COLUMNS, |ui| ui.add(egui::DragValue::new(&mut self.columns).range(1..=MAX_COLUMNS).prefix("columns ")));
                    ui.push_id(widget::GUTTER, |ui| ui.add(egui::DragValue::new(&mut self.gutter).range(0.0..=1000.0).prefix("gutter ")));
                    if ui.push_id(widget::APPLY_COLUMNS, |ui| ui.button("Apply")).inner.clicked() {
                        self.act(LayoutEdit::SetColumns { frame: frame.clone(), count: self.columns, gutter: self.gutter });
                    }
                    if ui.push_id(widget::UNTHREAD, |ui| ui.button("Unthread")).inner.clicked() {
                        self.act(LayoutEdit::Unthread { from: frame.clone() });
                    }
                });
                drawn.extend([widget::COLUMNS, widget::GUTTER, widget::APPLY_COLUMNS, widget::UNTHREAD]);
                self.thread_panel(ui, &frame, &mut drawn);
                self.story_editor(ui, &frame, &mut drawn);
            }
            self.styles_panel(ui, &mut drawn);
            self.tables_panel(ui, &mut drawn);
            self.find_panel(ui, &mut drawn);
            if let Some(e) = &self.last_error {
                ui.push_id(widget::ERROR, |ui| ui.colored_label(egui::Color32::RED, e.as_str()));
                drawn.push(widget::ERROR);
            }
        });
        drawn
    }

    /// Display-only geometry (egui vector maths over finite document lengths).
    #[allow(clippy::arithmetic_side_effects)]
    fn page_view(&mut self, ui: &mut egui::Ui) {
        const SCALE: f32 = 1.0;
        let spreads: Vec<SpreadGeo> = self
            .doc
            .document
            .page_spreads
            .iter()
            .map(|s| {
                let mut x = 0.0;
                let pages = s
                    .pages
                    .iter()
                    .map(|p| {
                        let at = x;
                        x += p.width.value;
                        (p.page_id.clone(), at)
                    })
                    .collect();
                (x, s.pages.iter().map(|p| p.height.value).fold(0.0, f64::max), pages)
            })
            .collect();
        let width = spreads.iter().map(|s| s.0).fold(1.0, f64::max) as f32 * SCALE;
        let height = spreads.iter().map(|s| s.1 + 16.0).sum::<f64>().max(1.0) as f32 * SCALE;
        let (resp, painter) = ui.allocate_painter(egui::vec2(width, height), egui::Sense::click_and_drag());
        let origin = resp.rect.min;
        let mut top = 0.0f64;
        let mut spread_tops = Vec::new();
        for (i, s) in self.doc.document.page_spreads.iter().enumerate() {
            spread_tops.push(top);
            for (p, (_, x)) in s.pages.iter().zip(&spreads[i].2) {
                let r = egui::Rect::from_min_size(
                    origin + egui::vec2(*x as f32, top as f32) * SCALE,
                    egui::vec2(p.width.value as f32, p.height.value as f32) * SCALE,
                );
                painter.rect_filled(r, 0.0, egui::Color32::WHITE);
                painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0_f32, egui::Color32::GRAY), egui::StrokeKind::Inside);
            }
            top += spreads[i].1 + 16.0;
        }
        // Frames, columns, lines.
        let place = |page_id: &str| -> Option<(f64, f64)> {
            for (i, s) in spreads.iter().enumerate() {
                if let Some((_, x)) = s.2.iter().find(|(p, _)| p == page_id) {
                    return Some((*x, spread_tops[i]));
                }
            }
            None
        };
        let parent_layers = pages::parent_item_layers(&self.rec.parents);
        for (page_id, items) in self.parent_overlays() {
            let Some((px, py)) = place(&page_id) else { continue };
            for r in items {
                let rr = egui::Rect::from_min_size(
                    origin + egui::vec2((px + r.x) as f32, (py + r.y) as f32) * SCALE,
                    egui::vec2(r.w as f32, r.h as f32) * SCALE,
                );
                painter.rect_stroke(rr, 0.0, egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(200, 120, 0)), egui::StrokeKind::Inside);
            }
        }
        for l in &self.doc.document.layers {
            if parent_layers.contains(l.layer_id.as_str()) {
                continue;
            }
            let Some(f) = &l.text_frame else { continue };
            let Some((px, py)) = place(&f.page_id) else { continue };
            let r = egui::Rect::from_min_size(
                origin + egui::vec2((px + f.bounds.x.value) as f32, (py + f.bounds.y.value) as f32) * SCALE,
                egui::vec2(f.bounds.width.value as f32, f.bounds.height.value as f32) * SCALE,
            );
            let selected = self.selected.as_deref() == Some(l.layer_id.as_str());
            let colour = if selected { egui::Color32::from_rgb(0, 120, 215) } else { egui::Color32::from_rgb(120, 120, 200) };
            painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0_f32, colour), egui::StrokeKind::Inside);
            self.draw_ports(&painter, r, &l.layer_id);
        }
        for st in &self.doc.stories {
            let Ok(p) = self.compose(&st.story_id) else { continue };
            let text = story_text(st);
            let mut previous: Option<egui::Pos2> = None;
            for fp in &p.frames {
                let spread_top = spread_tops.get(fp.spread).copied().unwrap_or(0.0);
                let fo = origin + egui::vec2(fp.origin_x_pt as f32, (spread_top + fp.origin_y_pt) as f32) * SCALE;
                if self.show_threads
                    && let Some(prev) = previous
                {
                    painter.line_segment([prev, fo], egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(0, 160, 0)));
                }
                for line in &fp.lines {
                    let s = usize::try_from(line.source.start).unwrap_or(0);
                    let e = usize::try_from(line.source.end).unwrap_or(0);
                    let t = text.get(s..e).unwrap_or_default();
                    painter.text(
                        fo + egui::vec2(line.x_start_pt as f32, line.baseline_y_pt as f32) * SCALE,
                        egui::Align2::LEFT_BOTTOM,
                        t,
                        egui::FontId::monospace(10.0),
                        egui::Color32::BLACK,
                    );
                }
                previous = fp.lines.last().map(|l| fo + egui::vec2((l.x_start_pt + l.width_pt) as f32, l.baseline_y_pt as f32) * SCALE);
            }
        }
        for (spread, x, y, w, h) in self.table_cell_rects() {
            let top = spread_tops.get(spread).copied().unwrap_or(0.0);
            let rr = egui::Rect::from_min_size(origin + egui::vec2(x as f32, (top + y) as f32) * SCALE, egui::vec2(w as f32, h as f32) * SCALE);
            painter.rect_stroke(rr, 0.0, egui::Stroke::new(1.0_f32, tables::TABLE_CELL_COLOUR), egui::StrokeKind::Inside);
        }
        // Interaction: page coordinates of the pointer.
        let to_page = |pos: egui::Pos2| -> Option<(usize, String, f64, f64)> {
            let v = (pos - origin) / SCALE;
            for (i, s) in spreads.iter().enumerate() {
                let y = f64::from(v.y) - spread_tops[i];
                if (0.0..=s.1).contains(&y) {
                    for (k, (pid, x)) in s.2.iter().enumerate() {
                        let w = self.doc.document.page_spreads[i].pages[k].width.value;
                        let lx = f64::from(v.x) - x;
                        if (0.0..=w).contains(&lx) {
                            return Some((i, pid.clone(), lx, y));
                        }
                    }
                }
            }
            None
        };
        match self.tool {
            Tool::Frame => {
                if resp.drag_started()
                    && let Some(pos) = resp.interact_pointer_pos()
                    && let Some((s, _, x, y)) = to_page(pos)
                {
                    self.drag_start = Some((s, x, y));
                }
                if resp.drag_stopped()
                    && let (Some((_, x0, y0)), Some(pos)) = (self.drag_start.take(), resp.interact_pointer_pos())
                    && let Some((_, page, x1, y1)) = to_page(pos)
                {
                    self.act(LayoutEdit::DrawFrame { page_id: page, x: x0.min(x1), y: y0.min(y1), w: (x1 - x0).abs(), h: (y1 - y0).abs() });
                }
            }
            Tool::Select | Tool::Thread => {
                if resp.clicked()
                    && let Some(pos) = resp.interact_pointer_pos()
                    && let Some((_, page, x, y)) = to_page(pos)
                {
                    let hit = self.frame_at(&page, x, y);
                    if self.tool == Tool::Thread {
                        match (self.thread_from.take(), hit) {
                            (Some(from), Some(to)) => self.act(LayoutEdit::Thread { from, to }),
                            (None, Some(f)) => self.thread_from = Some(f),
                            _ => {}
                        }
                    } else {
                        self.selected = hit;
                    }
                }
            }
        }
    }

    /// Topmost text frame at a page point.
    pub fn frame_at(&self, page_id: &str, x: f64, y: f64) -> Option<String> {
        let parent_layers = pages::parent_item_layers(&self.rec.parents);
        self.doc.document.layers.iter().rev().find_map(|l| {
            if parent_layers.contains(l.layer_id.as_str()) {
                return None;
            }
            let f = l.text_frame.as_ref()?;
            let b = &f.bounds;
            (f.page_id == page_id
                && x >= b.x.value
                && x <= b.x.value + b.width.value
                && y >= b.y.value
                && y <= b.y.value + b.height.value)
                .then(|| l.layer_id.clone())
        })
    }

    fn story_editor(&mut self, ui: &mut egui::Ui, frame: &str, drawn: &mut Vec<&'static str>) {
        let Some(st) = self.story_of_frame(frame).cloned() else { return };
        if self.draft_story.as_deref() != Some(st.story_id.as_str()) {
            self.draft = story_text(&st).to_owned();
            self.draft_story = Some(st.story_id.clone());
        }
        ui.push_id(widget::STORY_EDITOR, |ui| {
            ui.label("Story editor");
            let rows = self.story_rows(&st.story_id).unwrap_or_default();
            let names: Vec<(String, String)> = self
                .doc
                .styles
                .styles
                .iter()
                .filter(|s| s.scope == TypeStyleScope::Paragraph)
                .map(|s| (s.style_id.clone(), s.name.clone()))
                .collect();
            egui::Grid::new(widget::STYLE_COLUMN).show(ui, |ui| {
                for r in &rows {
                    let mut chosen = st.paragraphs[r.paragraph].style_ref.clone();
                    egui::ComboBox::from_id_salt((widget::STYLE_COLUMN, r.paragraph))
                        .selected_text(r.style_name.clone().unwrap_or_else(|| "[none]".into()))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut chosen, None, "[none]");
                            for (id, name) in &names {
                                ui.selectable_value(&mut chosen, Some(id.clone()), name);
                            }
                        });
                    ui.label(if r.overset { format!("{} [overset]", r.text) } else { r.text.clone() });
                    ui.end_row();
                    if chosen != st.paragraphs[r.paragraph].style_ref {
                        self.act(LayoutEdit::ApplyParagraphStyle { story: st.story_id.clone(), paragraph: r.paragraph, style: chosen });
                    }
                }
            });
            ui.push_id(widget::STORY_TEXT, |ui| ui.add(egui::TextEdit::multiline(&mut self.draft).code_editor()));
            if ui.push_id(widget::COMMIT_TEXT, |ui| ui.button("Commit text")).inner.clicked() {
                let end = story_text(&st).len() as u64;
                let text = self.draft.clone();
                self.act(LayoutEdit::ReplaceText { story: st.story_id.clone(), start: 0, end, text });
            }
            ui.horizontal(|ui| {
                ui.push_id(widget::CHAR_STYLE, |ui| {
                    ui.add(egui::DragValue::new(&mut self.char_range.0).prefix("from "));
                    ui.add(egui::DragValue::new(&mut self.char_range.1).prefix("to "));
                    let chars: Vec<(String, String)> = self
                        .doc
                        .styles
                        .styles
                        .iter()
                        .filter(|s| s.scope == TypeStyleScope::Character)
                        .map(|s| (s.style_id.clone(), s.name.clone()))
                        .collect();
                    egui::ComboBox::from_id_salt(widget::CHAR_STYLE)
                        .selected_text(self.char_style.clone().unwrap_or_else(|| "[none]".into()))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.char_style, None, "[none]");
                            for (id, name) in &chars {
                                ui.selectable_value(&mut self.char_style, Some(id.clone()), name);
                            }
                        });
                });
                if ui.push_id(widget::APPLY_CHAR_STYLE, |ui| ui.button("Apply character style")).inner.clicked() {
                    let (start, end) = self.char_range;
                    self.act(LayoutEdit::ApplyCharacterStyle { story: st.story_id.clone(), start, end, style: self.char_style.clone() });
                }
            });
        });
        drawn.extend([widget::STORY_EDITOR, widget::STYLE_COLUMN, widget::STORY_TEXT, widget::COMMIT_TEXT, widget::CHAR_STYLE, widget::APPLY_CHAR_STYLE]);
    }
}

/// Preview shaping (module docs): one fixed-advance paragraph per story paragraph, newline excluded.
pub fn preview_paragraphs(st: &StudioTextStory, styles: &StudioTypeStyleRecords) -> Result<Vec<ShapedParagraph>, LayoutUiError> {
    let text = story_text(st);
    st.paragraphs
        .iter()
        .map(|p| {
            let size = p
                .style_ref
                .as_deref()
                .and_then(|s| hsk_studio_layout::resolve_style(styles, s, TypeStyleScope::Paragraph).ok())
                .and_then(|r| r.font_size_pt)
                .unwrap_or(12.0);
            let body_end = if text.as_bytes().get(usize::try_from(p.end).unwrap_or(usize::MAX).wrapping_sub(1)) == Some(&b'\n') && p.end > p.start {
                p.end.saturating_sub(1)
            } else {
                p.end
            };
            ShapedParagraph::from_fixed_advance(text, SourceSpan { start: p.start, end: body_end }, size * 0.5, size * 1.25, size * 0.75)
                .ok_or(LayoutUiError::Layout(LayoutError::InvalidInput))
        })
        .collect()
}

pub(crate) fn mint(doc: &LayoutDoc, prefix: &str) -> Result<String, LayoutUiError> {
    for n in 1u64..=1_000_000 {
        let id = format!("{prefix}-019a5a10-0000-7000-8000-{n:012x}");
        let used = doc.document.layers.iter().any(|l| l.layer_id == id) || doc.stories.iter().any(|s| s.story_id == id);
        if !used {
            return Ok(id);
        }
    }
    Err(LayoutUiError::IdSpaceExhausted)
}

pub(crate) fn empty_story(id: String) -> StudioTextStory {
    StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: id,
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: String::new() },
        paragraphs: vec![StoryParagraph { start: 0, end: 0, style_ref: None, paragraph_direction: None }],
        character_ranges: vec![],
    }
}

fn frame_mut<'a>(doc: &'a mut LayoutDoc, id: &str) -> Result<&'a mut StudioTextFrame, LayoutUiError> {
    doc.document
        .layers
        .iter_mut()
        .find(|l| l.layer_id == id)
        .and_then(|l| l.text_frame.as_mut())
        .ok_or_else(|| LayoutUiError::UnknownFrame(id.to_owned()))
}

fn story_id_of(doc: &LayoutDoc, frame: &str) -> Result<String, LayoutUiError> {
    let l = doc
        .document
        .layers
        .iter()
        .find(|l| l.layer_id == frame && l.text_frame.is_some())
        .ok_or_else(|| LayoutUiError::UnknownFrame(frame.to_owned()))?;
    match &l.payload {
        Payload::Text { story_id } => Ok(story_id.clone()),
        _ => Err(LayoutUiError::UnknownFrame(frame.to_owned())),
    }
}

fn story_mut<'a>(doc: &'a mut LayoutDoc, id: &str) -> Result<&'a mut StudioTextStory, LayoutUiError> {
    doc.stories
        .iter_mut()
        .find(|s| s.story_id == id)
        .ok_or_else(|| LayoutUiError::UnknownStory(id.to_owned()))
}

fn check_style(doc: &LayoutDoc, style: &Option<String>, scope: TypeStyleScope) -> Result<(), LayoutUiError> {
    if let Some(s) = style {
        let found = doc.styles.style(s).ok_or_else(|| LayoutUiError::UnknownStyle(s.clone()))?;
        if found.scope != scope {
            return Err(LayoutUiError::WrongStyleScope(s.clone()));
        }
    }
    Ok(())
}

/// The state after one edit (pure; the caller swaps it in).
fn transform(doc: &LayoutDoc, edit: &LayoutEdit) -> Result<LayoutDoc, LayoutUiError> {
    let mut d = doc.clone();
    match edit {
        LayoutEdit::DrawFrame { page_id, x, y, w, h } => {
            if ![*x, *y, *w, *h].iter().all(|v| v.is_finite()) || *w <= 0.0 || *h <= 0.0 {
                return Err(LayoutUiError::InvalidGeometry);
            }
            let spread = d
                .document
                .page_spreads
                .iter()
                .find(|s| s.pages.iter().any(|p| p.page_id == *page_id))
                .ok_or_else(|| LayoutUiError::UnknownPage(page_id.clone()))?;
            let spread_id = spread.page_spread_id.clone();
            let layer_id = mint(&d, "SLYR")?;
            let story_id = mint(&d, "STXT")?;
            d.document.layers.push(StudioLayer {
                schema_id: LayerSchema::V1,
                layer_id: layer_id.clone(),
                name: "text frame".into(),
                parent: Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: spread_id }),
                kind: "text".into(),
                payload: Payload::Text { story_id: story_id.clone() },
                visible: true,
                appearance: None,
                text_frame: Some(StudioTextFrame {
                    page_id: page_id.clone(),
                    bounds: Bounds { x: len(*x), y: len(*y), width: len(*w), height: len(*h) },
                    inset_spacing: LayoutEdges { top: len(0.0), right: len(0.0), bottom: len(0.0), left: len(0.0) },
                    text_column_count: 1,
                    text_column_gutter: len(12.0),
                    first_baseline_offset: FirstBaseline::Fixed,
                    minimum_first_baseline_offset: len(10.0_f64.min(*h)),
                    vertical_justification: VerticalJustification::Top,
                    auto_sizing_type: AutoSizing::Off,
                    previous_text_frame: None,
                    next_text_frame: None,
                }),
            });
            d.stories.push(empty_story(story_id));
        }
        LayoutEdit::Thread { from, to } => {
            if from == to {
                return Err(LayoutUiError::ThreadCycle);
            }
            let (fs, ts) = (story_id_of(&d, from)?, story_id_of(&d, to)?);
            if frame_mut(&mut d, from)?.next_text_frame.is_some() {
                return Err(LayoutUiError::AlreadyThreaded(from.clone()));
            }
            if frame_mut(&mut d, to)?.previous_text_frame.is_some() {
                return Err(LayoutUiError::AlreadyThreaded(to.clone()));
            }
            // Walking forward from `to` must not reach `from` (a loop).
            let mut at = Some(to.clone());
            let mut steps = 0usize;
            while let Some(f) = at {
                if f == *from {
                    return Err(LayoutUiError::ThreadCycle);
                }
                steps = steps.checked_add(1).ok_or(LayoutUiError::ThreadCycle)?;
                if steps > d.document.layers.len() {
                    return Err(LayoutUiError::ThreadCycle);
                }
                at = frame_mut(&mut d, &f)?.next_text_frame.clone();
            }
            if fs != ts {
                let shown_elsewhere = d
                    .document
                    .layers
                    .iter()
                    .filter(|l| matches!(&l.payload, Payload::Text { story_id } if *story_id == ts))
                    .count()
                    > 1;
                let target = d.stories.iter().find(|s| s.story_id == ts);
                if shown_elsewhere || target.is_some_and(|s| !story_text(s).is_empty()) {
                    return Err(LayoutUiError::TargetNotEmpty(to.clone()));
                }
                d.stories.retain(|s| s.story_id != ts);
                if let Some(l) = d.document.layers.iter_mut().find(|l| l.layer_id == *to) {
                    l.payload = Payload::Text { story_id: fs };
                }
            }
            frame_mut(&mut d, from)?.next_text_frame = Some(to.clone());
            frame_mut(&mut d, to)?.previous_text_frame = Some(from.clone());
        }
        LayoutEdit::Unthread { from } => {
            let next = frame_mut(&mut d, from)?
                .next_text_frame
                .take()
                .ok_or_else(|| LayoutUiError::NotThreaded(from.clone()))?;
            frame_mut(&mut d, &next)?.previous_text_frame = None;
            let new_story = mint(&d, "STXT")?;
            let mut at = Some(next);
            let mut steps = 0usize;
            while let Some(f) = at {
                steps = steps.checked_add(1).ok_or(LayoutUiError::ThreadCycle)?;
                if steps > d.document.layers.len() {
                    return Err(LayoutUiError::ThreadCycle);
                }
                if let Some(l) = d.document.layers.iter_mut().find(|l| l.layer_id == f) {
                    l.payload = Payload::Text { story_id: new_story.clone() };
                }
                at = frame_mut(&mut d, &f)?.next_text_frame.clone();
            }
            d.stories.push(empty_story(new_story));
        }
        LayoutEdit::SetColumns { frame, count, gutter } => {
            if !(1..=MAX_COLUMNS).contains(count) || !gutter.is_finite() || *gutter < 0.0 {
                return Err(LayoutUiError::InvalidGeometry);
            }
            let f = frame_mut(&mut d, frame)?;
            let n = f64::from(*count);
            if f.bounds.width.value - (n - 1.0) * gutter <= 0.0 {
                return Err(LayoutUiError::InvalidGeometry);
            }
            f.text_column_count = *count;
            f.text_column_gutter = len(*gutter);
        }
        LayoutEdit::ReplaceText { story, start, end, text } => {
            let st = d.stories.iter().find(|s| s.story_id == *story).ok_or_else(|| LayoutUiError::UnknownStory(story.clone()))?;
            let edited = replace_story_text(st, None, *start, *end, text)?;
            *story_mut(&mut d, story)? = edited;
            shift_flow_records(&mut d.flow, story, *start, *end, text.len() as u64)?;
        }
        LayoutEdit::ApplyParagraphStyle { story, paragraph, style } => {
            check_style(&d, style, TypeStyleScope::Paragraph)?;
            let st = story_mut(&mut d, story)?;
            let p = st.paragraphs.get_mut(*paragraph).ok_or(LayoutUiError::Layout(LayoutError::InvalidInput))?;
            p.style_ref = style.clone();
        }
        LayoutEdit::ApplyCharacterStyle { story, start, end, style } => {
            check_style(&d, style, TypeStyleScope::Character)?;
            let st = story_mut(&mut d, story)?;
            let text = story_text(st);
            let (s, e) = (usize::try_from(*start).unwrap_or(usize::MAX), usize::try_from(*end).unwrap_or(usize::MAX));
            if start >= end || e > text.len() || !text.is_char_boundary(s) || !text.is_char_boundary(e) {
                return Err(LayoutUiError::Layout(LayoutError::InvalidInput));
            }
            let total = text.len() as u64;
            let base = if st.character_ranges.is_empty() {
                vec![StoryRange { start: 0, end: total, style_ref: None, applied_language: None }]
            } else {
                st.character_ranges.clone()
            };
            let mut out = Vec::with_capacity(base.len().saturating_add(2));
            for r in base {
                for (a, b, inside) in [(r.start, r.end.min(*start), false), (r.start.max(*start), r.end.min(*end), true), (r.start.max(*end), r.end, false)] {
                    if a < b {
                        let style_ref = if inside { style.clone() } else { r.style_ref.clone() };
                        match out.last_mut() {
                            Some(StoryRange { end: le, style_ref: ls, applied_language: ll, .. })
                                if *le == a && *ls == style_ref && *ll == r.applied_language =>
                            {
                                *le = b
                            }
                            _ => out.push(StoryRange { start: a, end: b, style_ref, applied_language: r.applied_language.clone() }),
                        }
                    }
                }
            }
            st.character_ranges = out;
        }
        // Panel edits (SMB-LAYOUT-UI-2) are dispatched by `LayoutEditor::apply` to their submodules.
        _ => return Err(LayoutUiError::Layout(LayoutError::InvalidInput)),
    }
    Ok(d)
}
