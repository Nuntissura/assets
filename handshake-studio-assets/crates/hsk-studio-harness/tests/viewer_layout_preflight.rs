//! Lane SMB-LAYOUT-UI-2 slice 5: live preflight panel over layout's preflight engine
//! (STU-LAY-055/057/154), headless (fixture in `layout_ui_fixture`: story 1 is overset on page 1,
//! page 2 is empty). InDesign preflight equality UNVERIFIED (D5).
#![cfg(feature = "layout-ui")]
mod layout_ui_fixture;
use hsk_studio_harness::layout_ui::{LayoutEdit, LayoutEditor, preflight::colour, widget};
use hsk_studio_layout::Severity;
use layout_ui_fixture::{doc, id, rect_colours, rects_of, ui_frames};

fn rules(ed: &LayoutEditor, scope: Option<&[String]>) -> Vec<(String, Severity, String, Option<String>)> {
    ed.preflight(scope).unwrap().findings.into_iter().map(|f| (f.rule_id, f.severity, f.object, f.page)).collect()
}

#[test]
fn preflight_reports_structured_findings_live_with_scope_and_severities() {
    let mut ed = LayoutEditor::new(doc());
    let r = ed.preflight(None).unwrap();
    assert_eq!((r.ok(), r.errors, r.warnings, r.informational), (false, 1, 1, 0));
    assert_eq!(
        rules(&ed, None),
        vec![
            ("text.overset".into(), Severity::Error, id("SLYR", 1), Some("page-1".into())),
            ("document.blank_page".into(), Severity::Warning, "page-2".into(), Some("page-2".into())),
        ]
    );
    // Page scope: only page 2's findings.
    assert_eq!(rules(&ed, Some(&["page-2".into()])).len(), 1);
    assert_eq!(ed.preflight(Some(&["page-9".into()])).unwrap_err().code(), "preflight_unknown_page");
    // Live: edits change the findings. A frame past page 2's edge, a double space in story 1.
    ed.apply(LayoutEdit::DrawFrame { page_id: "page-2".into(), x: 150.0, y: 10.0, w: 100.0, h: 20.0 }).unwrap();
    let wide = ed.selected.clone().unwrap();
    ed.apply(LayoutEdit::ReplaceText { story: id("STXT", 1), start: 4, end: 5, text: "  ".into() }).unwrap();
    let found = rules(&ed, None);
    assert!(found.contains(&("objects.outside_page".into(), Severity::Warning, wide.clone(), Some("page-2".into()))));
    assert!(found.contains(&("text.double_space".into(), Severity::Informational, id("STXT", 1), None)));
    assert!(!found.iter().any(|f| f.0 == "document.blank_page"));
    // Threading the overset story into the new frame clears the overset error.
    ed.apply(LayoutEdit::Thread { from: id("SLYR", 1), to: wide.clone() }).unwrap();
    assert!(ed.preflight(None).unwrap().ok());
    // Severities are the profile's: disabling a rule removes its findings; errors raise the count.
    ed.set_preflight_severity("objects.outside_page", Severity::Error).unwrap();
    assert_eq!(ed.preflight(None).unwrap().errors, 1);
    ed.set_preflight_severity("objects.outside_page", Severity::Disabled).unwrap();
    assert!(!rules(&ed, None).iter().any(|f| f.0 == "objects.outside_page"));
    assert_eq!(ed.set_preflight_severity("no.such.rule", Severity::Error).unwrap_err().code(), "preflight_unknown_rule");
    // Missing style references are findings (the composition keeps running).
    let mut d = doc();
    d.stories[0].paragraphs[0].style_ref = Some(id("STYS", 99));
    let bad = LayoutEditor::new(d);
    assert!(rules(&bad, None).iter().any(|f| f.0 == "text.missing_style" && f.2 == id("STXT", 1)));
}

#[test]
fn preflight_panel_draws_status_indicator_and_findings() {
    let mut ed = LayoutEditor::new(doc());
    let (drawn, shapes) = ui_frames(&mut ed);
    for w in [widget::PREFLIGHT_PANEL, widget::PREFLIGHT_STATUS, widget::PREFLIGHT_FINDINGS, widget::PREFLIGHT_PROFILE] {
        assert!(drawn.contains(&w), "{w} drawn");
    }
    assert_eq!((rects_of(&shapes, colour::ERRORS, true), rects_of(&shapes, colour::OK, true)), (1, 0), "{:?}", rect_colours(&shapes));
    let s = ed.state_json();
    assert_eq!((s["preflight"]["ok"].as_bool(), s["preflight"]["errors"].as_u64()), (Some(false), Some(1)));
    assert_eq!(s["preflight"]["findings"][0]["rule_id"], "text.overset");
    assert_eq!(s["preflight"]["findings"][0]["severity"], "severity.error");
}
