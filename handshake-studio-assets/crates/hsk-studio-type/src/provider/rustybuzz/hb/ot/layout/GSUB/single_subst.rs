use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::OT::hb_ot_apply_context_t;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::{Apply, WouldApply, WouldApplyContext};
use crate::provider::ttf::GlyphId;
use crate::provider::ttf::gsub::SingleSubstitution;

// SingleSubstFormat1::would_apply
// SingleSubstFormat2::would_apply
impl WouldApply for SingleSubstitution<'_> {
    fn would_apply(&self, ctx: &WouldApplyContext) -> bool {
        ctx.glyphs.len() == 1 && self.coverage().get(ctx.glyphs[0]).is_some()
    }
}

// SingleSubstFormat1::apply
// SingleSubstFormat2::apply
impl Apply for SingleSubstitution<'_> {
    fn apply(
        &self,
        ctx: &mut hb_ot_apply_context_t,
    ) -> crate::provider::ProviderResult<Option<()>> {
        ctx.meter.step()?;
        let glyph = ctx.buffer.cur(0).as_glyph();
        let subst = match *self {
            Self::Format1 { coverage, delta } => {
                if coverage.get_bounded(glyph, ctx.meter)?.is_none() {
                    return Ok(None);
                }
                GlyphId((i32::from(glyph.0) + i32::from(delta)) as u16)
            }
            Self::Format2 {
                coverage,
                substitutes,
            } => {
                let Some(index) = coverage.get_bounded(glyph, ctx.meter)? else {
                    return Ok(None);
                };
                substitutes
                    .get_bounded(index, ctx.meter)?
                    .ok_or(crate::Error::CorruptFont)?
            }
        };
        ctx.replace_glyph(subst)?;
        Ok(Some(()))
    }
}
