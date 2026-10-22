use crate::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GeometryError {
    NonFinite,
    Negative,
    NonPositive,
    ArithmeticOverflow,
    InvalidPlan(&'static str),
    MissingContext,
    Infeasible,
    OutsideViewport,
}
impl From<GeometryError> for Error {
    fn from(value: GeometryError) -> Self {
        Self::Geometry(value)
    }
}

/// Finite logical DIP number; canonical positive zero makes complete-envelope equality reflexive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scalar(f64);
#[cfg(feature = "serde")]
impl serde::Serialize for Scalar {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f64(self.get())
    }
}
#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Scalar {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = <f64 as serde::Deserialize>::deserialize(d)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}
impl Eq for Scalar {}
impl Scalar {
    pub fn new(value: f64) -> Result<Self, Error> {
        if !value.is_finite() {
            return Err(GeometryError::NonFinite.into());
        }
        Ok(Self(if value == 0.0 { 0.0 } else { value }))
    }
    pub fn get(self) -> f64 {
        self.0
    }
}
pub(crate) fn finite(value: f64) -> Result<f64, Error> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(GeometryError::ArithmeticOverflow.into())
    }
}
pub(crate) fn nonnegative(value: f64) -> Result<f64, Error> {
    finite(value)?;
    if value < 0.0 {
        Err(GeometryError::Negative.into())
    } else {
        Ok(value)
    }
}
pub(crate) fn positive(value: f64) -> Result<f64, Error> {
    finite(value)?;
    if value <= 0.0 {
        Err(GeometryError::NonPositive.into())
    } else {
        Ok(value)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Size {
    width: Scalar,
    height: Scalar,
}
#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Size {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            width: Scalar,
            height: Scalar,
        }
        let wire = <Wire as serde::Deserialize>::deserialize(d)?;
        Self::new(wire.width.get(), wire.height.get()).map_err(serde::de::Error::custom)
    }
}
impl Size {
    pub fn new(width: f64, height: f64) -> Result<Self, Error> {
        nonnegative(width)?;
        nonnegative(height)?;
        Ok(Self {
            width: Scalar::new(width)?,
            height: Scalar::new(height)?,
        })
    }
    pub fn width(self) -> f64 {
        self.width.get()
    }
    pub fn height(self) -> f64 {
        self.height.get()
    }
    pub(crate) fn zero() -> Self {
        Self::new(0.0, 0.0).unwrap()
    }
    pub(crate) fn max(self, other: Self) -> Result<Self, Error> {
        Self::new(
            self.width().max(other.width()),
            self.height().max(other.height()),
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Rect {
    x: Scalar,
    y: Scalar,
    size: Size,
}
#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Rect {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            x: Scalar,
            y: Scalar,
            size: Size,
        }
        let wire = <Wire as serde::Deserialize>::deserialize(d)?;
        Self::new(
            wire.x.get(),
            wire.y.get(),
            wire.size.width(),
            wire.size.height(),
        )
        .map_err(serde::de::Error::custom)
    }
}
impl Rect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Result<Self, Error> {
        let x = Scalar::new(x)?;
        let y = Scalar::new(y)?;
        let size = Size::new(width, height)?;
        finite(x.get() + width)?;
        finite(y.get() + height)?;
        Ok(Self { x, y, size })
    }
    pub fn x(self) -> f64 {
        self.x.get()
    }
    pub fn y(self) -> f64 {
        self.y.get()
    }
    pub fn width(self) -> f64 {
        self.size.width()
    }
    pub fn height(self) -> f64 {
        self.size.height()
    }
    pub fn right(self) -> f64 {
        self.x() + self.width()
    }
    pub fn bottom(self) -> f64 {
        self.y() + self.height()
    }
    pub fn size(self) -> Size {
        self.size
    }
    pub fn contains(self, other: Self) -> bool {
        other.x() >= self.x()
            && other.y() >= self.y()
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }
    pub(crate) fn clip(self, other: Self) -> Result<Self, Error> {
        let x = self.x().max(other.x());
        let y = self.y().max(other.y());
        Self::new(
            x,
            y,
            (self.right().min(other.right()) - x).max(0.0),
            (self.bottom().min(other.bottom()) - y).max(0.0),
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub enum Axis {
    Horizontal,
    Vertical,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
pub enum Overflow {
    Reject,
    Scrollable,
}

/// Minima first, then distribute only spare space by positive normalized weights.
pub(crate) fn tracks(
    minima: &[f64],
    weights: &[Scalar],
    extent: f64,
    gap: f64,
) -> Result<Vec<(f64, f64)>, Error> {
    if minima.len() != weights.len() {
        return Err(GeometryError::InvalidPlan("track length mismatch").into());
    }
    if minima.is_empty() {
        return Ok(vec![]);
    }
    let gap_total = finite(gap * minima.len().saturating_sub(1) as f64)?;
    let minimum_total = minima
        .iter()
        .try_fold(gap_total, |sum, m| finite(sum + m))?;
    if extent < minimum_total {
        return Err(GeometryError::Infeasible.into());
    }
    let max_weight = weights.iter().try_fold(0.0_f64, |max, w| {
        Ok::<_, Error>(max.max(positive(w.get())?))
    })?;
    let normalized: Vec<f64> = weights.iter().map(|w| w.get() / max_weight).collect();
    let sum = normalized.iter().try_fold(0.0, |a, b| finite(a + b))?;
    let spare = extent - minimum_total;
    let mut result = Vec::with_capacity(minima.len());
    let mut offset = 0.0;
    for i in 0..minima.len() {
        let length = if i + 1 == minima.len() {
            (extent - offset).max(0.0)
        } else {
            finite(minima[i] + spare * (normalized[i] / sum))?
        };
        // A rounding residual may not consume the last track's minimum.
        if length < minima[i] {
            return Err(GeometryError::Infeasible.into());
        }
        result.push((offset, length));
        if i + 1 < minima.len() {
            offset = finite(offset + length + gap)?;
        }
    }
    Ok(result)
}
