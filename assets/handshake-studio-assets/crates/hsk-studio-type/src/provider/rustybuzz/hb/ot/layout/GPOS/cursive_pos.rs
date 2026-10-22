use crate::provider::rustybuzz::hb::buffer::HB_BUFFER_SCRATCH_FLAG_HAS_GPOS_ATTACHMENT;
use crate::provider::rustybuzz::hb::ot_layout_common::lookup_flags;
use crate::provider::rustybuzz::hb::ot_layout_gpos_table::AnchorExt;
use crate::provider::rustybuzz::hb::ot_layout_gpos_table::attach_type;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::OT::hb_ot_apply_context_t;
use crate::provider::rustybuzz::hb::ot_layout_gsubgpos::{Apply, skipping_iterator_t};
use crate::provider::rustybuzz::{Direction, GlyphPosition};
use crate::provider::ttf::gpos::CursiveAdjustment;

impl Apply for CursiveAdjustment<'_> {
    fn apply(
        &self,
        ctx: &mut hb_ot_apply_context_t,
    ) -> crate::provider::ProviderResult<Option<()>> {
        ctx.meter.step()?;
        let this = ctx.buffer.cur(0).as_glyph();

        let index_this = lookup_match!(self.coverage.get_bounded(this, ctx.meter)?);
        let entry_this = lookup_match!(self.sets.entry_bounded(index_this, ctx.meter)?);

        let mut iter = skipping_iterator_t::new(ctx, ctx.buffer.idx, false);

        let mut unsafe_from = 0;
        if !iter.prev(Some(&mut unsafe_from))? {
            ctx.buffer.unsafe_to_concat_from_outbuffer(
                Some(unsafe_from),
                Some(ctx.buffer.idx + 1),
                ctx.meter,
            )?;
            return Ok(None);
        }

        let i = iter.index();
        let prev = ctx.buffer.info[i].as_glyph();
        let index_prev = lookup_match!(self.coverage.get_bounded(prev, ctx.meter)?);
        let Some(exit_prev) = self.sets.exit_bounded(index_prev, ctx.meter)? else {
            ctx.buffer.unsafe_to_concat_from_outbuffer(
                Some(iter.index()),
                Some(ctx.buffer.idx + 1),
                ctx.meter,
            )?;
            return Ok(None);
        };

        let (exit_x, exit_y) = exit_prev.get(ctx.face, ctx.meter)?;
        let (entry_x, entry_y) = entry_this.get(ctx.face, ctx.meter)?;

        let direction = ctx.buffer.direction;
        let j = ctx.buffer.idx;
        ctx.buffer
            .unsafe_to_break(Some(i), Some(j + 1), ctx.meter)?;

        let pos = &mut ctx.buffer.pos;
        match direction {
            Direction::LeftToRight => {
                pos[i].x_advance = exit_x
                    .checked_add(pos[i].x_offset)
                    .ok_or(crate::Error::Overflow)?;
                let d = entry_x
                    .checked_add(pos[j].x_offset)
                    .ok_or(crate::Error::Overflow)?;
                pos[j].x_advance = pos[j]
                    .x_advance
                    .checked_sub(d)
                    .ok_or(crate::Error::Overflow)?;
                pos[j].x_offset = pos[j]
                    .x_offset
                    .checked_sub(d)
                    .ok_or(crate::Error::Overflow)?;
            }
            Direction::RightToLeft => {
                let d = exit_x
                    .checked_add(pos[i].x_offset)
                    .ok_or(crate::Error::Overflow)?;
                pos[i].x_advance = pos[i]
                    .x_advance
                    .checked_sub(d)
                    .ok_or(crate::Error::Overflow)?;
                pos[i].x_offset = pos[i]
                    .x_offset
                    .checked_sub(d)
                    .ok_or(crate::Error::Overflow)?;
                pos[j].x_advance = entry_x
                    .checked_add(pos[j].x_offset)
                    .ok_or(crate::Error::Overflow)?;
            }
            Direction::TopToBottom => {
                pos[i].y_advance = exit_y
                    .checked_add(pos[i].y_offset)
                    .ok_or(crate::Error::Overflow)?;
                let d = entry_y
                    .checked_add(pos[j].y_offset)
                    .ok_or(crate::Error::Overflow)?;
                pos[j].y_advance = pos[j]
                    .y_advance
                    .checked_sub(d)
                    .ok_or(crate::Error::Overflow)?;
                pos[j].y_offset = pos[j]
                    .y_offset
                    .checked_sub(d)
                    .ok_or(crate::Error::Overflow)?;
            }
            Direction::BottomToTop => {
                let d = exit_y
                    .checked_add(pos[i].y_offset)
                    .ok_or(crate::Error::Overflow)?;
                pos[i].y_advance = pos[i]
                    .y_advance
                    .checked_sub(d)
                    .ok_or(crate::Error::Overflow)?;
                pos[i].y_offset = pos[i]
                    .y_offset
                    .checked_sub(d)
                    .ok_or(crate::Error::Overflow)?;
                pos[j].y_advance = entry_y;
            }
            Direction::Invalid => {}
        }

        // Cross-direction adjustment

        // We attach child to parent (think graph theory and rooted trees whereas
        // the root stays on baseline and each node aligns itself against its
        // parent.
        //
        // Optimize things for the case of RightToLeft, as that's most common in
        // Arabic.
        let mut child = i;
        let mut parent = j;
        let mut x_offset = entry_x.checked_sub(exit_x).ok_or(crate::Error::Overflow)?;
        let mut y_offset = entry_y.checked_sub(exit_y).ok_or(crate::Error::Overflow)?;

        // Low bits are lookup flags, so we want to truncate.
        if ctx.lookup_props as u16 & lookup_flags::RIGHT_TO_LEFT == 0 {
            core::mem::swap(&mut child, &mut parent);
            x_offset = x_offset.checked_neg().ok_or(crate::Error::Overflow)?;
            y_offset = y_offset.checked_neg().ok_or(crate::Error::Overflow)?;
        }

        // If child was already connected to someone else, walk through its old
        // chain and reverse the link direction, such that the whole tree of its
        // previous connection now attaches to new parent.  Watch out for case
        // where new parent is on the path from old chain...
        reverse_cursive_minor_offset(pos, child, direction, parent, ctx.meter)?;

        pos[child].set_attach_type(attach_type::CURSIVE);
        pos[child].set_attach_chain(
            i16::try_from(parent as i128 - child as i128).map_err(|_| crate::Error::Overflow)?,
        );

        ctx.buffer.scratch_flags |= HB_BUFFER_SCRATCH_FLAG_HAS_GPOS_ATTACHMENT;
        if direction.is_horizontal() {
            pos[child].y_offset = y_offset;
        } else {
            pos[child].x_offset = x_offset;
        }

        // If parent was attached to child, separate them.
        // https://github.com/harfbuzz/harfbuzz/issues/2469
        if pos[parent].attach_chain() == -pos[child].attach_chain() {
            pos[parent].set_attach_chain(0);

            if direction.is_horizontal() {
                pos[parent].y_offset = 0;
            } else {
                pos[parent].x_offset = 0;
            }
        }

        ctx.buffer.idx = ctx
            .buffer
            .idx
            .checked_add(1)
            .ok_or(crate::Error::Overflow)?;
        Ok(Some(()))
    }
}

fn reverse_cursive_minor_offset(
    pos: &mut [GlyphPosition],
    i: usize,
    direction: Direction,
    new_parent: usize,
    meter: &crate::provider::Context<'_>,
) -> crate::provider::ProviderResult<()> {
    meter.step()?;
    let _depth = meter.enter()?;
    if i >= pos.len() {
        return Err(crate::Error::CorruptFont);
    }
    let chain = pos[i].attach_chain();
    let attach_type = pos[i].attach_type();
    if chain == 0 || attach_type & attach_type::CURSIVE == 0 {
        return Ok(());
    }

    pos[i].set_attach_chain(0);

    // Stop if we see new parent in the chain.
    let j = usize::try_from(i as i128 + i128::from(chain)).map_err(|_| crate::Error::Overflow)?;
    if j >= pos.len() {
        return Err(crate::Error::CorruptFont);
    }
    if j == new_parent {
        return Ok(());
    }

    reverse_cursive_minor_offset(pos, j, direction, new_parent, meter)?;

    if direction.is_horizontal() {
        pos[j].y_offset = pos[i]
            .y_offset
            .checked_neg()
            .ok_or(crate::Error::Overflow)?;
    } else {
        pos[j].x_offset = pos[i]
            .x_offset
            .checked_neg()
            .ok_or(crate::Error::Overflow)?;
    }

    pos[j].set_attach_chain(chain.checked_neg().ok_or(crate::Error::Overflow)?);
    pos[j].set_attach_type(attach_type);
    Ok(())
}
