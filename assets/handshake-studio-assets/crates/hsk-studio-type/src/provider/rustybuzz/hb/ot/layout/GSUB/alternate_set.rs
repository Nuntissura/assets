use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::Apply;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::OT::hb_ot_apply_context_t;
use crate::provider::rustybuzz::hb::ot_map::hb_ot_map_t;
use crate::provider::ttf::gsub::AlternateSet;
use core::convert::TryFrom;

impl Apply for AlternateSet<'_> {
    fn apply(
        &self,
        ctx: &mut hb_ot_apply_context_t,
    ) -> crate::provider::ProviderResult<Option<()>> {
        ctx.meter.step()?;
        let len = self.alternates.len();
        if len == 0 {
            return Ok(None);
        }

        let glyph_mask = ctx.buffer.cur(0).mask;

        // Note: This breaks badly if two features enabled this lookup together.
        let shift = ctx.lookup_mask().trailing_zeros();
        let mut alt_index = (ctx.lookup_mask() & glyph_mask) >> shift;

        // If alt_index is MAX_VALUE, randomize feature if it is the rand feature.
        if alt_index == hb_ot_map_t::MAX_VALUE && ctx.random {
            // Maybe we can do better than unsafe-to-break all; but since we are
            // changing random state, it would be hard to track that.  Good 'nough.
            ctx.buffer
                .unsafe_to_break(Some(0), Some(ctx.buffer.len), ctx.meter)?;
            alt_index = ctx.random_number() % u32::from(len) + 1;
        }

        let Ok(index) = u16::try_from(alt_index) else {
            return Ok(None);
        };
        let Some(idx) = index.checked_sub(1) else {
            return Ok(None);
        };
        let Some(glyph) = self.alternates.get_bounded(idx, ctx.meter)? else {
            return Ok(None);
        };
        ctx.replace_glyph(glyph)?;

        Ok(Some(()))
    }
}
