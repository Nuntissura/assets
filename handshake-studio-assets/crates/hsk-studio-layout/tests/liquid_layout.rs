//! SMB-ID-LIQUID-LAYOUT-1 liquid rules (STU-LAY-009/111): closed-form geometry in pt, reflow
//! compared with the existing composer on the same story, typed refusals. Fixtures: pages 200 x
//! 200 pt, 10 pt margins, two pages per spread; toy font 6 pt per glyph, 12 pt lines, first baseline
//! 10 pt. InDesign numerics have no oracle case: NOT_PROVEN against InDesign (D5).
mod support_layout;
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use support_layout::*;

fn ctx<'a>(stories: &'a [StoryInput<'a>]) -> RuleContext<'a> {
    RuleContext {
        stories,
        flow: None,
        typeset: TypesetOptions::default(),
        composer: Composer::FirstFit,
        space: SpaceParams::default(),
        limits: Limits::default(),
        budget: hsk_studio_folio::Budget::default(),
        resolver: &Unresolved,
    }
}
fn rules(page_rules: &[(&str, StudioLiquidRule)]) -> StudioLayoutRuleRecords {
    let mut r = StudioLayoutRuleRecords::new(id("SDOC", 1));
    for (p, rule) in page_rules {
        r.page_rules.push(StudioPageLiquidRule { page_id: (*p).into(), liquid_rule: *rule });
    }
    r
}
fn request<'a>(doc: &'a StudioDocument, records: &'a StudioLayoutRuleRecords, page: &'a str, w: f64, h: f64, c: RuleContext<'a>) -> LiquidRequest<'a> {
    LiquidRequest {
        document: doc,
        records,
        parents: None,
        grids: None,
        graphics: None,
        page_id: page,
        width_pt: w,
        height_pt: h,
        expected_revision: 2,
        context: c,
    }
}
fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect { x0_pt: x, y0_pt: y, x1_pt: x + w, y1_pt: y + h }
}
fn after(p: &LiquidProposal, layer: u32) -> Rect {
    p.objects.iter().find(|o| o.layer_id == id("SLYR", layer)).map(|o| o.after).expect("moved")
}
fn page_of<'a>(d: &'a StudioDocument, key: &str) -> &'a StudioPage {
    d.locate_page(key).unwrap().page
}
fn words(n: usize) -> String {
    (0..n).map(|_| "word").collect::<Vec<_>>().join(" ")
}

/// `liquid.scale`: uniform factor min(400/200, 300/200) = 1.5 about the page origin; relative
/// positions hold, the story is reflowed in the scaled frame exactly as the composer lays it out.
#[test]
fn scale_keeps_relative_positions() {
    let doc = document(
        vec![frame_layer(1, 1, spec(0, 20.0, 30.0, 100.0, 40.0), None, None), frame_layer(2, 2, spec(0, 100.0, 100.0, 50.0, 50.0), None, None)],
        [PageSide::RightHand, PageSide::LeftHand],
    );
    let text = words(6);
    let (st, paras) = story(1, &text, &[(0, text.len() as u64)]);
    let (st2, paras2) = story(2, "tiny", &[(0, 4)]);
    let inputs = [
        StoryInput { story: &st, resolved_text: None, paragraphs: &paras },
        StoryInput { story: &st2, resolved_text: None, paragraphs: &paras2 },
    ];
    let r = rules(&[("page-1", StudioLiquidRule::Scale)]);
    let p = apply_liquid(&request(&doc, &r, "page-1", 400.0, 300.0, ctx(&inputs)), &Default::default()).expect("scale");
    assert_eq!(p.rule, StudioLiquidRule::Scale);
    assert_eq!((page_of(&p.document, "page-1").width.value, page_of(&p.document, "page-1").height.value), (400.0, 300.0));
    assert_eq!(page_of(&p.document, "page-2"), page_of(&doc, "page-2"), "other pages untouched");
    assert_eq!(after(&p, 1), rect(30.0, 45.0, 150.0, 60.0));
    assert_eq!(after(&p, 2), rect(150.0, 150.0, 75.0, 75.0));
    // Relative position: B's offset from A scales by the same factor.
    assert_eq!(after(&p, 2).x0_pt - after(&p, 1).x0_pt, 1.5 * (100.0 - 20.0));
    let expected = run(&p.document, &st, &paras, &records()).unwrap();
    assert_eq!(p.reflows[0].proposal, expected);
    assert_eq!(p.reflows[0].proposal.frames[0].lines[0].available_pt, 150.0);
    assert_eq!(p.reflows[0].overset, None);
    assert_eq!(p.base_revision, 2);
}

/// `liquid.recenter`: size kept, translated by half the size change; `liquid.off` keeps absolute
/// geometry. Hostile sizes, stale revisions and foreign records are typed refusals.
#[test]
fn recenter_keeps_size() {
    let doc = document(vec![frame_layer(1, 1, spec(1, 50.0, 50.0, 60.0, 60.0), None, None)], [PageSide::RightHand, PageSide::LeftHand]);
    let r = rules(&[("page-2", StudioLiquidRule::Recenter)]);
    let p = apply_liquid(&request(&doc, &r, "page-2", 300.0, 260.0, ctx(&[])), &Default::default()).expect("recenter");
    assert_eq!(after(&p, 1), rect(100.0, 80.0, 60.0, 60.0));
    let off = rules(&[("page-2", StudioLiquidRule::Off)]);
    let p = apply_liquid(&request(&doc, &off, "page-2", 300.0, 260.0, ctx(&[])), &Default::default()).expect("off");
    assert!(p.objects.is_empty());
    assert_eq!(page_of(&p.document, "page-2").width.value, 300.0);
    let none = rules(&[]);
    assert_eq!(apply_liquid(&request(&doc, &none, "page-2", 300.0, 260.0, ctx(&[])), &Default::default()).unwrap().rule, StudioLiquidRule::Off, "absent rule = off");
    for (w, h) in [(0.0, 100.0), (f64::NAN, 100.0), (100.0, f64::INFINITY), (-5.0, 100.0)] {
        let e = apply_liquid(&request(&doc, &r, "page-2", w, h, ctx(&[])), &Default::default()).unwrap_err();
        assert_eq!(e.code(), "invalid_geometry");
    }
    let mut q = request(&doc, &r, "page-2", 300.0, 260.0, ctx(&[]));
    q.expected_revision = 1;
    assert_eq!(apply_liquid(&q, &Default::default()).unwrap_err().code(), "stale_revision");
    let e = apply_liquid(&request(&doc, &r, "page-9", 300.0, 260.0, ctx(&[])), &Default::default()).unwrap_err();
    assert_eq!(e.code(), "page_missing");
    let foreign = StudioLayoutRuleRecords::new(id("SDOC", 2));
    assert_eq!(apply_liquid(&request(&doc, &foreign, "page-2", 300.0, 260.0, ctx(&[])), &Default::default()).unwrap_err().code(), "invalid_input");
    let canceled = hsk_studio_accord::CancellationToken::default();
    canceled.cancel();
    assert_eq!(apply_liquid(&request(&doc, &r, "page-2", 300.0, 260.0, ctx(&[])), &canceled).unwrap_err().code(), "canceled");
}

/// `liquid.object_based`: both pins keep both edge distances (relative width), one pin keeps
/// that edge distance (fixed width), no constraint = centre scales with fixed size. A pinned frame
/// squeezed to a non-positive width is refused, never written.
#[test]
fn object_pins_hold_edges() {
    let doc = document(
        vec![
            frame_layer(1, 1, spec(0, 20.0, 20.0, 160.0, 30.0), None, None),
            frame_layer(2, 2, spec(0, 150.0, 100.0, 40.0, 40.0), None, None),
            frame_layer(3, 3, spec(0, 20.0, 150.0, 40.0, 30.0), None, None),
        ],
        [PageSide::RightHand, PageSide::LeftHand],
    );
    let mut r = rules(&[("page-1", StudioLiquidRule::ObjectBased)]);
    let c = |n: u32, l: bool, t: bool, rr: bool, b: bool, w: StudioResizeConstraint, h: StudioResizeConstraint| StudioObjectLiquidConstraint {
        layer_id: id("SLYR", n),
        pin_left: l,
        pin_top: t,
        pin_right: rr,
        pin_bottom: b,
        width: w,
        height: h,
    };
    use StudioResizeConstraint::{Fixed, Relative};
    r.object_constraints.push(c(1, true, true, true, false, Relative, Fixed));
    r.object_constraints.push(c(2, false, false, true, true, Fixed, Fixed));
    validate_layout_rule_records(&r, &doc, None, hsk_studio_folio::Budget::default(), &Default::default(), &Unresolved).expect("records");
    let p = apply_liquid(&request(&doc, &r, "page-1", 300.0, 300.0, ctx(&[])), &Default::default()).expect("object based");
    assert_eq!(after(&p, 1), rect(20.0, 20.0, 260.0, 30.0), "left/right pinned, top pinned");
    assert_eq!(after(&p, 2), rect(250.0, 200.0, 40.0, 40.0), "right/bottom pinned, fixed size");
    assert_eq!(after(&p, 3), rect(40.0, 232.5, 40.0, 30.0), "unconstrained: centre scales");
    let e = apply_liquid(&request(&doc, &r, "page-1", 30.0, 200.0, ctx(&[])), &Default::default()).unwrap_err();
    assert_eq!(e.code(), "invalid_geometry");
}

/// `liquid.guide_based`: a vertical liquid guide at x = 100 crosses frame 1 and graphic 9, which
/// stretch with the page (x1.5); frame 2 is not crossed and keeps its width. The story in frame 1 is
/// overset at 160 pt and fits at 240 pt after reflow; the graphic is refitted without distortion.
#[test]
fn guide_crossed_frames_stretch_and_text_reflows() {
    let mut doc = document(
        vec![
            frame_layer(1, 1, spec(0, 20.0, 20.0, 160.0, 40.0), None, None),
            frame_layer(2, 2, spec(0, 20.0, 100.0, 60.0, 40.0), None, None),
            item_layer(9, on_spread()),
        ],
        [PageSide::RightHand, PageSide::LeftHand],
    );
    doc.revision = 2;
    let mut grids = StudioLayoutGrids::new(id("SDOC", 1));
    let guide = |gid: &str, page: Option<&str>, o: StudioGuideOrientation, at: f64, t: StudioGuideType| StudioRulerGuide {
        guide_id: gid.into(),
        page_spread_id: id("SPGS", 1),
        page_id: page.map(Into::into),
        orientation: o,
        location: len(at),
        guide_colour: StudioRgba { r: 0.0, g: 0.0, b: 1.0, a: 1.0 },
        guide_type: t,
        guide_view_threshold: 5.0,
        fit_to_page: false,
        locked: false,
        overridden: false,
        allow_overrides: false,
    };
    grids.guides.push(guide("liquid", Some("page-1"), StudioGuideOrientation::Vertical, 100.0, StudioGuideType::Liquid));
    grids.guides.push(guide("ruler", Some("page-1"), StudioGuideOrientation::Vertical, 50.0, StudioGuideType::Ruler));
    grids.guides.push(guide("spread", None, StudioGuideOrientation::Horizontal, 120.0, StudioGuideType::Liquid));
    let mut graphics = StudioLongDocRecords::new(id("SDOC", 1));
    graphics.graphic_frames.push(StudioGraphicFrame {
        layer_id: id("SLYR", 9),
        page_id: "page-1".into(),
        bounds: Bounds { x: len(20.0), y: len(150.0), width: len(160.0), height: len(40.0) },
        shape: StudioFrameShape::Rectangle,
        content_width: len(160.0),
        content_height: len(40.0),
        fit: Some(StudioFit::ContentToFrame),
        frame_fitting: None,
        anchor: StudioFitAnchor::Center,
        crop: None,
    });
    let text = words(20);
    let (st, paras) = story(1, &text, &[(0, text.len() as u64)]);
    let before = run(&doc, &st, &paras, &records()).unwrap();
    assert!(before.overset.is_some(), "overset at 160 pt");
    let inputs = [StoryInput { story: &st, resolved_text: None, paragraphs: &paras }];
    let r = rules(&[("page-1", StudioLiquidRule::GuideBased)]);
    let mut q = request(&doc, &r, "page-1", 300.0, 200.0, ctx(&inputs));
    q.grids = Some(&grids);
    q.graphics = Some(&graphics);
    let p = apply_liquid(&q, &Default::default()).expect("guide based");
    assert_eq!(after(&p, 1), rect(30.0, 20.0, 240.0, 40.0), "crossed: stretched");
    assert_eq!(after(&p, 2), rect(45.0, 100.0, 60.0, 40.0), "not crossed: width kept, centre scaled");
    assert_eq!(after(&p, 9), rect(30.0, 150.0, 240.0, 40.0));
    let refit = &p.refits[0];
    assert_eq!(refit.fit, Some(StudioFit::Proportionally), "content_to_frame would distort");
    assert_eq!((refit.fitted.transform[0], refit.fitted.transform[3]), (1.0, 1.0), "uniform scale");
    assert_eq!(refit.fitted.content_box, Rect { x0_pt: 40.0, y0_pt: 0.0, x1_pt: 200.0, y1_pt: 40.0 });
    assert_eq!(p.graphics.as_ref().unwrap().graphic_frames[0].fit, Some(StudioFit::Proportionally));
    assert_eq!(p.reflows[0].overset, None, "text reflowed into the stretched frame");
    assert_eq!(p.reflows[0].proposal, run(&p.document, &st, &paras, &records()).unwrap());
    // Shrinking reports overset on the chain's last frame instead of dropping text.
    let p = apply_liquid(&request(&doc, &r, "page-1", 150.0, 200.0, ctx(&inputs)), &Default::default()).expect("shrink");
    let (frame, span) = p.reflows[0].overset.clone().expect("overset");
    assert_eq!(frame, id("SLYR", 1));
    assert_eq!(span.end, text.len() as u64);
}

/// `liquid.use_parent`: page 1 applies parent B (use_parent) based on A (recenter): the recenter
/// geometry is applied; without parent records the page falls back to off.
#[test]
fn use_parent_follows_the_parent_rule() {
    let doc = document(vec![frame_layer(1, 1, spec(0, 50.0, 50.0, 60.0, 60.0), None, None)], [PageSide::RightHand, PageSide::LeftHand]);
    let mut parents = StudioParentPageRecords::new(id("SDOC", 1));
    for (pid, based) in [("A", None), ("B", Some("A"))] {
        parents.parents.push(StudioParentPage {
            schema_id: ParentPageSchema::V1,
            parent_id: pid.into(),
            name: pid.into(),
            based_on: based.map(Into::into),
            parent_page_items: vec![],
        });
    }
    parents.page_bindings.push(StudioPageParentBinding { page_id: "page-1".into(), applied_parent: Some("B".into()), override_list: vec![] });
    let mut r = rules(&[("page-1", StudioLiquidRule::UseParent)]);
    r.parent_rules.push(StudioParentLiquidRule { parent_id: "B".into(), liquid_rule: StudioLiquidRule::UseParent });
    r.parent_rules.push(StudioParentLiquidRule { parent_id: "A".into(), liquid_rule: StudioLiquidRule::Recenter });
    validate_layout_rule_records(&r, &doc, Some(&parents), hsk_studio_folio::Budget::default(), &Default::default(), &Unresolved).expect("records");
    let mut q = request(&doc, &r, "page-1", 300.0, 260.0, ctx(&[]));
    q.parents = Some(&parents);
    let p = apply_liquid(&q, &Default::default()).expect("use parent");
    assert_eq!(p.rule, StudioLiquidRule::Recenter);
    assert_eq!(after(&p, 1), rect(100.0, 80.0, 60.0, 60.0));
    let p = apply_liquid(&request(&doc, &r, "page-1", 300.0, 260.0, ctx(&[])), &Default::default()).expect("no parents");
    assert_eq!(p.rule, StudioLiquidRule::Off);
    assert!(p.objects.is_empty());
}
