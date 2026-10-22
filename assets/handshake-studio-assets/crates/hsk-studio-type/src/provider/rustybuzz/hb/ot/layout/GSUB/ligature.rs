use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::OT::hb_ot_apply_context_t;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::{
    Apply, WouldApply, WouldApplyContext, ligate_input, match_glyph, match_input,
};
use crate::provider::ttf::gsub::Ligature;

impl WouldApply for Ligature<'_> {
    fn would_apply(&self, ctx: &WouldApplyContext) -> bool {
        ctx.glyphs.len() == usize::from(self.components.len()) + 1
            && self
                .components
                .into_iter()
                .enumerate()
                .all(|(i, comp)| ctx.glyphs[i + 1] == comp)
    }
}

impl Apply for Ligature<'_> {
    fn apply(
        &self,
        ctx: &mut hb_ot_apply_context_t,
    ) -> crate::provider::ProviderResult<Option<()>> {
        ctx.meter.step()?;
        if self.components.is_empty() {
            ctx.replace_glyph(self.glyph)?;
            return Ok(Some(()));
        }
        let meter = ctx.meter;
        let matcher = |glyph, index| {
            let value = self
                .components
                .get_bounded(index, meter)?
                .ok_or(crate::Error::CorruptFont)?;
            Ok(match_glyph(glyph, value.0))
        };
        let mut end = 0;
        let mut positions = [0; crate::provider::rustybuzz::hb::ot_layout::MAX_CONTEXT_LENGTH];
        let mut components = 0;
        if !match_input(
            ctx,
            self.components.len(),
            &matcher,
            &mut end,
            &mut positions,
            Some(&mut components),
        )? {
            ctx.buffer
                .unsafe_to_concat(Some(ctx.buffer.idx), Some(end), ctx.meter)?;
            return Ok(None);
        }
        ligate_input(
            ctx,
            usize::from(self.components.len()) + 1,
            &positions,
            end,
            components,
            self.glyph,
        )?;
        Ok(Some(()))
    }
}
