//! Reflow and overset of imported threaded stories through `hsk-studio-layout`. Every story that
//! flows through a Folio frame chain is composed against the Folio proposal built from the IDML;
//! the result per thread is the layout placement (lines per frame) and the exact overset span, or
//! a typed layout refusal. A refusal on one thread never hides the others.
//!
//! Shaping is Type's capability (STU-ARC-007) and IDML names fonts, it does not carry them, so
//! this crate takes a [`ParagraphShaper`] port: the host adapts a Type shaper (resolving the IDML
//! style name to a font) and `ShapedParagraph::from_type`. [`FixedAdvanceShaper`] is a fixture, not
//! shaping.
//!
//! What is composed is exactly what layout composes: one column, top-aligned fixed-size frames,
//! left-to-right Latin stories. Anything else is a typed refusal passed through from layout
//! (`UnsupportedComposition`, ...); a story the importer flagged `reflow_certified == false`
//! (RTL, CJK, vertical, complex script) is refused here before any shaping.
//!
//! First baseline: layout composes only an explicit fixed first baseline measured from the frame
//! top, while InDesign defaults to the ascent offset. For `AscentOffset`, `LeadingOffset` and
//! `FixedHeight` the derived reflow document (the Folio proposal is untouched) sets the offset to
//! `inset_top + max(min_offset, metric)` with the metric taken from the story's first shaped
//! paragraph (ascent, line height, 0). That is reported as `first_baseline_mapped_to_fixed` and is
//! UNVERIFIED against InDesign; cap height, em box and x-height have no shaped metric and stay
//! refused by layout.
use crate::idml_decode::canceled;
use crate::model::IdmlImport;
use crate::{IdmlError, Result};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{FirstBaseline, GeometryUnit, Length, Payload, StudioTextStory};
use hsk_studio_layout::{
    Composer, LayoutError, LayoutProposal as Placement, Limits as ReflowLimits, ReflowRequest,
    ShapedParagraph, SourceSpan, SpaceParams, reflow,
};
use std::collections::BTreeMap;

/// One story paragraph to shape. `text` is the whole story text; `source` is the paragraph's byte
/// range in it without its terminating newline (empty for an empty paragraph).
#[derive(Clone, Copy, Debug)]
pub struct ShapeContext<'a> {
    pub story: &'a StudioTextStory,
    pub text: &'a str,
    pub paragraph_index: usize,
    pub source: SourceSpan,
    /// IDML name of the applied paragraph style (Folio holds only an unresolved reference).
    pub paragraph_style: Option<&'a str>,
}

/// Host port: shape one paragraph into layout's input. A font that cannot be resolved is
/// `LayoutError::MissingFont`.
pub trait ParagraphShaper {
    fn shape(&self, ctx: &ShapeContext<'_>) -> std::result::Result<ShapedParagraph, LayoutError>;
}

/// Fixture shaper, NOT shaping: one glyph per `char` with a fixed advance and fixed metrics.
#[derive(Clone, Copy, Debug)]
pub struct FixedAdvanceShaper {
    pub advance_pt: f64,
    pub line_height_pt: f64,
    pub ascent_pt: f64,
}
impl ParagraphShaper for FixedAdvanceShaper {
    fn shape(&self, ctx: &ShapeContext<'_>) -> std::result::Result<ShapedParagraph, LayoutError> {
        ShapedParagraph::from_fixed_advance(
            ctx.text,
            ctx.source,
            self.advance_pt,
            self.line_height_pt,
            self.ascent_pt,
        )
        .ok_or(LayoutError::InvalidInput)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ReflowOptions {
    pub composer: Composer,
    pub space: SpaceParams,
    pub limits: ReflowLimits,
}
impl Default for ReflowOptions {
    fn default() -> Self {
        Self {
            composer: Composer::FirstFit,
            space: SpaceParams::default(),
            limits: ReflowLimits::default(),
        }
    }
}

/// The outcome for one story's frame chain.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadReflow {
    /// Folio `STXT` id.
    pub story_id: String,
    /// IDML `Self` of the story.
    pub idml_story_id: String,
    /// Folio frame layer ids in flow order.
    pub frame_ids: Vec<String>,
    pub outcome: std::result::Result<Placement, LayoutError>,
    /// Approximations made to compose, for example `first_baseline_mapped_to_fixed`.
    pub notes: Vec<&'static str>,
}
impl ThreadReflow {
    /// First unplaced story byte through the end of the story; `None` when everything fits or the
    /// thread was refused.
    pub fn overset(&self) -> Option<SourceSpan> {
        self.outcome.as_ref().ok().and_then(|p| p.overset)
    }
    /// Stable code of the refusal, if any.
    pub fn refusal(&self) -> Option<&'static str> {
        self.outcome.as_ref().err().map(|e| e.code())
    }
}

/// Reflows every story of the import that has a frame chain, in story order. Stories without a
/// frame are not threads and are omitted. Only cancellation aborts the call.
pub fn reflow_import(
    import: &IdmlImport,
    shaper: &dyn ParagraphShaper,
    options: &ReflowOptions,
    cancel: &CancellationToken,
) -> Result<Vec<ThreadReflow>> {
    canceled(cancel)?;
    let document = import.proposal.snapshot.document();
    let threads = document.text_threads();
    let styles: BTreeMap<&str, &str> = import
        .proposal
        .style_refs
        .iter()
        .map(|s| (s.folio.as_str(), s.idml_name.as_str()))
        .collect();
    let mut out = Vec::new();
    for ps in &import.proposal.stories {
        canceled(cancel)?;
        let Some(thread) = threads.iter().find(|t| t.story_id() == ps.story.story_id) else {
            continue;
        };
        let mut notes = Vec::new();
        let outcome = (|| {
            if !ps.reflow_certified {
                return Err(LayoutError::UnsupportedComposition);
            }
            let text = ps.story.inline_text().ok_or(LayoutError::InvalidInput)?;
            let mut shaped = Vec::with_capacity(ps.story.paragraphs.len());
            for (i, p) in ps.story.paragraphs.iter().enumerate() {
                if cancel.check().is_err() {
                    return Err(LayoutError::Canceled);
                }
                let (start, mut end) = (p.start, p.end);
                if end > start && text.as_bytes().get(end as usize - 1) == Some(&b'\n') {
                    end -= 1;
                }
                shaped.push(shaper.shape(&ShapeContext {
                    story: &ps.story,
                    text,
                    paragraph_index: i,
                    source: SourceSpan { start, end },
                    paragraph_style: p.style_ref.as_deref().and_then(|s| styles.get(s).copied()),
                })?);
            }
            let mut derived = document.clone();
            if let Some(first) = shaped.first() {
                for layer in &mut derived.layers {
                    let Payload::Text { story_id } = &layer.payload else {
                        continue;
                    };
                    let (true, Some(f)) =
                        (*story_id == ps.story.story_id, layer.text_frame.as_mut())
                    else {
                        continue;
                    };
                    let metric = match f.first_baseline_offset {
                        FirstBaseline::Ascent => first.ascent_pt,
                        FirstBaseline::Leading => first.line_height_pt,
                        FirstBaseline::Fixed => 0.0,
                        _ => continue,
                    };
                    f.minimum_first_baseline_offset = Length {
                        value: f.inset_spacing.top.value
                            + f.minimum_first_baseline_offset.value.max(metric),
                        unit: GeometryUnit::Pt,
                    };
                    f.first_baseline_offset = FirstBaseline::Fixed;
                    if !notes.contains(&"first_baseline_mapped_to_fixed") {
                        notes.push("first_baseline_mapped_to_fixed");
                    }
                }
            }
            reflow(
                &ReflowRequest {
                    document: &derived,
                    story: &ps.story,
                    resolved_text: None,
                    paragraphs: &shaped,
                    obstacles: &[],
                    composer: options.composer,
                    space: options.space,
                    expected_revision: derived.revision,
                    limits: options.limits,
                },
                cancel,
            )
        })();
        if outcome == Err(LayoutError::Canceled) {
            return Err(IdmlError::Canceled);
        }
        out.push(ThreadReflow {
            story_id: ps.story.story_id.clone(),
            idml_story_id: ps.idml_id.clone(),
            frame_ids: thread.frames().to_vec(),
            outcome,
            notes,
        });
    }
    Ok(out)
}
