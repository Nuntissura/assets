//! SMB-ID-LIQUID-LAYOUT-1 adjust layout (STU-LAY-010/112): partial property bag, measurement
//! strings, scopes and the page-scope bleed exception, locked content, guides, font-size bounds and
//! undo. Pages 200 x 200 pt with 10 pt margins (margin box 180 pt). InDesign adjust-layout numerics
//! have no oracle case: NOT_PROVEN against InDesign (D5).
mod support_layout;
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use support_layout::*;

fn ctx() -> RuleContext<'static> {
    RuleContext {
        stories: &[],
        flow: None,
        typeset: TypesetOptions::default(),
        composer: Composer::FirstFit,
        space: SpaceParams::default(),
        limits: Limits::default(),
        budget: hsk_studio_folio::Budget::default(),
        resolver: &Unresolved,
    }
}
const MEASURE: MeasureContext = MeasureContext { percent_of_pt: 0.0, pixels_per_inch: 96.0, viewport_pt: [720.0, 360.0] };
fn options() -> AdjustOptions {
    AdjustOptions { adjust_font_size: false, font_size_min_pt: None, font_size_max_pt: None, include_locked: false, move_ruler_guides: false }
}
fn bag(entries: &[(&str, AdjustValue)]) -> AdjustBag {
    AdjustBag::from_entries(entries.iter().cloned()).expect("bag")
}
fn run_adjust(
    doc: &StudioDocument,
    grids: Option<&StudioLayoutGrids>,
    b: &AdjustBag,
    scope: &AdjustScope,
    o: AdjustOptions,
    locked: &[String],
) -> Result<AdjustProposal, LayoutRuleError> {
    adjust_layout(
        &AdjustRequest { document: doc, graphics: None, grids, bag: b, scope, options: o, locked, measure: MEASURE, expected_revision: 2, context: ctx() },
        &Default::default(),
    )
}
fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect { x0_pt: x, y0_pt: y, x1_pt: x + w, y1_pt: y + h }
}
fn moved(p: &AdjustProposal, layer: u32) -> Option<Rect> {
    p.objects.iter().find(|o| o.layer_id == id("SLYR", layer)).map(|o| o.after)
}
fn page_of<'a>(d: &'a StudioDocument, key: &str) -> &'a StudioPage {
    d.locate_page(key).unwrap().page
}
fn sides() -> [PageSide; 2] {
    [PageSide::RightHand, PageSide::LeftHand]
}
fn guide(gid: &str, at: f64, locked: bool) -> StudioRulerGuide {
    StudioRulerGuide {
        guide_id: gid.into(),
        page_spread_id: id("SPGS", 1),
        page_id: Some("page-1".into()),
        orientation: StudioGuideOrientation::Vertical,
        location: len(at),
        guide_colour: StudioRgba { r: 0.0, g: 1.0, b: 1.0, a: 1.0 },
        guide_type: StudioGuideType::Ruler,
        guide_view_threshold: 5.0,
        fit_to_page: false,
        locked,
        overridden: false,
        allow_overrides: false,
    }
}

/// Only `width` is named: every page gets the new width; height, margins and (absent) bleed are
/// unchanged; edges on the page edge stay there, other edges map through the margin box (x2).
/// Unknown, repeated and invalid keys or values are typed refusals.
#[test]
fn partial_bag_changes_only_named_values() {
    let doc = document(
        vec![frame_layer(1, 1, spec(0, 10.0, 20.0, 90.0, 40.0), None, None), frame_layer(2, 2, spec(0, 0.0, 100.0, 200.0, 20.0), None, None)],
        sides(),
    );
    let p = run_adjust(&doc, None, &bag(&[("width", AdjustValue::Pt(380.0))]), &AdjustScope::Document, options(), &[]).expect("adjust");
    for key in ["page-1", "page-2"] {
        let (before, after) = (page_of(&doc, key), page_of(&p.document, key));
        assert_eq!(after.width.value, 380.0);
        assert_eq!((&after.height, &after.margins, &after.bleed, after.side), (&before.height, &before.margins, &before.bleed, before.side));
    }
    assert_eq!(moved(&p, 1), Some(rect(10.0, 20.0, 180.0, 40.0)));
    assert_eq!(moved(&p, 2), Some(rect(0.0, 100.0, 380.0, 20.0)));
    assert!(p.ignored_keys.is_empty());
    assert_eq!(p.pages.len(), 2);
    assert_eq!(AdjustKey::ALL.len(), 10);
    for k in AdjustKey::ALL {
        assert_eq!(AdjustKey::parse(k.wire()), Some(k));
    }
    let e = AdjustBag::from_entries([("Width", AdjustValue::Pt(1.0))]).unwrap_err();
    assert_eq!(e.code(), "adjust_key_unknown");
    assert_eq!(AdjustBag::from_entries([("pageWidth", AdjustValue::Pt(1.0))]).unwrap_err().code(), "adjust_key_unknown");
    let e = AdjustBag::from_entries([("width", AdjustValue::Pt(1.0)), ("width", AdjustValue::Pt(2.0))]).unwrap_err();
    assert_eq!(e.code(), "adjust_key_repeated");
    for v in [AdjustValue::Pt(-5.0), AdjustValue::Pt(0.0), AdjustValue::Pt(f64::NAN), AdjustValue::Pt(f64::INFINITY)] {
        let e = run_adjust(&doc, None, &bag(&[("width", v)]), &AdjustScope::Document, options(), &[]).unwrap_err();
        assert_eq!(e.code(), "adjust_value_invalid");
    }
    let e = run_adjust(&doc, None, &bag(&[("leftMargin", AdjustValue::Pt(195.0))]), &AdjustScope::Document, options(), &[]).unwrap_err();
    assert_eq!(e.code(), "adjust_value_invalid", "margin box must stay positive");
    let e = run_adjust(&doc, None, &bag(&[("topMargin", AdjustValue::Pt(-1.0))]), &AdjustScope::Document, options(), &[]).unwrap_err();
    assert_eq!(e.code(), "adjust_value_invalid");
    assert_eq!(run_adjust(&doc, None, &bag(&[]), &AdjustScope::Pages(vec![]), options(), &[]).unwrap_err().code(), "scope_empty");
    assert_eq!(run_adjust(&doc, None, &bag(&[]), &AdjustScope::Page("page-9".into()), options(), &[]).unwrap_err().code(), "page_missing");
    let mut stale = doc.clone();
    stale.revision = 3;
    assert_eq!(run_adjust(&stale, None, &bag(&[]), &AdjustScope::Document, options(), &[]).unwrap_err().code(), "stale_revision");
}

/// Each STU-LAY-101 input suffix is accepted (`%` of the key's current value, `px` at the caller's
/// 96 ppi, viewport units against 720 x 360 pt, a bare number in `pt`); others are refused.
#[test]
fn measurement_strings_parse_every_suffix() {
    let doc = document(vec![], sides());
    let cases = [
        ("1in", 72.0),
        ("25.4mm", 72.0),
        ("2.54cm", 72.0),
        ("72pt", 72.0),
        ("1PT", 1.0),
        ("50%", 5.0),
        ("96px", 72.0),
        ("10vw", 72.0),
        ("20vh", 72.0),
        ("10vmin", 36.0),
        ("10vmax", 72.0),
        ("12", 12.0),
        (" 3.5 mm ", 3.5 * 72.0 / 25.4),
    ];
    for (text, want) in cases {
        let b = bag(&[("leftMargin", AdjustValue::Text(text.into()))]);
        let p = run_adjust(&doc, None, &b, &AdjustScope::Page("page-1".into()), options(), &[]).expect(text);
        let got = page_of(&p.document, "page-1").margins.left.value;
        assert!((got - want).abs() < 1e-9, "{text}: {got} != {want}");
        assert_eq!(page_of(&p.document, "page-2").margins.left.value, 10.0, "page scope");
    }
    let p = run_adjust(&doc, None, &bag(&[("rightMargin", AdjustValue::Pt(12.5))]), &AdjustScope::Document, options(), &[]).unwrap();
    assert_eq!(page_of(&p.document, "page-2").margins.right.value, 12.5);
    for (text, code) in [("1pc", "unit_suffix_unknown"), ("3 ciceros", "unit_suffix_unknown"), ("abc", "measure_not_a_number"), ("", "measure_not_a_number")] {
        let b = bag(&[("leftMargin", AdjustValue::Text(text.into()))]);
        let e = run_adjust(&doc, None, &b, &AdjustScope::Document, options(), &[]).unwrap_err();
        assert_eq!(e.code(), code, "{text}");
        assert!(matches!(e, LayoutRuleError::Measure { ref key, .. } if key == "leftMargin"));
    }
}

/// STU-LAY-112 exception: scoped to a page or a page collection, bleed keys have no effect (listed
/// in `ignored_keys`); at document scope the new bleed applies and an edge on the old bleed edge
/// follows it.
#[test]
fn page_scope_ignores_bleed() {
    let mut doc = document(vec![frame_layer(1, 1, spec(0, 20.0, -5.0, 50.0, 30.0), None, None)], sides());
    doc.page_spreads[0].pages[0].bleed = Some(PageBleed { top: len(5.0), bottom: len(5.0), inside_or_left: len(5.0), outside_or_right: len(5.0) });
    let b = bag(&[("bleedTop", AdjustValue::Text("9pt".into())), ("height", AdjustValue::Pt(380.0))]);
    for scope in [AdjustScope::Page("page-1".into()), AdjustScope::Pages(vec!["page-1".into(), "page-1".into()])] {
        let p = run_adjust(&doc, None, &b, &scope, options(), &[]).expect("page scope");
        assert_eq!(p.ignored_keys, vec![AdjustKey::BleedTop]);
        let page = page_of(&p.document, "page-1");
        assert_eq!((page.height.value, page.bleed.as_ref().unwrap().top.value), (380.0, 5.0));
        assert_eq!(moved(&p, 1), Some(Rect { x0_pt: 20.0, y0_pt: -5.0, x1_pt: 70.0, y1_pt: 40.0 }));
        assert_eq!(p.pages.len(), 1, "duplicates collapse");
    }
    let p = run_adjust(&doc, None, &b, &AdjustScope::Document, options(), &[]).expect("document scope");
    assert!(p.ignored_keys.is_empty());
    assert_eq!(page_of(&p.document, "page-1").bleed.as_ref().unwrap().top.value, 9.0);
    let b2 = page_of(&p.document, "page-2").bleed.clone().expect("bleed created");
    assert_eq!((b2.top.value, b2.bottom.value, b2.inside_or_left.value, b2.outside_or_right.value), (9.0, 0.0, 0.0, 0.0));
    assert_eq!(moved(&p, 1), Some(Rect { x0_pt: 20.0, y0_pt: -9.0, x1_pt: 70.0, y1_pt: 40.0 }), "bleed edge follows the bleed");
}

/// Locked objects and locked guides move only with `include_locked`; guides move only with
/// `move_ruler_guides`; undo restores the document and guides exactly.
#[test]
fn locked_content_moves_only_when_included() {
    let doc = document(
        vec![frame_layer(1, 1, spec(0, 10.0, 20.0, 90.0, 40.0), None, None), frame_layer(2, 2, spec(0, 100.0, 120.0, 40.0, 40.0), None, None)],
        sides(),
    );
    let mut grids = StudioLayoutGrids::new(id("SDOC", 1));
    grids.guides.push(guide("free", 100.0, false));
    grids.guides.push(guide("pinned", 50.0, true));
    let b = bag(&[("width", AdjustValue::Pt(380.0))]);
    let locked = vec![id("SLYR", 1)];
    let mut o = options();
    o.move_ruler_guides = true;
    let p = run_adjust(&doc, Some(&grids), &b, &AdjustScope::Document, o, &locked).expect("adjust");
    assert_eq!(moved(&p, 1), None);
    assert_eq!(p.locked_skipped, locked);
    assert_eq!(moved(&p, 2), Some(rect(190.0, 120.0, 80.0, 40.0)));
    assert_eq!(p.guides_moved, vec!["free".to_string()]);
    let g = p.grids.as_ref().unwrap();
    assert_eq!((g.guides[0].location.value, g.guides[1].location.value), (190.0, 50.0));
    o.include_locked = true;
    let p = run_adjust(&doc, Some(&grids), &b, &AdjustScope::Document, o, &locked).expect("include locked");
    assert_eq!(moved(&p, 1), Some(rect(10.0, 20.0, 180.0, 40.0)));
    assert!(p.locked_skipped.is_empty());
    let g = p.grids.as_ref().unwrap();
    assert_eq!((g.guides[0].location.value, g.guides[1].location.value), (190.0, 90.0));
    let (d, _, gr) = undo_adjust(&p.document, None, p.grids.as_ref(), &p.undo);
    assert_eq!(d, doc);
    assert_eq!(gr.as_ref(), Some(&grids));
    o.move_ruler_guides = false;
    let p = run_adjust(&doc, Some(&grids), &b, &AdjustScope::Document, o, &locked).expect("no guides");
    assert!(p.guides_moved.is_empty());
    assert_eq!(p.grids.as_ref(), Some(&grids));
}

/// STU-LAY-010 SPEC GAP: font-size bounds have no default, so `adjust_font_size` needs both caller
/// bounds; the factor is min(margin-box ratios) and sizes clamp to the bounds.
#[test]
fn font_size_adjust_needs_explicit_bounds() {
    let doc = document(vec![], sides());
    let b = bag(&[("width", AdjustValue::Pt(380.0)), ("height", AdjustValue::Pt(380.0))]);
    let mut o = options();
    o.adjust_font_size = true;
    assert_eq!(run_adjust(&doc, None, &b, &AdjustScope::Document, o, &[]).unwrap_err().code(), "font_size_bounds_missing");
    o.font_size_min_pt = Some(6.0);
    assert_eq!(run_adjust(&doc, None, &b, &AdjustScope::Document, o, &[]).unwrap_err().code(), "font_size_bounds_missing");
    for (min, max) in [(12.0, 6.0), (0.0, 6.0), (f64::NAN, 6.0), (6.0, f64::INFINITY)] {
        o.font_size_min_pt = Some(min);
        o.font_size_max_pt = Some(max);
        assert_eq!(run_adjust(&doc, None, &b, &AdjustScope::Document, o, &[]).unwrap_err().code(), "font_size_bounds_invalid");
    }
    o.font_size_min_pt = Some(6.0);
    o.font_size_max_pt = Some(12.0);
    let p = run_adjust(&doc, None, &b, &AdjustScope::Document, o, &[]).expect("bounds");
    let f = p.pages[0].font_size.expect("factor");
    assert_eq!(f, FontSizeAdjust { factor: 2.0, min_pt: 6.0, max_pt: 12.0 });
    assert_eq!((f.apply(10.0), f.apply(4.0), f.apply(1.0)), (12.0, 8.0, 6.0));
    o.adjust_font_size = false;
    let p = run_adjust(&doc, None, &b, &AdjustScope::Document, o, &[]).expect("off");
    assert_eq!(p.pages[0].font_size, None, "bounds without the option do nothing");
}
