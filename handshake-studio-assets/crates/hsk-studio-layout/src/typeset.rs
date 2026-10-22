//! Typesetting options on top of `reflow`: justification with word/letter spacing limits,
//! hyphenation, alignment and Folio type styles. `reflow` itself is unchanged (left-aligned,
//! unhyphenated); `reflow_typeset` is the same flow with these features.
//!
//! Justification model (InDesign-style three-value ranges; equality with InDesign output is
//! UNVERIFIED): word spacing is a multiple of each space's natural width (min / desired / max,
//! defaults 0.8 / 1.0 / 1.33), letter spacing an offset per glyph gap in multiples of
//! `reference_space_pt` (defaults 0 / 0 / 0), glyph scaling a factor on every advance, spaces
//! included (defaults 1 / 1 / 1). A justified line is first set to the desired values, then spaces
//! move toward min or max as far as needed, then letter spacing and glyph scaling share the rest in
//! proportion to their ranges (InDesign D5 oracle ID-02 glyph-scaling sets: word spacing additive,
//! every advance times the glyph scale, letters and glyphs by range after the words; lane
//! SMB-TYPE-FF-LIGATURE); what remains after the limits is `residual_pt` (the line is left loose or
//! tight, never scaled past the limits). The Knuth-Plass stretch/shrink ratios use `space_params()`
//! (stretch = max - 1, shrink = 1 - min of word spacing); letter spacing is not part of the breaking.
//!
//! Per-paragraph composition (lane SMB-LAYOUT-COMPOSER-WIRING): a paragraph whose Folio style chain
//! or local override states composition settings is composed with [`paragraph_params`] (composer,
//! H&J limits, hyphenation, single-word justification); every other paragraph keeps the request's
//! composer and these options unchanged.
use crate::breaker::{AdobeParams, Composer};
use crate::hyphenate::{HyphenationParams, Hyphenator, MAX_WORD_BYTES};
use crate::styles::{Alignment, ParagraphComposition, StyleSource, length_pt};
use crate::{LayoutError, SpaceParams};
use hsk_studio_folio::{StudioCompositionEngine, StudioCompositionScope, StudioSingleWordJustification};

/// Minimum / desired / maximum of one spacing quantity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spacing {
    pub min: f64,
    pub desired: f64,
    pub max: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JustifyParams {
    pub word_spacing: Spacing,
    pub letter_spacing: Spacing,
    /// Width a letter-spacing value of 1.0 stands for (the shaped space advance of the line's font).
    pub reference_space_pt: f64,
    /// Glyph scaling as a factor of every advance (spec glyph_scaling_minimum/desired/maximum,
    /// 50..200 %); 1 / 1 / 1 = off.
    pub glyph_scaling: Spacing,
}
impl Default for JustifyParams {
    fn default() -> Self {
        Self {
            word_spacing: Spacing {
                min: 0.8,
                desired: 1.0,
                max: 1.33,
            },
            letter_spacing: Spacing {
                min: 0.0,
                desired: 0.0,
                max: 0.0,
            },
            reference_space_pt: 0.0,
            glyph_scaling: Spacing {
                min: 1.0,
                desired: 1.0,
                max: 1.0,
            },
        }
    }
}
impl JustifyParams {
    /// Stretch and shrink of a space as fractions of its width, for the breakers.
    pub fn space_params(&self) -> SpaceParams {
        SpaceParams {
            stretch: (self.word_spacing.max - 1.0).max(0.0),
            shrink: (1.0 - self.word_spacing.min).max(0.0),
        }
    }
    pub(crate) fn validate(&self) -> Result<(), LayoutError> {
        let ok = |s: &Spacing, lo: f64| {
            [s.min, s.desired, s.max].iter().all(|v| v.is_finite() && *v >= lo)
                && s.min <= s.desired
                && s.desired <= s.max
        };
        if ok(&self.word_spacing, 0.0)
            && ok(&self.letter_spacing, -10.0)
            && ok(&self.glyph_scaling, 0.5)
            && self.glyph_scaling.max <= 2.0
            && self.reference_space_pt.is_finite()
            && self.reference_space_pt >= 0.0
        {
            Ok(())
        } else {
            Err(LayoutError::InvalidInput)
        }
    }
}

/// What justification did to one line. `word_scale` multiplies every inner space's natural width
/// (glyph scaling included: a space is set at `natural * word_scale`); `glyph_scale` multiplies every
/// other glyph's advance (and a line-end hyphen).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Justification {
    pub word_scale: f64,
    pub glyph_scale: f64,
    /// Added after every glyph but the last of the line.
    pub letter_gap_pt: f64,
    /// Slot width minus the justified line width: > 0 still loose, < 0 still tight, 0 exact.
    pub residual_pt: f64,
}

/// Solves one line: `natural_pt` is the line's natural width (hyphen included), `avail_pt` the slot,
/// `spaces_pt` the summed natural width of the inner spaces and `gaps` the glyph gaps. Pure.
pub fn justify_metrics(
    natural_pt: f64,
    avail_pt: f64,
    spaces_pt: f64,
    gaps: usize,
    jp: &JustifyParams,
) -> Justification {
    let (w, l) = (&jp.word_spacing, &jp.letter_spacing);
    let r = jp.reference_space_pt;
    let g = gaps as f64;
    let gs = &jp.glyph_scaling;
    let mut remaining = avail_pt
        - natural_pt
        - spaces_pt * (w.desired - 1.0)
        - g * l.desired * r
        - natural_pt * (gs.desired - 1.0);
    let mut scale = w.desired;
    if spaces_pt > 0.0 {
        let (room, limit) = if remaining > 0.0 {
            (spaces_pt * (w.max - w.desired), w.max)
        } else {
            (spaces_pt * (w.desired - w.min), w.min)
        };
        let use_ = remaining.abs().min(room);
        scale = if use_ >= room && room > 0.0 {
            limit
        } else {
            w.desired + remaining.signum() * use_ / spaces_pt
        };
        remaining -= remaining.signum() * use_;
    }
    let mut letter = l.desired * r;
    let mut glyph = gs.desired;
    let glyph_room = if remaining > 0.0 {
        natural_pt * (gs.max - gs.desired)
    } else {
        natural_pt * (gs.desired - gs.min)
    };
    if glyph_room > 0.0 {
        // Letter spacing and glyph scaling share what the words leave, in proportion to their
        // ranges (oracle ID-02 ls-5_0_5_gs90_100_110: both at 0.194 of their ranges on line 1).
        let (letter_room, letter_limit) = match (gaps > 0 && r > 0.0, remaining > 0.0) {
            (false, _) => (0.0, letter),
            (true, true) => (g * (l.max - l.desired) * r, l.max * r),
            (true, false) => (g * (l.desired - l.min) * r, l.min * r),
        };
        let total = letter_room + glyph_room;
        let use_ = remaining.abs().min(total);
        let k = use_ / total;
        if use_ >= total {
            letter = letter_limit;
            glyph = if remaining > 0.0 { gs.max } else { gs.min };
        } else {
            if letter_room > 0.0 {
                letter += remaining.signum() * k * letter_room / g;
            }
            glyph += remaining.signum() * k * glyph_room / natural_pt;
        }
        remaining -= remaining.signum() * use_;
    } else if gaps > 0 && r > 0.0 {
        let (room, limit) = if remaining > 0.0 {
            (g * (l.max - l.desired) * r, l.max * r)
        } else {
            (g * (l.desired - l.min) * r, l.min * r)
        };
        let use_ = remaining.abs().min(room);
        if use_ >= room && room > 0.0 {
            letter = limit;
        } else {
            letter += remaining.signum() * use_ / g;
        }
        remaining -= remaining.signum() * use_;
    }
    Justification {
        word_scale: scale + (glyph - 1.0),
        glyph_scale: glyph,
        letter_gap_pt: letter,
        residual_pt: if remaining.abs() < 1e-9 { 0.0 } else { remaining },
    }
}

/// Options of `reflow_typeset`. All `None`/default = plain `reflow` output.
#[derive(Clone, Copy, Debug)]
pub struct TypesetOptions<'a> {
    /// `Some` justifies `Alignment::Justified` paragraphs with these limits and derives the
    /// breakers' space stretch/shrink from them (overriding `ReflowRequest::space`). `None` with a
    /// justified paragraph uses `JustifyParams::default()`.
    pub justify: Option<JustifyParams>,
    /// `Some` inserts hyphenation break points (flagged penalties) into words.
    pub hyphenation: Option<HyphenationParams>,
    /// Pattern set; `None` = embedded en-US.
    pub hyphenator: Option<&'a Hyphenator>,
    /// Alignment of paragraphs whose style states none.
    pub alignment: Alignment,
    /// Folio type styles; `None` = unstyled.
    pub styles: Option<StyleSource<'a>>,
    /// Hyphen advance (points, shaped by the caller) for paragraphs whose style composition turns
    /// hyphenation on while `hyphenation` is `None` (see [`paragraph_params`]).
    pub hyphen_advance_pt: Option<f64>,
}
impl Default for TypesetOptions<'_> {
    fn default() -> Self {
        Self {
            justify: None,
            hyphenation: None,
            hyphenator: None,
            alignment: Alignment::Left,
            styles: None,
            hyphen_advance_pt: None,
        }
    }
}
impl TypesetOptions<'_> {
    pub(crate) fn validate(&self) -> Result<(), LayoutError> {
        if let Some(j) = &self.justify {
            j.validate()?;
        }
        // Every integer and float limit of the hyphenation options (checked arithmetic, typed
        // refusal): see `HyphenationParams::validate`. JustifyParams holds floats only.
        if let Some(h) = &self.hyphenation {
            h.validate().map_err(|_| LayoutError::InvalidInput)?;
        }
        if self.hyphen_advance_pt.is_some_and(|a| !(a.is_finite() && a >= 0.0)) {
            return Err(LayoutError::InvalidInput);
        }
        Ok(())
    }
}

/// What one paragraph is composed with (see [`paragraph_params`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParagraphParams {
    pub composer: Composer,
    pub justify: JustifyParams,
    pub space: SpaceParams,
    /// `None` = not hyphenated.
    pub hyphenation: Option<HyphenationParams>,
    /// STU-TYP-151 `hyphenate_last_word`: false keeps the paragraph's last word whole.
    pub hyphenate_last_word: bool,
    /// STU-TYP-146 setting of a justified line holding one word (`None` = fully justified, the
    /// behaviour before the setting existed).
    pub single_word: Option<StudioSingleWordJustification>,
}

/// Composition parameters of one paragraph (lane SMB-LAYOUT-COMPOSER-WIRING). Without composition
/// settings (`composition = None`) this is exactly the request: `composer`, `opts.justify` (or its
/// default), the space stretch/shrink of `opts.justify` (else `req_space`) and `opts.hyphenation`.
/// With settings, every field the settings state replaces the request's value:
/// - `composition_scope` selects the Adobe-parity composers (`single_line` -> `AdobeSingleLine`,
///   `every_line` -> `AdobeParagraph`); unset keeps the request's composer.
///   `hyphenation_zone` sets `AdobeParams::hyphenation_zone_pt` of an Adobe composer (36 pt when
///   the request's composer is not one). `composition_engine` `world_ready` / `adornment` is
///   `UnsupportedComposition` (layout composes Latin only); `latin_cjk` composes.
/// - word / letter / glyph spacing percentages are divided by 100 (`JustifyParams` units); letter
///   spacing is relative to the paragraph's shaped word space (`space_advance_pt`, else the
///   request's `reference_space_pt`); the resolved triples must be ordered (`InvalidInput`). A word
///   spacing setting or a `composition_scope` also sets the breakers' space stretch/shrink from the
///   H&J limits (as a request with `justify` does).
/// - `hyphenation` false turns hyphenation off; true (or any request hyphenation) turns it on with
///   `hyphenate_words_longer_than` as the minimum word length (InDesign's "Words with at least N
///   letters" reading, the mapping the D5 ID oracle matches), `hyphenate_after_first` /
///   `hyphenate_before_last` as the letters before / after a break, `hyphenate_ladder_limit` as the
///   consecutive limit (0 = unlimited; values above 255 saturate) and
///   `hyphenate_capitalized_words`. A minimum no hyphenable word can meet (above `MAX_WORD_BYTES`)
///   leaves the paragraph unhyphenated, the outcome the setting describes. Hyphen advance: the
///   request's `hyphenation` value, else `opts.hyphen_advance_pt`, else the paragraph's own U+002D
///   glyph, else one third of the font size (an ESTIMATE, UNVERIFIED: Arial's hyphen is 0.333 em).
/// - Inert here (stored, round-tripped, not composed): `hyphen_weight`, `hyphenate_across_columns`.
pub fn paragraph_params(
    composition: Option<&ParagraphComposition>,
    font_size_pt: f64,
    composer: Composer,
    req_space: SpaceParams,
    opts: &TypesetOptions<'_>,
) -> Result<ParagraphParams, LayoutError> {
    let base_justify = opts.justify.unwrap_or_default();
    let mut out = ParagraphParams {
        composer,
        justify: base_justify,
        space: if opts.justify.is_some() { base_justify.space_params() } else { req_space },
        hyphenation: opts.hyphenation,
        hyphenate_last_word: true,
        single_word: None,
    };
    let Some(c) = composition else {
        return Ok(out);
    };
    let s = &c.settings;
    match s.composition_engine {
        Some(StudioCompositionEngine::WorldReady | StudioCompositionEngine::Adornment) => {
            return Err(LayoutError::UnsupportedComposition);
        }
        Some(StudioCompositionEngine::LatinCjk) | None => {}
    }
    let mut adobe = match composer {
        Composer::AdobeSingleLine(p) | Composer::AdobeParagraph(p) => p,
        _ => AdobeParams::default(),
    };
    if let Some(zone) = &s.hyphenation_zone {
        let pt = length_pt(zone)?;
        if pt < 0.0 {
            return Err(LayoutError::InvalidInput);
        }
        adobe.hyphenation_zone_pt = pt;
    }
    out.composer = match (s.composition_scope, composer) {
        (Some(StudioCompositionScope::SingleLine), _) | (None, Composer::AdobeSingleLine(_)) => Composer::AdobeSingleLine(adobe),
        (Some(StudioCompositionScope::EveryLine), _) | (None, Composer::AdobeParagraph(_)) => Composer::AdobeParagraph(adobe),
        (None, other) => other,
    };
    let pct = |v: Option<f64>, base: f64| v.map_or(base, |v| v / 100.0);
    let jp = &mut out.justify;
    jp.word_spacing = Spacing {
        min: pct(s.word_spacing_minimum, jp.word_spacing.min),
        desired: pct(s.word_spacing_desired, jp.word_spacing.desired),
        max: pct(s.word_spacing_maximum, jp.word_spacing.max),
    };
    let letter_set = s.letter_spacing_minimum.is_some() || s.letter_spacing_desired.is_some() || s.letter_spacing_maximum.is_some();
    jp.letter_spacing = Spacing {
        min: pct(s.letter_spacing_minimum, jp.letter_spacing.min),
        desired: pct(s.letter_spacing_desired, jp.letter_spacing.desired),
        max: pct(s.letter_spacing_maximum, jp.letter_spacing.max),
    };
    if letter_set && let Some(space) = c.space_advance_pt {
        jp.reference_space_pt = space;
    }
    jp.glyph_scaling = Spacing {
        min: pct(s.glyph_scaling_minimum, jp.glyph_scaling.min),
        desired: pct(s.glyph_scaling_desired, jp.glyph_scaling.desired),
        max: pct(s.glyph_scaling_maximum, jp.glyph_scaling.max),
    };
    jp.validate()?;
    if s.composition_scope.is_some() || s.word_spacing_minimum.is_some() || s.word_spacing_desired.is_some() || s.word_spacing_maximum.is_some() {
        out.space = out.justify.space_params();
    }
    if s.hyphenation.unwrap_or(opts.hyphenation.is_some()) {
        let advance = opts
            .hyphenation
            .map(|h| h.hyphen_advance_pt)
            .or(opts.hyphen_advance_pt)
            .or(c.hyphen_advance_pt)
            .unwrap_or(font_size_pt / 3.0);
        let mut h = opts.hyphenation.unwrap_or(HyphenationParams { hyphen_advance_pt: advance, ..HyphenationParams::default() });
        let count = |v: Option<u32>, base: usize| v.map_or(base, |v| usize::try_from(v).unwrap_or(usize::MAX));
        h.min_word_len = count(s.hyphenate_words_longer_than, h.min_word_len);
        h.min_before = count(s.hyphenate_after_first, h.min_before);
        h.min_after = count(s.hyphenate_before_last, h.min_after);
        if let Some(limit) = s.hyphenate_ladder_limit {
            h.consecutive_limit = u8::try_from(limit).unwrap_or(u8::MAX);
        }
        if let Some(caps) = s.hyphenate_capitalized_words {
            h.hyphenate_capitalized = caps;
        }
        let reachable = h.min_word_len <= MAX_WORD_BYTES
            && h.min_before.checked_add(h.min_after).is_some_and(|n| n <= MAX_WORD_BYTES);
        out.hyphenation = if reachable {
            h.validate().map_err(|_| LayoutError::InvalidInput)?;
            Some(h)
        } else {
            None
        };
    } else {
        out.hyphenation = None;
    }
    out.hyphenate_last_word = s.hyphenate_last_word.unwrap_or(true);
    out.single_word = s.single_word_justification;
    Ok(out)
}
