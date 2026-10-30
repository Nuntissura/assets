//! STU-LAY-119 text-frame auto-size on the threaded reflow (lane SMB-ID-FRAME-AUTOSIZE-1). The
//! auto-sized frame is the last frame of its thread (or an unthreaded frame; a frame with a next
//! frame is `autosize_threaded_frame`). Studio readings, InDesign behaviour UNVERIFIED (D5):
//!
//! - `height_only`: the top edge and the width stay; the height is the bottom of the frame's last line
//!   (baseline + descent) plus the bottom inset. One column: measured from one composition at the
//!   extent bound. Several columns: the smallest height at which the story fits, columns filled in
//!   order (no balancing), found by bisection and then set exactly to the deepest line's bottom.
//! - `width_only`: lines do not wrap (the frame is composed at the extent bound); the left edge and
//!   the height stay; the width is the widest line (`Line::width_pt`, hanging space excluded) plus
//!   the left and right insets.
//! - `height_and_width`: the width rule, and the height fits the lines as for `height_only`.
//! - `height_and_width_proportionally`: the smallest uniform scale of the authored width and height
//!   (insets and offsets unscaled; the top-left corner stays) at which the story fits with no overset
//!   and no overfull line, by bisection on the scale until the bracket on the larger side is at most
//!   `AUTOSIZE_TOLERANCE_PT` (the found scale always fits).
//! - Width and proportional kinds with more than one column are `autosize_multi_column`. A frame with
//!   no line keeps the authored size on the measured axis.
//! - Folio stores no `auto_sizing_reference_point`, minimum width/height gates or
//!   `use_no_line_breaks_for_auto_sizing` (STU-LAY-118): readings top edge fixed for height, left
//!   edge for width, top-left for proportional; no minimum; no added line breaks.
//! - Sizing and the final composition set the frame unbalanced. STU-LAY-121 vertical justification
//!   is moot for the kinds that size the height (`height_only`, `height_and_width`, proportional):
//!   their lines are set top-aligned (a shorter last column or the slack a scaled frame keeps is not
//!   redistributed); `width_only` keeps the authored height and its vertical justification.
//! - Bounds: no width or height beyond `MAX_AUTOSIZE_EXTENT_PT`; a story that does not fit there
//!   (for example a run wider than the bound) is `autosize_over_bound`; at most
//!   `MAX_AUTOSIZE_ITERATIONS` compositions; the cancel token is checked before each one.
use crate::folio_input::{Frame, FrameVertical};
use crate::paragraph::ShapedParagraph;
use crate::threaded_reflow::{Composition, LayoutProposal, Line};
use crate::LayoutError;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    AutoSizing, Budget, Code, Diagnostic, GeometryUnit, Length, Resolver, Snapshot, StudioDocument, VerticalJustification,
    validate_document,
};

/// Largest width or height an auto-sized frame may take, pt (declared bound, Studio reading).
pub const MAX_AUTOSIZE_EXTENT_PT: f64 = 15_552.0;
/// Bisection stops when the bracket on the frame's larger side is at most this wide, pt.
pub const AUTOSIZE_TOLERANCE_PT: f64 = 0.01;
/// Most compositions one auto-size may run.
pub const MAX_AUTOSIZE_ITERATIONS: u32 = 64;

const EPS: f64 = 1e-9;

/// One auto-sized frame: its id, the authored and the new size, and the compositions run.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameResize {
    /// `layer_id` of the frame layer.
    pub frame_id: String,
    /// Index of the frame layer in `StudioDocument::layers`.
    pub frame_index: usize,
    pub auto_sizing: AutoSizing,
    pub before_width_pt: f64,
    pub before_height_pt: f64,
    pub after_width_pt: f64,
    pub after_height_pt: f64,
    /// Compositions run to find the size (1 for a direct measurement).
    pub compositions: u32,
}

/// The reflow with every auto-size applied (lines in the resized frames), and the resizes as its
/// receipt.
#[derive(Clone, Debug, PartialEq)]
pub struct AutosizeProposal {
    pub layout: LayoutProposal,
    pub resizes: Vec<FrameResize>,
}
impl AutosizeProposal {
    /// Every resize: frame id, size before and after.
    pub fn autosized_frames(&self) -> &[FrameResize] {
        &self.resizes
    }
    /// The prospective document: `base` (the document the reflow read, same revision) with every
    /// resized frame's width and height written (x and y kept: top-left reference), validated by
    /// Folio. The revision is not advanced; committing it stays the host's step. Refusals: a base of
    /// another revision `stale_revision` (target `autosize.base_revision`), a resize whose frame layer
    /// is not at its index in `base` `invalid_field` (target `autosize.frame`), and Folio's own codes.
    pub fn autosized_document(
        &self,
        base: &StudioDocument,
        budget: Budget,
        resolver: &dyn Resolver,
        cancel: &CancellationToken,
    ) -> Result<Snapshot, Diagnostic> {
        let refuse = |code: Code, target: &str| Diagnostic { code, target: target.into() };
        if base.revision != self.layout.base_revision {
            return Err(refuse(Code::StaleRevision, "autosize.base_revision"));
        }
        let mut document = base.clone();
        for r in &self.resizes {
            let frame = document
                .layers
                .get_mut(r.frame_index)
                .filter(|l| l.layer_id == r.frame_id)
                .and_then(|l| l.text_frame.as_mut())
                .ok_or_else(|| refuse(Code::InvalidField, "autosize.frame"))?;
            frame.bounds.width = Length { value: r.after_width_pt, unit: GeometryUnit::Pt };
            frame.bounds.height = Length { value: r.after_height_pt, unit: GeometryUnit::Pt };
        }
        validate_document(document, budget, cancel, resolver)
    }
}

/// Composes the given frames with the given frame options (the reflow's composition core).
pub(crate) type Compose<'a, 'f> = dyn FnMut(&[Frame<'a>], &[FrameVertical]) -> Result<Composition, LayoutError> + 'f;

/// Bottom of the deepest line (baseline + descent of its paragraph).
fn deepest(lines: &[Line], paragraphs: &[ShapedParagraph]) -> Option<f64> {
    lines
        .iter()
        .filter_map(|l| paragraphs.get(l.paragraph).map(|p| l.baseline_y_pt + (p.line_height_pt - p.ascent_pt)))
        .reduce(f64::max)
}

/// Sizes the auto-sized frame of `thread` in place (its width and height) and returns the resize, or
/// `None` when no frame auto-sizes. `compose` composes the given frames with the given options.
pub(crate) fn autosize<'a>(
    thread: &mut [Frame<'a>],
    verticals: &mut [FrameVertical],
    paragraphs: &[ShapedParagraph],
    compose: &mut Compose<'a, '_>,
    cancel: &CancellationToken,
) -> Result<Option<FrameResize>, LayoutError> {
    let Some(a) = verticals.iter().position(|v| v.auto != AutoSizing::Off) else {
        return Ok(None);
    };
    let kind = verticals[a].auto;
    if a + 1 != thread.len() {
        return Err(LayoutError::AutosizeThreadedFrame);
    }
    if thread[a].column_count > 1 && kind != AutoSizing::HeightOnly {
        return Err(LayoutError::AutosizeMultiColumn);
    }
    let mut sizing = verticals.to_vec();
    sizing[a].vjust = VerticalJustification::Top;
    thread[a].balance = false;
    let (w0, h0) = (thread[a].width_pt, thread[a].height_pt);
    let (left, right) = (thread[a].inset_left_pt, thread[a].inset_right_pt);
    let (top, bottom) = (thread[a].inset_top_pt, thread[a].inset_bottom_pt);
    let cap = MAX_AUTOSIZE_EXTENT_PT;
    let mut runs = 0u32;
    let mut run = |thread: &mut [Frame<'a>], w: f64, h: f64, runs: &mut u32| -> Result<Composition, LayoutError> {
        cancel.check().map_err(|_| LayoutError::Canceled)?;
        if *runs >= MAX_AUTOSIZE_ITERATIONS {
            return Err(LayoutError::AutosizeOverBound);
        }
        *runs += 1;
        thread[a].width_pt = w;
        thread[a].height_pt = h;
        compose(thread, &sizing)
    };
    let fitted_height = |c: &Composition| deepest(&c.lines[a], paragraphs).map_or(h0, |d| d + bottom);
    let (w, h) = match kind {
        AutoSizing::Off => return Ok(None),
        AutoSizing::HeightOnly => {
            let mut best = run(thread, w0, cap, &mut runs)?;
            if best.overset.is_some() {
                return Err(LayoutError::AutosizeOverBound);
            }
            if thread[a].column_count > 1 {
                // Smallest height that holds the story; then exactly the deepest line's bottom there
                // (every used slot still fits it and no unused slot opens, so the lines stand).
                let (mut lo, mut hi) = (top + bottom, cap);
                while hi - lo > AUTOSIZE_TOLERANCE_PT && runs < MAX_AUTOSIZE_ITERATIONS {
                    let mid = lo + (hi - lo) / 2.0;
                    let c = run(thread, w0, mid, &mut runs)?;
                    if c.overset.is_none() {
                        (hi, best) = (mid, c);
                    } else {
                        lo = mid;
                    }
                }
            }
            (w0, fitted_height(&best))
        }
        AutoSizing::WidthOnly | AutoSizing::HeightAndWidth => {
            let both = kind == AutoSizing::HeightAndWidth;
            let c = run(thread, cap, if both { cap } else { h0 }, &mut runs)?;
            // Lines do not wrap: the rest of a paragraph from a line of the frame on must fit the
            // bound on one line, else the paragraph cannot be set.
            let room = cap - left - right + EPS;
            let natural = |l: &Line| {
                paragraphs
                    .get(l.paragraph)
                    .and_then(|p| p.glyphs.get(l.glyph_range.start..))
                    .map_or(0.0, |g| g.iter().map(|g| g.advance_pt).sum::<f64>())
            };
            if c.lines[a].iter().any(|l| l.overfull || natural(l) > room) || (both && c.overset.is_some()) {
                return Err(LayoutError::AutosizeOverBound);
            }
            let widest = c.lines[a].iter().map(|l| l.width_pt).reduce(f64::max).filter(|v| *v > 0.0);
            (widest.map_or(w0, |v| v + left + right), if both { fitted_height(&c) } else { h0 })
        }
        AutoSizing::HeightAndWidthProportionally => {
            let big = w0.max(h0);
            let fits = |c: &Composition| c.overset.is_none() && c.lines[a].iter().all(|l| !l.overfull);
            let (mut lo, mut hi) = (((left + right) / w0).max((top + bottom) / h0), cap / big);
            if hi <= lo || !fits(&run(thread, w0 * hi, h0 * hi, &mut runs)?) {
                return Err(LayoutError::AutosizeOverBound);
            }
            while (hi - lo) * big > AUTOSIZE_TOLERANCE_PT && runs < MAX_AUTOSIZE_ITERATIONS {
                let mid = lo + (hi - lo) / 2.0;
                if fits(&run(thread, w0 * mid, h0 * mid, &mut runs)?) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            (w0 * hi, h0 * hi)
        }
    };
    thread[a].width_pt = w;
    thread[a].height_pt = h;
    if kind != AutoSizing::WidthOnly {
        verticals[a].vjust = VerticalJustification::Top;
    }
    Ok(Some(FrameResize {
        frame_id: thread[a].id.to_owned(),
        frame_index: thread[a].layer_index,
        auto_sizing: kind,
        before_width_pt: w0,
        before_height_pt: h0,
        after_width_pt: w,
        after_height_pt: h,
        compositions: runs,
    }))
}
