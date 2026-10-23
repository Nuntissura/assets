use crate::provider::rustybuzz::hb::ot_layout_gpos_table::ValueRecordExt;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::Apply;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::OT::hb_ot_apply_context_t;
use crate::provider::ttf::gpos::SingleAdjustment;

impl Apply for SingleAdjustment<'_> {
    fn apply(
        &self,
        ctx: &mut hb_ot_apply_context_t,
    ) -> crate::provider::ProviderResult<Option<()>> {
        ctx.meter.step()?;
        let glyph = ctx.buffer.cur(0).as_glyph();
        let record = match self {
            Self::Format1 { coverage, value } => {
                if coverage.get_bounded(glyph, ctx.meter)?.is_none() {
                    return Ok(None);
                }
                *value
            }
            Self::Format2 { coverage, values } => {
                let Some(index) = coverage.get_bounded(glyph, ctx.meter)? else {
                    return Ok(None);
                };
                values
                    .get_bounded(index, ctx.meter)?
                    .ok_or(crate::Error::CorruptFont)?
            }
        };
        record.apply(ctx, ctx.buffer.idx)?;
        ctx.buffer.idx = ctx
            .buffer
            .idx
            .checked_add(1)
            .ok_or(crate::Error::Overflow)?;
        Ok(Some(()))
    }
}
