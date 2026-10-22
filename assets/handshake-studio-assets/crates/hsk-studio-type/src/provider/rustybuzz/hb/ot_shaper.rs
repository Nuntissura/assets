use super::ot_shape_plan::PlanData;
use crate::provider::{Context, ProviderResult};

use super::buffer::*;
use super::ot_shape::*;
use super::ot_shape_normalize::*;
use super::ot_shape_plan::hb_ot_shape_plan_t;
use super::{Direction, Script, hb_font_t, hb_tag_t, script};

impl hb_glyph_info_t {
    pub(crate) fn ot_shaper_var_u8_category(&self) -> u8 {
        let v: &[u8; 4] = bytemuck::cast_ref(&self.var2);
        v[2]
    }

    pub(crate) fn set_ot_shaper_var_u8_category(&mut self, c: u8) {
        let v: &mut [u8; 4] = bytemuck::cast_mut(&mut self.var2);
        v[2] = c;
    }

    pub(crate) fn ot_shaper_var_u8_auxiliary(&self) -> u8 {
        let v: &[u8; 4] = bytemuck::cast_ref(&self.var2);
        v[3]
    }

    pub(crate) fn set_ot_shaper_var_u8_auxiliary(&mut self, c: u8) {
        let v: &mut [u8; 4] = bytemuck::cast_mut(&mut self.var2);
        v[3] = c;
    }
}

pub const MAX_COMBINING_MARKS: usize = 32;

pub type hb_ot_shape_zero_width_marks_type_t = u32;
pub const HB_OT_SHAPE_ZERO_WIDTH_MARKS_NONE: u32 = 0;
pub const HB_OT_SHAPE_ZERO_WIDTH_MARKS_BY_GDEF_EARLY: u32 = 1;
pub const HB_OT_SHAPE_ZERO_WIDTH_MARKS_BY_GDEF_LATE: u32 = 2;

pub type DecomposeFn = fn(&hb_ot_shape_normalize_context_t, char) -> Option<(char, char)>;
pub type ComposeFn = fn(&hb_ot_shape_normalize_context_t, char, char) -> Option<char>;

pub const DEFAULT_SHAPER: hb_ot_shaper_t = hb_ot_shaper_t {
    collect_features: None,
    override_features: None,
    create_data: None,
    preprocess_text: None,
    postprocess_glyphs: None,
    normalization_preference: HB_OT_SHAPE_NORMALIZATION_MODE_AUTO,
    decompose: None,
    compose: None,
    setup_masks: None,
    gpos_tag: None,
    reorder_marks: None,
    zero_width_marks: HB_OT_SHAPE_ZERO_WIDTH_MARKS_BY_GDEF_LATE,
    fallback_position: true,
};

pub struct hb_ot_shaper_t {
    /// Called during `shape_plan()`.
    /// Shapers should use plan.map to add their features and callbacks.
    pub collect_features: Option<fn(&mut hb_ot_shape_planner_t) -> ProviderResult<()>>,

    /// Called during `shape_plan()`.
    /// Shapers should use plan.map to override features and add callbacks after
    /// common features are added.
    pub override_features: Option<fn(&mut hb_ot_shape_planner_t) -> ProviderResult<()>>,

    /// Called at the end of `shape_plan()`.
    /// Whatever shapers return will be accessible through `plan.data()` later.
    pub create_data: Option<fn(&hb_ot_shape_plan_t, &Context<'_>) -> ProviderResult<PlanData>>,

    /// Called during `shape()`.
    /// Shapers can use to modify text before shaping starts.
    pub preprocess_text: Option<
        for<'a> fn(
            &hb_ot_shape_plan_t<'a>,
            &hb_font_t<'a>,
            &mut hb_buffer_t<'a>,
            &Context<'a>,
        ) -> ProviderResult<()>,
    >,

    /// Called during `shape()`.
    /// Shapers can use to modify text before shaping starts.
    pub postprocess_glyphs: Option<
        for<'a> fn(
            &hb_ot_shape_plan_t<'a>,
            &hb_font_t<'a>,
            &mut hb_buffer_t<'a>,
            &Context<'a>,
        ) -> ProviderResult<()>,
    >,

    /// How to normalize.
    pub normalization_preference: hb_ot_shape_normalization_mode_t,

    /// Called during `shape()`'s normalization.
    pub decompose: Option<DecomposeFn>,

    /// Called during `shape()`'s normalization.
    pub compose: Option<ComposeFn>,

    /// Called during `shape()`.
    /// Shapers should use map to get feature masks and set on buffer.
    /// Shapers may NOT modify characters.
    pub setup_masks: Option<
        for<'a> fn(
            &hb_ot_shape_plan_t<'a>,
            &hb_font_t<'a>,
            &mut hb_buffer_t<'a>,
            &Context<'a>,
        ) -> ProviderResult<()>,
    >,

    /// If not `None`, then must match found GPOS script tag for
    /// GPOS to be applied.  Otherwise, fallback positioning will be used.
    pub gpos_tag: Option<hb_tag_t>,

    /// Called during `shape()`.
    /// Shapers can use to modify ordering of combining marks.
    pub reorder_marks: Option<
        fn(&hb_ot_shape_plan_t, &mut hb_buffer_t, usize, usize, &Context<'_>) -> ProviderResult<()>,
    >,

    /// If and when to zero-width marks.
    pub zero_width_marks: hb_ot_shape_zero_width_marks_type_t,

    /// Whether to use fallback mark positioning.
    pub fallback_position: bool,
}

// Same as default but no mark advance zeroing / fallback positioning.
// Dumbest shaper ever, basically.
pub const DUMBER_SHAPER: hb_ot_shaper_t = hb_ot_shaper_t {
    collect_features: None,
    override_features: None,
    create_data: None,
    preprocess_text: None,
    postprocess_glyphs: None,
    normalization_preference: HB_OT_SHAPE_NORMALIZATION_MODE_AUTO,
    decompose: None,
    compose: None,
    setup_masks: None,
    gpos_tag: None,
    reorder_marks: None,
    zero_width_marks: HB_OT_SHAPE_ZERO_WIDTH_MARKS_NONE,
    fallback_position: false,
};

pub fn hb_ot_shape_complex_categorize(
    script: Script,
    direction: Direction,
    _gsub_script: Option<hb_tag_t>,
) -> &'static hb_ot_shaper_t {
    // The typed planner entry rejects every other script/profile before dispatch.
    if script == script::ARABIC && direction.is_horizontal() {
        &super::ot_shaper_arabic::ARABIC_SHAPER
    } else {
        &DEFAULT_SHAPER
    }
}
