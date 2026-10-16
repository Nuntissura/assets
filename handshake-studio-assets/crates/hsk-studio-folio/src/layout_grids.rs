//! Layout grids and guides (`hsk.studio.layout_grids@1`), lane SMB-ID-PREPRESS: STU-LAY-050
//! (`StudioLayoutGrid` records), STU-LAY-114 (ruler guides), STU-LAY-115 (document and baseline grid,
//! per-frame custom baseline grid), STU-LAY-117 (named grids, stored). Like the other layout record
//! sets it is stored beside the document (`StudioDocument` gains no fields); every member list is
//! `serde(default)` and not encoded when empty. Folio stores and validates only; grid lines,
//! snapping and baseline alignment are computed by hsk-studio-layout.
//!
//! Bounds are the captured ones: view thresholds 5..=4000 percent, `guide_snap_to_zone` 1..=36 px,
//! `minimum_scale` 5..=4000 percent; divisions and increments must be positive `pt` lengths (the spec
//! leaves their bounds unknown; a non-positive division has no grid). Spec defaults are unknown, so
//! nothing here has a default value. InDesign parity UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use super::{Code, GeometryUnit, Length, Result, StudioDocument, StudioRgba, canceled, err, finite_length, id, key};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

schema_token!(LayoutGridsSchema, "hsk.studio.layout_grids@1");

/// Most guides and frame grids per record set.
pub const MAX_GRID_RECORDS: usize = 65_536;

/// STU-LAY-115 `baseline_grid_relative_option` (zero point of the document baseline grid).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioBaselineRelative {
    TopOfPage,
    TopMargin,
}

/// STU-LAY-115 `baseline_frame_grid_relative_option`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioFrameBaselineRelative {
    TopOfPage,
    TopMargin,
    TopOfFrame,
    FrameInset,
}

/// STU-LAY-115 document grid and baseline grid.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioDocumentGrid {
    pub baseline_start: Length,
    pub baseline_division: Length,
    /// Percent, 5..=4000.
    pub baseline_view_threshold: f64,
    pub horizontal_gridline_division: Length,
    pub vertical_gridline_division: Length,
    pub horizontal_grid_subdivision: u32,
    pub vertical_grid_subdivision: u32,
    pub baseline_grid_shown: bool,
    pub document_grid_shown: bool,
    pub document_grid_snap_to: bool,
    pub grids_in_back: bool,
    pub grid_colour: StudioRgba,
    pub baseline_colour: StudioRgba,
    pub baseline_grid_relative_option: StudioBaselineRelative,
}

/// STU-LAY-115 per-frame custom baseline grid (the frame overrides the document baseline grid).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioFrameBaselineGrid {
    /// `SLYR` id of a text-frame layer.
    pub layer_id: String,
    pub use_custom_baseline_frame_grid: bool,
    pub starting_offset_for_baseline_frame_grid: Length,
    pub baseline_frame_grid_relative_option: StudioFrameBaselineRelative,
    pub baseline_frame_grid_increment: Length,
    pub baseline_frame_grid_colour: StudioRgba,
}

/// Guide orientation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioGuideOrientation {
    Horizontal,
    Vertical,
}

/// Guide kind (ruler or liquid guide, STU-LAY-111).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioGuideType {
    Ruler,
    Liquid,
}

/// STU-LAY-114 ruler guide.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioRulerGuide {
    /// Set-local key.
    pub guide_id: String,
    /// `SPGS` id of the spread.
    pub page_spread_id: String,
    /// Page key the guide is on; `None` = a spread guide (across the pasteboard).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_id: Option<String>,
    pub orientation: StudioGuideOrientation,
    /// Page (or spread) coordinate, `pt`.
    pub location: Length,
    pub guide_colour: StudioRgba,
    pub guide_type: StudioGuideType,
    /// Percent, 5..=4000.
    pub guide_view_threshold: f64,
    pub fit_to_page: bool,
    pub locked: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub overridden: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_overrides: bool,
}

/// STU-LAY-114 document-scoped guide preferences.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioGuidePreferences {
    pub guides_shown: bool,
    pub guides_locked: bool,
    pub guides_snap_to: bool,
    pub guides_in_back: bool,
    pub ruler_guides_colour: StudioRgba,
    /// Percent, 5..=4000.
    pub ruler_guides_view_threshold: f64,
    /// Device pixels, 1..=36.
    pub guide_snap_to_zone: u32,
}

/// STU-LAY-117 named grid (stored; character-grid composition is not implemented).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioNamedGrid {
    /// Set-local key.
    pub grid_id: String,
    pub name: String,
    pub characters_per_line: u32,
    pub text_area_width: Length,
    pub text_area_depth: Length,
    pub character_aki: Length,
    pub line_aki: Length,
    /// Percent, 5..=4000.
    pub minimum_scale: f64,
}

/// Paragraph "align to grid" (stored per story paragraph, named by its start byte; InDesign keeps it
/// on the paragraph style, UNVERIFIED D5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioAlignToGrid {
    None,
    AllLines,
    FirstLineOnly,
}

/// The grid alignment of one story paragraph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioGridAlignment {
    /// `STXT` id of a story some text frame shows.
    pub story_id: String,
    pub paragraph_start: u64,
    pub align_to_grid: StudioAlignToGrid,
}

/// The grid and guide records of one document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLayoutGrids {
    pub schema_id: LayoutGridsSchema,
    /// `SDOC` id of the annotated document.
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_grid: Option<StudioDocumentGrid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frame_baseline_grids: Vec<StudioFrameBaselineGrid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guide_preferences: Option<StudioGuidePreferences>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guides: Vec<StudioRulerGuide>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub named_grids: Vec<StudioNamedGrid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paragraph_alignments: Vec<StudioGridAlignment>,
}
impl StudioLayoutGrids {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self {
            schema_id: LayoutGridsSchema::V1,
            document_id: document_id.into(),
            document_grid: None,
            frame_baseline_grids: Vec::new(),
            guide_preferences: None,
            guides: Vec::new(),
            named_grids: Vec::new(),
            paragraph_alignments: Vec::new(),
        }
    }
    /// The custom baseline grid record of a frame layer.
    pub fn frame_baseline_grid(&self, layer_id: &str) -> Option<&StudioFrameBaselineGrid> {
        self.frame_baseline_grids.iter().find(|g| g.layer_id == layer_id)
    }
}

/// JSON schema (draft 2020-12) of the grid record set.
pub fn layout_grids_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<StudioLayoutGrids>()
}

fn positive(l: &Length, target: &str) -> Result<()> {
    finite_length(l, GeometryUnit::Pt, true, target)?;
    if l.value > 0.0 { Ok(()) } else { Err(err(Code::InvalidField, target)) }
}
fn threshold(v: f64, target: &str) -> Result<()> {
    if v.is_finite() && (5.0..=4000.0).contains(&v) { Ok(()) } else { Err(err(Code::InvalidField, target)) }
}
fn rgba(c: &StudioRgba, target: &str) -> Result<()> {
    if [c.r, c.g, c.b, c.a].iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
        Ok(())
    } else {
        Err(err(Code::InvalidField, target))
    }
}

/// Validates the set against its document: identity, ranges, frame layers and spreads/pages that
/// exist, unique keys, bounded counts.
pub fn validate_layout_grids(r: &StudioLayoutGrids, d: &StudioDocument, token: &CancellationToken) -> Result<()> {
    canceled(token)?;
    id(&r.document_id, "SDOC", "document_id")?;
    if r.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    if r.frame_baseline_grids.len() > MAX_GRID_RECORDS || r.guides.len() > MAX_GRID_RECORDS || r.named_grids.len() > MAX_GRID_RECORDS {
        return Err(err(Code::BudgetExceeded, "layout_grids"));
    }
    if let Some(g) = &r.document_grid {
        finite_length(&g.baseline_start, GeometryUnit::Pt, false, "document_grid/baseline_start")?;
        positive(&g.baseline_division, "document_grid/baseline_division")?;
        threshold(g.baseline_view_threshold, "document_grid/baseline_view_threshold")?;
        positive(&g.horizontal_gridline_division, "document_grid/horizontal_gridline_division")?;
        positive(&g.vertical_gridline_division, "document_grid/vertical_gridline_division")?;
        if g.horizontal_grid_subdivision == 0 || g.vertical_grid_subdivision == 0 {
            return Err(err(Code::InvalidField, "document_grid/grid_subdivision"));
        }
        rgba(&g.grid_colour, "document_grid/grid_colour")?;
        rgba(&g.baseline_colour, "document_grid/baseline_colour")?;
    }
    let frames: BTreeSet<&str> = d.layers.iter().filter(|l| l.text_frame.is_some()).map(|l| l.layer_id.as_str()).collect();
    let mut seen = BTreeSet::new();
    for f in &r.frame_baseline_grids {
        canceled(token)?;
        id(&f.layer_id, "SLYR", "frame_baseline_grids/layer_id")?;
        if !frames.contains(f.layer_id.as_str()) {
            return Err(err(Code::DanglingReference, &f.layer_id));
        }
        if !seen.insert(f.layer_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &f.layer_id));
        }
        finite_length(&f.starting_offset_for_baseline_frame_grid, GeometryUnit::Pt, false, "frame_baseline_grids/starting_offset")?;
        positive(&f.baseline_frame_grid_increment, "frame_baseline_grids/increment")?;
        rgba(&f.baseline_frame_grid_colour, "frame_baseline_grids/colour")?;
    }
    if let Some(p) = &r.guide_preferences {
        threshold(p.ruler_guides_view_threshold, "guide_preferences/ruler_guides_view_threshold")?;
        if !(1..=36).contains(&p.guide_snap_to_zone) {
            return Err(err(Code::InvalidField, "guide_preferences/guide_snap_to_zone"));
        }
        rgba(&p.ruler_guides_colour, "guide_preferences/ruler_guides_colour")?;
    }
    let mut keys = BTreeSet::new();
    for g in &r.guides {
        canceled(token)?;
        if !key(&g.guide_id) {
            return Err(err(Code::InvalidId, "guides/guide_id"));
        }
        if !keys.insert(g.guide_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &g.guide_id));
        }
        let spread = d
            .page_spreads
            .iter()
            .find(|s| s.page_spread_id == g.page_spread_id)
            .ok_or_else(|| err(Code::DanglingReference, &g.page_spread_id))?;
        if let Some(p) = &g.page_id
            && !spread.pages.iter().any(|q| q.page_id == *p)
        {
            return Err(err(Code::DanglingReference, p));
        }
        finite_length(&g.location, GeometryUnit::Pt, false, "guides/location")?;
        threshold(g.guide_view_threshold, "guides/guide_view_threshold")?;
        rgba(&g.guide_colour, "guides/guide_colour")?;
    }
    let mut names = BTreeSet::new();
    for n in &r.named_grids {
        canceled(token)?;
        if !key(&n.grid_id) {
            return Err(err(Code::InvalidId, "named_grids/grid_id"));
        }
        if !names.insert(n.grid_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &n.grid_id));
        }
        if n.name.trim().is_empty() || n.characters_per_line == 0 {
            return Err(err(Code::InvalidField, "named_grids"));
        }
        positive(&n.text_area_width, "named_grids/text_area_width")?;
        positive(&n.text_area_depth, "named_grids/text_area_depth")?;
        finite_length(&n.character_aki, GeometryUnit::Pt, false, "named_grids/character_aki")?;
        finite_length(&n.line_aki, GeometryUnit::Pt, false, "named_grids/line_aki")?;
        threshold(n.minimum_scale, "named_grids/minimum_scale")?;
    }
    if r.paragraph_alignments.len() > MAX_GRID_RECORDS {
        return Err(err(Code::BudgetExceeded, "paragraph_alignments"));
    }
    let stories: BTreeSet<&str> = d
        .layers
        .iter()
        .filter_map(|l| match &l.payload {
            super::Payload::Text { story_id } if l.text_frame.is_some() => Some(story_id.as_str()),
            _ => None,
        })
        .collect();
    let mut paras = BTreeSet::new();
    for a in &r.paragraph_alignments {
        canceled(token)?;
        id(&a.story_id, "STXT", "paragraph_alignments/story_id")?;
        if !stories.contains(a.story_id.as_str()) {
            return Err(err(Code::DanglingReference, &a.story_id));
        }
        if !paras.insert((a.story_id.as_str(), a.paragraph_start)) {
            return Err(err(Code::DuplicateIdentity, &a.story_id));
        }
    }
    Ok(())
}
