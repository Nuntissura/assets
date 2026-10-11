//! Shaped-paragraph input, UAX #14 break opportunities and the box/glue/penalty item model that
//! both line breakers consume. Shaping is Type's capability (STU-ARC-007): layout only reads the
//! glyph advances and source clusters Type produced.
use crate::{LayoutError, SourceSpan};
use hsk_studio_type as ty;
use unicode_linebreak::{BreakOpportunity, linebreaks};

/// One shaped glyph in the order given (logical = visual for the supported left-to-right slice).
/// `cluster` is the byte range of source text the glyph belongs to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutGlyph {
    pub cluster: SourceSpan,
    pub advance_pt: f64,
}

/// One paragraph of shaped text. `source` and every `cluster` are byte offsets into the story text.
/// `line_height_pt` and `ascent_pt` come from the caller's resolved style (descent is the rest).
#[derive(Clone, Debug, PartialEq)]
pub struct ShapedParagraph {
    pub source: SourceSpan,
    pub glyphs: Vec<LayoutGlyph>,
    pub line_height_pt: f64,
    pub ascent_pt: f64,
    pub font_missing: bool,
    pub right_to_left: bool,
    pub latin_only: bool,
}
impl ShapedParagraph {
    /// Latin left-to-right paragraph with every font present.
    pub fn latin_ltr(
        source: SourceSpan,
        glyphs: Vec<LayoutGlyph>,
        line_height_pt: f64,
        ascent_pt: f64,
    ) -> Self {
        Self {
            source,
            glyphs,
            line_height_pt,
            ascent_pt,
            font_missing: false,
            right_to_left: false,
            latin_only: true,
        }
    }
    /// Flattens a Type-shaped paragraph. A `Prepared` result never carries a missing font (Type
    /// refuses instead), so `font_missing` is false; right-to-left, odd bidi levels and scripts other
    /// than Latn/Zyyy mark the paragraph unsupported so `reflow` refuses it instead of mis-laying it.
    pub fn from_type(p: &ty::ParagraphResult<'_>, line_height_pt: f64, ascent_pt: f64) -> Self {
        let mut glyphs = Vec::new();
        let (mut rtl, mut latin) = (p.direction() == ty::Direction::RightToLeft, true);
        for run in p.runs() {
            rtl |= run.direction() == ty::Direction::RightToLeft || run.bidi_level() % 2 == 1;
            latin &= matches!(&run.script(), b"Latn" | b"Zyyy");
            glyphs.extend(run.glyphs().iter().map(|g| LayoutGlyph {
                cluster: SourceSpan {
                    start: g.cluster.start,
                    end: g.cluster.end,
                },
                advance_pt: g.x_advance_pt,
            }));
        }
        Self {
            source: SourceSpan {
                start: p.source().start,
                end: p.source().end,
            },
            glyphs,
            line_height_pt,
            ascent_pt,
            font_missing: false,
            right_to_left: rtl,
            latin_only: latin,
        }
    }
    /// Toy/test fixture, NOT shaping: one glyph per `char` of `text[source]` with a fixed advance.
    pub fn from_fixed_advance(
        text: &str,
        source: SourceSpan,
        advance_pt: f64,
        line_height_pt: f64,
        ascent_pt: f64,
    ) -> Option<Self> {
        let slice = text.get(usize::try_from(source.start).ok()?..usize::try_from(source.end).ok()?)?;
        let glyphs = slice
            .char_indices()
            .map(|(at, c)| LayoutGlyph {
                cluster: SourceSpan {
                    start: source.start + at as u64,
                    end: source.start + (at + c.len_utf8()) as u64,
                },
                advance_pt,
            })
            .collect();
        Some(Self::latin_ltr(source, glyphs, line_height_pt, ascent_pt))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BreakKind {
    Allowed,
    Mandatory,
}
/// A UAX #14 break position: a line may end at byte offset `byte` of the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Break {
    pub byte: usize,
    pub kind: BreakKind,
}
/// UAX #14 opportunities of `text`, ascending; the end of non-empty text is `Mandatory`.
pub fn break_opportunities(text: &str) -> Vec<Break> {
    linebreaks(text)
        .map(|(byte, kind)| Break {
            byte,
            kind: match kind {
                BreakOpportunity::Mandatory => BreakKind::Mandatory,
                BreakOpportunity::Allowed => BreakKind::Allowed,
            },
        })
        .collect()
}

/// Space stretch and shrink as fractions of the space width (TeX-style 1/2 and 1/3 by default).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpaceParams {
    pub stretch: f64,
    pub shrink: f64,
}
impl Default for SpaceParams {
    fn default() -> Self {
        Self {
            stretch: 0.5,
            shrink: 1.0 / 3.0,
        }
    }
}

pub(crate) const FORCED: f64 = f64::NEG_INFINITY;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Kind {
    Box,
    Glue,
    Penalty,
}
/// Knuth-Plass item. Glyph ranges are paragraph-local. A `Penalty` carries `first == end ==` the
/// glyph index where the next line would start if the line breaks there.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Item {
    pub kind: Kind,
    pub w: f64,
    pub y: f64,
    pub z: f64,
    pub inf_stretch: bool,
    pub p: f64,
    pub first: usize,
    pub end: usize,
    pub breakable: bool,
    /// Width ignored in line sums (hanging space before a forced break or the paragraph end).
    pub trailing: bool,
}
impl Item {
    fn penalty(p: f64, at: usize) -> Self {
        Self {
            kind: Kind::Penalty,
            w: 0.0,
            y: 0.0,
            z: 0.0,
            inf_stretch: false,
            p,
            first: at,
            end: at,
            breakable: true,
            trailing: false,
        }
    }
    fn fill(at: usize) -> Self {
        Self {
            kind: Kind::Glue,
            w: 0.0,
            y: 0.0,
            z: 0.0,
            inf_stretch: true,
            p: 0.0,
            first: at,
            end: at,
            breakable: false,
            trailing: false,
        }
    }
}

fn is_hanging_space(c: char) -> bool {
    c.is_whitespace() && !matches!(c, '\u{00A0}' | '\u{2007}' | '\u{202F}')
}

/// Builds the item list: boxes (unbreakable glyph runs), glue (whitespace runs), zero-width
/// penalties at in-word opportunities (after a hyphen), and finishing glue plus a forced penalty at
/// every mandatory break and at the paragraph end.
pub(crate) fn build_items(
    p: &ShapedParagraph,
    text: &str,
    space: SpaceParams,
) -> Result<Vec<Item>, LayoutError> {
    let (s, e) = (
        usize::try_from(p.source.start).map_err(|_| LayoutError::InvalidInput)?,
        usize::try_from(p.source.end).map_err(|_| LayoutError::InvalidInput)?,
    );
    let slice = text.get(s..e).ok_or(LayoutError::InvalidInput)?;
    let opps: Vec<Break> = break_opportunities(slice)
        .into_iter()
        .map(|b| Break {
            byte: b.byte + s,
            kind: b.kind,
        })
        .collect();
    let g = &p.glyphs;
    let mut previous_end = s;
    for glyph in g {
        let (a, b) = (glyph.cluster.start as usize, glyph.cluster.end as usize);
        if a < previous_end || b < a || b > e || !text.is_char_boundary(a) || !text.is_char_boundary(b)
        {
            return Err(LayoutError::InvalidInput);
        }
        if !glyph.advance_pt.is_finite() || glyph.advance_pt < 0.0 {
            return Err(LayoutError::InvalidInput);
        }
        previous_end = a;
    }
    let space_at = |i: usize| {
        let c = &g[i].cluster;
        text.get(c.start as usize..c.end as usize)
            .is_some_and(|t| !t.is_empty() && t.chars().all(is_hanging_space))
    };
    // Strongest opportunity strictly between glyph `i` and the next glyph (or the paragraph end).
    let opp_after = |i: usize| -> Option<BreakKind> {
        let lo = g[i].cluster.end as usize;
        let hi = g.get(i + 1).map_or(e, |n| n.cluster.start as usize);
        let from = opps.partition_point(|b| b.byte < lo);
        let mut found = None;
        for b in opps[from..].iter().take_while(|b| b.byte <= hi) {
            if b.kind == BreakKind::Mandatory {
                return Some(BreakKind::Mandatory);
            }
            found = Some(BreakKind::Allowed);
        }
        found
    };
    let adv = |a: usize, b: usize| g[a..b].iter().map(|x| x.advance_pt).sum::<f64>();
    let mut items: Vec<Item> = Vec::new();
    let finish = |items: &mut Vec<Item>, at: usize| {
        if let Some(last) = items.last_mut()
            && last.kind == Kind::Glue
            && !last.inf_stretch
        {
            last.trailing = true;
            last.breakable = false;
        }
        items.push(Item::fill(at));
        items.push(Item::penalty(FORCED, at));
    };
    let n = g.len();
    let mut i = 0;
    while i < n {
        let sp = space_at(i);
        let mut j = i + 1;
        while j < n && space_at(j) == sp && opp_after(j - 1).is_none() {
            j += 1;
        }
        let after = opp_after(j - 1);
        let w = adv(i, j);
        if sp {
            items.push(Item {
                kind: Kind::Glue,
                w,
                y: w * space.stretch,
                z: w * space.shrink,
                inf_stretch: false,
                p: 0.0,
                first: i,
                end: j,
                breakable: after == Some(BreakKind::Allowed),
                trailing: false,
            });
        } else {
            items.push(Item {
                kind: Kind::Box,
                w,
                y: 0.0,
                z: 0.0,
                inf_stretch: false,
                p: 0.0,
                first: i,
                end: j,
                breakable: false,
                trailing: false,
            });
        }
        if j < n {
            match after {
                Some(BreakKind::Mandatory) => finish(&mut items, j),
                Some(BreakKind::Allowed) if !sp && !space_at(j) => {
                    items.push(Item::penalty(0.0, j));
                }
                _ => {}
            }
        }
        i = j;
    }
    finish(&mut items, n);
    Ok(items)
}
