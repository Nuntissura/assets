use super::hb_font_t;
use super::ot_shape::{hb_ot_shape_context_t, shape_internal};
use super::ot_shape_plan::hb_ot_shape_plan_t;
use crate::Error;
use crate::provider::rustybuzz::{
    BufferFlags, Direction, Feature, GlyphBuffer, UnicodeBuffer, script,
};
use crate::provider::{Context, ProviderResult};

/// The selected native entry requires effective properties; it never guesses locale/script.
pub fn shape<'a>(
    face: &hb_font_t<'a>,
    features: &[Feature],
    buffer: UnicodeBuffer<'a>,
    meter: &Context<'a>,
) -> ProviderResult<GlyphBuffer<'a>> {
    meter.step()?;
    if !matches!(
        buffer.0.script,
        Some(script::LATIN | script::ARABIC | script::HAN)
    ) || !matches!(
        buffer.0.direction,
        Direction::LeftToRight | Direction::RightToLeft
    ) || buffer.0.language.is_none()
    {
        return Err(Error::UnsupportedProfile);
    }
    let flags = BufferFlags::BEGINNING_OF_TEXT
        | BufferFlags::END_OF_TEXT
        | BufferFlags::REMOVE_DEFAULT_IGNORABLES;
    if buffer.0.flags != flags {
        return Err(Error::UnsupportedFeature);
    }
    for _ in features {
        meter.step()?;
    }
    let plan = hb_ot_shape_plan_t::new(
        face,
        buffer.0.direction,
        buffer.0.script,
        buffer.0.language.as_ref(),
        features,
        meter,
    )?;
    shape_with_plan(face, &plan, buffer, meter)
}

pub fn shape_with_plan<'a>(
    face: &hb_font_t<'a>,
    plan: &hb_ot_shape_plan_t<'a>,
    buffer: UnicodeBuffer<'a>,
    meter: &Context<'a>,
) -> ProviderResult<GlyphBuffer<'a>> {
    meter.step()?;
    if buffer.0.direction != plan.direction || buffer.0.script != plan.script {
        return Err(Error::InvalidInput);
    }
    if !matches!(
        buffer.0.script,
        Some(script::LATIN | script::ARABIC | script::HAN)
    ) || buffer.0.language.is_none()
        || !buffer.0.direction.is_horizontal()
    {
        return Err(Error::UnsupportedProfile);
    }
    if buffer.0.flags
        != (BufferFlags::BEGINNING_OF_TEXT
            | BufferFlags::END_OF_TEXT
            | BufferFlags::REMOVE_DEFAULT_IGNORABLES)
    {
        return Err(Error::UnsupportedFeature);
    }
    let mut buffer = buffer.0;
    if buffer.len > 0 {
        let target_direction = buffer.direction;
        shape_internal(&mut hb_ot_shape_context_t {
            plan,
            face,
            buffer: &mut buffer,
            target_direction,
            meter,
        })?;
    }
    meter.poll()?;
    if !buffer.successful || buffer.shaping_failed {
        return Err(Error::PartialOutput);
    }
    Ok(GlyphBuffer(buffer))
}
