//! Paragraph and character styles from Folio's type style records (`StudioTypeStyleRecords`,
//! STU-TYP-200..203) applied to composition. `based_on` inheritance is resolved parent first (a
//! child field overrides, an absent field inherits); a cycle, a missing style, a `based_on` of
//! another scope or a chain longer than the set is refused as `InvalidInput`.
//!
//! What composition consumes: paragraph `align`, `line_height` and `letter_spacing`, and the
//! character-range `letter_spacing` (a range style overrides the paragraph's). Font family, style,
//! size and fill are resolved and returned for the caller's shaper and painter (shaping is Type's);
//! layout uses `font_size` only to turn percent line height and letter spacing into points.
//! Conventions, UNVERIFIED against InDesign/Figma: `Percent` is a number where 100 means 100% of
//! the font size; `px` = 0.75 pt (96 dpi); a character range's size is not used for line height.
use crate::{LayoutError, ShapedParagraph};
use hsk_studio_folio::{
    GeometryUnit, Length, StudioLineHeight, StudioLetterSpacing, StudioRgba, StudioTextAlign,
    StudioTextStory, StudioTypeStyle, StudioTypeStyleRecords, TypeStyleScope,
};
use std::collections::BTreeMap;

/// Horizontal paragraph alignment composed by `reflow_typeset`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Alignment {
    #[default]
    Left,
    Center,
    Right,
    /// Every line but the last fills the slot (last line left-aligned).
    Justified,
    /// Every line including the last fills the slot (spec justify.fully_justified; InDesign "Justify
    /// all lines", IDML `FullyJustified`).
    JustifiedAll,
}
impl From<StudioTextAlign> for Alignment {
    fn from(a: StudioTextAlign) -> Self {
        match a {
            StudioTextAlign::Left => Self::Left,
            StudioTextAlign::Center => Self::Center,
            StudioTextAlign::Right => Self::Right,
            StudioTextAlign::Justified => Self::Justified,
        }
    }
}

/// A style with its `based_on` chain folded in. Lengths are points.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ResolvedStyle {
    pub font_family: Option<String>,
    pub font_style: Option<String>,
    pub font_size_pt: Option<f64>,
    pub fill: Option<StudioRgba>,
    pub line_height: Option<StudioLineHeight>,
    pub letter_spacing: Option<StudioLetterSpacing>,
    pub align: Option<StudioTextAlign>,
}

/// Points per unit; `px` is 0.75 pt (UNVERIFIED convention).
pub fn length_pt(l: &Length) -> Result<f64, LayoutError> {
    let k = match l.unit {
        GeometryUnit::Pt => 1.0,
        GeometryUnit::In => 72.0,
        GeometryUnit::Mm => 72.0 / 25.4,
        GeometryUnit::Px => 0.75,
    };
    let v = l.value * k;
    if l.value.is_finite() && v.is_finite() {
        Ok(v)
    } else {
        Err(LayoutError::InvalidGeometry)
    }
}

/// Folds the `based_on` chain of `style_id` (which must have `scope`).
pub fn resolve_style(
    records: &StudioTypeStyleRecords,
    style_id: &str,
    scope: TypeStyleScope,
) -> Result<ResolvedStyle, LayoutError> {
    let mut chain: Vec<&StudioTypeStyle> = Vec::new();
    let mut at = Some(style_id);
    while let Some(id) = at {
        if chain.len() > records.styles.len() {
            return Err(LayoutError::InvalidInput);
        }
        let s = records.style(id).ok_or(LayoutError::InvalidInput)?;
        if s.scope != scope || chain.iter().any(|c| c.style_id == s.style_id) {
            return Err(LayoutError::InvalidInput);
        }
        chain.push(s);
        at = s.based_on.as_deref();
    }
    let mut r = ResolvedStyle::default();
    for s in chain.iter().rev() {
        if let Some(v) = &s.font_family {
            r.font_family = Some(v.clone());
        }
        if let Some(v) = &s.font_style {
            r.font_style = Some(v.clone());
        }
        if let Some(v) = &s.font_size {
            let pt = length_pt(v)?;
            if pt <= 0.0 {
                return Err(LayoutError::InvalidGeometry);
            }
            r.font_size_pt = Some(pt);
        }
        if let Some(v) = &s.fill {
            r.fill = Some(v.clone());
        }
        if let Some(v) = &s.line_height {
            r.line_height = Some(v.clone());
        }
        if let Some(v) = &s.letter_spacing {
            r.letter_spacing = Some(v.clone());
        }
        if let Some(v) = s.align {
            r.align = Some(v);
        }
    }
    Ok(r)
}

/// Composition inputs for one paragraph after style resolution.
#[derive(Clone, Debug, PartialEq)]
pub struct ParagraphStyling {
    pub alignment: Alignment,
    pub font_size_pt: f64,
    /// Resolved paragraph style (for the caller's shaper/painter); `None` when unstyled.
    pub style: Option<ResolvedStyle>,
}

/// Where styles come from.
#[derive(Clone, Copy, Debug)]
pub struct StyleSource<'a> {
    pub records: &'a StudioTypeStyleRecords,
    /// Font size when the paragraph style states none.
    pub default_font_size_pt: f64,
}

fn spacing_pt(ls: &Option<StudioLetterSpacing>, size: f64) -> Result<f64, LayoutError> {
    let v = match ls {
        None => 0.0,
        Some(StudioLetterSpacing::Percent { value }) => size * value / 100.0,
        Some(StudioLetterSpacing::Length { length }) => length_pt(length)?,
    };
    if v.is_finite() {
        Ok(v)
    } else {
        Err(LayoutError::InvalidGeometry)
    }
}

/// Applies styles: returns the paragraphs with letter spacing added to every glyph advance and the
/// line height overridden, plus the per-paragraph styling. `fallback` is the alignment of
/// paragraphs whose style states none.
pub(crate) fn apply(
    src: StyleSource<'_>,
    story: &StudioTextStory,
    paragraphs: &[ShapedParagraph],
    fallback: Alignment,
) -> Result<(Vec<ShapedParagraph>, Vec<ParagraphStyling>), LayoutError> {
    if !(src.default_font_size_pt.is_finite() && src.default_font_size_pt > 0.0) {
        return Err(LayoutError::InvalidGeometry);
    }
    let mut char_cache: BTreeMap<&str, ResolvedStyle> = BTreeMap::new();
    let mut out = Vec::with_capacity(paragraphs.len());
    let mut stylings = Vec::with_capacity(paragraphs.len());
    for p in paragraphs {
        let sp = story
            .paragraphs
            .iter()
            .find(|sp| p.source.start >= sp.start && (p.source.start < sp.end || p.source.start == sp.start));
        let resolved = match sp.and_then(|sp| sp.style_ref.as_deref()) {
            Some(id) => Some(resolve_style(src.records, id, TypeStyleScope::Paragraph)?),
            None => None,
        };
        let size = resolved
            .as_ref()
            .and_then(|r| r.font_size_pt)
            .unwrap_or(src.default_font_size_pt);
        let mut q = p.clone();
        if let Some(r) = &resolved {
            match &r.line_height {
                Some(StudioLineHeight::Percent { value }) => {
                    let h = size * value / 100.0;
                    if !(h.is_finite() && h > 0.0) {
                        return Err(LayoutError::InvalidGeometry);
                    }
                    q.line_height_pt = h;
                    q.ascent_pt = p.ascent_pt.min(h);
                }
                Some(StudioLineHeight::Length { length }) => {
                    let h = length_pt(length)?;
                    if h <= 0.0 {
                        return Err(LayoutError::InvalidGeometry);
                    }
                    q.line_height_pt = h;
                    q.ascent_pt = p.ascent_pt.min(h);
                }
                Some(StudioLineHeight::Auto) | None => {}
            }
        }
        let para_ls = spacing_pt(&resolved.as_ref().and_then(|r| r.letter_spacing.clone()), size)?;
        let mut ranges = story.character_ranges.iter().peekable();
        for g in &mut q.glyphs {
            while ranges.next_if(|r| r.end <= g.cluster.start).is_some() {}
            let extra = match ranges.peek().and_then(|r| {
                (r.start <= g.cluster.start).then_some(r.style_ref.as_deref()).flatten()
            }) {
                Some(id) => {
                    if !char_cache.contains_key(id) {
                        char_cache.insert(id, resolve_style(src.records, id, TypeStyleScope::Character)?);
                    }
                    let cs = &char_cache[id];
                    match &cs.letter_spacing {
                        Some(_) => spacing_pt(&cs.letter_spacing, cs.font_size_pt.unwrap_or(size))?,
                        None => para_ls,
                    }
                }
                None => para_ls,
            };
            g.advance_pt += extra;
            if !g.advance_pt.is_finite() || g.advance_pt < 0.0 {
                return Err(LayoutError::InvalidGeometry);
            }
        }
        stylings.push(ParagraphStyling {
            alignment: resolved
                .as_ref()
                .and_then(|r| r.align)
                .map_or(fallback, Alignment::from),
            font_size_pt: size,
            style: resolved,
        });
        out.push(q);
    }
    Ok((out, stylings))
}
