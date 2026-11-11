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
//! length term; ragged paragraphs minimise the cubed white space over the whole paragraph. Letter
//! spacing and glyph scaling ranges add to a line's stretch and shrink range (normalisation and
//! the past-range term); with both ranges zero the fitted word-only costs are unchanged bit for bit
//! (lane SMB-TYPE-FF-LIGATURE: ID-02 para ls-3, gs95 and ls-5 gs90 reproduced; UNVERIFIED beyond).
//!
//! Paragraph composer bounds (lane SMB-LAYOUT-COMPOSER-DEPTH-1, finding F-R56-1): the evaluation holds
//! no call-stack frame per line (an explicit stack, `Para::best`), works at most [`para_work_budget`]
//! steps and holds at most [`PARA_STATE_BUDGET`] states, past which it answers `BudgetExceeded`;
//! cancellation is read every 256 steps (tests/composer_depth.rs). When the ladder limit or the costs
//! leave no path at all, `paragraph` documents what it answers.
//!
//! Justification (`justify_line`): InDesign always fills a justified line to the measure: inside the
//! limits word spacing first, then letter spacing; past them the remainder is shared in proportion to
//! the ranges (ID-01: spaces at 42 % .. 2440 % of natural); a single-word line is letter-spaced; a
//! last line wider than the measure is compressed into it. Glyph scaling (lane SMB-TYPE-FF-LIGATURE,
//! oracle ID-02 gs sets): every advance times the glyph scale, after the words letters and glyphs
//! share by range, past the limits the words take the rest; the single-line candidates count the
//! glyph range as capacity.
use crate::LayoutError;
use crate::breaker::{AdobeParams, HyphenRules, LineBreak, Sums, check, forced, legal, line_start};
use crate::paragraph::Item;
use crate::typeset::{Justification, JustifyParams, Spacing, justify_metrics};
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
/// but the last, plus the hyphen of a hyphenated end and the left part of a ligature split there.
fn glyph_gaps(items: &[Item], s: usize, i: usize) -> usize {
    let first = items.get(s).map_or(0, |it| it.first);
    let end = items[s..i]
        .iter()
        .rev()
        .find(|it| it.kind == crate::paragraph::Kind::Box)
        .map_or(first, |it| it.end);
    end.saturating_sub(first).saturating_sub(1)
        + usize::from(items[i].flagged)
        + usize::from(items[i].end_part.is_some_and(|e| e.extra))
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
                    // Glyph scaling acts on every advance of the line, spaces included.
                    let gs = ctx.justify.glyph_scaling;
                    let shrink = spaces * (ws.desired - ws.min)
                        + letters * (ls.desired - ls.min)
                        + w * (gs.desired - gs.min);
                    let stretch = spaces * (ws.max - ws.desired)
                        + letters * (ls.max - ls.desired)
                        + w * (gs.max - gs.desired);
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
        // Overfull: past the shrink limits (words, letters, glyphs) and the spaces cannot close it.
        let overfull = pick.w > w_line + pick.shrink + EPS
            && pick.scale(w_line).is_none_or(|s| s <= 0.0);
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
    let gs = &jp.glyph_scaling;
    // With glyph scaling, past the limits the word spaces take the rest and glyphs and letters stay
    // at their limits (oracle ID-02 gs sets, last lines: glyph scale at its maximum, letters at
    // theirs, words 1.64 .. 2.63; UNVERIFIED outside those two sets).
    if gs.max > gs.min && spaces_pt > EPS {
        j.word_scale += rest / spaces_pt;
        j.residual_pt = 0.0;
        return j;
    }
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
        glyph_scale: 1.0,
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

/// The steps one paragraph may take: one per item a scan reads, one per state opened, linear in its
/// items up to a ceiling. Ordinary text takes steps in proportion to its items (a scan stops at the
/// line's end); a paragraph whose scans never stop (no glyph advance, a measure wider than the text)
/// takes their square. Declared from the readings in the lane notes: ordinary text up to
/// `Limits::max_glyphs` composes inside it in both windows, and the ceiling bounds the time to refuse.
pub(crate) fn para_work_budget(items: usize) -> u32 {
    const FLOOR: u32 = 1 << 24;
    const PER_ITEM: u32 = 1 << 10;
    const CEILING: u32 = 1 << 28;
    u32::try_from(items).unwrap_or(u32::MAX).saturating_mul(PER_ITEM).saturating_add(FLOOR).min(CEILING)
}
/// The states one paragraph may hold at once, memoised and open: the memory bound (about 130 bytes a
/// state, so about 270 MiB at the budget).
pub(crate) const PARA_STATE_BUDGET: usize = 1 << 21;
/// `d` of a paragraph composed over all its lines (`ragged_depth` 0). Every line advances its start,
/// so a paragraph of n items has fewer than n lines and a count from n + 1 could never reach 0: the
/// value of a state does not depend on it, and it stays out of the memo key.
const WHOLE_PARAGRAPH: usize = usize::MAX;

/// A state: line start, line class (`Para::flat_from`), hyphen run, lines left in the window.
type State = (usize, usize, usize, usize);
type Memo = std::collections::BTreeMap<State, (f64, Option<ParaLine>)>;

/// One step of the composer's budget.
fn para_step(budget: u32, cancel: &CancellationToken, work: &mut u32) -> Result<(), LayoutError> {
    check(cancel, work)?;
    if *work > budget {
        return Err(LayoutError::BudgetExceeded);
    }
    Ok(())
}

/// How far the feasible lines of one line start have been read (`Para::next_line`).
#[derive(Clone, Copy)]
struct Scan {
    next: usize,
    emitted: bool,
    done: bool,
}

/// One state under evaluation on the explicit stack.
struct Open {
    state: State,
    scan: Scan,
    /// The line whose rest of the paragraph is the state above this one.
    waiting: Option<ParaLine>,
    best: (f64, Option<ParaLine>),
}
impl Open {
    fn new(state: State) -> Self {
        Self {
            state,
            scan: Scan { next: state.0.saturating_add(1), emitted: false, done: false },
            waiting: None,
            best: (f64::INFINITY, None),
        }
    }
    /// The first candidate in scan order whose total beats the best by more than the epsilon.
    fn weigh(&mut self, c: ParaLine, rest: f64) {
        let total = c.cost + rest;
        if total < self.best.0 - 1e-12 {
            self.best = (total, Some(c));
        }
    }
}

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
    /// Letter spacing, glyph scaling and the letter reference width (their ranges add to the line's
    /// stretch and shrink range; zero ranges leave the fitted word-spacing model unchanged).
    letter: Spacing,
    glyph: Spacing,
    reference_pt: f64,
    /// From this line on every line has the width of this line (bit for bit), so the value of a state
    /// does not depend on which of those lines it is: they share one class in the memo key. A
    /// paragraph of n items has fewer than n lines, so reading `width` up to n covers every line.
    flat_from: usize,
    /// `para_work_budget` of this paragraph.
    budget: u32,
    /// Set once no path exists without it: a line start whose scan stops on a break the ladder limit
    /// skips goes on to the next break the limit allows, for the emergency line (`next_line`).
    rescue: bool,
}

impl Para<'_> {
    /// Stretch and shrink (points) letter spacing and glyph scaling add to the line `items[s..i)`
    /// of natural width `w` (lane SMB-TYPE-FF-LIGATURE; UNVERIFIED against InDesign beyond D5 ID-02).
    fn extra_ranges(&self, s: usize, i: usize, w: f64) -> (f64, f64) {
        let letters = glyph_gaps(self.items, s, i) as f64 * self.reference_pt;
        let (l, g) = (self.letter, self.glyph);
        (
            letters * (l.max - l.desired) + w * (g.max - g.desired),
            letters * (l.desired - l.min) + w * (g.desired - g.min),
        )
    }
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
        let (extra_stretch, extra_shrink) = self.extra_ranges(s, i, w);
        let cost = if w <= wl + EPS {
            let g = wl - w;
            let range = spaces * (self.max - self.desired).max(0.0) + extra_stretch;
            m.al * g.powf(m.pl) + m.bl * (g - range).max(0.0).powf(m.ql)
        } else {
            let o = w - wl;
            let floor = if last { m.last_min } else { m.tight_floor };
            let word_range = spaces * (self.desired - self.min);
            if !tight_ok(floor) || (self.desired - self.min <= EPS && extra_shrink <= EPS) {
                return None;
            }
            // Overflow normalised by the shrink range: the fitted word-only form when letters and
            // glyphs add none (bit-identical to the D5 fit), else per point of the total range.
            let normalised = if extra_shrink <= EPS {
                o * 0.2 / (self.desired - self.min)
            } else {
                o * 0.2 * spaces / (word_range + extra_shrink)
            };
            m.at * normalised.powf(m.pt)
                + m.bt * (o - word_range - extra_shrink).max(0.0).powf(m.qt)
        };
        let cost = cost.min(m.cap);
        Some(if m.square {
            (cost + 1.0) * (cost + 1.0)
        } else {
            cost
        })
    }

    /// The next feasible line from line start `s` (line number `line`), in item order; a hyphenation
    /// break past the ladder limit is skipped.
    fn next_line(
        &self,
        line: usize,
        s: usize,
        run: usize,
        scan: &mut Scan,
        work: &mut u32,
        cancel: &CancellationToken,
    ) -> Result<Option<ParaLine>, LayoutError> {
        let wl = (self.width)(line);
        let floor = if self.justified {
            self.model.tight_floor.min(self.model.last_min)
        } else {
            1.0
        };
        while !scan.done && scan.next < self.items.len() {
            let i = scan.next;
            scan.next += 1;
            para_step(self.budget, cancel, work)?;
            if !legal(self.items, i) {
                continue;
            }
            let (w, ..) = self.sums.measure(self.items, s, i);
            let spaces = self.sums.spaces(s, i);
            let flagged = self.items[i].flagged;
            let end = forced(&self.items[i]);
            // Past this point no later break can be compressed into the line.
            let hopeless = w - spaces * (1.0 - floor) > wl + EPS;
            let skipped = flagged && self.limit > 0 && run >= self.limit;
            let mut found = None;
            if !skipped {
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
                    found = Some(ParaLine {
                        item: i,
                        cost,
                        flagged,
                        end,
                    });
                } else if !scan.emitted && (end || hopeless) {
                    // Nothing fits at all: one emergency (overfull) line.
                    found = Some(ParaLine {
                        item: i,
                        cost: EMERGENCY,
                        flagged,
                        end,
                    });
                }
            }
            // Only under `rescue`: a skipped break cannot carry the emergency line, so the scan goes
            // on. It ends: the paragraph's last break is forced, and a forced break is never flagged.
            let goes_on = self.rescue && skipped && !scan.emitted;
            if (end || hopeless) && !goes_on {
                scan.done = true;
            }
            if found.is_some() {
                scan.emitted = true;
                return Ok(found);
            }
        }
        Ok(None)
    }

    /// The state after line `c` of `state`.
    fn after(&self, state: State, c: &ParaLine) -> State {
        let (_, line, run, d) = state;
        (
            line_start(self.items, c.item),
            (line + 1).min(self.flat_from),
            if c.flagged { (run + 1).min(MAX_RUN) } else { 0 },
            if d == WHOLE_PARAGRAPH { d } else { d.saturating_sub(1) },
        )
    }

    /// A state past the window or the paragraph: its value is 0 and nothing is evaluated.
    fn is_terminal(&self, state: State) -> bool {
        state.3 == 0 || state.0 >= self.items.len()
    }

    /// Minimum total cost from `root` and its first line. The definition, per state: the best of
    /// `line cost + value of the state after that line` over the feasible lines in `next_line`
    /// order, keeping the first that beats the current best by more than the epsilon (`Open::weigh`);
    /// 0 past the window or the paragraph. Evaluated depth first on a stack of its own, so a long
    /// paragraph takes heap bounded by `PARA_STATE_BUDGET`, never call stack. A value is a function
    /// of its state alone, so the order of evaluation cannot change a bit of it.
    fn best(
        &self,
        root: State,
        memo: &mut Memo,
        work: &mut u32,
        cancel: &CancellationToken,
    ) -> Result<(f64, Option<ParaLine>), LayoutError> {
        if self.is_terminal(root) {
            return Ok((0.0, None));
        }
        if let Some(v) = memo.get(&root) {
            return Ok(*v);
        }
        let mut stack = vec![Open::new(root)];
        let mut answer = (f64::INFINITY, None);
        while let Some(top) = stack.last_mut() {
            let (s, line, run, _) = top.state;
            let Some(c) = self.next_line(line, s, run, &mut top.scan, work, cancel)? else {
                let (state, value) = (top.state, top.best);
                stack.pop();
                memo.insert(state, value);
                if let Some(parent) = stack.last_mut()
                    && let Some(c) = parent.waiting.take()
                {
                    parent.weigh(c, value.0);
                }
                answer = value;
                continue;
            };
            if c.end && c.item + 1 >= self.items.len() {
                top.weigh(c, 0.0);
                continue;
            }
            let next = self.after(top.state, &c);
            if self.is_terminal(next) {
                top.weigh(c, 0.0);
            } else if let Some(v) = memo.get(&next) {
                top.weigh(c, v.0);
            } else {
                top.waiting = Some(c);
                para_step(self.budget, cancel, work)?;
                if memo.len().saturating_add(stack.len()) >= PARA_STATE_BUDGET {
                    return Err(LayoutError::BudgetExceeded);
                }
                stack.push(Open::new(next));
            }
        }
        Ok(answer)
    }
}

/// Adobe-parity paragraph composer: windowed minimum-cost composition (module docs).
///
/// No path from the first line (a ladder limit that skips every hyphen point in reach,
/// tests/composer_depth.rs): the search runs again under `Para::rescue`, where the emergency rule
/// reaches the next break the limit allows. A paragraph with a path from its first line never gets
/// there. Under `rescue`, a line start with no comparable total (a line cost past the range of a
/// float) is `InvalidGeometry`.
///
/// No path past the first line (a justified window can commit into a dead end its 7 lines did not
/// show): composition stops there and the caller's last line takes the rest of the paragraph's
/// glyphs, the excess reported as `hang_pt` and not marked overfull. That answer is kept as it was
/// (tests/composer_depth_diff.rs `a_window_dead_end_keeps_the_answer_of_the_base_sha`), an open
/// finding of the lane, because output does not change here.
pub(crate) fn paragraph(
    items: &[Item],
    sums: &Sums,
    width: &dyn Fn(usize) -> f64,
    rules: &HyphenRules,
    ctx: &AdobeContext<'_>,
    cancel: &CancellationToken,
) -> Result<Vec<LineBreak>, LayoutError> {
    paragraph_from(items, sums, width, rules, ctx, cancel, Resume::START, 0.0, &mut ParaStats::default())
}

/// `paragraph` from line `from.line`, which starts at item `from.item` after a hyphen run of
/// `from.run`: the breaks of that line and the lines after it. `stats` receives the steps taken and
/// the most states held at once.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paragraph_from(
    items: &[Item],
    sums: &Sums,
    width: &dyn Fn(usize) -> f64,
    rules: &HyphenRules,
    ctx: &AdobeContext<'_>,
    cancel: &CancellationToken,
    from: Resume,
    last_indent: f64,
    stats: &mut ParaStats,
) -> Result<Vec<LineBreak>, LayoutError> {
    let _ = last_indent;
    let ws = ctx.justify.word_spacing;
    let model = &PARA_MODEL;
    let n = items.len();
    let last_width = width(n).to_bits();
    let mut flat_from = n;
    while flat_from > 0 && width(flat_from - 1).to_bits() == last_width {
        flat_from -= 1;
    }
    let mut p = Para {
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
        letter: ctx.justify.letter_spacing,
        glyph: ctx.justify.glyph_scaling,
        reference_pt: ctx.justify.reference_space_pt,
        flat_from,
        budget: para_work_budget(n),
        rescue: false,
    };
    let window = if ctx.justified {
        model.depth
    } else {
        model.ragged_depth
    };
    let depth = if window == 0 { WHOLE_PARAGRAPH } else { window };
    let mut memo = Memo::new();
    let mut work = 0u32;
    let mut breaks = Vec::new();
    let (mut s, mut run, mut line) = (from.item, from.run, from.line);
    while s < n {
        let (_, next) = p.best((s, line.min(flat_from), run, depth), &mut memo, &mut work, cancel)?;
        let Some(b) = next else {
            if p.rescue {
                return Err(LayoutError::InvalidGeometry);
            }
            if !breaks.is_empty() {
                break;
            }
            p.rescue = true;
            stats.states = stats.states.max(memo.len());
            memo.clear();
            continue;
        };
        let wl = width(line);
        let (w, ..) = sums.measure(items, s, b.item);
        let spaces = sums.spaces(s, b.item);
        let scale = (spaces > EPS).then(|| 1.0 + (wl - w) / spaces);
        // Overfull: past the shrink limits (words, letters, glyphs) and the spaces cannot close it.
        let shrink = spaces * (p.desired - p.min).max(0.0) + p.extra_ranges(s, b.item, w).1;
        breaks.push(LineBreak {
            item: b.item,
            overfull: w > wl + shrink + EPS && scale.is_none_or(|x| x <= 0.0),
            ratio: scale,
        });
        if b.end && b.item + 1 >= n {
            break;
        }
        run = if b.flagged { (run + 1).min(MAX_RUN) } else { 0 };
        s = line_start(items, b.item);
        line += 1;
        // A window never looks back: the states before this line start are done with.
        if window > 0 {
            stats.states = stats.states.max(memo.len());
            memo = memo.split_off(&(s, 0, 0, 0));
        }
    }
    stats.states = stats.states.max(memo.len());
    stats.work = work;
    Ok(breaks)
}


/// Where a paragraph composition resumes (`paragraph_from`): the first item of line `line` and the
/// hyphen run of the lines before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Resume {
    pub item: usize,
    pub line: usize,
    pub run: usize,
}
impl Resume {
    pub(crate) const START: Self = Self { item: 0, line: 0, run: 0 };
}

/// Steps taken and the most states held at once by one composition (`paragraph_from`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ParaStats {
    pub work: u32,
    pub states: usize,
}

/// Lines above the paragraph's last line that a last-line re-break may change (lead ruling
/// 2026-11-11): the composer's window less one.
pub(crate) const PARA_TAIL_LINES: usize = PARA_MODEL.depth - 1;

/// The `Resume` of line `t` of `breaks`: its first item and the hyphen run ending before it, as
/// `paragraph_from` counts it (`MAX_RUN` at most).
pub(crate) fn resume_at(items: &[Item], breaks: &[LineBreak], t: usize) -> Resume {
    let run = breaks[..t].iter().fold(0, |run, b| if items[b.item].flagged { (run + 1).min(MAX_RUN) } else { 0 });
    Resume { item: if t == 0 { 0 } else { line_start(items, breaks[t - 1].item) }, line: t, run }
}

/// STU-TYP-185 `last_line_indent` re-break of the paragraph composer (`threaded_reflow`, after its
/// gate): `first` is the first pass at `width`; the result composes the paragraph again with its
/// last line narrowed by `last_indent`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn last_line_rebreak(
    items: &[Item],
    sums: &Sums,
    width: &dyn Fn(usize) -> f64,
    rules: &HyphenRules,
    ctx: &AdobeContext<'_>,
    cancel: &CancellationToken,
    first: Vec<LineBreak>,
    last_indent: f64,
    stats: &mut ParaStats,
) -> Result<Vec<LineBreak>, LayoutError> {
    let Some(l) = first.len().checked_sub(1) else {
        return Ok(first);
    };
    let narrowed = width(l) - last_indent;
    let narrow = |line: usize| if line == l { narrowed } else { width(line) };
    paragraph_from(items, sums, &narrow, rules, ctx, cancel, Resume::START, 0.0, stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paragraph::build_items;
    use crate::{ShapedParagraph, SourceSpan, SpaceParams};

    /// Cancellation stays honoured inside the pass, at any depth: the token is cancelled by the
    /// composer's own width read of a line far down the first descent, and the next check of the
    /// 256-step cadence answers it. Green on the base sha too (a pin, not a red proof).
    #[test]
    fn a_cancel_inside_the_pass_answers_canceled() {
        for justified in [false, true] {
            let text = "word ".repeat(2000);
            let p = ShapedParagraph::from_fixed_advance(&text, SourceSpan { start: 0, end: text.len() as u64 }, 6.0, 12.0, 10.0).unwrap();
            let items = build_items(&p, &text, SpaceParams::default(), None, true).unwrap();
            let sums = Sums::new(&items);
            let token = CancellationToken::default();
            let reads = std::cell::Cell::new(0usize);
            let width = |_: usize| {
                reads.set(reads.get() + 1);
                if reads.get() == items.len() + 2 + 500 {
                    token.cancel();
                }
                180.0
            };
            let jp = JustifyParams::default();
            let ctx = AdobeContext { params: AdobeParams::default(), justify: &jp, justified, justify_last: false };
            let got = paragraph(&items, &sums, &width, &HyphenRules::NONE, &ctx, &token);
            assert_eq!(got.unwrap_err(), LayoutError::Canceled, "justified {justified}");
        }
    }

    /// The measurement's toy text: `n` words of 2 to 6 glyphs, 6 pt each (tests/indent_spacing.rs).
    fn toy(n: usize) -> String {
        let words = ["aaaa", "bb", "cccccc", "ddd", "eeeee"];
        (0..n).map(|i| words[i % words.len()]).collect::<Vec<_>>().join(" ")
    }
    fn toy_items(text: &str, hyph: Option<(&crate::hyphenate::Hyphenator, &crate::hyphenate::HyphenationParams)>) -> Vec<Item> {
        let p = ShapedParagraph::from_fixed_advance(text, SourceSpan { start: 0, end: text.len() as u64 }, 6.0, 12.0, 10.0).unwrap();
        build_items(&p, text, SpaceParams::default(), hyph, true).unwrap()
    }

    /// Lead ruling 2026-11-11: the last-line re-break of a paragraph of 1800 words holds a number of
    /// states bounded by the lines it may change (`PARA_TAIL_LINES`), not by the paragraph, and takes
    /// no more steps than the first pass. Counts, never time. Before the ruling the re-break composed
    /// the whole paragraph with line l narrowed by number: every line number its own memo class,
    /// about 145 times the first pass's states on ragged text (diagnosis 2026-11-11).
    #[test]
    fn last_line_rebreak_work_is_bounded() {
        let text = toy(1800);
        let items = toy_items(&text, None);
        let sums = Sums::new(&items);
        let token = CancellationToken::default();
        let width = |_: usize| 180.0;
        let jp = JustifyParams::default();
        for justified in [false, true] {
            let ctx = AdobeContext { params: AdobeParams::default(), justify: &jp, justified, justify_last: false };
            let mut first_stats = ParaStats::default();
            let first = paragraph_from(&items, &sums, &width, &HyphenRules::NONE, &ctx, &token, Resume::START, 0.0, &mut first_stats).unwrap();
            let from = resume_at(&items, &first, (first.len() - 1).saturating_sub(PARA_TAIL_LINES));
            let mut stats = ParaStats::default();
            let b = last_line_rebreak(&items, &sums, &width, &HyphenRules::NONE, &ctx, &token, first.clone(), 100.0, &mut stats).unwrap();
            assert!(!b.is_empty());
            let tail_items = items.len() - from.item;
            assert!(stats.states <= 8 * tail_items, "justified {justified}: {stats:?}, tail items {tail_items}, first pass {first_stats:?}");
            assert!(stats.work <= first_stats.work, "justified {justified}: {stats:?} vs first pass {first_stats:?}");
        }
    }

    /// Lead ruling 2026-11-11: on justified (windowed) text the local tail re-break equals the
    /// re-break of the whole paragraph with the same last-line narrowing, including across a
    /// hyphen run at the seam (ladder limit 2).
    #[test]
    fn local_tail_equals_full_rebreak_when_justified() {
        let token = CancellationToken::default();
        let width = |_: usize| 180.0;
        let jp = JustifyParams::default();
        let ctx = AdobeContext { params: AdobeParams::default(), justify: &jp, justified: true, justify_last: false };
        let hp = crate::hyphenate::HyphenationParams { consecutive_limit: 2, hyphen_advance_pt: 6.0, ..Default::default() };
        let hyphen_text = "typographical justification hyphenation composition paragraphs ".repeat(60);
        let cases: Vec<(String, Option<(&crate::hyphenate::Hyphenator, &crate::hyphenate::HyphenationParams)>, HyphenRules)> = vec![
            (toy(600), None, HyphenRules::NONE),
            (toy(1800), None, HyphenRules::NONE),
            (hyphen_text.trim_end().to_string(), Some((crate::hyphenate::Hyphenator::en_us(), &hp)), HyphenRules { limit: 2, double_demerits: 10_000.0 }),
        ];
        for (text, hyph, rules) in cases {
            let items = toy_items(&text, hyph);
            let sums = Sums::new(&items);
            let first = paragraph(&items, &sums, &width, &rules, &ctx, &token).unwrap();
            let tail = last_line_rebreak(&items, &sums, &width, &rules, &ctx, &token, first, 100.0, &mut ParaStats::default()).unwrap();
            let full = paragraph_from(&items, &sums, &width, &rules, &ctx, &token, Resume::START, 100.0, &mut ParaStats::default()).unwrap();
            let bits = |b: &[LineBreak]| b.iter().map(|x| (x.item, x.overfull, x.ratio.map(f64::to_bits))).collect::<Vec<_>>();
            assert_eq!(bits(&tail), bits(&full), "{} items, hyphenated {}", items.len(), hyph.is_some());
            assert!(items.iter().any(|it| it.flagged) == hyph.is_some());
        }
    }
}
