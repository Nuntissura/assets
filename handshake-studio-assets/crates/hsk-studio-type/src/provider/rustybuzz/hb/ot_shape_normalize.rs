use super::buffer::*;
use super::common::hb_codepoint_t;
use super::hb_font_t;
use super::ot_layout::*;
use super::ot_shape_plan::hb_ot_shape_plan_t;
use super::ot_shaper::{ComposeFn, DecomposeFn, MAX_COMBINING_MARKS};
use super::unicode::{CharExt, hb_unicode_funcs_t};
use crate::Error;
use crate::provider::{Context, ProviderResult};

pub struct hb_ot_shape_normalize_context_t<'b, 'a> {
    pub plan: &'b hb_ot_shape_plan_t<'a>,
    pub buffer: &'b mut hb_buffer_t<'a>,
    pub face: &'b hb_font_t<'a>,
    pub meter: &'b Context<'a>,
    pub decompose: DecomposeFn,
    pub compose: ComposeFn,
}

impl hb_ot_shape_normalize_context_t<'_, '_> {
    pub(crate) fn override_decompose_and_compose(
        &mut self,
        decompose: Option<DecomposeFn>,
        compose: Option<ComposeFn>,
    ) {
        if let Some(decompose) = decompose {
            self.decompose = decompose;
        }

        if let Some(compose) = compose {
            self.compose = compose;
        }
    }
}

pub type hb_ot_shape_normalization_mode_t = i32;
pub const HB_OT_SHAPE_NORMALIZATION_MODE_NONE: i32 = 0;
pub const HB_OT_SHAPE_NORMALIZATION_MODE_DECOMPOSED: i32 = 1;
pub const HB_OT_SHAPE_NORMALIZATION_MODE_COMPOSED_DIACRITICS: i32 = 2; /* Never composes base-to-base */
pub const HB_OT_SHAPE_NORMALIZATION_MODE_COMPOSED_DIACRITICS_NO_SHORT_CIRCUIT: i32 = 3; /* Always fully decomposes and then recompose back */
pub const HB_OT_SHAPE_NORMALIZATION_MODE_AUTO: i32 = 4; /* See hb-ot-shape-normalize.cc for logic. */
#[allow(dead_code)]
pub const HB_OT_SHAPE_NORMALIZATION_MODE_DEFAULT: i32 = HB_OT_SHAPE_NORMALIZATION_MODE_AUTO;

// HIGHLEVEL DESIGN:
//
// This file exports one main function: normalize().
//
// This function closely reflects the Unicode Normalization Algorithm,
// yet it's different.
//
// Each shaper specifies whether it prefers decomposed (NFD) or composed (NFC).
// The logic however tries to use whatever the font can support.
//
// In general what happens is that: each grapheme is decomposed in a chain
// of 1:2 decompositions, marks reordered, and then recomposed if desired,
// so far it's like Unicode Normalization.  However, the decomposition and
// recomposition only happens if the font supports the resulting characters.
//
// The goals are:
//
//   - Try to render all canonically equivalent strings similarly.  To really
//     achieve this we have to always do the full decomposition and then
//     selectively recompose from there.  It's kinda too expensive though, so
//     we skip some cases.  For example, if composed is desired, we simply
//     don't touch 1-character clusters that are supported by the font, even
//     though their NFC may be different.
//
//   - When a font has a precomposed character for a sequence but the 'ccmp'
//     feature in the font is not adequate, use the precomposed character
//     which typically has better mark positioning.
//
//   - When a font does not support a combining mark, but supports it precomposed
//     with previous base, use that.  This needs the itemizer to have this
//     knowledge too.  We need to provide assistance to the itemizer.
//
//   - When a font does not support a character but supports its canonical
//     decomposition, well, use the decomposition.
//
//   - The shapers can customize the compose and decompose functions to
//     offload some of their requirements to the normalizer.  For example, the
//     Indic shaper may want to disallow recomposing of two matras.

fn decompose_unicode(
    _: &hb_ot_shape_normalize_context_t,
    ab: hb_codepoint_t,
) -> Option<(hb_codepoint_t, hb_codepoint_t)> {
    super::unicode::decompose(ab)
}

fn compose_unicode(
    _: &hb_ot_shape_normalize_context_t,
    a: hb_codepoint_t,
    b: hb_codepoint_t,
) -> Option<hb_codepoint_t> {
    super::unicode::compose(a, b)
}

fn set_glyph(
    info: &mut hb_glyph_info_t,
    font: &hb_font_t,
    meter: &Context<'_>,
) -> ProviderResult<()> {
    meter.step()?;
    if let Some(glyph_id) = font.get_nominal_glyph(info.glyph_id, meter)? {
        info.set_glyph_index(u32::from(glyph_id.0));
    }

    Ok(())
}

fn output_char<'a>(
    buffer: &mut hb_buffer_t<'a>,
    unichar: u32,
    glyph: u32,
    meter: &Context<'a>,
) -> ProviderResult<()> {
    meter.step()?;
    // This is very confusing indeed.
    buffer.cur_mut(0).set_glyph_index(glyph);
    buffer.output_glyph(unichar, meter)?;
    // TODO: should be _hb_glyph_info_set_unicode_props (&buffer->prev(), buffer);
    let mut flags = buffer.scratch_flags;
    buffer.prev_mut().init_unicode_props(&mut flags);
    buffer.scratch_flags = flags;

    Ok(())
}

fn next_char<'a>(
    buffer: &mut hb_buffer_t<'a>,
    glyph: u32,
    meter: &Context<'a>,
) -> ProviderResult<()> {
    meter.step()?;
    buffer.cur_mut(0).set_glyph_index(glyph);
    buffer.next_glyph(meter)?;

    Ok(())
}

fn skip_char(buffer: &mut hb_buffer_t, meter: &Context<'_>) -> ProviderResult<()> {
    meter.step()?;
    buffer.skip_glyph();

    Ok(())
}

/// Returns 0 if didn't decompose, number of resulting characters otherwise.
fn decompose(
    ctx: &mut hb_ot_shape_normalize_context_t,
    shortest: bool,
    ab: hb_codepoint_t,
) -> ProviderResult<u32> {
    let _depth = ctx.meter.enter()?;
    let Some((a, b)) = (ctx.decompose)(ctx, ab) else {
        return Ok(0);
    };
    let a_glyph = ctx.face.get_nominal_glyph(u32::from(a), ctx.meter)?;
    let b_glyph = if b != '\0' {
        let Some(glyph) = ctx.face.get_nominal_glyph(u32::from(b), ctx.meter)? else {
            return Ok(0);
        };
        Some(glyph)
    } else {
        None
    };
    if !shortest || a_glyph.is_none() {
        let count = decompose(ctx, shortest, a)?;
        if count != 0 {
            if let Some(glyph) = b_glyph {
                output_char(ctx.buffer, u32::from(b), u32::from(glyph.0), ctx.meter)?;
                return count.checked_add(1).ok_or(Error::Overflow);
            }
            return Ok(count);
        }
    }
    let Some(glyph) = a_glyph else {
        return Ok(0);
    };
    output_char(ctx.buffer, u32::from(a), u32::from(glyph.0), ctx.meter)?;
    if let Some(glyph) = b_glyph {
        output_char(ctx.buffer, u32::from(b), u32::from(glyph.0), ctx.meter)?;
        return Ok(2);
    }
    Ok(1)
}

fn decompose_current_character(
    ctx: &mut hb_ot_shape_normalize_context_t,
    shortest: bool,
) -> ProviderResult<()> {
    ctx.meter.step()?;
    let u = ctx.buffer.cur(0).as_char();
    let glyph = ctx.face.get_nominal_glyph(u32::from(u), ctx.meter)?;

    // TODO: different to harfbuzz, sync
    if !shortest || glyph.is_none() {
        if decompose(ctx, shortest, u)? > 0 {
            skip_char(ctx.buffer, ctx.meter)?;
            return Ok(());
        }
    }

    // TODO: different to harfbuzz, sync
    if let Some(glyph) = glyph {
        next_char(ctx.buffer, u32::from(glyph.0), ctx.meter)?;
        return Ok(());
    }

    if _hb_glyph_info_is_unicode_space(ctx.buffer.cur(0)) {
        let space_type = u.space_fallback();
        if space_type != hb_unicode_funcs_t::NOT_SPACE {
            let space_glyph = ctx
                .face
                .get_nominal_glyph(0x0020, ctx.meter)?
                .or(ctx.buffer.invisible);

            if let Some(space_glyph) = space_glyph {
                _hb_glyph_info_set_unicode_space_fallback_type(ctx.buffer.cur_mut(0), space_type);
                next_char(ctx.buffer, u32::from(space_glyph.0), ctx.meter)?;
                ctx.buffer.scratch_flags |= HB_BUFFER_SCRATCH_FLAG_HAS_SPACE_FALLBACK;
                return Ok(());
            }
        }
    }

    // U+2011 is the only sensible character that is a no-break version of another character
    // and not a space.  The space ones are handled already.  Handle this lone one.
    if u == '\u{2011}' {
        if let Some(other_glyph) = ctx.face.get_nominal_glyph(0x2010, ctx.meter)? {
            next_char(ctx.buffer, u32::from(other_glyph.0), ctx.meter)?;
            return Ok(());
        }
    }

    // Insert a .notdef glyph if decomposition failed.
    next_char(ctx.buffer, 0, ctx.meter)?;

    Ok(())
}

fn handle_variation_selector_cluster(
    ctx: &mut hb_ot_shape_normalize_context_t,
    end: usize,
    _: bool,
) -> ProviderResult<()> {
    ctx.meter.step()?;
    let face = ctx.face;

    // Currently if there's a variation-selector we give-up on normalization, it's just too hard.
    let buffer = &mut ctx.buffer;
    while buffer.idx < end - 1 && buffer.successful {
        ctx.meter.step()?;
        if buffer.cur(1).as_char().is_variation_selector() {
            if let Some(glyph_id) = face.glyph_variation_index_bounded(
                buffer.cur(0).as_char(),
                buffer.cur(1).as_char(),
                ctx.meter,
            )? {
                buffer.cur_mut(0).set_glyph_index(u32::from(glyph_id.0));
                let unicode = buffer.cur(0).glyph_id;
                buffer.replace_glyphs(2, 1, &[unicode], ctx.meter)?;
            } else {
                // Just pass on the two characters separately, let GSUB do its magic.
                set_glyph(buffer.cur_mut(0), face, ctx.meter)?;
                buffer.next_glyph(ctx.meter)?;

                buffer.scratch_flags |= HB_BUFFER_SCRATCH_FLAG_HAS_VARIATION_SELECTOR_FALLBACK;

                _hb_glyph_info_set_variation_selector(buffer.cur_mut(0), true);

                if buffer.not_found_variation_selector.is_some() {
                    _hb_glyph_info_clear_default_ignorable(buffer.cur_mut(0))
                }

                set_glyph(buffer.cur_mut(0), face, ctx.meter)?;
                buffer.next_glyph(ctx.meter)?;
            }

            // Skip any further variation selectors.
            while buffer.idx < end && buffer.cur(0).as_char().is_variation_selector() {
                ctx.meter.step()?;
                set_glyph(buffer.cur_mut(0), face, ctx.meter)?;
                buffer.next_glyph(ctx.meter)?;
            }
        } else {
            set_glyph(buffer.cur_mut(0), face, ctx.meter)?;
            buffer.next_glyph(ctx.meter)?;
        }
    }

    if ctx.buffer.idx < end {
        set_glyph(ctx.buffer.cur_mut(0), face, ctx.meter)?;
        ctx.buffer.next_glyph(ctx.meter)?;
    }

    Ok(())
}

fn decompose_multi_char_cluster(
    ctx: &mut hb_ot_shape_normalize_context_t,
    end: usize,
    short_circuit: bool,
) -> ProviderResult<()> {
    ctx.meter.step()?;
    let mut i = ctx.buffer.idx;
    while i < end && ctx.buffer.successful {
        ctx.meter.step()?;
        if ctx.buffer.info[i].as_char().is_variation_selector() {
            handle_variation_selector_cluster(ctx, end, short_circuit)?;
            return Ok(());
        }
        i += 1;
    }

    while ctx.buffer.idx < end && ctx.buffer.successful {
        ctx.meter.step()?;
        decompose_current_character(ctx, short_circuit)?;
    }

    Ok(())
}

fn compare_combining_class(pa: &hb_glyph_info_t, pb: &hb_glyph_info_t) -> bool {
    let a = _hb_glyph_info_get_modified_combining_class(pa);
    let b = _hb_glyph_info_get_modified_combining_class(pb);
    a > b
}

pub fn _hb_ot_shape_normalize<'a>(
    plan: &hb_ot_shape_plan_t<'a>,
    buffer: &mut hb_buffer_t<'a>,
    face: &hb_font_t<'a>,
    meter: &Context<'a>,
) -> ProviderResult<()> {
    meter.step()?;
    if buffer.is_empty() {
        return Ok(());
    }

    let mut mode = plan.shaper.normalization_preference;
    if mode == HB_OT_SHAPE_NORMALIZATION_MODE_AUTO {
        if plan.has_gpos_mark {
            // https://github.com/harfbuzz/harfbuzz/issues/653#issuecomment-423905920
            // mode = Some(HB_OT_SHAPE_NORMALIZATION_MODE_DECOMPOSED);
            mode = HB_OT_SHAPE_NORMALIZATION_MODE_COMPOSED_DIACRITICS;
        } else {
            mode = HB_OT_SHAPE_NORMALIZATION_MODE_COMPOSED_DIACRITICS;
        }
    }

    let mut ctx = hb_ot_shape_normalize_context_t {
        plan,
        buffer,
        face,
        meter,
        decompose: decompose_unicode,
        compose: compose_unicode,
    };
    ctx.override_decompose_and_compose(plan.shaper.decompose, plan.shaper.compose);

    let mut buffer = &mut ctx.buffer;

    let always_short_circuit = mode == HB_OT_SHAPE_NORMALIZATION_MODE_NONE;
    let might_short_circuit = always_short_circuit
        || (mode != HB_OT_SHAPE_NORMALIZATION_MODE_DECOMPOSED
            && mode != HB_OT_SHAPE_NORMALIZATION_MODE_COMPOSED_DIACRITICS_NO_SHORT_CIRCUIT);

    // We do a fairly straightforward yet custom normalization process in three
    // separate rounds: decompose, reorder, recompose (if desired).  Currently
    // this makes two buffer swaps.  We can make it faster by moving the last
    // two rounds into the inner loop for the first round, but it's more readable
    // this way.

    // First round, decompose
    let mut all_simple = true;
    {
        meter.step()?;
        buffer.clear_output();
        let count = buffer.len;
        buffer.idx = 0;
        loop {
            meter.step()?;
            let mut end = buffer.idx + 1;
            while end < count && !_hb_glyph_info_is_unicode_mark(&buffer.info[end]) {
                meter.step()?;
                end += 1;
            }

            if end < count {
                // Leave one base for the marks to cluster with.
                end -= 1;
            }

            // From idx to end are simple clusters.
            if might_short_circuit {
                let len = end - buffer.idx;
                let mut done = 0;
                while done < len {
                    meter.step()?;
                    let cur = buffer.cur_mut(done);
                    cur.set_glyph_index(match face.get_nominal_glyph(cur.glyph_id, meter)? {
                        Some(glyph_id) => u32::from(glyph_id.0),
                        None => break,
                    });
                    done += 1;
                }
                buffer.next_glyphs(done, meter)?;
            }

            while buffer.idx < end && buffer.successful {
                meter.step()?;
                decompose_current_character(&mut ctx, might_short_circuit)?;
                buffer = &mut ctx.buffer;
            }

            if buffer.idx == count || !buffer.successful {
                break;
            }

            all_simple = false;

            // Find all the marks now.
            end = buffer.idx + 1;
            while end < count && _hb_glyph_info_is_unicode_mark(&buffer.info[end]) {
                meter.step()?;
                end += 1;
            }

            // idx to end is one non-simple cluster.
            decompose_multi_char_cluster(&mut ctx, end, always_short_circuit)?;
            buffer = &mut ctx.buffer;

            if buffer.idx >= count || !buffer.successful {
                break;
            }
        }

        buffer.sync(meter)?;
    }

    // Second round, reorder (inplace)
    if !all_simple {
        let count = buffer.len;
        let mut i = 0;
        while i < count {
            meter.step()?;
            if _hb_glyph_info_get_modified_combining_class(&buffer.info[i]) == 0 {
                i += 1;
                continue;
            }

            let mut end = i + 1;
            while end < count && _hb_glyph_info_get_modified_combining_class(&buffer.info[end]) != 0
            {
                meter.step()?;
                end += 1;
            }

            // We are going to do a O(n^2).  Only do this if the sequence is short.
            if end - i <= MAX_COMBINING_MARKS {
                buffer.sort(i, end, compare_combining_class, meter)?;

                if let Some(reorder_marks) = ctx.plan.shaper.reorder_marks {
                    reorder_marks(ctx.plan, buffer, i, end, meter)?;
                }
            }

            i = end + 1;
        }
    }
    if buffer.scratch_flags & HB_BUFFER_SCRATCH_FLAG_HAS_CGJ != 0 {
        // For all CGJ, check if it prevented any reordering at all.
        // If it did NOT, then make it skippable.
        // https://github.com/harfbuzz/harfbuzz/issues/554
        for i in 1..buffer.len.saturating_sub(1) {
            meter.step()?;
            if buffer.info[i].glyph_id == 0x034F
            /* CGJ */
            {
                let last = _hb_glyph_info_get_modified_combining_class(&buffer.info[i - 1]);
                let next = _hb_glyph_info_get_modified_combining_class(&buffer.info[i + 1]);
                if next == 0 || last <= next {
                    buffer.info[i].unhide();
                }
            }
        }
    }

    // Third round, recompose
    if !all_simple
        && buffer.successful
        && (mode == HB_OT_SHAPE_NORMALIZATION_MODE_COMPOSED_DIACRITICS
            || mode == HB_OT_SHAPE_NORMALIZATION_MODE_COMPOSED_DIACRITICS_NO_SHORT_CIRCUIT)
    {
        // As noted in the comment earlier, we don't try to combine
        // ccc=0 chars with their previous Starter.

        let count = buffer.len;
        let mut starter = 0;
        buffer.clear_output();
        buffer.next_glyph(meter)?;
        while buffer.idx < count && buffer.successful {
            meter.step()?;
            // We don't try to compose a non-mark character with it's preceding starter.
            // This is both an optimization to avoid trying to compose every two neighboring
            // glyphs in most scripts AND a desired feature for Hangul.  Apparently Hangul
            // fonts are not designed to mix-and-match pre-composed syllables and Jamo.
            let cur = buffer.cur(0);
            if _hb_glyph_info_is_unicode_mark(cur) &&
                // If there's anything between the starter and this char, they should have CCC
                // smaller than this character's.
                (starter == buffer.out_len - 1
                    || _hb_glyph_info_get_modified_combining_class(buffer.prev()) < _hb_glyph_info_get_modified_combining_class(cur))
            {
                meter.step()?;
                let a = buffer.out_info()[starter].as_char();
                let b = cur.as_char();
                if let Some(composed) = (ctx.compose)(&ctx, a, b) {
                    if let Some(glyph_id) = face.get_nominal_glyph(u32::from(composed), meter)? {
                        // Copy to out-buffer.
                        buffer = &mut ctx.buffer;
                        buffer.next_glyph(meter)?;
                        if !buffer.successful {
                            return Ok(());
                        }

                        // Merge and remove the second composable.
                        buffer.merge_out_clusters(starter, buffer.out_len, meter)?;
                        buffer.out_len -= 1;

                        // Modify starter and carry on.
                        let mut flags = buffer.scratch_flags;
                        let info = &mut buffer.out_info_mut()[starter];
                        info.glyph_id = u32::from(composed);
                        info.set_glyph_index(u32::from(glyph_id.0));
                        info.init_unicode_props(&mut flags);
                        buffer.scratch_flags = flags;

                        continue;
                    }
                }
            }

            // Blocked, or doesn't compose.
            buffer = &mut ctx.buffer;
            buffer.next_glyph(meter)?;

            if _hb_glyph_info_get_modified_combining_class(buffer.prev()) == 0 {
                starter = buffer.out_len - 1;
            }
        }

        buffer.sync(meter)?;
    }

    Ok(())
}
