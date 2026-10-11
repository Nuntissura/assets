//! Owned, serializable path geometry: subpaths of move/line/quad/cubic/close with `f64` coordinates
//! and a fill rule. Plain data for renderers and interchange (CX-CAP-001: nib owns vector
//! geometry); it is independent of the borrowed document `Path` and of the Boolean provider.
//!
//! Coordinates are unit-agnostic numbers; the consumer fixes the unit (render backends use plan
//! pixels). Conversion from anchors takes the target unit explicitly. Serialization is a canonical
//! JSON text (`to_canonical_json`) with a content hash, so it needs no serde dependency.
use crate::path_contract::{Anchor, Error, Point, Winding};
use hsk_studio_accord::Unit;
use kurbo::{BezPath, PathEl};
use sha2::{Digest, Sha256};

/// Magnitude cap so bounds, samples and integer pixel boxes stay exact in binary64.
pub const COORDINATE_LIMIT: f64 = 1.0e9;
/// Default element budget of [`PathLimits`].
pub const MAX_PATH_ELEMENTS: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}
impl FillRule {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::NonZero => "nonzero",
            Self::EvenOdd => "evenodd",
        }
    }
    pub fn parse(text: &str) -> Result<Self, Error> {
        match text {
            "nonzero" => Ok(Self::NonZero),
            "evenodd" => Ok(Self::EvenOdd),
            _ => Err(Error::InvalidEncoding),
        }
    }
}
impl TryFrom<Winding> for FillRule {
    type Error = Error;
    /// `Winding::None` (no fill) has no fill rule.
    fn try_from(winding: Winding) -> Result<Self, Error> {
        match winding {
            Winding::NonZero => Ok(Self::NonZero),
            Winding::EvenOdd => Ok(Self::EvenOdd),
            Winding::None => Err(Error::InvalidGeometry),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PathPoint {
    pub x: f64,
    pub y: f64,
}
impl PathPoint {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// One path element; control points precede the end point as in SVG/PDF.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathElement {
    MoveTo(PathPoint),
    LineTo(PathPoint),
    QuadTo(PathPoint, PathPoint),
    CubicTo(PathPoint, PathPoint, PathPoint),
    /// Straight segment back to the subpath start; the next element must be `MoveTo`.
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathLimits {
    pub max_elements: usize,
    /// Every subpath must end with `Close` (rules for closed shapes; fills close implicitly).
    pub require_closed: bool,
}
impl Default for PathLimits {
    fn default() -> Self {
        Self {
            max_elements: MAX_PATH_ELEMENTS,
            require_closed: false,
        }
    }
}
impl PathLimits {
    /// Largest canonical JSON text accepted by `from_canonical_json`.
    pub fn max_input_bytes(&self) -> usize {
        self.max_elements.saturating_mul(160).saturating_add(256)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OwnedPath {
    pub elements: Vec<PathElement>,
    pub fill_rule: FillRule,
}

fn finite(p: PathPoint) -> bool {
    p.x.is_finite()
        && p.y.is_finite()
        && p.x.abs() <= COORDINATE_LIMIT
        && p.y.abs() <= COORDINATE_LIMIT
}
fn normalise(v: f64) -> f64 {
    if v == 0.0 { 0.0 } else { v }
}
fn kpt(p: PathPoint) -> kurbo::Point {
    kurbo::Point::new(p.x, p.y)
}
fn from_k(p: kurbo::Point) -> PathPoint {
    PathPoint::new(p.x, p.y)
}

impl OwnedPath {
    pub fn new(fill_rule: FillRule) -> Self {
        Self {
            elements: Vec::new(),
            fill_rule,
        }
    }

    /// Structural rules, all checked before use: coordinates finite and within
    /// [`COORDINATE_LIMIT`]; at most `max_elements` elements; the path starts with `MoveTo`;
    /// drawing elements only follow a `MoveTo` (never directly after `Close`); `Close` needs at
    /// least one drawing element in its subpath; with `require_closed` every subpath ends in
    /// `Close`. An empty path is valid (draws nothing).
    pub fn validate(&self, limits: &PathLimits) -> Result<(), Error> {
        if self.elements.len() > limits.max_elements {
            return Err(Error::BudgetExceeded);
        }
        // 0 = no open subpath (start or just closed), 1 = MoveTo seen, 2 = drawn since MoveTo.
        let mut state = 0_u8;
        for element in &self.elements {
            let ok = match *element {
                PathElement::MoveTo(p) => {
                    if limits.require_closed && state != 0 {
                        return Err(Error::InvalidGeometry);
                    }
                    state = 1;
                    finite(p)
                }
                PathElement::LineTo(p) => {
                    draw(&mut state)?;
                    finite(p)
                }
                PathElement::QuadTo(a, b) => {
                    draw(&mut state)?;
                    finite(a) && finite(b)
                }
                PathElement::CubicTo(a, b, c) => {
                    draw(&mut state)?;
                    finite(a) && finite(b) && finite(c)
                }
                PathElement::Close => {
                    if state != 2 {
                        return Err(Error::InvalidGeometry);
                    }
                    state = 0;
                    true
                }
            };
            if !ok {
                return Err(Error::InvalidGeometry);
            }
        }
        if limits.require_closed && state != 0 {
            return Err(Error::InvalidGeometry);
        }
        Ok(())
    }

    /// True when the path has elements and every subpath ends with `Close`.
    pub fn is_closed(&self) -> bool {
        let mut drawn = false;
        for element in &self.elements {
            match element {
                PathElement::MoveTo(_) if drawn => return false,
                PathElement::MoveTo(_) => {}
                PathElement::Close => drawn = false,
                _ => drawn = true,
            }
        }
        !self.elements.is_empty() && !drawn
    }

    /// Lossless conversion to a kurbo path (one element each). The path is validated with the
    /// default limits first, because kurbo debug-asserts on paths that do not begin with `MoveTo`.
    pub fn to_kurbo(&self) -> Result<BezPath, Error> {
        self.validate(&PathLimits::default())?;
        let mut path = BezPath::new();
        for element in &self.elements {
            match *element {
                PathElement::MoveTo(p) => path.move_to(kpt(p)),
                PathElement::LineTo(p) => path.line_to(kpt(p)),
                PathElement::QuadTo(a, b) => path.quad_to(kpt(a), kpt(b)),
                PathElement::CubicTo(a, b, c) => path.curve_to(kpt(a), kpt(b), kpt(c)),
                PathElement::Close => path.close_path(),
            }
        }
        Ok(path)
    }

    /// Lossless conversion from a kurbo path; the result is validated with `limits`.
    pub fn from_kurbo(
        path: &BezPath,
        fill_rule: FillRule,
        limits: &PathLimits,
    ) -> Result<Self, Error> {
        if path.elements().len() > limits.max_elements {
            return Err(Error::BudgetExceeded);
        }
        let elements = path
            .elements()
            .iter()
            .map(|el| match *el {
                PathEl::MoveTo(p) => PathElement::MoveTo(from_k(p)),
                PathEl::LineTo(p) => PathElement::LineTo(from_k(p)),
                PathEl::QuadTo(a, b) => PathElement::QuadTo(from_k(a), from_k(b)),
                PathEl::CurveTo(a, b, c) => PathElement::CubicTo(from_k(a), from_k(b), from_k(c)),
                PathEl::ClosePath => PathElement::Close,
            })
            .collect();
        let result = Self {
            elements,
            fill_rule,
        };
        result.validate(limits)?;
        Ok(result)
    }

    /// Anchor path to owned geometry in `unit`. Handles are vectors relative to their anchor (nib
    /// contract): the outgoing handle of anchor A and the incoming handle of anchor B bound the
    /// cubic A -> B; both zero is a straight segment. With `closed` the last anchor connects to
    /// the first through its handles and the path ends in `Close`; otherwise the path is left open
    /// (a fill closes it implicitly with a straight segment).
    pub fn from_anchors(
        anchors: &[Anchor],
        closed: bool,
        winding: Winding,
        unit: Unit,
        pixels_per_inch: Option<f64>,
        limits: &PathLimits,
    ) -> Result<Self, Error> {
        let fill_rule = FillRule::try_from(winding)?;
        if anchors.len() < 2 {
            return Err(Error::InvalidGeometry);
        }
        if anchors.len() >= limits.max_elements {
            return Err(Error::BudgetExceeded);
        }
        let to = |p: Point| -> Result<PathPoint, Error> {
            let convert = |l: hsk_studio_accord::Length| {
                l.convert(unit, pixels_per_inch)
                    .map(|c| normalise(c.value()))
                    .map_err(|_| Error::InvalidGeometry)
            };
            Ok(PathPoint::new(convert(p.x)?, convert(p.y)?))
        };
        let mut path = Self::new(fill_rule);
        path.elements
            .push(PathElement::MoveTo(to(anchors[0].position)?));
        let segments = if closed {
            anchors.len()
        } else {
            anchors.len() - 1
        };
        for i in 0..segments {
            let a = &anchors[i];
            let b = &anchors[(i + 1) % anchors.len()];
            let (pa, pb) = (to(a.position)?, to(b.position)?);
            let (out, inc) = (to(a.outgoing)?, to(b.incoming)?);
            if out.x == 0.0 && out.y == 0.0 && inc.x == 0.0 && inc.y == 0.0 {
                // The straight closing segment is the implicit one of `Close`.
                if !(closed && i + 1 == segments) {
                    path.elements.push(PathElement::LineTo(pb));
                }
            } else {
                path.elements.push(PathElement::CubicTo(
                    PathPoint::new(pa.x + out.x, pa.y + out.y),
                    PathPoint::new(pb.x + inc.x, pb.y + inc.y),
                    pb,
                ));
            }
        }
        if closed {
            path.elements.push(PathElement::Close);
        }
        path.validate(limits)?;
        Ok(path)
    }

    /// Deterministic JSON text: `{"fill_rule":"nonzero","elements":[["M",x,y],["L",x,y],
    /// ["Q",x1,y1,x,y],["C",x1,y1,x2,y2,x,y],["Z"]]}`. Numbers use the shortest round-trip decimal
    /// form; negative zero is written `0`. Validate first: non-finite values are not encodable.
    pub fn to_canonical_json(&self) -> String {
        let mut out = String::with_capacity(48 + self.elements.len() * 40);
        out.push_str("{\"fill_rule\":\"");
        out.push_str(self.fill_rule.wire());
        out.push_str("\",\"elements\":[");
        for (i, element) in self.elements.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let (verb, points): (&str, &[PathPoint]) = match element {
                PathElement::MoveTo(p) => ("M", std::slice::from_ref(p)),
                PathElement::LineTo(p) => ("L", std::slice::from_ref(p)),
                PathElement::QuadTo(a, b) => ("Q", &[*a, *b]),
                PathElement::CubicTo(a, b, c) => ("C", &[*a, *b, *c]),
                PathElement::Close => ("Z", &[]),
            };
            write_element(&mut out, verb, points);
        }
        out.push_str("]}");
        out
    }

    /// SHA-256 of the canonical JSON text; stable identity of the geometry and fill rule.
    pub fn content_sha256(&self) -> [u8; 32] {
        Sha256::digest(self.to_canonical_json().as_bytes()).into()
    }

    /// Strict inverse of [`OwnedPath::to_canonical_json`]: both fields required once, no unknown
    /// fields, bounded input, finite numbers, then [`OwnedPath::validate`].
    pub fn from_canonical_json(text: &str, limits: &PathLimits) -> Result<Self, Error> {
        if text.len() > limits.max_input_bytes() {
            return Err(Error::BudgetExceeded);
        }
        let mut p = Cursor {
            bytes: text.as_bytes(),
            at: 0,
        };
        let (mut fill_rule, mut elements) = (None, None);
        p.expect(b'{')?;
        loop {
            let key = p.string()?;
            p.expect(b':')?;
            match key {
                "fill_rule" if fill_rule.is_none() => {
                    fill_rule = Some(FillRule::parse(p.string()?)?)
                }
                "elements" if elements.is_none() => elements = Some(p.elements(limits)?),
                _ => return Err(Error::InvalidEncoding),
            }
            if p.eat(b',') {
                continue;
            }
            p.expect(b'}')?;
            break;
        }
        p.space();
        if p.at != p.bytes.len() {
            return Err(Error::InvalidEncoding);
        }
        let path = Self {
            fill_rule: fill_rule.ok_or(Error::MissingField)?,
            elements: elements.ok_or(Error::MissingField)?,
        };
        path.validate(limits)?;
        Ok(path)
    }
}

fn draw(state: &mut u8) -> Result<(), Error> {
    if *state == 0 {
        return Err(Error::InvalidGeometry);
    }
    *state = 2;
    Ok(())
}

fn write_element(out: &mut String, verb: &str, points: &[PathPoint]) {
    out.push_str("[\"");
    out.push_str(verb);
    out.push('"');
    for p in points {
        out.push(',');
        out.push_str(&normalise(p.x).to_string());
        out.push(',');
        out.push_str(&normalise(p.y).to_string());
    }
    out.push(']');
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Cursor<'a> {
    fn space(&mut self) {
        while matches!(self.bytes.get(self.at), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.at += 1;
        }
    }
    fn eat(&mut self, byte: u8) -> bool {
        self.space();
        if self.bytes.get(self.at) == Some(&byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, byte: u8) -> Result<(), Error> {
        if self.eat(byte) {
            Ok(())
        } else {
            Err(Error::InvalidEncoding)
        }
    }
    /// A string without escapes (all canonical strings are plain ASCII words).
    fn string(&mut self) -> Result<&'a str, Error> {
        self.expect(b'"')?;
        let start = self.at;
        while let Some(b) = self.bytes.get(self.at) {
            match b {
                b'"' => {
                    let s = std::str::from_utf8(&self.bytes[start..self.at])
                        .map_err(|_| Error::InvalidEncoding)?;
                    self.at += 1;
                    return Ok(s);
                }
                b'\\' => return Err(Error::InvalidEncoding),
                _ => self.at += 1,
            }
        }
        Err(Error::InvalidEncoding)
    }
    fn number(&mut self) -> Result<f64, Error> {
        self.space();
        let start = self.at;
        while matches!(
            self.bytes.get(self.at),
            Some(b'0'..=b'9' | b'+' | b'-' | b'.' | b'e' | b'E')
        ) {
            self.at += 1;
        }
        let value: f64 = std::str::from_utf8(&self.bytes[start..self.at])
            .map_err(|_| Error::InvalidEncoding)?
            .parse()
            .map_err(|_| Error::InvalidEncoding)?;
        if value.is_finite() {
            Ok(normalise(value))
        } else {
            Err(Error::InvalidGeometry)
        }
    }
    fn point(&mut self) -> Result<PathPoint, Error> {
        self.expect(b',')?;
        let x = self.number()?;
        self.expect(b',')?;
        Ok(PathPoint::new(x, self.number()?))
    }
    fn elements(&mut self, limits: &PathLimits) -> Result<Vec<PathElement>, Error> {
        self.expect(b'[')?;
        let mut out = Vec::new();
        if self.eat(b']') {
            return Ok(out);
        }
        loop {
            if out.len() >= limits.max_elements {
                return Err(Error::BudgetExceeded);
            }
            self.expect(b'[')?;
            out.push(match self.string()? {
                "M" => PathElement::MoveTo(self.point()?),
                "L" => PathElement::LineTo(self.point()?),
                "Q" => PathElement::QuadTo(self.point()?, self.point()?),
                "C" => PathElement::CubicTo(self.point()?, self.point()?, self.point()?),
                "Z" => PathElement::Close,
                _ => return Err(Error::InvalidEncoding),
            });
            self.expect(b']')?;
            if self.eat(b',') {
                continue;
            }
            self.expect(b']')?;
            return Ok(out);
        }
    }
}
