use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::OT::hb_ot_apply_context_t;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::{Apply, WouldApply, WouldApplyContext};
use crate::provider::ttf::gsub::MultipleSubstitution;

// MultipleSubstFormat1::would_apply
impl WouldApply for MultipleSubstitution<'_> {
    fn would_apply(&self, ctx: &WouldApplyContext) -> bool {
        ctx.glyphs.len() == 1 && self.coverage.get(ctx.glyphs[0]).is_some()
    }
}

// MultipleSubstFormat1::apply
impl Apply for MultipleSubstitution<'_> {
    fn apply(
        &self,
        ctx: &mut hb_ot_apply_context_t,
    ) -> crate::provider::ProviderResult<Option<()>> {
        ctx.meter.step()?;
        let glyph = ctx.buffer.cur(0).as_glyph();
        let Some(index) = self.coverage.get_bounded(glyph, ctx.meter)? else {
            return Ok(None);
        };
        let sequence = self
            .sequences
            .get_bounded(index, ctx.meter)?
            .ok_or(crate::Error::CorruptFont)?;
        sequence.apply(ctx)
    }
}
