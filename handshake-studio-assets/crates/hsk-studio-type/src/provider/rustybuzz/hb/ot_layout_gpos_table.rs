use crate::provider::{Context, ProviderResult};

use super::buffer::*;
use super::hb_font_t;
use super::ot_layout::*;
use super::ot_layout_common::{PositioningLookup, PositioningTable};
use super::ot_layout_gsubgpos::{Apply, OT::hb_ot_apply_context_t};
use super::ot_shape_plan::hb_ot_shape_plan_t;
use crate::provider::rustybuzz::Direction;
use crate::provider::ttf::gpos::*;
use crate::provider::ttf::opentype_layout::LookupIndex;

pub fn position<'a>(
    plan: &hb_ot_shape_plan_t,
    face: &hb_font_t<'a>,
    buffer: &mut hb_buffer_t<'a>,
    meter: &Context<'a>,
) -> ProviderResult<()> {
    apply_layout_table(plan, face, buffer, face.gpos.as_ref(), meter)
}

pub(crate) trait ValueRecordExt {
    fn is_empty(&self) -> bool;
    fn apply(&self, ctx: &mut hb_ot_apply_context_t, idx: usize) -> ProviderResult<bool>;
    fn apply_to_pos(
        &self,
        ctx: &mut hb_ot_apply_context_t,
        pos: &mut GlyphPosition,
    ) -> ProviderResult<bool>;
}

impl ValueRecordExt for ValueRecord<'_> {
    fn is_empty(&self) -> bool {
        self.x_placement == 0
            && self.y_placement == 0
            && self.x_advance == 0
            && self.y_advance == 0
            && self.x_placement_device.is_none()
            && self.y_placement_device.is_none()
            && self.x_advance_device.is_none()
            && self.y_advance_device.is_none()
    }

    fn apply(&self, ctx: &mut hb_ot_apply_context_t, idx: usize) -> ProviderResult<bool> {
        let mut pos = ctx.buffer.pos[idx];
        ctx.meter.step()?;
        let worked = self.apply_to_pos(ctx, &mut pos)?;
        ctx.buffer.pos[idx] = pos;
        Ok(worked)
    }

    fn apply_to_pos(
        &self,
        ctx: &mut hb_ot_apply_context_t,
        pos: &mut GlyphPosition,
    ) -> ProviderResult<bool> {
        ctx.meter.step()?;
        let horizontal = ctx.buffer.direction.is_horizontal();
        let mut worked = false;

        if self.x_placement != 0 {
            pos.x_offset = pos
                .x_offset
                .checked_add(i32::from(self.x_placement))
                .ok_or(crate::Error::Overflow)?;
            worked = true;
        }

        if self.y_placement != 0 {
            pos.y_offset = pos
                .y_offset
                .checked_add(i32::from(self.y_placement))
                .ok_or(crate::Error::Overflow)?;
            worked = true;
        }

        if self.x_advance != 0 && horizontal {
            pos.x_advance = pos
                .x_advance
                .checked_add(i32::from(self.x_advance))
                .ok_or(crate::Error::Overflow)?;
            worked = true;
        }

        if self.y_advance != 0 && !horizontal {
            // y_advance values grow downward but font-space grows upward, hence negation
            pos.y_advance = pos
                .y_advance
                .checked_sub(i32::from(self.y_advance))
                .ok_or(crate::Error::Overflow)?;
            worked = true;
        }

        {
            let (ppem_x, ppem_y) = ctx.face.pixels_per_em().unwrap_or((0, 0));
            let coords = ctx.face.ttfp_face.variation_coordinates().len();
            let use_x_device = ppem_x != 0 || coords != 0;
            let use_y_device = ppem_y != 0 || coords != 0;

            if use_x_device {
                if let Some(device) = self.x_placement_device {
                    pos.x_offset = pos
                        .x_offset
                        .checked_add(device.get_x_delta(ctx.face, ctx.meter)?.unwrap_or(0))
                        .ok_or(crate::Error::Overflow)?;
                    worked = true; // TODO: even when 0?
                }
            }

            if use_y_device {
                if let Some(device) = self.y_placement_device {
                    pos.y_offset = pos
                        .y_offset
                        .checked_add(device.get_y_delta(ctx.face, ctx.meter)?.unwrap_or(0))
                        .ok_or(crate::Error::Overflow)?;
                    worked = true;
                }
            }

            if horizontal && use_x_device {
                if let Some(device) = self.x_advance_device {
                    pos.x_advance = pos
                        .x_advance
                        .checked_add(device.get_x_delta(ctx.face, ctx.meter)?.unwrap_or(0))
                        .ok_or(crate::Error::Overflow)?;
                    worked = true;
                }
            }

            if !horizontal && use_y_device {
                if let Some(device) = self.y_advance_device {
                    // y_advance values grow downward but face-space grows upward, hence negation
                    pos.y_advance = pos
                        .y_advance
                        .checked_sub(device.get_y_delta(ctx.face, ctx.meter)?.unwrap_or(0))
                        .ok_or(crate::Error::Overflow)?;
                    worked = true;
                }
            }
        }

        Ok(worked)
    }
}

pub(crate) trait AnchorExt {
    fn get(&self, face: &hb_font_t, meter: &Context<'_>) -> ProviderResult<(i32, i32)>;
}

impl AnchorExt for Anchor<'_> {
    fn get(&self, face: &hb_font_t, meter: &Context<'_>) -> ProviderResult<(i32, i32)> {
        meter.step()?;
        let mut x = i32::from(self.x);
        let mut y = i32::from(self.y);

        if self.x_device.is_some() || self.y_device.is_some() {
            let (ppem_x, ppem_y) = face.pixels_per_em().unwrap_or((0, 0));
            let coords = face.ttfp_face.variation_coordinates().len();

            if let Some(device) = self.x_device {
                if ppem_x != 0 || coords != 0 {
                    x = x
                        .checked_add(device.get_x_delta(face, meter)?.unwrap_or(0))
                        .ok_or(crate::Error::Overflow)?;
                }
            }

            if let Some(device) = self.y_device {
                if ppem_y != 0 || coords != 0 {
                    y = y
                        .checked_add(device.get_y_delta(face, meter)?.unwrap_or(0))
                        .ok_or(crate::Error::Overflow)?;
                }
            }
        }

        Ok((x, y))
    }
}

impl<'a> LayoutTable for PositioningTable<'a> {
    const INDEX: TableIndex = TableIndex::GPOS;
    const IN_PLACE: bool = true;

    type Lookup = PositioningLookup<'a>;

    fn get_lookup(&self, index: LookupIndex) -> Option<&Self::Lookup> {
        self.lookups.get(usize::from(index))
    }
}

pub mod attach_type {
    pub const MARK: u8 = 1;
    pub const CURSIVE: u8 = 2;
}

/// Just like TryFrom<N>, but for numeric types not supported by the Rust's std.
pub(crate) trait TryNumFrom<T>: Sized {
    /// Casts between numeric types.
    fn try_num_from(_: T) -> Option<Self>;
}

impl TryNumFrom<f32> for i32 {
    #[inline]
    fn try_num_from(v: f32) -> Option<Self> {
        // Based on https://github.com/rust-num/num-traits/blob/master/src/cast.rs

        // Float as int truncates toward zero, so we want to allow values
        // in the exclusive range `(MIN-1, MAX+1)`.

        // We can't represent `MIN-1` exactly, but there's no fractional part
        // at this magnitude, so we can just use a `MIN` inclusive boundary.
        const MIN: f32 = i32::MIN as f32;
        // We can't represent `MAX` exactly, but it will round up to exactly
        // `MAX+1` (a power of two) when we cast it.
        const MAX_P1: f32 = i32::MAX as f32;
        if v >= MIN && v < MAX_P1 {
            Some(v as i32)
        } else {
            None
        }
    }
}

pub(crate) trait DeviceExt {
    fn get_x_delta(&self, face: &hb_font_t, meter: &Context<'_>) -> ProviderResult<Option<i32>>;
    fn get_y_delta(&self, face: &hb_font_t, meter: &Context<'_>) -> ProviderResult<Option<i32>>;
}
impl DeviceExt for Device<'_> {
    fn get_x_delta(&self, face: &hb_font_t, meter: &Context<'_>) -> ProviderResult<Option<i32>> {
        meter.step()?;
        match self {
            Device::Hinting(h) => Ok(h.x_delta(face.units_per_em, face.pixels_per_em())),
            Device::Variation(v) => {
                let table = face.tables().gdef.ok_or(crate::Error::CorruptFont)?;
                let value = table
                    .glyph_variation_delta_bounded(
                        v.outer_index,
                        v.inner_index,
                        face.variation_coordinates(),
                        meter,
                    )?
                    .ok_or(crate::Error::CorruptFont)?;
                Ok(Some(
                    i32::try_num_from(value.round()).ok_or(crate::Error::Overflow)?,
                ))
            }
        }
    }
    fn get_y_delta(&self, face: &hb_font_t, meter: &Context<'_>) -> ProviderResult<Option<i32>> {
        meter.step()?;
        match self {
            Device::Hinting(h) => Ok(h.y_delta(face.units_per_em, face.pixels_per_em())),
            Device::Variation(_) => self.get_x_delta(face, meter),
        }
    }
}

impl Apply for PositioningSubtable<'_> {
    fn apply(&self, ctx: &mut hb_ot_apply_context_t) -> ProviderResult<Option<()>> {
        match self {
            Self::Single(t) => t.apply(ctx),
            Self::Pair(t) => t.apply(ctx),
            Self::Cursive(t) => t.apply(ctx),
            Self::MarkToBase(t) => t.apply(ctx),
            Self::MarkToLigature(t) => t.apply(ctx),
            Self::MarkToMark(t) => t.apply(ctx),
            Self::Context(t) => t.apply(ctx),
            Self::ChainContext(t) => t.apply(ctx),
        }
    }
}

fn propagate_attachment_offsets(
    pos: &mut [GlyphPosition],
    len: usize,
    i: usize,
    direction: Direction,
    meter: &Context<'_>,
) -> ProviderResult<()> {
    meter.step()?;
    let _depth = meter.enter()?;
    if i >= len || len > pos.len() {
        return Err(crate::Error::CorruptFont);
    }
    // Adjusts offsets of attached glyphs (both cursive and mark) to accumulate
    // offset of glyph they are attached to.
    let chain = pos[i].attach_chain();
    let kind = pos[i].attach_type();
    if chain == 0 {
        return Ok(());
    }

    pos[i].set_attach_chain(0);

    let j = usize::try_from(i as i128 + i128::from(chain)).map_err(|_| crate::Error::Overflow)?;
    if j >= len {
        return Err(crate::Error::CorruptFont);
    }

    propagate_attachment_offsets(pos, len, j, direction, meter)?;

    match kind {
        attach_type::MARK => {
            pos[i].x_offset = pos[i]
                .x_offset
                .checked_add(pos[j].x_offset)
                .ok_or(crate::Error::Overflow)?;
            pos[i].y_offset = pos[i]
                .y_offset
                .checked_add(pos[j].y_offset)
                .ok_or(crate::Error::Overflow)?;

            if j >= i {
                return Err(crate::Error::CorruptFont);
            }
            if direction.is_forward() {
                for k in j..i {
                    meter.step()?;
                    pos[i].x_offset = pos[i]
                        .x_offset
                        .checked_sub(pos[k].x_advance)
                        .ok_or(crate::Error::Overflow)?;
                    pos[i].y_offset = pos[i]
                        .y_offset
                        .checked_sub(pos[k].y_advance)
                        .ok_or(crate::Error::Overflow)?;
                }
            } else {
                for k in j + 1..i + 1 {
                    meter.step()?;
                    pos[i].x_offset = pos[i]
                        .x_offset
                        .checked_add(pos[k].x_advance)
                        .ok_or(crate::Error::Overflow)?;
                    pos[i].y_offset = pos[i]
                        .y_offset
                        .checked_add(pos[k].y_advance)
                        .ok_or(crate::Error::Overflow)?;
                }
            }
        }
        attach_type::CURSIVE => {
            if direction.is_horizontal() {
                pos[i].y_offset = pos[i]
                    .y_offset
                    .checked_add(pos[j].y_offset)
                    .ok_or(crate::Error::Overflow)?;
            } else {
                pos[i].x_offset = pos[i]
                    .x_offset
                    .checked_add(pos[j].x_offset)
                    .ok_or(crate::Error::Overflow)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub mod GPOS {
    use super::*;

    pub fn position_start(
        _: &hb_font_t,
        buffer: &mut hb_buffer_t,
        meter: &Context<'_>,
    ) -> ProviderResult<()> {
        meter.step()?;
        let len = buffer.len;
        for pos in &mut buffer.pos[..len] {
            meter.step()?;
            pos.set_attach_chain(0);
            pos.set_attach_type(0);
        }
        Ok(())
    }

    pub fn position_finish_advances(
        _: &hb_font_t,
        _: &mut hb_buffer_t,
        meter: &Context<'_>,
    ) -> ProviderResult<()> {
        meter.step()
    }

    pub fn position_finish_offsets(
        _: &hb_font_t,
        buffer: &mut hb_buffer_t,
        meter: &Context<'_>,
    ) -> ProviderResult<()> {
        meter.step()?;
        let len = buffer.len;
        let direction = buffer.direction;

        // Handle attachments
        if buffer.scratch_flags & HB_BUFFER_SCRATCH_FLAG_HAS_GPOS_ATTACHMENT != 0 {
            for i in 0..len {
                meter.step()?;
                propagate_attachment_offsets(&mut buffer.pos, len, i, direction, meter)?;
            }
        }
        Ok(())
    }
}
