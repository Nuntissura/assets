use crate::provider::{Context, ProviderResult, rustybuzz as rb, storage::ChargedVec, ttf};
use crate::*;
/// Face precedes its borrower lease in drop order; neither can escape the invocation.
pub(crate) struct CachedFont<'a> {
    pub face: rb::Face<'a>,
    pub read: FontRead<'a>,
    pub resource: FontResource<'a>,
}
pub(crate) fn load<'a>(
    resource: FontResource<'a>,
    ports: &Ports<'a>,
    limits: Limits,
    ctx: &Context<'a>,
) -> ProviderResult<CachedFont<'a>> {
    ctx.step()?;
    let resolver: &'a dyn FontResolver = ports.resolver;
    let read = resolver.resolve(&resource)?;
    ctx.poll()?;
    if read.resource_revision != resource.resource_revision
        || read.grant_revision != resource.grant_revision
    {
        return Err(Error::RevokedFont);
    }
    if read.bytes.is_empty() || read.bytes.len() as u64 > limits.font_bytes {
        return Err(Error::Budget);
    }
    if read.lease.requested_bytes() < read.bytes.len() as u64 {
        return Err(Error::LeaseUnavailable);
    }
    let allocation = ports.admission.snapshot()?;
    if allocation.current_requested_bytes > limits.requested_owned_bytes
        || allocation.operation_peak_requested_bytes > limits.requested_owned_bytes
    {
        return Err(Error::Budget);
    }
    resolver.verify_lifetime(&resource, &read)?;
    if crate::validate::hash(read.bytes, ctx)? != resource.content_hash {
        return Err(Error::HashMismatch);
    }
    ctx.step()?;
    let raw =
        ttf::RawFace::parse(read.bytes, resource.face_index).map_err(|error| match error {
            ttf::FaceParsingError::FaceIndexOutOfBounds => Error::UnavailableFace,
            ttf::FaceParsingError::UnknownMagic => Error::UnsupportedFont,
            _ => Error::CorruptFont,
        })?;
    if u64::from(raw.table_records.len()) > limits.parser_tables {
        return Err(Error::Budget);
    }
    if raw
        .table_bounded(ttf::Tag::from_bytes(b"CFF2"), ctx)?
        .is_some()
    {
        return Err(Error::UnsupportedProfile);
    }
    let face = rb::Face::from_slice(read.bytes, resource.face_index, ctx)?;
    ctx.poll()?;
    Ok(CachedFont {
        face,
        read,
        resource,
    })
}
pub(crate) fn axes<'a>(
    face: &mut rb::Face<'_>,
    style: &Style<'_>,
    limits: Limits,
    ctx: &Context<'a>,
) -> ProviderResult<ChargedVec<'a, AxisReceipt>> {
    let parsed = face.variation_axes_bounded(ctx)?;
    if u64::from(parsed.len()) > limits.axes {
        return Err(Error::Budget);
    }
    let mut effective = ChargedVec::with_capacity(parsed.len() as usize, ctx)?;
    for index in 0..parsed.len() {
        ctx.step()?;
        let axis = parsed.get_bounded(index, ctx)?.ok_or(Error::CorruptFont)?;
        if !axis.min_value.is_finite()
            || !axis.max_value.is_finite()
            || !axis.def_value.is_finite()
            || axis.min_value > axis.def_value
            || axis.def_value > axis.max_value
        {
            return Err(Error::CorruptFont);
        }
        let mut value = axis.def_value;
        let mut defaulted = true;
        for requested in style.axes {
            ctx.step()?;
            if requested.tag == axis.tag.to_bytes() {
                value = requested.value;
                defaulted = false;
                break;
            }
        }
        if value < axis.min_value || value > axis.max_value {
            return Err(Error::InvalidAxis);
        }
        effective.push(
            AxisReceipt {
                axis: Axis {
                    tag: axis.tag.to_bytes(),
                    value,
                },
                defaulted,
            },
            ctx,
        )?;
    }
    for requested in style.axes {
        ctx.step()?;
        let mut found = false;
        for axis in effective.iter() {
            ctx.step()?;
            if axis.axis.tag == requested.tag {
                found = true;
                break;
            }
        }
        if !found {
            return Err(Error::InvalidAxis);
        }
    }
    for i in 1..effective.len() {
        let mut j = i;
        while j > 0 {
            ctx.step()?;
            if effective[j - 1].axis.tag <= effective[j].axis.tag {
                break;
            }
            effective.swap(j - 1, j);
            j -= 1;
        }
    }
    for axis in effective.iter() {
        ctx.step()?;
        face.set_variations(
            &[rb::Variation {
                tag: ttf::Tag::from_bytes(&axis.axis.tag),
                value: axis.axis.value,
            }],
            ctx,
        )?;
    }
    Ok(effective)
}
pub(crate) fn covers(face: &rb::Face<'_>, text: &str, ctx: &Context<'_>) -> ProviderResult<bool> {
    for character in text.chars() {
        ctx.step()?;
        if rb::is_default_ignorable(character) {
            continue;
        }
        let class = crate::provider::bidi::class(character, ctx)?;
        if matches!(
            class,
            crate::provider::bidi::BidiClass::B | crate::provider::bidi::BidiClass::BN
        ) {
            continue;
        }
        if !face.has_glyph(character as u32, ctx)? {
            return Ok(false);
        }
    }
    Ok(true)
}
