//! Paragraph and character styles panels (lane SMB-LAYOUT-UI-2 slice 3; STU-LAY-036/037/038/143):
//! the style lists of both scopes with usage counts and an override indicator, new style (based on
//! another), edit (name, based on, next style, font family, size, leading, alignment), delete with
//! a replacement style, GREP rules on paragraph styles (add/remove), and clear overrides (character
//! ranges inside a paragraph go back to the paragraph style). Applying styles stays the story
//! editor's (`ApplyParagraphStyle` / `ApplyCharacterStyle`). Every result is checked by Folio's
//! `validate_type_style_records` (references, scopes, `based_on` cycles) and GREP patterns by
//! layout's bounded engine (`compile_find`, the same limits as GREP styles); effective values come
//! from layout's `resolve_style`.
//!
//! STU-LAY-143 names constraint failures as distinct typed errors; the name rules no engine checks
//! yet are refused here with their own codes: `style_name_empty`, `style_name_duplicate` (unique per
//! scope), `style_name_bracket`, `style_name_too_long` (> 128 bytes); a `based_on` loop is
//! `style_based_on_cycle`. Declared gaps: nested and line styles (STU-LAY-038: the engine has none),
//! style groups, redefine from selection, break link, cross-document load, reserved names.
//! InDesign style-panel behaviour UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use super::{LayoutDoc, LayoutEdit, LayoutEditor, LayoutUiError, len, widget};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    Code, StoryRange, StudioGrepStyle, StudioLineHeight, StudioTextAlign, StudioTypeStyle, TypeStyleSchema, TypeStyleScope,
    Unresolved, validate_type_style_records,
};
use hsk_studio_layout::{FindMode, compile_find, resolve_style};
use serde_json::{Value, json};

/// Longest style name accepted.
pub const MAX_STYLE_NAME_BYTES: usize = 128;

/// Changed fields of a style (`None` = unchanged; `Some(None)` = cleared).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StylePatch {
    pub name: Option<String>,
    pub based_on: Option<Option<String>>,
    pub next_style: Option<Option<String>>,
    pub font_family: Option<Option<String>>,
    pub font_size_pt: Option<Option<f64>>,
    pub leading_pt: Option<Option<f64>>,
    pub align: Option<Option<StudioTextAlign>>,
}

/// One styles-panel command.
#[derive(Clone, Debug, PartialEq)]
pub enum StyleEdit {
    NewStyle { scope: TypeStyleScope, name: String, based_on: Option<String> },
    EditStyle { style: String, patch: StylePatch },
    /// Deletes a style; every use (paragraphs, ranges, `based_on`, `next_style`, GREP rules) takes
    /// `replace_with` (same scope) or no style.
    DeleteStyle { style: String, replace_with: Option<String> },
    AddGrepRule { style: String, pattern: String, character_style: String },
    RemoveGrepRule { style: String, index: usize },
    /// Character ranges inside the paragraph lose their character style.
    ClearOverrides { story: String, paragraph: usize },
}

/// One row of a styles panel.
#[derive(Clone, Debug, PartialEq)]
pub struct StyleRow {
    pub style_id: String,
    pub name: String,
    pub scope: TypeStyleScope,
    pub based_on: Option<String>,
    /// Paragraphs (paragraph scope) or character ranges (character scope) using it.
    pub uses: usize,
    pub grep_rules: usize,
    /// Effective size from the based-on chain (layout `resolve_style`).
    pub font_size_pt: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct StylesUi {
    pub selected: Option<String>,
    pub new_name: String,
    pub size: f64,
    pub grep_pattern: String,
    pub grep_style: Option<String>,
    pub paragraph: usize,
}

fn mint_style(d: &LayoutDoc) -> Result<String, LayoutUiError> {
    for n in 1u64..=1_000_000 {
        let id = format!("STYS-019a5a10-0000-7000-8000-{n:012x}");
        if d.styles.style(&id).is_none() {
            return Ok(id);
        }
    }
    Err(LayoutUiError::IdSpaceExhausted)
}

fn check_name(d: &LayoutDoc, name: &str, scope: TypeStyleScope, except: Option<&str>) -> Result<(), LayoutUiError> {
    if name.trim().is_empty() {
        return Err(LayoutUiError::Style("style_name_empty"));
    }
    if name.len() > MAX_STYLE_NAME_BYTES || name.chars().any(char::is_control) {
        return Err(LayoutUiError::Style("style_name_too_long"));
    }
    if name.contains(['[', ']']) {
        return Err(LayoutUiError::Style("style_name_bracket"));
    }
    if d.styles.styles.iter().any(|s| s.scope == scope && s.name == name && Some(s.style_id.as_str()) != except) {
        return Err(LayoutUiError::Style("style_name_duplicate"));
    }
    Ok(())
}

fn style_mut<'a>(d: &'a mut LayoutDoc, id: &str) -> Result<&'a mut StudioTypeStyle, LayoutUiError> {
    d.styles.styles.iter_mut().find(|s| s.style_id == id).ok_or_else(|| LayoutUiError::UnknownStyle(id.to_owned()))
}

fn validate(d: &LayoutDoc) -> Result<(), LayoutUiError> {
    validate_type_style_records(&d.styles, &d.document, hsk_studio_folio::Budget::default(), &CancellationToken::default(), &Unresolved)
        .map(|_| ())
        .map_err(|e| match e.code {
            Code::RenderCycle => LayoutUiError::Style("style_based_on_cycle"),
            Code::PayloadMismatch => LayoutUiError::WrongStyleScope(e.target),
            Code::DanglingReference => LayoutUiError::UnknownStyle(e.target),
            _ => LayoutUiError::Folio("folio_invalid_style", e.target),
        })
}

/// The state after one styles edit (pure; Folio-validated).
pub(crate) fn transform(doc: &LayoutDoc, edit: &StyleEdit) -> Result<LayoutDoc, LayoutUiError> {
    let mut d = doc.clone();
    match edit {
        StyleEdit::NewStyle { scope, name, based_on } => {
            check_name(&d, name, *scope, None)?;
            let style_id = mint_style(&d)?;
            d.styles.styles.push(StudioTypeStyle {
                schema_id: TypeStyleSchema::V1,
                style_id,
                scope: *scope,
                name: name.clone(),
                based_on: based_on.clone(),
                next_style: None,
                font_family: None,
                font_style: None,
                font_size: None,
                fill: None,
                line_height: None,
                letter_spacing: None,
                align: None,
                grep_styles: Vec::new(),
            });
        }
        StyleEdit::EditStyle { style, patch } => {
            let scope = d.styles.style(style).map(|s| s.scope).ok_or_else(|| LayoutUiError::UnknownStyle(style.clone()))?;
            if let Some(n) = &patch.name {
                check_name(&d, n, scope, Some(style))?;
            }
            for v in [patch.font_size_pt, patch.leading_pt].into_iter().flatten().flatten() {
                if !v.is_finite() || v <= 0.0 || v > 1296.0 {
                    return Err(LayoutUiError::InvalidGeometry);
                }
            }
            let s = style_mut(&mut d, style)?;
            if let Some(n) = &patch.name {
                s.name = n.clone();
            }
            if let Some(b) = &patch.based_on {
                s.based_on = b.clone();
            }
            if let Some(n) = &patch.next_style {
                s.next_style = n.clone();
            }
            if let Some(f) = &patch.font_family {
                s.font_family = f.clone();
            }
            if let Some(v) = patch.font_size_pt {
                s.font_size = v.map(len);
            }
            if let Some(v) = patch.leading_pt {
                s.line_height = v.map(|v| StudioLineHeight::Length { length: len(v) });
            }
            if let Some(a) = patch.align {
                s.align = a;
            }
        }
        StyleEdit::DeleteStyle { style, replace_with } => {
            let scope = d.styles.style(style).map(|s| s.scope).ok_or_else(|| LayoutUiError::UnknownStyle(style.clone()))?;
            if let Some(r) = replace_with {
                let found = d.styles.style(r).ok_or_else(|| LayoutUiError::UnknownStyle(r.clone()))?;
                if found.scope != scope {
                    return Err(LayoutUiError::WrongStyleScope(r.clone()));
                }
                if r == style {
                    return Err(LayoutUiError::Style("style_replace_with_itself"));
                }
            }
            let parent = d.styles.style(style).and_then(|s| s.based_on.clone());
            d.styles.styles.retain(|s| s.style_id != *style);
            let swap = |v: &mut Option<String>, with: &Option<String>| {
                if v.as_deref() == Some(style.as_str()) {
                    *v = with.clone();
                }
            };
            for s in &mut d.styles.styles {
                // Children keep inheriting from the deleted style's own parent.
                swap(&mut s.based_on, &parent);
                swap(&mut s.next_style, replace_with);
                if scope == TypeStyleScope::Character {
                    match replace_with {
                        Some(r) => s.grep_styles.iter_mut().filter(|g| g.character_style == *style).for_each(|g| g.character_style = r.clone()),
                        None => s.grep_styles.retain(|g| g.character_style != *style),
                    }
                }
            }
            for st in &mut d.stories {
                st.paragraphs.iter_mut().for_each(|p| swap(&mut p.style_ref, replace_with));
                st.character_ranges.iter_mut().for_each(|r| swap(&mut r.style_ref, replace_with));
            }
        }
        StyleEdit::AddGrepRule { style, pattern, character_style } => {
            compile_find(pattern, FindMode::Grep { case_sensitive: true }).map_err(|e| LayoutUiError::Grep(e.code()))?;
            let s = style_mut(&mut d, style)?;
            if s.scope != TypeStyleScope::Paragraph {
                return Err(LayoutUiError::WrongStyleScope(style.clone()));
            }
            if s.grep_styles.len() >= hsk_studio_folio::MAX_GREP_STYLES {
                return Err(LayoutUiError::Layout(hsk_studio_layout::LayoutError::BudgetExceeded));
            }
            s.grep_styles.push(StudioGrepStyle { pattern: pattern.clone(), character_style: character_style.clone() });
        }
        StyleEdit::RemoveGrepRule { style, index } => {
            let s = style_mut(&mut d, style)?;
            if *index >= s.grep_styles.len() {
                return Err(LayoutUiError::Layout(hsk_studio_layout::LayoutError::InvalidInput));
            }
            s.grep_styles.remove(*index);
        }
        StyleEdit::ClearOverrides { story, paragraph } => {
            let st = d.stories.iter_mut().find(|s| s.story_id == *story).ok_or_else(|| LayoutUiError::UnknownStory(story.clone()))?;
            let p = st.paragraphs.get(*paragraph).ok_or(LayoutUiError::Layout(hsk_studio_layout::LayoutError::InvalidInput))?;
            let (a, b) = (p.start, p.end);
            let mut out: Vec<StoryRange> = Vec::with_capacity(st.character_ranges.len().saturating_add(2));
            for r in &st.character_ranges {
                for (s, e, inside) in [(r.start, r.end.min(a), false), (r.start.max(a), r.end.min(b), true), (r.start.max(b), r.end, false)] {
                    if s < e {
                        let style_ref = if inside { None } else { r.style_ref.clone() };
                        match out.last_mut() {
                            Some(last) if last.end == s && last.style_ref == style_ref && last.applied_language == r.applied_language => last.end = e,
                            _ => out.push(StoryRange { start: s, end: e, style_ref, applied_language: r.applied_language.clone() }),
                        }
                    }
                }
            }
            st.character_ranges = out;
        }
    }
    validate(&d)?;
    Ok(d)
}

impl LayoutEditor {
    /// Style rows of one scope, in set order.
    pub fn style_rows(&self, scope: TypeStyleScope) -> Vec<StyleRow> {
        self.doc
            .styles
            .styles
            .iter()
            .filter(|s| s.scope == scope)
            .map(|s| {
                let uses = self
                    .doc
                    .stories
                    .iter()
                    .map(|st| match scope {
                        TypeStyleScope::Paragraph => st.paragraphs.iter().filter(|p| p.style_ref.as_deref() == Some(s.style_id.as_str())).count(),
                        TypeStyleScope::Character => st.character_ranges.iter().filter(|r| r.style_ref.as_deref() == Some(s.style_id.as_str())).count(),
                    })
                    .fold(0usize, usize::saturating_add);
                StyleRow {
                    style_id: s.style_id.clone(),
                    name: s.name.clone(),
                    scope,
                    based_on: s.based_on.clone(),
                    uses,
                    grep_rules: s.grep_styles.len(),
                    font_size_pt: resolve_style(&self.doc.styles, &s.style_id, scope).ok().and_then(|r| r.font_size_pt),
                }
            })
            .collect()
    }

    /// Selects a style in the panel (its name and effective size fill the edit fields).
    pub fn select_style(&mut self, style: &str) {
        if let Some(r) = [TypeStyleScope::Paragraph, TypeStyleScope::Character].into_iter().flat_map(|s| self.style_rows(s)).find(|r| r.style_id == style) {
            self.styles_ui.new_name = r.name.clone();
            self.styles_ui.size = r.font_size_pt.unwrap_or(12.0);
            self.styles_ui.selected = Some(r.style_id);
        }
    }

    /// Paragraphs of a story whose text carries a character style (the override indicator `+`).
    pub fn paragraph_overrides(&self, story: &str) -> Vec<bool> {
        let Some(st) = self.doc.stories.iter().find(|s| s.story_id == story) else { return Vec::new() };
        st.paragraphs
            .iter()
            .map(|p| st.character_ranges.iter().any(|r| r.style_ref.is_some() && r.start < p.end && p.start < r.end))
            .collect()
    }

    pub(crate) fn styles_json(&self) -> Value {
        let rows = |scope| {
            self.style_rows(scope)
                .into_iter()
                .map(|r| json!({"style_id": r.style_id, "name": r.name, "based_on": r.based_on, "uses": r.uses, "grep_rules": r.grep_rules, "font_size_pt": r.font_size_pt}))
                .collect::<Vec<_>>()
        };
        json!({"paragraph": rows(TypeStyleScope::Paragraph), "character": rows(TypeStyleScope::Character)})
    }

    /// The paragraph- and character-styles panel.
    pub(crate) fn styles_panel(&mut self, ui: &mut egui::Ui, drawn: &mut Vec<&'static str>) {
        ui.push_id(widget::STYLES_PANEL, |ui| {
            ui.label("Paragraph styles | Character styles");
            ui.columns(2, |cols| {
                for (col, scope, id) in [(0usize, TypeStyleScope::Paragraph, widget::PARA_STYLES), (1, TypeStyleScope::Character, widget::CHAR_STYLES)] {
                    let rows = self.style_rows(scope);
                    cols[col].push_id(id, |ui| {
                        for r in rows {
                            let label = format!("{} ({}){}", r.name, r.uses, if r.grep_rules > 0 { " GREP" } else { "" });
                            if ui.selectable_label(self.styles_ui.selected.as_deref() == Some(r.style_id.as_str()), label).clicked() {
                                self.styles_ui.selected = Some(r.style_id.clone());
                                self.styles_ui.new_name = r.name.clone();
                                self.styles_ui.size = r.font_size_pt.unwrap_or(12.0);
                            }
                        }
                        if ui.button("New").clicked() {
                            let base = self.styles_ui.selected.clone().filter(|s| self.doc.styles.style(s).is_some_and(|x| x.scope == scope));
                            let name = format!("{} {}", if scope == TypeStyleScope::Paragraph { "Paragraph Style" } else { "Character Style" }, self.doc.styles.styles.len().saturating_add(1));
                            self.act(LayoutEdit::Style(StyleEdit::NewStyle { scope, name, based_on: base }));
                        }
                    });
                }
            });
            drawn.extend([widget::STYLES_PANEL, widget::PARA_STYLES, widget::CHAR_STYLES]);
            let Some(sel) = self.styles_ui.selected.clone().filter(|s| self.doc.styles.style(s).is_some()) else { return };
            let style = self.doc.styles.style(&sel).cloned();
            ui.push_id(widget::STYLE_EDIT, |ui| {
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.styles_ui.new_name);
                    ui.add(egui::DragValue::new(&mut self.styles_ui.size).range(1.0..=1296.0).suffix(" pt"));
                    if ui.button("Save").clicked() {
                        let patch = StylePatch { name: Some(self.styles_ui.new_name.clone()), font_size_pt: Some(Some(self.styles_ui.size)), ..StylePatch::default() };
                        self.act(LayoutEdit::Style(StyleEdit::EditStyle { style: sel.clone(), patch }));
                    }
                    if ui.button("Delete").clicked() {
                        self.act(LayoutEdit::Style(StyleEdit::DeleteStyle { style: sel.clone(), replace_with: None }));
                    }
                });
            });
            drawn.push(widget::STYLE_EDIT);
            if style.as_ref().is_some_and(|s| s.scope == TypeStyleScope::Paragraph) {
                ui.push_id(widget::GREP_RULES, |ui| {
                    let rules = style.as_ref().map(|s| s.grep_styles.clone()).unwrap_or_default();
                    for (i, g) in rules.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let cs = self.doc.styles.style(&g.character_style).map_or("?", |s| s.name.as_str()).to_owned();
                            ui.label(format!("{} -> {}", g.pattern, cs));
                            if ui.button("Remove").clicked() {
                                self.act(LayoutEdit::Style(StyleEdit::RemoveGrepRule { style: sel.clone(), index: i }));
                            }
                        });
                    }
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.styles_ui.grep_pattern);
                        let chars: Vec<(String, String)> = self.style_rows(TypeStyleScope::Character).into_iter().map(|r| (r.style_id, r.name)).collect();
                        egui::ComboBox::from_id_salt("grep_style")
                            .selected_text(self.styles_ui.grep_style.as_deref().and_then(|s| self.doc.styles.style(s)).map_or("[character style]".to_owned(), |s| s.name.clone()))
                            .show_ui(ui, |ui| {
                                for (id, name) in &chars {
                                    ui.selectable_value(&mut self.styles_ui.grep_style, Some(id.clone()), name);
                                }
                            });
                        if ui.button("Add GREP style").clicked()
                            && let Some(cs) = self.styles_ui.grep_style.clone()
                        {
                            let pattern = self.styles_ui.grep_pattern.clone();
                            self.act(LayoutEdit::Style(StyleEdit::AddGrepRule { style: sel.clone(), pattern, character_style: cs }));
                        }
                    });
                });
                drawn.push(widget::GREP_RULES);
            }
            if let Some(frame) = self.selected.clone()
                && let Some(story) = self.story_of_frame(&frame).map(|s| s.story_id.clone())
            {
                ui.push_id(widget::CLEAR_OVERRIDES, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut self.styles_ui.paragraph).prefix("paragraph "));
                        if ui.button("Clear overrides").clicked() {
                            self.act(LayoutEdit::Style(StyleEdit::ClearOverrides { story, paragraph: self.styles_ui.paragraph }));
                        }
                    });
                });
                drawn.push(widget::CLEAR_OVERRIDES);
            }
        });
    }
}
