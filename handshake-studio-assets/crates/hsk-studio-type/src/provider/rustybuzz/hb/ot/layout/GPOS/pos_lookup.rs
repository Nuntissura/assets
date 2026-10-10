use crate::provider::rustybuzz::hb::ot_layout::LayoutLookup;
use crate::provider::rustybuzz::hb::ot_layout_common::PositioningLookup;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::Apply;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::OT::hb_ot_apply_context_t;
use crate::provider::rustybuzz::hb::set_digest::{hb_set_digest_ext, hb_set_digest_t};

impl LayoutLookup for PositioningLookup<'_> {
    fn props(&self) -> u32 {
        self.props
    }

    fn is_reverse(&self) -> bool {
        false
    }

    fn digest(&self) -> &hb_set_digest_t {
        &self.set_digest
    }
}

impl Apply for PositioningLookup<'_> {
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
