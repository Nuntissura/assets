//! Magic wand / flood select (STU-RAS-018 "Flood select (tolerance)", STU-RAS-119 `contiguous`
//! default true, `sample_all_layers` default false, `anti_alias` default true).
//!
//! Studio reading (class Approximate, Photoshop UNVERIFIED, D5): a pixel matches when the
//! Chebyshev distance between its premultiplied RGBA and the seed's, in 8-bit units (`x 255`), is
//! at most `tolerance`. The seed colour is the mean premultiplied RGBA over a `sample_size` square
//! centred on the clicked pixel (clipped to the canvas). Contiguous mode floods from the seed
//! through matching pixels (4- or 8-connected); global mode takes every matching pixel. With the
//! document anti-alias toggle on, pixels on the selected / unselected boundary get the 3x3 box
//! mean of the binary selection (pixels outside the canvas repeat the centre); others are 0 or 1.
#![deny(clippy::arithmetic_side_effects)]
use super::{Composite, ToolError, ToolLimits, admit_plane, coords_of, index_in, tighten};
use crate::selection::{ColourView, CoveragePlane, SelectionContext};
use crate::{Error, Rect};

/// Neighbourhood of the contiguous flood (Studio default 4-connected, UNVERIFIED).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Connectivity {
    #[default]
    Four,
    Eight,
}

/// Magic-wand options. `tolerance` is in 8-bit units and is not clamped: any finite value `>= 0`
/// is admitted (`>= 255` selects every pixel).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WandOptions {
    pub tolerance: f64,
    pub contiguous: bool,
    pub sample_merged: bool,
    /// Odd side of the seed sampling square (1 = point sample).
    pub sample_size: u32,
    pub connectivity: Connectivity,
}
impl Default for WandOptions {
    /// Tolerance 32 is a Studio default (the Photoshop default is UNVERIFIED); `contiguous` true
    /// and `sample_merged` false follow STU-RAS-119.
    fn default() -> Self {
        Self {
            tolerance: 32.0,
            contiguous: true,
            sample_merged: false,
            sample_size: 1,
            connectivity: Connectivity::Four,
        }
    }
}

/// Working bytes per canvas pixel: state byte + flood stack entry (8) + canvas-size output (2).
const WORK_BYTES_PER_PIXEL: u64 = 11;

const UNSEEN: u8 = 0;
const IN: u8 = 1;
const OUT: u8 = 2;

/// Magic wand over `layers` (bottom-to-top; `active` is the sampled layer unless
/// `sample_merged`). The result is clipped to `ctx.canvas` and tightened to its non-zero samples.
pub fn magic_wand(
    layers: &[ColourView<'_>],
    active: usize,
    seed: (i64, i64),
    opts: &WandOptions,
    ctx: &SelectionContext,
    limits: &ToolLimits,
) -> Result<CoveragePlane, ToolError> {
    if !opts.tolerance.is_finite() {
        return Err(Error::Nonfinite.into());
    }
    if opts.tolerance < 0.0 || opts.sample_size == 0 || opts.sample_size.is_multiple_of(2) {
        return Err(ToolError::OutOfRange);
    }
    let src = Composite::select(layers, active, opts.sample_merged)?;
    let canvas = ctx.canvas;
    canvas.end()?;
    let seed_index = index_in(canvas, seed.0, seed.1).ok_or(Error::InvalidInput)?;
    admit_plane(canvas, &ctx.limits)?;
    let n = limits.admit(canvas.area(), WORK_BYTES_PER_PIXEL)?;

    // Seed colour: mean premultiplied RGBA over the sample square clipped to the canvas.
    let half = i64::from(opts.sample_size / 2);
    let square = Rect {
        x: seed.0.checked_sub(half).ok_or(Error::Overflow)?,
        y: seed.1.checked_sub(half).ok_or(Error::Overflow)?,
        width: opts.sample_size,
        height: opts.sample_size,
    };
    let square = square.intersect(canvas)?.ok_or(Error::InvalidInput)?;
    let (sx1, sy1) = square.end()?;
    let mut sum = [0.0f64; 4];
    for y in square.y..sy1 {
        for x in square.x..sx1 {
            let p = src.premul(x, y)?;
            for (s, v) in sum.iter_mut().zip(p) {
                *s += v;
            }
        }
    }
    let count = square.area() as f64;
    let target = sum.map(|s| s / count);
    // 1e-9 absorbs the binary rounding of k/255 encodings so an 8-bit step of exactly
    // `tolerance` is inside.
    let limit = opts.tolerance + 1e-9;
    let matches = |x: i64, y: i64| -> Result<bool, Error> {
        let p = src.premul(x, y)?;
        let d = p
            .iter()
            .zip(target)
            .map(|(a, b)| (a - b).abs() * 255.0)
            .fold(0.0f64, f64::max);
        Ok(d <= limit)
    };

    let mut state = vec![UNSEEN; n];
    if opts.contiguous {
        let offsets: &[(i64, i64)] = match opts.connectivity {
            Connectivity::Four => &[(1, 0), (-1, 0), (0, 1), (0, -1)],
            Connectivity::Eight => &[
                (1, 0),
                (-1, 0),
                (0, 1),
                (0, -1),
                (1, 1),
                (1, -1),
                (-1, 1),
                (-1, -1),
            ],
        };
        // Mark on push: every pixel enters the stack at most once (stack <= canvas pixels,
        // admitted above).
        let mut stack: Vec<usize> = Vec::new();
        if let Some(s) = state.get_mut(seed_index) {
            *s = IN;
        }
        stack.push(seed_index);
        while let Some(i) = stack.pop() {
            let (x, y) = coords_of(canvas, i);
            for (ox, oy) in offsets {
                let (nx, ny) = (x.saturating_add(*ox), y.saturating_add(*oy));
                let Some(j) = index_in(canvas, nx, ny) else {
                    continue;
                };
                if state.get(j) != Some(&UNSEEN) {
                    continue;
                }
                let m = matches(nx, ny)?;
                if let Some(s) = state.get_mut(j) {
                    *s = if m { IN } else { OUT };
                }
                if m {
                    stack.push(j);
                }
            }
        }
    } else {
        for (i, s) in state.iter_mut().enumerate() {
            let (x, y) = coords_of(canvas, i);
            *s = if matches(x, y)? { IN } else { OUT };
        }
    }

    let selected = |i: usize| state.get(i) == Some(&IN);
    let mut data = Vec::with_capacity(n);
    for i in 0..n {
        let centre = selected(i);
        if !ctx.anti_alias {
            data.push(if centre { u16::MAX } else { 0 });
            continue;
        }
        let (x, y) = coords_of(canvas, i);
        let mut boundary = false;
        let mut hits = 0u64;
        for oy in -1i64..=1 {
            for ox in -1i64..=1 {
                let v = match index_in(canvas, x.saturating_add(ox), y.saturating_add(oy)) {
                    Some(j) => selected(j),
                    None => centre,
                };
                if v != centre && (ox == 0 || oy == 0) {
                    boundary = true;
                }
                if v {
                    hits = hits.saturating_add(1);
                }
            }
        }
        data.push(if boundary {
            // round(hits / 9 * 65535), round half up.
            let q = hits
                .saturating_mul(65535)
                .saturating_mul(2)
                .saturating_add(9)
                .checked_div(18)
                .unwrap_or(0);
            u16::try_from(q).unwrap_or(u16::MAX)
        } else if centre {
            u16::MAX
        } else {
            0
        });
    }
    Ok(tighten(CoveragePlane {
        bounds: canvas,
        data,
    })?)
}
