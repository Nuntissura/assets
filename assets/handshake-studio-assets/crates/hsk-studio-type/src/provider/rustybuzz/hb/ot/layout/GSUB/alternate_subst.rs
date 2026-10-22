use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::OT::hb_ot_apply_context_t;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::{Apply, WouldApply, WouldApplyContext};
use crate::provider::ttf::gsub::AlternateSubstitution;

// AlternateSubstFormat1::would_apply
impl WouldApply for AlternateSubstitution<'_> {
    fn would_apply(&self, ctx: &WouldApplyContext) -> bool {
        ctx.glyphs.len() == 1 && self.coverage.get(ctx.glyphs[0]).is_some()
    }
}

// AlternateSubstFormat1::apply
impl Apply for AlternateSubstitution<'_> {
    fn apply(
        &self,
        ctx: &mut hb_ot_apply_context_t,
    ) -> crate::provider::ProviderResult<Option<()>> {
        ctx.meter.step()?;
        let Some(index) = self
            .coverage
            .get_bounded(ctx.buffer.cur(0).as_glyph(), ctx.meter)?
        else {
            return Ok(None);
        };
        self.alternate_sets
            .get_bounded(index, ctx.meter)?
            .ok_or(crate::Error::CorruptFont)?
            .apply(ctx)
    }
}
