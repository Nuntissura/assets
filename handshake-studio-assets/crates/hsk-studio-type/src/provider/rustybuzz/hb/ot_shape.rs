use super::buffer::*;
use super::ot_layout::*;
use super::ot_layout_gpos_table::GPOS;
use super::ot_map::*;
use super::ot_shape_plan::hb_ot_shape_plan_t;
use super::ot_shaper::*;
use super::unicode::{CharExt, GeneralCategoryExt, hb_unicode_general_category_t};
use super::*;
use super::{hb_font_t, hb_tag_t};
use crate::Error;
use crate::provider::rustybuzz::BufferFlags;
use crate::provider::rustybuzz::hb::algs::{rb_flag, rb_flag_unsafe};
use crate::provider::rustybuzz::hb::buffer::glyph_flag::{
    SAFE_TO_INSERT_TATWEEL, UNSAFE_TO_BREAK, UNSAFE_TO_CONCAT,
};
use crate::provider::rustybuzz::hb::unicode::hb_gc::{
    RB_UNICODE_GENERAL_CATEGORY_LOWERCASE_LETTER, RB_UNICODE_GENERAL_CATEGORY_OTHER_LETTER,
    RB_UNICODE_GENERAL_CATEGORY_SPACE_SEPARATOR, RB_UNICODE_GENERAL_CATEGORY_TITLECASE_LETTER,
    RB_UNICODE_GENERAL_CATEGORY_UPPERCASE_LETTER,
};
use crate::provider::rustybuzz::{Direction, Feature, Language, Script};
use crate::provider::{Context, ProviderResult};

pub struct hb_ot_shape_planner_t<'f, 'a> {
    pub meter: &'f Context<'a>,
    pub face: &'f hb_font_t<'a>,
    pub direction: Direction,
    pub script: Option<Script>,
    pub ot_map: hb_ot_map_builder_t<'f, 'a>,
    pub apply_morx: bool,
    pub script_zero_marks: bool,
    pub script_fallback_mark_positioning: bool,
    pub shaper: &'static hb_ot_shaper_t,
}

impl<'f, 'a> hb_ot_shape_planner_t<'f, 'a> {
    pub fn new(
        face: &'f hb_font_t<'a>,
        direction: Direction,
        script: Option<Script>,
        language: Option<&Language>,
        meter: &'f Context<'a>,
    ) -> ProviderResult<Self> {
        let ot_map = hb_ot_map_builder_t::new(face, script, language, meter)?;

        let mut shaper = match script {
            Some(script) => hb_ot_shape_complex_categorize(
                script,
                direction,
                ot_map.chosen_script(TableIndex::GSUB),
            ),
            None => &DEFAULT_SHAPER,
        };

        let script_zero_marks = shaper.zero_width_marks != HB_OT_SHAPE_ZERO_WIDTH_MARKS_NONE;
        let script_fallback_mark_positioning = shaper.fallback_position;

        // https://github.com/harfbuzz/harfbuzz/issues/2124
        let apply_morx = false;

        // https://github.com/harfbuzz/harfbuzz/issues/1528
        if apply_morx && shaper as *const _ != &DEFAULT_SHAPER as *const _ {
            shaper = &DUMBER_SHAPER;
        }

        Ok(hb_ot_shape_planner_t {
            meter,
            face,
            direction,
            script,
            ot_map,
            apply_morx,
            script_zero_marks,
            script_fallback_mark_positioning,
            shaper,
        })
    }

    pub fn collect_features(&mut self, user_features: &[Feature]) -> ProviderResult<()> {
        self.meter.step()?;
        const COMMON_FEATURES: &[(hb_tag_t, hb_ot_map_feature_flags_t)] = &[
            (hb_tag_t::from_bytes(b"abvm"), F_GLOBAL),
            (hb_tag_t::from_bytes(b"blwm"), F_GLOBAL),
            (hb_tag_t::from_bytes(b"ccmp"), F_GLOBAL),
            (hb_tag_t::from_bytes(b"locl"), F_GLOBAL),
            (hb_tag_t::from_bytes(b"mark"), F_GLOBAL_MANUAL_JOINERS),
            (hb_tag_t::from_bytes(b"mkmk"), F_GLOBAL_MANUAL_JOINERS),
            (hb_tag_t::from_bytes(b"rlig"), F_GLOBAL),
        ];

        const HORIZONTAL_FEATURES: &[(hb_tag_t, hb_ot_map_feature_flags_t)] = &[
            (hb_tag_t::from_bytes(b"calt"), F_GLOBAL),
            (hb_tag_t::from_bytes(b"clig"), F_GLOBAL),
            (hb_tag_t::from_bytes(b"curs"), F_GLOBAL),
            (hb_tag_t::from_bytes(b"dist"), F_GLOBAL),
            (hb_tag_t::from_bytes(b"kern"), F_GLOBAL_HAS_FALLBACK),
            (hb_tag_t::from_bytes(b"liga"), F_GLOBAL),
            (hb_tag_t::from_bytes(b"rclt"), F_GLOBAL),
        ];

        let empty = F_NONE;

        self.ot_map.is_simple = true;

        self.ot_map
            .enable_feature(hb_tag_t::from_bytes(b"rvrn"), empty, 1)?;
        self.ot_map.add_gsub_pause(None)?;

        match self.direction {
            Direction::LeftToRight => {
                self.ot_map
                    .enable_feature(hb_tag_t::from_bytes(b"ltra"), empty, 1)?;
                self.ot_map
                    .enable_feature(hb_tag_t::from_bytes(b"ltrm"), empty, 1)?;
            }
            Direction::RightToLeft => {
                self.ot_map
                    .enable_feature(hb_tag_t::from_bytes(b"rtla"), empty, 1)?;
                self.ot_map
                    .add_feature(hb_tag_t::from_bytes(b"rtlm"), empty, 1)?;
            }
            _ => {}
        }

        // Automatic fractions.
        self.ot_map
            .add_feature(hb_tag_t::from_bytes(b"frac"), empty, 1)?;
        self.ot_map
            .add_feature(hb_tag_t::from_bytes(b"numr"), empty, 1)?;
        self.ot_map
            .add_feature(hb_tag_t::from_bytes(b"dnom"), empty, 1)?;

        // Random!
        self.ot_map.enable_feature(
            hb_tag_t::from_bytes(b"rand"),
            F_RANDOM,
            hb_ot_map_t::MAX_VALUE,
        )?;

        // Tracking.  We enable dummy feature here just to allow disabling
        // AAT 'trak' table using features.
        // https://github.com/harfbuzz/harfbuzz/issues/1303
        self.ot_map
            .enable_feature(hb_tag_t::from_bytes(b"trak"), F_HAS_FALLBACK, 1)?;

        self.ot_map
            .enable_feature(hb_tag_t::from_bytes(b"Harf"), empty, 1)?; // Considered required.
        self.ot_map
            .enable_feature(hb_tag_t::from_bytes(b"HARF"), empty, 1)?; // Considered discretionary.

        if let Some(func) = self.shaper.collect_features {
            self.ot_map.is_simple = false;
            func(self)?;
        }

        self.ot_map
            .enable_feature(hb_tag_t::from_bytes(b"Buzz"), empty, 1)?; // Considered required.
        self.ot_map
            .enable_feature(hb_tag_t::from_bytes(b"BUZZ"), empty, 1)?; // Considered discretionary.

        for &(tag, flags) in COMMON_FEATURES {
            self.meter.step()?;
            self.ot_map.add_feature(tag, flags, 1)?;
        }

        if self.direction.is_horizontal() {
            for &(tag, flags) in HORIZONTAL_FEATURES {
                self.meter.step()?;
                self.ot_map.add_feature(tag, flags, 1)?;
            }
        } else {
            // We only apply `vert` feature. See:
            // https://github.com/harfbuzz/harfbuzz/commit/d71c0df2d17f4590d5611239577a6cb532c26528
            // https://lists.freedesktop.org/archives/harfbuzz/2013-August/003490.html

            // We really want to find a 'vert' feature if there's any in the font, no
            // matter which script/langsys it is listed (or not) under.
            // See various bugs referenced from:
            // https://github.com/harfbuzz/harfbuzz/issues/63
            self.ot_map
                .enable_feature(hb_tag_t::from_bytes(b"vert"), F_GLOBAL_SEARCH, 1)?;
        }

        if !user_features.is_empty() {
            self.ot_map.is_simple = false;
        }

        for feature in user_features {
            self.meter.step()?;
            let flags = if feature.is_global() { F_GLOBAL } else { empty };
            self.ot_map.add_feature(feature.tag, flags, feature.value)?;
        }

        if let Some(func) = self.shaper.override_features {
            func(self)?;
        }

        Ok(())
    }

    pub fn compile(mut self, user_features: &[Feature]) -> ProviderResult<hb_ot_shape_plan_t<'a>> {
        let ot_map = self.ot_map.compile()?;
        let mut retained_features =
            crate::provider::storage::ChargedVec::with_capacity(user_features.len(), self.meter)?;
        retained_features.extend_copy(user_features, self.meter)?;

        let frac_mask = ot_map.get_1_mask(hb_tag_t::from_bytes(b"frac"), self.meter)?;
        let numr_mask = ot_map.get_1_mask(hb_tag_t::from_bytes(b"numr"), self.meter)?;
        let dnom_mask = ot_map.get_1_mask(hb_tag_t::from_bytes(b"dnom"), self.meter)?;
        let has_frac = frac_mask != 0 || (numr_mask != 0 && dnom_mask != 0);

        let rtlm_mask = ot_map.get_1_mask(hb_tag_t::from_bytes(b"rtlm"), self.meter)?;
        let has_vert = ot_map.get_1_mask(hb_tag_t::from_bytes(b"vert"), self.meter)? != 0;

        let horizontal = self.direction.is_horizontal();
        let kern_tag = if horizontal {
            hb_tag_t::from_bytes(b"kern")
        } else {
            hb_tag_t::from_bytes(b"vkrn")
        };
        let kern_mask = ot_map.get_mask(kern_tag, self.meter)?.0;
        let requested_kerning = kern_mask != 0;
        let trak_mask = ot_map
            .get_mask(hb_tag_t::from_bytes(b"trak"), self.meter)?
            .0;

        let has_gpos_kern = ot_map
            .get_feature_index(TableIndex::GPOS, kern_tag, self.meter)?
            .is_some();
        let disable_gpos = self.shaper.gpos_tag.is_some()
            && self.shaper.gpos_tag != ot_map.chosen_script(TableIndex::GPOS);

        // Decide who provides glyph classes. GDEF or Unicode.
        let fallback_glyph_classes = !hb_ot_layout_has_glyph_classes(self.face);

        // Decide who does substitutions. GSUB, morx, or fallback.
        let apply_morx = self.apply_morx;

        let mut apply_gpos = false;
        let mut apply_kerx = false;
        let mut apply_kern = false;

        // Decide who does positioning. GPOS, kerx, kern, or fallback.
        let has_kerx = false;
        let has_gsub = !apply_morx && self.face.tables().gsub.is_some();
        let has_gpos = !disable_gpos && self.face.tables().gpos.is_some();

        // Prefer GPOS over kerx if GSUB is present;
        // https://github.com/harfbuzz/harfbuzz/issues/3008
        if has_kerx && !(has_gsub && has_gpos) {
            apply_kerx = true;
        } else if has_gpos {
            apply_gpos = true;
        }

        if !apply_kerx && (!has_gpos_kern || !apply_gpos) {
            if has_kerx {
                apply_kerx = true;
            } else if hb_ot_layout_has_kerning(self.face) {
                apply_kern = true;
            }
        }

        let apply_fallback_kern = !(apply_gpos || apply_kerx || apply_kern);
        if requested_kerning && (apply_kern || apply_fallback_kern) {
            return Err(Error::UnsupportedProfile);
        }
        let zero_marks = self.script_zero_marks && !apply_kerx && !apply_kern;

        let has_gpos_mark = ot_map.get_1_mask(hb_tag_t::from_bytes(b"mark"), self.meter)? != 0;

        let mut adjust_mark_positioning_when_zeroing = !apply_gpos && !apply_kerx && !apply_kern;

        let fallback_mark_positioning =
            adjust_mark_positioning_when_zeroing && self.script_fallback_mark_positioning;

        if fallback_mark_positioning || (requested_kerning && (apply_kern || apply_fallback_kern)) {
            return Err(Error::UnsupportedProfile);
        }

        // If we're using morx shaping, we cancel mark position adjustment because
        // Apple Color Emoji assumes this will NOT be done when forming emoji sequences;
        // https://github.com/harfbuzz/harfbuzz/issues/2967.
        if apply_morx {
            adjust_mark_positioning_when_zeroing = false;
        }

        // Currently we always apply trak.
        let apply_trak = false;

        let mut plan = hb_ot_shape_plan_t {
            direction: self.direction,
            script: self.script,
            shaper: self.shaper,
            ot_map,
            data: super::ot_shape_plan::PlanData::Default,
            frac_mask,
            numr_mask,
            dnom_mask,
            rtlm_mask,
            kern_mask,
            trak_mask,
            requested_kerning,
            has_frac,
            has_vert,
            has_gpos_mark,
            zero_marks,
            fallback_glyph_classes,
            fallback_mark_positioning,
            adjust_mark_positioning_when_zeroing,
            apply_gpos,
            apply_kern,
            apply_fallback_kern,
            apply_kerx,
            apply_morx,
            apply_trak,
            user_features: retained_features,
        };

        if let Some(func) = self.shaper.create_data {
            plan.data = func(&plan, self.meter)?;
        }

        Ok(plan)
    }
}

pub struct hb_ot_shape_context_t<'b, 'a> {
    pub plan: &'b hb_ot_shape_plan_t<'a>,
    pub face: &'b hb_font_t<'a>,
    pub buffer: &'b mut hb_buffer_t<'a>,
    // Transient stuff
    pub target_direction: Direction,
    pub meter: &'b Context<'a>,
}

// Pull it all together!
pub fn shape_internal(ctx: &mut hb_ot_shape_context_t) -> ProviderResult<()> {
    ctx.meter.step()?;
    ctx.buffer.enter();

    initialize_masks(ctx)?;
    set_unicode_props(ctx.buffer, ctx.meter)?;
    insert_dotted_circle(ctx.buffer, ctx.face, ctx.meter)?;

    form_clusters(ctx.buffer, ctx.meter)?;

    ensure_native_direction(ctx.buffer, ctx.meter)?;

    if let Some(func) = ctx.plan.shaper.preprocess_text {
        func(ctx.plan, ctx.face, ctx.buffer, ctx.meter)?;
    }

    substitute_pre(ctx)?;
    position(ctx)?;
    substitute_post(ctx)?;

    propagate_flags(ctx.buffer, ctx.meter)?;

    ctx.buffer.direction = ctx.target_direction;
    ctx.buffer.leave();
    ctx.meter.poll()?;
    if !ctx.buffer.successful || ctx.buffer.shaping_failed {
        return Err(Error::PartialOutput);
    }
    Ok(())
}

fn substitute_pre(ctx: &mut hb_ot_shape_context_t) -> ProviderResult<()> {
    ctx.meter.step()?;
    hb_ot_substitute_default(ctx)?;
    hb_ot_substitute_plan(ctx)?;
    Ok(())
}

fn substitute_post(ctx: &mut hb_ot_shape_context_t) -> ProviderResult<()> {
    ctx.meter.step()?;
    deal_with_variation_selectors(ctx.buffer, ctx.meter)?;
    hide_default_ignorables(ctx.buffer, ctx.face, ctx.meter)?;

    if let Some(func) = ctx.plan.shaper.postprocess_glyphs {
        func(ctx.plan, ctx.face, ctx.buffer, ctx.meter)?;
    }
    Ok(())
}

fn hb_ot_substitute_default(ctx: &mut hb_ot_shape_context_t) -> ProviderResult<()> {
    ctx.meter.step()?;
    rotate_chars(ctx)?;

    ot_shape_normalize::_hb_ot_shape_normalize(ctx.plan, ctx.buffer, ctx.face, ctx.meter)?;

    setup_masks(ctx)?;

    map_glyphs_fast(ctx.buffer, ctx.meter)?;
    Ok(())
}

fn hb_ot_substitute_plan(ctx: &mut hb_ot_shape_context_t) -> ProviderResult<()> {
    ctx.meter.step()?;
    hb_ot_layout_substitute_start(ctx.face, ctx.buffer, ctx.meter)?;

    if ctx.plan.fallback_glyph_classes {
        hb_synthesize_glyph_classes(ctx.buffer, ctx.meter)?;
    }

    super::ot_layout_gsub_table::substitute(ctx.plan, ctx.face, ctx.buffer, ctx.meter)?;
    Ok(())
}

fn position(ctx: &mut hb_ot_shape_context_t) -> ProviderResult<()> {
    ctx.meter.step()?;
    ctx.buffer.clear_positions(ctx.meter)?;

    position_default(ctx)?;

    position_complex(ctx)?;

    if ctx.buffer.direction.is_backward() {
        ctx.buffer.reverse(ctx.meter)?;
    }
    Ok(())
}

fn position_default(ctx: &mut hb_ot_shape_context_t) -> ProviderResult<()> {
    ctx.meter.step()?;
    let len = ctx.buffer.len;

    if ctx.buffer.direction.is_horizontal() {
        for (info, pos) in ctx.buffer.info[..len]
            .iter()
            .zip(&mut ctx.buffer.pos[..len])
        {
            ctx.meter.step()?;
            pos.x_advance = ctx.face.glyph_h_advance(info.as_glyph(), ctx.meter)?;
        }
    } else {
        return Err(Error::UnsupportedProfile);
    }
    if ctx.buffer.scratch_flags & HB_BUFFER_SCRATCH_FLAG_HAS_SPACE_FALLBACK != 0 {
        return Err(Error::UnsupportedProfile);
    }
    Ok(())
}

fn position_complex(ctx: &mut hb_ot_shape_context_t) -> ProviderResult<()> {
    ctx.meter.step()?;
    // If the font has no GPOS and direction is forward, then when
    // zeroing mark widths, we shift the mark with it, such that the
    // mark is positioned hanging over the previous glyph.  When
    // direction is backward we don't shift and it will end up
    // hanging over the next glyph after the final reordering.
    //
    // Note: If fallback positioning happens, we don't care about
    // this as it will be overridden.
    let adjust_offsets_when_zeroing =
        ctx.plan.adjust_mark_positioning_when_zeroing && ctx.buffer.direction.is_forward();

    // We change glyph origin to what GPOS expects (horizontal), apply GPOS, change it back.

    GPOS::position_start(ctx.face, ctx.buffer, ctx.meter)?;

    if ctx.plan.zero_marks
        && ctx.plan.shaper.zero_width_marks == HB_OT_SHAPE_ZERO_WIDTH_MARKS_BY_GDEF_EARLY
    {
        zero_mark_widths_by_gdef(ctx.buffer, adjust_offsets_when_zeroing, ctx.meter)?;
    }

    position_by_plan(ctx.plan, ctx.face, ctx.buffer, ctx.meter)?;

    if ctx.plan.zero_marks
        && ctx.plan.shaper.zero_width_marks == HB_OT_SHAPE_ZERO_WIDTH_MARKS_BY_GDEF_LATE
    {
        zero_mark_widths_by_gdef(ctx.buffer, adjust_offsets_when_zeroing, ctx.meter)?;
    }

    // Finish off.  Has to follow a certain order.
    GPOS::position_finish_advances(ctx.face, ctx.buffer, ctx.meter)?;
    zero_width_default_ignorables(ctx.buffer, ctx.meter)?;

    GPOS::position_finish_offsets(ctx.face, ctx.buffer, ctx.meter)?;

    Ok(())
}

fn position_by_plan<'a>(
    plan: &hb_ot_shape_plan_t,
    face: &hb_font_t<'a>,
    buffer: &mut hb_buffer_t<'a>,
    meter: &Context<'a>,
) -> ProviderResult<()> {
    if plan.apply_gpos {
        super::ot_layout_gpos_table::position(plan, face, buffer, meter)?;
    }
    if plan.requested_kerning && (plan.apply_kern || plan.apply_fallback_kern) {
        return Err(Error::UnsupportedProfile);
    }
    Ok(())
}

fn initialize_masks(ctx: &mut hb_ot_shape_context_t) -> ProviderResult<()> {
    ctx.meter.step()?;
    let global_mask = ctx.plan.ot_map.get_global_mask();
    ctx.buffer.reset_masks(global_mask, ctx.meter)?;
    Ok(())
}

fn setup_masks(ctx: &mut hb_ot_shape_context_t) -> ProviderResult<()> {
    ctx.meter.step()?;
    setup_masks_fraction(ctx)?;

    if let Some(func) = ctx.plan.shaper.setup_masks {
        func(ctx.plan, ctx.face, ctx.buffer, ctx.meter)?;
    }

    for feature in ctx.plan.user_features.iter() {
        ctx.meter.step()?;
        if !feature.is_global() {
            let (mask, shift) = ctx.plan.ot_map.get_mask(feature.tag, ctx.meter)?;
            ctx.buffer.set_masks(
                feature.value << shift,
                mask,
                feature.start,
                feature.end,
                ctx.meter,
            )?;
        }
    }
    Ok(())
}

fn setup_masks_fraction(ctx: &mut hb_ot_shape_context_t) -> ProviderResult<()> {
    ctx.meter.step()?;
    let buffer = &mut ctx.buffer;
    if buffer.scratch_flags & HB_BUFFER_SCRATCH_FLAG_HAS_NON_ASCII == 0 || !ctx.plan.has_frac {
        return Ok(());
    }

    let (pre_mask, post_mask) = if buffer.direction.is_forward() {
        (
            ctx.plan.numr_mask | ctx.plan.frac_mask,
            ctx.plan.frac_mask | ctx.plan.dnom_mask,
        )
    } else {
        (
            ctx.plan.frac_mask | ctx.plan.dnom_mask,
            ctx.plan.numr_mask | ctx.plan.frac_mask,
        )
    };

    let len = buffer.len;
    let mut i = 0;
    while i < len {
        ctx.meter.step()?;
        // FRACTION SLASH
        if buffer.info[i].glyph_id == 0x2044 {
            let mut start = i;
            while start > 0
                && _hb_glyph_info_get_general_category(&buffer.info[start - 1])
                    == hb_unicode_general_category_t::DecimalNumber
            {
                ctx.meter.step()?;
                start -= 1;
            }

            let mut end = i + 1;
            while end < len
                && _hb_glyph_info_get_general_category(&buffer.info[end])
                    == hb_unicode_general_category_t::DecimalNumber
            {
                ctx.meter.step()?;
                end += 1;
            }

            if start == i || end == i + 1 {
                if start == i {
                    buffer.unsafe_to_concat(Some(start), Some(start + 1), ctx.meter)?;
                }

                if end == i + 1 {
                    buffer.unsafe_to_concat(Some(end - 1), Some(end), ctx.meter)?;
                }

                i += 1;
                continue;
            }

            buffer.unsafe_to_break(Some(start), Some(end), ctx.meter)?;

            for info in &mut buffer.info[start..i] {
                ctx.meter.step()?;
                info.mask |= pre_mask;
            }

            buffer.info[i].mask |= ctx.plan.frac_mask;

            for info in &mut buffer.info[i + 1..end] {
                ctx.meter.step()?;
                info.mask |= post_mask;
            }

            i = end;
        } else {
            i += 1;
        }
    }
    Ok(())
}

fn set_unicode_props(buffer: &mut hb_buffer_t, meter: &Context<'_>) -> ProviderResult<()> {
    // Implement enough of Unicode Graphemes here that shaping
    // in reverse-direction wouldn't break graphemes.  Namely,
    // we mark all marks and ZWJ and ZWJ,Extended_Pictographic
    // sequences as continuations.  The foreach_grapheme()
    // macro uses this bit.
    //
    // https://www.unicode.org/reports/tr29/#Regex_Definitions

    let len = buffer.len;

    let mut i = 0;
    while i < len {
        meter.step()?;
        // Mutably borrow buffer.info[i] and immutably borrow
        // buffer.info[i - 1] (if present) in a way that the borrow
        // checker can understand.
        let (prior, later) = buffer.info.split_at_mut(i);
        let info = &mut later[0];
        info.init_unicode_props(&mut buffer.scratch_flags);

        let gen_cat = _hb_glyph_info_get_general_category(info);

        if (rb_flag_unsafe(gen_cat.to_rb())
            & (rb_flag(RB_UNICODE_GENERAL_CATEGORY_LOWERCASE_LETTER)
                | rb_flag(RB_UNICODE_GENERAL_CATEGORY_UPPERCASE_LETTER)
                | rb_flag(RB_UNICODE_GENERAL_CATEGORY_TITLECASE_LETTER)
                | rb_flag(RB_UNICODE_GENERAL_CATEGORY_OTHER_LETTER)
                | rb_flag(RB_UNICODE_GENERAL_CATEGORY_SPACE_SEPARATOR)))
            != 0
        {
            i += 1;
            continue;
        }

        // Marks are already set as continuation by the above line.
        // Handle Emoji_Modifier and ZWJ-continuation.
        if gen_cat == hb_unicode_general_category_t::ModifierSymbol
            && matches!(info.glyph_id, 0x1F3FB..=0x1F3FF)
        {
            _hb_glyph_info_set_continuation(info);
        } else if i != 0 && matches!(info.glyph_id, 0x1F1E6..=0x1F1FF) {
            // Should never fail because we checked for i > 0.
            // TODO: use let chains when they become stable
            let prev = prior.last().unwrap();
            if matches!(prev.glyph_id, 0x1F1E6..=0x1F1FF) && !_hb_glyph_info_is_continuation(prev) {
                meter.step()?;
                _hb_glyph_info_set_continuation(info);
            }
        } else if _hb_glyph_info_is_zwj(info) {
            _hb_glyph_info_set_continuation(info);
            if let Some(next) = buffer.info[..len].get_mut(i + 1) {
                if next.as_char().is_emoji_extended_pictographic() {
                    next.init_unicode_props(&mut buffer.scratch_flags);
                    _hb_glyph_info_set_continuation(next);
                    i += 1;
                }
            }
        } else if matches!(info.glyph_id, 0xFF9E..=0xFF9F | 0xE0020..=0xE007F) {
            // Or part of the Other_Grapheme_Extend that is not marks.
            // As of Unicode 15 that is just:
            //
            // 200C          ; Other_Grapheme_Extend # Cf       ZERO WIDTH NON-JOINER
            // FF9E..FF9F    ; Other_Grapheme_Extend # Lm   [2] HALFWIDTH KATAKANA VOICED SOUND MARK..HALFWIDTH KATAKANA
            // SEMI-VOICED SOUND MARK E0020..E007F  ; Other_Grapheme_Extend # Cf  [96] TAG SPACE..CANCEL TAG
            //
            // ZWNJ is special, we don't want to merge it as there's no need, and keeping
            // it separate results in more granular clusters.
            // Tags are used for Emoji sub-region flag sequences:
            // https://github.com/harfbuzz/harfbuzz/issues/1556
            // Katakana ones were requested:
            // https://github.com/harfbuzz/harfbuzz/issues/3844
            _hb_glyph_info_set_continuation(info);
        }

        i += 1;
    }
    Ok(())
}

pub(crate) fn syllabic_clear_var(
    _: &hb_ot_shape_plan_t,
    _: &hb_font_t,
    buffer: &mut hb_buffer_t,
) -> bool {
    for info in buffer.info.iter_mut() {
        info.set_syllable(0);
    }

    false
}

fn insert_dotted_circle<'a>(
    buffer: &mut hb_buffer_t<'a>,
    face: &hb_font_t,
    meter: &Context<'a>,
) -> ProviderResult<()> {
    meter.step()?;
    if buffer.len == 0 {
        return Ok(());
    }
    if !buffer
        .flags
        .contains(BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE)
        && buffer.flags.contains(BufferFlags::BEGINNING_OF_TEXT)
        && buffer.context_len[0] == 0
        && _hb_glyph_info_is_unicode_mark(&buffer.info[0])
        && face.has_glyph(0x25CC, meter)?
    {
        let mut info = hb_glyph_info_t {
            glyph_id: 0x25CC,
            mask: buffer.cur(0).mask,
            cluster: buffer.cur(0).cluster,
            ..hb_glyph_info_t::default()
        };

        info.init_unicode_props(&mut buffer.scratch_flags);
        buffer.clear_output();
        buffer.output_info(info, meter)?;
        buffer.sync(meter)?;
    }
    Ok(())
}

fn form_clusters(buffer: &mut hb_buffer_t, meter: &Context<'_>) -> ProviderResult<()> {
    meter.step()?;
    if buffer.scratch_flags & HB_BUFFER_SCRATCH_FLAG_HAS_NON_ASCII != 0 {
        if buffer.cluster_level == HB_BUFFER_CLUSTER_LEVEL_MONOTONE_GRAPHEMES {
            foreach_grapheme!(buffer, meter, start, end, {
                buffer.merge_clusters(start, end, meter)?
            });
        } else {
            foreach_grapheme!(buffer, meter, start, end, {
                buffer.unsafe_to_break(Some(start), Some(end), meter)?;
            });
        }
    }

    Ok(())
}

fn ensure_native_direction(buffer: &mut hb_buffer_t, meter: &Context<'_>) -> ProviderResult<()> {
    meter.step()?;
    let dir = buffer.direction;
    let mut hor = buffer
        .script
        .and_then(Direction::from_script)
        .unwrap_or_default();

    // Numeric runs in natively-RTL scripts are actually native-LTR, so we reset
    // the horiz_dir if the run contains at least one decimal-number char, and no
    // letter chars (ideally we should be checking for chars with strong
    // directionality but hb-unicode currently lacks bidi categories).
    //
    // This allows digit sequences in Arabic etc to be shaped in "native"
    // direction, so that features like ligatures will work as intended.
    //
    // https://github.com/harfbuzz/harfbuzz/issues/501
    //
    // Similar thing about Regional_Indicators; They are bidi=L, but Script=Common.
    // If they are present in a run of natively-RTL text, they get assigned a script
    // with natively RTL direction, which would result in wrong shaping if we
    // assign such native RTL direction to them then. Detect that as well.
    //
    // https://github.com/harfbuzz/harfbuzz/issues/3314

    if hor == Direction::RightToLeft && dir == Direction::LeftToRight {
        let mut found_number = false;
        let mut found_letter = false;
        let mut found_ri = false;
        for info in buffer.info.iter() {
            meter.step()?;
            let gc = _hb_glyph_info_get_general_category(info);
            if gc == hb_unicode_general_category_t::DecimalNumber {
                found_number = true;
            } else if gc.is_letter() {
                found_letter = true;
                break;
            } else if matches!(info.glyph_id, 0x1F1E6..=0x1F1FF) {
                found_ri = true;
            }
        }
        if (found_number || found_ri) && !found_letter {
            hor = Direction::LeftToRight;
        }
    }

    // TODO vertical:
    // The only BTT vertical script is Ogham, but it's not clear to me whether OpenType
    // Ogham fonts are supposed to be implemented BTT or not.  Need to research that
    // first.
    if (dir.is_horizontal() && dir != hor && hor != Direction::Invalid)
        || (dir.is_vertical() && dir != Direction::TopToBottom)
    {
        _hb_ot_layout_reverse_graphemes(buffer, meter)?;
        buffer.direction = buffer.direction.reverse();
    }
    Ok(())
}

fn rotate_chars(ctx: &mut hb_ot_shape_context_t) -> ProviderResult<()> {
    ctx.meter.step()?;
    let len = ctx.buffer.len;

    if ctx.target_direction.is_backward() {
        let rtlm_mask = ctx.plan.rtlm_mask;

        for info in &mut ctx.buffer.info[..len] {
            ctx.meter.step()?;
            if let Some(c) = info.as_char().mirrored().map(u32::from) {
                if ctx.face.has_glyph(c, ctx.meter)? {
                    info.glyph_id = c;
                    continue;
                }
            }
            info.mask |= rtlm_mask;
        }
    }

    if ctx.target_direction.is_vertical() && !ctx.plan.has_vert {
        for info in &mut ctx.buffer.info[..len] {
            ctx.meter.step()?;
            if let Some(c) = info.as_char().vertical().map(u32::from) {
                if ctx.face.has_glyph(c, ctx.meter)? {
                    info.glyph_id = c;
                }
            }
        }
    }
    Ok(())
}

fn map_glyphs_fast(buffer: &mut hb_buffer_t, meter: &Context<'_>) -> ProviderResult<()> {
    // Normalization process sets up glyph_index(), we just copy it.
    let len = buffer.len;
    for info in &mut buffer.info[..len] {
        meter.step()?;
        info.glyph_id = info.glyph_index();
    }

    for info in &mut buffer.out_info_mut()[..len] {
        meter.step()?;
        info.glyph_id = info.glyph_index();
    }
    Ok(())
}

fn hb_synthesize_glyph_classes(
    buffer: &mut hb_buffer_t,
    meter: &Context<'_>,
) -> ProviderResult<()> {
    let len = buffer.len;
    for info in &mut buffer.info[..len] {
        meter.step()?;
        // Never mark default-ignorables as marks.
        // They won't get in the way of lookups anyway,
        // but having them as mark will cause them to be skipped
        // over if the lookup-flag says so, but at least for the
        // Mongolian variation selectors, looks like Uniscribe
        // marks them as non-mark.  Some Mongolian fonts without
        // GDEF rely on this.  Another notable character that
        // this applies to is COMBINING GRAPHEME JOINER.
        let class = if _hb_glyph_info_get_general_category(info)
            != hb_unicode_general_category_t::NonspacingMark
            || _hb_glyph_info_is_default_ignorable(info)
        {
            meter.step()?;
            GlyphPropsFlags::BASE_GLYPH
        } else {
            GlyphPropsFlags::MARK
        };

        info.set_glyph_props(class.bits());
    }
    Ok(())
}

fn zero_width_default_ignorables(
    buffer: &mut hb_buffer_t,
    meter: &Context<'_>,
) -> ProviderResult<()> {
    if buffer.scratch_flags & HB_BUFFER_SCRATCH_FLAG_HAS_DEFAULT_IGNORABLES != 0
        && !buffer
            .flags
            .contains(BufferFlags::PRESERVE_DEFAULT_IGNORABLES)
        && !buffer
            .flags
            .contains(BufferFlags::REMOVE_DEFAULT_IGNORABLES)
    {
        let len = buffer.len;
        for (info, pos) in buffer.info[..len].iter().zip(&mut buffer.pos[..len]) {
            meter.step()?;
            if _hb_glyph_info_is_default_ignorable(info) {
                pos.x_advance = 0;
                pos.y_advance = 0;
                pos.x_offset = 0;
                pos.y_offset = 0;
            }
        }
    }
    Ok(())
}

fn deal_with_variation_selectors(
    buffer: &mut hb_buffer_t,
    meter: &Context<'_>,
) -> ProviderResult<()> {
    if buffer.scratch_flags & HB_BUFFER_SCRATCH_FLAG_HAS_VARIATION_SELECTOR_FALLBACK == 0 {
        return Ok(());
    }

    // Note: In harfbuzz, this is part of the condition above (with OR), so it needs to stay
    // in sync.
    let Some(nf) = buffer.not_found_variation_selector else {
        return Ok(());
    };

    let count = buffer.len;
    let info = &mut buffer.info;
    let pos = &mut buffer.pos;

    for i in 0..count {
        meter.step()?;
        if _hb_glyph_info_is_variation_selector(&info[i]) {
            info[i].glyph_id = nf;
            pos[i].x_advance = 0;
            pos[i].y_advance = 0;
            pos[i].x_offset = 0;
            pos[i].y_offset = 0;
            _hb_glyph_info_set_variation_selector(&mut info[i], false);
        }
    }
    Ok(())
}

fn zero_mark_widths_by_gdef(
    buffer: &mut hb_buffer_t,
    adjust_offsets: bool,
    meter: &Context<'_>,
) -> ProviderResult<()> {
    let len = buffer.len;
    for (info, pos) in buffer.info[..len].iter().zip(&mut buffer.pos[..len]) {
        meter.step()?;
        if _hb_glyph_info_is_mark(info) {
            if adjust_offsets {
                pos.x_offset = pos
                    .x_offset
                    .checked_sub(pos.x_advance)
                    .ok_or(Error::Overflow)?;
                pos.y_offset = pos
                    .y_offset
                    .checked_sub(pos.y_advance)
                    .ok_or(Error::Overflow)?;
            }

            pos.x_advance = 0;
            pos.y_advance = 0;
        }
    }
    Ok(())
}

fn hide_default_ignorables<'a>(
    buffer: &mut hb_buffer_t<'a>,
    _: &hb_font_t,
    meter: &Context<'a>,
) -> ProviderResult<()> {
    meter.step()?;
    buffer.capture_source_groups(meter)?;
    if buffer.scratch_flags & HB_BUFFER_SCRATCH_FLAG_HAS_DEFAULT_IGNORABLES != 0 {
        if buffer
            .flags
            .contains(BufferFlags::PRESERVE_DEFAULT_IGNORABLES)
            || !buffer
                .flags
                .contains(BufferFlags::REMOVE_DEFAULT_IGNORABLES)
        {
            return Err(Error::UnsupportedProfile);
        }
        buffer.delete_glyphs_inplace(_hb_glyph_info_is_default_ignorable, meter)?;
    }
    Ok(())
}

fn propagate_flags(buffer: &mut hb_buffer_t, meter: &Context<'_>) -> ProviderResult<()> {
    meter.step()?;
    // Propagate cluster-level glyph flags to be the same on all cluster glyphs.
    // Simplifies using them.

    if buffer.scratch_flags & HB_BUFFER_SCRATCH_FLAG_HAS_GLYPH_FLAGS == 0 {
        return Ok(());
    }

    /* If we are producing SAFE_TO_INSERT_TATWEEL, then do two things:
     *
     * - If the places that the Arabic shaper marked as SAFE_TO_INSERT_TATWEEL,
     *   are UNSAFE_TO_BREAK, then clear the SAFE_TO_INSERT_TATWEEL,
     * - Any place that is SAFE_TO_INSERT_TATWEEL, is also now UNSAFE_TO_BREAK.
     *
     * We couldn't make this interaction earlier. It has to be done here.
     */
    let flip_tatweel = buffer
        .flags
        .contains(BufferFlags::PRODUCE_SAFE_TO_INSERT_TATWEEL);

    let clear_concat = !buffer.flags.contains(BufferFlags::PRODUCE_UNSAFE_TO_CONCAT);

    foreach_cluster!(buffer, meter, start, end, {
        let mut mask = 0;
        for info in &buffer.info[start..end] {
            meter.step()?;
            mask |= info.mask & glyph_flag::DEFINED;
        }

        if flip_tatweel {
            if mask & UNSAFE_TO_BREAK != 0 {
                mask &= !SAFE_TO_INSERT_TATWEEL;
            }

            if mask & SAFE_TO_INSERT_TATWEEL != 0 {
                mask |= UNSAFE_TO_BREAK | UNSAFE_TO_CONCAT;
            }
        }

        if clear_concat {
            mask &= !UNSAFE_TO_CONCAT;

            for info in &mut buffer.info[start..end] {
                meter.step()?;
                info.mask = mask;
            }
        }
    });
    Ok(())
}
