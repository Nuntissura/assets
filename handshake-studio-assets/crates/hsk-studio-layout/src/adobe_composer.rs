//! InDesign-parity composers (lane SMB-ID-COMPOSER-PARITY, spec STU-TYP-135 `composition_scope`
//! `single_line` / `every_line`). Both work on the same box/glue/penalty items as `breaker`.
//!
//! The rules and constants below are FITTED to the InDesign 21.6 oracle of D5 (ID-01..ID-04: one
//! Arial 12/14.4 paragraph, widths 150..420 pt, word spacing 80/100/133 plus the ID-02 limit sets).
//! They reproduce every line start of those cases they are listed for in
//! `hsk-studio-interop-layout/tests/d5_id_oracle.rs`; InDesign behaviour outside that oracle is
//! UNVERIFIED. Adobe documents the Single-line Composer only as "composing text one line at a time"
//! and the Paragraph Composer as weighting break points by evenness of spacing and hyphenation
//! (helpx.adobe.com "Text composition"); Adobe's patent US 6,510,441 ("Optimal line break
//! determination") describes demerits of the form (K * R^3 + H + 1)^2 plus ladder, last-line and
//! consistency terms over a window of lines. No Adobe code exists or was read.
//!
//! Single-line composer (each line in isolation), measured against the oracle:
//! * ragged lines: the longest line that fits at natural spacing; a word is hyphenated only when
//!   breaking before it leaves more than the hyphenation zone (IDML `HyphenationZone`);
//! * justified lines ("inside the limits" = the overflow or white space is covered by the word and
//!   letter spacing ranges of the line): (a) the first whole-word break that overflows the measure
//!   is taken when it fits inside the limits, (b) else the last whole-word break that fits when it
//!   stays inside them (when both qualify, the one with the smaller deviation, each normalised by
//!   its range, ties to the tighter), (c) else a hyphenation break between them inside the limits
//!   (tighter first, else the longest), (d) else the tight break is taken past the limits when
//!   `under > SINGLE_TIGHT_C * spaces / shrink range * over` (= `SINGLE_TIGHT_C / (1 - min)` with
//!   word spacing alone; `under` = white space left by the loose break, `over` = overflow of the
//!   tight one, points) and its spaces stay positive; at the paragraph end the tight break is taken
//!   when its spaces stay at or above `LAST_LINE_MIN_SCALE` of their natural width.
//!
//! Paragraph composer: minimum total cost over a window of `PARA_MODEL.depth` lines, committing one
//! line at a time; line costs in points (loose white space, tight overflow normalised by the
//! word-spacing shrink range), the patent's `(cost + 1)^2`, hyphen and ladder terms, and a last-line
//! length term; ragged paragraphs minimise the cubed white space over the whole paragraph.
//!
//! Justification (`justify_line`): InDesign always fills a justified line to the measure: inside the
//! limits word spacing first, then letter spacing; past them the remainder is shared in proportion to
//! the ranges (ID-01: spaces at 42 % .. 2440 % of natural); a single-word line is letter-spaced; a
//! last line wider than the measure is compressed into it. Glyph scaling is not implemented (oracle
//! measurement recorded in the lane notes).
use crate::LayoutError;
use crate::breaker::{AdobeParams, HyphenRules, LineBreak, Sums, check, forced, legal, line_start};
use crate::paragraph::Item;
use crate::typeset::{Justification, JustifyParams, justify_metrics};
use hsk_studio_accord::CancellationToken;

const EPS: f64 = 1e-9;
/// Fitted (oracle ID-01/ID-02 single-line, 44 of 44 line sets): the separating band is
/// 1.203 < c < 1.316 (largest rejected and smallest accepted `under / over * (1 - min)`).
pub(crate) const SINGLE_TIGHT_C: f64 = 1.26;
/// Fitted: a last line is compressed into the measure down to this word-space scale (oracle:
/// accepted 0.680 and 0.691, refused 0.396).
pub(crate) const LAST_LINE_MIN_SCALE: f64 = 0.5;

/// One break candidate of the line starting at item `s`.
#[derive(Clone, Copy, Debug)]
struct Cand {
    item: usize,
    w: f64,
    spaces: f64,
    /// Shrink and stretch inside the limits (points): word spacing plus letter spacing.
    shrink: f64,
    stretch: f64,
    flagged: bool,
    end: bool,
}
impl Cand {
    /// Word-space scale that sets the line exactly to `width` (None without spaces).
    fn scale(&self, width: f64) -> Option<f64> {
        (self.spaces > EPS).then(|| 1.0 + (width - self.w) / self.spaces)
    }
    /// The line meets `width` inside the justification limits (and has a word space).
    fn within(&self, width: f64) -> bool {
        self.spaces > EPS
            && if self.w > width {
                self.w - width <= self.shrink + EPS
            } else {
                width - self.w <= self.stretch + EPS
            }
    }
}

/// Glyph gaps of the line `items[s..i)` that letter spacing acts on: every glyph of the content
/// but the last, plus the hyphen of a hyphenated end.
fn glyph_gaps(items: &[Item], s: usize, i: usize) -> usize {
    let first = items.get(s).map_or(0, |it| it.first);
    let end = items[s..i]
        .iter()
        .rev()
        .find(|it| it.kind == crate::paragraph::Kind::Box)
        .map_or(first, |it| it.end);
    end.saturating_sub(first).saturating_sub(1) + usize::from(items[i].flagged)
}

/// The context both Adobe composers need.
pub(crate) struct AdobeContext<'a> {
    pub params: AdobeParams,
    pub justify: &'a JustifyParams,
    /// The paragraph's lines (except an unjustified last line) fill the measure.
    pub justified: bool,
    /// The last line is justified too (`Alignment::JustifiedAll`).
    pub justify_last: bool,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn single_line(
    items: &[Item],
    sums: &Sums,
    width: &dyn Fn(usize) -> f64,
    rules: &HyphenRules,
    ctx: &AdobeContext<'_>,
    cancel: &CancellationToken,
) -> Result<Vec<LineBreak>, LayoutError> {
    let (mut breaks, mut start, mut run, mut work) = (Vec::new(), 0usize, 0u8, 0u32);
    let n = items.len();
    while start < n {
        let w_line = width(breaks.len());
        // Candidates of this line up to the first whole-word break that overflows, or the first
        // forced break.
        let mut cands: Vec<Cand> = Vec::new();
        let mut i = start;
        while i < n {
            check(cancel, &mut work)?;
            if i > start && legal(items, i) {
                let flagged = items[i].flagged;
                let skip = flagged && rules.limit > 0 && run >= rules.limit;
                if !skip {
                    let (w, ..) = sums.measure(items, start, i);
                    let end = forced(&items[i]);
                    let spaces = sums.spaces(start, i);
                    let letters =
                        glyph_gaps(items, start, i) as f64 * ctx.justify.reference_space_pt;
                    let (ls, ws) = (ctx.justify.letter_spacing, ctx.justify.word_spacing);
                    let shrink = spaces * (ws.desired - ws.min) + letters * (ls.desired - ls.min);
                    let stretch = spaces * (ws.max - ws.desired) + letters * (ls.max - ls.desired);
                    cands.push(Cand {
                        item: i,
                        w,
                        spaces,
                        shrink,
                        stretch,
                        flagged,
                        end,
                    });
                    if end || (!flagged && w > w_line + EPS) {
                        break;
                    }
                }
            }
            i += 1;
        }
        let Some(last) = cands.last().copied() else {
            break;
        };
        let pick = if last.end && last.w <= w_line + EPS {
            last
        } else if ctx.justified {
            justified_pick(&cands, w_line)
        } else {
            ragged_pick(&cands, w_line, ctx.params.hyphenation_zone_pt)
        };
        let overfull = pick.w > w_line + EPS && pick.scale(w_line).is_none_or(|s| s <= 0.0);
        run = if pick.flagged {
            run.saturating_add(1)
        } else {
            0
        };
        breaks.push(LineBreak {
            item: pick.item,
            overfull,
            ratio: pick.scale(w_line),
        });
        if pick.end && pick.item + 1 >= n {
            break;
        }
        start = line_start(items, pick.item);
    }
    Ok(breaks)
}

fn ragged_pick(cands: &[Cand], w_line: f64, zone: f64) -> Cand {
    let lw = cands
        .iter()
        .rev()
        .find(|c| !c.flagged && c.w <= w_line + EPS);
    let hyphenate = lw.is_none_or(|c| w_line - c.w > zone);
    let usable = cands.iter().filter(|c| hyphenate || !c.flagged);
    let mut fit = None;
    let mut over = None;
    for c in usable {
        if c.w <= w_line + EPS {
            fit = Some(*c);
        } else {
            over = Some(*c);
            break;
        }
    }
    fit.or(over).unwrap_or(cands[cands.len() - 1])
}

fn justified_pick(cands: &[Cand], w_line: f64) -> Cand {
    let lw = cands
        .iter()
        .rev()
        .find(|c| !c.flagged && c.w <= w_line + EPS)
        .copied();
    let tw = cands
        .iter()
        .find(|c| !c.flagged && c.w > w_line + EPS)
        .copied();
    let t_ok = tw.is_some_and(|c| c.within(w_line));
    let l_ok = lw.is_some_and(|c| c.within(w_line));
    if let (Some(t), Some(l), true, true) = (tw, lw, t_ok, l_ok) {
        // Deviation from desired, each normalised by its range; ties to the tighter line.
        let dt = (t.w - w_line) / t.shrink.max(EPS);
        let dl = if l.stretch > EPS {
            (w_line - l.w) / l.stretch
        } else {
            0.0
        };
        return if dt <= dl { t } else { l };
    }
    if let (Some(t), true) = (tw, t_ok) {
        return t;
    }
    if let (Some(l), true) = (lw, l_ok) {
        return l;
    }
    // (c) hyphenation breaks between the whole-word candidates, inside the limits.
    let lo = lw.map_or(0, |c| c.item);
    let hi = tw.map_or(usize::MAX, |c| c.item);
    let inside: Vec<Cand> = cands
        .iter()
        .filter(|c| c.flagged && c.item > lo && c.item < hi && c.within(w_line))
        .copied()
        .collect();
    if let Some(t) = inside.iter().find(|c| c.w > w_line + EPS) {
        return *t;
    }
    if let Some(l) = inside.last() {
        return *l;
    }
    // (d) out of limits: the nearest fitting and overflowing candidates of any kind.
    let fit = cands.iter().rev().find(|c| c.w <= w_line + EPS).copied();
    let over = cands.iter().find(|c| c.w > w_line + EPS).copied();
    match (fit, over) {
        (None, Some(t)) => t,
        (Some(l), None) => l,
        (None, None) => cands[cands.len() - 1],
        (Some(l), Some(t)) => {
            let ts = t.scale(w_line);
            if t.end {
                if ts.is_some_and(|s| s >= LAST_LINE_MIN_SCALE) {
                    t
                } else {
                    l
                }
            } else {
                // SINGLE_TIGHT_C / (desired - min) with word spacing alone; the shrink range of the
                // line in general.
                let k = if t.shrink > EPS {
                    SINGLE_TIGHT_C * t.spaces / t.shrink
                } else {
                    f64::INFINITY
                };
                let (under, over) = (w_line - l.w, t.w - w_line);
                if ts.is_some_and(|s| s > 0.0) && under > k * over {
                    t
                } else {
                    l
                }
            }
        }
    }
}

/// InDesign line justification: within the limits as `justify_metrics` (word spacing first, then
/// letter spacing), then the remainder past the limits shared by word and letter spacing in
/// proportion to their limit ranges (oracle ID-02 `ls-3_0_3` last line: word scale 1.7577 and letter
/// gap 0.2297 pt are both 2.297 times their ranges); with no range at all it goes into the word
/// spaces, and on a line without spaces into the letter gaps.
pub(crate) fn justify_line(
    natural_pt: f64,
    avail_pt: f64,
    spaces_pt: f64,
    gaps: usize,
    jp: &JustifyParams,
) -> Justification {
    let mut j = justify_metrics(natural_pt, avail_pt, spaces_pt, gaps, jp);
    let rest = j.residual_pt;
    if rest == 0.0 {
        return j;
    }
    let (w, l, r) = (&jp.word_spacing, &jp.letter_spacing, jp.reference_space_pt);
    let g = gaps as f64;
    let (word_range, letter_range) = if rest > 0.0 {
        ((w.max - w.desired) * spaces_pt, (l.max - l.desired) * r * g)
    } else {
        ((w.desired - w.min) * spaces_pt, (l.desired - l.min) * r * g)
    };
    let total = word_range + letter_range;
    if total > EPS {
        let k = rest / total;
        if spaces_pt > EPS {
            j.word_scale += k * word_range / spaces_pt;
        }
        if gaps > 0 {
            j.letter_gap_pt += k * letter_range / g;
        }
        j.residual_pt = 0.0;
    } else if spaces_pt > EPS {
        j.word_scale += rest / spaces_pt;
        j.residual_pt = 0.0;
    } else if gaps > 0 {
        // A single-word line: InDesign's Single Word Justification (IDML default `FullyJustified`)
        // letter-spaces it to the measure (oracle ID-01 'internationalization,' at 210 pt: +5.33 pt
        // per gap).
        j.letter_gap_pt += rest / g;
        j.residual_pt = 0.0;
    }
    j
}

/// A line wider than its slot outside justification (an Adobe last line set tight): spaces
/// compressed so the line meets the measure.
pub(crate) fn compress_line(
    natural_pt: f64,
    avail_pt: f64,
    spaces_pt: f64,
) -> Option<Justification> {
    (natural_pt > avail_pt + EPS && spaces_pt > EPS).then(|| Justification {
        word_scale: 1.0 + (avail_pt - natural_pt) / spaces_pt,
        letter_gap_pt: 0.0,
        residual_pt: 0.0,
    })
}

/// Paragraph-composer cost model (module docs). Every constant is fitted to the D5 oracle
/// (`PARA_MODEL`); outside it the values are UNVERIFIED.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ParaModel {
    /// Loose line: `al * g^pl + bl * max(0, g - stretch range)^ql`, `g` = white space left (pt).
    pub al: f64,
    pub pl: f64,
    pub bl: f64,
    pub ql: f64,
    /// Tight line: `at * (o * 0.2 / (desired - min))^pt + bt * max(0, o - shrink range)^qt`,
    /// `o` = overflow (pt).
    pub at: f64,
    pub pt: f64,
    pub bt: f64,
    pub qt: f64,
    /// A tight line whose spaces would fall under this fraction of their natural width is refused.
    pub tight_floor: f64,
    /// A line's cost saturates here before squaring (TeX-style badness cap; INFINITY = none).
    pub cap: f64,
    /// Line demerits are `(cost + 1)^2` (patent form) when set.
    pub square: bool,
    /// Hyphenated line end, added once per hyphen.
    pub hyphen: f64,
    /// Ladder: `ladder * ladder_growth^(run - 2)` for the run-th consecutive hyphen (run >= 2).
    pub ladder: f64,
    pub ladder_growth: f64,
    /// Last line: compressed down to `last_min` of its natural spaces at `last_tight * o^2`; when it
    /// fits, `last_short * max(0, short * W - w)^2 + last_long * max(0, w - long * W)^2`.
    pub last_min: f64,
    pub last_tight: f64,
    pub last_short: f64,
    pub short: f64,
    pub last_long: f64,
    pub long: f64,
    /// Lines of look-ahead when a break is committed (Adobe's window, patent US 6,510,441); 0 = the
    /// whole paragraph.
    pub depth: usize,
    /// Ragged lines: `g^ragged_p` (never compressed), hyphen `ragged_hyphen`, no ladder term (the
    /// ladder limit still holds), last line free,
    /// composed over the whole paragraph (`ragged_depth` 0) unless a window is given.
    pub ragged_p: f64,
    pub ragged_hyphen: f64,
    pub ragged_depth: usize,
}

/// The fitted paragraph-composer constants (search over the D5 oracle; the scratch search scripts
/// and their score are recorded in the lane notes).
pub(crate) const PARA_MODEL: ParaModel = ParaModel {
    al: 0.3679201753788433,
    pl: 2.3027382191852457,
    bl: 0.07671628669802522,
    ql: 2.2716828852032465,
    at: 2.96040673091101,
    pt: 2.8129263191677167,
    bt: 232.57808838978542,
    qt: 1.089612298054988,
    tight_floor: 0.15867427561083194,
    cap: f64::INFINITY,
    square: true,
    hyphen: 100000.0,
    ladder: 19196.08790222356,
    ladder_growth: 3.817794039653118,
    last_min: 0.5419261623068473,
    last_tight: 81.56347687414083,
    last_short: 0.8567343887498621,
    short: 0.44677369263188693,
    last_long: 10.961808017160683,
    long: 0.5,
    depth: 7,
    ragged_p: 3.0,
    ragged_hyphen: 21000.0,
    ragged_depth: 0,
};

/// Highest consecutive-hyphen run the paragraph composer tracks (penalties saturate there).
const MAX_RUN: usize = 8;
/// Cost of an emergency line (nothing fits): taken only when no feasible line exists.
const EMERGENCY: f64 = 1e30;

/// One feasible line of the paragraph composer.
#[derive(Clone, Copy, Debug)]
struct ParaLine {
    item: usize,
    cost: f64,
    flagged: bool,
    end: bool,
}

type Memo = std::collections::BTreeMap<(usize, usize, usize, usize), (f64, Option<ParaLine>)>;

struct Para<'a> {
    items: &'a [Item],
    sums: &'a Sums,
    width: &'a dyn Fn(usize) -> f64,
    model: &'a ParaModel,
    min: f64,
    desired: f64,
    max: f64,
    justified: bool,
    justify_last: bool,
    limit: usize,
}

impl Para<'_> {
    fn line_cost(&self, line: usize, s: usize, i: usize, last: bool) -> Option<f64> {
        let m = self.model;
        let wl = (self.width)(line);
        let (w, ..) = self.sums.measure(self.items, s, i);
        let spaces = self.sums.spaces(s, i);
        if !self.justified {
            return (w <= wl + EPS).then(|| if last { 0.0 } else { (wl - w).powf(m.ragged_p) });
        }
        let tight_ok = |floor: f64| spaces > EPS && 1.0 - (w - wl) / spaces >= floor;
        if last && !self.justify_last {
            if w > wl + EPS {
                return tight_ok(m.last_min).then(|| m.last_tight * (w - wl).powi(2));
            }
            let short = (m.short * wl - w).max(0.0);
            let long = (w - m.long * wl).max(0.0);
            return Some(m.last_short * short * short + m.last_long * long * long);
        }
        let cost = if w <= wl + EPS {
            let g = wl - w;
            let range = spaces * (self.max - self.desired).max(0.0);
            m.al * g.powf(m.pl) + m.bl * (g - range).max(0.0).powf(m.ql)
        } else {
            let o = w - wl;
            let floor = if last { m.last_min } else { m.tight_floor };
            if !tight_ok(floor) || self.desired - self.min <= EPS {
                return None;
            }
            let range = spaces * (self.desired - self.min);
            m.at * (o * 0.2 / (self.desired - self.min)).powf(m.pt)
                + m.bt * (o - range).max(0.0).powf(m.qt)
        };
        let cost = cost.min(m.cap);
        Some(if m.square {
            (cost + 1.0) * (cost + 1.0)
        } else {
            cost
        })
    }

    /// Feasible lines from line start `s` (line number `line`); a hyphenation break past the ladder
    /// limit is skipped.
    fn lines_from(
        &self,
        line: usize,
        s: usize,
        run: usize,
        work: &mut u32,
        cancel: &CancellationToken,
    ) -> Result<Vec<ParaLine>, LayoutError> {
        let mut out: Vec<ParaLine> = Vec::new();
        let wl = (self.width)(line);
        let floor = if self.justified {
            self.model.tight_floor.min(self.model.last_min)
        } else {
            1.0
        };
        for i in s + 1..self.items.len() {
            check(cancel, work)?;
            if !legal(self.items, i) {
                continue;
            }
            let (w, ..) = self.sums.measure(self.items, s, i);
            let spaces = self.sums.spaces(s, i);
            let flagged = self.items[i].flagged;
            let end = forced(&self.items[i]);
            // Past this point no later break can be compressed into the line.
            let hopeless = w - spaces * (1.0 - floor) > wl + EPS;
            if !(flagged && self.limit > 0 && run >= self.limit) {
                let last = end && i + 1 >= self.items.len();
                if let Some(mut cost) = self.line_cost(line, s, i, last) {
                    if flagged {
                        let nr = run + 1;
                        cost += if self.justified {
                            self.model.hyphen
                        } else {
                            self.model.ragged_hyphen
                        };
                        if nr >= 2 && self.justified {
                            let exp = i32::try_from(nr.min(MAX_RUN) - 2).unwrap_or(0);
                            cost += self.model.ladder * self.model.ladder_growth.powi(exp);
                        }
                    }
                    out.push(ParaLine {
                        item: i,
                        cost,
                        flagged,
                        end,
                    });
                } else if out.is_empty() && (end || hopeless) {
                    // Nothing fits at all: one emergency (overfull) line.
                    out.push(ParaLine {
                        item: i,
                        cost: EMERGENCY,
                        flagged,
                        end,
                    });
                }
            }
            if end || hopeless {
                break;
            }
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn best(
        &self,
        s: usize,
        line: usize,
        run: usize,
        d: usize,
        memo: &mut Memo,
        work: &mut u32,
        cancel: &CancellationToken,
    ) -> Result<(f64, Option<ParaLine>), LayoutError> {
        if d == 0 || s >= self.items.len() {
            return Ok((0.0, None));
        }
        let key = (s, line, run, d);
        if let Some(v) = memo.get(&key) {
            return Ok(*v);
        }
        let mut out = (f64::INFINITY, None);
        for c in self.lines_from(line, s, run, work, cancel)? {
            let rest = if c.end && c.item + 1 >= self.items.len() {
                0.0
            } else {
                let nr = if c.flagged { (run + 1).min(MAX_RUN) } else { 0 };
                self.best(
                    line_start(self.items, c.item),
                    line + 1,
                    nr,
                    d.saturating_sub(1),
                    memo,
                    work,
                    cancel,
                )?
                .0
            };
            let total = c.cost + rest;
            if total < out.0 - 1e-12 {
                out = (total, Some(c));
            }
        }
        memo.insert(key, out);
        Ok(out)
    }
}

/// Adobe-parity paragraph composer: windowed minimum-cost composition (module docs).
pub(crate) fn paragraph(
    items: &[Item],
    sums: &Sums,
    width: &dyn Fn(usize) -> f64,
    rules: &HyphenRules,
    ctx: &AdobeContext<'_>,
    cancel: &CancellationToken,
) -> Result<Vec<LineBreak>, LayoutError> {
    let ws = ctx.justify.word_spacing;
    let model = &PARA_MODEL;
    let p = Para {
        items,
        sums,
        width,
        model,
        min: ws.min,
        desired: ws.desired,
        max: ws.max,
        justified: ctx.justified,
        justify_last: ctx.justify_last,
        limit: usize::from(rules.limit).min(MAX_RUN),
    };
    let n = items.len();
    // The window is bounded by the line budget of the caller (items) so `d` never overflows.
    let window = if ctx.justified {
        model.depth
    } else {
        model.ragged_depth
    };
    let depth = if window == 0 {
        n.saturating_add(1)
    } else {
        window
    };
    let mut memo = Memo::new();
    let mut work = 0u32;
    let mut breaks = Vec::new();
    let (mut s, mut run, mut line) = (0usize, 0usize, 0usize);
    while s < n {
        let (_, next) = p.best(s, line, run, depth, &mut memo, &mut work, cancel)?;
        let Some(b) = next else { break };
        let wl = width(line);
        let (w, ..) = sums.measure(items, s, b.item);
        let spaces = sums.spaces(s, b.item);
        let scale = (spaces > EPS).then(|| 1.0 + (wl - w) / spaces);
        breaks.push(LineBreak {
            item: b.item,
            overfull: w > wl + EPS && scale.is_none_or(|x| x <= 0.0),
            ratio: scale,
        });
        if b.end && b.item + 1 >= n {
            break;
        }
        run = if b.flagged { (run + 1).min(MAX_RUN) } else { 0 };
        s = line_start(items, b.item);
        line += 1;
    }
    Ok(breaks)
}
