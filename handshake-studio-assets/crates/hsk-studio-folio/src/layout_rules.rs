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
//!   synced to (SHA-256 over the JSON of the parent's content, 64 lowercase hex digits), and the
//!   state is `out_of_date` when the parent's current fingerprint differs
//!   ([`StudioLinkedStory::state`]). A serialization failure is a typed error, never a fingerprint.
//!   Records written before F-R40-6 carry a 16-hex FNV-1a 64 value: it still validates, but it is
//!   never compared with a SHA-256 value; such a link reads `StaleFingerprint` (treated as changed;
//!   an update or accept recomputes the fingerprint).
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
    /// The stored fingerprint is a legacy FNV-1a 64 value (16 hex digits, records before F-R40-6):
    /// it is never compared with the current SHA-256 fingerprint, so the link counts as changed
    /// until a sync recomputes it (module docs).
    StaleFingerprint,
}

/// State of a link from its stored fingerprint and the parent's current one.
fn link_state(stored: &str, current: &str) -> StudioLinkState {
    if stored.len() == LEGACY_FINGERPRINT_HEX {
        StudioLinkState::StaleFingerprint
    } else if stored == current {
        StudioLinkState::UpToDate
    } else {
        StudioLinkState::OutOfDate
    }
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
    /// [`linked_story_fingerprint`] of the parent at the last sync (64 lowercase hex digits; a
    /// legacy 16-digit FNV-1a value reads as [`StudioLinkState::StaleFingerprint`]).
    pub synced_parent_fingerprint: String,
}
impl StudioLinkedStory {
    /// `OutOfDate` when `parent` changed since the last sync; `Err` when the parent cannot be
    /// fingerprinted.
    pub fn state(&self, parent: &StudioTextStory) -> Result<StudioLinkState> {
        Ok(link_state(&self.synced_parent_fingerprint, &linked_story_fingerprint(parent)?))
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
    /// [`linked_page_item_fingerprint`] of the parent at the last sync (as for linked stories).
    pub synced_parent_fingerprint: String,
}
impl StudioLinkedPageItem {
    /// `OutOfDate` when `parent` changed since the last sync; `Err` when the parent cannot be
    /// fingerprinted.
    pub fn state(&self, parent: &StudioLayer) -> Result<StudioLinkState> {
        Ok(link_state(&self.synced_parent_fingerprint, &linked_page_item_fingerprint(parent)?))
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

/// Hex digits of a current (SHA-256) link fingerprint.
pub const LINK_FINGERPRINT_HEX: usize = 64;
/// Hex digits of a legacy FNV-1a 64 link fingerprint (records before F-R40-6; read, never written).
pub const LEGACY_FINGERPRINT_HEX: usize = 16;

/// SHA-256 of the serialized content as 64 lowercase hex digits; a serialization failure is the
/// typed `InvalidField` diagnostic at `target`, never the fingerprint of empty input.
fn content_fingerprint<T: Serialize>(content: &T, target: String) -> Result<String> {
    use std::fmt::Write as _;
    let bytes = serde_json::to_vec(content).map_err(|_| err(Code::InvalidField, target))?;
    let mut hex = String::with_capacity(LINK_FINGERPRINT_HEX);
    for b in sha256::digest(&bytes) {
        let _ = write!(hex, "{b:02x}");
    }
    Ok(hex)
}

/// Fingerprint of a story's content (kind, direction, content, paragraphs, character ranges; not
/// its id), for linked-story state. `Err` (`InvalidField` at `<story_id>/fingerprint`) when the
/// content does not serialize.
pub fn linked_story_fingerprint(s: &StudioTextStory) -> Result<String> {
    content_fingerprint(
        &(&s.text_kind, &s.story_direction, &s.content, &s.paragraphs, &s.character_ranges),
        format!("{}/fingerprint", s.story_id),
    )
}

/// Fingerprint of a layer's content (kind, payload, visibility, appearance; not its id, name,
/// parent or frame geometry), for linked-page-item state. `Err` (`InvalidField` at
/// `<layer_id>/fingerprint`) when the content does not serialize.
pub fn linked_page_item_fingerprint(l: &StudioLayer) -> Result<String> {
    content_fingerprint(&(&l.kind, &l.payload, l.visible, &l.appearance), format!("{}/fingerprint", l.layer_id))
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
    (s.len() == LINK_FINGERPRINT_HEX || s.len() == LEGACY_FINGERPRINT_HEX)
        && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
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

/// SHA-256 (FIPS 180-4) for link fingerprints. In-crate because Folio's approved direct libraries
/// are schemars, serde and serde_json only (tools/workspace-probe.rs APPROVED); known answers in
/// the test below.
mod sha256 {
    const K: [u32; 64] = [
        0x428a_2f98, 0x7137_4491, 0xb5c0_fbcf, 0xe9b5_dba5, 0x3956_c25b, 0x59f1_11f1, 0x923f_82a4, 0xab1c_5ed5,
        0xd807_aa98, 0x1283_5b01, 0x2431_85be, 0x550c_7dc3, 0x72be_5d74, 0x80de_b1fe, 0x9bdc_06a7, 0xc19b_f174,
        0xe49b_69c1, 0xefbe_4786, 0x0fc1_9dc6, 0x240c_a1cc, 0x2de9_2c6f, 0x4a74_84aa, 0x5cb0_a9dc, 0x76f9_88da,
        0x983e_5152, 0xa831_c66d, 0xb003_27c8, 0xbf59_7fc7, 0xc6e0_0bf3, 0xd5a7_9147, 0x06ca_6351, 0x1429_2967,
        0x27b7_0a85, 0x2e1b_2138, 0x4d2c_6dfc, 0x5338_0d13, 0x650a_7354, 0x766a_0abb, 0x81c2_c92e, 0x9272_2c85,
        0xa2bf_e8a1, 0xa81a_664b, 0xc24b_8b70, 0xc76c_51a3, 0xd192_e819, 0xd699_0624, 0xf40e_3585, 0x106a_a070,
        0x19a4_c116, 0x1e37_6c08, 0x2748_774c, 0x34b0_bcb5, 0x391c_0cb3, 0x4ed8_aa4a, 0x5b9c_ca4f, 0x682e_6ff3,
        0x748f_82ee, 0x78a5_636f, 0x84c8_7814, 0x8cc7_0208, 0x90be_fffa, 0xa450_6ceb, 0xbef9_a3f7, 0xc671_78f2,
    ];

    pub(super) fn digest(data: &[u8]) -> [u8; 32] {
        let mut h: [u32; 8] =
            [0x6a09_e667, 0xbb67_ae85, 0x3c6e_f372, 0xa54f_f53a, 0x510e_527f, 0x9b05_688c, 0x1f83_d9ab, 0x5be0_cd19];
        let bit_len = u64::try_from(data.len()).unwrap_or(u64::MAX).wrapping_mul(8);
        let mut msg = data.to_vec();
        msg.push(0x80);
        while msg.len() & 63 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bit_len.to_be_bytes());
        for block in msg.chunks_exact(64) {
            let mut w = [0u32; 64];
            for (slot, word) in w.iter_mut().zip(block.chunks_exact(4)) {
                *slot = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
            }
            for i in 16..64_usize {
                let (w15, w2) = (w[i.wrapping_sub(15)], w[i.wrapping_sub(2)]);
                let s0 = w15.rotate_right(7) ^ w15.rotate_right(18) ^ (w15 >> 3);
                let s1 = w2.rotate_right(17) ^ w2.rotate_right(19) ^ (w2 >> 10);
                w[i] = w[i.wrapping_sub(16)].wrapping_add(s0).wrapping_add(w[i.wrapping_sub(7)]).wrapping_add(s1);
            }
            let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
            for (k, wi) in K.iter().zip(w) {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ (!e & g);
                let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(*k).wrapping_add(wi);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
                *x = x.wrapping_add(y);
            }
        }
        let mut out = [0u8; 32];
        for (chunk, word) in out.chunks_exact_mut(4).zip(h) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    #[cfg(test)]
    mod tests {
        fn hex(d: [u8; 32]) -> String {
            use std::fmt::Write as _;
            let mut s = String::new();
            for b in d {
                let _ = write!(s, "{b:02x}");
            }
            s
        }

        /// FIPS 180-4 known answers: empty input, one block, two blocks, one million 'a'.
        #[test]
        fn sha256_known_answers() {
            assert_eq!(hex(super::digest(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
            assert_eq!(hex(super::digest(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
            assert_eq!(
                hex(super::digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
            );
            assert_eq!(
                hex(super::digest(&vec![b'a'; 1_000_000])),
                "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
            );
        }
    }
}

#[cfg(test)]
mod fingerprint_tests {
    use super::{Code, content_fingerprint};

    struct Unserializable;
    impl serde::Serialize for Unserializable {
        fn serialize<S: serde::Serializer>(&self, _: S) -> std::result::Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("refused"))
        }
    }

    /// F-R40-6: a serialization failure is the typed diagnostic, never the fingerprint of empty
    /// input (before, `unwrap_or_default` hashed zero bytes and returned a valid-looking value).
    #[test]
    fn fingerprint_serialization_failure_is_a_typed_error() {
        let d = content_fingerprint(&Unserializable, "STXT-x/fingerprint".to_owned()).expect_err("refused");
        assert_eq!((d.code, d.target.as_str()), (Code::InvalidField, "STXT-x/fingerprint"));
        assert_eq!(content_fingerprint(&(), "t".to_owned()).map(|f| f.len()), Ok(64));
    }
}
