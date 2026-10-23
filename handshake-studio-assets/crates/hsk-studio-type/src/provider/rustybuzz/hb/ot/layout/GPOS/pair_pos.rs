use crate::provider::rustybuzz::hb::ot_layout_gpos_table::ValueRecordExt;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::OT::hb_ot_apply_context_t;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::{Apply, skipping_iterator_t};
use crate::provider::ttf::gpos::PairAdjustment;

impl Apply for PairAdjustment<'_> {
    fn apply(
        &self,
        ctx: &mut hb_ot_apply_context_t,
    ) -> crate::provider::ProviderResult<Option<()>> {
        ctx.meter.step()?;
        let first = ctx.buffer.cur(0).as_glyph();
        let index = lookup_match!(self.coverage().get_bounded(first, ctx.meter)?);
        let mut iter = skipping_iterator_t::new(ctx, ctx.buffer.idx, false);
        let mut unsafe_to = 0;
        if !iter.next(Some(&mut unsafe_to))? {
            ctx.buffer
                .unsafe_to_concat(Some(ctx.buffer.idx), Some(unsafe_to), ctx.meter)?;
            return Ok(None);
        }
        let second_index = iter.index();
        let second = ctx.buffer.info[second_index].as_glyph();
        let records = match self {
            Self::Format1 { sets, .. } => {
                let set = sets
                    .get_bounded(index, ctx.meter)?
                    .ok_or(crate::Error::CorruptFont)?;
                lookup_match!(set.get_bounded(second, ctx.meter)?)
            }
            Self::Format2 {
                classes, matrix, ..
            } => {
                let pair = (
                    classes.0.get_bounded(first, ctx.meter)?,
                    classes.1.get_bounded(second, ctx.meter)?,
                );
                let Some(records) = matrix.get_bounded(pair, ctx.meter)? else {
                    ctx.buffer.unsafe_to_concat(
                        Some(ctx.buffer.idx),
                        Some(second_index.checked_add(1).ok_or(crate::Error::Overflow)?),
                        ctx.meter,
                    )?;
                    return Ok(None);
                };
                records
            }
        };
        let has_record1 = !records.0.is_empty();
        let has_record2 = !records.1.is_empty();
        let flag1 = has_record1 && records.0.apply(ctx, ctx.buffer.idx)?;
        let flag2 = has_record2 && records.1.apply(ctx, second_index)?;
        let mut next = second_index;
        if flag1 || flag2 {
            ctx.buffer.unsafe_to_break(
                Some(ctx.buffer.idx),
                Some(second_index.checked_add(1).ok_or(crate::Error::Overflow)?),
                ctx.meter,
            )?;
        } else {
            ctx.buffer.unsafe_to_concat(
                Some(ctx.buffer.idx),
                Some(second_index.checked_add(1).ok_or(crate::Error::Overflow)?),
                ctx.meter,
            )?;
        }
        if has_record2 {
            next = next.checked_add(1).ok_or(crate::Error::Overflow)?;
            ctx.buffer.unsafe_to_break(
                Some(ctx.buffer.idx),
                Some(next.checked_add(1).ok_or(crate::Error::Overflow)?),
                ctx.meter,
            )?;
        }
        ctx.buffer.idx = next;
        Ok(Some(()))
    }
}
