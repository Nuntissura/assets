//! Shaped-paragraph input, UAX #14 break opportunities and the box/glue/penalty item model that
//! both line breakers consume. Shaping is Type's capability (STU-ARC-007): layout only reads the
//! glyph advances and source clusters Type produced.
use crate::hyphenate::{HyphenationParams, Hyphenator};
use crate::{LayoutError, SourceSpan};
use hsk_studio_folio::FirstBaseline;
use hsk_studio_type as ty;
use unicode_linebreak::{BreakOpportunity, linebreaks};

/// One shaped glyph in the order given (logical = visual for the supported left-to-right slice).
/// `cluster` is the byte range of source text the glyph belongs to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutGlyph {
    pub cluster: SourceSpan,
    pub advance_pt: f64,
}

/// The advance glyph `glyph` takes when a line ends right after it at a hyphenation break, where it
/// differs from the shaped advance (Type composition v2 `LineEndAdvance`: the glyph's `hmtx`
/// advance, so pair kerning toward the next glyph does not cross the line end).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineEndAdvance {
    pub glyph: usize,
    pub advance_pt: f64,
}
/// One component of the ligature glyph `glyph` (Type composition v2 `LigatureComponent`): its
/// source characters and nominal advance. A hyphenation break inside the ligature sets each side
/// with the summed advances of its components.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LigaturePart {
    pub glyph: usize,
    pub source: SourceSpan,
    pub advance_pt: f64,
}

/// One paragraph of shaped text. `source` and every `cluster` are byte offsets into the story text.
/// `line_height_pt` and `ascent_pt` come from the caller's resolved style (descent is the rest).
/// `line_end_advances` and `ligature_parts` (paragraph-local glyph indices, ascending) carry Type's
/// composition v2 line-end data; both are empty for other sources.
/// `cap_height_pt`, `x_height_pt` and `em_box_pt` are the STU-LAY-120 first-baseline metrics of the
/// paragraph's first run (lane SMB-ID-FRAME-AUTOSIZE-1): the face's OS/2 `sCapHeight` / `sxHeight`
/// scaled by font size / units per em, and the em box height, which Studio reads as the font size
/// (UNVERIFIED, D5). `None` = not known: a frame whose first baseline needs it is refused, never
/// guessed (`first_baseline_metric`).
#[derive(Clone, Debug, PartialEq)]
pub struct ShapedParagraph {
    pub source: SourceSpan,
    pub glyphs: Vec<LayoutGlyph>,
    pub line_height_pt: f64,
    pub ascent_pt: f64,
    pub font_missing: bool,
    pub right_to_left: bool,
    pub latin_only: bool,
    pub line_end_advances: Vec<LineEndAdvance>,
    pub ligature_parts: Vec<LigaturePart>,
    pub cap_height_pt: Option<f64>,
    pub x_height_pt: Option<f64>,
    pub em_box_pt: Option<f64>,
}

/// A STU-LAY-120 first-baseline kind whose metric the paragraph does not carry (a face without the
/// OS/2 value, or a source that states no font size). `code()` is
/// `first_baseline_metric_unavailable:<kind>`; the threaded reflow refuses such a frame with
/// `LayoutError::UnsupportedComposition` (the refusal the reflow has always given for these kinds).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirstBaselineUnavailable {
    pub kind: FirstBaseline,
}
impl FirstBaselineUnavailable {
    pub const fn code(self) -> &'static str {
        match self.kind {
            FirstBaseline::Ascent => "first_baseline_metric_unavailable:ascent",
            FirstBaseline::CapHeight => "first_baseline_metric_unavailable:cap_height",
            FirstBaseline::Leading => "first_baseline_metric_unavailable:leading",
            FirstBaseline::EmBoxHeight => "first_baseline_metric_unavailable:em_box_height",
            FirstBaseline::XHeight => "first_baseline_metric_unavailable:x_height",
            FirstBaseline::Fixed => "first_baseline_metric_unavailable:fixed",
        }
    }
}
impl std::fmt::Display for FirstBaselineUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for FirstBaselineUnavailable {}

impl ShapedParagraph {
    /// The STU-LAY-120 metric of `kind` (pt above the baseline): ascent, cap height, leading (the
    /// line height), em box height or x height; `fixed` has none (0). A metric the paragraph does not
    /// carry is refused, never replaced by another one.
    pub fn first_baseline_metric(&self, kind: FirstBaseline) -> Result<f64, FirstBaselineUnavailable> {
        let v = match kind {
            FirstBaseline::Ascent => Some(self.ascent_pt),
            FirstBaseline::Leading => Some(self.line_height_pt),
            FirstBaseline::CapHeight => self.cap_height_pt,
            FirstBaseline::XHeight => self.x_height_pt,
            FirstBaseline::EmBoxHeight => self.em_box_pt,
            FirstBaseline::Fixed => Some(0.0),
        };
        v.ok_or(FirstBaselineUnavailable { kind })
    }
    /// Sets the first-baseline metrics from a face's `head.unitsPerEm` and OS/2 `sCapHeight` /
    /// `sxHeight` (font units; `None` when the face has no OS/2 table of version 2 or later) at
    /// `font_size_pt`: each is scaled by `font_size_pt / units_per_em`, and the em box height is the
    /// font size. `units_per_em == 0` leaves cap and x height unknown.
    pub fn with_face_metrics(
        mut self,
        font_size_pt: f64,
        units_per_em: u16,
        cap_height: Option<i16>,
        x_height: Option<i16>,
    ) -> Self {
        let scale = (units_per_em > 0).then(|| font_size_pt / f64::from(units_per_em));
        self.cap_height_pt = scale.zip(cap_height).map(|(s, v)| f64::from(v) * s);
        self.x_height_pt = scale.zip(x_height).map(|(s, v)| f64::from(v) * s);
        self.em_box_pt = Some(font_size_pt);
        self
    }
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
            line_end_advances: Vec::new(),
            ligature_parts: Vec::new(),
            cap_height_pt: None,
            x_height_pt: None,
            em_box_pt: None,
        }
    }
    /// Flattens a Type-shaped paragraph. A `Prepared` result never carries a missing font (Type
    /// refuses instead), so `font_missing` is false; right-to-left, odd bidi levels and scripts other
    /// than Latn/Zyyy mark the paragraph unsupported so `reflow` refuses it instead of mis-laying it.
    /// The em box height is the first run's font size; Type's public API states no OS/2 cap or x
    /// height, so those stay unknown until the host supplies them (`with_face_metrics`).
    pub fn from_type(p: &ty::ParagraphResult<'_>, line_height_pt: f64, ascent_pt: f64) -> Self {
        let mut glyphs = Vec::new();
        let (mut line_end_advances, mut ligature_parts) = (Vec::new(), Vec::new());
        let (mut rtl, mut latin) = (p.direction() == ty::Direction::RightToLeft, true);
        for run in p.runs() {
            rtl |= run.direction() == ty::Direction::RightToLeft || run.bidi_level() % 2 == 1;
            latin &= matches!(&run.script(), b"Latn" | b"Zyyy");
            let base = glyphs.len();
            line_end_advances.extend(run.line_end_advances().iter().map(|a| LineEndAdvance {
                glyph: base + a.glyph as usize,
                advance_pt: a.advance_pt,
            }));
            ligature_parts.extend(run.ligature_components().iter().map(|c| LigaturePart {
                glyph: base + c.glyph as usize,
                source: SourceSpan {
                    start: c.source.start,
                    end: c.source.end,
                },
                advance_pt: c.advance_pt,
            }));
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
            line_end_advances,
            ligature_parts,
            cap_height_pt: None,
            x_height_pt: None,
            em_box_pt: p.runs().first().map(|r| r.font_size_pt()),
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
    /// Flagged penalty = a hyphenation break (the line gets a hyphen of width `w`).
    pub flagged: bool,
    /// Flagged penalty: how the line ending here sets the glyph material before the break when
    /// Type's line-end data changes it (`w` = hyphen + that change).
    pub end_part: Option<EndPart>,
    /// Box: its first glyph is the right part of a ligature split by the break just before it; when
    /// the box starts a line, the line is `head.delta` wider than the box's natural width.
    pub head: Option<HeadPart>,
}
/// Glyph material set at the end of a line by a hyphenation break (Type composition v2 data).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EndPart {
    /// Paragraph-local glyph set at the line end.
    pub glyph: usize,
    /// Source end of the material: the glyph's cluster end, or a byte inside a ligature.
    pub end: u64,
    pub advance_pt: f64,
    /// The material is the left part of a ligature that lies past the line's glyph range (the line
    /// sets one more glyph than its range holds).
    pub extra: bool,
}
/// The right part of a ligature split by a hyphenation break: set from source byte `start` with
/// `advance_pt` (`delta` = that minus the ligature's shaped advance).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct HeadPart {
    pub start: u64,
    pub advance_pt: f64,
    pub delta: f64,
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
            flagged: false,
            end_part: None,
            head: None,
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
            flagged: false,
            end_part: None,
            head: None,
        }
    }
}

fn is_hanging_space(c: char) -> bool {
    c.is_whitespace() && !matches!(c, '\u{00A0}' | '\u{2007}' | '\u{202F}')
}

/// Summed component advances of ligature glyph `k` before and after source byte `at`, when Type
/// recorded its components and `at` falls between two of them.
fn ligature_split(p: &ShapedParagraph, k: usize, at: u64) -> Option<(f64, f64)> {
    let (mut before, mut after, mut n) = (0.0, 0.0, 0usize);
    for part in p.ligature_parts.iter().filter(|c| c.glyph == k) {
        n += 1;
        if part.source.end <= at {
            before += part.advance_pt;
        } else if part.source.start >= at {
            after += part.advance_pt;
        } else {
            return None;
        }
    }
    (n >= 2 && before > 0.0 && after > 0.0).then_some((before, after))
}

/// Builds the item list: boxes (unbreakable glyph runs), glue (whitespace runs), zero-width
/// penalties at in-word opportunities (after a hyphen), and finishing glue plus a forced penalty at
/// every mandatory break and at the paragraph end. `hyphenate_last_word` false (STU-TYP-151) adds no
/// hyphenation point to the paragraph's last word (the glyph run holding its last non-space glyph).
pub(crate) fn build_items(
    p: &ShapedParagraph,
    text: &str,
    space: SpaceParams,
    hyph: Option<(&Hyphenator, &HyphenationParams)>,
    hyphenate_last_word: bool,
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
    // End (exclusive) of the paragraph's last non-space glyph: the run ending there is its last word.
    let last_word_end = (0..n).rev().find(|k| !space_at(*k)).map_or(0, |k| k + 1);
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
                flagged: false,
                end_part: None,
                head: None,
            });
        } else {
            // Hyphenation cuts: glyph index `k` where the next line would start, and `Some(byte)` when
            // the cut lies inside glyph `k` (a ligature with Type component data), else `None` (the
            // hyphen follows glyph `k - 1`).
            let mut cuts: Vec<(usize, Option<u64>)> = Vec::new();
            if let Some((h, hp)) = hyph
                && j - i >= 2
                && (hyphenate_last_word || j != last_word_end)
            {
                let (a, b) = (g[i].cluster.start as usize, g[j - 1].cluster.end as usize);
                if let Some(word) = text.get(a..b) {
                    for (_, bytes) in h.hyphenate_text(word, hp) {
                        for byte in bytes {
                            let abs = a + byte;
                            if let Some(k) = (i + 1..j).find(|k| {
                                g[*k].cluster.start as usize == abs
                                    && g[*k - 1].cluster.end as usize == abs
                            }) {
                                cuts.push((k, None));
                            } else if let Some(k) = (i + 1..j).find(|k| {
                                (g[*k].cluster.start as usize) < abs
                                    && abs < g[*k].cluster.end as usize
                            }) && ligature_split(p, k, abs as u64).is_some()
                            {
                                cuts.push((k, Some(abs as u64)));
                            }
                        }
                    }
                    // One cut per glyph index; a cut at the glyph boundary wins over one inside it.
                    cuts.sort_unstable_by_key(|c| (c.0, c.1.is_some()));
                    cuts.dedup_by_key(|c| c.0);
                }
            }
            let hyphen_w = hyph.map_or(0.0, |x| x.1.hyphen_advance_pt);
            let mut from = i;
            let mut head = None;
            for &(cut, inside) in cuts.iter().chain(std::iter::once(&(j, None))) {
                items.push(Item {
                    kind: Kind::Box,
                    w: adv(from, cut),
                    y: 0.0,
                    z: 0.0,
                    inf_stretch: false,
                    p: 0.0,
                    first: from,
                    end: cut,
                    breakable: false,
                    trailing: false,
                    flagged: false,
                    end_part: None,
                    head: head.take(),
                });
                if cut != j {
                    let mut pen = Item::penalty(hyph.map_or(0.0, |x| x.1.hyphen_penalty), cut);
                    pen.w = hyphen_w;
                    pen.flagged = true;
                    match inside {
                        // Pair kerning toward the next glyph does not cross the line end.
                        None => {
                            let k = cut - 1;
                            if let Some(a) = p.line_end_advances.iter().find(|a| a.glyph == k) {
                                pen.w += a.advance_pt - g[k].advance_pt;
                                pen.end_part = Some(EndPart {
                                    glyph: k,
                                    end: g[k].cluster.end,
                                    advance_pt: a.advance_pt,
                                    extra: false,
                                });
                            }
                        }
                        // A ligature split by the hyphen: each side set with its components.
                        Some(at) => {
                            let (before, after) = ligature_split(p, cut, at).unwrap_or((0.0, 0.0));
                            pen.w += before;
                            pen.end_part = Some(EndPart {
                                glyph: cut,
                                end: at,
                                advance_pt: before,
                                extra: true,
                            });
                            head = Some(HeadPart {
                                start: at,
                                advance_pt: after,
                                delta: after - g[cut].advance_pt,
                            });
                        }
                    }
                    items.push(pen);
                }
                from = cut;
            }
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
