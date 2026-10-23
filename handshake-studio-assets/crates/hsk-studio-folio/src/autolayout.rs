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
//! Declared gaps, not invented here: `layout_mode` `GRID` and the grid container and child fields
//! (STU-DS-146/147/150) are recognised and refused as `UnsupportedDescriptor`; the
//! `horizontal_padding` / `vertical_padding` convenience writers of STU-DS-144 are not stored (they
//! are derived by [`StudioAutoLayout::horizontal_padding`] / [`StudioAutoLayout::vertical_padding`]);
//! the cross-field agreement of container `AUTO`/`FIXED` with child `HUG`/`FILL`/`FIXED`, of
//! `layout_grow` with `FILL` and of `layout_align` with `STRETCH` is not stated by the spec and is not
//! enforced here; layout grids (STU-DS-153..156) are not stored.
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
    if a.layout_mode == LayoutMode::Grid {
        return Err(err(
            Code::UnsupportedDescriptor,
            format!("{target}/auto_layout/layout_mode"),
        ));
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
