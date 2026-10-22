//! Layout-flow records (`hsk.studio.layout_flow_records@1`), lane SMB-LAYOUT-UI. Like the
//! parent-page and long-document sets this one is stored beside the document (`StudioDocument`
//! gains no fields); every member list is `serde(default)` and not encoded when empty, so record
//! sets written before a member existed stay byte-identical. Folio stores and validates only;
//! composition belongs to hsk-studio-layout.
//!
//! Frame flow options (STU-LAY-118 `vertical_balance_columns`): per text-frame layer. The spec
//! (STU-LAY-121) says only `vjust.justify` activates the flag while layout composes `vjust.top`
//! only; layout honours the flag with top justification (spec conflict recorded, InDesign gating
//! UNVERIFIED, D5).
//!
//! Vertical thresholds (lane SMB-ID-PREPRESS, STU-LAY-118 `vertical_threshold`, STU-TYP-190): per
//! text-frame layer, the paragraph spacing limit of `vjust.justify` in `pt` (hard_min 0, no
//! declared maximum); absent = no limit. A separate member list so `StudioFrameFlowOptions`
//! literals and earlier record sets stay unchanged.
//!
//! Paragraph flow overrides (STU-LAY-021, STU-LAY-125 "paragraph overrides"): per story paragraph
//! (named by its start byte) the span/split enumeration `{single_column, span_columns,
//! split_columns}`, the `span_count.all` sentinel AND the numeric count as separate fields (the
//! spec forbids encoding `all` as a magic number), the split sub-column count, space before/after
//! and the inside/outside gutters.
//!
//! Anchored objects (STU-LAY-025, STU-LAY-127): an anchored item (a document layer) bound to a byte
//! of a story, with the captured settings (`anchored_position` inline / above_line / custom, x/y
//! offsets, space above, horizontal/vertical alignment and reference points, nine-point anchor,
//! spine_relative, lock_position, pin_position) and the box of the item in pt. A document-scope
//! default settings record (`anchored_object_defaults`) is carried; `insert_anchored_object` copies
//! it into new anchors, `release_anchored_object` removes one. Members the spec does not enumerate
//! (horizontal/vertical alignment, vertical reference points) are Studio's provisional sets
//! (UNVERIFIED, D5). A member of a group layer cannot be anchored (`IllegalParent`).
//!
//! Cross-references (STU-LAY-047, STU-LAY-151): named text anchors (story byte), formats assembled
//! from building blocks (a tag string, parsed by hsk-studio-layout; syntax UNVERIFIED against
//! InDesign) with an optional character style, and references from a source range of a story to a
//! paragraph or a text anchor, caching the last resolved text so stale references can be flagged.
//!
//! Conditional text (STU-LAY-049, STU-LAY-153): conditions (name, visible, indicator colour, method
//! and underline appearance), condition sets (condition/visibility pairs; `redefine` captures the
//! current visibility), tagged story ranges and the document indicator mode. `delete_condition`
//! takes the REPLACING-WITH condition so tagged text is re-tagged, never orphaned.
//!
//! Object styles (STU-LAY-039, STU-LAY-142): named records with `based_on` (no cycles, unique
//! names), all nineteen named category-enable booleans (the spec says seventeen and lists
//! nineteen), the stroke surface with its hard ranges (miter 1..=500, arrow-head scales
//! 1..=1000 %, gap tint 0..=100), corner radii, paragraph style, text-frame general / baseline /
//! auto-size options, anchored settings, frame fitting, the export-tag map keyed by export type and
//! the ARIA fields; `applied_object_styles` records the style each layer carries. Application is
//! hsk-studio-layout `apply_object_style`.
//!
//! Hostile wire JSON: integer arithmetic is denied (workflow V4-01); list sizes are checked against
//! the declared ceilings before anything is indexed.
#![deny(clippy::arithmetic_side_effects)]
use super::{
    Code, GeometryUnit, Length, Payload, Result, StudioDocument, canceled, err, finite_length, id,
};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

schema_token!(LayoutFlowRecordsSchema, "hsk.studio.layout_flow_records@1");

/// Frame option entries per record set.
pub const MAX_FRAME_FLOW_OPTIONS: usize = 65_536;
/// Paragraph flow overrides per record set.
pub const MAX_PARAGRAPH_FLOWS: usize = 65_536;
/// Upper bound for span and split counts (InDesign's own maximum is UNVERIFIED; D5).
pub const MAX_SPLIT_COLUMNS: u32 = 40;

fn is_false(b: &bool) -> bool {
    !*b
}

/// STU-LAY-118 frame options that `StudioTextFrame` does not carry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioFrameFlowOptions {
    /// `SLYR` id of a text-frame layer of the document.
    pub layer_id: String,
    /// Balance the columns of the frame that ends the story (and the band above a spanning
    /// paragraph).
    #[serde(default, skip_serializing_if = "is_false")]
    pub vertical_balance_columns: bool,
}

/// STU-LAY-118 `vertical_threshold` of one text-frame layer: the most space `vjust.justify` may
/// add between two paragraphs, on top of their own space before/after (`pt`, non-negative).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioFrameVerticalThreshold {
    /// `SLYR` id of a text-frame layer of the document.
    pub layer_id: String,
    pub vertical_threshold: Length,
}

/// STU-LAY-021 `span_column` enumeration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioSpanColumn {
    #[default]
    SingleColumn,
    SpanColumns,
    SplitColumns,
}

/// STU-LAY-021 `span_count` enumeration; `all` is its only captured member.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioSpanCount {
    All,
}

/// A paragraph-level span/split override of one story paragraph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioParagraphFlow {
    /// `STXT` id of the story.
    pub story_id: String,
    /// Start byte of the paragraph (`StoryParagraph::start`).
    pub paragraph_start: u64,
    pub span_column: StudioSpanColumn,
    /// The `span_count.all` sentinel; when present it wins over `span_column_count`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_count: Option<StudioSpanCount>,
    /// Numeric N of `span_columns` (1..=MAX_SPLIT_COLUMNS).
    pub span_column_count: u32,
    /// Sub-columns of `split_columns` (2..=MAX_SPLIT_COLUMNS).
    pub split_column_count: u32,
    /// `pt`, non-negative.
    pub space_before: Length,
    /// `pt`, non-negative.
    pub space_after: Length,
    /// Split only: gap between sub-columns, `pt`, non-negative.
    pub inside_gutter: Length,
    /// Split only: inset of the split area from both column edges, `pt`, non-negative.
    pub outside_gutter: Length,
}

/// The layout-flow records of one document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLayoutFlowRecords {
    pub schema_id: LayoutFlowRecordsSchema,
    /// `SDOC` id of the annotated document.
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frame_options: Vec<StudioFrameFlowOptions>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paragraph_flows: Vec<StudioParagraphFlow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchored_object_defaults: Option<StudioAnchoredSettings>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchored_objects: Vec<StudioAnchoredObject>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_anchors: Vec<StudioTextAnchor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub xref_formats: Vec<StudioXrefFormat>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cross_references: Vec<StudioCrossReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<StudioCondition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub condition_sets: Vec<StudioConditionSet>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditional_ranges: Vec<StudioConditionalRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditional_text_preferences: Option<StudioConditionalTextPreferences>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub object_styles: Vec<StudioObjectStyle>,
    /// The object style each layer last had applied (STU-LAY-039).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_object_styles: Vec<StudioAppliedObjectStyle>,
    /// Paragraph spacing limits of justified frames (STU-LAY-118 `vertical_threshold`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vertical_thresholds: Vec<StudioFrameVerticalThreshold>,
}
impl StudioLayoutFlowRecords {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self {
            schema_id: LayoutFlowRecordsSchema::V1,
            document_id: document_id.into(),
            frame_options: Vec::new(),
            paragraph_flows: Vec::new(),
            anchored_object_defaults: None,
            anchored_objects: Vec::new(),
            text_anchors: Vec::new(),
            xref_formats: Vec::new(),
            cross_references: Vec::new(),
            conditions: Vec::new(),
            condition_sets: Vec::new(),
            conditional_ranges: Vec::new(),
            conditional_text_preferences: None,
            object_styles: Vec::new(),
            applied_object_styles: Vec::new(),
            vertical_thresholds: Vec::new(),
        }
    }
    /// The `vertical_threshold` stored for this frame layer.
    pub fn vertical_threshold(&self, layer_id: &str) -> Option<&Length> {
        self.vertical_thresholds
            .iter()
            .find(|v| v.layer_id == layer_id)
            .map(|v| &v.vertical_threshold)
    }
    /// The object style with this key.
    pub fn object_style(&self, style_id: &str) -> Option<&StudioObjectStyle> {
        self.object_styles.iter().find(|s| s.style_id == style_id)
    }
    /// The anchored-object record of this layer.
    pub fn anchored_object(&self, layer_id: &str) -> Option<&StudioAnchoredObject> {
        self.anchored_objects.iter().find(|a| a.layer_id == layer_id)
    }
    /// The condition with this key.
    pub fn condition(&self, condition_id: &str) -> Option<&StudioCondition> {
        self.conditions.iter().find(|c| c.condition_id == condition_id)
    }
    /// STU-LAY-127 `insert_anchored_object`: anchors `layer_id` at `byte` of `story_id` with the
    /// document default settings (or `StudioAnchoredSettings::default()`) and `position`; members
    /// not valid for `position` are cleared.
    pub fn insert_anchored_object(
        &mut self,
        layer_id: &str,
        story_id: &str,
        byte: u64,
        position: StudioAnchorPosition,
        size: [Length; 2],
    ) -> Result<()> {
        if self.anchored_object(layer_id).is_some() {
            return Err(err(Code::DuplicateIdentity, layer_id));
        }
        let mut settings = self.anchored_object_defaults.clone().unwrap_or_default();
        settings.anchored_position = position;
        if position != StudioAnchorPosition::Custom {
            settings.horizontal_reference_point = None;
            settings.vertical_alignment = None;
            settings.vertical_reference_point = None;
        }
        if position != StudioAnchorPosition::AboveLine {
            settings.anchor_space_above = zero_pt();
        }
        if position == StudioAnchorPosition::Inline {
            settings.horizontal_alignment = None;
        }
        let [width, height] = size;
        self.anchored_objects.push(StudioAnchoredObject {
            layer_id: layer_id.into(),
            story_id: story_id.into(),
            anchor_byte: byte,
            width,
            height,
            settings,
        });
        Ok(())
    }
    /// STU-LAY-127 `release_anchored_object`: the removed record, if the layer was anchored.
    pub fn release_anchored_object(&mut self, layer_id: &str) -> Option<StudioAnchoredObject> {
        let i = self.anchored_objects.iter().position(|a| a.layer_id == layer_id)?;
        Some(self.anchored_objects.remove(i))
    }
    /// Applies a condition set: every condition it names takes the stored visibility.
    pub fn apply_condition_set(&mut self, set_id: &str) -> Result<()> {
        let set = self
            .condition_sets
            .iter()
            .find(|s| s.set_id == set_id)
            .ok_or_else(|| err(Code::DanglingReference, set_id))?
            .clone();
        if let Some(v) = set.visibility.iter().find(|v| self.condition(&v.condition_id).is_none()) {
            return Err(err(Code::DanglingReference, &v.condition_id));
        }
        for v in &set.visibility {
            if let Some(c) = self.conditions.iter_mut().find(|c| c.condition_id == v.condition_id) {
                c.visible = v.visible;
            }
        }
        if let Some(p) = self.conditional_text_preferences.as_mut() {
            p.active_set = Some(set_id.into());
        }
        Ok(())
    }
    /// STU-LAY-153 `redefine`: the set captures the current visibility of every condition.
    pub fn redefine_condition_set(&mut self, set_id: &str) -> Result<()> {
        let now: Vec<StudioConditionVisibility> = self
            .conditions
            .iter()
            .map(|c| StudioConditionVisibility {
                condition_id: c.condition_id.clone(),
                visible: c.visible,
            })
            .collect();
        let set = self
            .condition_sets
            .iter_mut()
            .find(|s| s.set_id == set_id)
            .ok_or_else(|| err(Code::DanglingReference, set_id))?;
        set.visibility = now;
        Ok(())
    }
    /// Deletes a condition; tagged ranges are re-tagged with `replacing_with` (or untagged when
    /// `None`), sets drop it. A replacement that does not exist (or is the condition itself) is
    /// refused before anything changes.
    pub fn delete_condition(&mut self, condition_id: &str, replacing_with: Option<&str>) -> Result<()> {
        if self.condition(condition_id).is_none() {
            return Err(err(Code::DanglingReference, condition_id));
        }
        if let Some(r) = replacing_with
            && (r == condition_id || self.condition(r).is_none())
        {
            return Err(err(Code::DanglingReference, r));
        }
        self.conditions.retain(|c| c.condition_id != condition_id);
        for s in &mut self.condition_sets {
            s.visibility.retain(|v| v.condition_id != condition_id);
        }
        for r in &mut self.conditional_ranges {
            if r.conditions.iter().any(|c| c == condition_id) {
                r.conditions.retain(|c| c != condition_id);
                if let Some(new) = replacing_with
                    && !r.conditions.iter().any(|c| c == new)
                {
                    r.conditions.push(new.into());
                }
            }
        }
        self.conditional_ranges.retain(|r| !r.conditions.is_empty());
        Ok(())
    }
    /// The options of this frame layer.
    pub fn frame_options(&self, layer_id: &str) -> Option<&StudioFrameFlowOptions> {
        self.frame_options.iter().find(|f| f.layer_id == layer_id)
    }
    /// The override of the paragraph of `story_id` starting at `start`.
    pub fn paragraph_flow(&self, story_id: &str, start: u64) -> Option<&StudioParagraphFlow> {
        self.paragraph_flows
            .iter()
            .find(|p| p.story_id == story_id && p.paragraph_start == start)
    }
}

/// STU-LAY-127 `anchored_position`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioAnchorPosition {
    #[default]
    Inline,
    AboveLine,
    Custom,
}

/// Horizontal alignment of an above-line or custom anchored object (provisional members).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioAnchorHAlign {
    Left,
    Center,
    Right,
}

/// STU-LAY-127 `horizontal_reference_point` (captured members).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioAnchorHRef {
    ColumnEdge,
    TextFrame,
    PageMargins,
    PageEdge,
    AnchorLocation,
}

/// Vertical alignment of a custom anchored object (provisional members).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioAnchorVAlign {
    Top,
    Center,
    Bottom,
}

/// Vertical reference point of a custom anchored object (provisional members; the spec does not
/// enumerate them).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioAnchorVRef {
    LineBaseline,
    ColumnEdge,
    TextFrame,
    PageMargins,
    PageEdge,
}

/// STU-LAY-127 nine-position `anchor_point`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioAnchorPoint {
    #[default]
    TopLeft,
    TopCenter,
    TopRight,
    LeftCenter,
    Center,
    RightCenter,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

fn zero_pt() -> Length {
    Length {
        value: 0.0,
        unit: GeometryUnit::Pt,
    }
}

/// STU-LAY-127 anchored-object settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAnchoredSettings {
    pub anchored_position: StudioAnchorPosition,
    pub anchor_x_offset: Length,
    /// Inline: the object rises this far above the baseline; above line: space after the object;
    /// custom: offset from the vertical reference.
    pub anchor_y_offset: Length,
    /// Above line only (0 otherwise).
    pub anchor_space_above: Length,
    /// Not valid for inline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub horizontal_alignment: Option<StudioAnchorHAlign>,
    /// Custom only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub horizontal_reference_point: Option<StudioAnchorHRef>,
    /// Custom only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertical_alignment: Option<StudioAnchorVAlign>,
    /// Custom only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertical_reference_point: Option<StudioAnchorVRef>,
    #[serde(default)]
    pub anchor_point: StudioAnchorPoint,
    #[serde(default, skip_serializing_if = "is_false")]
    pub spine_relative: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub lock_position: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub pin_position: bool,
}
impl Default for StudioAnchoredSettings {
    fn default() -> Self {
        Self {
            anchored_position: StudioAnchorPosition::Inline,
            anchor_x_offset: zero_pt(),
            anchor_y_offset: zero_pt(),
            anchor_space_above: zero_pt(),
            horizontal_alignment: None,
            horizontal_reference_point: None,
            vertical_alignment: None,
            vertical_reference_point: None,
            anchor_point: StudioAnchorPoint::TopLeft,
            spine_relative: false,
            lock_position: false,
            pin_position: false,
        }
    }
}

/// One anchored item: a document layer bound to a story byte.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAnchoredObject {
    /// `SLYR` id of the anchored layer.
    pub layer_id: String,
    /// `STXT` id of the story holding the anchor.
    pub story_id: String,
    /// Story byte of the anchor (inline: the start of its placeholder character).
    pub anchor_byte: u64,
    /// Box of the item, pt, positive.
    pub width: Length,
    pub height: Length,
    pub settings: StudioAnchoredSettings,
}

/// A named text anchor (hyperlink and cross-reference destination).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTextAnchor {
    /// Document-local key, unique in the set.
    pub anchor_id: String,
    /// 1..=256 characters, unique.
    pub name: String,
    pub story_id: String,
    pub byte: u64,
}

/// A cross-reference format: building blocks as a tag string (parsed by layout) and an optional
/// character style for the whole reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioXrefFormat {
    pub format_id: String,
    pub name: String,
    /// Building-block string, at most `MAX_XREF_FORMAT_BYTES`.
    pub definition: String,
    /// `STYS` id of a character style.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_style: Option<String>,
}

/// STU-LAY-151: a cross-reference destination is a paragraph or a text anchor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioXrefDestination {
    Paragraph { story_id: String, paragraph_start: u64 },
    TextAnchor { anchor_id: String },
}

/// A cross-reference: the source range of a story that shows the reference text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCrossReference {
    pub xref_id: String,
    pub source_story_id: String,
    /// Half-open source range in the source story (the reference text; may be empty).
    pub source_start: u64,
    pub source_end: u64,
    pub destination: StudioXrefDestination,
    pub format_id: String,
    /// The text last written into the source range (stale detection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_text: Option<String>,
}

/// STU-LAY-153 `indicator_method`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioIndicatorMethod {
    Underline,
    Highlight,
}

/// STU-LAY-153 `underline_indicator_appearance`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioUnderlineAppearance {
    Wavy,
    Solid,
    Dashed,
}

/// STU-LAY-153 condition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioCondition {
    pub condition_id: String,
    pub name: String,
    pub visible: bool,
    pub indicator_colour: super::StudioRgba,
    pub indicator_method: StudioIndicatorMethod,
    pub underline_indicator_appearance: StudioUnderlineAppearance,
}

/// One condition/visibility pair of a set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioConditionVisibility {
    pub condition_id: String,
    pub visible: bool,
}

/// STU-LAY-153 condition set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioConditionSet {
    pub set_id: String,
    pub name: String,
    pub visibility: Vec<StudioConditionVisibility>,
}

/// A story range tagged with one or more conditions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioConditionalRange {
    pub story_id: String,
    pub start: u64,
    pub end: u64,
    /// Condition keys, at least one.
    pub conditions: Vec<String>,
}

/// STU-LAY-153 indicator mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioIndicatorMode {
    #[default]
    Show,
    ShowAndPrint,
    Hide,
}

/// Document-scope conditional-text preferences.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioConditionalTextPreferences {
    pub indicator_mode: StudioIndicatorMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_set: Option<String>,
}

/// STU-LAY-142: the object style's category-enable booleans. The spec says "SEVENTEEN" and then
/// names nineteen; all nineteen named members are carried (spec count conflict recorded).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct StudioObjectStyleCategories {
    pub enable_fill: bool,
    pub enable_stroke: bool,
    pub enable_stroke_and_corner_options: bool,
    pub enable_paragraph_style: bool,
    pub enable_text_frame_general_options: bool,
    pub enable_text_frame_baseline_options: bool,
    pub enable_text_frame_auto_sizing_options: bool,
    pub enable_text_frame_column_rule_options: bool,
    pub enable_text_frame_footnote_options: bool,
    pub enable_story_options: bool,
    pub enable_text_wrap_and_others: bool,
    pub enable_anchored_object_options: bool,
    pub enable_frame_fitting_options: bool,
    pub enable_export_tagging: bool,
    pub enable_object_export_alt_text_options: bool,
    pub enable_object_export_tagged_pdf_options: bool,
    pub enable_object_export_epub_options: bool,
    pub enable_transform_attributes: bool,
    pub enable_flex_layout_attributes: bool,
}

/// Stroke end cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioEndCap {
    Butt,
    Round,
    Projecting,
}

/// Stroke end join.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioEndJoin {
    Miter,
    Round,
    Bevel,
}

/// Stroke alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioStrokeAlignment {
    Center,
    Inside,
    Outside,
}

/// Arrow-head alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioArrowHeadAlignment {
    InsidePath,
    OutsidePath,
}

/// STU-LAY-142 `epub_aria_label_source_type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioAriaLabelSource {
    Automatic,
    Custom,
    None,
}

/// STU-LAY-146: one entry of the export-tag map, keyed by object export type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioExportTag {
    pub export_type: String,
    pub tag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
}

/// STU-LAY-039/142 object style (the frame-formatting surface Studio stores; absent = not set by
/// this style, inherited through `based_on`). Declared gaps (not stored): text wrap (no Folio
/// wrap record), column rules, per-frame footnote options, transform and flex-layout attributes,
/// the tagged-PDF and EPUB option groups beyond the ARIA fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioObjectStyle {
    pub style_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub based_on: Option<String>,
    #[serde(default)]
    pub categories: StudioObjectStyleCategories,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<super::StudioRgba>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_colour: Option<super::StudioRgba>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_weight: Option<Length>,
    /// Hard 1..=500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub miter_limit: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_cap: Option<StudioEndCap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_join: Option<StudioEndJoin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_line_end: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right_line_end: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap_colour: Option<super::StudioRgba>,
    /// Percent 0..=100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap_tint: Option<f64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub overprint_gap: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_alignment: Option<StudioStrokeAlignment>,
    /// Percent, hard 1..=1000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_arrow_head_scale: Option<f64>,
    /// Percent, hard 1..=1000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right_arrow_head_scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrow_head_alignment: Option<StudioArrowHeadAlignment>,
    /// Top-left, top-right, bottom-right, bottom-left radii, pt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corner_radii: Option<[Length; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corner_option: Option<String>,
    /// `STYS` id of a paragraph style.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paragraph_style: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub apply_next_paragraph_style: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_named_grid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyboard_shortcut: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extended_keyboard_shortcut: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub nonprinting: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub emit_css: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_column_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_column_gutter: Option<Length>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inset_spacing: Option<super::LayoutEdges>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertical_justification: Option<super::VerticalJustification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertical_balance_columns: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_baseline_offset: Option<super::FirstBaseline>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_first_baseline_offset: Option<Length>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_sizing_type: Option<super::AutoSizing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchored_settings: Option<StudioAnchoredSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_fitting: Option<super::StudioFit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fitting_anchor: Option<super::StudioFitAnchor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub export_tags: Vec<StudioExportTag>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epub_aria_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epub_aria_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epub_aria_label_source_type: Option<StudioAriaLabelSource>,
}
impl StudioObjectStyle {
    /// An empty style (no category enabled, nothing set).
    pub fn new(style_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            style_id: style_id.into(),
            name: name.into(),
            based_on: None,
            categories: StudioObjectStyleCategories::default(),
            fill: None,
            stroke_colour: None,
            stroke_weight: None,
            miter_limit: None,
            end_cap: None,
            end_join: None,
            stroke_type: None,
            left_line_end: None,
            right_line_end: None,
            gap_colour: None,
            gap_tint: None,
            overprint_gap: false,
            stroke_alignment: None,
            left_arrow_head_scale: None,
            right_arrow_head_scale: None,
            arrow_head_alignment: None,
            corner_radii: None,
            corner_option: None,
            paragraph_style: None,
            apply_next_paragraph_style: false,
            applied_named_grid: None,
            keyboard_shortcut: None,
            extended_keyboard_shortcut: None,
            nonprinting: false,
            emit_css: false,
            text_column_count: None,
            text_column_gutter: None,
            inset_spacing: None,
            vertical_justification: None,
            vertical_balance_columns: None,
            first_baseline_offset: None,
            minimum_first_baseline_offset: None,
            auto_sizing_type: None,
            anchored_settings: None,
            frame_fitting: None,
            fitting_anchor: None,
            export_tags: Vec::new(),
            alt_text: None,
            epub_aria_role: None,
            epub_aria_label: None,
            epub_aria_label_source_type: None,
        }
    }
}

/// The object style applied to a layer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAppliedObjectStyle {
    pub layer_id: String,
    pub style_id: String,
}

/// Records of one kind per set (anchors, formats, references, conditions, sets, ranges).
pub const MAX_FLOW_ITEMS: usize = 65_536;
/// Bytes of a cross-reference format definition.
pub const MAX_XREF_FORMAT_BYTES: usize = 4096;
/// Characters of a name.
pub const MAX_NAME_CHARS: usize = 256;

/// JSON schema (draft 2020-12) of the layout-flow record set.
pub fn layout_flow_records_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<StudioLayoutFlowRecords>()
}

fn pt_nonneg(l: &Length, target: &str) -> Result<()> {
    finite_length(l, GeometryUnit::Pt, true, target)
}

fn check_paragraph_flow(p: &StudioParagraphFlow) -> Result<()> {
    id(&p.story_id, "STXT", "paragraph_flows/story_id")?;
    let t = "paragraph_flows";
    if p.span_column_count == 0 || p.span_column_count > MAX_SPLIT_COLUMNS {
        return Err(err(Code::InvalidField, format!("{t}/span_column_count")));
    }
    if p.split_column_count < 2 || p.split_column_count > MAX_SPLIT_COLUMNS {
        return Err(err(Code::InvalidField, format!("{t}/split_column_count")));
    }
    pt_nonneg(&p.space_before, "paragraph_flows/space_before")?;
    pt_nonneg(&p.space_after, "paragraph_flows/space_after")?;
    pt_nonneg(&p.inside_gutter, "paragraph_flows/inside_gutter")?;
    pt_nonneg(&p.outside_gutter, "paragraph_flows/outside_gutter")
}

/// Validates the set against its document: identity, text-frame layers that exist, stories that
/// some frame of the document shows, unique keys, bounded counts and lengths.
pub fn validate_layout_flow_records(
    r: &StudioLayoutFlowRecords,
    d: &StudioDocument,
    token: &CancellationToken,
) -> Result<()> {
    canceled(token)?;
    id(&r.document_id, "SDOC", "document_id")?;
    if r.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    if r.frame_options.len() > MAX_FRAME_FLOW_OPTIONS {
        return Err(err(Code::BudgetExceeded, "frame_options"));
    }
    if r.paragraph_flows.len() > MAX_PARAGRAPH_FLOWS {
        return Err(err(Code::BudgetExceeded, "paragraph_flows"));
    }
    let frames: BTreeSet<&str> = d
        .layers
        .iter()
        .filter(|l| l.text_frame.is_some())
        .map(|l| l.layer_id.as_str())
        .collect();
    let stories: BTreeSet<&str> = d
        .layers
        .iter()
        .filter_map(|l| match &l.payload {
            Payload::Text { story_id } if l.text_frame.is_some() => Some(story_id.as_str()),
            _ => None,
        })
        .collect();
    let mut seen = BTreeSet::new();
    for f in &r.frame_options {
        canceled(token)?;
        id(&f.layer_id, "SLYR", "frame_options/layer_id")?;
        if !frames.contains(f.layer_id.as_str()) {
            return Err(err(Code::DanglingReference, &f.layer_id));
        }
        if !seen.insert(f.layer_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &f.layer_id));
        }
    }
    if r.vertical_thresholds.len() > MAX_FRAME_FLOW_OPTIONS {
        return Err(err(Code::BudgetExceeded, "vertical_thresholds"));
    }
    let mut seen = BTreeSet::new();
    for v in &r.vertical_thresholds {
        canceled(token)?;
        id(&v.layer_id, "SLYR", "vertical_thresholds/layer_id")?;
        if !frames.contains(v.layer_id.as_str()) {
            return Err(err(Code::DanglingReference, &v.layer_id));
        }
        if !seen.insert(v.layer_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &v.layer_id));
        }
        pt_nonneg(&v.vertical_threshold, "vertical_thresholds/vertical_threshold")?;
    }
    let mut keys = BTreeSet::new();
    for p in &r.paragraph_flows {
        canceled(token)?;
        check_paragraph_flow(p)?;
        if !stories.contains(p.story_id.as_str()) {
            return Err(err(Code::DanglingReference, &p.story_id));
        }
        if !keys.insert((p.story_id.as_str(), p.paragraph_start)) {
            return Err(err(Code::DuplicateIdentity, &p.story_id));
        }
    }
    check_anchors(r, d, &stories, token)?;
    check_xrefs(r, &stories, token)?;
    check_conditions(r, &stories, token)?;
    check_object_styles(r, d, token)
}

fn in_range(v: Option<f64>, lo: f64, hi: f64, target: &str) -> Result<()> {
    match v {
        Some(x) if !(lo..=hi).contains(&x) => Err(err(Code::InvalidField, target)),
        _ => Ok(()),
    }
}

fn check_object_style(s: &StudioObjectStyle) -> Result<()> {
    key_ok(&s.style_id, "object_styles/style_id")?;
    name_ok(&s.name, "object_styles/name")?;
    in_range(s.miter_limit, 1.0, 500.0, "object_styles/miter_limit")?;
    in_range(s.gap_tint, 0.0, 100.0, "object_styles/gap_tint")?;
    in_range(s.left_arrow_head_scale, 1.0, 1000.0, "object_styles/left_arrow_head_scale")?;
    in_range(s.right_arrow_head_scale, 1.0, 1000.0, "object_styles/right_arrow_head_scale")?;
    for c in [&s.fill, &s.stroke_colour, &s.gap_colour].into_iter().flatten() {
        if ![c.r, c.g, c.b, c.a].iter().all(|v| (0.0..=1.0).contains(v)) {
            return Err(err(Code::InvalidField, "object_styles/colour"));
        }
    }
    for l in [&s.stroke_weight, &s.text_column_gutter, &s.minimum_first_baseline_offset].into_iter().flatten() {
        pt_nonneg(l, "object_styles/length")?;
    }
    for l in s.corner_radii.iter().flatten() {
        pt_nonneg(l, "object_styles/corner_radii")?;
    }
    if let Some(e) = &s.inset_spacing {
        for l in [&e.top, &e.right, &e.bottom, &e.left] {
            pt_nonneg(l, "object_styles/inset_spacing")?;
        }
    }
    if let Some(n) = s.text_column_count
        && !(1..=1024).contains(&n)
    {
        return Err(err(Code::InvalidField, "object_styles/text_column_count"));
    }
    if let Some(p) = &s.paragraph_style {
        id(p, "STYS", "object_styles/paragraph_style")?;
    }
    if let Some(a) = &s.anchored_settings {
        check_settings(a, "object_styles/anchored_settings")?;
    }
    let mut types = BTreeSet::new();
    for t in &s.export_tags {
        if t.export_type.is_empty() || !types.insert(t.export_type.as_str()) {
            return Err(err(Code::DuplicateIdentity, "object_styles/export_tags"));
        }
    }
    Ok(())
}

fn check_object_styles(r: &StudioLayoutFlowRecords, d: &StudioDocument, token: &CancellationToken) -> Result<()> {
    if r.object_styles.len() > MAX_FLOW_ITEMS || r.applied_object_styles.len() > MAX_FLOW_ITEMS {
        return Err(err(Code::BudgetExceeded, "object_styles"));
    }
    let (mut ids, mut names) = (BTreeSet::new(), BTreeSet::new());
    for s in &r.object_styles {
        canceled(token)?;
        check_object_style(s)?;
        // STU-LAY-143: style names unique within their scope.
        if !ids.insert(s.style_id.as_str()) || !names.insert(s.name.as_str()) {
            return Err(err(Code::DuplicateIdentity, &s.style_id));
        }
    }
    for s in &r.object_styles {
        // STU-LAY-143: a based-on chain must not be circular (bounded by the set size).
        let mut at = s.based_on.as_deref();
        let mut steps = 0usize;
        while let Some(p) = at {
            canceled(token)?;
            let parent = r.object_style(p).ok_or_else(|| err(Code::DanglingReference, p))?;
            steps = steps.checked_add(1).ok_or_else(|| err(Code::BudgetExceeded, "object_styles"))?;
            if p == s.style_id || steps > r.object_styles.len() {
                return Err(err(Code::RenderCycle, &s.style_id));
            }
            at = parent.based_on.as_deref();
        }
    }
    let mut layers = BTreeSet::new();
    for a in &r.applied_object_styles {
        if !d.layers.iter().any(|l| l.layer_id == a.layer_id) {
            return Err(err(Code::DanglingReference, &a.layer_id));
        }
        if r.object_style(&a.style_id).is_none() {
            return Err(err(Code::DanglingReference, &a.style_id));
        }
        if !layers.insert(a.layer_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &a.layer_id));
        }
    }
    Ok(())
}

fn name_ok(name: &str, target: &str) -> Result<()> {
    let n = name.chars().count();
    if n == 0 || n > MAX_NAME_CHARS {
        return Err(err(Code::InvalidField, target));
    }
    Ok(())
}

fn key_ok(k: &str, target: &str) -> Result<()> {
    if super::key(k) {
        Ok(())
    } else {
        Err(err(Code::InvalidField, target))
    }
}

fn check_settings(s: &StudioAnchoredSettings, target: &str) -> Result<()> {
    finite_length(&s.anchor_x_offset, GeometryUnit::Pt, false, target)?;
    finite_length(&s.anchor_y_offset, GeometryUnit::Pt, false, target)?;
    pt_nonneg(&s.anchor_space_above, target)?;
    let custom_only = s.horizontal_reference_point.is_some()
        || s.vertical_alignment.is_some()
        || s.vertical_reference_point.is_some();
    let space = s.anchor_space_above.value != 0.0;
    let bad = match s.anchored_position {
        StudioAnchorPosition::Inline => s.horizontal_alignment.is_some() || custom_only || space,
        StudioAnchorPosition::AboveLine => custom_only,
        StudioAnchorPosition::Custom => space,
    };
    if bad {
        return Err(err(Code::InvalidField, target));
    }
    Ok(())
}

fn check_anchors(
    r: &StudioLayoutFlowRecords,
    d: &StudioDocument,
    stories: &BTreeSet<&str>,
    token: &CancellationToken,
) -> Result<()> {
    if r.anchored_objects.len() > MAX_FLOW_ITEMS {
        return Err(err(Code::BudgetExceeded, "anchored_objects"));
    }
    if let Some(s) = &r.anchored_object_defaults {
        check_settings(s, "anchored_object_defaults")?;
    }
    let mut seen = BTreeSet::new();
    for a in &r.anchored_objects {
        canceled(token)?;
        id(&a.layer_id, "SLYR", "anchored_objects/layer_id")?;
        id(&a.story_id, "STXT", "anchored_objects/story_id")?;
        let layer = d
            .layers
            .iter()
            .find(|l| l.layer_id == a.layer_id)
            .ok_or_else(|| err(Code::DanglingReference, &a.layer_id))?;
        // STU-LAY-127: a member of a group cannot be (re-)anchored.
        if let super::Parent::Node(n) = &layer.parent
            && n.node_kind == super::NodeKind::Layer
        {
            return Err(err(Code::IllegalParent, &a.layer_id));
        }
        if !stories.contains(a.story_id.as_str()) {
            return Err(err(Code::DanglingReference, &a.story_id));
        }
        if !seen.insert(a.layer_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &a.layer_id));
        }
        finite_length(&a.width, GeometryUnit::Pt, true, "anchored_objects/width")?;
        finite_length(&a.height, GeometryUnit::Pt, true, "anchored_objects/height")?;
        if a.width.value <= 0.0 || a.height.value <= 0.0 {
            return Err(err(Code::InvalidField, "anchored_objects/size"));
        }
        check_settings(&a.settings, "anchored_objects/settings")?;
    }
    Ok(())
}

fn check_xrefs(
    r: &StudioLayoutFlowRecords,
    stories: &BTreeSet<&str>,
    token: &CancellationToken,
) -> Result<()> {
    if r.text_anchors.len() > MAX_FLOW_ITEMS
        || r.xref_formats.len() > MAX_FLOW_ITEMS
        || r.cross_references.len() > MAX_FLOW_ITEMS
    {
        return Err(err(Code::BudgetExceeded, "cross_references"));
    }
    let (mut ids, mut names) = (BTreeSet::new(), BTreeSet::new());
    for a in &r.text_anchors {
        canceled(token)?;
        key_ok(&a.anchor_id, "text_anchors/anchor_id")?;
        name_ok(&a.name, "text_anchors/name")?;
        if !stories.contains(a.story_id.as_str()) {
            return Err(err(Code::DanglingReference, &a.story_id));
        }
        // STU-LAY-151: a name already in use.
        if !ids.insert(a.anchor_id.as_str()) || !names.insert(a.name.as_str()) {
            return Err(err(Code::DuplicateIdentity, &a.anchor_id));
        }
    }
    let mut formats = BTreeSet::new();
    for f in &r.xref_formats {
        canceled(token)?;
        key_ok(&f.format_id, "xref_formats/format_id")?;
        name_ok(&f.name, "xref_formats/name")?;
        if f.definition.len() > MAX_XREF_FORMAT_BYTES {
            return Err(err(Code::BudgetExceeded, "xref_formats/definition"));
        }
        if let Some(cs) = &f.character_style {
            id(cs, "STYS", "xref_formats/character_style")?;
        }
        if !formats.insert(f.format_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &f.format_id));
        }
    }
    let mut xrefs = BTreeSet::new();
    for x in &r.cross_references {
        canceled(token)?;
        key_ok(&x.xref_id, "cross_references/xref_id")?;
        if !xrefs.insert(x.xref_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &x.xref_id));
        }
        if !stories.contains(x.source_story_id.as_str()) {
            return Err(err(Code::DanglingReference, &x.source_story_id));
        }
        if x.source_start > x.source_end {
            return Err(err(Code::InvalidField, "cross_references/source"));
        }
        if !formats.contains(x.format_id.as_str()) {
            return Err(err(Code::DanglingReference, &x.format_id));
        }
        match &x.destination {
            StudioXrefDestination::Paragraph { story_id, .. } => {
                if !stories.contains(story_id.as_str()) {
                    return Err(err(Code::DanglingReference, story_id));
                }
            }
            StudioXrefDestination::TextAnchor { anchor_id } => {
                if !ids.contains(anchor_id.as_str()) {
                    return Err(err(Code::DanglingReference, anchor_id));
                }
            }
        }
    }
    Ok(())
}

fn check_conditions(
    r: &StudioLayoutFlowRecords,
    stories: &BTreeSet<&str>,
    token: &CancellationToken,
) -> Result<()> {
    if r.conditions.len() > MAX_FLOW_ITEMS
        || r.condition_sets.len() > MAX_FLOW_ITEMS
        || r.conditional_ranges.len() > MAX_FLOW_ITEMS
    {
        return Err(err(Code::BudgetExceeded, "conditions"));
    }
    let (mut ids, mut names) = (BTreeSet::new(), BTreeSet::new());
    for c in &r.conditions {
        canceled(token)?;
        key_ok(&c.condition_id, "conditions/condition_id")?;
        name_ok(&c.name, "conditions/name")?;
        let k = &c.indicator_colour;
        if ![k.r, k.g, k.b, k.a].iter().all(|v| (0.0..=1.0).contains(v)) {
            return Err(err(Code::InvalidField, "conditions/indicator_colour"));
        }
        if !ids.insert(c.condition_id.as_str()) || !names.insert(c.name.as_str()) {
            return Err(err(Code::DuplicateIdentity, &c.condition_id));
        }
    }
    let mut sets = BTreeSet::new();
    for s in &r.condition_sets {
        canceled(token)?;
        key_ok(&s.set_id, "condition_sets/set_id")?;
        name_ok(&s.name, "condition_sets/name")?;
        if !sets.insert(s.set_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &s.set_id));
        }
        let mut named = BTreeSet::new();
        for v in &s.visibility {
            if !ids.contains(v.condition_id.as_str()) {
                return Err(err(Code::DanglingReference, &v.condition_id));
            }
            if !named.insert(v.condition_id.as_str()) {
                return Err(err(Code::DuplicateIdentity, &v.condition_id));
            }
        }
    }
    for g in &r.conditional_ranges {
        canceled(token)?;
        if !stories.contains(g.story_id.as_str()) {
            return Err(err(Code::DanglingReference, &g.story_id));
        }
        if g.start >= g.end || g.conditions.is_empty() {
            return Err(err(Code::InvalidField, "conditional_ranges"));
        }
        for c in &g.conditions {
            if !ids.contains(c.as_str()) {
                return Err(err(Code::DanglingReference, c));
            }
        }
    }
    if let Some(p) = &r.conditional_text_preferences
        && let Some(a) = &p.active_set
        && !sets.contains(a.as_str())
    {
        return Err(err(Code::DanglingReference, a));
    }
    Ok(())
}
