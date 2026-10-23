use crate::provider::{Context, ProviderResult};
use crate::*;
pub(crate) fn verify(
    request: &Request<'_>,
    ports: &Ports<'_>,
    ctx: &Context<'_>,
) -> ProviderResult<()> {
    ctx.step()?;
    ports.context.member(request)?;
    if ports.resolver.revision()? != request.resolver_revision {
        return Err(Error::RevokedFont);
    }
    for (i, address) in request.reads.iter().enumerate() {
        ctx.step()?;
        let current = ports.context.read(*address)?;
        if current.revision != address.expected_revision
            || current.fingerprint != address.fingerprint
        {
            return Err(Error::StaleRead);
        }
        if current.bytes.len() as u64 > request.limits.requested_owned_bytes {
            return Err(Error::Budget);
        }
        if crate::validate::hash(current.bytes, ctx)? != address.fingerprint {
            return Err(Error::HashMismatch);
        }
        if i == request.text_read_index as usize {
            if current.bytes.len() != request.text_utf8.len() {
                return Err(Error::StaleRead);
            }
            for (a, b) in current.bytes.iter().zip(request.text_utf8.as_bytes()) {
                ctx.step()?;
                if a != b {
                    return Err(Error::StaleRead);
                }
            }
        }
    }
    for style in request.styles {
        ctx.step()?;
        let read = *request
            .reads
            .get(style.read_index as usize)
            .ok_or(Error::AbsentRead)?;
        let current = ports.context.resolved_style(read)?;
        if !same_style(style, &current, ctx)? {
            return Err(Error::StaleRead);
        }
    }
    ctx.step()?;
    let target = ports.context.read(request.result_target.address)?;
    if target.revision != request.result_target.address.expected_revision
        || target.fingerprint != request.result_target.address.fingerprint
    {
        return Err(Error::StaleRead);
    }
    if target.bytes.len() != request.result_target.preimage.len() {
        return Err(Error::StaleRead);
    }
    for (a, b) in target.bytes.iter().zip(request.result_target.preimage) {
        ctx.step()?;
        if a != b {
            return Err(Error::StaleRead);
        }
    }
    ctx.poll()
}
fn bytes_equal(a: &[u8], b: &[u8], ctx: &Context<'_>) -> ProviderResult<bool> {
    if a.len() != b.len() {
        return Ok(false);
    }
    for (a, b) in a.iter().zip(b) {
        ctx.step()?;
        if a != b {
            return Ok(false);
        }
    }
    Ok(true)
}
fn same_style(a: &Style<'_>, b: &Style<'_>, ctx: &Context<'_>) -> ProviderResult<bool> {
    if a.range != b.range
        || a.font_size_pt.to_bits() != b.font_size_pt.to_bits()
        || a.script != b.script
        || a.character_override != b.character_override
        || a.candidates.len() != b.candidates.len()
        || a.axes.len() != b.axes.len()
        || a.features.len() != b.features.len()
    {
        return Ok(false);
    }
    if !bytes_equal(
        a.requested_identity.as_bytes(),
        b.requested_identity.as_bytes(),
        ctx,
    )? || !bytes_equal(a.language.as_bytes(), b.language.as_bytes(), ctx)?
    {
        return Ok(false);
    }
    match (a.substitution, b.substitution) {
        (None, None) => {}
        (Some(a), Some(b)) => {
            if a.reason != b.reason
                || !bytes_equal(
                    a.requested_identity.as_bytes(),
                    b.requested_identity.as_bytes(),
                    ctx,
                )?
                || !bytes_equal(
                    a.resolved_identity.as_bytes(),
                    b.resolved_identity.as_bytes(),
                    ctx,
                )?
            {
                return Ok(false);
            }
        }
        _ => return Ok(false),
    }
    for (a, b) in a.candidates.iter().zip(b.candidates) {
        ctx.step()?;
        if a.content_hash != b.content_hash
            || a.face_index != b.face_index
            || a.resource_revision != b.resource_revision
            || a.grant_revision != b.grant_revision
            || !bytes_equal(a.identity.as_bytes(), b.identity.as_bytes(), ctx)?
            || !bytes_equal(a.location.as_bytes(), b.location.as_bytes(), ctx)?
        {
            return Ok(false);
        }
    }
    for (a, b) in a.axes.iter().zip(b.axes) {
        ctx.step()?;
        if a.tag != b.tag || a.value.to_bits() != b.value.to_bits() {
            return Ok(false);
        }
    }
    for (a, b) in a.features.iter().zip(b.features) {
        ctx.step()?;
        if a.tag != b.tag || a.value != b.value || a.range != b.range || a.declared != b.declared {
            return Ok(false);
        }
    }
    Ok(true)
}
