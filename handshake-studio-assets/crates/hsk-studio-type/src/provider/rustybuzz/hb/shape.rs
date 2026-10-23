use super::buffer::hb_buffer_t;
use super::hb_font_t;
use super::ot_shape::{hb_ot_shape_context_t, shape_internal};
use super::ot_shape_plan::hb_ot_shape_plan_t;
use super::unicode::CharExt;
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
        Some(script::LATIN | script::ARABIC | script::HAN | script::COMMON)
    ) || !matches!(
        buffer.0.direction,
        Direction::LeftToRight | Direction::RightToLeft
    ) || buffer.0.language.is_none()
    {
        return Err(Error::UnsupportedProfile);
    }
    admit_common(&buffer.0, meter)?;
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
        Some(script::LATIN | script::ARABIC | script::HAN | script::COMMON)
    ) || buffer.0.language.is_none()
        || !buffer.0.direction.is_horizontal()
    {
        return Err(Error::UnsupportedProfile);
    }
    admit_common(&buffer.0, meter)?;
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
    // A Common control span is removed entirely; any surviving glyph is glyph-bearing Common.
    if buffer.script == Some(script::COMMON) && buffer.len != 0 {
        return Err(Error::UnsupportedProfile);
    }
    Ok(GlyphBuffer(buffer))
}

/// Common has no shaping profile of its own: admit it only for a non-empty span whose every
/// scalar is a pinned default-ignorable, so removal and source coverage stay provider-derived.
fn admit_common(buffer: &hb_buffer_t<'_>, meter: &Context<'_>) -> ProviderResult<()> {
    if buffer.script != Some(script::COMMON) {
        return Ok(());
    }
    if buffer.len == 0 {
        return Err(Error::UnsupportedProfile);
    }
    for info in &buffer.info[..buffer.len] {
        meter.step()?;
        if !info.as_char().is_default_ignorable() {
            return Err(Error::UnsupportedProfile);
        }
    }
    Ok(())
}
