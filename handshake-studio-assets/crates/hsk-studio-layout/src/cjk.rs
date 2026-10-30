//! Horizontal CJK composition (lane SMB-ID-CJK-KINSOKU-1): kinsoku line breaking (STU-TYP-137) and
//! mojikumi inter-character spacing (STU-TYP-138) for paragraphs whose named composer (STU-TYP-136,
//! Folio `hsk.studio.cjk_composition@1`) binds CJK tables. Vertical text, tate-chu-yoko, ruby,
//! kenten and warichu are not composed here.
//!
//! Data sources and licence: the character classes (cl-01..cl-19, cl-24, cl-26, cl-27), the shipped
//! `hard_kinsoku` / `soft_kinsoku` tables and the class-pair spacing rules of the Japanese base sets
//! are DATA read from the W3C Working Group Note "Requirements for Japanese Text Layout" (JLREQ,
//! https://www.w3.org/TR/jlreq/, sections 3.1 and 3.8 and Appendix A; W3C Document License); the two
//! Chinese defaults follow "Requirements for Chinese Text Layout" (CLREQ, https://www.w3.org/TR/clreq/,
//! section 3.1 punctuation width). No code was copied. Where those documents give no single value
//! (the seventeen InDesign set names carry no published numbers) the values below are declared
//! Studio readings, listed in the lane notes as spec deltas; equality with InDesign's Japanese
//! composer and its shipped sets is UNVERIFIED (D5, no oracle case).
//!
//! Model (all values in em of the paragraph: the caller's font size, else the widest CJK advance):
//! * Glyph boxes: brackets, commas and full stops are half-width glyphs whose other half is aki
//!   (JLREQ 3.1.4): an opening bracket's box is its advance less 1/2 em of aki before it, closing
//!   brackets, commas and full stops lose 1/2 em after, middle dots 1/4 em on each side.
//! * Boundaries between two glyphs of which at least one is CJK are break opportunities unless the
//!   kinsoku set forbids them (`cant_begin` after, `cant_end` before, two `cant_be_separated`
//!   characters, two digits under `rensuuji`); other boundaries keep UAX #14.
//! * Each CJK boundary carries the class-pair aki as glue (natural, shrink, stretch): line-end aki of
//!   the previous glyph before the break, the rest after it, line-start aki after a zero-width anchor
//!   box so it survives the break (Knuth-Plass item model).
//! * Hanging punctuation (burasagari): the break after a hanging character gets a negative-width
//!   penalty, so the line is measured without it; `forced` always hangs, `standard` / `regular` only
//!   when the character does not fit.
//! * Push-in / push-out (`kinsoku_type`) when the natural line end falls inside a forbidden run:
//!   single-line scope chooses explicitly per line; every-line scope biases the Knuth-Plass break
//!   penalties by [`KINSOKU_BIAS`]; `push_out_only` removes all aki shrink.
//! * `single_line` composes each line in isolation with the rule above; `every_line` runs
//!   Knuth-Plass over the CJK items. The Adobe-parity Latin constants (fitted to a Latin oracle) are
//!   not applied to CJK paragraphs.
use crate::breaker::{HyphenRules, KnuthPlassParams, LineBreak, Sums, check, forced, knuth_plass, legal, line_start};
use crate::paragraph::{Break, BreakKind, FORCED, Item, Kind, ShapedParagraph, SpaceParams, break_opportunities};
use crate::threaded_reflow::{LayoutProposal, ReflowRequest, reflow_core};
use crate::typeset::TypesetOptions;
use crate::LayoutError;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    StudioBurasagariType, StudioCjkCompositionRecords, StudioCompositionEngine, StudioCompositionScope, StudioKinsokuHangType,
    StudioKinsokuOrder, StudioKinsokuTable, StudioKinsokuType, StudioLayoutFlowRecords, StudioMojikumiBaseSet, StudioMojikumiClass,
    StudioMojikumiTable,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

type K = StudioMojikumiClass;
const EPS: f64 = 1e-9;
/// Penalty added to the disfavoured side of a kinsoku decision in every-line scope (declared).
pub const KINSOKU_BIAS: f64 = 50.0;
/// Penalty of an optional hanging break (`standard` / `regular`): a hang is taken only when it
/// gives the clearly better line (declared).
pub const HANG_COST: f64 = 100.0;
/// Characters `bunri_kinshi` adds to the active cant-be-separated set: the double period (U+2025
/// TWO DOT LEADER), the ellipsis (U+2026) and the double hyphen (U+2E40); declared reading.
pub const BUNRI_KINSHI_CHARS: &str = "\u{2025}\u{2026}\u{2E40}";

/// JLREQ cl-01 opening brackets.
pub const OPENING_BRACKETS: &str = "\u{2018}\u{201C}\u{FF08}\u{3014}\u{FF3B}\u{FF5B}\u{3008}\u{300A}\u{300C}\u{300E}\u{3010}\u{FF5F}\u{3018}\u{3016}\u{00AB}\u{301D}";
/// JLREQ cl-02 closing brackets.
pub const CLOSING_BRACKETS: &str = "\u{2019}\u{201D}\u{FF09}\u{3015}\u{FF3D}\u{FF5D}\u{3009}\u{300B}\u{300D}\u{300F}\u{3011}\u{FF60}\u{3019}\u{3017}\u{00BB}\u{301F}";
/// JLREQ cl-03 hyphens.
pub const HYPHENS: &str = "\u{2010}\u{301C}\u{30A0}\u{2013}";
/// JLREQ cl-04 dividing punctuation marks.
pub const DIVIDING_PUNCTUATION: &str = "\u{FF1F}\u{FF01}\u{203C}\u{2047}\u{2048}\u{2049}";
/// JLREQ cl-05 middle dots.
pub const MIDDLE_DOTS: &str = "\u{30FB}\u{FF1A}\u{FF1B}";
/// JLREQ cl-06 full stops.
pub const FULL_STOPS: &str = "\u{3002}\u{FF0E}";
/// JLREQ cl-07 commas.
pub const COMMAS: &str = "\u{3001}\u{FF0C}";
/// JLREQ cl-08 inseparable characters.
pub const INSEPARABLE: &str = "\u{2014}\u{2026}\u{2025}\u{3033}\u{3034}\u{3035}";
/// JLREQ cl-09 iteration marks.
pub const ITERATION_MARKS: &str = "\u{30FD}\u{30FE}\u{309D}\u{309E}\u{3005}\u{303B}";
/// JLREQ cl-10 prolonged sound mark.
pub const PROLONGED_SOUND_MARK: &str = "\u{30FC}";
/// JLREQ cl-11 small kana.
pub const SMALL_KANA: &str = "\u{3041}\u{3043}\u{3045}\u{3047}\u{3049}\u{3063}\u{3083}\u{3085}\u{3087}\u{308E}\u{3095}\u{3096}\u{30A1}\u{30A3}\u{30A5}\u{30A7}\u{30A9}\u{30C3}\u{30E3}\u{30E5}\u{30E7}\u{30EE}\u{30F5}\u{30F6}\u{31F0}\u{31F1}\u{31F2}\u{31F3}\u{31F4}\u{31F5}\u{31F6}\u{31F7}\u{31F8}\u{31F9}\u{31FA}\u{31FB}\u{31FC}\u{31FD}\u{31FE}\u{31FF}";
const PREFIXED: &str = "\u{FFE5}\u{FF04}\u{FFE1}\u{FF03}\u{20AC}\u{2116}";
const POSTFIXED: &str = "\u{00B0}\u{2032}\u{2033}\u{2103}\u{FFE0}\u{FF05}\u{2030}\u{33CB}\u{2113}";
const MATH_SYMBOLS: &str = "\u{FF1D}\u{2260}\u{2266}\u{2267}\u{2252}\u{2261}\u{FF1C}\u{FF1E}";
const MATH_OPERATORS: &str = "\u{FF0B}\u{2212}\u{00D7}\u{00F7}\u{00B1}";

/// The JLREQ class of a character (codepoint lookup). Characters JLREQ does not list as CJK are
/// western: ASCII digits `GroupedNumerals`, U+0020 `WesternWordSpace`, the rest `WesternCharacters`.
pub fn char_class(c: char) -> StudioMojikumiClass {
    let has = |set: &str| set.contains(c);
    if has(OPENING_BRACKETS) {
        K::OpeningBrackets
    } else if has(CLOSING_BRACKETS) {
        K::ClosingBrackets
    } else if has(HYPHENS) {
        K::Hyphens
    } else if has(DIVIDING_PUNCTUATION) {
        K::DividingPunctuation
    } else if has(MIDDLE_DOTS) {
        K::MiddleDots
    } else if has(FULL_STOPS) {
        K::FullStops
    } else if has(COMMAS) {
        K::Commas
    } else if has(INSEPARABLE) {
        K::InseparableCharacters
    } else if has(ITERATION_MARKS) {
        K::IterationMarks
    } else if has(PROLONGED_SOUND_MARK) {
        K::ProlongedSoundMark
    } else if has(SMALL_KANA) {
        K::SmallKana
    } else if has(PREFIXED) {
        K::PrefixedAbbreviations
    } else if has(POSTFIXED) {
        K::PostfixedAbbreviations
    } else if c == '\u{3000}' {
        K::IdeographicSpace
    } else if has(MATH_SYMBOLS) {
        K::MathSymbols
    } else if has(MATH_OPERATORS) {
        K::MathOperators
    } else if c.is_ascii_digit() {
        K::GroupedNumerals
    } else if c == ' ' {
        K::WesternWordSpace
    } else {
        match u32::from(c) {
            0x3041..=0x309F => K::Hiragana,
            0x30A0..=0x30FF | 0x31F0..=0x31FF => K::Katakana,
            0x3007 | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x3FFFF => K::Ideographic,
            // Full-width forms, Hangul syllables and jamo compose like ideographs (declared).
            0xFF10..=0xFF19 | 0xFF21..=0xFF3A | 0xFF41..=0xFF5A | 0xAC00..=0xD7AF | 0x1100..=0x11FF | 0x3130..=0x318F => K::Ideographic,
            _ => K::WesternCharacters,
        }
    }
}

/// True for the classes composed with CJK rules (everything but western text and line positions).
pub fn is_cjk(class: StudioMojikumiClass) -> bool {
    !matches!(class, K::GroupedNumerals | K::WesternWordSpace | K::WesternCharacters | K::LineStart | K::LineEnd)
}

fn is_digit(c: char) -> bool {
    c.is_ascii_digit() || ('\u{FF10}'..='\u{FF19}').contains(&c)
}

/// The four kinsoku character sets of one table.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct KinsokuSet {
    pub cant_begin: BTreeSet<char>,
    pub cant_end: BTreeSet<char>,
    pub cant_be_separated: BTreeSet<char>,
    pub hanging: BTreeSet<char>,
}
impl KinsokuSet {
    /// The sets of a Folio kinsoku table.
    pub fn from_table(t: &StudioKinsokuTable) -> Self {
        Self {
            cant_begin: t.cant_begin_line_chars.chars().collect(),
            cant_end: t.cant_end_line_chars.chars().collect(),
            cant_be_separated: t.cant_be_separated_chars.chars().collect(),
            hanging: t.hanging_punctuation_chars.chars().collect(),
        }
    }
    /// A shipped table (JLREQ 3.1.7 line-start / line-end prohibitions): `hard_kinsoku` forbids
    /// cl-02..cl-07 and cl-09..cl-11 at a line start, `soft_kinsoku` only cl-02 and cl-04..cl-07
    /// (small kana, the prolonged sound mark, iteration marks and hyphens may start a line); both
    /// forbid cl-01 at a line end, keep cl-08 together and hang the commas and full stops.
    pub fn shipped(name: &str) -> Option<Self> {
        let begin: String = match name {
            "hard_kinsoku" => [CLOSING_BRACKETS, HYPHENS, DIVIDING_PUNCTUATION, MIDDLE_DOTS, FULL_STOPS, COMMAS, ITERATION_MARKS, PROLONGED_SOUND_MARK, SMALL_KANA].concat(),
            "soft_kinsoku" => [CLOSING_BRACKETS, DIVIDING_PUNCTUATION, MIDDLE_DOTS, FULL_STOPS, COMMAS].concat(),
            _ => return None,
        };
        Some(Self {
            cant_begin: begin.chars().collect(),
            cant_end: OPENING_BRACKETS.chars().collect(),
            cant_be_separated: INSEPARABLE.chars().collect(),
            hanging: [COMMAS, FULL_STOPS].concat().chars().collect(),
        })
    }
}

/// Aki (spacing) in em: shrink limit, natural and stretch limit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aki {
    pub minimum_em: f64,
    pub desired_em: f64,
    pub maximum_em: f64,
}
impl Aki {
    pub const ZERO: Self = Self { minimum_em: 0.0, desired_em: 0.0, maximum_em: 0.0 };
    const fn new(minimum_em: f64, desired_em: f64, maximum_em: f64) -> Self {
        Self { minimum_em, desired_em, maximum_em }
    }
    const fn fixed(v: f64) -> Self {
        Self::new(v, v, v)
    }
    fn plus(self, o: Self) -> Self {
        Self::new(self.minimum_em + o.minimum_em, self.desired_em + o.desired_em, self.maximum_em + o.maximum_em)
    }
    fn is_zero(self) -> bool {
        self == Self::ZERO
    }
    fn max_with(self, o: Self) -> Self {
        if o.desired_em > self.desired_em { o } else { self }
    }
}

/// How punctuation of one group is set at a line end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineEnd {
    /// Half-width: no aki after it (the set names' `one_half_em`).
    Half,
    /// Full-width: its 1/2 em aki kept, not compressed (`one_em`).
    Full,
    /// Not fixed: 1/2 em that line adjustment may compress to none (`no_float`; declared reading).
    Mid,
    /// 1/2 em compressible to none (CLREQ line-end compression).
    Compressible,
}

/// Resolved spacing rules of one mojikumi table.
#[derive(Clone, Debug, PartialEq)]
pub struct MojikumiRules {
    pub base: StudioMojikumiBaseSet,
    /// First-line indent of the paragraph, em.
    pub indent_em: f64,
    /// Extra aki before an opening bracket that starts the paragraph, em.
    pub opening_start_extra_em: f64,
    solid: bool,
    /// Punctuation aki may be compressed mid-line.
    compress: bool,
    /// Consecutive punctuation shares one aki (JLREQ 3.1.4).
    combine: bool,
    /// Line-end setting of closing brackets, commas and full stops.
    line_end: [LineEnd; 3],
    overrides: BTreeMap<(K, K), Aki>,
}
impl MojikumiRules {
    /// The rules of a base set (no overrides).
    pub fn base(set: StudioMojikumiBaseSet) -> Self {
        use LineEnd::*;
        use StudioMojikumiBaseSet as S;
        let japanese = |indent_em: f64, extra: f64, line_end: [LineEnd; 3]| Self {
            base: set,
            indent_em,
            opening_start_extra_em: extra,
            solid: false,
            compress: true,
            combine: true,
            line_end,
            overrides: BTreeMap::new(),
        };
        // Groups: [closing brackets, commas, full stops]; "uke" = closing brackets and commas,
        // "period" = full stops, "all" = the three. Unnamed groups (declared readings): the
        // `uke_one_half_em` sets keep the mid-line rule for full stops, the `uke_no_float` sets set
        // full stops half-width, the `period_one_em` sets set the uke group half-width.
        let all = |m| [m, m, m];
        let uke = |m| [m, m, Mid];
        let period = |m| [Half, Half, m];
        match set {
            S::Nothing => Self { solid: true, compress: false, combine: false, ..japanese(0.0, 0.0, all(Half)) },
            S::LineEndAllOneHalfEm => japanese(0.0, 0.0, all(Half)),
            S::OneEmIndentLineEndUkeOneHalfEm => japanese(1.0, 0.0, uke(Half)),
            S::OneOrOneHalfEmIndentLineEndUkeOneHalfEm => japanese(1.0, 0.5, uke(Half)),
            S::OneOrOneHalfEmIndentLineEndAllOneEm => japanese(1.0, 0.5, all(Full)),
            S::OneEmIndentLineEndAllOneEm => japanese(1.0, 0.0, all(Full)),
            S::OneEmIndentLineEndAllNoFloat => japanese(1.0, 0.0, all(Mid)),
            S::OneEmIndentLineEndUkeNoFloat => japanese(1.0, 0.0, uke(Mid)).with_period(Half),
            S::OneOrOneHalfEmIndentLineEndUkeNoFloat => japanese(1.0, 0.5, uke(Mid)).with_period(Half),
            S::OneEmIndentLineEndAllOneHalfEm => japanese(1.0, 0.0, all(Half)),
            S::LineEndAllOneEm => japanese(0.0, 0.0, all(Full)),
            S::LineEndUkeNoFloat => japanese(0.0, 0.0, uke(Mid)).with_period(Half),
            S::OneOrOneHalfEmIndentLineEndPeriodOneEm => japanese(1.0, 0.5, period(Full)),
            S::OneEmIndentLineEndPeriodOneEm => japanese(1.0, 0.0, period(Full)),
            S::LineEndPeriodOneEm => japanese(0.0, 0.0, period(Full)),
            // CLREQ 3.1: Simplified Chinese sets punctuation full-width, consecutive punctuation
            // shares one aki and line-end punctuation may compress to half-width.
            S::SimplifiedChineseDefault => Self { compress: false, ..japanese(2.0, 0.0, all(Compressible)) },
            // Traditional Chinese centres punctuation in a full em box: no compression, no
            // combining, full-width at the line end.
            S::TraditionalChineseDefault => Self { compress: false, combine: false, ..japanese(2.0, 0.0, all(Full)) },
        }
    }
    /// The full stop group's line end (the `uke_no_float` sets do not name it; declared).
    fn with_period(mut self, m: LineEnd) -> Self {
        self.line_end[2] = m;
        self
    }
    /// The rules of a Folio table: its base set with the overrides applied. `use_cid_mojikumi` is
    /// `UnsupportedComposition` (Studio has no glyph-CID class mapping).
    pub fn from_table(t: &StudioMojikumiTable) -> Result<Self, LayoutError> {
        if t.use_cid_mojikumi {
            return Err(LayoutError::UnsupportedComposition);
        }
        let mut r = Self::base(t.based_on);
        for o in &t.aki_override_list {
            let ok = |v: f64| v.is_finite() && v >= 0.0;
            if !(ok(o.minimum_em) && ok(o.desired_em) && ok(o.maximum_em)) || o.minimum_em > o.desired_em || o.desired_em > o.maximum_em {
                return Err(LayoutError::InvalidInput);
            }
            r.overrides.insert((o.previous, o.next), Aki::new(o.minimum_em, o.desired_em, o.maximum_em));
        }
        Ok(r)
    }
    fn punct_after(&self, half: f64, min_full: bool) -> Aki {
        Aki::new(if self.compress && !min_full { 0.0 } else { half }, half, half)
    }
    /// Aki after a glyph of class `a` before a non-punctuation neighbour, em.
    fn after(&self, a: K) -> Aki {
        match a {
            K::ClosingBrackets | K::Commas => self.punct_after(0.5, false),
            // JLREQ keeps the full stop's half em mid-line (declared: not compressed).
            K::FullStops => self.punct_after(0.5, true),
            K::MiddleDots => self.punct_after(0.25, false),
            _ => Aki::ZERO,
        }
    }
    fn before(&self, b: K) -> Aki {
        match b {
            K::OpeningBrackets => self.punct_after(0.5, false),
            K::MiddleDots => self.punct_after(0.25, false),
            _ => Aki::ZERO,
        }
    }
    /// Mid-line aki between a glyph of class `a` and one of class `b`, em.
    pub fn pair(&self, a: K, b: K) -> Aki {
        if let Some(o) = self.overrides.get(&(a, b)) {
            return *o;
        }
        if self.solid {
            return Aki::ZERO;
        }
        let follower = |c: K| matches!(c, K::ClosingBrackets | K::Commas | K::FullStops);
        let (after, before) = (self.after(a), self.before(b));
        if self.combine {
            if follower(a) && (follower(b) || b == K::MiddleDots) {
                return before;
            }
            if a == K::OpeningBrackets && b == K::OpeningBrackets {
                return Aki::ZERO;
            }
            if !after.is_zero() && !before.is_zero() {
                return after.max_with(before);
            }
        }
        if after.is_zero() && before.is_zero() {
            let (ca, cb) = (is_cjk(a), is_cjk(b));
            let western = |c: K| matches!(c, K::GroupedNumerals | K::WesternCharacters);
            return if ca && cb {
                // JLREQ 3.8 inter-character expansion for justification (declared 1/4 em).
                Aki::new(0.0, 0.0, 0.25)
            } else if (ca && western(b)) || (cb && western(a)) {
                // JLREQ 3.2.2 quarter em between Japanese and western text.
                Aki::new(0.0, 0.25, 0.5)
            } else {
                Aki::ZERO
            };
        }
        after.plus(before)
    }
    /// Aki after a glyph of class `a` that ends a line, em.
    pub fn line_end(&self, a: K) -> Aki {
        if let Some(o) = self.overrides.get(&(a, K::LineEnd)) {
            return *o;
        }
        if self.solid {
            return Aki::ZERO;
        }
        let group = match a {
            K::ClosingBrackets => 0,
            K::Commas => 1,
            K::FullStops => 2,
            K::MiddleDots => return self.after(a),
            _ => return Aki::ZERO,
        };
        match self.line_end[group] {
            LineEnd::Half => Aki::ZERO,
            LineEnd::Full => Aki::fixed(0.5),
            // Unfixed at the line end: the half em may compress away (full stops included, which
            // keep their half em mid-line).
            LineEnd::Mid | LineEnd::Compressible => Aki::new(0.0, 0.5, 0.5),
        }
    }
    /// Aki before a glyph of class `b` that starts a line, em (an opening bracket starts a line
    /// half-width unless an override says otherwise).
    pub fn line_start(&self, b: K) -> Aki {
        self.overrides.get(&(K::LineStart, b)).copied().unwrap_or(Aki::ZERO)
    }
    /// Aki inside the glyph box of class `c` (before, after), em.
    pub fn embedded(&self, c: K) -> (f64, f64) {
        if self.solid {
            return (0.0, 0.0);
        }
        match c {
            K::OpeningBrackets => (0.5, 0.0),
            K::ClosingBrackets | K::Commas | K::FullStops => (0.0, 0.5),
            K::MiddleDots => (0.25, 0.25),
            _ => (0.0, 0.0),
        }
    }
}

/// Hanging punctuation mode: the stronger of `burasagari_type` and `kinsoku_hang_type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HangMode {
    Off,
    /// Hang only when the character does not fit (`standard` / `regular`).
    Optional,
    /// Always hang at a line end (`forced` / `force`).
    Forced,
}
impl HangMode {
    pub fn of(b: StudioBurasagariType, h: StudioKinsokuHangType) -> Self {
        let a = match b {
            StudioBurasagariType::None => Self::Off,
            StudioBurasagariType::Standard => Self::Optional,
            StudioBurasagariType::Forced => Self::Forced,
        };
        let k = match h {
            StudioKinsokuHangType::None => Self::Off,
            StudioKinsokuHangType::Regular => Self::Optional,
            StudioKinsokuHangType::Force => Self::Forced,
        };
        a.max(k)
    }
}

/// Everything one CJK paragraph composes with.
#[derive(Clone, Debug, PartialEq)]
pub struct CjkParagraph {
    pub scope: StudioCompositionScope,
    /// `None` = no kinsoku table bound (every CJK boundary may break).
    pub kinsoku: Option<KinsokuSet>,
    pub mojikumi: MojikumiRules,
    pub kinsoku_type: StudioKinsokuType,
    pub hang: HangMode,
    pub bunri_kinshi: bool,
    pub rensuuji: bool,
    pub kinsoku_order: StudioKinsokuOrder,
}
impl CjkParagraph {
    /// A paragraph with `kinsoku`, `mojikumi` and the given scope; other attributes at their
    /// mildest (push-in first, no hanging, no bunri kinshi, no rensuuji).
    pub fn new(scope: StudioCompositionScope, kinsoku: Option<KinsokuSet>, mojikumi: MojikumiRules) -> Self {
        Self {
            scope,
            kinsoku,
            mojikumi,
            kinsoku_type: StudioKinsokuType::PushInFirst,
            hang: HangMode::Off,
            bunri_kinshi: false,
            rensuuji: false,
            kinsoku_order: StudioKinsokuOrder::PushIn,
        }
    }
    fn breakable(&self, a: char, b: char) -> bool {
        if self.rensuuji && is_digit(a) && is_digit(b) {
            return false;
        }
        let bunri = |c: char| self.bunri_kinshi && BUNRI_KINSHI_CHARS.contains(c);
        match &self.kinsoku {
            None => !(bunri(a) && bunri(b)),
            Some(k) => {
                let sep = |c: char| k.cant_be_separated.contains(&c) || bunri(c);
                !(k.cant_begin.contains(&b) || k.cant_end.contains(&a) || (sep(a) && sep(b)))
            }
        }
    }
    fn hangs(&self, c: char) -> bool {
        self.hang != HangMode::Off && self.kinsoku.as_ref().is_some_and(|k| k.hanging.contains(&c))
    }
}

/// The paragraph of style `style_id` per the Folio CJK records (STU-TYP-136): `None` when the style
/// has no binding. A composer or table name that resolves nowhere is `InvalidInput` (never a silent
/// fallback); an engine other than `latin_cjk` and `use_cid_mojikumi` are `UnsupportedComposition`.
pub fn resolve_cjk_paragraph(records: &StudioCjkCompositionRecords, style_id: &str) -> Result<Option<CjkParagraph>, LayoutError> {
    let Some(b) = records.binding(style_id) else {
        return Ok(None);
    };
    let c = records.composer(&b.composer_name).ok_or(LayoutError::InvalidInput)?;
    if c.composition_engine != StudioCompositionEngine::LatinCjk {
        return Err(LayoutError::UnsupportedComposition);
    }
    let kinsoku = match &c.kinsoku_table {
        None => None,
        Some(name) => Some(match records.kinsoku_table(name) {
            Some(t) => KinsokuSet::from_table(t),
            None => KinsokuSet::shipped(name).ok_or(LayoutError::InvalidInput)?,
        }),
    };
    let mojikumi = match &c.mojikumi_table {
        None => MojikumiRules::base(StudioMojikumiBaseSet::Nothing),
        Some(name) => match records.mojikumi_table(name) {
            Some(t) => MojikumiRules::from_table(t)?,
            None => MojikumiRules::base(StudioMojikumiBaseSet::from_name(name).ok_or(LayoutError::InvalidInput)?),
        },
    };
    Ok(Some(CjkParagraph {
        scope: c.composition_scope,
        kinsoku,
        mojikumi,
        kinsoku_type: b.kinsoku_type,
        hang: HangMode::of(b.burasagari_type, b.kinsoku_hang_type),
        bunri_kinshi: b.bunri_kinshi,
        rensuuji: b.rensuuji,
        kinsoku_order: b.kinsoku_order,
    }))
}

/// The CJK item list of one paragraph plus what the line finisher needs.
pub(crate) struct CjkItems {
    pub items: Vec<Item>,
    /// Item indices of the glue at kinsoku-forbidden boundaries, ascending.
    forbidden: Vec<usize>,
    /// Per glyph: embedded aki before and after, points.
    embedded: Vec<(f64, f64)>,
    /// The paragraph end may hang its last character only when it does not fit.
    optional_end_hang: bool,
}

fn glue(w: f64, y: f64, z: f64, first: usize, end: usize) -> Item {
    Item {
        kind: Kind::Glue,
        w,
        y,
        z,
        inf_stretch: false,
        p: 0.0,
        first,
        end,
        breakable: false,
        trailing: false,
        flagged: false,
        end_part: None,
        head: None,
    }
}
fn aki_glue(a: Aki, em: f64, at: usize) -> Item {
    glue(a.desired_em * em, (a.maximum_em - a.desired_em).max(0.0) * em, (a.desired_em - a.minimum_em).max(0.0) * em, at, at)
}
fn boxed(w: f64, first: usize, end: usize) -> Item {
    Item { kind: Kind::Box, ..glue(w, 0.0, 0.0, first, end) }
}
fn penalty(p: f64, w: f64, at: usize) -> Item {
    Item { kind: Kind::Penalty, p, breakable: true, ..glue(w, 0.0, 0.0, at, at) }
}
fn is_hanging_space(c: char) -> bool {
    c.is_whitespace() && !matches!(c, '\u{00A0}' | '\u{2007}' | '\u{202F}')
}

/// Builds the CJK item list (module notes). `em_pt` > 0 is the paragraph's em; 0 = the widest
/// advance of the paragraph's CJK glyphs. Glyph clusters are checked like the Latin item builder.
pub(crate) fn build(p: &ShapedParagraph, text: &str, c: &CjkParagraph, em_pt: f64, space: SpaceParams) -> Result<CjkItems, LayoutError> {
    let (s, e) = (
        usize::try_from(p.source.start).map_err(|_| LayoutError::InvalidInput)?,
        usize::try_from(p.source.end).map_err(|_| LayoutError::InvalidInput)?,
    );
    let slice = text.get(s..e).ok_or(LayoutError::InvalidInput)?;
    let g = &p.glyphs;
    let mut previous_end = s;
    let mut chars: Vec<(char, char)> = Vec::with_capacity(g.len());
    for glyph in g {
        let (a, b) = (usize::try_from(glyph.cluster.start).map_err(|_| LayoutError::InvalidInput)?, usize::try_from(glyph.cluster.end).map_err(|_| LayoutError::InvalidInput)?);
        if a < previous_end || b <= a || b > e || !glyph.advance_pt.is_finite() || glyph.advance_pt < 0.0 {
            return Err(LayoutError::InvalidInput);
        }
        let cluster = text.get(a..b).ok_or(LayoutError::InvalidInput)?;
        let (Some(first), Some(last)) = (cluster.chars().next(), cluster.chars().next_back()) else {
            return Err(LayoutError::InvalidInput);
        };
        chars.push((first, last));
        previous_end = a;
    }
    if !(em_pt.is_finite() && em_pt >= 0.0) {
        return Err(LayoutError::InvalidInput);
    }
    let em = if em_pt > 0.0 {
        em_pt
    } else {
        g.iter().zip(&chars).filter(|(_, (f, _))| is_cjk(char_class(*f))).map(|(x, _)| x.advance_pt).fold(0.0, f64::max)
    };
    let opps: Vec<Break> = break_opportunities(slice).into_iter().map(|b| Break { byte: b.byte + s, kind: b.kind }).collect();
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
    let space_at = |i: usize| {
        let cl = &g[i].cluster;
        text.get(cl.start as usize..cl.end as usize).is_some_and(|t| t.chars().all(is_hanging_space))
    };
    let m = &c.mojikumi;
    let embedded: Vec<(f64, f64)> = chars
        .iter()
        .map(|(f, _)| {
            let (b, a) = m.embedded(char_class(*f));
            (b * em, a * em)
        })
        .collect();
    let box_w = |i: usize| (g[i].advance_pt - embedded[i].0 - embedded[i].1).max(0.0);
    let n = g.len();
    let mut items: Vec<Item> = Vec::with_capacity(n.saturating_mul(4).saturating_add(2));
    let mut forbidden = Vec::new();
    if n > 0 {
        let mut indent_pt = m.indent_em * em;
        if char_class(chars[0].0) == K::OpeningBrackets {
            indent_pt += m.opening_start_extra_em * em;
        }
        if indent_pt > 0.0 {
            items.push(boxed(indent_pt, 0, 0));
        }
    }
    // Finishing glue + forced break; only whitespace glue becomes trailing (aki glue keeps its width).
    let finish = |items: &mut Vec<Item>, at: usize, hang_w: f64| {
        if let Some(last) = items.last_mut()
            && last.kind == Kind::Glue
            && last.end > last.first
        {
            last.trailing = true;
            last.breakable = false;
        }
        items.push(Item { inf_stretch: true, ..glue(0.0, 0.0, 0.0, at, at) });
        items.push(penalty(FORCED, hang_w, at));
    };
    let mut i = 0;
    while i < n {
        if space_at(i) {
            let mut j = i + 1;
            while j < n && space_at(j) && opp_after(j - 1).is_none() {
                j += 1;
            }
            let w: f64 = g[i..j].iter().map(|x| x.advance_pt).sum();
            let after = opp_after(j - 1);
            items.push(Item { breakable: after == Some(BreakKind::Allowed), ..glue(w, w * space.stretch, w * space.shrink, i, j) });
            if j < n && after == Some(BreakKind::Mandatory) {
                finish(&mut items, j, 0.0);
            }
            i = j;
            continue;
        }
        items.push(boxed(box_w(i), i, i + 1));
        let j = i + 1;
        if j < n {
            let after = opp_after(i);
            if after == Some(BreakKind::Mandatory) {
                finish(&mut items, j, 0.0);
            } else if !space_at(j) {
                let (a, b) = (chars[i].1, chars[j].0);
                let (ca, cb) = (char_class(a), char_class(b));
                if !(is_cjk(ca) || is_cjk(cb)) {
                    if after == Some(BreakKind::Allowed) {
                        items.push(penalty(0.0, 0.0, j));
                    }
                } else if !c.breakable(a, b) {
                    forbidden.push(items.len());
                    items.push(aki_glue(m.pair(ca, cb), em, j));
                } else {
                    let (mid, le, ls) = (m.pair(ca, cb), m.line_end(ca), m.line_start(cb));
                    items.push(aki_glue(le, em, j));
                    if c.hangs(a) {
                        let hang_w = -(box_w(i) + le.desired_em * em);
                        if c.hang == HangMode::Forced {
                            items.push(penalty(0.0, hang_w, j));
                        } else {
                            items.push(penalty(HANG_COST, hang_w, j));
                            items.push(penalty(0.0, 0.0, j));
                        }
                    } else {
                        items.push(penalty(0.0, 0.0, j));
                    }
                    let rest = Aki::new(
                        mid.minimum_em - le.minimum_em - ls.minimum_em,
                        mid.desired_em - le.desired_em - ls.desired_em,
                        mid.maximum_em - le.maximum_em - ls.maximum_em,
                    );
                    // Natural may be negative (line-end aki above the mid-line aki); stretch and
                    // shrink are clamped to non-negative (declared approximation).
                    items.push(glue(
                        rest.desired_em * em,
                        (mid.maximum_em - mid.desired_em - (le.maximum_em - le.desired_em) - (ls.maximum_em - ls.desired_em)).max(0.0) * em,
                        (mid.desired_em - mid.minimum_em - (le.desired_em - le.minimum_em) - (ls.desired_em - ls.minimum_em)).max(0.0) * em,
                        j,
                        j,
                    ));
                    if !ls.is_zero() {
                        items.push(boxed(0.0, j, j));
                        items.push(aki_glue(ls, em, j));
                    }
                }
            }
        }
        i = j;
    }
    // Paragraph end: the last glyph keeps its line-end aki; a hanging last character may hang.
    let mut hang_w = 0.0;
    let mut optional_end_hang = false;
    if let Some(last) = (0..n).last().filter(|k| !space_at(*k)) {
        let a = chars[last].1;
        let le = m.line_end(char_class(a));
        if !le.is_zero() {
            items.push(aki_glue(le, em, n));
        }
        if c.hangs(a) {
            hang_w = -(box_w(last) + le.desired_em * em);
            optional_end_hang = c.hang == HangMode::Optional;
        }
    }
    finish(&mut items, n, hang_w);
    // Every-line scope: bias the breaks on each side of a forbidden run; push_out_only: no shrink.
    if c.kinsoku_type == StudioKinsokuType::PushOutOnly {
        for it in items.iter_mut().filter(|it| it.kind == Kind::Glue && it.end == it.first) {
            it.z = 0.0;
        }
    }
    for &f in &forbidden {
        let candidate = |it: &Item| it.kind == Kind::Penalty && it.p < f64::INFINITY && it.p > FORCED;
        let before = (0..f).rev().find(|k| candidate(&items[*k]));
        let after = (f + 1..items.len()).find(|k| candidate(&items[*k]));
        let target = match c.kinsoku_type {
            StudioKinsokuType::PushInFirst => before,
            StudioKinsokuType::PushOutFirst | StudioKinsokuType::PushOutOnly => after,
            StudioKinsokuType::PrioritizeAdjustment => None,
        };
        if let Some(t) = target
            && items[t].p < KINSOKU_BIAS
        {
            items[t].p += KINSOKU_BIAS;
        }
    }
    Ok(CjkItems { items, forbidden, embedded, optional_end_hang })
}

/// Line breaks of a CJK paragraph: Knuth-Plass for `every_line`, the per-line kinsoku rule for
/// `single_line`.
pub(crate) fn break_lines(
    ci: &CjkItems,
    sums: &Sums,
    width: &dyn Fn(usize) -> f64,
    c: &CjkParagraph,
    cancel: &CancellationToken,
) -> Result<Vec<LineBreak>, LayoutError> {
    match c.scope {
        StudioCompositionScope::EveryLine => knuth_plass(&ci.items, sums, width, &KnuthPlassParams::default(), &HyphenRules::NONE, cancel),
        StudioCompositionScope::SingleLine => single_line(ci, sums, width, c, cancel),
    }
}

fn single_line(
    ci: &CjkItems,
    sums: &Sums,
    width: &dyn Fn(usize) -> f64,
    c: &CjkParagraph,
    cancel: &CancellationToken,
) -> Result<Vec<LineBreak>, LayoutError> {
    let items = &ci.items;
    let n = items.len();
    let (mut breaks, mut start, mut work) = (Vec::new(), 0usize, 0u32);
    while start < n {
        let w_line = width(breaks.len());
        let (mut fit, mut over) = (None::<usize>, None::<usize>);
        let mut i = start;
        while i < n {
            check(cancel, &mut work)?;
            if i > start && legal(items, i) {
                let (w, ..) = sums.measure(items, start, i);
                if w <= w_line + EPS {
                    fit = Some(i);
                    if forced(&items[i]) {
                        break;
                    }
                } else {
                    over = Some(i);
                    break;
                }
            }
            i += 1;
        }
        let pick = match (fit, over) {
            (Some(f), _) if forced(&items[f]) => f,
            (Some(f), None) => f,
            (None, Some(o)) => o,
            (Some(f), Some(o)) => {
                let (wo, _, zo, _) = sums.measure(items, start, o);
                let push_in = wo - zo <= w_line + EPS;
                let kinsoku = ci.forbidden.iter().any(|x| *x > f && *x < o);
                if !push_in || !kinsoku {
                    f
                } else {
                    match c.kinsoku_type {
                        StudioKinsokuType::PushInFirst => o,
                        StudioKinsokuType::PushOutFirst | StudioKinsokuType::PushOutOnly => f,
                        StudioKinsokuType::PrioritizeAdjustment => {
                            let (wf, ..) = sums.measure(items, start, f);
                            let (shrink, stretch) = (wo - w_line, w_line - wf);
                            if shrink < stretch - EPS || ((shrink - stretch).abs() <= EPS && c.kinsoku_order == StudioKinsokuOrder::PushIn) {
                                o
                            } else {
                                f
                            }
                        }
                    }
                }
            }
            (None, None) => n - 1,
        };
        let (w, _, z, _) = sums.measure(items, start, pick);
        breaks.push(LineBreak { item: pick, overfull: w - z > w_line + EPS, ratio: None });
        start = line_start(items, pick);
        if forced(&items[pick]) && pick + 1 >= n {
            break;
        }
    }
    Ok(breaks)
}

/// A glyph boundary whose spacing differs from the shaped advances: the glyph after `glyph` is
/// placed `delta_pt` further right (negative = closer) than `glyph`'s shaped advance alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CjkGap {
    /// Paragraph-local glyph index.
    pub glyph: usize,
    pub delta_pt: f64,
}

/// How one CJK line is set.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LineSet {
    /// Offset of the first glyph's origin from the line start (indent + line-start aki - the aki
    /// inside the first glyph's box), points.
    pub start_offset_pt: f64,
    /// Space the indent and line-start aki take before the first glyph box.
    pub lead_pt: f64,
    /// Set width from the first glyph box to the line end, hanging material excluded.
    pub width_pt: f64,
    pub hang_pt: f64,
    pub ratio: f64,
    pub overfull: bool,
    pub gaps: Vec<CjkGap>,
}

/// Sets the line `items[s..b]` (ending with a break at item `b`) into `available` points:
/// `justify` fills the measure through the aki glue (stretch past its maximum when it must; shrink
/// to its minimum at most), a line wider than the measure is compressed as far as the aki allows.
pub(crate) fn finish_line(ci: &CjkItems, p: &ShapedParagraph, s: usize, b: usize, available: f64, justify: bool) -> LineSet {
    let items = &ci.items;
    let pen_w = if items[b].kind == Kind::Penalty { items[b].w } else { 0.0 };
    let mut used_pen = pen_w;
    let measured = |pen: f64| {
        let mut w = 0.0;
        let (mut y, mut z) = (0.0, 0.0);
        for it in &items[s..b] {
            match it.kind {
                Kind::Box => w += it.w,
                Kind::Glue if !it.trailing && !it.inf_stretch => {
                    w += it.w;
                    y += it.y;
                    z += it.z;
                }
                _ => {}
            }
        }
        (w + pen, y, z)
    };
    // An optional paragraph-end hang applies only when the character does not fit.
    if ci.optional_end_hang && forced(&items[b]) && b + 1 == items.len() && measured(0.0).0 <= available + EPS {
        used_pen = 0.0;
    }
    let (w, y, z) = measured(used_pen);
    let r = if w > available + EPS {
        if z > EPS { ((available - w) / z).max(-1.0) } else { 0.0 }
    } else if justify && w < available - EPS && y > EPS {
        (available - w) / y
    } else {
        0.0
    };
    let set = |it: &Item| it.w + if r > 0.0 { r * it.y } else { r * it.z };
    // Walk the line with a pen: glyph origins, then the deltas against the shaped advances.
    let mut pen = 0.0;
    let mut origins: Vec<(usize, f64)> = Vec::new();
    let mut lead = None;
    for it in &items[s..b] {
        match it.kind {
            Kind::Box if it.end > it.first => {
                if lead.is_none() {
                    lead = Some(pen);
                }
                origins.push((it.first, pen - ci.embedded[it.first].0));
                pen += it.w;
            }
            Kind::Box => pen += it.w,
            Kind::Glue if it.inf_stretch || it.trailing => {}
            Kind::Glue if it.end > it.first => {
                let natural = it.w;
                let scale = if natural > EPS { set(it) / natural } else { 1.0 };
                if lead.is_none() {
                    lead = Some(pen);
                }
                let mut x = pen;
                for k in it.first..it.end {
                    origins.push((k, x));
                    x += p.glyphs[k].advance_pt * scale;
                }
                pen += set(it);
            }
            Kind::Glue => pen += set(it),
            Kind::Penalty => {}
        }
    }
    let content_end = pen + used_pen;
    let lead_pt = lead.unwrap_or(pen);
    let start_offset_pt = origins.first().map_or(lead_pt, |o| o.1);
    let mut gaps = Vec::new();
    for pair in origins.windows(2) {
        let ((g0, x0), (g1, x1)) = (pair[0], pair[1]);
        if g1 == g0 + 1 {
            let delta = x1 - x0 - p.glyphs[g0].advance_pt;
            if delta.abs() > EPS {
                gaps.push(CjkGap { glyph: g0, delta_pt: delta });
            }
        }
    }
    let trailing: f64 = items[s..b].iter().filter(|it| it.kind == Kind::Glue && it.trailing).map(|it| it.w).sum();
    LineSet {
        start_offset_pt,
        lead_pt,
        width_pt: (content_end - lead_pt).max(0.0) + 0.0,
        hang_pt: -used_pen + trailing + 0.0,
        ratio: r,
        overfull: content_end > available + EPS,
        gaps,
    }
}

/// One line of [`compose_cjk_lines`].
#[derive(Clone, Debug, PartialEq)]
pub struct CjkComposedLine {
    /// Paragraph-local glyphs of the line (consecutive lines tile the paragraph).
    pub glyph_range: Range<usize>,
    /// Origin of the first glyph relative to the line start (indent, line-start aki and the aki
    /// inside the first glyph's box), points.
    pub start_offset_pt: f64,
    /// Set width from the first glyph box to the line end, hanging material excluded.
    pub width_pt: f64,
    /// Width of hanging punctuation and trailing spaces past the line end.
    pub hang_pt: f64,
    /// Aki adjustment ratio (negative = compressed toward the minima, positive = stretched).
    pub ratio: f64,
    pub overfull: bool,
    pub gaps: Vec<CjkGap>,
}

/// Composes one CJK paragraph into lines of `measure_pt` (the engine of [`reflow_cjk`] without
/// frames): `justified` fills every line but the last. `em_pt` as in the module notes (0 = the
/// widest CJK advance). Deterministic: no hashing; ties resolve to the earlier break.
pub fn compose_cjk_lines(
    p: &ShapedParagraph,
    text: &str,
    c: &CjkParagraph,
    em_pt: f64,
    measure_pt: f64,
    justified: bool,
    cancel: &CancellationToken,
) -> Result<Vec<CjkComposedLine>, LayoutError> {
    if !(measure_pt.is_finite() && measure_pt > 0.0) {
        return Err(LayoutError::InvalidGeometry);
    }
    let ci = build(p, text, c, em_pt, SpaceParams::default())?;
    let sums = Sums::new(&ci.items);
    let breaks = break_lines(&ci, &sums, &|_| measure_pt, c, cancel)?;
    let n = p.glyphs.len();
    let (mut out, mut s) = (Vec::with_capacity(breaks.len()), 0usize);
    for (k, b) in breaks.iter().enumerate() {
        let first = ci.items.get(s).map_or(n, |it| it.first);
        let next = line_start(&ci.items, b.item);
        let end = if k + 1 == breaks.len() { n } else { ci.items.get(next).map_or(n, |it| it.first) };
        let set = finish_line(&ci, p, s, b.item, measure_pt, justified && k + 1 < breaks.len());
        out.push(CjkComposedLine {
            glyph_range: first..end.max(first),
            start_offset_pt: set.start_offset_pt,
            width_pt: set.width_pt,
            hang_pt: set.hang_pt,
            ratio: set.ratio,
            overfull: b.overfull || set.overfull,
            gaps: set.gaps,
        });
        s = next;
    }
    Ok(out)
}

/// Spacing of one CJK-composed line of a [`CjkReflow`].
#[derive(Clone, Debug, PartialEq)]
pub struct CjkLineSpacing {
    /// Index into `proposal.frames`.
    pub frame: usize,
    /// Index into that frame's `lines`.
    pub line: usize,
    pub gaps: Vec<CjkGap>,
}

/// `reflow_flow` result with the spacing of every CJK-composed line. Those lines carry
/// `x_start_pt` = the first glyph's origin, `width_pt` / `hang_pt` as set, `ratio` = the aki
/// ratio and no `justification` (the aki gaps carry it).
#[derive(Clone, Debug, PartialEq)]
pub struct CjkReflow {
    pub proposal: LayoutProposal,
    pub spacing: Vec<CjkLineSpacing>,
}

/// `reflow_flow` with the Folio CJK composition records: each paragraph whose style binds a named
/// composer (STU-TYP-136) composes through kinsoku and mojikumi in its scope (the paragraph's own
/// `composition_scope` from the type-style composition settings overrides the composer's); every
/// other paragraph composes exactly as `reflow_flow` does. A record set of another document is
/// `invalid_input`; resolution errors as in [`resolve_cjk_paragraph`].
pub fn reflow_cjk(
    req: &ReflowRequest<'_>,
    opts: &TypesetOptions<'_>,
    flow: &StudioLayoutFlowRecords,
    records: &StudioCjkCompositionRecords,
    cancel: &CancellationToken,
) -> Result<CjkReflow, LayoutError> {
    if records.document_id != req.document.document_id {
        return Err(LayoutError::InvalidInput);
    }
    let mut per = Vec::with_capacity(req.paragraphs.len());
    for p in req.paragraphs {
        let style = req.story.paragraphs.iter().find(|s| s.start == p.source.start).and_then(|s| s.style_ref.as_deref());
        per.push(match style {
            Some(id) => resolve_cjk_paragraph(records, id)?,
            None => None,
        });
    }
    let (proposal, spacing) = reflow_core(req, opts, flow, None, &per, cancel)?;
    Ok(CjkReflow { proposal, spacing })
}
