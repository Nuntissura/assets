//! Placed graphics in frames (STU-LAY-028, STU-LAY-029, STU-LAY-131): where a graphic frame's
//! content sits after fitting. Pure geometry over the Folio `StudioGraphicFrame` record; the pixels
//! stay with the layer's raster payload and are drawn by the caller (interop-layout PDF export
//! draws pigment tiles through the transform, clipped to the frame shape).
//!
//! Model (documented; InDesign parity UNVERIFIED, D5): the content box is the content at 100 %
//! minus the crop amounts (`crop`: positive trims, negative adds space). Fitting maps that box into
//! the frame:
//! - `content_to_frame`: scale each axis to the frame (proportions ignored), at the frame origin;
//! - `proportionally`: largest uniform scale that fits inside, positioned at the anchor;
//! - `fill_proportionally`: smallest uniform scale that covers the frame, positioned at the anchor
//!   (the overflow is cropped by the frame clip);
//! - `center_content`: 100 %, centred (the anchor is not used);
//! - `preserve_sizes`: 100 %, positioned at the anchor;
//! - `frame_to_content`: 100 %; the frame is resized to the content box, keeping the anchor point
//!   of the frame where it was;
//! - `apply_frame_fitting_options`: the frame's stored rule (`frame_fitting`), refused when none;
//! - `content_aware`: typed `content_aware_unavailable` (no content analysis in this build; the
//!   spec requires the typed channel, STU-LAY-131);
//! - `honour_existing_style`: data-merge only, typed `fit_not_legal_here`;
//! - no fit (cleared fitting): 100 % at the frame's top-left.
//!
//! "Positioned at the anchor": with anchor fractions (ax, ay) the box's top-left is
//! `frame_origin + (frame_size - box_size) * (ax, ay)`.
use crate::LayoutError;
use crate::folio_input::pt;
use crate::geometry::Rect;
use hsk_studio_folio::{StudioDocument, StudioFit, StudioFitAnchor, StudioFrameShape, StudioGraphicFrame};

/// Typed refusals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FitError {
    Layout(LayoutError),
    ContentAwareUnavailable,
    NotLegalHere,
    NoStoredFitting,
}
impl FitError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Layout(e) => e.code(),
            Self::ContentAwareUnavailable => "content_aware_unavailable",
            Self::NotLegalHere => "fit_not_legal_here",
            Self::NoStoredFitting => "no_stored_fitting",
        }
    }
}
impl std::fmt::Display for FitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for FitError {}
impl From<LayoutError> for FitError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}

/// The content box in content coordinates (pt at 100 %, origin = content top-left).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Crop {
    pub top: f64,
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
}

/// A fitted placement: the frame (possibly resized) and the content transform, both in the
/// coordinates of the input frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fitted {
    pub frame: Rect,
    /// Content (pt at 100 %) to frame coordinates: `[sx, 0, 0, sy, tx, ty]`.
    pub transform: [f64; 6],
    /// The cropped content box after fitting.
    pub content_box: Rect,
}

/// Fits content of `content_w` x `content_h` pt (minus `crop`) into `frame` (module docs).
pub fn fit_content(
    frame: Rect,
    content_w: f64,
    content_h: f64,
    fit: Option<StudioFit>,
    stored: Option<StudioFit>,
    anchor: StudioFitAnchor,
    crop: Crop,
) -> Result<Fitted, FitError> {
    let bad = || FitError::Layout(LayoutError::InvalidGeometry);
    let finite = [content_w, content_h, crop.top, crop.left, crop.bottom, crop.right]
        .iter()
        .all(|v| v.is_finite());
    if !finite || !frame.valid() {
        return Err(bad());
    }
    let (fw, fh) = (frame.x1_pt - frame.x0_pt, frame.y1_pt - frame.y0_pt);
    let (ew, eh) = (
        content_w - crop.left - crop.right,
        content_h - crop.top - crop.bottom,
    );
    if !(fw > 0.0 && fh > 0.0 && ew > 0.0 && eh > 0.0 && content_w > 0.0 && content_h > 0.0) {
        return Err(bad());
    }
    let fit = match fit {
        Some(StudioFit::ApplyFrameFittingOptions) => match stored {
            None => return Err(FitError::NoStoredFitting),
            Some(StudioFit::ApplyFrameFittingOptions) => return Err(FitError::NotLegalHere),
            s => s,
        },
        f => f,
    };
    let (ax, ay) = anchor.fractions();
    let at = |pw: f64, ph: f64| {
        (
            frame.x0_pt + (fw - pw) * ax,
            frame.y0_pt + (fh - ph) * ay,
        )
    };
    let mut out_frame = frame;
    let (sx, sy, px, py) = match fit {
        None => (1.0, 1.0, frame.x0_pt, frame.y0_pt),
        Some(StudioFit::ContentToFrame) => (fw / ew, fh / eh, frame.x0_pt, frame.y0_pt),
        Some(StudioFit::Proportionally) => {
            let s = (fw / ew).min(fh / eh);
            let (x, y) = at(ew * s, eh * s);
            (s, s, x, y)
        }
        Some(StudioFit::FillProportionally) => {
            let s = (fw / ew).max(fh / eh);
            let (x, y) = at(ew * s, eh * s);
            (s, s, x, y)
        }
        Some(StudioFit::CenterContent) => (
            1.0,
            1.0,
            frame.x0_pt + (fw - ew) / 2.0,
            frame.y0_pt + (fh - eh) / 2.0,
        ),
        Some(StudioFit::PreserveSizes) => {
            let (x, y) = at(ew, eh);
            (1.0, 1.0, x, y)
        }
        Some(StudioFit::FrameToContent) => {
            let (x, y) = at(ew, eh);
            out_frame = Rect {
                x0_pt: x,
                y0_pt: y,
                x1_pt: x + ew,
                y1_pt: y + eh,
            };
            (1.0, 1.0, x, y)
        }
        Some(StudioFit::ContentAware) => return Err(FitError::ContentAwareUnavailable),
        Some(StudioFit::HonourExistingStyle | StudioFit::ApplyFrameFittingOptions) => {
            return Err(FitError::NotLegalHere);
        }
    };
    let transform = [sx, 0.0, 0.0, sy, px - sx * crop.left, py - sy * crop.top];
    let content_box = Rect {
        x0_pt: px,
        y0_pt: py,
        x1_pt: px + ew * sx,
        y1_pt: py + eh * sy,
    };
    let all = transform
        .iter()
        .chain([content_box.x1_pt, content_box.y1_pt, out_frame.x1_pt, out_frame.y1_pt].iter())
        .all(|v| v.is_finite());
    if !all || !(sx > 0.0 && sy > 0.0) {
        return Err(bad());
    }
    Ok(Fitted {
        frame: out_frame,
        transform,
        content_box,
    })
}

/// A graphic frame resolved onto its spread.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedGraphic {
    pub layer_id: String,
    pub spread: usize,
    pub page: usize,
    /// Frame in spread coordinates (after fitting; `frame_to_content` may resize it).
    pub frame: Rect,
    pub shape: StudioFrameShape,
    /// Content (pt at 100 %) to spread coordinates: `[sx, 0, 0, sy, tx, ty]`.
    pub transform: [f64; 6],
    pub content_box: Rect,
    /// Content size at 100 %, pt (the record's `content_width` / `content_height`).
    pub content_size: [f64; 2],
}

/// Fits one Folio graphic frame and moves it into spread coordinates (pages left to right, the
/// convention of `reflow`).
pub fn place_graphic(
    document: &StudioDocument,
    g: &StudioGraphicFrame,
) -> Result<PlacedGraphic, FitError> {
    let loc = document
        .locate_page(&g.page_id)
        .ok_or(LayoutError::InvalidInput)?;
    let page_x = loc.spread.pages[..loc.page_index]
        .iter()
        .map(|p| pt(&p.width))
        .sum::<Result<f64, LayoutError>>()?;
    let b = &g.bounds;
    let (x, y, w, h) = (pt(&b.x)?, pt(&b.y)?, pt(&b.width)?, pt(&b.height)?);
    let frame = Rect {
        x0_pt: page_x + x,
        y0_pt: y,
        x1_pt: page_x + x + w,
        y1_pt: y + h,
    };
    let crop = match &g.crop {
        None => Crop {
            top: 0.0,
            left: 0.0,
            bottom: 0.0,
            right: 0.0,
        },
        Some(e) => Crop {
            top: pt(&e.top)?,
            left: pt(&e.left)?,
            bottom: pt(&e.bottom)?,
            right: pt(&e.right)?,
        },
    };
    let content_size = [pt(&g.content_width)?, pt(&g.content_height)?];
    let fitted = fit_content(
        frame,
        content_size[0],
        content_size[1],
        g.fit,
        g.frame_fitting,
        g.anchor,
        crop,
    )?;
    Ok(PlacedGraphic {
        layer_id: g.layer_id.clone(),
        spread: loc.spread_index,
        page: loc.page_index,
        frame: fitted.frame,
        shape: g.shape.clone(),
        transform: fitted.transform,
        content_box: fitted.content_box,
        content_size,
    })
}
