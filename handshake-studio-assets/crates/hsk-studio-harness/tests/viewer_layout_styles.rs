//! Lane SMB-LAYOUT-UI-2 slice 3: paragraph and character styles panels (STU-LAY-036/037/038/143),
//! headless (fixture in `layout_ui_fixture`: Body = STYS 1 paragraph, Num = STYS 2 character).
//! InDesign style-panel behaviour UNVERIFIED (D5).
#![cfg(feature = "layout-ui")]
mod layout_ui_fixture;
use hsk_studio_folio::TypeStyleScope;
use hsk_studio_harness::layout_ui::{LayoutEdit, LayoutEditor, StyleEdit, StylePatch, widget};
use layout_ui_fixture::{doc, id};

fn se(e: StyleEdit) -> LayoutEdit {
    LayoutEdit::Style(e)
}

#[test]
fn styles_create_edit_grep_delete_and_constraints_are_typed() {
    let original = doc();
    let mut ed = LayoutEditor::new(original.clone());
    // Heading based on Body; Body 10 pt, Heading inherits it then overrides to 20 pt.
    ed.apply(se(StyleEdit::NewStyle { scope: TypeStyleScope::Paragraph, name: "Heading".into(), based_on: Some(id("STYS", 1)) })).unwrap();
    let heading = ed.style_rows(TypeStyleScope::Paragraph)[1].style_id.clone();
    ed.apply(se(StyleEdit::EditStyle { style: id("STYS", 1), patch: StylePatch { font_size_pt: Some(Some(10.0)), ..StylePatch::default() } })).unwrap();
    assert_eq!(ed.style_rows(TypeStyleScope::Paragraph)[1].font_size_pt, Some(10.0), "inherited through based_on (layout resolve_style)");
    ed.apply(se(StyleEdit::EditStyle { style: heading.clone(), patch: StylePatch { font_size_pt: Some(Some(20.0)), next_style: Some(Some(id("STYS", 1))), ..StylePatch::default() } })).unwrap();
    assert_eq!(ed.style_rows(TypeStyleScope::Paragraph)[1].font_size_pt, Some(20.0));
    // Applying the 20 pt style changes the composition (preview advance 10 pt): fewer glyphs per line.
    ed.apply(LayoutEdit::ApplyParagraphStyle { story: id("STXT", 1), paragraph: 0, style: Some(heading.clone()) }).unwrap();
    assert_eq!(ed.style_rows(TypeStyleScope::Paragraph)[1].uses, 1);
    assert_eq!(ed.compose(&id("STXT", 1)).unwrap().frames[0].lines[0].source.end, 10);

    // GREP style on Heading: digits get Num; the pattern engine refuses look-around with its code.
    ed.apply(se(StyleEdit::AddGrepRule { style: heading.clone(), pattern: r"\d+".into(), character_style: id("STYS", 2) })).unwrap();
    assert_eq!(ed.style_rows(TypeStyleScope::Paragraph)[1].grep_rules, 1);
    let steps = ed.history_len().0;
    let last = ed.doc().clone();
    let refused = [
        (se(StyleEdit::AddGrepRule { style: heading.clone(), pattern: r"(?<=a)b".into(), character_style: id("STYS", 2) }), "grep_pattern_unsupported"),
        (se(StyleEdit::AddGrepRule { style: heading.clone(), pattern: "(".into(), character_style: id("STYS", 2) }), "grep_pattern_invalid"),
        (se(StyleEdit::AddGrepRule { style: id("STYS", 2), pattern: "a".into(), character_style: id("STYS", 2) }), "wrong_style_scope"),
        (se(StyleEdit::AddGrepRule { style: heading.clone(), pattern: "a".into(), character_style: id("STYS", 1) }), "wrong_style_scope"),
        // STU-LAY-143: each constraint is its own typed error.
        (se(StyleEdit::NewStyle { scope: TypeStyleScope::Paragraph, name: " ".into(), based_on: None }), "style_name_empty"),
        (se(StyleEdit::NewStyle { scope: TypeStyleScope::Paragraph, name: "Body".into(), based_on: None }), "style_name_duplicate"),
        (se(StyleEdit::NewStyle { scope: TypeStyleScope::Paragraph, name: "[Basic]".into(), based_on: None }), "style_name_bracket"),
        (se(StyleEdit::NewStyle { scope: TypeStyleScope::Paragraph, name: "x".repeat(129), based_on: None }), "style_name_too_long"),
        (se(StyleEdit::EditStyle { style: id("STYS", 1), patch: StylePatch { based_on: Some(Some(heading.clone())), ..StylePatch::default() } }), "style_based_on_cycle"),
        (se(StyleEdit::EditStyle { style: id("STYS", 1), patch: StylePatch { font_size_pt: Some(Some(f64::NAN)), ..StylePatch::default() } }), "invalid_geometry"),
        (se(StyleEdit::DeleteStyle { style: id("STYS", 1), replace_with: Some(id("STYS", 2)) }), "wrong_style_scope"),
    ];
    for (edit, code) in refused {
        assert_eq!(ed.apply(edit).unwrap_err().code(), code);
        assert_eq!((ed.doc(), ed.history_len().0), (&last, steps));
    }
    // A character style of the same name as a paragraph style is fine (unique per scope).
    ed.apply(se(StyleEdit::NewStyle { scope: TypeStyleScope::Character, name: "Body".into(), based_on: None })).unwrap();

    // Overrides: a character range in paragraph 0, cleared back to the paragraph style.
    ed.apply(LayoutEdit::ApplyCharacterStyle { story: id("STXT", 1), start: 0, end: 4, style: Some(id("STYS", 2)) }).unwrap();
    assert_eq!(ed.paragraph_overrides(&id("STXT", 1)), vec![true]);
    ed.apply(se(StyleEdit::ClearOverrides { story: id("STXT", 1), paragraph: 0 })).unwrap();
    assert_eq!(ed.paragraph_overrides(&id("STXT", 1)), vec![false]);

    // Delete Heading, replacing it with Body: its paragraphs move to Body.
    ed.apply(se(StyleEdit::DeleteStyle { style: heading.clone(), replace_with: Some(id("STYS", 1)) })).unwrap();
    assert_eq!(ed.doc().stories[0].paragraphs[0].style_ref.as_deref(), Some(id("STYS", 1).as_str()));
    assert!(ed.doc().styles.style(&heading).is_none());
    // Every edit is one undo step.
    while ed.undo() {}
    assert_eq!(ed.doc(), &original);
}

#[test]
fn styles_panel_draws_both_lists_and_the_grep_editor() {
    let mut ed = LayoutEditor::new(doc());
    ed.selected = Some(id("SLYR", 1));
    let ctx = egui::Context::default();
    // First frame selects nothing; select Body through the panel state, then draw again.
    let mut drawn = Vec::new();
    let _ = ctx.run(egui::RawInput::default(), |ctx| drawn = ed.ui(ctx));
    for w in [widget::STYLES_PANEL, widget::PARA_STYLES, widget::CHAR_STYLES] {
        assert!(drawn.contains(&w), "{w} drawn");
    }
    ed.select_style(&id("STYS", 1));
    let _ = ctx.run(egui::RawInput::default(), |ctx| drawn = ed.ui(ctx));
    for w in [widget::STYLE_EDIT, widget::GREP_RULES, widget::CLEAR_OVERRIDES] {
        assert!(drawn.contains(&w), "{w} drawn");
    }
    let s = ed.state_json();
    assert_eq!(s["styles"]["paragraph"][0]["name"], "Body");
    assert_eq!(s["styles"]["character"][0]["name"], "Num");
}

/// Lane SMB-TYPE-INDENT-SPACING-1 (STU-TYP-185): the existing `EditStyle` command edits a paragraph
/// style's composition settings (indents and spacing among them) as one undo step; the composition
/// reads them. Folio refuses a non-point or non-finite value, a character style is refused by scope,
/// and every refusal leaves the document and the history untouched.
#[test]
fn style_composition_edit_composes_and_undoes() {
    use hsk_studio_folio::{GeometryUnit, Length, StudioCompositionSettings};
    let pt = |value: f64| Some(Length { value, unit: GeometryUnit::Pt });
    let mut d = doc();
    d.stories[0].paragraphs[0].style_ref = Some(id("STYS", 1));
    let mut ed = LayoutEditor::new(d);
    let x0 = |ed: &mut LayoutEditor| ed.compose(&id("STXT", 1)).unwrap().frames[0].lines.iter().map(|l| l.x_start_pt).collect::<Vec<_>>();
    assert_eq!(x0(&mut ed), vec![0.0, 0.0]);
    let indented = StudioCompositionSettings { left_indent: pt(12.0), first_line_indent: pt(6.0), ..Default::default() };
    let patch = |c: Option<StudioCompositionSettings>| StylePatch { composition: Some(c.map(Box::new)), ..StylePatch::default() };
    ed.apply(se(StyleEdit::EditStyle { style: id("STYS", 1), patch: patch(Some(indented.clone())) })).unwrap();
    assert_eq!(ed.doc().styles.style_composition(&id("STYS", 1)), Some(&indented));
    assert_eq!(x0(&mut ed), vec![18.0, 12.0]);
    let steps = ed.history_len().0;
    let last = ed.doc().clone();
    let mm = StudioCompositionSettings { left_indent: Some(Length { value: 3.0, unit: GeometryUnit::Mm }), ..Default::default() };
    let nan = StudioCompositionSettings { space_before: pt(f64::NAN), ..Default::default() };
    for (edit, code) in [
        (se(StyleEdit::EditStyle { style: id("STYS", 1), patch: patch(Some(mm)) }), "folio_invalid_style"),
        (se(StyleEdit::EditStyle { style: id("STYS", 1), patch: patch(Some(nan)) }), "folio_invalid_style"),
        (se(StyleEdit::EditStyle { style: id("STYS", 2), patch: patch(Some(indented.clone())) }), "wrong_style_scope"),
        (se(StyleEdit::EditStyle { style: id("STYS", 2), patch: patch(None) }), "wrong_style_scope"),
    ] {
        assert_eq!(ed.apply(edit).unwrap_err().code(), code);
        assert_eq!((ed.doc(), ed.history_len().0), (&last, steps));
    }
    // Clearing removes the style's entry; undo and redo walk the steps.
    ed.apply(se(StyleEdit::EditStyle { style: id("STYS", 1), patch: patch(None) })).unwrap();
    assert!(ed.doc().styles.style_composition.is_empty());
    assert_eq!(x0(&mut ed), vec![0.0, 0.0]);
    assert!(ed.undo());
    assert_eq!(x0(&mut ed), vec![18.0, 12.0]);
    assert!(ed.undo());
    assert_eq!(x0(&mut ed), vec![0.0, 0.0]);
    assert!(ed.redo());
    assert_eq!(x0(&mut ed), vec![18.0, 12.0]);
    // Deleting a style with settings removes its entry too (the set stays valid).
    ed.apply(se(StyleEdit::DeleteStyle { style: id("STYS", 1), replace_with: None })).unwrap();
    assert!(ed.doc().styles.style_composition.is_empty());
}

/// A text edit before an overridden paragraph moves the override's start byte with the paragraph
/// (it used to stay behind, so the next composition refused `invalid_input` or applied it to
/// another paragraph); merging two paragraphs drops the override and the span/split record of the
/// paragraph that no longer starts (the first touched paragraph wins, as for its style).
#[test]
fn text_edits_move_paragraph_keyed_records_with_their_paragraph() {
    use hsk_studio_folio::{
        GeometryUnit, Length, StoryContent, StoryParagraph, StudioCompositionSettings, StudioParagraphComposition, StudioParagraphFlow,
        StudioSpanColumn,
    };
    let mut d = doc();
    let text = "aaaa bbbb\ncccc dddd";
    d.stories[0].content = StoryContent::Inline { text_utf8: text.into() };
    d.stories[0].paragraphs = vec![
        StoryParagraph { start: 0, end: 10, style_ref: None, paragraph_direction: None },
        StoryParagraph { start: 10, end: 19, style_ref: None, paragraph_direction: None },
    ];
    d.stories[0].character_ranges[0].end = 19;
    let indent = StudioCompositionSettings { left_indent: Some(Length { value: 30.0, unit: GeometryUnit::Pt }), ..Default::default() };
    d.styles.paragraph_composition.push(StudioParagraphComposition { text_id: id("STXT", 1), paragraph_start: 10, settings: indent });
    let len0 = Length { value: 0.0, unit: GeometryUnit::Pt };
    d.flow.paragraph_flows.push(StudioParagraphFlow {
        story_id: id("STXT", 1),
        paragraph_start: 10,
        span_column: StudioSpanColumn::SingleColumn,
        span_count: None,
        span_column_count: 1,
        split_column_count: 2,
        space_before: len0.clone(),
        space_after: len0.clone(),
        inside_gutter: len0.clone(),
        outside_gutter: len0,
    });
    let mut ed = LayoutEditor::new(d);
    let lines = |ed: &mut LayoutEditor| ed.compose(&id("STXT", 1)).unwrap().frames[0].lines.iter().map(|l| (l.source.start, l.x_start_pt)).collect::<Vec<_>>();
    assert_eq!(lines(&mut ed), vec![(0, 0.0), (10, 30.0)]);
    // Typing before the overridden paragraph (inside paragraph 0).
    ed.apply(LayoutEdit::ReplaceText { story: id("STXT", 1), start: 1, end: 1, text: "xx".into() }).unwrap();
    assert_eq!(ed.doc().styles.paragraph_composition[0].paragraph_start, 12);
    assert_eq!(ed.doc().flow.paragraph_flows[0].paragraph_start, 12);
    assert_eq!(lines(&mut ed), vec![(0, 0.0), (12, 30.0)]);
    // Deleting the terminator merges the paragraphs: both records of the second one go.
    ed.apply(LayoutEdit::ReplaceText { story: id("STXT", 1), start: 11, end: 12, text: " ".into() }).unwrap();
    assert_eq!(ed.doc().stories[0].paragraphs.len(), 1);
    assert!(ed.doc().styles.paragraph_composition.is_empty());
    assert!(ed.doc().flow.paragraph_flows.is_empty());
    assert_eq!(lines(&mut ed)[0], (0, 0.0));
}

/// The drive row `layout.style.edit_style` decodes a `composition` key as Folio settings (null
/// clears, absent keeps; an unknown member is refused) and the composed lines read them.
#[cfg(feature = "viewer")]
#[test]
fn drive_edit_style_composition_sets_the_indent() {
    use hsk_studio_harness::view::Viewer;
    use hsk_studio_harness::view::render::Prefer;
    use serde_json::{Value, json};
    let run = |v: &mut Viewer, id: &str, p: Value| v.run_command(id, p.as_object().unwrap());
    use hsk_studio_harness_core::document_new::{DocumentProfile, NewDocument};
    let mut v = Viewer::new(Prefer::Cpu);
    v.new_document(&NewDocument::new(DocumentProfile::Layout, Some((600.0, 400.0))), None).unwrap();
    // The first layout command opens the editor on the document; its page carries the frame.
    run(&mut v, "layout.preflight", json!({})).unwrap();
    let page = v.layout_editor().unwrap().doc().document.page_spreads[0].pages[0].page_id.clone();
    run(&mut v, "layout.draw_frame", json!({"page_id":page,"x":10,"y":10,"w":300,"h":200})).unwrap();
    let story = v.layout_editor().unwrap().doc().stories.last().unwrap().story_id.clone();
    run(&mut v, "layout.replace_text", json!({"story":story,"start":0,"end":0,"text":"Indented body copy"})).unwrap();
    run(&mut v, "layout.style.new_style", json!({"scope":"paragraph","name":"Body"})).unwrap();
    let style = v.layout_editor().unwrap().style_rows(TypeStyleScope::Paragraph).into_iter().find(|r| r.name == "Body").unwrap().style_id;
    run(&mut v, "layout.apply_paragraph_style", json!({"story":story,"paragraph":0,"style":style})).unwrap();
    let x = |v: &mut Viewer| v.layout_editor().unwrap().compose(&story).unwrap().frames[0].lines[0].x_start_pt;
    assert_eq!(x(&mut v), 0.0);
    run(&mut v, "layout.style.edit_style", json!({"style":style,"patch":{"composition":{"left_indent":{"value":24.0,"unit":"pt"}}}})).unwrap();
    assert_eq!(x(&mut v), 24.0);
    // Absent keeps, null clears; an unknown settings member and a millimetre value are refused.
    run(&mut v, "layout.style.edit_style", json!({"style":style,"patch":{"name":"Body text"}})).unwrap();
    assert_eq!(x(&mut v), 24.0);
    let bad = run(&mut v, "layout.style.edit_style", json!({"style":style,"patch":{"composition":{"left_indnet":{"value":1.0,"unit":"pt"}}}})).unwrap_err();
    assert_eq!(bad.code, "parameter_invalid");
    assert!(run(&mut v, "layout.style.edit_style", json!({"style":style,"patch":{"composition":{"left_indent":{"value":1.0,"unit":"mm"}}}})).is_err());
    assert_eq!(x(&mut v), 24.0);
    run(&mut v, "layout.style.edit_style", json!({"style":style,"patch":{"composition":null}})).unwrap();
    assert_eq!(x(&mut v), 0.0);
}
