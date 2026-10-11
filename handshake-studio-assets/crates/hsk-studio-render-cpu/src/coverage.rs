//! Path-fill coverage: exact winding of the true curve (kurbo, analytic on cubic segments) sampled
//! on an N x N pixel-centred grid. Coverage is `hits / N^2`, so a pixel fully inside is exactly 1.0,
//! fully outside exactly 0.0, and an axis-aligned half-pixel edge exactly 0.5 for even N.
use crate::RenderError;
use hsk_studio_accord::Unit;
use hsk_studio_nib::{Anchor, Point, Winding};
use hsk_studio_pigment::Rect;
use kurbo::{BezPath, Shape};

/// Magnitude cap (pixels) so integer bounds and sample coordinates stay exact in binary64.
const COORD_LIMIT: f64 = 1.0e9;

#[derive(Clone, Debug)]
pub(crate) struct PreparedPath {
    path: BezPath,
    pub(crate) bounds: Rect,
    even_odd: bool,
    samples: u32,
}

fn px(value: hsk_studio_accord::Length, ppi: Option<f64>) -> Result<f64, RenderError> {
    let v = value
        .convert(Unit::Pixels, ppi)
        .map_err(|_| RenderError::InvalidGeometry)?
        .value();
    if v.is_finite() && v.abs() <= COORD_LIMIT {
        Ok(v)
    } else {
        Err(RenderError::InvalidGeometry)
    }
}

fn pair(p: Point, ppi: Option<f64>) -> Result<(f64, f64), RenderError> {
    Ok((px(p.x, ppi)?, px(p.y, ppi)?))
}

impl PreparedPath {
    /// Anchor handles are vectors relative to their anchor (nib descriptor: "zero relative pt
    /// vectors"). The outgoing handle of anchor A and the incoming handle of anchor B bound the
    /// cubic A -> B; both zero means a straight segment. The path is always filled closed; when
    /// `closed` is false the closing segment is a straight line (SVG/PDF fill semantics).
    pub(crate) fn prepare(
        anchors: &[Anchor],
        closed: bool,
        winding: Winding,
        samples: u8,
        ppi: Option<f64>,
    ) -> Result<Self, RenderError> {
        let even_odd = match winding {
            Winding::NonZero => false,
            Winding::EvenOdd => true,
            Winding::None => return Err(RenderError::UnsupportedOp("path_fill_winding_none")),
        };
        if anchors.len() < 2 {
            return Err(RenderError::InvalidGeometry);
        }
        if !(1..=16).contains(&samples) {
            return Err(RenderError::InvalidInput("aa_samples"));
        }
        let mut path = BezPath::new();
        let first = pair(anchors[0].position, ppi)?;
        path.move_to(first);
        let segments = if closed {
            anchors.len()
        } else {
            anchors.len() - 1
        };
        for i in 0..segments {
            let a = &anchors[i];
            let b = &anchors[(i + 1) % anchors.len()];
            let (ax, ay) = pair(a.position, ppi)?;
            let (bx, by) = pair(b.position, ppi)?;
            let (ox, oy) = pair(a.outgoing, ppi)?;
            let (ix, iy) = pair(b.incoming, ppi)?;
            if ox == 0.0 && oy == 0.0 && ix == 0.0 && iy == 0.0 {
                path.line_to((bx, by));
            } else {
                path.curve_to((ax + ox, ay + oy), (bx + ix, by + iy), (bx, by));
            }
        }
        path.close_path();
        let bb = path.bounding_box();
        let (x0, y0) = (bb.x0.floor(), bb.y0.floor());
        let (x1, y1) = (bb.x1.ceil(), bb.y1.ceil());
        if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) {
            return Err(RenderError::InvalidGeometry);
        }
        let width = u32::try_from((x1 - x0) as i64).map_err(|_| RenderError::Overflow)?;
        let height = u32::try_from((y1 - y0) as i64).map_err(|_| RenderError::Overflow)?;
        Ok(Self {
            path,
            bounds: Rect {
                x: x0 as i64,
                y: y0 as i64,
                width,
                height,
            },
            even_odd,
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
