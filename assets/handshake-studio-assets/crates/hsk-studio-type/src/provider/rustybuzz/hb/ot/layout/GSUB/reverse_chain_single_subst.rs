use crate::provider::rustybuzz::hb::ot_layout::MAX_NESTING_LEVEL;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::OT::hb_ot_apply_context_t;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::{
    Apply, WouldApply, WouldApplyContext, match_backtrack, match_lookahead,
};
use crate::provider::ttf::gsub::ReverseChainSingleSubstitution;

// ReverseChainSingleSubstFormat1::would_apply
impl WouldApply for ReverseChainSingleSubstitution<'_> {
    fn would_apply(&self, ctx: &WouldApplyContext) -> bool {
        ctx.glyphs.len() == 1 && self.coverage.get(ctx.glyphs[0]).is_some()
    }
}

// ReverseChainSingleSubstFormat1::apply
impl Apply for ReverseChainSingleSubstitution<'_> {
    fn apply(
        &self,
        ctx: &mut hb_ot_apply_context_t,
    ) -> crate::provider::ProviderResult<Option<()>> {
        ctx.meter.step()?;
        let index = lookup_match!(
            self.coverage
                .get_bounded(ctx.buffer.cur(0).as_glyph(), ctx.meter)?
        );
        if index >= self.substitutes.len() || ctx.nesting_level_left != MAX_NESTING_LEVEL {
            return Ok(None);
        }
        let subst = self
            .substitutes
            .get_bounded(index, ctx.meter)?
            .ok_or(crate::Error::CorruptFont)?;
        let meter = ctx.meter;
        let back = |glyph, index| {
            self.backtrack_coverages
                .get_bounded(index, meter)?
                .ok_or(crate::Error::CorruptFont)?
                .contains_bounded(glyph, meter)
        };
        let ahead = |glyph, index| {
            self.lookahead_coverages
                .get_bounded(index, meter)?
                .ok_or(crate::Error::CorruptFont)?
                .contains_bounded(glyph, meter)
        };
        let mut start = 0;
        let mut end = 0;
        if match_backtrack(ctx, self.backtrack_coverages.len(), &back, &mut start)?
            && match_lookahead(
                ctx,
                self.lookahead_coverages.len(),
                &ahead,
                ctx.buffer
                    .idx
                    .checked_add(1)
                    .ok_or(crate::Error::Overflow)?,
                &mut end,
            )?
        {
            ctx.buffer
                .unsafe_to_break_from_outbuffer(Some(start), Some(end), ctx.meter)?;
            ctx.replace_glyph_inplace(subst)?;
            return Ok(Some(()));
        }
        ctx.buffer
            .unsafe_to_concat_from_outbuffer(Some(start), Some(end), ctx.meter)?;
        Ok(None)
    }
}
