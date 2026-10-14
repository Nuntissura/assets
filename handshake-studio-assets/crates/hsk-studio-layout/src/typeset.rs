//! Typesetting options on top of `reflow`: justification with word/letter spacing limits,
//! hyphenation, alignment and Folio type styles. `reflow` itself is unchanged (left-aligned,
//! unhyphenated); `reflow_typeset` is the same flow with these features.
//!
//! Justification model (InDesign-style three-value ranges; equality with InDesign output is
//! UNVERIFIED): word spacing is a multiple of each space's natural width (min / desired / max,
//! defaults 0.8 / 1.0 / 1.33), letter spacing an offset per glyph gap in multiples of
//! `reference_space_pt` (defaults 0 / 0 / 0). Glyph scaling is not implemented. A justified line is
//! first set to the desired values, then spaces move toward min or max as far as needed, then letter
//! spacing; what remains after both limits is `residual_pt` (the line is left loose or tight, never
//! scaled past the limits). The Knuth-Plass stretch/shrink ratios use `space_params()`
//! (stretch = max - 1, shrink = 1 - min of word spacing); letter spacing is not part of the breaking.
use crate::hyphenate::{HyphenationParams, Hyphenator};
use crate::styles::{Alignment, StyleSource};
use crate::{LayoutError, SpaceParams};

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
            && self.reference_space_pt.is_finite()
            && self.reference_space_pt >= 0.0
        {
            Ok(())
        } else {
            Err(LayoutError::InvalidInput)
        }
    }
}

/// What justification did to one line. `word_scale` multiplies every inner space's natural width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Justification {
    pub word_scale: f64,
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
    let mut remaining = avail_pt - natural_pt - spaces_pt * (w.desired - 1.0) - g * l.desired * r;
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
    if gaps > 0 && r > 0.0 {
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
        word_scale: scale,
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
}
impl Default for TypesetOptions<'_> {
    fn default() -> Self {
        Self {
            justify: None,
            hyphenation: None,
            hyphenator: None,
            alignment: Alignment::Left,
            styles: None,
        }
    }
}
impl TypesetOptions<'_> {
    pub(crate) fn validate(&self) -> Result<(), LayoutError> {
        if let Some(j) = &self.justify {
            j.validate()?;
        }
        if let Some(h) = &self.hyphenation
            && !(h.hyphen_advance_pt.is_finite()
                && h.hyphen_advance_pt >= 0.0
                && h.hyphen_penalty.is_finite()
                && h.double_hyphen_demerits.is_finite()
                && h.double_hyphen_demerits >= 0.0)
        {
            return Err(LayoutError::InvalidInput);
        }
        Ok(())
    }
}
