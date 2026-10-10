use crate::provider::rustybuzz::hb::ot::layout::GPOS::mark_array::MarkArrayExt;
use crate::provider::rustybuzz::hb::ot_layout::{
    _hb_glyph_info_get_lig_comp, _hb_glyph_info_get_lig_id,
};
use crate::provider::rustybuzz::hb::ot_layout_common::lookup_flags;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::OT::hb_ot_apply_context_t;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::{Apply, match_t, skipping_iterator_t};
use crate::provider::ttf::gpos::MarkToLigatureAdjustment;

impl Apply for MarkToLigatureAdjustment<'_> {
    fn apply(
        &self,
        ctx: &mut hb_ot_apply_context_t,
    ) -> crate::provider::ProviderResult<Option<()>> {
        ctx.meter.step()?;
        let buffer = &ctx.buffer;
        let mark_glyph = ctx.buffer.cur(0).as_glyph();
        let mark_index = lookup_match!(self.mark_coverage.get_bounded(mark_glyph, ctx.meter)?);

        // Due to borrowing rules, we have this piece of code before creating the
        // iterator, unlike in harfbuzz.
        if ctx.last_base_until > buffer.idx as u32 {
            ctx.last_base_until = 0;
            ctx.last_base = -1;
        }

        // Now we search backwards for a non-mark glyph
        let mut iter = skipping_iterator_t::new(ctx, 0, false);
        iter.set_lookup_props(u32::from(lookup_flags::IGNORE_MARKS));

        let mut j = buffer.idx;
        while j > ctx.last_base_until as usize {
            ctx.meter.step()?;
            let mut _match = iter.match_(&buffer.info[j - 1])?;
            if _match == match_t::MATCH {
                ctx.last_base = j as i32 - 1;
                break;
            }
            j -= 1;
        }

        ctx.last_base_until = buffer.idx as u32;

        if ctx.last_base == -1 {
            ctx.buffer
                .unsafe_to_concat_from_outbuffer(Some(0), Some(buffer.idx + 1), ctx.meter)?;
            return Ok(None);
        }

        let idx = ctx.last_base as usize;

        // Checking that matched glyph is actually a ligature by GDEF is too strong; disabled

        let lig_glyph = buffer.info[idx].as_glyph();
        let Some(lig_index) = self.ligature_coverage.get_bounded(lig_glyph, ctx.meter)? else {
            ctx.buffer.unsafe_to_concat_from_outbuffer(
                Some(idx),
                Some(buffer.idx + 1),
                ctx.meter,
            )?;
            return Ok(None);
        };
        let lig_attach = lookup_match!(self.ligature_array.get_bounded(lig_index, ctx.meter)?);

        // Find component to attach to
        let comp_count = lig_attach.rows;
        if comp_count == 0 {
            ctx.buffer.unsafe_to_concat_from_outbuffer(
                Some(idx),
                Some(buffer.idx + 1),
                ctx.meter,
            )?;
            return Ok(None);
        }

        // We must now check whether the ligature ID of the current mark glyph
        // is identical to the ligature ID of the found ligature.  If yes, we
        // can directly use the component index.  If not, we attach the mark
        // glyph to the last component of the ligature.
        let lig_id = _hb_glyph_info_get_lig_id(&buffer.info[idx]);
        let mark_id = _hb_glyph_info_get_lig_id(buffer.cur(0));
        let mark_comp = u16::from(_hb_glyph_info_get_lig_comp(buffer.cur(0)));
        let matches = lig_id != 0 && lig_id == mark_id && mark_comp > 0;
        let comp_index = if matches {
            mark_comp.min(comp_count)
        } else {
            comp_count
        } - 1;

        self.marks
            .apply(ctx, lig_attach, mark_index, comp_index, idx)
    }
}
