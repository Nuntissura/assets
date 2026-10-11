//! Checked pixel geometry shared by the executor modules (workflow v4 V4-01): every integer
//! operation on a plan-derived size or coordinate is checked or saturating and ends in the typed
//! `BackendError::Overflow`, never in a wrapped value or a panic.
#![deny(clippy::arithmetic_side_effects)]
use crate::plan_sink::RenderError;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::render_plan::BackendError;

pub(crate) const fn overflow() -> RenderError {
    RenderError::Backend(BackendError::Overflow)
}

/// Exclusive end corner of `rect`.
pub(crate) fn end(rect: Rect) -> Result<(i64, i64), RenderError> {
    rect.end().map_err(|_| overflow())
}

pub(crate) fn intersect(a: Rect, b: Rect) -> Result<Option<Rect>, RenderError> {
    a.intersect(b).map_err(|_| overflow())
}

/// Smallest rectangle holding both (a missing side is the other one).
pub(crate) fn union(a: Option<Rect>, b: Option<Rect>) -> Result<Option<Rect>, RenderError> {
    let (Some(a), Some(b)) = (a, b) else {
        return Ok(a.or(b));
    };
    let (ax1, ay1) = end(a)?;
    let (bx1, by1) = end(b)?;
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    let span = |far: i64, near: i64| {
        far.checked_sub(near)
            .and_then(|d| u32::try_from(d).ok())
            .ok_or_else(overflow)
    };
    Ok(Some(Rect {
        x,
        y,
        width: span(ax1.max(bx1), x)?,
        height: span(ay1.max(by1), y)?,
    }))
}

/// `value - origin` as an offset; `Overflow` when negative or out of range.
pub(crate) fn offset(value: i64, origin: i64) -> Result<usize, RenderError> {
    value
        .checked_sub(origin)
        .and_then(|d| usize::try_from(d).ok())
        .ok_or_else(overflow)
}

pub(crate) fn width_of(rect: Rect) -> Result<usize, RenderError> {
    usize::try_from(rect.width).map_err(|_| overflow())
}

pub(crate) fn height_of(rect: Rect) -> Result<usize, RenderError> {
    usize::try_from(rect.height).map_err(|_| overflow())
}

/// Pixels in `rect`.
pub(crate) fn area(rect: Rect) -> Result<usize, RenderError> {
    width_of(rect)?
        .checked_mul(height_of(rect)?)
        .ok_or_else(overflow)
}

/// Row-major index of plan pixel `(x, y)` in a buffer laid out over `rect`.
pub(crate) fn index_in(rect: Rect, x: i64, y: i64) -> Result<usize, RenderError> {
    let column = offset(x, rect.x)?;
    offset(y, rect.y)?
        .checked_mul(width_of(rect)?)
        .and_then(|row| row.checked_add(column))
        .ok_or_else(overflow)
}

pub(crate) fn add_u(a: usize, b: usize) -> Result<usize, RenderError> {
    a.checked_add(b).ok_or_else(overflow)
}

pub(crate) fn mul_u(a: usize, b: usize) -> Result<usize, RenderError> {
    a.checked_mul(b).ok_or_else(overflow)
}

pub(crate) fn to_i64(value: usize) -> Result<i64, RenderError> {
    i64::try_from(value).map_err(|_| overflow())
}

pub(crate) fn to_usize(value: u64) -> Result<usize, RenderError> {
    usize::try_from(value).map_err(|_| overflow())
}
