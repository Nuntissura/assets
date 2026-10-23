//! Crop, canvas size and trim (lane SMB-PIGMENT-RETOUCH slice 3, Photoshop parity gap PS-21; spec
//! basis STU-RAS-142 transform/crop/trim parameter contracts, STU-RAS-141 nine-member anchor).
//! Pure compute over [`RgbaPlane`]s; the caller applies the result to every layer and to the
//! document canvas (one operator call per layer plane).
//!
//! Coordinates. A plane lives in document pixel coordinates; the document canvas is the rect
//! `(0, 0, W, H)`. Every operator returns a [`CanvasResult`]: the new plane, the new canvas
//! `(0, 0, W', H')` and the integer `offset` that old document coordinates moved by (for a crop
//! with rotation the offset is the crop rect's top-left negated; the rotation is in
//! `rotation_deg`).
//!
//! - Canvas size ([`canvas_resize`]): content moves by `offset = floor((W' - W) a_x)`,
//!   `floor((H' - H) a_y)` with `a = 0 | 1/2 | 1` by the anchor column and row (floor division,
//!   so a centred odd difference puts the extra pixel right / below: Studio reading, Photoshop
//!   rounding UNVERIFIED); new area outside the old plane is `fill` (transparent for a layer, the
//!   background colour for a background layer: the caller's choice); content beyond the new canvas
//!   is dropped.
//! - Crop ([`crop`]): `rect` in document coordinates. Angle 0 is an exact integer copy; a
//!   rectangle beyond the plane reads transparent there (crop-expand). With
//!   `content_aware_fill_on_expand = true` the newly exposed area is synthesized by the native
//!   content-aware fill ([`crate::content_aware::fill`], lane SMB-PIGMENT-CONTENT-AWARE-2,
//!   STU-RAS-143 "content-aware crop-expand"): the hole is every pixel where an opaque
//!   mask of the plane bounds, cropped the same way, has alpha below 1 (so a rotated crop fills its
//!   transparent corners and their anti-aliased edge), and the crop pixel is composited
//!   source-over the fill there (a partly exposed pixel keeps its visible part); the sampling area
//!   is every other pixel of the crop result (keep-pixels: the moved plane);
//!   [`crop_content_aware`] takes the fill options and returns the quality receipt, [`crop`] uses
//!   `ContentAwareOptions::default()` (test `tests/content_aware_wiring.rs`). With
//!   `delete_cropped_pixels = false` (angle 0 only) the whole plane is kept and only moved by the
//!   offset, so pixels outside the new canvas survive (hidden; with content-aware expand the plane
//!   grows to the union of the moved plane and the canvas, and only the canvas pixels outside the
//!   moved plane are filled); a rotated crop that keeps pixels is
//!   refused (`crop_rotate_keep_pixels_unsupported`). A non-zero `angle_deg` (clockwise on screen,
//!   y down; `-360..=360`) rotates the crop rectangle about its centre on the source and returns its
//!   content upright: destination `d = R(-angle) (s - c) + (w/2, h/2)` resampled by the layer
//!   transform operator ([`crate::transform_math`], edge transparent, the caller's filter), so the
//!   bytes equal `transform_math::resample_reference` of that transform (test
//!   `tests/retouch.rs` `canvas::crop_copies_exactly_and_rotation_is_the_transform_operator`).
//! - Trim ([`trim`], [`trim_rect`]): basis `transparent_pixels = 0` (alpha exactly 0),
//!   `top_left_pixel = 1`, `bottom_right_pixel = 9` (the discriminant 9 is preserved as the spec
//!   requires; a pixel equal in all four components to that corner pixel is trimmed); the selected
//!   edges move inwards to the content bounds, unselected edges stay. Everything trimmed is the
//!   typed refusal `trim_everything` (Photoshop behaviour there UNVERIFIED).
//!
//! Hostile sizes (V4-01/02): output bytes are checked against [`RetouchLimits`] before
//! allocation; integer arithmetic is checked.
#![deny(clippy::arithmetic_side_effects)]
use crate::content_aware::{ContentAwareOptions, ContentAwareReceipt, SamplingArea};
use crate::retouch::{RGBA_SAMPLE_BYTES, RetouchError, RetouchLimits, RgbaPlane, check_sample, paint_over};
use crate::selection::CoveragePlane;
use crate::transform_math::{
    Affine2, EdgeMode, LayerTransform, PreparedTransform, ResampleFilter,
};
use crate::{Error, Rect};

/// STU-RAS-141 anchor; discriminants are the spec values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Anchor {
    TopLeft = 1,
    TopCentre = 2,
    TopRight = 3,
    MiddleLeft = 4,
    MiddleCentre = 5,
    MiddleRight = 6,
    BottomLeft = 7,
    BottomCentre = 8,
    BottomRight = 9,
}
impl Anchor {
    pub const fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Self::TopLeft,
            2 => Self::TopCentre,
            3 => Self::TopRight,
            4 => Self::MiddleLeft,
            5 => Self::MiddleCentre,
            6 => Self::MiddleRight,
            7 => Self::BottomLeft,
            8 => Self::BottomCentre,
            9 => Self::BottomRight,
            _ => return None,
        })
    }
    pub const fn key(self) -> &'static str {
        match self {
            Self::TopLeft => "top_left",
            Self::TopCentre => "top_centre",
            Self::TopRight => "top_right",
            Self::MiddleLeft => "middle_left",
            Self::MiddleCentre => "middle_centre",
            Self::MiddleRight => "middle_right",
            Self::BottomLeft => "bottom_left",
            Self::BottomCentre => "bottom_centre",
            Self::BottomRight => "bottom_right",
        }
    }
    /// Column and row in `0..=2` (0 = left/top, 1 = centre/middle, 2 = right/bottom).
    pub const fn column_row(self) -> (i64, i64) {
        match self {
            Self::TopLeft => (0, 0),
            Self::TopCentre => (1, 0),
            Self::TopRight => (2, 0),
            Self::MiddleLeft => (0, 1),
            Self::MiddleCentre => (1, 1),
            Self::MiddleRight => (2, 1),
            Self::BottomLeft => (0, 2),
            Self::BottomCentre => (1, 2),
            Self::BottomRight => (2, 2),
        }
    }
}

/// Result of a canvas operation.
#[derive(Clone, Debug, PartialEq)]
pub struct CanvasResult {
    pub plane: RgbaPlane,
    /// The new document canvas, `(0, 0, W', H')`.
    pub canvas: Rect,
    /// Old document coordinates move by this offset (`new = old + offset`).
    pub offset: (i64, i64),
    /// Crop rotation applied (degrees; 0 for every other operator).
    pub rotation_deg: f64,
}

/// `floor(diff * column / 2)` for column in `0..=2`.
fn anchor_shift(diff: i64, column: i64) -> Result<i64, Error> {
    diff.checked_mul(column)
        .and_then(|v| v.checked_div_euclid(2))
        .ok_or(Error::Overflow)
}

fn canvas_rect(width: u32, height: u32) -> Rect {
    Rect { x: 0, y: 0, width, height }
}

/// Copy of `plane` moved by `offset` and clipped to `out`; `fill` where the moved plane is absent.
fn place(
    plane: &RgbaPlane,
    offset: (i64, i64),
    out: Rect,
    fill: [f32; 4],
    limits: &RetouchLimits,
) -> Result<RgbaPlane, RetouchError> {
    let n = limits.admit_area(out, RGBA_SAMPLE_BYTES)?;
    let (ex, ey) = out.end()?;
    let mut data = Vec::with_capacity(n);
    for y in out.y..ey {
        let sy = y.checked_sub(offset.1).ok_or(Error::Overflow)?;
        for x in out.x..ex {
            let sx = x.checked_sub(offset.0).ok_or(Error::Overflow)?;
            data.push(match plane.index(sx, sy) {
                Some(i) => plane.data.get(i).copied().unwrap_or(fill),
                None => fill,
            });
        }
    }
    Ok(RgbaPlane { bounds: out, data })
}

/// STU-RAS-142 canvas size: the old canvas `(0, 0, old_width, old_height)` becomes
/// `(0, 0, new_width, new_height)` anchored at `anchor`; `fill` covers the new area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasResize {
    pub old_width: u32,
    pub old_height: u32,
    pub new_width: u32,
    pub new_height: u32,
    pub anchor: Anchor,
    pub fill: [f32; 4],
}

/// Canvas size ([`CanvasResize`]).
pub fn canvas_resize(
    plane: &RgbaPlane,
    resize: &CanvasResize,
    limits: &RetouchLimits,
) -> Result<CanvasResult, RetouchError> {
    plane.validate()?;
    check_sample(resize.fill)?;
    let canvas = canvas_rect(resize.new_width, resize.new_height);
    limits.admit_area(canvas, RGBA_SAMPLE_BYTES)?;
    let (column, row) = resize.anchor.column_row();
    let dw = i64::from(resize.new_width)
        .checked_sub(i64::from(resize.old_width))
        .ok_or(Error::Overflow)?;
    let dh = i64::from(resize.new_height)
        .checked_sub(i64::from(resize.old_height))
        .ok_or(Error::Overflow)?;
    let offset = (anchor_shift(dw, column)?, anchor_shift(dh, row)?);
    let out = place(plane, offset, canvas, resize.fill, limits)?;
    Ok(CanvasResult { plane: out, canvas, offset, rotation_deg: 0.0 })
}

/// Crop options (STU-RAS-142 `angle`, `delete_cropped_pixels`, `content_aware_fill_on_expand`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropOptions {
    /// Clockwise on screen (y down), `-360..=360`; 0 = no rotation.
    pub angle_deg: f64,
    /// Resampling filter of a rotated crop (ignored at angle 0).
    pub filter: ResampleFilter,
    pub delete_cropped_pixels: bool,
    pub content_aware_fill_on_expand: bool,
}
impl Default for CropOptions {
    fn default() -> Self {
        Self {
            angle_deg: 0.0,
            filter: ResampleFilter::Bicubic,
            delete_cropped_pixels: true,
            content_aware_fill_on_expand: false,
        }
    }
}

/// The layer transform of a rotated crop of `rect` (see the module docs).
pub fn crop_transform(rect: Rect, options: &CropOptions) -> LayerTransform {
    let (w, h) = (f64::from(rect.width), f64::from(rect.height));
    let (cx, cy) = (rect.x as f64 + w / 2.0, rect.y as f64 + h / 2.0);
    let affine = Affine2::from_parts(cx, cy, 1.0, 1.0, -options.angle_deg, w / 2.0 - cx, h / 2.0 - cy);
    LayerTransform::new(affine, options.filter).with_edge(EdgeMode::Transparent)
}

/// Crop the plane to `rect` (document coordinates) with optional rotation; a content-aware expand
/// uses `ContentAwareOptions::default()` ([`crop_content_aware`]).
pub fn crop(
    plane: &RgbaPlane,
    rect: Rect,
    options: &CropOptions,
    limits: &RetouchLimits,
) -> Result<CanvasResult, RetouchError> {
    crop_content_aware(plane, rect, options, &ContentAwareOptions::default(), limits).map(|(r, _)| r)
}

/// Crop like [`crop`]; when `content_aware_fill_on_expand` is set and the crop exposes area outside
/// the plane, that area is synthesized by [`crate::content_aware::fill`] with `fill` (module docs)
/// and its quality receipt is returned (`None` when no fill ran). The fill options are validated
/// before any work (typed refusals of `content_aware::fill`); a crop result with nothing to sample
/// is `content_aware_no_source`. Studio reading, class Approximate; Photoshop's crop-expand fill is
/// UNVERIFIED (D5).
pub fn crop_content_aware(
    plane: &RgbaPlane,
    rect: Rect,
    options: &CropOptions,
    fill: &ContentAwareOptions,
    limits: &RetouchLimits,
) -> Result<(CanvasResult, Option<ContentAwareReceipt>), RetouchError> {
    if !options.content_aware_fill_on_expand {
        return Ok((crop_plain(plane, rect, options, limits)?, None));
    }
    fill.validate()?;
    let plain = crop_plain(plane, rect, options, limits)?;
    let expands = !plane.bounds.contains(rect)? || options.angle_deg.rem_euclid(360.0) != 0.0;
    if !expands {
        return Ok((plain, None));
    }
    let CanvasResult { plane: cropped, canvas, offset, rotation_deg } = plain;
    let mut blank: Vec<bool> = Vec::new();
    let (target, hole, sampling) = if options.delete_cropped_pixels {
        // Exposed = the pixels where an opaque mask of the plane bounds, cropped the same way, has
        // alpha below 1 (`round((1 - alpha) 65535) > 0`); the hole is binary and the visible part
        // of a partly exposed pixel is composited over the fill below.
        let mask = RgbaPlane::filled(plane.bounds, [1.0; 4], limits)?;
        let m = crop_plain(&mask, rect, options, limits)?;
        let data = m
            .plane
            .data
            .iter()
            .map(|p| {
                let open = ((1.0 - f64::from(p[3]).clamp(0.0, 1.0)) * f64::from(u16::MAX)).round();
                if open > 0.0 { u16::MAX } else { 0 }
            })
            .collect();
        (cropped, CoveragePlane { bounds: m.plane.bounds, data }, SamplingArea::Auto)
    } else {
        // Keep pixels (angle 0): the plane grows to the union of the moved plane and the canvas.
        // Every union pixel outside the moved plane is synthesized (so no transparent filler
        // enters a patch or the healing boundary); afterwards only the canvas pixels keep the fill
        // and the pixels outside both the canvas and the moved plane return to transparent.
        let moved = cropped.bounds;
        let all = union_rect(moved, canvas)?;
        let grown = place(&cropped, (0, 0), all, [0.0; 4], limits)?;
        let n = limits.admit_area(all, 3)?;
        let mut data = Vec::with_capacity(n);
        blank = Vec::with_capacity(n);
        let (ex, ey) = all.end()?;
        let (mx, my) = moved.end()?;
        let (cx, cy) = canvas.end()?;
        for y in all.y..ey {
            for x in all.x..ex {
                let in_canvas = x >= canvas.x && y >= canvas.y && x < cx && y < cy;
                let in_moved = x >= moved.x && y >= moved.y && x < mx && y < my;
                data.push(if in_moved { 0 } else { u16::MAX });
                blank.push(!in_moved && !in_canvas);
            }
        }
        // Only the moved plane holds content: the rest of the union is never sampled.
        (grown, CoveragePlane { bounds: all, data }, SamplingArea::Rectangular(moved))
    };
    let filled = crate::content_aware::fill(&target, &hole, sampling, fill, limits)?;
    let mut out = filled.result.plane;
    for (o, b) in out.data.iter_mut().zip(&blank) {
        if *b {
            *o = [0.0; 4];
        }
    }
    // The visible part of a partly exposed pixel stays on top: crop pixel over the fill.
    for (i, (o, h)) in out.data.iter_mut().zip(&hole.data).enumerate() {
        if *h > 0
            && let Some(d) = target.data.get(i)
            && d[3] > 0.0
        {
            *o = paint_over(*o, *d, 1.0);
        }
    }
    Ok((CanvasResult { plane: out, canvas, offset, rotation_deg }, Some(filled.receipt)))
}

/// Smallest rectangle containing `a` and `b` (checked).
fn union_rect(a: Rect, b: Rect) -> Result<Rect, Error> {
    let (ae, af) = a.end()?;
    let (be, bf) = b.end()?;
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    let (e, f) = (ae.max(be), af.max(bf));
    Ok(Rect {
        x,
        y,
        width: u32::try_from(e.checked_sub(x).ok_or(Error::Overflow)?).map_err(|_| Error::Overflow)?,
        height: u32::try_from(f.checked_sub(y).ok_or(Error::Overflow)?).map_err(|_| Error::Overflow)?,
    })
}

/// The crop without content-aware expand (exposed area transparent).
fn crop_plain(
    plane: &RgbaPlane,
    rect: Rect,
    options: &CropOptions,
    limits: &RetouchLimits,
) -> Result<CanvasResult, RetouchError> {
    plane.validate()?;
    rect.end()?;
    let a = options.angle_deg;
    if !(a.is_finite() && (-360.0..=360.0).contains(&a)) {
        return Err(RetouchError::OutOfRange);
    }
    if rect.width == 0 || rect.height == 0 {
        return Err(RetouchError::OutOfRange);
    }
    let canvas = canvas_rect(rect.width, rect.height);
    let offset = (
        rect.x.checked_neg().ok_or(Error::Overflow)?,
        rect.y.checked_neg().ok_or(Error::Overflow)?,
    );
    if a.rem_euclid(360.0) == 0.0 {
        if options.delete_cropped_pixels {
            let out = place(plane, offset, canvas, [0.0; 4], limits)?;
            return Ok(CanvasResult { plane: out, canvas, offset, rotation_deg: 0.0 });
        }
        // Keep every pixel; only move the plane.
        limits.admit_area(plane.bounds, RGBA_SAMPLE_BYTES)?;
        let bounds = Rect {
            x: plane.bounds.x.checked_add(offset.0).ok_or(Error::Overflow)?,
            y: plane.bounds.y.checked_add(offset.1).ok_or(Error::Overflow)?,
            ..plane.bounds
        };
        bounds.end()?;
        let out = RgbaPlane { bounds, data: plane.data.clone() };
        return Ok(CanvasResult { plane: out, canvas, offset, rotation_deg: 0.0 });
    }
    if !options.delete_cropped_pixels {
        return Err(RetouchError::CropRotateKeepPixelsUnsupported);
    }
    let n = limits.admit_area(canvas, RGBA_SAMPLE_BYTES)?;
    let mut data = Vec::with_capacity(n);
    if plane.bounds.area() == 0 {
        data.resize(n, [0.0; 4]);
    } else {
        let prepared = PreparedTransform::new(&crop_transform(rect, options), plane.bounds)?;
        for y in 0..i64::from(canvas.height) {
            for x in 0..i64::from(canvas.width) {
                let column_row = |i: u32, j: u32| -> [f32; 4] {
                    let at = (j as usize)
                        .checked_mul(plane.bounds.width as usize)
                        .and_then(|r| r.checked_add(i as usize));
                    at.and_then(|k| plane.data.get(k)).copied().unwrap_or([0.0; 4])
                };
                data.push(prepared.sample_rgba(x, y, |i, j| Ok::<_, RetouchError>(column_row(i, j)))?);
            }
        }
    }
    Ok(CanvasResult {
        plane: RgbaPlane { bounds: canvas, data },
        canvas,
        offset,
        rotation_deg: a,
    })
}

/// STU-RAS-142 trim basis; discriminants are the spec values (9 is real and preserved).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TrimBasis {
    TransparentPixels = 0,
    TopLeftPixel = 1,
    BottomRightPixel = 9,
}
impl TrimBasis {
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::TransparentPixels),
            1 => Some(Self::TopLeftPixel),
            9 => Some(Self::BottomRightPixel),
            _ => None,
        }
    }
}

/// Which edges a trim moves (STU-RAS-142 `top`, `left`, `bottom`, `right`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrimEdges {
    pub top: bool,
    pub left: bool,
    pub bottom: bool,
    pub right: bool,
}
impl TrimEdges {
    pub const ALL: Self = Self { top: true, left: true, bottom: true, right: true };
}

/// The rect a trim keeps (document coordinates), or `None` when every pixel is trimmed.
pub fn trim_rect(plane: &RgbaPlane, basis: TrimBasis, edges: TrimEdges) -> Result<Option<Rect>, RetouchError> {
    plane.validate()?;
    if plane.bounds.area() == 0 {
        return Ok(None);
    }
    let reference = match basis {
        TrimBasis::TransparentPixels => None,
        TrimBasis::TopLeftPixel => plane.data.first().copied(),
        TrimBasis::BottomRightPixel => plane.data.last().copied(),
    };
    let trimmed = |p: [f32; 4]| match reference {
        None => p[3] == 0.0,
        Some(r) => p == r,
    };
    let (ex, ey) = plane.bounds.end()?;
    let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    let mut k = 0usize;
    for y in plane.bounds.y..ey {
        for x in plane.bounds.x..ex {
            let p = plane.data.get(k).copied().unwrap_or([0.0; 4]);
            k = k.checked_add(1).ok_or(Error::Overflow)?;
            if !trimmed(p) {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x1 < x0 {
        return Ok(None);
    }
    let left = if edges.left { x0 } else { plane.bounds.x };
    let top = if edges.top { y0 } else { plane.bounds.y };
    let right = if edges.right { x1.checked_add(1).ok_or(Error::Overflow)? } else { ex };
    let bottom = if edges.bottom { y1.checked_add(1).ok_or(Error::Overflow)? } else { ey };
    let side = |a: i64, b: i64| -> Result<u32, Error> {
        u32::try_from(b.checked_sub(a).ok_or(Error::Overflow)?).map_err(|_| Error::Overflow)
    };
    Ok(Some(Rect { x: left, y: top, width: side(left, right)?, height: side(top, bottom)? }))
}

/// Trim: crop to [`trim_rect`]; everything trimmed is [`RetouchError::TrimEverything`].
pub fn trim(
    plane: &RgbaPlane,
    basis: TrimBasis,
    edges: TrimEdges,
    limits: &RetouchLimits,
) -> Result<CanvasResult, RetouchError> {
    let rect = trim_rect(plane, basis, edges)?.ok_or(RetouchError::TrimEverything)?;
    crop(plane, rect, &CropOptions::default(), limits)
}

/// Canvas surface description (Studio readings, parity NOT_PROVEN).
pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PIGMENT","surface":"canvas_ops","version":1,"canvas_resize":"anchor STU-RAS-141 1..9; offset = floor((W'-W) * {0,1/2,1}), floor((H'-H) * {0,1/2,1}); fill outside the old plane; content beyond the new canvas dropped","crop":"rect in document coordinates; angle 0 = exact integer copy, expand reads transparent, content_aware_fill_on_expand = native content_aware::fill of the exposed pixels (cropped opaque bounds mask alpha < 1, crop pixel composited source-over the fill; keep-pixels: canvas outside the moved plane, plane grows to the union, sampling the moved plane) sampling the rest of the result (crop_content_aware takes the fill options and returns the receipt), delete_cropped_pixels false keeps the moved plane; angle -360..360 (clockwise, y down) = transform_math layer transform d = R(-angle)(s - c) + (w/2, h/2), edge transparent, caller filter (default bicubic); rotated keep-pixels refused crop_rotate_keep_pixels_unsupported","trim":"basis transparent_pixels=0 (alpha 0) | top_left_pixel=1 | bottom_right_pixel=9 (exact four-component match); selected edges move to the content bounds; all trimmed refused trim_everything","class":"exact for integer copies, approximate for rotated crops","parity":"NOT_PROVEN (Photoshop centre rounding, crop angle sign, trim match rule UNVERIFIED, D5)"}"#;
