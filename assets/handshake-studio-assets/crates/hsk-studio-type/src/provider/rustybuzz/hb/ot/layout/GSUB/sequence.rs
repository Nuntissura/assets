use crate::provider::rustybuzz::hb::buffer::GlyphPropsFlags;
use crate::provider::rustybuzz::hb::ot_layout::{
    _hb_glyph_info_get_lig_id, _hb_glyph_info_is_ligature,
    _hb_glyph_info_set_lig_props_for_component,
};
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::Apply;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::OT::hb_ot_apply_context_t;
use crate::provider::ttf::gsub::Sequence;

impl Apply for Sequence<'_> {
    fn apply(
        &self,
        ctx: &mut hb_ot_apply_context_t,
    ) -> crate::provider::ProviderResult<Option<()>> {
        ctx.meter.step()?;
        let count = usize::from(self.substitutes.len());
        ctx.meter.check_glyphs(
            ctx.buffer
                .len
                .checked_add(count)
                .ok_or(crate::Error::Overflow)? as u64,
        )?;
        match count {
            0 => ctx.buffer.delete_glyph(ctx.meter)?,
            1 => ctx.replace_glyph(
                self.substitutes
                    .get_bounded(0, ctx.meter)?
                    .ok_or(crate::Error::CorruptFont)?,
            )?,
            _ => {
                let class = if _hb_glyph_info_is_ligature(ctx.buffer.cur(0)) {
                    GlyphPropsFlags::BASE_GLYPH
                } else {
                    GlyphPropsFlags::empty()
                };
                let lig_id = _hb_glyph_info_get_lig_id(ctx.buffer.cur(0));
                for i in 0..self.substitutes.len() {
                    ctx.meter.step()?;
                    let subst = self
                        .substitutes
                        .get_bounded(i, ctx.meter)?
                        .ok_or(crate::Error::CorruptFont)?;
                    if lig_id == 0 {
                        _hb_glyph_info_set_lig_props_for_component(ctx.buffer.cur_mut(0), i as u8);
                    }
                    ctx.output_glyph_for_component(subst, class)?;
                }
                ctx.buffer.skip_glyph();
            }
        }
        Ok(Some(()))
    }
}
