use crate::provider::rustybuzz::hb::buffer::HB_BUFFER_SCRATCH_FLAG_HAS_GPOS_ATTACHMENT;
use crate::provider::rustybuzz::hb::ot_layout_gpos_table::{AnchorExt, attach_type};
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::OT::hb_ot_apply_context_t;
use crate::provider::ttf::gpos::{AnchorMatrix, MarkArray};

pub(crate) trait MarkArrayExt {
    fn apply(
        &self,
        ctx: &mut hb_ot_apply_context_t,
        anchors: AnchorMatrix,
        mark_index: u16,
        glyph_index: u16,
        glyph_pos: usize,
    ) -> crate::provider::ProviderResult<Option<()>>;
}

impl MarkArrayExt for MarkArray<'_> {
    fn apply(
        &self,
        ctx: &mut hb_ot_apply_context_t,
        anchors: AnchorMatrix,
        mark_index: u16,
        glyph_index: u16,
        glyph_pos: usize,
    ) -> crate::provider::ProviderResult<Option<()>> {
        // If this subtable doesn't have an anchor for this base and this class
        // return `None` such that the subsequent subtables have a chance at it.
        ctx.meter.step()?;
        let (mark_class, mark_anchor) = lookup_match!(self.get_bounded(mark_index, ctx.meter)?);
        let base_anchor = lookup_match!(anchors.get_bounded(glyph_index, mark_class, ctx.meter)?);

        let (mark_x, mark_y) = mark_anchor.get(ctx.face, ctx.meter)?;
        let (base_x, base_y) = base_anchor.get(ctx.face, ctx.meter)?;

        ctx.buffer
            .unsafe_to_break(Some(glyph_pos), Some(ctx.buffer.idx + 1), ctx.meter)?;

        let idx = ctx.buffer.idx;
        let pos = ctx.buffer.cur_pos_mut();
        pos.x_offset = base_x.checked_sub(mark_x).ok_or(crate::Error::Overflow)?;
        pos.y_offset = base_y.checked_sub(mark_y).ok_or(crate::Error::Overflow)?;
        pos.set_attach_type(attach_type::MARK);
        pos.set_attach_chain(
            i16::try_from(glyph_pos as i128 - idx as i128).map_err(|_| crate::Error::Overflow)?,
        );

        ctx.buffer.scratch_flags |= HB_BUFFER_SCRATCH_FLAG_HAS_GPOS_ATTACHMENT;
        ctx.buffer.idx = ctx
            .buffer
            .idx
            .checked_add(1)
            .ok_or(crate::Error::Overflow)?;

        Ok(Some(()))
    }
}
