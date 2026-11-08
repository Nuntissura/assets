//! Fit before build (layouts.json fit_rules): decisions a view needs before its tree is built, measured with [`Text`],
//! because the view closure of [`crate::tree::Panel::update`] gets a Builder and no Text. A row of labels with padding
//! steps and an overflow chip (FIT-M1 menu titles), and tab overflow into +N with the active tab always shown
//! (docking.tab_overflow). No Studio vocabulary: callers pass labels and widths.
use crate::text::{MAX_LABEL_BYTES, Text};
use crate::tokens::TextToken;

/// The outcome of [`fit_labels`].
#[derive(Clone, Debug, PartialEq)]
pub struct LabelFit {
    /// Index into the `pads` argument of the padding used.
    pub pad_step: usize,
    /// That padding, logical px per side.
    pub pad: f32,
    /// How many labels show, from the start, before the overflow chip.
    pub shown: usize,
    /// Text width of every label, logical px (infinite for a label over [`MAX_LABEL_BYTES`], which never fits).
    pub widths: Vec<f32>,
    /// Width the shown slots take, the chip included when there is overflow.
    pub used: f32,
}
impl LabelFit {
    pub fn overflow(&self) -> bool {
        self.shown < self.widths.len()
    }
    /// Width of slot `i` (label plus padding on both sides).
    pub fn slot(&self, i: usize) -> f32 {
        self.widths.get(i).map_or(0.0, |w| w + 2.0 * self.pad)
    }
}

/// Fits a row of labels into `avail` logical px: the first padding step of `pads` at which every label fits wins;
/// otherwise the last step, with as many labels as fit before an overflow chip `chip_w` wide. Each label is measured
/// once. An empty `pads` means padding 0.
#[allow(clippy::too_many_arguments)]
pub fn fit_labels(text: &mut Text, labels: &[&str], token: TextToken, weight: u16, pads: &[f32], avail: f32, chip_w: f32) -> LabelFit {
    let widths: Vec<f32> = labels.iter().map(|l| width(text, l, token, weight)).collect();
    let pads: &[f32] = if pads.is_empty() { &[0.0] } else { pads };
    for (i, &pad) in pads.iter().enumerate() {
        let total: f32 = widths.iter().map(|w| w + 2.0 * pad).sum();
        if total <= avail {
            return LabelFit { pad_step: i, pad, shown: labels.len(), widths, used: total };
        }
    }
    let i = pads.len() - 1;
    let pad = pads[i];
    let (mut used, mut shown) = (chip_w, 0);
    for w in &widths {
        let s = w + 2.0 * pad;
        if used + s > avail {
            break;
        }
        used += s;
        shown += 1;
    }
    LabelFit { pad_step: i, pad, shown, widths, used }
}

/// A label's width as layout sizes a Hug label (measured, ceiled); infinite over the shaping bound.
fn width(text: &mut Text, s: &str, token: TextToken, weight: u16) -> f32 {
    if s.len() > MAX_LABEL_BYTES { f32::INFINITY } else { text.measure(s, token, weight).ceil() }
}

/// The outcome of [`fit_tabs`]: indices of the tabs shown, in order, and of those in the +N menu.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TabFit {
    pub shown: Vec<usize>,
    pub hidden: Vec<usize>,
}

/// docking.tab_overflow: tabs show full labels; those that do not fit move, from the end, into a +N chip `chip_w`
/// wide. The active tab is always shown and takes the slot of the last visible tab while it is active, so order stays
/// stable. `active_weight` is the active tab's weight (layout measures what emit draws).
#[allow(clippy::too_many_arguments)]
pub fn fit_tabs(text: &mut Text, labels: &[&str], active: usize, token: TextToken, weight: u16, active_weight: u16, pad: f32, avail: f32, chip_w: f32) -> TabFit {
    let n = labels.len();
    if n == 0 {
        return TabFit::default();
    }
    let active = active.min(n - 1);
    let w: Vec<f32> = labels.iter().enumerate().map(|(i, l)| width(text, l, token, if i == active { active_weight } else { weight }) + 2.0 * pad).collect();
    if w.iter().sum::<f32>() <= avail {
        return TabFit { shown: (0..n).collect(), hidden: Vec::new() };
    }
    let (mut shown, mut used) = (Vec::new(), chip_w);
    for (i, wi) in w.iter().enumerate() {
        if used + wi > avail {
            break;
        }
        shown.push(i);
        used += wi;
    }
    if !shown.contains(&active) {
        // The active tab takes the slot of the last visible tab(s) it needs.
        while let Some(&last) = shown.last() {
            if used + w[active] <= avail {
                break;
            }
            used -= w[last];
            shown.pop();
        }
        shown.push(active);
    }
    let hidden = (0..n).filter(|i| !shown.contains(i)).collect();
    TabFit { shown, hidden }
}
