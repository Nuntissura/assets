use crate::provider::rustybuzz::hb::ot_layout::LayoutLookup;
use crate::provider::rustybuzz::hb::ot_layout_common::SubstLookup;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::OT::hb_ot_apply_context_t;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::{Apply, WouldApply, WouldApplyContext};
use crate::provider::rustybuzz::hb::set_digest::{hb_set_digest_ext, hb_set_digest_t};

impl LayoutLookup for SubstLookup<'_> {
    fn props(&self) -> u32 {
        self.props
    }

    fn is_reverse(&self) -> bool {
        self.reverse
    }

    fn digest(&self) -> &hb_set_digest_t {
        &self.set_digest
    }
}

impl WouldApply for SubstLookup<'_> {
    fn would_apply(&self, ctx: &WouldApplyContext) -> bool {
        self.digest().may_have_glyph(ctx.glyphs[0])
            && self
                .subtables
                .iter()
                .any(|subtable| subtable.would_apply(ctx))
    }
}

impl Apply for SubstLookup<'_> {
    fn apply(
        &self,
        ctx: &mut hb_ot_apply_context_t,
    ) -> crate::provider::ProviderResult<Option<()>> {
        ctx.meter.step()?;
        if self.digest().may_have_glyph(ctx.buffer.cur(0).as_glyph()) {
            for subtable in self.subtables.iter() {
                ctx.meter.step()?;
                if subtable.apply(ctx)?.is_some() {
                    return Ok(Some(()));
                }
            }
        }

        Ok(None)
    }
}
