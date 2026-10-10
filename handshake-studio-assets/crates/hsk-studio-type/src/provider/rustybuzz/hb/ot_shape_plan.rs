use crate::Error;
use crate::provider::{Context, ProviderResult};

use crate::provider::storage::ChargedVec;

use super::ot_map::*;
use super::ot_shape::*;
use super::ot_shaper::*;
use super::{Direction, Feature, Language, Script, hb_font_t, hb_mask_t};

/// A reusable plan for shaping a text buffer.
pub struct hb_ot_shape_plan_t<'a> {
    pub(crate) direction: Direction,
    pub(crate) script: Option<Script>,
    pub(crate) shaper: &'static hb_ot_shaper_t,
    pub(crate) ot_map: hb_ot_map_t<'a>,
    pub(crate) data: PlanData,

    pub(crate) frac_mask: hb_mask_t,
    pub(crate) numr_mask: hb_mask_t,
    pub(crate) dnom_mask: hb_mask_t,
    pub(crate) rtlm_mask: hb_mask_t,
    pub(crate) kern_mask: hb_mask_t,
    pub(crate) trak_mask: hb_mask_t,

    pub(crate) requested_kerning: bool,
    pub(crate) has_frac: bool,
    pub(crate) has_vert: bool,
    pub(crate) has_gpos_mark: bool,
    pub(crate) zero_marks: bool,
    pub(crate) fallback_glyph_classes: bool,
    pub(crate) fallback_mark_positioning: bool,
    pub(crate) adjust_mark_positioning_when_zeroing: bool,

    pub(crate) apply_gpos: bool,
    pub(crate) apply_fallback_kern: bool,
    pub(crate) apply_kern: bool,
    pub(crate) apply_kerx: bool,
    pub(crate) apply_morx: bool,
    pub(crate) apply_trak: bool,

    pub(crate) user_features: ChargedVec<'a, Feature>,
}

impl<'a> hb_ot_shape_plan_t<'a> {
    /// Returns a plan that can be used for shaping any buffer with the
    /// provided properties.
    pub fn new(
        face: &hb_font_t<'a>,
        direction: Direction,
        script: Option<Script>,
        language: Option<&Language>,
        user_features: &[Feature],
        meter: &Context<'a>,
    ) -> ProviderResult<Self> {
        meter.step()?;
        if !matches!(
            script,
            Some(super::script::LATIN | super::script::ARABIC | super::script::HAN)
        ) || !direction.is_horizontal()
            || language.is_none()
        {
            return Err(Error::UnsupportedProfile);
        }
        let mut planner = hb_ot_shape_planner_t::new(face, direction, script, language, meter)?;
        planner.collect_features(user_features)?;
        planner.compile(user_features)
    }

    pub fn feature_available(
        &self,
        tag: super::hb_tag_t,
        meter: &Context<'_>,
    ) -> ProviderResult<bool> {
        self.ot_map.feature_available(tag, meter)
    }

    pub(crate) fn arabic_data(
        &self,
    ) -> ProviderResult<&super::ot_shaper_arabic::arabic_shape_plan_t> {
        match &self.data {
            PlanData::Arabic(data) => Ok(data),
            PlanData::Default => Err(Error::InternalInvariant),
        }
    }
}

pub(crate) enum PlanData {
    Default,
    Arabic(super::ot_shaper_arabic::arabic_shape_plan_t),
}
