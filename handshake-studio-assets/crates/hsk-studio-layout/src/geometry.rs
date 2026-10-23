//! Plain geometry that is layout's own. Pages, spreads, text frames and threads are Folio's
//! (`folio_input` reads them); only text-wrap obstacles remain here because Folio stores none.

/// Axis-aligned rectangle in points; `x0 <= x1` and `y0 <= y1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0_pt: f64,
    pub y0_pt: f64,
    pub x1_pt: f64,
    pub y1_pt: f64,
}
impl Rect {
    pub(crate) fn valid(&self) -> bool {
        [self.x0_pt, self.y0_pt, self.x1_pt, self.y1_pt]
            .iter()
            .all(|v| v.is_finite())
            && self.x0_pt <= self.x1_pt
            && self.y0_pt <= self.y1_pt
    }
}
