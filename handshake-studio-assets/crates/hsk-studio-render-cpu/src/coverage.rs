//! Path-fill coverage: exact winding of the true curve (kurbo, analytic on cubic segments) sampled
//! on an N x N pixel-centred grid. Coverage is `hits / N^2`, so a pixel fully inside is exactly 1.0,
//! fully outside exactly 0.0, and an axis-aligned half-pixel edge exactly 0.5 for even N.
//! The path model itself is nib's `OwnedPath` (CX-CAP-001); this module only rasterizes it.
use crate::plan_sink::{RenderError, unsupported};
use hsk_studio_nib::{Error as NibError, FillRule, OwnedPath, PathLimits};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::BackendError;
use kurbo::{BezPath, PathEl, Shape};

#[derive(Clone, Debug)]
pub(crate) struct PreparedPath {
    path: BezPath,
    pub(crate) bounds: Rect,
    even_odd: bool,
    samples: u32,
}

fn nib_error(error: NibError, limits: &PathLimits, requested: usize) -> RenderError {
    match error {
        NibError::BudgetExceeded => RenderError::Backend(BackendError::BudgetExceeded {
            requested: requested as u64,
            limit: limits.max_elements as u64,
        }),
        _ => unsupported("vector_geometry"),
    }
}

impl PreparedPath {
    /// Validates `path` (finite, bounded, well-formed subpaths), closes every open subpath with a
    /// straight segment (fill semantics), and computes the integer pixel bounding box.
    pub(crate) fn prepare(
        path: &OwnedPath,
        samples: u8,
        limits: &PathLimits,
    ) -> Result<Self, RenderError> {
        if !(1..=16).contains(&samples) {
            return Err(unsupported("vector_aa_samples"));
        }
        path.validate(limits)
            .map_err(|e| nib_error(e, limits, path.elements.len()))?;
        let source = path
            .to_kurbo()
            .map_err(|e| nib_error(e, limits, path.elements.len()))?;
        // kurbo's winding only counts a subpath's closing segment when it is explicit.
        let mut closed = BezPath::new();
        let mut open = false;
        for element in source.elements() {
            match element {
                PathEl::MoveTo(_) => {
                    if open {
                        closed.close_path();
                    }
                    open = false;
                    closed.push(*element);
                }
                PathEl::ClosePath => {
                    closed.push(*element);
                    open = false;
                }
                _ => {
                    closed.push(*element);
                    open = true;
                }
            }
        }
        if open {
            closed.close_path();
        }
        let bounds = if closed.elements().is_empty() {
            Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            }
        } else {
            let bb = closed.bounding_box();
            let (x0, y0) = (bb.x0.floor(), bb.y0.floor());
            let (x1, y1) = (bb.x1.ceil(), bb.y1.ceil());
            let overflow = |_| RenderError::Backend(BackendError::Overflow);
            Rect {
                x: x0 as i64,
                y: y0 as i64,
                width: u32::try_from((x1 - x0) as i64).map_err(overflow)?,
                height: u32::try_from((y1 - y0) as i64).map_err(overflow)?,
            }
        };
        Ok(Self {
            path: closed,
            bounds,
            even_odd: path.fill_rule == FillRule::EvenOdd,
            samples: u32::from(samples),
        })
    }

    /// Fraction of the pixel `[px, px+1) x [py, py+1)` inside the filled region.
    pub(crate) fn coverage(&self, pixel_x: i64, pixel_y: i64) -> f32 {
        let n = self.samples;
        let step = 1.0 / f64::from(n);
        let mut hits = 0_u32;
        for j in 0..n {
            for i in 0..n {
                let x = pixel_x as f64 + (f64::from(i) + 0.5) * step;
                let y = pixel_y as f64 + (f64::from(j) + 0.5) * step;
                let w = self.path.winding(kurbo::Point::new(x, y));
                let inside = if self.even_odd { w & 1 != 0 } else { w != 0 };
                hits += u32::from(inside);
            }
        }
        (f64::from(hits) / f64::from(n * n)) as f32
    }
}
