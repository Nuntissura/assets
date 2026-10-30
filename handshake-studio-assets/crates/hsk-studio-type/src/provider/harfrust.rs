//! SMB-TYPE-ENG-1 spike (feature `harfrust`): HarfRust 0.14.0 as a second shaping provider behind
//! the Type provider boundary. Replacing the vendored provider is Operator decision D7.
//!
//! Bounds are enforced only at the call boundary, never inside HarfRust:
//! - before the call: input scalars against `limits.glyphs`, a pre-charged worst-case work budget
//!   equal to HarfRust's own internal op ceiling (`max(len * 4096, 65536)`), and admission
//!   reservations for the owned font copy read-fonts requires plus an estimated buffer;
//! - cancellation is polled before and after the call only (no mid-shaping abort);
//! - after the call: output glyph count against the glyph budget, HarfRust's own
//!   op/length exhaustion flag, and charged copies of the output.
//!
//! Allocations inside HarfRust (layout cache, plan, buffer growth) are not observable here.
use crate::provider::{Context, ProviderResult, rustybuzz as rb, storage::ChargedVec};
use crate::*;
use harfrust::font::{Font, Variation};
use std::ops::Range;

/// HarfRust's internal per-buffer op ceiling (`Buffer::MAX_OPS_FACTOR`/`MAX_OPS_MIN`, 0.14.0).
const OPS_FACTOR: u64 = 4096;
const OPS_MIN: u64 = 65536;
/// Estimated HarfRust buffer bytes per input scalar: info + position, current and out buffers.
const BUFFER_BYTES_PER_SCALAR: u64 = 4 * 20;

#[derive(Clone, Copy)]
struct Shaped {
    glyph_id: u32,
    cluster: u32,
    x_advance: i32,
    y_advance: i32,
    x_offset: i32,
    y_offset: i32,
}
#[derive(Clone, Copy)]
struct Group {
    cluster: u32,
    surviving_glyphs: usize,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn shape_run<'a>(
    request: &Request<'a>,
    style: &Style<'a>,
    span: Range<usize>,
    level: u8,
    paragraph: usize,
    run: usize,
    ports: &Ports<'a>,
    cache: &mut ChargedVec<'a, crate::fonts::CachedFont<'a>>,
    coverage: &mut ChargedVec<'a, Coverage>,
    counts: &mut Counts,
    ctx: &Context<'a>,
) -> ProviderResult<Run<'a>> {
    // The spike provider implements composition v1 only (no presentation-form ligatures or
    // line-end advance data): a v2 request is refused, never silently composed as v1.
    if crate::validate::semantics(request.composition_version)
        != Some(crate::validate::Semantics::V1)
    {
        return Err(Error::UnknownComposition);
    }
    let text = &request.text_utf8[span.clone()];
    let (candidate, index) =
        crate::engine::select_font(request, style, text, ports, cache, counts, ctx)?;
    let font = &mut cache[index];
    ports.resolver.verify_lifetime(&font.resource, &font.read)?;
    let axes = crate::fonts::axes(&mut font.face, style, request.limits, ctx)?;
    let effective_script = resolved_script(style.script, text, ctx)?;
    let script = harfrust::Script::from_iso15924_tag(harfrust::Tag::new(&effective_script))
        .ok_or(Error::UnsupportedProfile)?;
    let rtl = level % 2 == 1;
    let language: harfrust::Language =
        style.language.parse().map_err(|_| Error::InvalidInput)?;
    let mut receipts = ChargedVec::with_capacity(style.features.len(), ctx)?;
    let mut features = ChargedVec::with_capacity(style.features.len(), ctx)?;
    for feature in style.features {
        ctx.step()?;
        let tag = crate::provider::ttf::Tag::from_bytes(&feature.tag);
        let mut available = false;
        for (_, table) in font.face.layout_tables() {
            if table.features.index_bounded(tag, ctx)?.is_some() {
                available = true;
                break;
            }
        }
        if feature.declared && !available {
            return Err(Error::UnsupportedFeature);
        }
        receipts.push(
            FeatureReceipt {
                feature: *feature,
                state: if available {
                    FeatureState::Applied
                } else {
                    FeatureState::UnavailableInert
                },
            },
            ctx,
        )?;
        features.push(
            harfrust::Feature {
                tag: harfrust::Tag::new(&feature.tag),
                value: feature.value,
                start: u32::try_from(feature.range.start).map_err(|_| Error::Overflow)?,
                end: u32::try_from(feature.range.end).map_err(|_| Error::Overflow)?,
            },
            ctx,
        )?;
    }
    for i in 1..receipts.len() {
        let mut j = i;
        while j > 0 {
            ctx.step()?;
            let a = receipts[j - 1].feature;
            let b = receipts[j].feature;
            if (a.tag, a.range.start, a.range.end, a.value, a.declared)
                <= (b.tag, b.range.start, b.range.end, b.value, b.declared)
            {
                break;
            }
            receipts.swap(j - 1, j);
            j -= 1;
        }
    }

    // Call boundary: everything HarfRust may consume is admitted before the call.
    let scalars = text.chars().count() as u64;
    if scalars > request.limits.glyphs {
        return Err(Error::Budget);
    }
    ctx.units(scalars.saturating_mul(OPS_FACTOR).max(OPS_MIN))?;
    let bytes = font.read.bytes;
    let _font_copy = ctx.reserve(bytes.len() as u64)?;
    let _buffer =
        ctx.reserve(scalars.max(1).checked_mul(BUFFER_BYTES_PER_SCALAR).ok_or(Error::Overflow)?)?;
    ctx.units(bytes.len() as u64)?;
    ctx.poll()?;
    let owned = Font::new(bytes.to_vec(), font.resource.face_index).ok_or(Error::CorruptFont)?;
    let instance = if axes.is_empty() {
        owned
    } else {
        owned
            .instance_builder()
            .variations(
                axes.iter()
                    .map(|a| Variation::new(harfrust::Tag::new(&a.axis.tag), a.axis.value)),
            )
            .build()
    };
    let upem = instance.units_per_em();
    let shaper = harfrust::ShaperFont::new(&instance);
    let mut buffer = harfrust::Buffer::new();
    for (offset, ch) in text.char_indices() {
        ctx.step()?;
        buffer.push(
            u32::from(ch),
            u32::try_from(span.start + offset).map_err(|_| Error::Overflow)?,
        );
    }
    buffer.set_direction(if rtl {
        harfrust::Direction::RightToLeft
    } else {
        harfrust::Direction::LeftToRight
    });
    buffer.set_script(Some(script));
    buffer.set_language(Some(language));
    buffer.set_cluster_level(harfrust::ClusterLevel::MonotoneGraphemes);
    buffer.set_flags(
        harfrust::BufferFlags::BEGINNING_OF_TEXT
            | harfrust::BufferFlags::END_OF_TEXT
            | harfrust::BufferFlags::REMOVE_DEFAULT_IGNORABLES,
    );
    harfrust::shape(
        &shaper,
        &mut buffer,
        harfrust::ShapeOptions::new().features(features.as_slice()),
    )
    .map_err(|_| Error::InvalidInput)?;
    ctx.poll()?;
    if !buffer.allocation_successful() {
        return Err(Error::PartialOutput);
    }
    let infos = buffer.glyph_infos();
    let positions = buffer.glyph_positions();
    if infos.len() != positions.len() {
        return Err(Error::CorruptFont);
    }
    counts.glyphs = counts
        .glyphs
        .checked_add(infos.len() as u64)
        .ok_or(Error::Overflow)?;
    if counts.glyphs > request.limits.glyphs {
        return Err(Error::Budget);
    }
    let mut shaped = ChargedVec::with_capacity(infos.len(), ctx)?;
    for (info, position) in infos.iter().zip(positions) {
        shaped.push(
            Shaped {
                glyph_id: info.glyph_id,
                cluster: info.cluster,
                x_advance: position.x_advance,
                y_advance: position.y_advance,
                x_offset: position.x_offset,
                y_offset: position.y_offset,
            },
            ctx,
        )?;
    }
    drop(buffer);
    drop(shaper);
    drop(instance);

    // HarfRust exposes no pre-deletion source groups: derive them from surviving clusters, with a
    // zero-glyph group for any leading span bytes whose glyphs were all removed.
    let mut groups: ChargedVec<'a, Group> = ChargedVec::with_capacity(shaped.len() + 1, ctx)?;
    for glyph in shaped.iter() {
        ctx.step()?;
        let mut found = false;
        for group in groups.iter_mut() {
            ctx.step()?;
            if group.cluster == glyph.cluster {
                group.surviving_glyphs += 1;
                found = true;
                break;
            }
        }
        if !found {
            groups.push(
                Group {
                    cluster: glyph.cluster,
                    surviving_glyphs: 1,
                },
                ctx,
            )?;
        }
    }
    let first = u32::try_from(span.start).map_err(|_| Error::Overflow)?;
    if !groups.iter().any(|g| g.cluster == first) {
        groups.push(
            Group {
                cluster: first,
                surviving_glyphs: 0,
            },
            ctx,
        )?;
    }
    for i in 1..groups.len() {
        let mut j = i;
        while j > 0 {
            ctx.step()?;
            if groups[j - 1].cluster <= groups[j].cluster {
                break;
            }
            groups.swap(j - 1, j);
            j -= 1;
        }
    }
    let scale = u32::from(upem);
    if scale == 0 {
        return Err(Error::CorruptFont);
    }
    let points = |value| crate::engine::points(value, style.font_size_pt, scale);
    let mut glyphs = ChargedVec::with_capacity(shaped.len(), ctx)?;
    for glyph in shaped.iter() {
        ctx.step()?;
        if glyph.glyph_id == 0 {
            return Err(Error::MissingGlyph);
        }
        let mut end = None;
        for (i, group) in groups.iter().enumerate() {
            ctx.step()?;
            if group.cluster == glyph.cluster {
                end = Some(
                    groups
                        .get(i + 1)
                        .map_or(span.end as u64, |v| u64::from(v.cluster)),
                );
                break;
            }
        }
        glyphs.push(
            Glyph {
                glyph_id: glyph.glyph_id,
                cluster: SourceRange {
                    start: u64::from(glyph.cluster),
                    end: end.ok_or(Error::InvalidMapping)?,
                },
                x_advance_pt: points(glyph.x_advance)?,
                y_advance_pt: points(glyph.y_advance)?,
                x_offset_pt: points(glyph.x_offset)?,
                y_offset_pt: points(glyph.y_offset)?,
            },
            ctx,
        )?;
    }
    let mut expected = span.start as u64;
    for (i, group) in groups.iter().enumerate() {
        ctx.step()?;
        let source = SourceRange {
            start: u64::from(group.cluster),
            end: groups
                .get(i + 1)
                .map_or(span.end as u64, |v| u64::from(v.cluster)),
        };
        if source.start != expected
            || source.end <= source.start
            || source.end > span.end as u64
            || !request.text_utf8.is_char_boundary(source.start as usize)
            || !request.text_utf8.is_char_boundary(source.end as usize)
        {
            return Err(Error::InvalidMapping);
        }
        expected = source.end;
        let mut first = None;
        let mut last = 0;
        let mut found = 0;
        for (j, glyph) in glyphs.iter().enumerate() {
            ctx.step()?;
            if glyph.cluster == source {
                if first.is_none() {
                    first = Some(j);
                }
                last = j + 1;
                found += 1;
            }
        }
        let (kind, reference) = if let Some(first) = first {
            if last - first != found || found != group.surviving_glyphs {
                return Err(Error::InvalidMapping);
            }
            (
                CoverageKind::ShapedCluster,
                Some(GlyphReference {
                    paragraph: u32::try_from(paragraph).map_err(|_| Error::Overflow)?,
                    run: u32::try_from(run).map_err(|_| Error::Overflow)?,
                    start: u32::try_from(first).map_err(|_| Error::Overflow)?,
                    end: u32::try_from(last).map_err(|_| Error::Overflow)?,
                }),
            )
        } else {
            for ch in request.text_utf8[source.start as usize..source.end as usize].chars() {
                ctx.step()?;
                if !rb::is_default_ignorable(ch) {
                    return Err(Error::MissingGlyph);
                }
            }
            (CoverageKind::NonrenderingControl, None)
        };
        coverage.push(
            Coverage {
                source,
                kind,
                glyphs: reference,
            },
            ctx,
        )?;
    }
    if expected != span.end as u64 {
        return Err(Error::InvalidMapping);
    }
    Ok(Run {
        source: SourceRange {
            start: span.start as u64,
            end: span.end as u64,
        },
        bidi_level: level,
        direction: if rtl {
            Direction::RightToLeft
        } else {
            Direction::LeftToRight
        },
        script: effective_script,
        language: style.language,
        font_size_pt: style.font_size_pt,
        requested_identity: style.requested_identity,
        resolved: font.resource,
        candidate_index: u32::try_from(candidate).map_err(|_| Error::Overflow)?,
        substitution: if candidate > 0 {
            Some(SubstitutionReason::CoverageFallback)
        } else {
            style.substitution.map(|v| v.reason)
        },
        provider_scale: scale,
        units_per_em: scale,
        cap_height_pt: crate::engine::face_metric(font.face.capital_height(), style.font_size_pt, scale),
        x_height_pt: crate::engine::face_metric(font.face.x_height(), style.font_size_pt, scale),
        axes,
        features: receipts,
        glyphs,
        line_end_advances: ChargedVec::new(),
        ligature_components: ChargedVec::new(),
    })
}

/// One strong ISO 15924 script per span (any script HarfRust knows); Common only for spans of
/// pinned default-ignorables, as on the vendored path.
fn resolved_script(declared: [u8; 4], text: &str, ctx: &Context<'_>) -> ProviderResult<[u8; 4]> {
    use unicode_script::{Script, UnicodeScript};
    let mut selected = if declared == *b"Zyyy" {
        None
    } else {
        Some(declared)
    };
    for ch in text.chars() {
        ctx.units(13)?;
        let current = match ch.script() {
            Script::Common | Script::Inherited => continue,
            Script::Unknown => return Err(Error::UnsupportedProfile),
            other => <[u8; 4]>::try_from(other.short_name().as_bytes())
                .map_err(|_| Error::UnsupportedProfile)?,
        };
        match selected {
            Some(previous) if previous != current => return Err(Error::UnsupportedProfile),
            Some(_) => {}
            None => selected = Some(current),
        }
    }
    if let Some(script) = selected {
        return Ok(script);
    }
    for ch in text.chars() {
        ctx.step()?;
        if !rb::is_default_ignorable(ch) {
            return Err(Error::UnsupportedProfile);
        }
    }
    Ok(*b"Zyyy")
}
