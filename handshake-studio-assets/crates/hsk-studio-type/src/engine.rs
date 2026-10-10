use crate::provider::{Context, ProviderResult, bidi, rustybuzz as rb, storage::ChargedVec, ttf};
use crate::*;
use std::ops::Range;

/// Stateless native shaping; every resource and authority is supplied by the caller.
#[derive(Default)]
pub struct TextEngine;
impl TextEngine {
    pub fn shaped_runs<'a>(
        &self,
        request: Request<'a>,
        ports: &Ports<'a>,
    ) -> Result<Prepared<'a>, Rejected> {
        let ctx = Context::new(
            request.cancellation,
            ports.context,
            request.cancel_epoch,
            ports.admission,
            request.limits.work_units,
            request.limits.requested_owned_bytes,
            request.limits.recursion,
        )
        .map_err(|error| Rejected {
            error,
            source: None,
            counts: Counts::default(),
            cancel_epoch: request.cancel_epoch,
        })?;
        let mut counts = Counts {
            input_bytes: request.text_utf8.len() as u64,
            ..Counts::default()
        };
        self.prepare(request, ports, &ctx, &mut counts)
            .map_err(|error| {
                counts.work_units = ctx.work();
                if let Ok(snapshot) = ports.admission.snapshot() {
                    counts.current_requested_bytes = snapshot.current_requested_bytes;
                    counts.operation_peak_requested_bytes = snapshot.operation_peak_requested_bytes;
                }
                counts.retained_generations = 0;
                Rejected {
                    error,
                    source: ctx.source(),
                    counts,
                    cancel_epoch: request.cancel_epoch,
                }
            })
    }
    fn prepare<'a>(
        &self,
        request: Request<'a>,
        ports: &Ports<'a>,
        ctx: &Context<'a>,
        counts: &mut Counts,
    ) -> ProviderResult<Prepared<'a>> {
        let input = ports.input_lifetime.verify(&request)?;
        let initial = ports.admission.snapshot()?;
        if input.lifetime_id == 0
            || input.requested_bytes == 0
            || input.requested_bytes > initial.current_requested_bytes
            || initial.current_requested_bytes > request.limits.requested_owned_bytes
            || initial.retained_generations >= request.limits.retained_generations
        {
            return Err(Error::LeaseUnavailable);
        }
        let generation = ports
            .admission
            .retain_generation(request.limits.retained_generations)?;
        let retained = ports.admission.snapshot()?;
        if retained.retained_generations
            != initial
                .retained_generations
                .checked_add(1)
                .ok_or(Error::Overflow)?
        {
            return Err(Error::LeaseUnavailable);
        }
        let order = crate::validate::validate(&request, &ctx)?;
        crate::readset::verify(&request, ports, &ctx)?;
        for _ in request.text_utf8.chars() {
            ctx.step()?;
            counts.scalars = counts.scalars.checked_add(1).ok_or(Error::Overflow)?;
        }
        let mut paragraphs = ChargedVec::with_capacity(request.paragraphs.len(), &ctx)?;
        let mut coverage = ChargedVec::new();
        let mut cache = ChargedVec::new();
        if !request.text_utf8.is_empty() {
            let mut overrides = ChargedVec::new();
            overrides.resize_copy(request.text_utf8.len(), bidi::StyleOverride::Default, &ctx)?;
            for index in order.iter() {
                ctx.step()?;
                let style = &request.styles[*index as usize];
                let value = match style.character_override {
                    CharacterOverride::Default => bidi::StyleOverride::Default,
                    CharacterOverride::LeftToRight => bidi::StyleOverride::Ltr,
                    CharacterOverride::RightToLeft => bidi::StyleOverride::Rtl,
                };
                for byte in style.range.start..style.range.end {
                    ctx.step()?;
                    overrides[byte as usize] = value;
                }
            }
            let mut levels = ChargedVec::with_capacity(request.paragraphs.len(), &ctx)?;
            for para in request.paragraphs {
                ctx.step()?;
                levels.push(
                    match para.direction {
                        Direction::LeftToRight => bidi::level::Level::ltr(),
                        Direction::RightToLeft => bidi::level::Level::rtl(),
                    },
                    &ctx,
                )?;
            }
            let mut resolved = bidi::resolve(request.text_utf8, &overrides, &levels, &ctx)?;
            for p in 0..resolved.paragraphs.len() {
                ctx.step()?;
                let paragraph = resolved.paragraphs[p];
                let visual = bidi::visual_runs(
                    &mut resolved,
                    request.text_utf8,
                    paragraph,
                    paragraph.start..paragraph.end,
                    &ctx,
                )?;
                let mut runs = ChargedVec::new();
                for visual_range in visual.iter() {
                    ctx.step()?;
                    let level = resolved.levels[visual_range.start].number();
                    let mut pieces = ChargedVec::new();
                    for index in order.iter() {
                        ctx.step()?;
                        let style = &request.styles[*index as usize];
                        let start = visual_range.start.max(style.range.start as usize);
                        let end = visual_range.end.min(style.range.end as usize);
                        if start >= end {
                            continue;
                        }
                        // Keep each original paragraph separator outside the shaping buffer.
                        let mut cursor = start;
                        for (offset, ch) in request.text_utf8[start..end].char_indices() {
                            ctx.step()?;
                            let at = start + offset;
                            if bidi::class(ch, &ctx)? == bidi::BidiClass::B {
                                if cursor < at {
                                    pieces.push((*index, cursor..at), &ctx)?;
                                }
                                coverage.push(
                                    Coverage {
                                        source: SourceRange {
                                            start: at as u64,
                                            end: (at + ch.len_utf8()) as u64,
                                        },
                                        kind: CoverageKind::ParagraphSeparator,
                                        glyphs: None,
                                    },
                                    &ctx,
                                )?;
                                cursor = at + ch.len_utf8();
                            }
                        }
                        if cursor < end {
                            pieces.push((*index, cursor..end), &ctx)?;
                        }
                    }
                    if level % 2 == 1 {
                        let mut left = 0;
                        let mut right = pieces.len();
                        while left < right {
                            ctx.step()?;
                            right -= 1;
                            if left < right {
                                pieces.swap(left, right);
                                left += 1;
                            }
                        }
                    }
                    for (index, span) in pieces.iter() {
                        ctx.step()?;
                        counts.runs = counts.runs.checked_add(1).ok_or(Error::Overflow)?;
                        if counts.runs > request.limits.runs {
                            return Err(Error::Budget);
                        }
                        ctx.set_source(Some(SourceRange {
                            start: span.start as u64,
                            end: span.end as u64,
                        }));
                        let run = shape(
                            &request,
                            &request.styles[*index as usize],
                            span.clone(),
                            level,
                            p,
                            runs.len(),
                            ports,
                            &mut cache,
                            &mut coverage,
                            counts,
                            &ctx,
                        )?;
                        runs.push(run, &ctx)?;
                        ctx.set_source(None);
                    }
                }
                paragraphs.push(
                    ParagraphResult {
                        source: SourceRange {
                            start: paragraph.start as u64,
                            end: paragraph.end as u64,
                        },
                        direction: request.paragraphs[p].direction,
                        runs,
                    },
                    &ctx,
                )?;
            }
        }
        for i in 1..coverage.len() {
            let mut j = i;
            while j > 0 {
                ctx.step()?;
                if coverage[j - 1].source.start <= coverage[j].source.start {
                    break;
                }
                coverage.swap(j - 1, j);
                j -= 1;
            }
        }
        let mut endpoint = 0;
        for entry in coverage.iter() {
            ctx.step()?;
            if entry.source.start != endpoint || entry.source.end <= endpoint {
                return Err(Error::InvalidMapping);
            }
            endpoint = entry.source.end;
        }
        if endpoint != request.text_utf8.len() as u64 {
            return Err(Error::InvalidMapping);
        }
        let disposition = if request.text_utf8.is_empty() {
            TextDisposition::EmptyInput
        } else if counts.glyphs == 0 {
            TextDisposition::ControlOnly
        } else {
            TextDisposition::Shaped
        };
        let serialized =
            crate::serialize::encode(&request, &paragraphs, &coverage, disposition, &ctx)?;
        // Drop all parser borrowers before their font leases, then verify the authoritative closure again.
        drop(cache);
        crate::readset::verify(&request, ports, &ctx)?;
        let allocation = ports.admission.snapshot()?;
        if allocation.current_requested_bytes > request.limits.requested_owned_bytes
            || allocation.operation_peak_requested_bytes > request.limits.requested_owned_bytes
        {
            return Err(Error::Budget);
        }
        counts.work_units = ctx.work();
        counts.current_requested_bytes = allocation.current_requested_bytes;
        counts.operation_peak_requested_bytes = allocation.operation_peak_requested_bytes;
        counts.retained_generations = allocation.retained_generations;
        ctx.poll()?;
        Ok(Prepared {
            request,
            paragraphs,
            coverage,
            serialized,
            disposition,
            counts: *counts,
            _generation: generation,
        })
    }
}

fn shape<'a>(
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
    let text = &request.text_utf8[span.clone()];
    let mut selected = None;
    for (candidate, resource) in style.candidates.iter().enumerate() {
        ctx.step()?;
        if candidate > 0 {
            counts.fallback_attempts = counts
                .fallback_attempts
                .checked_add(1)
                .ok_or(Error::Overflow)?;
            if counts.fallback_attempts > request.limits.fallback_attempts {
                return Err(Error::Budget);
            }
        }
        if candidate == 0 && resource.identity != style.requested_identity {
            let mapping = style.substitution.ok_or(Error::MissingFont)?;
            if mapping.requested_identity != style.requested_identity
                || mapping.resolved_identity != resource.identity
                || mapping.reason == SubstitutionReason::CoverageFallback
            {
                return Err(Error::MissingFont);
            }
        }
        let mut cached = None;
        for (i, font) in cache.iter().enumerate() {
            ctx.step()?;
            if font.resource.content_hash == resource.content_hash
                && font.resource.face_index == resource.face_index
                && font.resource.resource_revision == resource.resource_revision
                && font.resource.grant_revision == resource.grant_revision
                && font.resource.identity == resource.identity
                && font.resource.location == resource.location
            {
                cached = Some(i);
                break;
            }
        }
        let index = match cached {
            Some(index) => index,
            None => {
                if cache.len() as u64 >= request.limits.font_resources {
                    return Err(Error::Budget);
                }
                let font = crate::fonts::load(*resource, ports, request.limits, ctx)?;
                cache.push(font, ctx)?;
                counts.fonts = counts.fonts.checked_add(1).ok_or(Error::Overflow)?;
                cache.len() - 1
            }
        };
        // A complete style/bidi span is a conservative shaping-safe fallback unit: never split marks or joining context.
        if crate::fonts::covers(&cache[index].face, text, ctx)? {
            selected = Some((candidate, index));
            break;
        }
    }
    let (candidate, index) = selected.ok_or(Error::MissingGlyph)?;
    let font = &mut cache[index];
    ports.resolver.verify_lifetime(&font.resource, &font.read)?;
    let axes = crate::fonts::axes(&mut font.face, style, request.limits, ctx)?;
    let effective_script = resolved_script(style.script, text, ctx)?;
    let script = match &effective_script {
        b"Latn" => rb::script::LATIN,
        b"Arab" => rb::script::ARABIC,
        b"Hani" => rb::script::HAN,
        b"Zyyy" => rb::script::COMMON,
        _ => return Err(Error::UnsupportedProfile),
    };
    let direction = if level % 2 == 0 {
        Direction::LeftToRight
    } else {
        Direction::RightToLeft
    };
    let provider_direction = if level % 2 == 0 {
        rb::Direction::LeftToRight
    } else {
        rb::Direction::RightToLeft
    };
    let language = rb::Language::parse(style.language, ctx)?;
    let mut features = ChargedVec::with_capacity(style.features.len(), ctx)?;
    for feature in style.features {
        ctx.step()?;
        features.push(
            rb::Feature {
                tag: ttf::Tag::from_bytes(&feature.tag),
                value: feature.value,
                start: u32::try_from(feature.range.start).map_err(|_| Error::Overflow)?,
                end: u32::try_from(feature.range.end).map_err(|_| Error::Overflow)?,
            },
            ctx,
        )?;
    }
    let plan = rb::ShapePlan::new(
        &font.face,
        provider_direction,
        Some(script),
        Some(&language),
        &features,
        ctx,
    )?;
    let mut receipts = ChargedVec::with_capacity(style.features.len(), ctx)?;
    for feature in style.features {
        ctx.step()?;
        let available = plan.feature_available(ttf::Tag::from_bytes(&feature.tag), ctx)?;
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
    let mut buffer = rb::UnicodeBuffer::new();
    buffer.set_direction(provider_direction);
    buffer.set_script(script);
    buffer.set_language(language);
    buffer.set_cluster_level(rb::BufferClusterLevel::MonotoneGraphemes);
    buffer.set_flags(
        rb::BufferFlags::BEGINNING_OF_TEXT
            | rb::BufferFlags::END_OF_TEXT
            | rb::BufferFlags::REMOVE_DEFAULT_IGNORABLES,
    );
    for (offset, ch) in text.char_indices() {
        ctx.step()?;
        buffer.add(
            ch,
            u32::try_from(span.start + offset).map_err(|_| Error::Overflow)?,
            ctx,
        )?;
    }
    let shaped = rb::shape_with_plan(&font.face, &plan, buffer, ctx)?;
    counts.glyphs = counts
        .glyphs
        .checked_add(shaped.len() as u64)
        .ok_or(Error::Overflow)?;
    if counts.glyphs > request.limits.glyphs {
        return Err(Error::Budget);
    }
    let scale = u32::try_from(font.face.units_per_em()).map_err(|_| Error::CorruptFont)?;
    if scale == 0 || shaped.glyph_infos().len() != shaped.glyph_positions().len() {
        return Err(Error::CorruptFont);
    }
    let mut groups = ChargedVec::with_capacity(shaped.source_groups().len(), ctx)?;
    groups.extend_copy(shaped.source_groups(), ctx)?;
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
    let mut glyphs = ChargedVec::with_capacity(shaped.len(), ctx)?;
    for (info, position) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
        ctx.step()?;
        if info.glyph_id == 0 {
            return Err(Error::MissingGlyph);
        }
        let mut end = None;
        for (i, group) in groups.iter().enumerate() {
            ctx.step()?;
            if group.cluster == info.cluster {
                end = Some(
                    groups
                        .get(i + 1)
                        .map_or(span.end as u64, |v| u64::from(v.cluster)),
                );
                break;
            }
        }
        let end = end.ok_or(Error::InvalidMapping)?;
        glyphs.push(
            Glyph {
                glyph_id: info.glyph_id,
                cluster: SourceRange {
                    start: u64::from(info.cluster),
                    end,
                },
                x_advance_pt: points(position.x_advance, style.font_size_pt, scale)?,
                y_advance_pt: points(position.y_advance, style.font_size_pt, scale)?,
                x_offset_pt: points(position.x_offset, style.font_size_pt, scale)?,
                y_offset_pt: points(position.y_offset, style.font_size_pt, scale)?,
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
        direction,
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
        axes,
        features: receipts,
        glyphs,
    })
}
fn resolved_script(declared: [u8; 4], text: &str, ctx: &Context<'_>) -> ProviderResult<[u8; 4]> {
    use unicode_script::{Script, UnicodeScript};
    let mut selected = if declared == *b"Zyyy" {
        None
    } else {
        Some(declared)
    };
    for ch in text.chars() {
        // Exact pinned table: 2287 ranges, at most 13 binary-search comparisons; no allocation.
        ctx.units(13)?;
        let current = match ch.script() {
            Script::Latin => *b"Latn",
            Script::Arabic => *b"Arab",
            Script::Han => *b"Hani",
            Script::Common | Script::Inherited => continue,
            _ => return Err(Error::UnsupportedProfile),
        };
        if let Some(previous) = selected {
            if previous != current {
                return Err(Error::UnsupportedProfile);
            }
        } else {
            selected = Some(current);
        }
    }
    if let Some(script) = selected {
        return Ok(script);
    }
    // A standalone pinned default-ignorable has no strong script. Keep it in
    // the shaper so removal and original-byte coverage remain provider-derived.
    for ch in text.chars() {
        ctx.step()?;
        if !rb::is_default_ignorable(ch) {
            return Err(Error::UnsupportedProfile);
        }
    }
    Ok(*b"Zyyy")
}
fn points(value: i32, size: f64, scale: u32) -> ProviderResult<f64> {
    let product = f64::from(value) * size;
    let value = product / f64::from(scale);
    if !product.is_finite() || !value.is_finite() {
        return Err(Error::Overflow);
    }
    Ok(if value == 0.0 { 0.0 } else { value })
}
