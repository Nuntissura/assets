//! Layout members of the one `StudioDocument` graph: pages inside a `StudioPageSpread` (spec
//! STU-LAY-001, STU-LAY-003, STU-LAY-107, STU-LAY-113) and the text-frame and thread contract carried
//! by a text `StudioLayer` (STU-LAY-017, STU-LAY-018, STU-LAY-118..121, STU-LAY-122). Folio stores and
//! validates only; composition, reflow and the overset result belong to the layout crate.
//!
//! Every length is a Folio `Length` whose unit must be `pt`: STU-LAY-100/105/113/118 declare points
//! canonical for page size, margins, bleed, inset, gutter and baseline offset, and the unit stays on
//! the wire. Frame bounds are in page coordinates (origin = top-left of the page, y grows downward).
//!
//! Absent `pages` and an absent `text_frame` are the same as empty and are not encoded, so every
//! document written before this module re-serializes byte-identically.
//!
//! Authored thread state is `previous_text_frame` / `next_text_frame` only. The other STU-LAY-122
//! fields are derived from the chain and exposed by [`StudioDocument::text_threads`]: `parent_story`
//! is the text payload's `story_id`, `start_text_frame` / `end_text_frame` are the chain ends and
//! `text_frame_index` is the position in the chain. `overflows` (overset) is layout output and is not
//! stored in the authored document.
//!
//! Declared gaps, not invented here: v02.215 declares no identity prefix for a page, so `page_id` is
//! a document-local key (`[A-Za-z0-9_-]{1,128}`); the page fields `document_offset`, `applied_section`,
//! `applied_alternate_layout`, `applied_parent`, `parent_transform`, `override_list`,
//! `parent_page_items`, `page_colour_label`, `layout_rule`, `snapshot_blending_mode`, slug, grid and
//! column guides (STU-LAY-107/113), the spread `page_count`, `binding_location` and shuffle flags
//! (STU-LAY-106) and the frame fields for fixed/flexible column width, footnotes, column rules,
//! `ignore_text_wrap`, `frame_type`, `story_orientation` and the wrap or obstacle refs
//! (STU-LAY-118, STU-LAY-126) are not stored yet. STU-LAY-119 and STU-TYP-116 spell the proportional
//! auto-size member differently; this module follows STU-LAY-119 without the `autosize.` namespace.
use super::{
    Bounds, Code, GeometryUnit, Length, NodeKind, Parent, Payload, Result, StudioDocument,
    StudioLayer, StudioPageSpread, canceled, err, finite_length, id, key, required,
};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Hard ceiling on pages in one document: the `pages_per_document` hard maximum of STU-LAY-105. The
/// per-spread page limit is not hard-coded (STU-LAY-106): pages are charged to `Budget::nodes`.
pub const MAX_PAGES_PER_DOCUMENT: usize = 9999;

/// STU-LAY-107 `side`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PageSide {
    RightHand,
    LeftHand,
    SingleSided,
}

/// Four lengths in `pt`. Page margins (STU-LAY-113: on a facing-pages document `left` / `right` mean
/// inside / outside) and frame `inset_spacing` (STU-LAY-118: the four-value form `[top, left, bottom,
/// right]`; a scalar spelling is stored as four equal values).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayoutEdges {
    pub top: Length,
    pub right: Length,
    pub bottom: Length,
    pub left: Length,
}
impl LayoutEdges {
    fn each(&self) -> [&Length; 4] {
        [&self.top, &self.right, &self.bottom, &self.left]
    }
}

/// STU-LAY-105 bleed, `pt`, per side.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PageBleed {
    pub top: Length,
    pub bottom: Length,
    pub inside_or_left: Length,
    pub outside_or_right: Length,
}

/// One page of a spread. A spread lists its pages left to right.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioPage {
    /// Document-local key, unique across all spreads (no page identity prefix is declared).
    pub page_id: String,
    /// Trim width, `pt`, greater than zero.
    pub width: Length,
    /// Trim height, `pt`, greater than zero.
    pub height: Length,
    pub side: PageSide,
    /// Per-page margins; must leave a positive margin box.
    pub margins: LayoutEdges,
    /// Bleed, or explicit null when the page declares none.
    #[serde(deserialize_with = "required")]
    #[schemars(required)]
    pub bleed: Option<PageBleed>,
}

/// STU-LAY-120 first-baseline enumeration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FirstBaseline {
    Ascent,
    CapHeight,
    Leading,
    EmBoxHeight,
    XHeight,
    Fixed,
}

/// STU-LAY-121 vertical-justification enumeration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerticalJustification {
    Top,
    Center,
    Bottom,
    Justify,
}

/// STU-LAY-119 auto-sizing enumeration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutoSizing {
    Off,
    HeightOnly,
    WidthOnly,
    HeightAndWidth,
    HeightAndWidthProportionally,
}

/// Text-frame members of a `StudioLayer` of kind `text` whose payload names the story (STU-LAY-017).
/// The frame identity is the layer's `layer_id`; thread links name other frame layers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTextFrame {
    /// Page key the frame sits on; page coordinates for `bounds`.
    pub page_id: String,
    /// `pt` in page coordinates; width and height greater than zero.
    pub bounds: Bounds,
    /// STU-LAY-118 `inset_spacing`, `pt`, non-negative; must leave a positive inner box.
    pub inset_spacing: LayoutEdges,
    /// STU-LAY-118 `text_column_count`, at least 1.
    pub text_column_count: u32,
    /// STU-LAY-118 `text_column_gutter`, `pt`, non-negative; columns must keep a positive width.
    pub text_column_gutter: Length,
    pub first_baseline_offset: FirstBaseline,
    /// STU-LAY-118 `minimum_first_baseline_offset`, `pt`, non-negative; the explicit offset when
    /// `first_baseline_offset` is `fixed`.
    pub minimum_first_baseline_offset: Length,
    pub vertical_justification: VerticalJustification,
    pub auto_sizing_type: AutoSizing,
    /// STU-LAY-122: the preceding frame layer of the thread, or explicit null for the first frame.
    #[serde(deserialize_with = "required")]
    #[schemars(required)]
    pub previous_text_frame: Option<String>,
    /// STU-LAY-122: the following frame layer of the thread, or explicit null for the last frame.
    #[serde(deserialize_with = "required")]
    #[schemars(required)]
    pub next_text_frame: Option<String>,
}

/// Derived view of one thread chain (STU-LAY-122): one story flowing through an ordered chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextThread {
    story_id: String,
    frames: Vec<String>,
}
impl TextThread {
    /// STU-LAY-122 `parent_story`.
    pub fn story_id(&self) -> &str {
        &self.story_id
    }
    /// Frame layer ids in flow order.
    pub fn frames(&self) -> &[String] {
        &self.frames
    }
    /// STU-LAY-122 `start_text_frame`.
    pub fn start_text_frame(&self) -> &str {
        self.frames.first().map_or("", String::as_str)
    }
    /// STU-LAY-122 `end_text_frame`.
    pub fn end_text_frame(&self) -> &str {
        self.frames.last().map_or("", String::as_str)
    }
    /// STU-LAY-122 `text_frame_index`: the ordinal of `frame_id` within the story.
    pub fn text_frame_index(&self, frame_id: &str) -> Option<usize> {
        self.frames.iter().position(|f| f == frame_id)
    }
    /// STU-LAY-122 `previous_text_frame` as seen from the chain.
    pub fn previous_text_frame(&self, frame_id: &str) -> Option<&str> {
        let i = self.text_frame_index(frame_id)?;
        i.checked_sub(1).map(|p| self.frames[p].as_str())
    }
    /// STU-LAY-122 `next_text_frame` as seen from the chain.
    pub fn next_text_frame(&self, frame_id: &str) -> Option<&str> {
        let i = self.text_frame_index(frame_id)?;
        self.frames.get(i + 1).map(String::as_str)
    }
}

/// Where a page sits in the document.
#[derive(Clone, Copy, Debug)]
pub struct PageLocation<'a> {
    pub spread: &'a StudioPageSpread,
    /// Position of the page within `spread.pages` (left to right).
    pub page_index: usize,
    pub page: &'a StudioPage,
}

fn story_of(l: &StudioLayer) -> Option<&str> {
    match &l.payload {
        Payload::Text { story_id } => Some(story_id.as_str()),
        _ => None,
    }
}

impl StudioDocument {
    /// The page with this key and its spread.
    pub fn locate_page(&self, page_id: &str) -> Option<PageLocation<'_>> {
        self.page_spreads.iter().find_map(|spread| {
            let page_index = spread.pages.iter().position(|p| p.page_id == page_id)?;
            Some(PageLocation {
                spread,
                page_index,
                page: &spread.pages[page_index],
            })
        })
    }
    /// Every thread chain, ordered by the document position of its first frame. Safe on a document
    /// that has not been validated: a link that does not resolve or repeats ends the walk.
    pub fn text_threads(&self) -> Vec<TextThread> {
        let frames: BTreeMap<&str, &StudioLayer> = self
            .layers
            .iter()
            .filter(|l| l.text_frame.is_some())
            .map(|l| (l.layer_id.as_str(), l))
            .collect();
        let mut threads = Vec::new();
        for head in self.layers.iter().filter(|l| {
            l.text_frame
                .as_ref()
                .is_some_and(|f| f.previous_text_frame.is_none())
        }) {
            let mut chain: Vec<String> = Vec::new();
            let mut seen = BTreeSet::new();
            let mut cursor = Some(head.layer_id.as_str());
            while let Some(current) = cursor {
                if !seen.insert(current) {
                    break;
                }
                chain.push(current.to_owned());
                cursor = frames
                    .get(current)
                    .and_then(|l| l.text_frame.as_ref())
                    .and_then(|f| f.next_text_frame.as_deref())
                    .filter(|next| frames.contains_key(next));
            }
            threads.push(TextThread {
                story_id: story_of(head).unwrap_or_default().to_owned(),
                frames: chain,
            });
        }
        threads
    }
}

/// Pages across all spreads, charged against `Budget::nodes`.
pub(crate) fn page_count(d: &StudioDocument) -> usize {
    d.page_spreads
        .iter()
        .fold(0usize, |n, s| n.saturating_add(s.pages.len()))
}

fn pt(l: &Length, non_negative: bool, target: &str) -> Result<()> {
    finite_length(l, GeometryUnit::Pt, non_negative, target)
}
fn edges(e: &LayoutEdges, target: &str) -> Result<()> {
    e.each().into_iter().try_for_each(|l| pt(l, true, target))
}

/// Validates every page and returns page key -> containing page-spread id.
fn validate_pages<'a>(
    d: &'a StudioDocument,
    token: &CancellationToken,
) -> Result<BTreeMap<&'a str, &'a str>> {
    let mut pages = BTreeMap::new();
    for s in &d.page_spreads {
        for p in &s.pages {
            canceled(token)?;
            let pid = p.page_id.as_str();
            if !key(pid) {
                return Err(err(Code::InvalidId, "page_id"));
            }
            if pages.insert(pid, s.page_spread_id.as_str()).is_some() {
                return Err(err(Code::DuplicateIdentity, pid));
            }
            pt(&p.width, true, pid)?;
            pt(&p.height, true, pid)?;
            if p.width.value <= 0.0 || p.height.value <= 0.0 {
                return Err(err(Code::InvalidField, format!("{pid}/size")));
            }
            edges(&p.margins, &format!("{pid}/margins"))?;
            let m = &p.margins;
            if p.width.value - m.left.value - m.right.value <= 0.0
                || p.height.value - m.top.value - m.bottom.value <= 0.0
            {
                return Err(err(Code::InvalidField, format!("{pid}/margins")));
            }
            if let Some(b) = &p.bleed {
                for l in [&b.top, &b.bottom, &b.inside_or_left, &b.outside_or_right] {
                    pt(l, true, &format!("{pid}/bleed"))?;
                }
            }
        }
    }
    Ok(pages)
}

fn validate_frame(
    l: &StudioLayer,
    f: &StudioTextFrame,
    pages: &BTreeMap<&str, &str>,
) -> Result<()> {
    let lid = l.layer_id.as_str();
    if !key(&f.page_id) {
        return Err(err(Code::InvalidId, format!("{lid}/page")));
    }
    let Some(spread_id) = pages.get(f.page_id.as_str()) else {
        return Err(err(Code::DanglingReference, format!("{lid}/page")));
    };
    if let Parent::Node(p) = &l.parent
        && p.node_kind == NodeKind::PageSpread
        && p.node_id != *spread_id
    {
        return Err(err(Code::InvalidField, format!("{lid}/page")));
    }
    let b = &f.bounds;
    let target = format!("{lid}/bounds");
    pt(&b.x, false, &target)?;
    pt(&b.y, false, &target)?;
    pt(&b.width, true, &target)?;
    pt(&b.height, true, &target)?;
    if b.width.value <= 0.0 || b.height.value <= 0.0 {
        return Err(err(Code::InvalidField, target));
    }
    let target = format!("{lid}/inset_spacing");
    edges(&f.inset_spacing, &target)?;
    let i = &f.inset_spacing;
    let inner_width = b.width.value - i.left.value - i.right.value;
    if inner_width <= 0.0 || b.height.value - i.top.value - i.bottom.value <= 0.0 {
        return Err(err(Code::InvalidField, target));
    }
    pt(
        &f.text_column_gutter,
        true,
        &format!("{lid}/text_column_gutter"),
    )?;
    pt(
        &f.minimum_first_baseline_offset,
        true,
        &format!("{lid}/minimum_first_baseline_offset"),
    )?;
    let target = format!("{lid}/text_column_count");
    if f.text_column_count == 0 {
        return Err(err(Code::InvalidField, target));
    }
    let gutters = f64::from(f.text_column_count - 1) * f.text_column_gutter.value;
    if inner_width - gutters <= 0.0 {
        return Err(err(Code::InvalidField, target));
    }
    Ok(())
}

/// Resolves one thread link to the frame it names.
fn link<'a>(
    frames: &BTreeMap<&'a str, (&'a StudioLayer, &'a StudioTextFrame)>,
    from: &str,
    field: &str,
    to: &'a Option<String>,
) -> Result<Option<&'a str>> {
    let Some(to) = to else {
        return Ok(None);
    };
    let target = format!("{from}/{field}");
    id(to, "SLYR", &target)?;
    frames
        .get_key_value(to.as_str())
        .map(|(k, _)| Some(*k))
        .ok_or_else(|| err(Code::DanglingReference, target))
}

/// Single-successor graphs are functional: a repeated node on the current path is a cycle.
fn acyclic(successor: &BTreeMap<&str, &str>, token: &CancellationToken) -> Result<()> {
    let mut finished: BTreeSet<&str> = BTreeSet::new();
    for start in successor.keys().copied() {
        if finished.contains(start) {
            continue;
        }
        let mut path: Vec<&str> = Vec::new();
        let mut on_path = BTreeSet::new();
        let mut cursor = Some(start);
        while let Some(current) = cursor {
            canceled(token)?;
            if on_path.contains(current) {
                return Err(err(Code::ThreadCycle, current));
            }
            if finished.contains(current) {
                break;
            }
            on_path.insert(current);
            path.push(current);
            cursor = successor.get(current).copied();
        }
        finished.extend(path);
    }
    Ok(())
}

pub(crate) fn validate_layout(d: &StudioDocument, token: &CancellationToken) -> Result<()> {
    canceled(token)?;
    if page_count(d) > MAX_PAGES_PER_DOCUMENT {
        return Err(err(Code::BudgetExceeded, "pages"));
    }
    let pages = validate_pages(d, token)?;
    let mut frames: BTreeMap<&str, (&StudioLayer, &StudioTextFrame)> = BTreeMap::new();
    for l in &d.layers {
        canceled(token)?;
        let Some(f) = &l.text_frame else {
            continue;
        };
        if l.kind != "text" || story_of(l).is_none() {
            return Err(err(Code::PayloadMismatch, &l.layer_id));
        }
        validate_frame(l, f, &pages)?;
        frames.insert(l.layer_id.as_str(), (l, f));
    }
    if frames.is_empty() {
        return Ok(());
    }
    let (mut next_of, mut previous_of) = (BTreeMap::new(), BTreeMap::new());
    for (lid, (_, f)) in &frames {
        canceled(token)?;
        if let Some(n) = link(&frames, lid, "next", &f.next_text_frame)? {
            next_of.insert(*lid, n);
        }
        if let Some(p) = link(&frames, lid, "previous", &f.previous_text_frame)? {
            previous_of.insert(*lid, p);
        }
    }
    acyclic(&next_of, token)?;
    acyclic(&previous_of, token)?;
    for (a, b) in &next_of {
        if previous_of.get(b) != Some(a) {
            return Err(err(Code::InvalidThread, format!("{a}/next")));
        }
    }
    for (b, a) in &previous_of {
        if next_of.get(a) != Some(b) {
            return Err(err(Code::InvalidThread, format!("{b}/previous")));
        }
    }
    // Links are now reciprocal, single-successor and acyclic: every frame lies on exactly one chain
    // that starts at a frame without a predecessor.
    let mut story_head: BTreeMap<&str, &str> = BTreeMap::new();
    let mut walked = 0usize;
    for l in &d.layers {
        let lid = l.layer_id.as_str();
        if !frames.contains_key(lid) || previous_of.contains_key(lid) {
            continue;
        }
        let story = story_of(l).unwrap_or_default();
        if story_head.insert(story, lid).is_some() {
            return Err(err(Code::InvalidThread, format!("{lid}/story")));
        }
        let mut cursor = Some(lid);
        while let Some(current) = cursor {
            canceled(token)?;
            walked += 1;
            if frames
                .get(current)
                .and_then(|(l, _)| story_of(l))
                .is_none_or(|s| s != story)
            {
                return Err(err(Code::InvalidThread, format!("{current}/story")));
            }
            cursor = next_of.get(current).copied();
        }
    }
    if walked != frames.len() {
        return Err(err(Code::InvalidThread, "threads"));
    }
    Ok(())
}
