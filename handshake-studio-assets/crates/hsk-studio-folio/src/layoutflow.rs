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
//! Paragraph flow overrides (STU-LAY-021, STU-LAY-125 "paragraph overrides"): per story paragraph
//! (named by its start byte) the span/split enumeration `{single_column, span_columns,
//! split_columns}`, the `span_count.all` sentinel AND the numeric count as separate fields (the
//! spec forbids encoding `all` as a magic number), the split sub-column count, space before/after
//! and the inside/outside gutters.
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
}
impl StudioLayoutFlowRecords {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self {
            schema_id: LayoutFlowRecordsSchema::V1,
            document_id: document_id.into(),
            frame_options: Vec::new(),
            paragraph_flows: Vec::new(),
        }
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
    Ok(())
}
