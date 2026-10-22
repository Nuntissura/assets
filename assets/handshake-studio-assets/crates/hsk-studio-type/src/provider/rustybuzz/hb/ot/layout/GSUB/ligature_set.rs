use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::OT::hb_ot_apply_context_t;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::{Apply, WouldApply, WouldApplyContext};
use crate::provider::ttf::gsub::LigatureSet;

impl WouldApply for LigatureSet<'_> {
    fn would_apply(&self, ctx: &WouldApplyContext) -> bool {
        self.into_iter().any(|lig| lig.would_apply(ctx))
    }
}

impl Apply for LigatureSet<'_> {
    fn apply(
        &self,
        ctx: &mut hb_ot_apply_context_t,
    ) -> crate::provider::ProviderResult<Option<()>> {
        for i in 0..self.len() {
            ctx.meter.step()?;
            let lig = self
                .get_bounded(i, ctx.meter)?
                .ok_or(crate::Error::CorruptFont)?;
            if lig.apply(ctx)?.is_some() {
                return Ok(Some(()));
            }
        }
        Ok(None)
    }
}
