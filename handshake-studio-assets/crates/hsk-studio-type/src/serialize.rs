use crate::provider::{Context, ProviderResult, storage::ChargedVec};
use crate::*;
struct Writer<'a, 'c> {
    count: u64,
    values: Option<&'c mut ChargedVec<'a, u8>>,
    ctx: &'c Context<'a>,
}
impl Writer<'_, '_> {
    fn bytes(&mut self, bytes: &[u8]) -> ProviderResult<()> {
        self.count = self
            .count
            .checked_add(bytes.len() as u64)
            .ok_or(Error::Overflow)?;
        self.ctx.units(bytes.len() as u64)?;
        if let Some(values) = self.values.as_mut() {
            values.extend_copy(bytes, self.ctx)?;
        }
        Ok(())
    }
    fn u64(&mut self, v: u64) -> ProviderResult<()> {
        self.bytes(&v.to_le_bytes())
    }
    fn u32(&mut self, v: u32) -> ProviderResult<()> {
        self.bytes(&v.to_le_bytes())
    }
    fn byte(&mut self, v: u8) -> ProviderResult<()> {
        self.bytes(&[v])
    }
    fn string(&mut self, v: &str) -> ProviderResult<()> {
        self.u64(v.len() as u64)?;
        self.bytes(v.as_bytes())
    }
    fn number(&mut self, v: f64) -> ProviderResult<()> {
        if !v.is_finite() {
            return Err(Error::Overflow);
        }
        self.u64(if v == 0.0 { 0 } else { v.to_bits() })
    }
    fn range(&mut self, v: SourceRange) -> ProviderResult<()> {
        self.u64(v.start)?;
        self.u64(v.end)
    }
    fn direction(&mut self, v: Direction) -> ProviderResult<()> {
        self.byte(match v {
            Direction::LeftToRight => 0,
            Direction::RightToLeft => 1,
        })
    }
    fn read(&mut self, v: ReadAddress<'_>) -> ProviderResult<()> {
        self.string(v.owner.as_str())?;
        self.string(v.property)?;
        self.u64(v.expected_revision)?;
        self.bytes(&v.fingerprint)
    }
}
pub(crate) fn encode<'a>(
    request: &Request<'a>,
    paragraphs: &[ParagraphResult<'a>],
    coverage: &[Coverage],
    disposition: TextDisposition,
    ctx: &Context<'a>,
) -> ProviderResult<ChargedVec<'a, u8>> {
    let mut count = Writer {
        count: 0,
        values: None,
        ctx,
    };
    write(request, paragraphs, coverage, disposition, &mut count)?;
    let length = usize::try_from(count.count).map_err(|_| Error::Overflow)?;
    let mut result = ChargedVec::with_capacity(length, ctx)?;
    let mut output = Writer {
        count: 0,
        values: Some(&mut result),
        ctx,
    };
    write(request, paragraphs, coverage, disposition, &mut output)?;
    if output.count != length as u64 {
        return Err(Error::InternalInvariant);
    }
    ctx.poll()?;
    Ok(result)
}
fn write(
    request: &Request<'_>,
    paragraphs: &[ParagraphResult<'_>],
    coverage: &[Coverage],
    disposition: TextDisposition,
    w: &mut Writer<'_, '_>,
) -> ProviderResult<()> {
    w.bytes(b"HSKTYPE\0\x01")?;
    w.string(request.composition_version)?;
    w.string(request.document_id.as_str())?;
    w.u64(request.document_revision)?;
    w.string(request.story_id.as_str())?;
    w.string(request.layer_id.as_str())?;
    w.bytes(&request.text_sha256)?;
    w.u64(request.text_utf8.len() as u64)?;
    w.u64(request.resolver_revision)?;
    w.u64(request.cancel_epoch)?;
    w.string(request.correlation_id)?;
    w.u64(request.observation_correlation)?;
    w.byte(match request.story_direction {
        StoryDirection::LeftToRight => 0,
        StoryDirection::RightToLeft => 1,
        StoryDirection::Unknown => 2,
    })?;
    w.u64(request.reads.len() as u64)?;
    for read in request.reads {
        w.ctx.step()?;
        w.read(*read)?;
    }
    w.read(request.result_target.address)?;
    w.byte(match disposition {
        TextDisposition::Shaped => 0,
        TextDisposition::EmptyInput => 1,
        TextDisposition::ControlOnly => 2,
    })?;
    w.u64(paragraphs.len() as u64)?;
    for para in paragraphs {
        w.ctx.step()?;
        w.range(para.source)?;
        w.direction(para.direction)?;
        w.u64(para.runs.len() as u64)?;
        for run in para.runs.iter() {
            w.ctx.step()?;
            w.range(run.source)?;
            w.byte(run.bidi_level)?;
            w.direction(run.direction)?;
            w.bytes(&run.script)?;
            w.string(run.language)?;
            w.number(run.font_size_pt)?;
            w.string(run.requested_identity)?;
            w.string(run.resolved.identity)?;
            w.string(run.resolved.location)?;
            w.bytes(&run.resolved.content_hash)?;
            w.u32(run.resolved.face_index)?;
            w.u64(run.resolved.resource_revision)?;
            w.u64(run.resolved.grant_revision)?;
            w.u32(run.candidate_index)?;
            w.byte(match run.substitution {
                None => 0,
                Some(SubstitutionReason::MissingRequested) => 1,
                Some(SubstitutionReason::UnavailableRequested) => 2,
                Some(SubstitutionReason::CoverageFallback) => 3,
            })?;
            w.u32(run.provider_scale)?;
            w.u32(run.units_per_em)?;
            w.u64(run.axes.len() as u64)?;
            for axis in run.axes.iter() {
                w.ctx.step()?;
                w.bytes(&axis.axis.tag)?;
                w.u32(if axis.axis.value == 0.0 {
                    0
                } else {
                    axis.axis.value.to_bits()
                })?;
                w.byte(u8::from(axis.defaulted))?;
            }
            w.u64(run.features.len() as u64)?;
            for feature in run.features.iter() {
                w.ctx.step()?;
                w.bytes(&feature.feature.tag)?;
                w.u32(feature.feature.value)?;
                w.range(feature.feature.range)?;
                w.byte(u8::from(feature.feature.declared))?;
                w.byte(match feature.state {
                    FeatureState::Applied => 0,
                    FeatureState::UnavailableInert => 1,
                })?;
            }
            w.u64(run.glyphs.len() as u64)?;
            for glyph in run.glyphs.iter() {
                w.ctx.step()?;
                w.u32(glyph.glyph_id)?;
                w.range(glyph.cluster)?;
                w.number(glyph.x_advance_pt)?;
                w.number(glyph.y_advance_pt)?;
                w.number(glyph.x_offset_pt)?;
                w.number(glyph.y_offset_pt)?;
            }
        }
    }
    w.u64(coverage.len() as u64)?;
    for entry in coverage {
        w.ctx.step()?;
        w.range(entry.source)?;
        w.byte(match entry.kind {
            CoverageKind::ShapedCluster => 0,
            CoverageKind::ParagraphSeparator => 1,
            CoverageKind::NonrenderingControl => 2,
        })?;
        match entry.glyphs {
            None => w.byte(0)?,
            Some(g) => {
                w.byte(1)?;
                w.u32(g.paragraph)?;
                w.u32(g.run)?;
                w.u32(g.start)?;
                w.u32(g.end)?;
            }
        }
    }
    Ok(())
}
