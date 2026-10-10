use crate::provider::{Context, ProviderResult, storage::ChargedVec};
use crate::*;
use sha2::{Digest, Sha256};
pub const COMPOSITION_VERSION: &str = "hsk-type-shaped-runs@1/rustybuzz-0.20.1/ttf-parser-0.25.1/bidi-0.3.18-u16/ccc-0.4.0-u16/mirroring-0.4.0-u16/script-0.5.8-u17/properties-0.1.4-u17/preserve/source-groups-v1";
pub(crate) fn hash(bytes: &[u8], ctx: &Context<'_>) -> ProviderResult<[u8; 32]> {
    let mut digest = Sha256::new();
    for chunk in bytes.chunks(256) {
        ctx.step()?;
        digest.update(chunk);
    }
    Ok(digest.finalize().into())
}
fn key(value: &str, ctx: &Context<'_>) -> ProviderResult<()> {
    if value.is_empty() || value.len() > 128 {
        return Err(Error::InvalidInput);
    }
    for byte in value.bytes() {
        ctx.step()?;
        if !byte.is_ascii_alphanumeric() && byte != b'_' && byte != b'-' && byte != b'.' {
            return Err(Error::InvalidInput);
        }
    }
    Ok(())
}
fn range(value: SourceRange, text: &str) -> ProviderResult<()> {
    let start = usize::try_from(value.start).map_err(|_| Error::Overflow)?;
    let end = usize::try_from(value.end).map_err(|_| Error::Overflow)?;
    if start >= end
        || end > text.len()
        || !text.is_char_boundary(start)
        || !text.is_char_boundary(end)
    {
        return Err(Error::InvalidInput);
    }
    Ok(())
}
fn tag(value: [u8; 4]) -> ProviderResult<()> {
    if !value.iter().all(u8::is_ascii) {
        return Err(Error::InvalidInput);
    }
    Ok(())
}
pub(crate) fn validate<'a>(
    request: &Request<'a>,
    ctx: &Context<'a>,
) -> ProviderResult<ChargedVec<'a, u32>> {
    ctx.step()?;
    let l = request.limits;
    for cap in [
        l.input_bytes,
        l.input_scalars,
        l.styles,
        l.features,
        l.axes,
        l.font_resources,
        l.font_bytes,
        l.parser_tables,
        l.lookups,
        l.runs,
        l.glyphs,
        l.fallback_attempts,
        l.recursion,
        l.work_units,
        l.requested_owned_bytes,
        l.retained_generations,
    ] {
        ctx.step()?;
        if cap == 0 {
            return Err(Error::Budget);
        }
    }
    ctx.set_shape_limits(l.glyphs, l.lookups)?;
    if request.document_id.prefix() != "SDOC"
        || request.story_id.prefix() != "STXT"
        || request.layer_id.prefix() != "SLYR"
    {
        return Err(Error::InvalidInput);
    }
    key(request.correlation_id, ctx)?;
    if request.composition_version != COMPOSITION_VERSION {
        return Err(Error::UnknownComposition);
    }
    if request.normalization != Normalization::Preserve {
        return Err(Error::UnsupportedNormalization);
    }
    if request.text_utf8.len() as u64 > l.input_bytes || request.styles.len() as u64 > l.styles {
        return Err(Error::Budget);
    }
    let mut scalars = 0u64;
    for _ in request.text_utf8.chars() {
        ctx.step()?;
        scalars = scalars.checked_add(1).ok_or(Error::Overflow)?;
        if scalars > l.input_scalars {
            return Err(Error::Budget);
        }
    }
    if hash(request.text_utf8.as_bytes(), ctx)? != request.text_sha256 {
        return Err(Error::HashMismatch);
    }
    for read in request.reads {
        ctx.step()?;
        key(read.property, ctx)?;
        if !matches!(read.owner.prefix(), "STXT" | "STYS" | "SLYR") {
            return Err(Error::InvalidInput);
        }
    }
    if !matches!(
        request.result_target.address.owner.prefix(),
        "STXT" | "SLYR"
    ) {
        return Err(Error::InvalidInput);
    }
    key(request.result_target.address.property, ctx)?;
    if hash(request.result_target.preimage, ctx)? != request.result_target.address.fingerprint {
        return Err(Error::HashMismatch);
    }
    let text_read = request
        .reads
        .get(request.text_read_index as usize)
        .ok_or(Error::AbsentRead)?;
    if text_read.owner != request.story_id || text_read.fingerprint != request.text_sha256 {
        return Err(Error::InvalidInput);
    }
    let mut order = ChargedVec::with_capacity(request.styles.len(), ctx)?;
    let mut features = 0u64;
    let mut axes = 0u64;
    let mut fonts = 0u64;
    for (index, style) in request.styles.iter().enumerate() {
        ctx.step()?;
        range(style.range, request.text_utf8)?;
        request
            .reads
            .get(style.read_index as usize)
            .ok_or(Error::AbsentRead)?;
        if !style.font_size_pt.is_finite() || style.font_size_pt <= 0.0 {
            return Err(Error::InvalidInput);
        }
        if !matches!(&style.script, b"Latn" | b"Arab" | b"Hani" | b"Zyyy") {
            return Err(Error::UnsupportedProfile);
        }
        if style.language.is_empty()
            || style.language.len() > 63
            || style.requested_identity.is_empty()
        {
            return Err(Error::InvalidInput);
        }
        for byte in style.language.bytes() {
            ctx.step()?;
            if !byte.is_ascii_alphanumeric() && byte != b'-' {
                return Err(Error::InvalidInput);
            }
        }
        features = features
            .checked_add(style.features.len() as u64)
            .ok_or(Error::Overflow)?;
        axes = axes
            .checked_add(style.axes.len() as u64)
            .ok_or(Error::Overflow)?;
        fonts = fonts
            .checked_add(style.candidates.len() as u64)
            .ok_or(Error::Overflow)?;
        if features > l.features || axes > l.axes || fonts > l.font_resources {
            return Err(Error::Budget);
        }
        if style.candidates.is_empty() {
            return Err(Error::MissingFont);
        }
        for (i, axis) in style.axes.iter().enumerate() {
            ctx.step()?;
            tag(axis.tag)?;
            if !axis.value.is_finite() {
                return Err(Error::InvalidAxis);
            }
            for other in &style.axes[..i] {
                ctx.step()?;
                if other.tag == axis.tag {
                    return Err(Error::InvalidAxis);
                }
            }
        }
        for (i, feature) in style.features.iter().enumerate() {
            ctx.step()?;
            tag(feature.tag)?;
            range(feature.range, request.text_utf8)?;
            if feature.range.start < style.range.start || feature.range.end > style.range.end {
                return Err(Error::InvalidInput);
            }
            for other in &style.features[..i] {
                ctx.step()?;
                if other.tag == feature.tag
                    && other.range.start < feature.range.end
                    && feature.range.start < other.range.end
                    && (other.value != feature.value || other.declared != feature.declared)
                {
                    return Err(Error::UnsupportedFeature);
                }
            }
        }
        for candidate in style.candidates {
            ctx.step()?;
            if candidate.identity.is_empty() || candidate.location.is_empty() {
                return Err(Error::InvalidInput);
            }
        }
        order.push(u32::try_from(index).map_err(|_| Error::Overflow)?, ctx)?;
    }
    for i in 1..order.len() {
        ctx.step()?;
        let mut j = i;
        while j > 0 {
            ctx.step()?;
            if request.styles[order[j - 1] as usize].range.start
                <= request.styles[order[j] as usize].range.start
            {
                break;
            }
            order.swap(j - 1, j);
            j -= 1;
        }
    }
    let mut end = 0;
    for index in order.iter() {
        ctx.step()?;
        let span = request.styles[*index as usize].range;
        if span.start != end {
            return Err(Error::InvalidInput);
        }
        end = span.end;
    }
    if end != request.text_utf8.len() as u64 {
        return Err(Error::InvalidInput);
    }
    if request.text_utf8.is_empty() {
        if !request.styles.is_empty() || !request.paragraphs.is_empty() {
            return Err(Error::InvalidInput);
        }
    } else {
        let mut expected = 0;
        let mut para_index = 0usize;
        for (i, c) in request.text_utf8.char_indices() {
            ctx.step()?;
            if crate::provider::bidi::class(c, ctx)? == crate::provider::bidi::BidiClass::B {
                check_paragraph(request, para_index, expected, (i + c.len_utf8()) as u64)?;
                expected = (i + c.len_utf8()) as u64;
                para_index += 1;
            }
        }
        if expected < end {
            check_paragraph(request, para_index, expected, end)?;
            para_index += 1;
        }
        if para_index != request.paragraphs.len() {
            return Err(Error::InvalidBidi);
        }
    }
    Ok(order)
}
fn check_paragraph(
    request: &Request<'_>,
    index: usize,
    start: u64,
    end: u64,
) -> ProviderResult<()> {
    let para = request.paragraphs.get(index).ok_or(Error::InvalidBidi)?;
    if para.range != (SourceRange { start, end }) {
        return Err(Error::InvalidBidi);
    }
    if para.provenance == DirectionProvenance::StoryDefault {
        match (request.story_direction, para.direction) {
            (StoryDirection::LeftToRight, Direction::LeftToRight)
            | (StoryDirection::RightToLeft, Direction::RightToLeft) => {}
            _ => return Err(Error::InvalidBidi),
        }
    }
    Ok(())
}
