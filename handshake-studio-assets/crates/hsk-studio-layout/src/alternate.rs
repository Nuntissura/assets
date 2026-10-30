//! Alternate layouts and linked content (lane SMB-ID-LIQUID-LAYOUT-1; STU-LAY-011, STU-LAY-019,
//! STU-TYP-125). An alternate layout is a SECTION of the one `StudioDocument` (Folio
//! `StudioAlternateLayout`: `alternate_layout` name, `alternate_layout_length` page count).
//!
//! [`create_alternate_layout`] copies the pages of a source layout (a named alternate-layout section,
//! or the primary layout = pages outside every section) into new page spreads appended to the
//! document, at the requested page size, and records them as a new section. Every copied text frame
//! shows a CHILD copy of its story, linked back to the parent with the linked-story options; every
//! copied graphic frame is a child layer linked back with the linked-page-item options. Objects are
//! placed by the chosen liquid rule from the source page size to the new size ([`crate::liquid`]);
//! `preserve_existing` uses each source page's effective rule, `use_parent` the source page's parent
//! rule; the new pages store the rule that was applied. Object constraints are copied to the child
//! layers. The proposal is validated by Folio and the child stories are reflowed.
//!
//! [`update_linked_stories`] and [`update_linked_page_items`] propagate parent edits: an
//! out-of-date link with `auto_update` (or one the caller accepts) is overwritten from its parent and
//! re-synced; one with `warn_on_change` is reported as warned and NOT overwritten. A linked story
//! update replaces the child's text and partitions (its id stays); a linked page item update
//! replaces the child layer's kind, payload, visibility and appearance and keeps its id, name,
//! parent and geometry, so the two option objects have different update semantics.
//!
//! Declared readings and gaps (InDesign UNVERIFIED, D5): new spreads are placed below the last
//! spread (72 pt apart) on the pasteboard; a thread leaving the copied pages is refused
//! (`thread_leaves_layout`); guides, parent bindings and layers that are neither text frames nor
//! graphic frames are not copied; `remove_forced_line_breaks` is carried on the record (applying it
//! changes byte offsets and needs a Type re-shape: NOT implemented); the flex/container mode of
//! STU-LAY-011 consumes `StudioAutoLayout` and is outside this slice; the page-navigation side-by-side
//! display is a GUI surface (later lane).
#![deny(clippy::arithmetic_side_effects)]
use crate::folio_input::pt;
use crate::liquid::{
    LayoutRuleError, PageObjectChange, PageObject, PageObjectKind, RuleContext, StoryInput, StoryReflow, Work, check_revision, finish,
    len, liquid_guides, liquid_rect, page_objects, page_size,
};
use crate::LayoutError;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    Bounds, NodeKind, NodeRef, PageSpreadSchema, Parent, Payload, StudioAlternateLayout, StudioDocument, StudioLayoutGrids,
    StudioLayoutRuleRecords, StudioLinkUpdatePolicy, StudioLinkedPageItem, StudioLinkedPageItemOptions, StudioLinkedStory,
    StudioLinkedStoryOptions, StudioLiquidRule, StudioLongDocRecords, StudioObjectLiquidConstraint, StudioPageLiquidRule,
    StudioPageSpread, StudioParentPageRecords, StudioTextStory, check_alternate_layout_name, linked_page_item_fingerprint,
    linked_story_fingerprint, resolve_liquid_rule,
};
use std::collections::{BTreeMap, BTreeSet};

/// Pasteboard gap between the last spread and a new alternate-layout spread, pt.
pub const ALTERNATE_SPREAD_GAP_PT: f64 = 72.0;

/// One create-alternate-layout command.
#[derive(Clone)]
pub struct AlternateLayoutRequest<'a> {
    pub document: &'a StudioDocument,
    pub records: &'a StudioLayoutRuleRecords,
    pub parents: Option<&'a StudioParentPageRecords>,
    pub grids: Option<&'a StudioLayoutGrids>,
    pub graphics: Option<&'a StudioLongDocRecords>,
    /// Alternate-layout section to copy; `None` = the primary layout.
    pub source_section: Option<&'a str>,
    /// Key of the new section.
    pub section_id: &'a str,
    /// The alternate-layout name (not blank, unique, not ending with `*` or `:`).
    pub name: &'a str,
    /// Page size of the new layout, pt (orientation = which side is longer).
    pub width_pt: f64,
    pub height_pt: f64,
    pub liquid_rule: StudioLiquidRule,
    pub story_options: StudioLinkedStoryOptions,
    pub item_options: StudioLinkedPageItemOptions,
    /// New page keys are `<prefix><n>`, n from 1.
    pub page_key_prefix: &'a str,
    pub expected_revision: u64,
    /// `stories` holds every parent story shown on the source pages (with its shaped paragraphs).
    pub context: RuleContext<'a>,
}

/// The create proposal; nothing is written until the caller accepts it.
#[derive(Clone, Debug, PartialEq)]
pub struct AlternateLayoutProposal {
    pub base_revision: u64,
    pub document: StudioDocument,
    pub records: StudioLayoutRuleRecords,
    pub graphics: Option<StudioLongDocRecords>,
    /// The child story copies, one per parent story.
    pub child_stories: Vec<StudioTextStory>,
    /// New page keys in order.
    pub pages: Vec<String>,
    /// Child objects: `before` = the parent's rect on its source page, `after` = the placed rect.
    pub objects: Vec<PageObjectChange>,
    pub reflows: Vec<StoryReflow>,
}

fn source_pages(r: &AlternateLayoutRequest<'_>) -> Result<Vec<(usize, String)>, LayoutRuleError> {
    let order: Vec<(usize, &str)> = r
        .document
        .page_spreads
        .iter()
        .enumerate()
        .flat_map(|(si, s)| s.pages.iter().map(move |p| (si, p.page_id.as_str())))
        .collect();
    let span = |a: &StudioAlternateLayout| -> Option<(usize, usize)> {
        let start = order.iter().position(|(_, p)| *p == a.page_start)?;
        let end = start.checked_add(usize::try_from(a.alternate_layout_length).ok()?)?;
        Some((start, end.min(order.len())))
    };
    let picked: Vec<(usize, String)> = match r.source_section {
        Some(id) => {
            let a = r.records.alternate_layout(id).ok_or_else(|| LayoutRuleError::SectionMissing(id.chars().take(128).collect()))?;
            let (s, e) = span(a).ok_or(LayoutError::InvalidInput)?;
            order[s..e].iter().map(|(si, p)| (*si, (*p).to_owned())).collect()
        }
        None => {
            let spans: Vec<(usize, usize)> = r.records.alternate_layouts.iter().filter_map(span).collect();
            order
                .iter()
                .enumerate()
                .filter(|(i, _)| !spans.iter().any(|(s, e)| i >= s && i < e))
                .map(|(_, (si, p))| (*si, (*p).to_owned()))
                .collect()
        }
    };
    if picked.is_empty() {
        return Err(LayoutRuleError::ScopeEmpty);
    }
    Ok(picked)
}

fn map_parent(p: &Parent, spreads: &BTreeMap<String, String>, layer: &str) -> Result<Parent, LayoutRuleError> {
    match p {
        Parent::Root(()) => Ok(Parent::Root(())),
        Parent::Node(n) if n.node_kind == NodeKind::PageSpread => spreads
            .get(&n.node_id)
            .map(|id| Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: id.clone() }))
            .ok_or_else(|| LayoutRuleError::UnsupportedParent(layer.to_owned())),
        Parent::Node(_) => Err(LayoutRuleError::UnsupportedParent(layer.to_owned())),
    }
}

/// Creates an alternate layout (module docs). `new_id(prefix)` returns a fresh Accord id for
/// `SPGS` (spreads), `SLYR` (layers) and `STXT` (child stories); Folio validates them.
pub fn create_alternate_layout(
    req: &AlternateLayoutRequest<'_>,
    new_id: &mut dyn FnMut(&str) -> String,
    cancel: &CancellationToken,
) -> Result<AlternateLayoutProposal, LayoutRuleError> {
    cancel.check().map_err(|_| LayoutError::Canceled)?;
    check_revision(req.document, req.expected_revision)?;
    if req.records.document_id != req.document.document_id {
        return Err(LayoutError::InvalidInput.into());
    }
    check_alternate_layout_name(req.name).map_err(LayoutRuleError::AlternateLayoutName)?;
    let (w2, h2) = (req.width_pt, req.height_pt);
    if !(w2.is_finite() && h2.is_finite() && w2 > 0.0 && h2 > 0.0) {
        return Err(LayoutError::InvalidGeometry.into());
    }
    let pages = source_pages(req)?;

    // Text frames and graphic frames of the source pages; threads must stay inside them.
    let mut objects: Vec<(String, PageObject)> = Vec::new();
    for (_, p) in &pages {
        for o in page_objects(req.document, req.graphics, p)? {
            objects.push((p.clone(), o));
        }
    }
    let copied: BTreeSet<&str> = objects.iter().map(|(_, o)| o.layer_id.as_str()).collect();
    let mut parent_stories: Vec<&str> = Vec::new();
    for (_, o) in objects.iter().filter(|(_, o)| o.kind == PageObjectKind::TextFrame) {
        let l = &req.document.layers[o.index];
        let f = l.text_frame.as_ref().ok_or(LayoutError::InvalidInput)?;
        for link in [&f.previous_text_frame, &f.next_text_frame].into_iter().flatten() {
            if !copied.contains(link.as_str()) {
                return Err(LayoutRuleError::ThreadLeavesLayout(l.layer_id.clone()));
            }
        }
        if let Payload::Text { story_id } = &l.payload
            && !parent_stories.contains(&story_id.as_str())
        {
            parent_stories.push(story_id);
        }
    }
    let inputs: Vec<StoryInput<'_>> = parent_stories
        .iter()
        .map(|s| {
            req.context
                .stories
                .iter()
                .find(|i| i.story.story_id == *s)
                .copied()
                .ok_or_else(|| LayoutRuleError::StoryMissing((*s).to_owned()))
        })
        .collect::<Result<_, _>>()?;

    // Identities.
    let mut spread_ids: BTreeMap<String, String> = BTreeMap::new();
    for (si, _) in &pages {
        let src = &req.document.page_spreads[*si].page_spread_id;
        if !spread_ids.contains_key(src) {
            spread_ids.insert(src.clone(), new_id("SPGS"));
        }
    }
    let layer_ids: BTreeMap<String, String> = objects.iter().map(|(_, o)| (o.layer_id.clone(), new_id("SLYR"))).collect();
    let story_ids: BTreeMap<&str, String> = parent_stories.iter().map(|s| (*s, new_id("STXT"))).collect();
    let mut page_keys: BTreeMap<&str, String> = BTreeMap::new();
    for (n, (_, p)) in (1u32..).zip(&pages) {
        page_keys.insert(p.as_str(), format!("{}{n}", req.page_key_prefix));
    }

    // New spreads below the last one.
    let mut doc = req.document.clone();
    let mut y = 0.0f64;
    for s in &req.document.page_spreads {
        y = y.max(pt(&s.bounds.y)? + pt(&s.bounds.height)?);
    }
    y += ALTERNATE_SPREAD_GAP_PT;
    let mut new_spreads: Vec<StudioPageSpread> = Vec::new();
    for (si, p) in &pages {
        let src = &req.document.page_spreads[*si];
        let id = spread_ids.get(&src.page_spread_id).ok_or(LayoutError::InvalidInput)?;
        if new_spreads.last().is_none_or(|s| s.page_spread_id != *id) {
            if let Some(last) = new_spreads.last() {
                y += pt(&last.bounds.height)? + ALTERNATE_SPREAD_GAP_PT;
            }
            new_spreads.push(StudioPageSpread {
                schema_id: PageSpreadSchema::V1,
                page_spread_id: id.clone(),
                name: req.name.to_owned(),
                parent: Parent::Root(()),
                bounds: Bounds { x: len(0.0), y: len(y), width: len(0.0), height: len(h2) },
                pages: Vec::new(),
            });
        }
        let source = req.document.locate_page(p).ok_or_else(|| LayoutRuleError::PageMissing(p.clone()))?.page;
        let mut page = source.clone();
        page.page_id = page_keys.get(p.as_str()).cloned().ok_or(LayoutError::InvalidInput)?;
        page.width = len(w2);
        page.height = len(h2);
        let spread = new_spreads.last_mut().ok_or(LayoutError::InvalidInput)?;
        spread.bounds.width = len(pt(&spread.bounds.width)? + w2);
        spread.pages.push(page);
    }
    doc.page_spreads.extend(new_spreads);

    // Child layers at their parent's geometry first, then placed by the liquid rule.
    let mut records = req.records.clone();
    let mut graphics = req.graphics.cloned();
    let mut placements: Vec<(String, PageObject, StudioLiquidRule, (f64, f64))> = Vec::new();
    let mut rules: BTreeMap<&str, StudioLiquidRule> = BTreeMap::new();
    for (_, p) in &pages {
        let rule = match req.liquid_rule {
            StudioLiquidRule::PreserveExisting => resolve_liquid_rule(req.records, req.parents, p),
            StudioLiquidRule::UseParent => {
                let mut probe = req.records.clone();
                probe.apply_page_rules(&[(p.clone(), StudioLiquidRule::UseParent)]);
                resolve_liquid_rule(&probe, req.parents, p)
            }
            r => r,
        };
        rules.insert(p.as_str(), rule);
    }
    for (src_page, o) in &objects {
        cancel.check().map_err(|_| LayoutError::Canceled)?;
        let child_id = layer_ids.get(&o.layer_id).cloned().ok_or(LayoutError::InvalidInput)?;
        let new_page = page_keys.get(src_page.as_str()).cloned().ok_or(LayoutError::InvalidInput)?;
        let src_layer = req.document.layers.iter().find(|l| l.layer_id == o.layer_id).ok_or(LayoutError::InvalidInput)?;
        let mut layer = src_layer.clone();
        layer.layer_id = child_id.clone();
        layer.parent = map_parent(&src_layer.parent, &spread_ids, &src_layer.layer_id)?;
        let index = match o.kind {
            PageObjectKind::TextFrame => {
                if let Payload::Text { story_id } = &mut layer.payload {
                    *story_id = story_ids.get(story_id.as_str()).cloned().ok_or(LayoutError::InvalidInput)?;
                }
                let f = layer.text_frame.as_mut().ok_or(LayoutError::InvalidInput)?;
                f.page_id = new_page.clone();
                let remap = |l: &Option<String>| -> Result<Option<String>, LayoutRuleError> {
                    l.as_ref().map(|id| layer_ids.get(id).cloned().ok_or(LayoutRuleError::ThreadLeavesLayout(id.clone()))).transpose()
                };
                f.previous_text_frame = remap(&f.previous_text_frame)?;
                f.next_text_frame = remap(&f.next_text_frame)?;
                doc.layers.len()
            }
            PageObjectKind::GraphicFrame => {
                let g = graphics.as_mut().ok_or(LayoutError::InvalidInput)?;
                let mut frame = g.graphic_frames.get(o.index).cloned().ok_or(LayoutError::InvalidInput)?;
                frame.layer_id = child_id.clone();
                frame.page_id = new_page.clone();
                g.graphic_frames.push(frame);
                records.linked_page_items.push(StudioLinkedPageItem {
                    child_layer_id: child_id.clone(),
                    parent_layer_id: src_layer.layer_id.clone(),
                    options: req.item_options.clone(),
                    synced_parent_fingerprint: linked_page_item_fingerprint(src_layer)?,
                });
                g.graphic_frames.len().checked_sub(1).ok_or(LayoutError::InvalidInput)?
            }
        };
        doc.layers.push(layer);
        if let Some(c) = req.records.object_constraint(&o.layer_id) {
            records.object_constraints.push(StudioObjectLiquidConstraint { layer_id: child_id.clone(), ..c.clone() });
        }
        let rule = rules.get(src_page.as_str()).copied().unwrap_or(StudioLiquidRule::Off);
        let size = page_size(req.document, src_page)?;
        placements.push((new_page, PageObject { kind: o.kind, index, layer_id: child_id, rect: o.rect }, rule, size));
    }
    let mut work = Work { document: doc, graphics, objects: Vec::new(), refits: Vec::new() };
    for ((new_page, obj, rule, old), (src_page, src)) in placements.iter().zip(&objects) {
        let guides = liquid_guides(req.document, req.grids, src_page)?;
        let after = liquid_rect(*rule, records.object_constraint(&obj.layer_id), &guides, *old, (w2, h2), src.rect)?;
        work.place(new_page, obj, after)?;
    }
    // Unchanged placements still report the child geometry.
    for (new_page, obj, _, _) in &placements {
        if !work.objects.iter().any(|c| c.layer_id == obj.layer_id) {
            work.objects.push(PageObjectChange { layer_id: obj.layer_id.clone(), page_id: new_page.clone(), kind: obj.kind, before: obj.rect, after: obj.rect });
        }
    }

    // Child stories, links, section and page rules.
    let mut child_stories = Vec::new();
    for input in &inputs {
        let parent = input.story;
        let mut child = parent.clone();
        child.story_id = story_ids.get(parent.story_id.as_str()).cloned().ok_or(LayoutError::InvalidInput)?;
        records.linked_stories.push(StudioLinkedStory {
            child_story_id: child.story_id.clone(),
            parent_story_id: parent.story_id.clone(),
            options: req.story_options.clone(),
            synced_parent_fingerprint: linked_story_fingerprint(parent)?,
        });
        child_stories.push(child);
    }
    let new_pages: Vec<String> = pages.iter().filter_map(|(_, p)| page_keys.get(p.as_str()).cloned()).collect();
    records.alternate_layouts.push(StudioAlternateLayout {
        section_id: req.section_id.to_owned(),
        alternate_layout: req.name.to_owned(),
        alternate_layout_length: u32::try_from(new_pages.len()).map_err(|_| LayoutError::BudgetExceeded)?,
        page_start: new_pages.first().cloned().ok_or(LayoutRuleError::ScopeEmpty)?,
    });
    for (_, p) in &pages {
        if let (Some(key), Some(rule)) = (page_keys.get(p.as_str()), rules.get(p.as_str())) {
            records.page_rules.push(StudioPageLiquidRule { page_id: key.clone(), liquid_rule: *rule });
        }
    }
    let child_inputs: Vec<StoryInput<'_>> = child_stories
        .iter()
        .zip(&inputs)
        .map(|(c, i)| StoryInput { story: c, resolved_text: i.resolved_text, paragraphs: i.paragraphs })
        .collect();
    let ctx = RuleContext { stories: &[], ..req.context };
    let reflows = finish(&work.document, work.graphics.as_ref(), Some((&records, req.parents)), &ctx, &child_inputs, cancel)?;
    Ok(AlternateLayoutProposal {
        base_revision: req.document.revision,
        document: work.document,
        records,
        graphics: work.graphics,
        child_stories,
        pages: new_pages,
        objects: work.objects,
        reflows,
    })
}

/// What happened to one link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkOutcome {
    UpToDate,
    /// Overwritten from the parent and re-synced.
    Updated,
    /// Out of date under `warn_on_change`: reported, not overwritten.
    Warned,
}

/// The outcome of one link update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkUpdate {
    /// Child story id or child layer id.
    pub child_id: String,
    pub outcome: LinkOutcome,
}

/// Link records after an update and the per-link outcomes.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkedStoryUpdate {
    pub records: StudioLayoutRuleRecords,
    /// The input stories with every updated child replaced.
    pub stories: Vec<StudioTextStory>,
    pub updates: Vec<LinkUpdate>,
}

/// Links ordered so a parent that is itself a child is handled before its own children.
fn by_depth<'a>(links: &[(&'a str, &'a str)]) -> Vec<usize> {
    let parent_of: BTreeMap<&str, &str> = links.iter().copied().collect();
    let mut depth: Vec<(usize, usize)> = links
        .iter()
        .enumerate()
        .map(|(i, (child, _))| {
            let mut d = 0usize;
            let mut seen = BTreeSet::new();
            let mut cursor = parent_of.get(child).copied();
            while let Some(p) = cursor {
                if !seen.insert(p) {
                    break;
                }
                d = d.saturating_add(1);
                cursor = parent_of.get(p).copied();
            }
            (d, i)
        })
        .collect();
    depth.sort_unstable();
    depth.into_iter().map(|(_, i)| i).collect()
}

/// Propagates parent story edits to linked child stories (module docs). `accept` names child
/// stories whose warning the caller accepted (they are overwritten like `auto_update`). Every
/// linked story, parent and child, must be in `stories`.
pub fn update_linked_stories(
    records: &StudioLayoutRuleRecords,
    stories: &[StudioTextStory],
    accept: &[String],
) -> Result<LinkedStoryUpdate, LayoutRuleError> {
    let mut out = records.clone();
    let mut all: Vec<StudioTextStory> = stories.to_vec();
    let pairs: Vec<(&str, &str)> = records.linked_stories.iter().map(|l| (l.child_story_id.as_str(), l.parent_story_id.as_str())).collect();
    let mut updates = Vec::new();
    for i in by_depth(&pairs) {
        let link = &records.linked_stories[i];
        let find = |id: &str| all.iter().position(|s| s.story_id == id).ok_or_else(|| LayoutRuleError::StoryMissing(id.to_owned()));
        let (pi, ci) = (find(&link.parent_story_id)?, find(&link.child_story_id)?);
        let outcome = if link.state(&all[pi])? == hsk_studio_folio::StudioLinkState::UpToDate {
            LinkOutcome::UpToDate
        } else if link.options.update_policy == StudioLinkUpdatePolicy::AutoUpdate || accept.contains(&link.child_story_id) {
            let parent = all[pi].clone();
            let child = &mut all[ci];
            child.text_kind = parent.text_kind;
            child.story_direction = parent.story_direction;
            child.content = parent.content.clone();
            child.paragraphs = parent.paragraphs.clone();
            child.character_ranges = parent.character_ranges.clone();
            out.linked_stories[i].synced_parent_fingerprint = linked_story_fingerprint(&parent)?;
            LinkOutcome::Updated
        } else {
            LinkOutcome::Warned
        };
        updates.push(LinkUpdate { child_id: link.child_story_id.clone(), outcome });
    }
    Ok(LinkedStoryUpdate { records: out, stories: all, updates })
}

/// Propagates parent layer edits to linked child page items (module docs); returns the records, the
/// document and the per-link outcomes. The document revision is not changed (the caller commits).
pub fn update_linked_page_items(
    records: &StudioLayoutRuleRecords,
    document: &StudioDocument,
    accept: &[String],
) -> Result<(StudioLayoutRuleRecords, StudioDocument, Vec<LinkUpdate>), LayoutRuleError> {
    let mut out = records.clone();
    let mut doc = document.clone();
    let pairs: Vec<(&str, &str)> = records.linked_page_items.iter().map(|l| (l.child_layer_id.as_str(), l.parent_layer_id.as_str())).collect();
    let mut updates = Vec::new();
    for i in by_depth(&pairs) {
        let link = &records.linked_page_items[i];
        let find = |id: &str| doc.layers.iter().position(|l| l.layer_id == id).ok_or_else(|| LayoutRuleError::LinkMissing(id.to_owned()));
        let (pi, ci) = (find(&link.parent_layer_id)?, find(&link.child_layer_id)?);
        let outcome = if link.state(&doc.layers[pi])? == hsk_studio_folio::StudioLinkState::UpToDate {
            LinkOutcome::UpToDate
        } else if link.options.update_policy == StudioLinkUpdatePolicy::AutoUpdate || accept.contains(&link.child_layer_id) {
            let parent = doc.layers[pi].clone();
            let child = &mut doc.layers[ci];
            child.kind = parent.kind.clone();
            child.payload = parent.payload.clone();
            child.visible = parent.visible;
            child.appearance = parent.appearance.clone();
            out.linked_page_items[i].synced_parent_fingerprint = linked_page_item_fingerprint(&parent)?;
            LinkOutcome::Updated
        } else {
            LinkOutcome::Warned
        };
        updates.push(LinkUpdate { child_id: link.child_layer_id.clone(), outcome });
    }
    Ok((out, doc, updates))
}
