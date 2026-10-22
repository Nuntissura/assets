//! Text-on-path bindings of the vector record set (lane SMB-AI-TYPE-PATH-RECOLOR; STU-TYP-004,
//! STU-TYP-120, STU-TYP-121, STU-VEC-038, STU-LAY-023). Member `path_texts` of
//! `StudioVectorRecords`, skipped when empty, so record sets written before it are byte-identical.
//!
//! `StudioPathText` binds a `StudioTextStory` (`STXT`, `text_kind = path`) to the baseline path
//! `path_id` (`SVPT`) of vector layer `path_layer_id` and stores the STU-TYP-120/121 options:
//! segment-relative flow-extent handles `start_t` / `end_t`, `spacing` (points), `flipped`
//! (`not_flipped` | `flipped` | `undefined`), `path_type_alignment`, `text_type_alignment`,
//! `path_effect` (`rainbow` default, `skew`, `ribbon`, `stair_step`, `gravity`), the flow
//! direction and the extent alignment, and the per-run baseline offsets (half-open original-UTF-8
//! byte ranges of the story, points). Folio stores and validates only: Type lays the text out
//! (`hsk_studio_type::layout_path_text`), Nib owns the path geometry. Declared gaps: the handles
//! are not checked against the path's segment count (Folio does not hold path geometry); the story
//! is not resolved here (stories are standalone primitives).
use super::*;

/// Most path-text bindings of one record set.
pub const PATH_TEXT_MAX_ENTRIES: usize = 4096;
/// Most baseline-offset ranges of one binding.
pub const PATH_TEXT_MAX_OFFSETS: usize = 4096;
/// Largest handle value (segments).
pub const PATH_TEXT_MAX_T: f64 = 1.0e6;
/// Largest magnitude of `spacing` and of a baseline offset, points.
pub const PATH_TEXT_MAX_PT: f64 = 1.0e5;

/// STU-TYP-120 `flipped`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioPathTextFlip {
    #[default]
    NotFlipped,
    Flipped,
    Undefined,
}

/// STU-TYP-121 `path_type_alignment`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioPathTypeAlignment {
    TopPath,
    BottomPath,
    #[default]
    CenterPath,
}

/// STU-TYP-121 `text_type_alignment`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioTextTypeAlignment {
    Ascender,
    Descender,
    Center,
    #[default]
    Baseline,
    AboveRightEmBox,
    BelowLeftEmBox,
    AboveRightIcfBox,
    BelowLeftIcfBox,
}

/// STU-TYP-121 `path_effect`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioPathEffect {
    #[default]
    Rainbow,
    Skew,
    Ribbon,
    StairStep,
    Gravity,
}

/// STU-TYP-004 flow direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioPathFlowDirection {
    #[default]
    LeftToRight,
    RightToLeft,
}

/// Placement of the text inside the flow extent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioPathTextAlign {
    #[default]
    Start,
    Center,
    End,
}

/// One per-run baseline offset (STU-TYP-120): story bytes `[start, end)`, points.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioPathTextOffset {
    pub start: u64,
    pub end: u64,
    pub offset_pt: f64,
}

/// A text-on-path binding (module docs).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioPathText {
    /// `STXT` story laid along the path.
    pub story_id: String,
    /// `SLYR` vector layer holding the baseline path.
    pub path_layer_id: String,
    /// `SVPT` path of that layer.
    pub path_id: String,
    pub start_t: f64,
    pub end_t: f64,
    #[serde(default)]
    pub spacing: f64,
    #[serde(default)]
    pub flipped: StudioPathTextFlip,
    #[serde(default)]
    pub path_type_alignment: StudioPathTypeAlignment,
    #[serde(default)]
    pub text_type_alignment: StudioTextTypeAlignment,
    #[serde(default)]
    pub path_effect: StudioPathEffect,
    #[serde(default)]
    pub flow_direction: StudioPathFlowDirection,
    #[serde(default)]
    pub align: StudioPathTextAlign,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub baseline_offsets: Vec<StudioPathTextOffset>,
}

pub(super) fn validate_path_texts(r: &StudioVectorRecords, layers: &BTreeMap<&str, &StudioLayer>, cx: &mut Ctx) -> Result<()> {
    check(r.path_texts.len() <= PATH_TEXT_MAX_ENTRIES, Code::BudgetExceeded, "path_texts", "count")?;
    cx.nodes(r.path_texts.len(), "path_texts")?;
    let mut stories = BTreeSet::new();
    for p in &r.path_texts {
        cx.tick()?;
        let t = p.story_id.as_str();
        id(t, "STXT", "path_texts/story_id")?;
        unique(&mut stories, t, t)?;
        let l = layer(layers, &p.path_layer_id, &format!("{t}/path_layer_id"))?;
        id(&p.path_id, "SVPT", &format!("{t}/path_id"))?;
        let holds = matches!(&l.payload, Payload::Vector { path_ids } if path_ids.contains(&p.path_id));
        check(holds, Code::DanglingReference, t, "path_id")?;
        let handle = |v: f64| v.is_finite() && (0.0..=PATH_TEXT_MAX_T).contains(&v);
        check(handle(p.start_t), Code::InvalidField, t, "start_t")?;
        check(handle(p.end_t), Code::InvalidField, t, "end_t")?;
        check(p.spacing.is_finite() && p.spacing.abs() <= PATH_TEXT_MAX_PT, Code::InvalidField, t, "spacing")?;
        check(p.baseline_offsets.len() <= PATH_TEXT_MAX_OFFSETS, Code::BudgetExceeded, t, "baseline_offsets")?;
        cx.nodes(p.baseline_offsets.len(), t)?;
        let mut last = 0_u64;
        for (n, o) in p.baseline_offsets.iter().enumerate() {
            let ok = o.start >= last && o.end > o.start && o.offset_pt.is_finite() && o.offset_pt.abs() <= PATH_TEXT_MAX_PT;
            check(ok, Code::InvalidField, t, &format!("baseline_offsets/{n}"))?;
            last = o.end;
        }
    }
    Ok(())
}
