//! Liquid, adjust and alternate layout records (`hsk.studio.layout_rules@1`), lane
//! SMB-ID-LIQUID-LAYOUT-1: STU-LAY-009/111 (per-page liquid rules, object pins and resize
//! constraints), STU-LAY-011 (alternate layouts stored AS SECTIONS: `alternate_layout` name and
//! `alternate_layout_length` page count) and STU-LAY-019 / STU-TYP-125 (linked stories and linked
//! page items with their SEPARATE option objects). Like the other layout record sets it is stored
//! beside the document (`StudioDocument`, `StudioPage`, `StudioPageSpread` and `StudioLayer` gain
//! no fields); every member list is `serde(default)` and not encoded when empty, so a document
//! without these records stays byte-identical (test `absent_layout_rules_keep_documents_byte_identical`).
//! Folio stores and validates only; geometry, reflow and propagation belong to hsk-studio-layout.
//!
//! Declared readings (InDesign parity UNVERIFIED, D5; the spec does not settle them):
//! - `liquid.preserve_existing` is an apply-range directive ([`StudioLayoutRuleRecords::apply_page_rules`]
//!   skips such pages); it is never a page's stored rule (`InvalidField`).
//! - A page without a stored rule, and `use_parent` without a parent page or a parent rule, resolve
//!   to `liquid.off` ([`resolve_liquid_rule`]); `use_parent` on a parent page follows `based_on`.
//! - `liquid.object_based` constraints: pins on both edges of an axis with a `fixed` size on that
//!   axis are over-constrained (`InvalidField`).
//! - Alternate-layout sections live in this set (the longdoc numbering `StudioSection` gains no
//!   field); a section covers `alternate_layout_length` pages in document page order from
//!   `page_start`; sections never overlap; pages outside every section are the primary layout.
//! - Link state is derived, not stored: a link carries the fingerprint of the parent it was last
//!   synced to (FNV-1a 64 over the parent's content, not a security digest), and the state is
//!   `out_of_date` when the parent's current fingerprint differs ([`StudioLinkedStory::state`]).
//!   The linked-page-item option object carries `update_policy` only; further members were not
//!   recovered by the capture (SPEC GAP).
#![deny(clippy::arithmetic_side_effects)]
use super::{
    Budget, Code, ResolutionIssue, Resolver, Result, StudioDocument, StudioLayer, StudioParentPageRecords,
    StudioTextStory, canceled, err, id,
    values::{Ctx, label},
};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

schema_token!(LayoutRuleRecordsSchema, "hsk.studio.layout_rules@1");

/// Most records in one member list (checked before any record is examined).
pub const MAX_LAYOUT_RULE_RECORDS: usize = 65_536;

/// STU-LAY-111 liquid-rule enumeration: the seven captured members.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioLiquidRule {
    /// `liquid.off`: objects keep absolute geometry.
    Off,
    /// `liquid.scale`: all page content scales proportionally.
    Scale,
    /// `liquid.recenter`: content keeps its size and re-centres.
    Recenter,
    /// `liquid.object_based`: per-object pins and resize constraints.
    ObjectBased,
    /// `liquid.guide_based`: objects a liquid guide crosses stretch.
    GuideBased,
    /// `liquid.use_parent`: the parent page's rule.
    UseParent,
    /// `liquid.preserve_existing`: leave the page's rule untouched (apply-range directive only).
    PreserveExisting,
}
impl StudioLiquidRule {
    /// The seven members in captured order.
    pub const ALL: [Self; 7] = [
        Self::Off,
        Self::Scale,
        Self::Recenter,
        Self::ObjectBased,
        Self::GuideBased,
        Self::UseParent,
        Self::PreserveExisting,
    ];
    /// The Studio identifier (`liquid.<member>`).
    pub fn studio_id(self) -> &'static str {
        match self {
            Self::Off => "liquid.off",
            Self::Scale => "liquid.scale",
            Self::Recenter => "liquid.recenter",
            Self::ObjectBased => "liquid.object_based",
            Self::GuideBased => "liquid.guide_based",
            Self::UseParent => "liquid.use_parent",
            Self::PreserveExisting => "liquid.preserve_existing",
        }
    }
}

/// The stored liquid rule of one document page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioPageLiquidRule {
    /// `StudioPage::page_id`.
    pub page_id: String,
    pub liquid_rule: StudioLiquidRule,
}

/// The liquid rule a parent page defines (`liquid.use_parent` pages inherit it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioParentLiquidRule {
    /// `StudioParentPage::parent_id`.
    pub parent_id: String,
    pub liquid_rule: StudioLiquidRule,
}

/// Resize constraint of one axis under `liquid.object_based`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioResizeConstraint {
    /// The size on this axis is kept.
    Fixed,
    /// The size on this axis follows the page size ratio.
    Relative,
}

/// Pins and resize constraints of one page object (a text-frame or graphic-frame layer).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioObjectLiquidConstraint {
    /// `SLYR` id of a document layer.
    pub layer_id: String,
    pub pin_left: bool,
    pub pin_top: bool,
    pub pin_right: bool,
    pub pin_bottom: bool,
    pub width: StudioResizeConstraint,
    pub height: StudioResizeConstraint,
}

/// An alternate layout stored as a section (STU-LAY-011).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAlternateLayout {
    /// Document-local key, unique in the set.
    pub section_id: String,
    /// The alternate-layout name: not blank, unique, not ending with `*` or `:`.
    pub alternate_layout: String,
    /// Page count of the section, at least 1.
    pub alternate_layout_length: u32,
    /// Page key of the section's first page.
    pub page_start: String,
}

/// STU-TYP-125 `update_policy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioLinkUpdatePolicy {
    /// Parent changes propagate on update.
    AutoUpdate,
    /// Parent changes are reported; the child is not overwritten.
    WarnOnChange,
}

/// The linked-STORY option object (STU-LAY-019, STU-TYP-125).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLinkedStoryOptions {
    pub update_policy: StudioLinkUpdatePolicy,
    pub remove_forced_line_breaks: bool,
}

/// The linked-PAGE-ITEM option object (STU-LAY-019; separate from the story options).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLinkedPageItemOptions {
    pub update_policy: StudioLinkUpdatePolicy,
}

/// Derived link state (STU-LAY-019 "update state").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudioLinkState {
    UpToDate,
    OutOfDate,
}

/// A child story placed and linked from a parent story.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLinkedStory {
    /// `STXT` id of the child copy.
    pub child_story_id: String,
    /// `STXT` id of the parent.
    pub parent_story_id: String,
    pub options: StudioLinkedStoryOptions,
    /// [`linked_story_fingerprint`] of the parent at the last sync (16 lowercase hex digits).
    pub synced_parent_fingerprint: String,
}
impl StudioLinkedStory {
    /// `out_of_date` when `parent` changed since the last sync.
    pub fn state(&self, parent: &StudioTextStory) -> StudioLinkState {
        if linked_story_fingerprint(parent) == self.synced_parent_fingerprint {
            StudioLinkState::UpToDate
        } else {
            StudioLinkState::OutOfDate
        }
    }
}

/// A child page item (layer) placed and linked from a parent layer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLinkedPageItem {
    /// `SLYR` id of the child layer.
    pub child_layer_id: String,
    /// `SLYR` id of the parent layer.
    pub parent_layer_id: String,
    pub options: StudioLinkedPageItemOptions,
    /// [`linked_page_item_fingerprint`] of the parent at the last sync.
    pub synced_parent_fingerprint: String,
}
impl StudioLinkedPageItem {
    /// `out_of_date` when `parent` changed since the last sync.
    pub fn state(&self, parent: &StudioLayer) -> StudioLinkState {
        if linked_page_item_fingerprint(parent) == self.synced_parent_fingerprint {
            StudioLinkState::UpToDate
        } else {
            StudioLinkState::OutOfDate
        }
    }
}

/// The liquid, alternate-layout and link records of one document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLayoutRuleRecords {
    pub schema_id: LayoutRuleRecordsSchema,
    /// `SDOC` id of the annotated document.
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub page_rules: Vec<StudioPageLiquidRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parent_rules: Vec<StudioParentLiquidRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub object_constraints: Vec<StudioObjectLiquidConstraint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternate_layouts: Vec<StudioAlternateLayout>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked_stories: Vec<StudioLinkedStory>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked_page_items: Vec<StudioLinkedPageItem>,
}
impl StudioLayoutRuleRecords {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self {
            schema_id: LayoutRuleRecordsSchema::V1,
            document_id: document_id.into(),
            page_rules: Vec::new(),
            parent_rules: Vec::new(),
            object_constraints: Vec::new(),
            alternate_layouts: Vec::new(),
            linked_stories: Vec::new(),
            linked_page_items: Vec::new(),
        }
    }
    /// The stored rule of a page.
    pub fn page_rule(&self, page_id: &str) -> Option<StudioLiquidRule> {
        self.page_rules.iter().find(|r| r.page_id == page_id).map(|r| r.liquid_rule)
    }
    /// The rule a parent page defines.
    pub fn parent_rule(&self, parent_id: &str) -> Option<StudioLiquidRule> {
        self.parent_rules.iter().find(|r| r.parent_id == parent_id).map(|r| r.liquid_rule)
    }
    /// The pins and constraints of a layer.
    pub fn object_constraint(&self, layer_id: &str) -> Option<&StudioObjectLiquidConstraint> {
        self.object_constraints.iter().find(|c| c.layer_id == layer_id)
    }
    /// The alternate-layout section with this key.
    pub fn alternate_layout(&self, section_id: &str) -> Option<&StudioAlternateLayout> {
        self.alternate_layouts.iter().find(|a| a.section_id == section_id)
    }
    /// The link record of a child story.
    pub fn linked_story(&self, child_story_id: &str) -> Option<&StudioLinkedStory> {
        self.linked_stories.iter().find(|l| l.child_story_id == child_story_id)
    }
    /// Applies a rule across a range of pages: `liquid.preserve_existing` leaves that page's stored
    /// rule untouched, any other member replaces (or adds) the page's stored rule. Returns how many
    /// pages were skipped. Validate the set afterwards.
    pub fn apply_page_rules(&mut self, assignments: &[(String, StudioLiquidRule)]) -> usize {
        let mut skipped = 0usize;
        for (page_id, rule) in assignments {
            if *rule == StudioLiquidRule::PreserveExisting {
                skipped = skipped.saturating_add(1);
                continue;
            }
            match self.page_rules.iter_mut().find(|r| r.page_id == *page_id) {
                Some(r) => r.liquid_rule = *rule,
                None => self.page_rules.push(StudioPageLiquidRule { page_id: page_id.clone(), liquid_rule: *rule }),
            }
        }
        skipped
    }
}

/// JSON schema (draft 2020-12) of the layout-rule record set.
pub fn layout_rule_records_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<StudioLayoutRuleRecords>()
}

/// FNV-1a 64 over bytes, as 16 lowercase hex digits (change detection, not a security digest).
fn fnv1a(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Fingerprint of a story's content (kind, direction, content, paragraphs, character ranges; not
/// its id), for linked-story state.
pub fn linked_story_fingerprint(s: &StudioTextStory) -> String {
    let bytes = serde_json::to_vec(&(&s.text_kind, &s.story_direction, &s.content, &s.paragraphs, &s.character_ranges))
        .unwrap_or_default();
    fnv1a(&bytes)
}

/// Fingerprint of a layer's content (kind, payload, visibility, appearance; not its id, name,
/// parent or frame geometry), for linked-page-item state.
pub fn linked_page_item_fingerprint(l: &StudioLayer) -> String {
    let bytes = serde_json::to_vec(&(&l.kind, &l.payload, l.visible, &l.appearance)).unwrap_or_default();
    fnv1a(&bytes)
}

/// The effective liquid rule of a document page (module docs): the stored rule, `use_parent`
/// resolved through the page's applied parent and its `based_on` chain; never `use_parent` or
/// `preserve_existing`. Safe on unvalidated input: a chain is walked at most once per parent.
pub fn resolve_liquid_rule(
    r: &StudioLayoutRuleRecords,
    parents: Option<&StudioParentPageRecords>,
    page_id: &str,
) -> StudioLiquidRule {
    let rule = r.page_rule(page_id).unwrap_or(StudioLiquidRule::Off);
    if rule != StudioLiquidRule::UseParent {
        return effective(rule);
    }
    let Some(p) = parents else {
        return StudioLiquidRule::Off;
    };
    let mut cursor = p.binding(page_id).and_then(|b| b.applied_parent.as_deref());
    let mut seen = BTreeSet::new();
    while let Some(parent) = cursor {
        if !seen.insert(parent) {
            break;
        }
        match r.parent_rule(parent) {
            Some(StudioLiquidRule::UseParent) | None => {
                cursor = p.parent(parent).and_then(|q| q.based_on.as_deref());
            }
            Some(rule) => return effective(rule),
        }
    }
    StudioLiquidRule::Off
}
fn effective(rule: StudioLiquidRule) -> StudioLiquidRule {
    match rule {
        StudioLiquidRule::UseParent | StudioLiquidRule::PreserveExisting => StudioLiquidRule::Off,
        r => r,
    }
}

fn fingerprint_ok(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Alternate-layout name rules (STU-LAY-011): `Err` names the broken rule.
pub fn check_alternate_layout_name(name: &str) -> std::result::Result<(), &'static str> {
    if name.trim().is_empty() {
        return Err("alternate_layout_blank");
    }
    if name.ends_with('*') || name.ends_with(':') {
        return Err("alternate_layout_suffix");
    }
    if !label(name) {
        return Err("alternate_layout_label");
    }
    Ok(())
}

/// Single-successor link chains (child -> parent) must not loop.
fn acyclic(next: &BTreeMap<&str, &str>, token: &CancellationToken) -> Result<()> {
    for start in next.keys().copied() {
        let mut seen = BTreeSet::new();
        let mut cursor = Some(start);
        while let Some(node) = cursor {
            canceled(token)?;
            if !seen.insert(node) {
                return Err(err(Code::RenderCycle, start));
            }
            cursor = next.get(node).copied();
        }
    }
    Ok(())
}

/// The single validator: identity and document binding; bounded lists (`BudgetExceeded`, records
/// charged to `Budget::nodes`, names to `Budget::payload_bytes`); page rules of pages the document
/// holds, parent rules of parents the parent-page set holds (`DanglingReference`, also without a
/// parent-page set), `preserve_existing` never stored and over-constrained object axes
/// (`InvalidField`); object layers the document holds; alternate-layout names (blank, `*`/`:`
/// suffix: `InvalidField`; repeated: `DuplicateIdentity`), sections inside the document page order
/// and not overlapping; linked stories shown by text frames, linked page items naming document
/// layers, one link per child, no self or looping links (`RenderCycle`), well-formed fingerprints.
pub fn validate_layout_rule_records(
    r: &StudioLayoutRuleRecords,
    d: &StudioDocument,
    parents: Option<&StudioParentPageRecords>,
    b: Budget,
    token: &CancellationToken,
    resolver: &dyn Resolver,
) -> Result<Vec<ResolutionIssue>> {
    let mut cx = Ctx::new(b, token, resolver);
    cx.tick()?;
    id(&r.document_id, "SDOC", "document_id")?;
    if r.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    let lists = [
        (r.page_rules.len(), "page_rules"),
        (r.parent_rules.len(), "parent_rules"),
        (r.object_constraints.len(), "object_constraints"),
        (r.alternate_layouts.len(), "alternate_layouts"),
        (r.linked_stories.len(), "linked_stories"),
        (r.linked_page_items.len(), "linked_page_items"),
    ];
    for (n, target) in lists {
        if n > MAX_LAYOUT_RULE_RECORDS {
            return Err(err(Code::BudgetExceeded, target));
        }
        cx.nodes(n, target)?;
    }
    if let Some(p) = parents
        && p.document_id != d.document_id
    {
        return Err(err(Code::InvalidField, "parents/document_id"));
    }

    let mut pages = BTreeSet::new();
    for rule in &r.page_rules {
        cx.tick()?;
        let t = rule.page_id.as_str();
        if d.locate_page(t).is_none() {
            return Err(err(Code::DanglingReference, format!("page_rules/{t}")));
        }
        if !pages.insert(t) {
            return Err(err(Code::DuplicateIdentity, format!("page_rules/{t}")));
        }
        if rule.liquid_rule == StudioLiquidRule::PreserveExisting {
            return Err(err(Code::InvalidField, format!("page_rules/{t}/liquid_rule")));
        }
    }
    let mut parent_keys = BTreeSet::new();
    for rule in &r.parent_rules {
        cx.tick()?;
        let t = rule.parent_id.as_str();
        if !parents.is_some_and(|p| p.parent(t).is_some()) {
            return Err(err(Code::DanglingReference, format!("parent_rules/{t}")));
        }
        if !parent_keys.insert(t) {
            return Err(err(Code::DuplicateIdentity, format!("parent_rules/{t}")));
        }
        if rule.liquid_rule == StudioLiquidRule::PreserveExisting {
            return Err(err(Code::InvalidField, format!("parent_rules/{t}/liquid_rule")));
        }
    }

    let layers: BTreeMap<&str, &StudioLayer> = d.layers.iter().map(|l| (l.layer_id.as_str(), l)).collect();
    let layer_ref = |layer: &str, target: String| -> Result<()> {
        id(layer, "SLYR", &target)?;
        if layers.contains_key(layer) { Ok(()) } else { Err(err(Code::DanglingReference, target)) }
    };
    let mut constrained = BTreeSet::new();
    for c in &r.object_constraints {
        cx.tick()?;
        let t = c.layer_id.as_str();
        layer_ref(t, format!("object_constraints/{t}"))?;
        if !constrained.insert(t) {
            return Err(err(Code::DuplicateIdentity, format!("object_constraints/{t}")));
        }
        if c.pin_left && c.pin_right && c.width == StudioResizeConstraint::Fixed {
            return Err(err(Code::InvalidField, format!("object_constraints/{t}/width")));
        }
        if c.pin_top && c.pin_bottom && c.height == StudioResizeConstraint::Fixed {
            return Err(err(Code::InvalidField, format!("object_constraints/{t}/height")));
        }
    }

    let order: BTreeMap<&str, usize> = d
        .page_spreads
        .iter()
        .flat_map(|s| s.pages.iter())
        .enumerate()
        .map(|(i, p)| (p.page_id.as_str(), i))
        .collect();
    let total = order.len();
    let mut keys = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut covered: Vec<(usize, usize)> = Vec::new();
    for a in &r.alternate_layouts {
        cx.tick()?;
        let t = a.section_id.as_str();
        if !label(t) {
            return Err(err(Code::InvalidField, "alternate_layouts/section_id"));
        }
        if !keys.insert(t) {
            return Err(err(Code::DuplicateIdentity, format!("alternate_layouts/{t}")));
        }
        if let Err(rule) = check_alternate_layout_name(&a.alternate_layout) {
            return Err(err(Code::InvalidField, format!("alternate_layouts/{t}/{rule}")));
        }
        cx.text(&a.alternate_layout, t)?;
        if !names.insert(a.alternate_layout.as_str()) {
            return Err(err(Code::DuplicateIdentity, format!("alternate_layouts/{t}/alternate_layout")));
        }
        let start = *order
            .get(a.page_start.as_str())
            .ok_or_else(|| err(Code::DanglingReference, format!("alternate_layouts/{t}/page_start")))?;
        let len = usize::try_from(a.alternate_layout_length).unwrap_or(usize::MAX);
        let end = start.checked_add(len).filter(|e| len > 0 && *e <= total);
        let Some(end) = end else {
            return Err(err(Code::InvalidField, format!("alternate_layouts/{t}/alternate_layout_length")));
        };
        if covered.iter().any(|(s, e)| start < *e && *s < end) {
            return Err(err(Code::InvalidField, format!("alternate_layouts/{t}/overlap")));
        }
        covered.push((start, end));
    }

    let shown: BTreeSet<&str> = d
        .layers
        .iter()
        .filter_map(|l| match &l.payload {
            super::Payload::Text { story_id } if l.text_frame.is_some() => Some(story_id.as_str()),
            _ => None,
        })
        .collect();
    let mut story_next: BTreeMap<&str, &str> = BTreeMap::new();
    for link in &r.linked_stories {
        cx.tick()?;
        let (c, p) = (link.child_story_id.as_str(), link.parent_story_id.as_str());
        let target = format!("linked_stories/{c}");
        id(c, "STXT", &target)?;
        id(p, "STXT", &target)?;
        if !shown.contains(c) || !shown.contains(p) {
            return Err(err(Code::DanglingReference, target));
        }
        if c == p {
            return Err(err(Code::RenderCycle, target));
        }
        if story_next.insert(c, p).is_some() {
            return Err(err(Code::DuplicateIdentity, target));
        }
        if !fingerprint_ok(&link.synced_parent_fingerprint) {
            return Err(err(Code::InvalidField, format!("{target}/synced_parent_fingerprint")));
        }
    }
    acyclic(&story_next, token)?;
    let mut item_next: BTreeMap<&str, &str> = BTreeMap::new();
    for link in &r.linked_page_items {
        cx.tick()?;
        let (c, p) = (link.child_layer_id.as_str(), link.parent_layer_id.as_str());
        let target = format!("linked_page_items/{c}");
        layer_ref(c, target.clone())?;
        layer_ref(p, target.clone())?;
        if c == p {
            return Err(err(Code::RenderCycle, target));
        }
        if item_next.insert(c, p).is_some() {
            return Err(err(Code::DuplicateIdentity, target));
        }
        if !fingerprint_ok(&link.synced_parent_fingerprint) {
            return Err(err(Code::InvalidField, format!("{target}/synced_parent_fingerprint")));
        }
    }
    acyclic(&item_next, token)?;
    canceled(token)?;
    Ok(cx.issues)
}
