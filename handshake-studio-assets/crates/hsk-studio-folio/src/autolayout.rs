//! Auto-layout, child layout, size clamp and constraint records (spec STU-DS-035, STU-DS-046,
//! STU-DS-048, STU-DS-144, STU-DS-145, STU-DS-148, STU-DS-149, STU-DS-151, STU-DS-152). They are the
//! fields the spec puts on a frame `StudioLayer`; `StudioLayer` has no field for them (adding one would
//! break every struct-literal constructor), so a `StudioNodeDesign` entry in `StudioDesignRecords`
//! carries them keyed by the node id. Folio stores and validates only; solving the layout is the
//! components crate's job.
//!
//! Numbers are plain `f64` in the owning document's geometry unit (STU-DS-145 `unit`), bounds are the
//! Studio-declared hard bounds of STU-DS-145/149, and two-decimal precision is not enforced. Enumeration
//! tokens are the spec tokens verbatim.
//!
//! GRID (lane SMB-FIG-GRID-1): `layout_mode` `GRID` carries the eight STU-DS-146 container fields in
//! [`StudioAutoLayout::grid`] ([`StudioGridLayout`], track sizes per STU-DS-147) and a grid child
//! carries the STU-DS-150 anchors and spans in [`StudioChildLayout::grid_child`]. Both members are
//! skipped when absent, so a record set without them encodes byte-identically to earlier ones
//! (test `non_grid_records_encode_byte_identically`). `grid` is present exactly when `layout_mode` is
//! `GRID`; the track lists match the counts; a span past the declared count is a validation error;
//! `grid_child_index` is read-only and derived (document order), never stored. Solving the grid and
//! the three STU-DS-146 commands are the components crate's.
//!
//! Declared gaps, not invented here: the
//! `horizontal_padding` / `vertical_padding` convenience writers of STU-DS-144 are not stored (they
//! are derived by [`StudioAutoLayout::horizontal_padding`] / [`StudioAutoLayout::vertical_padding`]);
//! the cross-field agreement of container `AUTO`/`FIXED` with child `HUG`/`FILL`/`FIXED`, of
//! `layout_grow` with `FILL` and of `layout_align` with `STRETCH` is not stated by the spec and is not
//! enforced here; layout grids (STU-DS-153..156) are not stored; the FIXED and FLEX track value
//! maxima are not declared in source (Studio declares [`TRACK_VALUE_MAX`], `studio_declared`).
#![deny(clippy::arithmetic_side_effects)]
use super::{Code, Result, err, values::Ctx};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

schema_token!(AutoLayoutSchema, "hsk.studio.auto_layout@1");
schema_token!(ConstraintSchema, "hsk.studio.constraint@1");

/// STU-DS-145 `studio_declared` hard bounds.
pub const SPACING_MIN: f64 = -10_000.0;
pub const SPACING_MAX: f64 = 100_000.0;
pub const PADDING_MAX: f64 = 100_000.0;
/// STU-DS-149 `studio_declared` hard maximum of a size clamp.
pub const CLAMP_MAX: f64 = 100_000.0;
/// STU-DS-145 grid gap column: hard_min 0, `studio_declared` hard_max 100000.
pub const GRID_GAP_MAX: f64 = 100_000.0;
/// STU-DS-146 `grid_row_count` / `grid_column_count`: hard_min 1, `studio_declared` hard_max 1000.
pub const GRID_COUNT_MAX: u32 = 1000;
/// STU-DS-147 FIXED (document unit) and FLEX (factor) value maximum; not declared in source,
/// `studio_declared`. Both have hard_min 0.
pub const TRACK_VALUE_MAX: f64 = 100_000.0;

/// STU-DS-144 `layout_mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LayoutMode {
    None,
    Horizontal,
    Vertical,
    Grid,
}
impl LayoutMode {
    /// True for the two flow modes that this crate stores and that can hold flow children.
    pub fn is_flow(self) -> bool {
        matches!(self, Self::Horizontal | Self::Vertical)
    }
}

/// STU-DS-144 `layout_wrap`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LayoutWrap {
    NoWrap,
    Wrap,
}

/// STU-DS-144 `primary_axis_sizing_mode` / `counter_axis_sizing_mode`; `AUTO` hugs the contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AxisSizingMode {
    Fixed,
    Auto,
}

/// STU-DS-144 `primary_axis_align_items`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PrimaryAxisAlign {
    Min,
    Center,
    Max,
    SpaceBetween,
}

/// STU-DS-144 `counter_axis_align_items`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CounterAxisAlign {
    Min,
    Center,
    Max,
    Baseline,
}

/// STU-DS-144 `counter_axis_align_content`; applies only when `layout_wrap` is `WRAP`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CounterAxisAlignContent {
    Auto,
    SpaceBetween,
}

/// The auto-layout container fields of a frame (STU-DS-144, flow modes).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAutoLayout {
    pub schema_id: AutoLayoutSchema,
    pub layout_mode: LayoutMode,
    pub layout_wrap: LayoutWrap,
    pub primary_axis_sizing_mode: AxisSizingMode,
    pub counter_axis_sizing_mode: AxisSizingMode,
    pub primary_axis_align_items: PrimaryAxisAlign,
    pub counter_axis_align_items: CounterAxisAlign,
    pub counter_axis_align_content: CounterAxisAlignContent,
    /// Gap along the flow axis; `-10000..=100000` (negative gaps are legal).
    pub item_spacing: f64,
    /// Gap between wrapped lines, or explicit null to use `item_spacing`.
    #[serde(deserialize_with = "super::required")]
    #[schemars(required)]
    pub counter_axis_spacing: Option<f64>,
    pub padding_top: f64,
    pub padding_right: f64,
    pub padding_bottom: f64,
    pub padding_left: f64,
    pub strokes_included_in_layout: bool,
    pub item_reverse_z_index: bool,
    /// STU-DS-146 grid fields; present exactly when `layout_mode` is `GRID`, absent (not encoded)
    /// otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid: Option<StudioGridLayout>,
}
impl StudioAutoLayout {
    /// The STU-DS-144 defaults: `layout_mode` `NONE`, so the frame is not an auto-layout container.
    pub fn none() -> Self {
        Self {
            schema_id: AutoLayoutSchema::V1,
            layout_mode: LayoutMode::None,
            layout_wrap: LayoutWrap::NoWrap,
            primary_axis_sizing_mode: AxisSizingMode::Auto,
            counter_axis_sizing_mode: AxisSizingMode::Auto,
            primary_axis_align_items: PrimaryAxisAlign::Min,
            counter_axis_align_items: CounterAxisAlign::Min,
            counter_axis_align_content: CounterAxisAlignContent::Auto,
            item_spacing: 0.0,
            counter_axis_spacing: None,
            padding_top: 0.0,
            padding_right: 0.0,
            padding_bottom: 0.0,
            padding_left: 0.0,
            strokes_included_in_layout: false,
            item_reverse_z_index: false,
            grid: None,
        }
    }
    /// STU-DS-144 `horizontal_padding` read: the left value, and whether left and right differ
    /// (the `mixed` flag of the read receipt).
    pub fn horizontal_padding(&self) -> (f64, bool) {
        (self.padding_left, self.padding_left != self.padding_right)
    }
    /// STU-DS-144 `vertical_padding` read: the top value, and whether top and bottom differ.
    pub fn vertical_padding(&self) -> (f64, bool) {
        (self.padding_top, self.padding_top != self.padding_bottom)
    }
}

/// STU-DS-148 `layout_align`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ChildAlign {
    Min,
    Center,
    Max,
    Stretch,
    Inherit,
}

/// STU-DS-148 `layout_positioning`; `ABSOLUTE` falls back to the child's constraints (STU-DS-048).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ChildPositioning {
    Auto,
    Absolute,
}

/// STU-DS-148 `layout_sizing_horizontal` / `layout_sizing_vertical`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ChildSizing {
    Fixed,
    Hug,
    Fill,
}

/// The fields of a child participating in auto layout (STU-DS-148).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioChildLayout {
    pub layout_align: ChildAlign,
    /// `0` or `1`: `1` grows to fill the remaining primary-axis space.
    #[schemars(range(min = 0, max = 1))]
    pub layout_grow: u8,
    pub layout_positioning: ChildPositioning,
    pub layout_sizing_horizontal: ChildSizing,
    pub layout_sizing_vertical: ChildSizing,
    /// STU-DS-150 placement in a `GRID` parent; absent (not encoded) for flow children and for
    /// grid children that auto-flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid_child: Option<StudioGridChild>,
}

/// STU-DS-147 track-size `type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GridTrackType {
    Fixed,
    Flex,
    Hug,
}

/// STU-DS-147 track-size record `{type, value?}`: `value` is the FIXED size (document unit) or the
/// FLEX factor (default 1), and is absent for HUG.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioGridTrackSize {
    #[serde(rename = "type")]
    pub track_type: GridTrackType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
}
impl StudioGridTrackSize {
    pub fn hug() -> Self {
        Self {
            track_type: GridTrackType::Hug,
            value: None,
        }
    }
    pub fn fixed(size: f64) -> Self {
        Self {
            track_type: GridTrackType::Fixed,
            value: Some(size),
        }
    }
    pub fn flex(factor: f64) -> Self {
        Self {
            track_type: GridTrackType::Flex,
            value: Some(factor),
        }
    }
}

/// STU-DS-146 `grid_auto_tracks`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GridAutoTracks {
    None,
    Rows,
}

/// STU-DS-146 `grid_items_positioning`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GridItemsPositioning {
    Manual,
    RowAutoFlow,
}

/// The eight STU-DS-146 grid container fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioGridLayout {
    /// `1..=1000` (`studio_declared` maximum).
    pub grid_row_count: u32,
    pub grid_column_count: u32,
    /// STU-DS-145 grid column: `0..=100000` (`studio_declared` maximum).
    pub grid_row_gap: f64,
    pub grid_column_gap: f64,
    /// Exactly `grid_row_count` entries.
    pub grid_row_sizes: Vec<StudioGridTrackSize>,
    /// Exactly `grid_column_count` entries.
    pub grid_column_sizes: Vec<StudioGridTrackSize>,
    pub grid_auto_tracks: GridAutoTracks,
    pub grid_items_positioning: GridItemsPositioning,
}
impl StudioGridLayout {
    /// The STU-DS-146 defaults: one row and one column, each a HUG track, gaps 0, auto tracks
    /// `NONE`, `ROW_AUTO_FLOW`.
    pub fn new() -> Self {
        Self {
            grid_row_count: 1,
            grid_column_count: 1,
            grid_row_gap: 0.0,
            grid_column_gap: 0.0,
            grid_row_sizes: vec![StudioGridTrackSize::hug()],
            grid_column_sizes: vec![StudioGridTrackSize::hug()],
            grid_auto_tracks: GridAutoTracks::None,
            grid_items_positioning: GridItemsPositioning::RowAutoFlow,
        }
    }
}
impl Default for StudioGridLayout {
    fn default() -> Self {
        Self::new()
    }
}

/// STU-DS-150 grid-child placement: anchors from 0, spans from 1, anchor + span within the
/// parent's declared counts. `grid_child_index` is derived and not stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioGridChild {
    pub grid_row_anchor_index: u32,
    pub grid_column_anchor_index: u32,
    pub grid_row_span: u32,
    pub grid_column_span: u32,
}

/// STU-DS-149 sizing clamps; each bound is `0..=100000` or explicit null (unclamped), and a minimum
/// above its maximum is an error, not a silent clamp.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioSizeClamps {
    #[serde(deserialize_with = "super::required")]
    #[schemars(required)]
    pub min_width: Option<f64>,
    #[serde(deserialize_with = "super::required")]
    #[schemars(required)]
    pub max_width: Option<f64>,
    #[serde(deserialize_with = "super::required")]
    #[schemars(required)]
    pub min_height: Option<f64>,
    #[serde(deserialize_with = "super::required")]
    #[schemars(required)]
    pub max_height: Option<f64>,
}

/// STU-DS-151 constraint type (both axes take the same five members).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConstraintType {
    Min,
    Center,
    Max,
    Stretch,
    Scale,
}

/// STU-DS-152 constraint record of a child that is not driven by auto layout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioConstraint {
    pub schema_id: ConstraintSchema,
    pub horizontal: ConstraintType,
    pub vertical: ConstraintType,
}
impl StudioConstraint {
    /// The STU-DS-152 default: pinned top-left.
    pub fn top_left() -> Self {
        Self {
            schema_id: ConstraintSchema::V1,
            horizontal: ConstraintType::Min,
            vertical: ConstraintType::Min,
        }
    }
}

fn within(v: f64, min: f64, max: f64) -> bool {
    v.is_finite() && (min..=max).contains(&v)
}

pub(crate) fn validate_auto_layout(a: &StudioAutoLayout, target: &str, c: &mut Ctx) -> Result<()> {
    c.tick()?;
    let bad = |what: &str| err(Code::InvalidField, format!("{target}/auto_layout/{what}"));
    match (a.layout_mode == LayoutMode::Grid, &a.grid) {
        (true, None) => {
            return Err(err(
                Code::MissingField,
                format!("{target}/auto_layout/grid"),
            ));
        }
        (false, Some(_)) => return Err(bad("grid")),
        (true, Some(g)) => validate_grid(g, target)?,
        (false, None) => {}
    }
    if a.layout_wrap == LayoutWrap::Wrap && !a.layout_mode.is_flow() {
        return Err(bad("layout_wrap"));
    }
    if a.counter_axis_align_content == CounterAxisAlignContent::SpaceBetween
        && a.layout_wrap != LayoutWrap::Wrap
    {
        return Err(bad("counter_axis_align_content"));
    }
    if !within(a.item_spacing, SPACING_MIN, SPACING_MAX) {
        return Err(bad("item_spacing"));
    }
    if a.counter_axis_spacing
        .is_some_and(|v| !within(v, SPACING_MIN, SPACING_MAX))
    {
        return Err(bad("counter_axis_spacing"));
    }
    for (name, v) in [
        ("padding_top", a.padding_top),
        ("padding_right", a.padding_right),
        ("padding_bottom", a.padding_bottom),
        ("padding_left", a.padding_left),
    ] {
        if !within(v, 0.0, PADDING_MAX) {
            return Err(bad(name));
        }
    }
    Ok(())
}

/// STU-DS-147: FIXED and FLEX carry a finite value in `0..=TRACK_VALUE_MAX`; HUG carries none.
pub fn grid_track_ok(t: &StudioGridTrackSize) -> bool {
    match (t.track_type, t.value) {
        (GridTrackType::Hug, None) => true,
        (GridTrackType::Fixed | GridTrackType::Flex, Some(v)) => within(v, 0.0, TRACK_VALUE_MAX),
        _ => false,
    }
}

/// STU-DS-145/146/147. The counts and the list lengths are compared before any entry is read.
fn validate_grid(g: &StudioGridLayout, target: &str) -> Result<()> {
    let bad = |what: &str| err(Code::InvalidField, format!("{target}/auto_layout/grid/{what}"));
    for (name, n) in [
        ("grid_row_count", g.grid_row_count),
        ("grid_column_count", g.grid_column_count),
    ] {
        if !(1..=GRID_COUNT_MAX).contains(&n) {
            return Err(bad(name));
        }
    }
    for (name, v) in [
        ("grid_row_gap", g.grid_row_gap),
        ("grid_column_gap", g.grid_column_gap),
    ] {
        if !within(v, 0.0, GRID_GAP_MAX) {
            return Err(bad(name));
        }
    }
    for (name, list, n) in [
        ("grid_row_sizes", &g.grid_row_sizes, g.grid_row_count),
        ("grid_column_sizes", &g.grid_column_sizes, g.grid_column_count),
    ] {
        if usize::try_from(n).ok() != Some(list.len()) {
            return Err(bad(name));
        }
        if let Some(i) = list.iter().position(|t| !grid_track_ok(t)) {
            return Err(bad(&format!("{name}/{i}")));
        }
    }
    Ok(())
}

/// STU-DS-150: a `grid_child` needs a `GRID` parent and an `AUTO` child; spans are at least 1 and
/// anchor + span stays within the parent's declared count (a span past the count is a validation
/// error). `parent` is the auto layout of the child's document parent.
pub(crate) fn validate_grid_child(
    k: &StudioChildLayout,
    parent: Option<&StudioAutoLayout>,
    target: &str,
) -> Result<()> {
    let Some(gc) = &k.grid_child else {
        return Ok(());
    };
    let bad = |what: &str| err(Code::InvalidField, format!("{target}/child_layout/grid_child{what}"));
    let grid = parent
        .filter(|p| p.layout_mode == LayoutMode::Grid)
        .and_then(|p| p.grid.as_ref());
    let Some(g) = grid else {
        return Err(bad(""));
    };
    if k.layout_positioning == ChildPositioning::Absolute {
        return Err(bad(""));
    }
    for (axis, anchor, span, count) in [
        ("row", gc.grid_row_anchor_index, gc.grid_row_span, g.grid_row_count),
        (
            "column",
            gc.grid_column_anchor_index,
            gc.grid_column_span,
            g.grid_column_count,
        ),
    ] {
        if anchor >= count {
            return Err(bad(&format!("/grid_{axis}_anchor_index")));
        }
        if span < 1 || anchor.checked_add(span).is_none_or(|end| end > count) {
            return Err(bad(&format!("/grid_{axis}_span")));
        }
    }
    Ok(())
}

pub(crate) fn validate_clamps(k: &StudioSizeClamps, target: &str) -> Result<()> {
    let bad = |what: &str| err(Code::InvalidField, format!("{target}/size_clamps/{what}"));
    for (name, v) in [
        ("min_width", k.min_width),
        ("max_width", k.max_width),
        ("min_height", k.min_height),
        ("max_height", k.max_height),
    ] {
        if v.is_some_and(|v| !within(v, 0.0, CLAMP_MAX)) {
            return Err(bad(name));
        }
    }
    if k.min_width.zip(k.max_width).is_some_and(|(lo, hi)| lo > hi) {
        return Err(bad("width"));
    }
    if k.min_height
        .zip(k.max_height)
        .is_some_and(|(lo, hi)| lo > hi)
    {
        return Err(bad("height"));
    }
    Ok(())
}
