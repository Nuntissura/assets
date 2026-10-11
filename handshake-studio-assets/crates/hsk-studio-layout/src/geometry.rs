//! Page, spread and text-frame geometry. Every length is in points (STU-DOC-003) and every field
//! name carries `_pt`. Folio has no frame or thread type yet, so this model is crate-local.
use crate::LayoutError;

/// Axis-aligned rectangle; `x0 <= x1` and `y0 <= y1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0_pt: f64,
    pub y0_pt: f64,
    pub x1_pt: f64,
    pub y1_pt: f64,
}
impl Rect {
    fn valid(&self) -> bool {
        [self.x0_pt, self.y0_pt, self.x1_pt, self.y1_pt]
            .iter()
            .all(|v| v.is_finite())
            && self.x0_pt <= self.x1_pt
            && self.y0_pt <= self.y1_pt
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Margins {
    pub top_pt: f64,
    pub right_pt: f64,
    pub bottom_pt: f64,
    pub left_pt: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub id: String,
    pub width_pt: f64,
    pub height_pt: f64,
    pub margins: Margins,
}
impl Page {
    /// Margin box in page coordinates; `None` when the margins leave no area.
    pub fn content_rect(&self) -> Option<Rect> {
        let m = &self.margins;
        let r = Rect {
            x0_pt: m.left_pt,
            y0_pt: m.top_pt,
            x1_pt: self.width_pt - m.right_pt,
            y1_pt: self.height_pt - m.bottom_pt,
        };
        (r.x0_pt < r.x1_pt && r.y0_pt < r.y1_pt).then_some(r)
    }
}

/// Facing pages laid left to right without a gutter; spread coordinates start at the left edge of
/// the first page, y grows downward.
#[derive(Clone, Debug, PartialEq)]
pub struct Spread {
    pub id: String,
    pub pages: Vec<Page>,
}
impl Spread {
    pub fn width_pt(&self) -> f64 {
        self.pages.iter().map(|p| p.width_pt).sum()
    }
    pub fn height_pt(&self) -> f64 {
        self.pages.iter().map(|p| p.height_pt).fold(0.0, f64::max)
    }
    /// Left edge of `page` in spread coordinates.
    pub fn page_origin_x_pt(&self, page: usize) -> Option<f64> {
        (page < self.pages.len()).then(|| self.pages[..page].iter().map(|p| p.width_pt).sum())
    }
    pub(crate) fn validate(&self) -> Result<(), LayoutError> {
        if self.pages.is_empty() {
            return Err(LayoutError::InvalidGeometry);
        }
        for p in &self.pages {
            let m = &p.margins;
            if ![p.width_pt, p.height_pt].iter().all(|v| v.is_finite() && *v > 0.0)
                || ![m.top_pt, m.right_pt, m.bottom_pt, m.left_pt]
                    .iter()
                    .all(|v| v.is_finite() && *v >= 0.0)
            {
                return Err(LayoutError::InvalidGeometry);
            }
        }
        Ok(())
    }
}

/// A text frame placed on one page of one spread. `x_pt`/`y_pt` are the frame's top-left corner in
/// page coordinates. `first_baseline_pt` is measured from the frame top and is explicit (no default).
/// `obstacles` are text-wrap rectangles in frame coordinates (origin = frame top-left).
#[derive(Clone, Debug, PartialEq)]
pub struct TextFrame {
    pub id: String,
    pub spread: usize,
    pub page: usize,
    pub x_pt: f64,
    pub y_pt: f64,
    pub width_pt: f64,
    pub height_pt: f64,
    pub inset_pt: f64,
    pub first_baseline_pt: f64,
    pub obstacles: Vec<Rect>,
}
impl TextFrame {
    /// Top-left corner in spread coordinates.
    pub fn origin_in_spread(&self, spreads: &[Spread]) -> Option<(f64, f64)> {
        let x = spreads.get(self.spread)?.page_origin_x_pt(self.page)?;
        Some((x + self.x_pt, self.y_pt))
    }
    pub(crate) fn validate(&self, spreads: &[Spread]) -> Result<(), LayoutError> {
        let finite = [
            self.x_pt,
            self.y_pt,
            self.width_pt,
            self.height_pt,
            self.inset_pt,
            self.first_baseline_pt,
        ]
        .iter()
        .all(|v| v.is_finite());
        if !finite
            || self.width_pt <= 0.0
            || self.height_pt <= 0.0
            || self.inset_pt < 0.0
            || self.inset_pt * 2.0 >= self.width_pt
            || self.inset_pt * 2.0 >= self.height_pt
            || self.first_baseline_pt < self.inset_pt
            || self.origin_in_spread(spreads).is_none()
            || !self.obstacles.iter().all(Rect::valid)
        {
            return Err(LayoutError::InvalidGeometry);
        }
        Ok(())
    }
}
