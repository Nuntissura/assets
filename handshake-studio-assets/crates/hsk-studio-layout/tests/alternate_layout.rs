//! SMB-ID-LIQUID-LAYOUT-1 alternate layouts (STU-LAY-011) and linked content (STU-LAY-019,
//! STU-TYP-125). Pages 200 x 200 pt, 10 pt margins; toy font 6 pt per glyph. The page-navigation
//! side-by-side display is a GUI surface: NOT_PROVEN here (no harness-view hunk in this lane).
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
fn graphic(layer: u32, page: &str) -> StudioGraphicFrame {
    StudioGraphicFrame {
        layer_id: id("SLYR", layer),
        page_id: page.into(),
        bounds: Bounds { x: len(20.0), y: len(150.0), width: len(160.0), height: len(40.0) },
        shape: StudioFrameShape::Rectangle,
        content_width: len(160.0),
        content_height: len(40.0),
        fit: Some(StudioFit::Proportionally),
        frame_fitting: None,
        anchor: StudioFitAnchor::Center,
        crop: None,
    }
}
struct Fixture {
    doc: StudioDocument,
    graphics: StudioLongDocRecords,
    stories: Vec<(StudioTextStory, Vec<ShapedParagraph>)>,
}
fn fixture() -> Fixture {
    let doc = document(
        vec![
            frame_layer(1, 1, spec(0, 20.0, 20.0, 160.0, 40.0), None, None),
            frame_layer(2, 2, spec(1, 20.0, 20.0, 100.0, 40.0), None, None),
            item_layer(9, on_spread()),
        ],
        [PageSide::RightHand, PageSide::LeftHand],
    );
    let mut graphics = StudioLongDocRecords::new(id("SDOC", 1));
    graphics.graphic_frames.push(graphic(9, "page-1"));
    let t1 = "word word word word word word";
    let stories = vec![story(1, t1, &[(0, t1.len() as u64)]), story(2, "tiny", &[(0, 4)])];
    Fixture { doc, graphics, stories }
}
fn inputs(stories: &[(StudioTextStory, Vec<ShapedParagraph>)]) -> Vec<StoryInput<'_>> {
    stories.iter().map(|(s, p)| StoryInput { story: s, resolved_text: None, paragraphs: p }).collect()
}
fn ids() -> impl FnMut(&str) -> String {
    let mut n = 100u32;
    move |prefix: &str| {
        n += 1;
        id(prefix, n)
    }
}
#[allow(clippy::too_many_arguments)]
fn request<'a>(
    doc: &'a StudioDocument,
    records: &'a StudioLayoutRuleRecords,
    graphics: Option<&'a StudioLongDocRecords>,
    source: Option<&'a str>,
    section: &'a str,
    name: &'a str,
    policy: StudioLinkUpdatePolicy,
    c: RuleContext<'a>,
) -> AlternateLayoutRequest<'a> {
    AlternateLayoutRequest {
        document: doc,
        records,
        parents: None,
        grids: None,
        graphics,
        source_section: source,
        section_id: section,
        name,
        width_pt: 100.0,
        height_pt: 200.0,
        liquid_rule: StudioLiquidRule::Scale,
        story_options: StudioLinkedStoryOptions { update_policy: policy, remove_forced_line_breaks: false },
        item_options: StudioLinkedPageItemOptions { update_policy: policy },
        page_key_prefix: if section == "phone" { "alt-" } else { "tab-" },
        expected_revision: 2,
        context: c,
    }
}
fn child_layer(p: &AlternateLayoutProposal, parent: u32) -> &StudioLayer {
    let pid = id("SLYR", parent);
    let child = p
        .records
        .linked_page_items
        .iter()
        .find(|l| l.parent_layer_id == pid)
        .map(|l| l.child_layer_id.clone())
        .unwrap_or_else(|| {
            let story = match &p.document.layers.iter().find(|l| l.layer_id == pid).unwrap().payload {
                Payload::Text { story_id } => story_id.clone(),
                _ => unreachable!(),
            };
            let child_story = p.records.linked_stories.iter().find(|l| l.parent_story_id == story).unwrap().child_story_id.clone();
            p.document
                .layers
                .iter()
                .find(|l| matches!(&l.payload, Payload::Text { story_id } if *story_id == child_story))
                .unwrap()
                .layer_id
                .clone()
        });
    p.document.layers.iter().find(|l| l.layer_id == child).unwrap()
}

/// The primary layout's two pages are copied at 100 x 200 pt into a new spread below the last one
/// and recorded as a section {alternate_layout, alternate_layout_length = 2, page_start}; objects
/// are placed by `liquid.scale` (x0.5); Folio validates the proposal; name rules and threads that
/// leave the copied pages are typed refusals; an alternate layout can be made from a section.
#[test]
fn alternate_layout_is_a_section_with_its_page_count() {
    let f = fixture();
    let ins = inputs(&f.stories);
    let records = StudioLayoutRuleRecords::new(id("SDOC", 1));
    let mut new_id = ids();
    let req = request(&f.doc, &records, Some(&f.graphics), None, "phone", "Phone V", StudioLinkUpdatePolicy::AutoUpdate, ctx(&ins));
    let p = create_alternate_layout(&req, &mut new_id, &Default::default()).expect("create");
    assert_eq!(p.pages, vec!["alt-1".to_string(), "alt-2".to_string()]);
    assert_eq!(
        p.records.alternate_layouts,
        vec![StudioAlternateLayout { section_id: "phone".into(), alternate_layout: "Phone V".into(), alternate_layout_length: 2, page_start: "alt-1".into() }]
    );
    assert_eq!(p.document.page_spreads.len(), 2);
    let spread = &p.document.page_spreads[1];
    assert_eq!((spread.bounds.y.value, spread.bounds.width.value, spread.bounds.height.value), (200.0 + ALTERNATE_SPREAD_GAP_PT, 200.0, 200.0));
    assert!(spread.pages.iter().all(|pg| pg.width.value == 100.0 && pg.height.value == 200.0));
    validate_layout_rule_records(&p.records, &p.document, None, hsk_studio_folio::Budget::default(), &Default::default(), &Unresolved).expect("valid");
    assert_eq!(p.records.page_rules.iter().map(|r| (r.page_id.as_str(), r.liquid_rule)).collect::<Vec<_>>(), vec![("alt-1", StudioLiquidRule::Scale), ("alt-2", StudioLiquidRule::Scale)]);
    // Children: text frames show child stories, the graphic is a linked page item.
    let c1 = child_layer(&p, 1);
    let frame = c1.text_frame.as_ref().unwrap();
    assert_eq!((frame.page_id.as_str(), frame.bounds.x.value, frame.bounds.y.value, frame.bounds.width.value, frame.bounds.height.value), ("alt-1", 10.0, 10.0, 80.0, 20.0));
    assert_eq!(c1.parent, Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: spread.page_spread_id.clone() }));
    assert_eq!(p.child_stories.len(), 2);
    assert_eq!(p.records.linked_stories.len(), 2);
    assert_eq!(p.records.linked_page_items.len(), 1);
    let g = p.graphics.as_ref().unwrap().graphic_frames.last().unwrap();
    assert_eq!((g.page_id.as_str(), g.bounds.x.value, g.bounds.y.value, g.bounds.width.value), ("alt-1", 10.0, 75.0, 80.0));
    assert_eq!(f.doc.layers.len() + 3, p.document.layers.len());
    // Child reflow: the narrower frame oversets, reported on its last frame.
    let r1 = p.reflows.iter().find(|r| r.story_id == p.child_stories[0].story_id).unwrap();
    assert_eq!(r1.overset.as_ref().map(|o| o.0.clone()), Some(c1.layer_id.clone()));
    assert_eq!(p.base_revision, 2);

    // Name rules, duplicates, unknown sections.
    for (name, code) in [("Phone:", "alternate_layout_suffix"), ("Phone*", "alternate_layout_suffix"), ("  ", "alternate_layout_blank")] {
        let req = request(&f.doc, &records, Some(&f.graphics), None, "phone", name, StudioLinkUpdatePolicy::AutoUpdate, ctx(&ins));
        assert_eq!(create_alternate_layout(&req, &mut new_id, &Default::default()).unwrap_err().code(), code);
    }
    let mut stories2 = f.stories.clone();
    for c in &p.child_stories {
        let paras = f.stories.iter().find(|(s, _)| p.records.linked_story(&c.story_id).unwrap().parent_story_id == s.story_id).unwrap().1.clone();
        stories2.push((c.clone(), paras));
    }
    let ins2 = inputs(&stories2);
    let dup = request(&p.document, &p.records, p.graphics.as_ref(), Some("phone"), "tablet", "Phone V", StudioLinkUpdatePolicy::AutoUpdate, ctx(&ins2));
    let e = create_alternate_layout(&dup, &mut new_id, &Default::default()).unwrap_err();
    assert!(matches!(e, LayoutRuleError::Folio { code: Code::DuplicateIdentity, .. }), "{e:?}");
    let missing = request(&p.document, &p.records, p.graphics.as_ref(), Some("nope"), "tablet", "Tablet", StudioLinkUpdatePolicy::AutoUpdate, ctx(&ins2));
    assert_eq!(create_alternate_layout(&missing, &mut new_id, &Default::default()).unwrap_err().code(), "section_missing");
    let from_section = request(&p.document, &p.records, p.graphics.as_ref(), Some("phone"), "tablet", "Tablet H", StudioLinkUpdatePolicy::AutoUpdate, ctx(&ins2));
    let q = create_alternate_layout(&from_section, &mut new_id, &Default::default()).expect("from section");
    assert_eq!(q.pages, vec!["tab-1".to_string(), "tab-2".to_string()]);
    assert_eq!(q.records.alternate_layouts.len(), 2);
    assert_eq!(q.document.page_spreads.len(), 3);
    // A story whose parent was not supplied, and a thread leaving the copied pages.
    let req = request(&f.doc, &records, Some(&f.graphics), None, "phone", "Phone V", StudioLinkUpdatePolicy::AutoUpdate, ctx(&ins[..1]));
    assert_eq!(create_alternate_layout(&req, &mut new_id, &Default::default()).unwrap_err().code(), "story_missing");
    let threaded = document(
        vec![frame_layer(1, 3, spec(0, 20.0, 20.0, 160.0, 40.0), None, Some(2)), frame_layer(2, 3, spec(1, 20.0, 20.0, 160.0, 40.0), Some(1), None)],
        [PageSide::RightHand, PageSide::LeftHand],
    );
    let mut split = StudioLayoutRuleRecords::new(id("SDOC", 1));
    split.alternate_layouts.push(StudioAlternateLayout { section_id: "s".into(), alternate_layout: "Second".into(), alternate_layout_length: 1, page_start: "page-2".into() });
    let req = request(&threaded, &split, None, None, "phone", "Phone V", StudioLinkUpdatePolicy::AutoUpdate, ctx(&[]));
    assert_eq!(create_alternate_layout(&req, &mut new_id, &Default::default()).unwrap_err().code(), "thread_leaves_layout");
}

/// `auto_update`: editing the parent makes the link out of date; the update overwrites the child
/// from the parent and re-syncs it. A linked page item updates its content, not its geometry.
#[test]
fn linked_story_goes_out_of_date_and_updates() {
    let f = fixture();
    let ins = inputs(&f.stories);
    let records = StudioLayoutRuleRecords::new(id("SDOC", 1));
    let req = request(&f.doc, &records, Some(&f.graphics), None, "phone", "Phone V", StudioLinkUpdatePolicy::AutoUpdate, ctx(&ins));
    let p = create_alternate_layout(&req, &mut ids(), &Default::default()).expect("create");
    let link = p.records.linked_story(&p.child_stories[0].story_id).unwrap().clone();
    assert_eq!(link.parent_story_id, id("STXT", 1));
    assert_eq!(link.state(&f.stories[0].0), StudioLinkState::UpToDate);
    let (edited, _) = story(1, "edited parent text", &[(0, 18)]);
    assert_eq!(link.state(&edited), StudioLinkState::OutOfDate);
    let all = vec![edited.clone(), f.stories[1].0.clone(), p.child_stories[0].clone(), p.child_stories[1].clone()];
    let u = update_linked_stories(&p.records, &all, &[]).expect("update");
    let outcome = |c: &str| u.updates.iter().find(|x| x.child_id == c).unwrap().outcome;
    assert_eq!(outcome(&p.child_stories[0].story_id), LinkOutcome::Updated);
    assert_eq!(outcome(&p.child_stories[1].story_id), LinkOutcome::UpToDate);
    let child = u.stories.iter().find(|s| s.story_id == p.child_stories[0].story_id).unwrap();
    assert_eq!((&child.content, &child.paragraphs), (&edited.content, &edited.paragraphs));
    assert_eq!(u.records.linked_story(&child.story_id).unwrap().state(&edited), StudioLinkState::UpToDate);
    assert_eq!(update_linked_stories(&p.records, &all[..2], &[]).unwrap_err().code(), "story_missing");

    let mut doc = p.document.clone();
    let parent = doc.layers.iter_mut().find(|l| l.layer_id == id("SLYR", 9)).unwrap();
    parent.visible = false;
    let (r2, d2, ups) = update_linked_page_items(&p.records, &doc, &[]).expect("items");
    assert_eq!(ups[0].outcome, LinkOutcome::Updated);
    let child = d2.layers.iter().find(|l| l.layer_id == ups[0].child_id).unwrap();
    assert!(!child.visible);
    assert_eq!(r2.linked_page_items[0].state(d2.layers.iter().find(|l| l.layer_id == id("SLYR", 9)).unwrap()), StudioLinkState::UpToDate);
    let before = p.document.layers.iter().find(|l| l.layer_id == ups[0].child_id).unwrap();
    assert_eq!((&child.parent, &child.name), (&before.parent, &before.name), "geometry/identity kept");
}

/// `warn_on_change`: an out-of-date child is reported (Warned) and NOT overwritten; once the caller
/// accepts the warning for that child, it updates.
#[test]
fn warn_on_parent_change_does_not_overwrite() {
    let f = fixture();
    let ins = inputs(&f.stories);
    let records = StudioLayoutRuleRecords::new(id("SDOC", 1));
    let req = request(&f.doc, &records, Some(&f.graphics), None, "phone", "Phone V", StudioLinkUpdatePolicy::WarnOnChange, ctx(&ins));
    let p = create_alternate_layout(&req, &mut ids(), &Default::default()).expect("create");
    let child_id = p.child_stories[0].story_id.clone();
    let (edited, _) = story(1, "edited parent text", &[(0, 18)]);
    let all = vec![edited.clone(), f.stories[1].0.clone(), p.child_stories[0].clone(), p.child_stories[1].clone()];
    let u = update_linked_stories(&p.records, &all, &[]).expect("update");
    assert_eq!(u.updates.iter().find(|x| x.child_id == child_id).unwrap().outcome, LinkOutcome::Warned);
    let child = u.stories.iter().find(|s| s.story_id == child_id).unwrap();
    assert_eq!(child, &p.child_stories[0], "not overwritten");
    assert_eq!(u.records.linked_story(&child_id).unwrap().state(&edited), StudioLinkState::OutOfDate);
    let u = update_linked_stories(&p.records, &all, std::slice::from_ref(&child_id)).expect("accepted");
    assert_eq!(u.updates.iter().find(|x| x.child_id == child_id).unwrap().outcome, LinkOutcome::Updated);
    assert_eq!(u.stories.iter().find(|s| s.story_id == child_id).unwrap().content, edited.content);

    let mut doc = p.document.clone();
    doc.layers.iter_mut().find(|l| l.layer_id == id("SLYR", 9)).unwrap().visible = false;
    let (_, d2, ups) = update_linked_page_items(&p.records, &doc, &[]).expect("items");
    assert_eq!(ups[0].outcome, LinkOutcome::Warned);
    assert!(d2.layers.iter().find(|l| l.layer_id == ups[0].child_id).unwrap().visible, "page item not overwritten");
}
