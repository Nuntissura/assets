//! Lane SMB-LAYOUT-UI-2 slice 5: live preflight engine (STU-LAY-055/057/154). Hand-computed
//! oracles over `support_layout` (6 pt glyphs, 12 pt lines, 200 pt pages). InDesign equality
//! UNVERIFIED (D5).
mod support_layout;
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use support_layout::*;

#[test]
fn preflight_rules_read_engine_results_with_profile_and_scope() {
    // Story 1 in a one-line frame on page 1 (overset); page 2 holds a frame past its right edge.
    let doc = document(
        vec![frame_layer(1, 1, spec(0, 10.0, 10.0, 120.0, 15.0), None, None), frame_layer(2, 2, spec(1, 150.0, 10.0, 100.0, 15.0), None, None)],
        [PageSide::RightHand, PageSide::RightHand],
    );
    let (s1, p1) = story(1, "aaaa bbbb cccc dddd eeee ffff", &[(0, 29)]);
    let (s2, p2) = story(2, "x", &[(0, 1)]);
    let flow = records();
    let comps = vec![(id("STXT", 1), run(&doc, &s1, &p1, &flow)), (id("STXT", 2), run(&doc, &s2, &p2, &flow))];
    let mut styles = StudioTypeStyleRecords::new(id("SDOC", 1));
    styles.styles = vec![];
    let stories = [s1.clone(), s2.clone()];
    let input = PreflightInput { document: &doc, stories: &stories, compositions: &comps, styles: &styles, flow: &flow, parents: None, pages: &[] };
    let r = run_preflight(&input, &PreflightProfile::basic(), None).unwrap();
    let got: Vec<(&str, Severity, &str, Option<&str>)> = r.findings.iter().map(|f| (f.rule_id.as_str(), f.severity, f.object.as_str(), f.page.as_deref())).collect();
    let (l1, l2) = (id("SLYR", 1), id("SLYR", 2));
    assert_eq!(got, vec![("text.overset", Severity::Error, l1.as_str(), Some("page-1")), ("objects.outside_page", Severity::Warning, l2.as_str(), Some("page-2"))]);
    assert_eq!((r.errors, r.warnings, r.ok()), (1, 1, false));
    // Scope page 2 only; a disabled rule reports nothing; a composition refusal is its own finding.
    assert_eq!(run_preflight(&input, &PreflightProfile::basic(), Some(&["page-2".into()])).unwrap().findings.len(), 1);
    let mut quiet = PreflightProfile::basic();
    quiet.rules.iter_mut().for_each(|r| r.severity = Severity::Disabled);
    assert!(run_preflight(&input, &quiet, None).unwrap().findings.is_empty());
    let refused = vec![(id("STXT", 1), Err(LayoutError::CyclicThread))];
    let input2 = PreflightInput { compositions: &refused, ..input };
    let r2 = run_preflight(&input2, &PreflightProfile::basic(), None).unwrap();
    assert_eq!((r2.findings[0].rule_id.as_str(), r2.findings[0].detail.as_str()), ("text.composition_refused", "cyclic_thread"));
    // Typed refusals: unknown rule in a profile, unknown page in a scope.
    let mut odd = PreflightProfile::basic();
    odd.rules.push(PreflightRuleInstance { rule_id: "ink.density".into(), severity: Severity::Error });
    assert_eq!(run_preflight(&input2, &odd, None).unwrap_err().code(), "preflight_unknown_rule");
    assert_eq!(run_preflight(&input2, &PreflightProfile::basic(), Some(&["page-7".into()])).unwrap_err().code(), "preflight_unknown_page");
    // Missing style refs and double spaces in story text.
    let (mut s3, _) = story(1, "a  b   c", &[(0, 8)]);
    s3.paragraphs[0].style_ref = Some(id("STYS", 9));
    let stories3 = [s3];
    let input3 = PreflightInput { stories: &stories3, compositions: &[], ..input2 };
    let r3 = run_preflight(&input3, &PreflightProfile::basic(), None).unwrap();
    let rules3: Vec<(&str, &str)> = r3.findings.iter().filter(|f| f.rule_id.starts_with("text.")).map(|f| (f.rule_id.as_str(), f.detail.as_str())).collect();
    assert_eq!(rules3, vec![("text.missing_style", &*format!("paragraph 0 style {}", id("STYS", 9))), ("text.double_space", "byte 1"), ("text.double_space", "byte 4")]);
}
