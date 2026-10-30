//! Live path text and area text (lane SMB-AI-AREA-TYPE-1; STU-TYP-002, STU-TYP-114, STU-TYP-116,
//! STU-TYP-120/121, STU-VEC-038). Folio `StudioPathText` and `StudioAreaText` records bind a story
//! to a path of a vector layer; this module draws that story every time the vector records are
//! prepared or expanded, from the story as it is NOW: nothing is baked into the records, so editing
//! the story or the path redraws the text.
//!
//! The caller shapes the story ([`VectorAssets::shaped_story`], like `crate::text`: Type's shaping
//! engine needs the caller's ports and fonts) and returns a [`ShapedStory`]: glyph advances, break
//! classes, source byte offsets and outlines (font units), the run metrics and font sizes, and the
//! story's paint rows (one fill paint per row, each glyph naming its row; resolved from the story's
//! character styles by the caller, Folio stores no story paint yet). Type lays the story out
//! (`layout_path_text` along the baseline path with the record's options and per-run baseline
//! offsets; `layout_area_text` inside the container path with the record's inset, first baseline
//! and auto-size, the story's alignment) and outlines the placed glyphs (`create_outlines`). Points
//! are document units (1 pt = 1 plan px, as the viewer's text tools; a declared reading).
//!
//! Each paint row that has placed glyphs becomes one own-geometry row
//! ([`FillPaint::StrokeGeometry`]: its outline is the glyph compound paths of that row, painted
//! Normal at full opacity) appended to the layer that holds the path, so the text paints above the
//! path's own fills and strokes in the existing vector route of both backends (no new operator).
//! Rows paint in row order (overlapping glyphs of different rows: the later row is on top). A text
//! row ignores the layer's envelope (declared gap). Overset glyphs are not drawn and are reported
//! ([`TextReport`]); an area text's auto-sized container is reported, not written back (the host
//! commits it).
//!
//! Refusals: a story the caller does not provide ([`TEXT_NOT_PROVIDED`]) is reported and the layer
//! draws without it (hosts that shape no live text keep their output); a story of the wrong
//! `text_kind` ([`REFUSE_TEXT_KIND`]), a malformed shaped story ([`REFUSE_TEXT_STORY`]), an
//! unresolved path and every Type refusal ([`REFUSE_TEXT_LAYOUT`]) refuse the layer, typed.
//!
//! [`create_text_outlines`] (Create Outlines, STU-VEC-038) returns the same geometry as a proposal:
//! plain filled shapes per paint row, which the host commits as one step (a vector layer replacing
//! the text binding).
use crate::vector::{ExpandedRow, FillPaint, FillSpec, OwnedPath, PathElement, PathPoint, VectorAssets, REFUSE_PATH_UNRESOLVED};
use hsk_studio_folio::{StoryTextKind, StudioAreaText, StudioPathText, StudioVectorRecords};
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_type as ty;

/// The host does not shape live text: the layer draws without it (reported).
pub const TEXT_NOT_PROVIDED: &str = "vector_text_not_provided";
/// The story's `text_kind` is not the binding's (`path` for path text, `area` for area text).
pub const REFUSE_TEXT_KIND: &str = "vector_text_kind";
/// The shaped story is malformed (counts, paint indices, non-finite values).
pub const REFUSE_TEXT_STORY: &str = "vector_text_story";
/// Type refused the layout or the outlines.
pub const REFUSE_TEXT_LAYOUT: &str = "vector_text_layout";

/// Most glyphs of one shaped story.
pub const MAX_TEXT_GLYPHS: usize = 1 << 20;

/// One run of a shaped story: its line metrics (`glyph_count` included) and font size.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapedRun {
    pub metrics: ty::ComposeRun,
    pub font_size_pt: f64,
}

/// One shaped glyph in logical order.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapedGlyph {
    pub advance_pt: f64,
    pub brk: ty::GlyphBreak,
    /// Original-UTF-8 byte offset of the glyph's cluster in the story.
    pub cluster: u64,
    /// Outline in font units (empty for spaces).
    pub outline: ty::GlyphOutline,
    /// Index into [`ShapedStory::paints`].
    pub paint: usize,
}

/// A story shaped by the caller (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct ShapedStory {
    pub text_kind: StoryTextKind,
    pub runs: Vec<ShapedRun>,
    pub glyphs: Vec<ShapedGlyph>,
    /// The story's paint rows (solid, gradient or pattern).
    pub paints: Vec<FillPaint>,
    /// Paragraph alignment of area text.
    pub align: ty::TextAlign,
}

/// What was drawn of one binding.
#[derive(Clone, Debug, PartialEq)]
pub struct TextDrawn {
    pub placed: usize,
    pub overset_from: Option<usize>,
    pub overset_count: usize,
    /// Area text: the container box the text was set in (`[x0, y0, x1, y1]`, after auto-size).
    pub container: Option<[f64; 4]>,
    pub resized: bool,
}

/// The outcome of one path- or area-text binding.
#[derive(Clone, Debug, PartialEq)]
pub struct TextReport {
    /// The story id (`STXT`).
    pub text_id: String,
    /// The vector layer holding the path.
    pub layer_id: String,
    pub outcome: Result<TextDrawn, &'static str>,
}

fn baseline(path: &OwnedPath) -> Vec<ty::BaselineCommand> {
    let p = |q: PathPoint| [q.x, q.y];
    path.elements
        .iter()
        .map(|e| match *e {
            PathElement::MoveTo(a) => ty::BaselineCommand::MoveTo(p(a)),
            PathElement::LineTo(a) => ty::BaselineCommand::LineTo(p(a)),
            PathElement::QuadTo(a, b) => ty::BaselineCommand::QuadTo(p(a), p(b)),
            PathElement::CubicTo(a, b, c) => ty::BaselineCommand::CubicTo(p(a), p(b), p(c)),
            PathElement::Close => ty::BaselineCommand::Close,
        })
        .collect()
}

fn owned(commands: &[ty::BaselineCommand]) -> OwnedPath {
    let q = |a: [f64; 2]| PathPoint::new(a[0], a[1]);
    OwnedPath {
        elements: commands
            .iter()
            .map(|c| match *c {
                ty::BaselineCommand::MoveTo(a) => PathElement::MoveTo(q(a)),
                ty::BaselineCommand::LineTo(a) => PathElement::LineTo(q(a)),
                ty::BaselineCommand::QuadTo(a, b) => PathElement::QuadTo(q(a), q(b)),
                ty::BaselineCommand::CubicTo(a, b, d) => PathElement::CubicTo(q(a), q(b), q(d)),
                ty::BaselineCommand::Close => PathElement::Close,
            })
            .collect(),
        fill_rule: hsk_studio_nib::FillRule::NonZero,
    }
}

/// Checks the shaped story; returns the story run of every glyph.
fn check_story(s: &ShapedStory, kind: StoryTextKind) -> Result<Vec<usize>, &'static str> {
    if s.text_kind != kind {
        return Err(REFUSE_TEXT_KIND);
    }
    if s.glyphs.len() > MAX_TEXT_GLYPHS {
        return Err(REFUSE_TEXT_STORY);
    }
    let counted = s.runs.iter().try_fold(0_usize, |a, r| a.checked_add(r.metrics.glyph_count)).ok_or(REFUSE_TEXT_STORY)?;
    if counted != s.glyphs.len() || s.runs.iter().any(|r| !(r.font_size_pt.is_finite() && r.font_size_pt > 0.0)) {
        return Err(REFUSE_TEXT_STORY);
    }
    if s.glyphs.iter().any(|g| g.paint >= s.paints.len()) {
        return Err(REFUSE_TEXT_STORY);
    }
    Ok(s.runs.iter().enumerate().flat_map(|(i, r)| std::iter::repeat_n(i, r.metrics.glyph_count)).collect())
}

/// Outlined glyphs of a placed layout, grouped per paint row (row order), with the row's paint.
fn rows_of(s: &ShapedStory, run_of: &[usize], layout: &dyn ty::ComposedGlyphs) -> Result<Vec<(FillPaint, Vec<OwnedPath>)>, &'static str> {
    let sources: Vec<ty::OutlineSource<'_>> = s
        .glyphs
        .iter()
        .zip(run_of)
        .map(|(g, r)| ty::OutlineSource { outline: &g.outline, font_size_pt: s.runs[*r].font_size_pt, paint: g.paint })
        .collect();
    let outlined = ty::create_outlines(layout, &sources, &ty::CreateOutlinesLimits::default()).map_err(|_| REFUSE_TEXT_LAYOUT)?;
    let mut rows: Vec<(usize, Vec<OwnedPath>)> = Vec::new();
    for g in outlined {
        match rows.iter_mut().find(|(p, _)| *p == g.paint) {
            Some((_, paths)) => paths.push(owned(&g.commands)),
            None => rows.push((g.paint, vec![owned(&g.commands)])),
        }
    }
    rows.sort_by_key(|(p, _)| *p);
    rows.into_iter().map(|(p, paths)| Ok((s.paints.get(p).cloned().ok_or(REFUSE_TEXT_STORY)?, paths))).collect()
}

fn path_options(p: &StudioPathText) -> ty::PathTextOptions {
    use hsk_studio_folio::{
        StudioPathEffect as E, StudioPathFlowDirection as F, StudioPathTextAlign as A, StudioPathTextFlip as P,
        StudioPathTypeAlignment as PA, StudioTextTypeAlignment as TA,
    };
    ty::PathTextOptions {
        start_t: Some(p.start_t),
        end_t: Some(p.end_t),
        spacing_pt: p.spacing,
        flipped: match p.flipped {
            P::NotFlipped => ty::PathFlip::NotFlipped,
            P::Flipped => ty::PathFlip::Flipped,
            P::Undefined => ty::PathFlip::Undefined,
        },
        effect: match p.path_effect {
            E::Rainbow => ty::PathEffect::Rainbow,
            E::Skew => ty::PathEffect::Skew,
            E::Ribbon => ty::PathEffect::Ribbon,
            E::StairStep => ty::PathEffect::StairStep,
            E::Gravity => ty::PathEffect::Gravity,
        },
        path_alignment: match p.path_type_alignment {
            PA::TopPath => ty::PathTypeAlignment::TopPath,
            PA::BottomPath => ty::PathTypeAlignment::BottomPath,
            PA::CenterPath => ty::PathTypeAlignment::CenterPath,
        },
        text_alignment: match p.text_type_alignment {
            TA::Ascender => ty::TextTypeAlignment::Ascender,
            TA::Descender => ty::TextTypeAlignment::Descender,
            TA::Center => ty::TextTypeAlignment::Center,
            TA::Baseline => ty::TextTypeAlignment::Baseline,
            TA::AboveRightEmBox => ty::TextTypeAlignment::AboveRightEmBox,
            TA::BelowLeftEmBox => ty::TextTypeAlignment::BelowLeftEmBox,
            TA::AboveRightIcfBox => ty::TextTypeAlignment::AboveRightIcfBox,
            TA::BelowLeftIcfBox => ty::TextTypeAlignment::BelowLeftIcfBox,
        },
        flow_direction: match p.flow_direction {
            F::LeftToRight => ty::PathFlowDirection::LeftToRight,
            F::RightToLeft => ty::PathFlowDirection::RightToLeft,
        },
        align: match p.align {
            A::Start => ty::PathTextAlign::Start,
            A::Center => ty::PathTextAlign::Center,
            A::End => ty::PathTextAlign::End,
        },
        // The stroke width the text sits on is the layer's style; not resolved here (declared gap:
        // top/bottom path alignment measures from the centre line).
        stroke_width_pt: 0.0,
    }
}

/// One drawn binding: its paint rows (paint, glyph paths) and the report.
type Drawn = (Vec<(FillPaint, Vec<OwnedPath>)>, TextDrawn);

fn draw_path_text(p: &StudioPathText, s: &ShapedStory, assets: &dyn VectorAssets) -> Result<Drawn, &'static str> {
    let run_of = check_story(s, StoryTextKind::Path)?;
    let path = assets.path(&p.path_id).ok_or(REFUSE_PATH_UNRESOLVED)?;
    // Path runs: consecutive glyphs of one story run and one baseline offset.
    let offset_of = |cluster: u64| p.baseline_offsets.iter().find(|o| o.start <= cluster && cluster < o.end).map_or(0.0, |o| o.offset_pt);
    let mut runs: Vec<(usize, f64, usize)> = Vec::new(); // (story run, offset, count)
    for (g, r) in s.glyphs.iter().zip(&run_of) {
        let off = offset_of(g.cluster);
        match runs.last_mut() {
            Some((lr, lo, n)) if *lr == *r && *lo == off => *n = n.saturating_add(1),
            _ => runs.push((*r, off, 1)),
        }
    }
    let path_runs: Vec<ty::PathTextRun> = runs
        .iter()
        .map(|(r, off, n)| {
            let m = &s.runs[*r].metrics;
            ty::PathTextRun { glyph_count: *n, baseline_offset_pt: *off, ascent_pt: m.ascent_pt, descent_pt: m.descent_pt, em_top_pt: m.ascent_pt, em_bottom_pt: -m.descent_pt, icf_top_pt: m.ascent_pt, icf_bottom_pt: -m.descent_pt }
        })
        .collect();
    let advances: Vec<f64> = s.glyphs.iter().map(|g| g.advance_pt).collect();
    let layout = ty::layout_path_text(&baseline(&path), &path_runs, &advances, &path_options(p), &ty::PathTextLimits::default()).map_err(|_| REFUSE_TEXT_LAYOUT)?;
    let rows = rows_of(s, &run_of, &layout)?;
    let drawn = TextDrawn { placed: layout.glyphs.len(), overset_from: layout.overset_from, overset_count: layout.overset_count, container: None, resized: false };
    Ok((rows, drawn))
}

fn draw_area_text(a: &StudioAreaText, s: &ShapedStory, assets: &dyn VectorAssets) -> Result<Drawn, &'static str> {
    use hsk_studio_folio::{StudioAutoSize as S, StudioFirstBaseline as B};
    let run_of = check_story(s, StoryTextKind::Area)?;
    let path = assets.path(&a.container_id).ok_or(REFUSE_PATH_UNRESOLVED)?;
    let options = ty::AreaTextOptions {
        inset_pt: a.inset,
        first_baseline: match a.first_baseline {
            B::Ascent => ty::FirstBaseline::Ascent,
            B::CapHeight => ty::FirstBaseline::CapHeight,
            B::XHeight => ty::FirstBaseline::XHeight,
            B::Leading => ty::FirstBaseline::Leading,
            B::Fixed => ty::FirstBaseline::Fixed,
        },
        first_baseline_min_pt: a.first_baseline_min,
        auto_size: match a.auto_size {
            S::Off => ty::AutoSize::Off,
            S::HeightOnly => ty::AutoSize::HeightOnly,
            S::WidthOnly => ty::AutoSize::WidthOnly,
            S::HeightAndWidth => ty::AutoSize::HeightAndWidth,
            S::HeightAndWidthProportional => ty::AutoSize::HeightAndWidthProportional,
        },
        align: s.align,
        ..ty::AreaTextOptions::default()
    };
    let runs: Vec<ty::ComposeRun> = s.runs.iter().map(|r| r.metrics).collect();
    let glyphs: Vec<ty::ComposeGlyph> = s.glyphs.iter().map(|g| ty::ComposeGlyph { advance_pt: g.advance_pt, brk: g.brk }).collect();
    let layout = ty::layout_area_text(&baseline(&path), &runs, &glyphs, &options, &ty::AreaTextLimits::default()).map_err(|_| REFUSE_TEXT_LAYOUT)?;
    let rows = rows_of(s, &run_of, &layout)?;
    let drawn = TextDrawn { placed: layout.glyphs.len(), overset_from: layout.overset_from, overset_count: layout.overset_count, container: Some(layout.container), resized: layout.resized };
    Ok((rows, drawn))
}

/// One binding drawn: `Ok(None)` when the caller provides no story.
fn draw(text_id: &str, records: &StudioVectorRecords, assets: &dyn VectorAssets) -> Result<Option<Drawn>, &'static str> {
    let Some(story) = assets.shaped_story(text_id) else {
        return Ok(None);
    };
    if let Some(p) = records.path_texts.iter().find(|p| p.story_id == text_id) {
        return draw_path_text(p, &story, assets).map(Some);
    }
    match records.area_texts.iter().find(|a| a.text_id == text_id) {
        Some(a) => draw_area_text(a, &story, assets).map(Some),
        None => Err(REFUSE_TEXT_STORY),
    }
}

/// The story ids bound to paths of `layer_id` (path texts first, then area texts; record order).
fn bound(records: &StudioVectorRecords, layer_id: &str) -> Vec<String> {
    records
        .path_texts
        .iter()
        .filter(|p| p.path_layer_id == layer_id)
        .map(|p| p.story_id.clone())
        .chain(records.area_texts.iter().filter(|a| a.container_layer_id == layer_id).map(|a| a.text_id.clone()))
        .collect()
}

/// The live text rows of `layer_id` (module docs), appended after its own rows; every binding is
/// reported into `reports`. `Err(code)` refuses the layer.
pub(crate) fn text_rows(records: &StudioVectorRecords, layer_id: &str, assets: &dyn VectorAssets, reports: &mut Vec<TextReport>) -> Result<Vec<FillSpec>, &'static str> {
    let mut out = Vec::new();
    for text_id in bound(records, layer_id) {
        let report = |outcome| TextReport { text_id: text_id.clone(), layer_id: layer_id.to_owned(), outcome };
        match draw(&text_id, records, assets) {
            Ok(None) => reports.push(report(Err(TEXT_NOT_PROVIDED))),
            Ok(Some((rows, drawn))) => {
                for (paint, outline) in rows {
                    out.push(FillSpec { paint: FillPaint::StrokeGeometry { outline, paint: Box::new(paint) }, mode: StudioBlendMode::Normal, opacity: 1.0 });
                }
                reports.push(report(Ok(drawn)));
            }
            Err(code) => {
                reports.push(report(Err(code)));
                return Err(code);
            }
        }
    }
    Ok(out)
}

/// Create Outlines of the path- or area-text binding of story `text_id` (module docs): the layer
/// holding its path, one filled row per paint row (glyph compound paths, non-zero) and what was
/// drawn (overset glyphs are not outlined; they are reported). Typed refusals as for live text;
/// [`TEXT_NOT_PROVIDED`] when the caller shapes no story, [`REFUSE_TEXT_STORY`] for an unbound id.
pub fn create_text_outlines(records: &StudioVectorRecords, text_id: &str, assets: &dyn VectorAssets) -> Result<(String, Vec<ExpandedRow>, TextDrawn), &'static str> {
    let layer = records
        .path_texts
        .iter()
        .find(|p| p.story_id == text_id)
        .map(|p| p.path_layer_id.clone())
        .or_else(|| records.area_texts.iter().find(|a| a.text_id == text_id).map(|a| a.container_layer_id.clone()))
        .ok_or(REFUSE_TEXT_STORY)?;
    let (rows, drawn) = draw(text_id, records, assets)?.ok_or(TEXT_NOT_PROVIDED)?;
    let out = rows.into_iter().map(|(paint, paths)| ExpandedRow { paths, paint, mode: StudioBlendMode::Normal, opacity: 1.0 }).collect();
    Ok((layer, out, drawn))
}
