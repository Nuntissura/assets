//! Find/change panel (lane SMB-LAYOUT-UI-2 slice 4; STU-LAY-027/129): text and GREP modes with
//! case and whole-word toggles, scope document / story / selection range, find (all hits listed),
//! next, change, change all and change/find. Matching and capture-group expansion are layout's
//! (`hsk_studio_layout::find_in_story`, the bounded linear-time engine); a change applies the hits
//! back to front through `replace_story_text` + `shift_flow_records`, ALL hits of one change-all
//! being ONE undo step. "object contains no text for find/change" is its own typed error
//! (`no_text_for_find`), never reported as no matches. Declared gaps: glyph, object, transliterate
//! and colour modes, formatting criteria, saved queries, locked/hidden layer and parent-page
//! include toggles. InDesign find/change behaviour UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use super::{LayoutDoc, LayoutEdit, LayoutEditor, LayoutRecords, LayoutUiError, story_text, widget};
use hsk_studio_layout::{FindHit, FindMode, find_in_story, replace_story_text, shift_flow_records};
use serde_json::{Value, json};

/// Where find/change looks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FindScope {
    Document,
    Story(String),
    /// Story bytes `[start, end)`.
    Selection { story: String, start: u64, end: u64 },
}

/// One query.
#[derive(Clone, Debug, PartialEq)]
pub struct FindQuery {
    pub pattern: String,
    pub mode: FindMode,
    pub scope: FindScope,
}

/// One hit in a story.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoryHit {
    pub story: String,
    pub start: u64,
    pub end: u64,
    pub replacement: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FindUi {
    pub pattern: String,
    pub change: String,
    pub grep: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub story_only: bool,
    pub hits: Vec<StoryHit>,
    pub cursor: usize,
}
/// Every hit of the query, story by story in document order (with replacements when `change`).
pub(crate) fn find_hits(d: &LayoutDoc, q: &FindQuery, change: Option<&str>) -> Result<Vec<StoryHit>, LayoutUiError> {
    let targets: Vec<(&hsk_studio_folio::StudioTextStory, Option<(u64, u64)>)> = match &q.scope {
        FindScope::Document => d.stories.iter().map(|s| (s, None)).collect(),
        FindScope::Story(id) => vec![(d.stories.iter().find(|s| s.story_id == *id).ok_or_else(|| LayoutUiError::UnknownStory(id.clone()))?, None)],
        FindScope::Selection { story, start, end } => {
            vec![(d.stories.iter().find(|s| s.story_id == *story).ok_or_else(|| LayoutUiError::UnknownStory(story.clone()))?, Some((*start, *end)))]
        }
    };
    let has_text = targets.iter().any(|(s, r)| match r {
        Some((a, b)) => a < b,
        None => !story_text(s).is_empty(),
    });
    if !has_text {
        return Err(LayoutUiError::NoTextForFind);
    }
    let mut out = Vec::new();
    for (s, r) in targets {
        let hits: Vec<FindHit> = find_in_story(s, None, &q.pattern, q.mode, r, change).map_err(|e| LayoutUiError::Grep(e.code()))?;
        out.extend(hits.into_iter().map(|h| StoryHit { story: s.story_id.clone(), start: h.start, end: h.end, replacement: h.replacement }));
    }
    Ok(out)
}

/// The state after a change: `only` = the hit starting at that story byte, else every hit.
pub(crate) fn transform_change(
    doc: &LayoutDoc,
    rec: &LayoutRecords,
    q: &FindQuery,
    replacement: &str,
    only: Option<&(String, u64)>,
) -> Result<(LayoutDoc, LayoutRecords), LayoutUiError> {
    let mut d = doc.clone();
    let mut r = rec.clone();
    let mut hits = find_hits(doc, q, Some(replacement))?;
    if let Some((story, start)) = only {
        hits.retain(|h| h.story == *story && h.start == *start);
        if hits.is_empty() {
            return Err(LayoutUiError::Layout(hsk_studio_layout::LayoutError::InvalidInput));
        }
    }
    // Back to front so earlier offsets stay valid.
    for h in hits.iter().rev() {
        let repl = h.replacement.as_deref().unwrap_or_default();
        let st = d.stories.iter().find(|s| s.story_id == h.story).ok_or_else(|| LayoutUiError::UnknownStory(h.story.clone()))?;
        let edited = replace_story_text(st, None, h.start, h.end, repl)?;
        if let Some(slot) = d.stories.iter_mut().find(|s| s.story_id == h.story) {
            *slot = edited;
        }
        shift_flow_records(&mut d.flow, &h.story, h.start, h.end, repl.len() as u64)?;
        super::tables::shift_anchors(&mut r, &h.story, h.start, h.end, repl.len() as u64);
    }
    Ok((d, r))
}

impl LayoutEditor {
    /// Every hit of `q` (no edit).
    pub fn find(&self, q: &FindQuery) -> Result<Vec<StoryHit>, LayoutUiError> {
        find_hits(&self.doc, q, None)
    }

    fn query(&self) -> FindQuery {
        let f = &self.find_ui;
        let mode = if f.grep { FindMode::Grep { case_sensitive: f.case_sensitive } } else { FindMode::Text { case_sensitive: f.case_sensitive, whole_word: f.whole_word } };
        let story = self.selected.as_deref().and_then(|fr| self.story_of_frame(fr)).map(|s| s.story_id.clone());
        let scope = match (f.story_only, story) {
            (true, Some(s)) => FindScope::Story(s),
            _ => FindScope::Document,
        };
        FindQuery { pattern: f.pattern.clone(), mode, scope }
    }

    pub(crate) fn find_json(&self) -> Value {
        json!({
            "hits": self.find_ui.hits.iter().map(|h| json!([h.story, h.start, h.end])).collect::<Vec<_>>(),
            "cursor": self.find_ui.cursor,
        })
    }

    fn run_find(&mut self) {
        match self.find(&self.query()) {
            Ok(h) => {
                self.find_ui.hits = h;
                self.find_ui.cursor = 0;
                self.last_error = None;
            }
            Err(e) => {
                self.find_ui.hits.clear();
                self.last_error = Some(e.code().to_owned());
            }
        }
    }

    /// The find/change panel.
    pub(crate) fn find_panel(&mut self, ui: &mut egui::Ui, drawn: &mut Vec<&'static str>) {
        ui.push_id(widget::FIND_PANEL, |ui| {
            ui.label("Find/Change");
            ui.horizontal(|ui| {
                ui.push_id(widget::FIND_TEXT, |ui| ui.text_edit_singleline(&mut self.find_ui.pattern));
                ui.push_id(widget::CHANGE_TEXT, |ui| ui.text_edit_singleline(&mut self.find_ui.change));
            });
            ui.push_id(widget::FIND_MODE, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.find_ui.grep, false, "Text");
                    ui.selectable_value(&mut self.find_ui.grep, true, "GREP");
                    ui.checkbox(&mut self.find_ui.case_sensitive, "Case sensitive");
                    ui.add_enabled(!self.find_ui.grep, egui::Checkbox::new(&mut self.find_ui.whole_word, "Whole word"));
                    ui.checkbox(&mut self.find_ui.story_only, "Selected story only");
                });
            });
            ui.horizontal(|ui| {
                if ui.push_id(widget::FIND_NEXT, |ui| ui.button("Find")).inner.clicked() {
                    if self.find_ui.hits.is_empty() {
                        self.run_find();
                    } else {
                        self.find_ui.cursor = self.find_ui.cursor.saturating_add(1).checked_rem(self.find_ui.hits.len()).unwrap_or(0);
                    }
                }
                let current = self.find_ui.hits.get(self.find_ui.cursor).map(|h| (h.story.clone(), h.start));
                if ui.push_id(widget::CHANGE, |ui| ui.add_enabled(current.is_some(), egui::Button::new("Change"))).inner.clicked()
                    && let Some(only) = current.clone()
                {
                    let (query, replacement) = (self.query(), self.find_ui.change.clone());
                    self.act(LayoutEdit::Change { query, replacement, only: Some(only) });
                    self.run_find();
                }
                if ui.push_id(widget::CHANGE_ALL, |ui| ui.button("Change all")).inner.clicked() {
                    let (query, replacement) = (self.query(), self.find_ui.change.clone());
                    self.act(LayoutEdit::Change { query, replacement, only: None });
                    self.find_ui.hits.clear();
                }
            });
            ui.push_id(widget::FIND_HITS, |ui| {
                for (i, h) in self.find_ui.hits.iter().enumerate().take(200) {
                    let text = self.doc.stories.iter().find(|s| s.story_id == h.story).map(story_text).unwrap_or_default();
                    let found = text.get(usize::try_from(h.start).unwrap_or(0)..usize::try_from(h.end).unwrap_or(0)).unwrap_or_default();
                    let mark = if i == self.find_ui.cursor { ">" } else { " " };
                    ui.label(format!("{mark} {}..{} {found}", h.start, h.end));
                }
            });
        });
        drawn.extend([widget::FIND_PANEL, widget::FIND_TEXT, widget::CHANGE_TEXT, widget::FIND_MODE, widget::FIND_NEXT, widget::CHANGE, widget::CHANGE_ALL, widget::FIND_HITS]);
    }
}
