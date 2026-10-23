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
