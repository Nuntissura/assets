//! DXF group-code reader and the drawing subset the DXF import draws (lane SMB-DXF-EPS-IMPORT,
//! remediation 2026-10-21). Adapted, with attribution in the crate NOTICE, from the MIT-licensed
//! `dxf` crate 0.6.2 (ixmilia/dxf-rs, Copyright (c) IxMilia): the group-code value-type table
//! (`expected_type.rs`), the ASCII and binary code-pair readers (`code_pair_iter.rs`: two-byte
//! codes detected from the first pair, the `255` escape of one-byte codes, one-byte booleans after
//! R13), the string un-escaping (`code_pair_value.rs`: `\U+XXXX` and `^` control escapes), the
//! section, table, block and entity reading order (`drawing.rs`, `block.rs`, `entity.rs`,
//! `entity_iter.rs`: POLYLINE gathers its VERTEX records, an INSERT with attributes its ATTRIB
//! records, each up to an optional SEQEND; LWPOLYLINE code 10 opens a vertex), the field codes and
//! default values of the entity and LAYER records this import reads (`spec/EntitiesSpec.xml`,
//! `spec/TableSpec.xml`, `spec/HeaderVariablesSpec.xml`), and the layer colour sign convention
//! (negative = layer off). Not adapted: thumbnails (the `image` dependency), objects, classes,
//! writing, DXB and every entity this import does not draw (those are kept as kinds without fields
//! so the import can report them as typed losses).
//!
//! Differences from the `dxf` crate, all toward fail-closed: an unknown group code, a number that
//! does not parse, a truncated pair or a section or entity cut off by the end of the input is
//! `MalformedDxf`; an entity type this subset does not model is kept (as [`Kind::Other`] and
//! friends) instead of being skipped silently, so the import reports it; a DIMENSION without a
//! subclass marker is kept as a dimension.
//!
//! Hostile input: the reader never allocates more than the input holds plus the value budget
//! (vertices, knots, weights, control and fit points summed over the drawing,
//! `BudgetExceeded("dxf_values")`), checks the cancellation token every 4096 pairs, and uses
//! checked or saturating integer arithmetic only (proof: `dxf_import::tests` and
//! `tests/dxf_import.rs` `hostile_dxf_is_bounded_and_typed`, `adapted_reader_rejects_damage`).
#![deny(clippy::arithmetic_side_effects)]
use crate::cad::BINARY_DXF_SENTINEL;
use crate::error::{Result, VectorError};
use hsk_studio_accord::CancellationToken;
use std::f64::consts::TAU;

/// A point or vector `[x, y, z]`.
pub(crate) type P3 = [f64; 3];
const Z_AXIS: P3 = [0.0, 0.0, 1.0];
const ORIGIN: P3 = [0.0, 0.0, 0.0];

/// Value type of a group code (adapted from `dxf` 0.6.2 `ExpectedType::new`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValueType {
    Bool,
    I32,
    I64,
    I16,
    F64,
    Str,
    Binary,
}

pub(crate) fn value_type(code: i32) -> Option<ValueType> {
    use ValueType::*;
    Some(match code {
        0..=9 | 100..=102 | 105 | 300..=309 | 320..=369 | 390..=399 | 410..=419 | 430..=439 | 470..=481 | 999 | 1000..=1009 => Str,
        10..=59 | 110..=149 | 210..=239 | 460..=469 | 1010..=1059 => F64,
        60..=79 | 170..=179 | 270..=289 | 370..=389 | 400..=409 | 1060..=1070 => I16,
        90..=99 | 420..=429 | 440..=459 | 1071 => I32,
        160..=169 => I64,
        290..=299 => Bool,
        310..=319 => Binary,
        _ => return None,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Str(String),
    F64(f64),
    I16(i16),
    I32(i32),
    /// A 64-bit integer (codes 160..=169): validated, not kept (nothing here reads one).
    I64,
    /// Binary chunks are skipped (only their length is read in binary DXF).
    Binary,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Pair {
    pub code: i32,
    pub value: Value,
}

impl Pair {
    fn f64(&self) -> Result<f64> {
        match self.value {
            Value::F64(v) => Ok(v),
            _ => Err(VectorError::MalformedDxf("value_type")),
        }
    }
    fn i16(&self) -> Result<i16> {
        match self.value {
            Value::I16(v) => Ok(v),
            _ => Err(VectorError::MalformedDxf("value_type")),
        }
    }
    fn i32(&self) -> Result<i32> {
        match self.value {
            Value::I32(v) => Ok(v),
            _ => Err(VectorError::MalformedDxf("value_type")),
        }
    }
    fn str(&self) -> Result<&str> {
        match &self.value {
            Value::Str(v) => Ok(v),
            _ => Err(VectorError::MalformedDxf("value_type")),
        }
    }
    fn string(self) -> Result<String> {
        match self.value {
            Value::Str(v) => Ok(v),
            _ => Err(VectorError::MalformedDxf("value_type")),
        }
    }
    fn is_zero(&self, name: &str) -> bool {
        self.code == 0 && matches!(&self.value, Value::Str(s) if s == name)
    }
}

/// Windows-1252 characters for the bytes 0x80..=0x9F (the rest of the code page is Latin-1).
const CP1252_80: [char; 32] = [
    '\u{20AC}', '\u{81}', '\u{201A}', '\u{192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}', '\u{2C6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8D}', '\u{17D}', '\u{8F}',
    '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}', '\u{2DC}', '\u{2122}', '\u{161}', '\u{203A}', '\u{153}', '\u{9D}', '\u{17E}', '\u{178}',
];

/// UTF-8 when the bytes are valid UTF-8 (R2007+ files), else Windows-1252 (the `dxf` default).
fn decode_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|b| if (0x80..0xA0).contains(b) { CP1252_80[usize::from(b.wrapping_sub(0x80))] } else { char::from(*b) }).collect(),
    }
}

/// `\U+XXXX` escapes, then `^` control escapes (`^J` is LF, `^ ` is `^`), as `dxf` 0.6.2 does.
fn unescape(s: String) -> String {
    if !s.contains('^') && !s.contains("\\U+") {
        return s;
    }
    let chars: Vec<char> = s.chars().collect();
    let mut uni = String::with_capacity(s.len());
    let mut i = 0usize;
    while let Some(c) = chars.get(i).copied() {
        let next = i.saturating_add(1);
        if c == '\\'
            && chars.get(next) == Some(&'U')
            && chars.get(next.saturating_add(1)) == Some(&'+')
            && let Some(hex) = chars.get(next.saturating_add(2)..next.saturating_add(6))
        {
            let text: String = hex.iter().collect();
            uni.push(u32::from_str_radix(&text, 16).ok().and_then(char::from_u32).unwrap_or('?'));
            i = next.saturating_add(6);
            continue;
        }
        uni.push(c);
        i = next;
    }
    let mut out = String::with_capacity(uni.len());
    let mut escape = false;
    for c in uni.chars() {
        if escape {
            escape = false;
            out.push(match c {
                '@'..='_' => char::from(u8::try_from(u32::from(c)).unwrap_or(0x40).wrapping_sub(0x40)),
                ' ' => '^',
                other => other,
            });
        } else if c == '^' {
            escape = true;
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_num<T: std::str::FromStr>(line: &[u8]) -> Result<T> {
    std::str::from_utf8(line).ok().and_then(|s| s.trim().parse::<T>().ok()).ok_or(VectorError::MalformedDxf("value"))
}

/// Shorts may be written as doubles (`2.0`); the value saturates into `i16` as in `dxf` 0.6.2.
fn parse_short(line: &[u8]) -> Result<i16> {
    let v: f64 = parse_num(line)?;
    if !v.is_finite() {
        return Err(VectorError::MalformedDxf("value"));
    }
    Ok(v as i16)
}

enum Source<'a> {
    Text { rest: &'a [u8] },
    Binary { rest: &'a [u8], two_byte: bool, detected: bool },
}

fn take<'a>(rest: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
    let all: &'a [u8] = rest;
    if all.len() < n {
        return Err(VectorError::MalformedDxf("truncated"));
    }
    let (head, tail) = all.split_at(n);
    *rest = tail;
    Ok(head)
}

fn take_cstr<'a>(rest: &mut &'a [u8]) -> Result<&'a [u8]> {
    let end = rest.iter().position(|b| *b == 0).ok_or(VectorError::MalformedDxf("truncated"))?;
    let s = take(rest, end)?;
    take(rest, 1)?;
    Ok(s)
}

/// Code-pair stream over ASCII or binary DXF with one pair of put-back; `999` comments skipped.
pub(crate) struct Reader<'a, 'c> {
    source: Source<'a>,
    peeked: Option<Pair>,
    pairs: usize,
    values: usize,
    value_cap: usize,
    cancel: &'c CancellationToken,
}

impl<'a, 'c> Reader<'a, 'c> {
    pub(crate) fn new(bytes: &'a [u8], binary: bool, value_cap: usize, cancel: &'c CancellationToken) -> Result<Self> {
        let source = if binary {
            let rest = bytes.strip_prefix(BINARY_DXF_SENTINEL).ok_or(VectorError::MalformedDxf("binary_header"))?;
            Source::Binary { rest, two_byte: false, detected: false }
        } else {
            let rest = bytes.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(bytes);
            Source::Text { rest }
        };
        Ok(Self { source, peeked: None, pairs: 0, values: 0, value_cap, cancel })
    }

    /// Counts one stored vertex, knot, weight or point against the drawing's value budget.
    fn value(&mut self) -> Result<()> {
        self.values = self.values.saturating_add(1);
        if self.values > self.value_cap {
            return Err(VectorError::BudgetExceeded("dxf_values"));
        }
        Ok(())
    }

    fn put_back(&mut self, pair: Pair) {
        self.peeked = Some(pair);
    }

    /// The next pair, `None` at the end of the input.
    pub(crate) fn next(&mut self) -> Result<Option<Pair>> {
        if let Some(p) = self.peeked.take() {
            return Ok(Some(p));
        }
        loop {
            self.pairs = self.pairs.saturating_add(1);
            if self.pairs.is_multiple_of(4096) {
                self.cancel.check().map_err(|_| VectorError::Canceled)?;
            }
            let pair = match &mut self.source {
                Source::Text { rest } => read_text_pair(rest)?,
                Source::Binary { rest, two_byte, detected } => read_binary_pair(rest, two_byte, detected)?,
            };
            match pair {
                Some(Pair { code: 999, .. }) => continue,
                other => return Ok(other),
            }
        }
    }

    /// The next pair, which must exist.
    fn expect(&mut self) -> Result<Pair> {
        self.next()?.ok_or(VectorError::MalformedDxf("truncated"))
    }
}

fn line<'a>(rest: &mut &'a [u8]) -> Option<&'a [u8]> {
    let all: &'a [u8] = rest;
    if all.is_empty() {
        return None;
    }
    let (l, tail) = match all.iter().position(|b| *b == b'\n') {
        Some(at) => (&all[..at], &all[at.saturating_add(1)..]),
        None => (all, &all[all.len()..]),
    };
    *rest = tail;
    Some(l.strip_suffix(b"\r").unwrap_or(l))
}

fn read_text_pair(rest: &mut &[u8]) -> Result<Option<Pair>> {
    let Some(code_line) = line(rest) else { return Ok(None) };
    let code_text = code_line.trim_ascii();
    if code_text.is_empty() {
        // A trailing blank line ends the stream (as in `dxf` 0.6.2).
        return Ok(None);
    }
    let code: i32 = std::str::from_utf8(code_text).ok().and_then(|s| s.parse().ok()).ok_or(VectorError::MalformedDxf("group_code"))?;
    let ty = value_type(code).ok_or(VectorError::MalformedDxf("group_code"))?;
    let value_line = line(rest).ok_or(VectorError::MalformedDxf("truncated"))?;
    let value = match ty {
        ValueType::Str => Value::Str(unescape(decode_text(value_line))),
        ValueType::F64 => Value::F64(parse_num(value_line)?),
        ValueType::I16 | ValueType::Bool => Value::I16(parse_short(value_line)?),
        ValueType::I32 => Value::I32(parse_num(value_line)?),
        ValueType::I64 => {
            parse_num::<i64>(value_line)?;
            Value::I64
        }
        ValueType::Binary => Value::Binary,
    };
    Ok(Some(Pair { code, value }))
}

fn read_binary_pair(rest: &mut &[u8], two_byte: &mut bool, detected: &mut bool) -> Result<Option<Pair>> {
    let Some((&lo, _)) = rest.split_first() else { return Ok(None) };
    take(rest, 1)?;
    let code = if *two_byte {
        let hi = take(rest, 1)?[0];
        i32::from(u16::from_le_bytes([lo, hi]))
    } else if lo == 255 {
        let b = take(rest, 2)?;
        i32::from(i16::from_le_bytes([b[0], b[1]]))
    } else {
        i32::from(lo)
    };
    let ty = value_type(code).ok_or(VectorError::MalformedDxf("group_code"))?;
    let value = match ty {
        ValueType::Str => {
            let mut s = take_cstr(rest)?;
            if !*detected && code == 0 && s.is_empty() {
                // R13+ binary: the empty "string" was the high byte of the two-byte code 0.
                *two_byte = true;
                s = take_cstr(rest)?;
            }
            Value::Str(unescape(decode_text(s)))
        }
        ValueType::Bool => {
            if *two_byte {
                Value::I16(i16::from(take(rest, 1)?[0]))
            } else {
                let b = take(rest, 2)?;
                Value::I16(i16::from_le_bytes([b[0], b[1]]))
            }
        }
        ValueType::I16 => {
            let b = take(rest, 2)?;
            Value::I16(i16::from_le_bytes([b[0], b[1]]))
        }
        ValueType::I32 => {
            let b = take(rest, 4)?;
            Value::I32(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        }
        ValueType::I64 => {
            take(rest, 8)?;
            Value::I64
        }
        ValueType::F64 => {
            let b = take(rest, 8)?;
            Value::F64(f64::from_le_bytes(b.try_into().map_err(|_| VectorError::MalformedDxf("truncated"))?))
        }
        ValueType::Binary => {
            let n = take(rest, 1)?[0];
            take(rest, usize::from(n))?;
            Value::Binary
        }
    };
    *detected = true;
    Ok(Some(Pair { code, value }))
}

/// ACI colour field (`dxf` 0.6.2 `Color`): 0 BYBLOCK, 256 BYLAYER, 1..=255 an index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Colour(pub i16);

impl Colour {
    pub(crate) fn is_by_layer(self) -> bool {
        self.0 == 256
    }
    pub(crate) fn is_by_block(self) -> bool {
        self.0 == 0
    }
    pub(crate) fn index(self) -> Option<u8> {
        u8::try_from(self.0).ok().filter(|v| *v >= 1)
    }
}

/// Fields every entity has (defaults as `dxf` 0.6.2 `EntityCommon`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Common {
    pub layer: String,
    pub line_type: String,
    pub colour: Colour,
    /// Code 370 in hundredths of a millimetre; -1 BYLAYER, -2 BYBLOCK, 0 when absent.
    pub lineweight: i16,
    /// Code 420, 0x00RRGGBB; 0 when absent.
    pub true_colour: i32,
    pub visible: bool,
    pub paper_space: bool,
}

impl Default for Common {
    fn default() -> Self {
        Self { layer: "0".into(), line_type: "BYLAYER".into(), colour: Colour(256), lineweight: 0, true_colour: 0, visible: true, paper_space: false }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct LwVertex {
    pub x: f64,
    pub y: f64,
    pub start_width: f64,
    pub end_width: f64,
    pub bulge: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Vertex {
    pub location: P3,
    pub start_width: f64,
    pub end_width: f64,
    pub bulge: f64,
    pub flags: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Kind {
    Line { p1: P3, p2: P3 },
    Circle { centre: P3, radius: f64, normal: P3 },
    /// Angles in degrees; `end` defaults to 360.
    Arc { centre: P3, radius: f64, normal: P3, start: f64, end: f64 },
    /// Parameters in radians; `end` defaults to 2 pi.
    Ellipse { centre: P3, major: P3, normal: P3, ratio: f64, start: f64, end: f64 },
    LwPolyline { flags: i32, constant_width: f64, vertices: Vec<LwVertex>, extrusion: P3 },
    Polyline { flags: i32, start_width: f64, end_width: f64, normal: P3, vertices: Vec<Vertex> },
    Spline { normal: P3, flags: i32, degree: i32, knots: Vec<f64>, weights: Vec<f64>, control: Vec<P3>, fit: Vec<P3> },
    Solid { corners: [P3; 4], extrusion: P3 },
    Trace { corners: [P3; 4], extrusion: P3 },
    Insert { name: String, location: P3, x_scale: f64, y_scale: f64, rotation: f64, columns: i16, rows: i16, column_spacing: f64, row_spacing: f64, extrusion: P3, has_attributes: bool, attributes: usize },
    Point,
    /// TEXT, MTEXT, ATTDEF.
    Text,
    /// ATTRIB (gathered by its INSERT when the insert says it has attributes).
    Attrib,
    Dimension,
    Leader,
    /// IMAGE, WIPEOUT.
    Image,
    Vertex(Vertex),
    Seqend,
    /// Any other entity type (hatches, 3D faces, solids, regions, proxies, ...).
    Other,
}

impl Kind {
    fn for_type(name: &str) -> Self {
        match name {
            "LINE" | "3DLINE" => Self::Line { p1: ORIGIN, p2: ORIGIN },
            "CIRCLE" => Self::Circle { centre: ORIGIN, radius: 0.0, normal: Z_AXIS },
            "ARC" => Self::Arc { centre: ORIGIN, radius: 0.0, normal: Z_AXIS, start: 0.0, end: 360.0 },
            "ELLIPSE" => Self::Ellipse { centre: ORIGIN, major: [1.0, 0.0, 0.0], normal: Z_AXIS, ratio: 1.0, start: 0.0, end: TAU },
            "LWPOLYLINE" => Self::LwPolyline { flags: 0, constant_width: 0.0, vertices: Vec::new(), extrusion: Z_AXIS },
            "POLYLINE" => Self::Polyline { flags: 0, start_width: 0.0, end_width: 0.0, normal: Z_AXIS, vertices: Vec::new() },
            "VERTEX" => Self::Vertex(Vertex::default()),
            "SEQEND" => Self::Seqend,
            "SPLINE" => Self::Spline { normal: Z_AXIS, flags: 0, degree: 1, knots: Vec::new(), weights: Vec::new(), control: Vec::new(), fit: Vec::new() },
            "SOLID" => Self::Solid { corners: [ORIGIN; 4], extrusion: Z_AXIS },
            "TRACE" => Self::Trace { corners: [ORIGIN; 4], extrusion: Z_AXIS },
            "INSERT" => Self::Insert {
                name: String::new(),
                location: ORIGIN,
                x_scale: 1.0,
                y_scale: 1.0,
                rotation: 0.0,
                columns: 1,
                rows: 1,
                column_spacing: 0.0,
                row_spacing: 0.0,
                extrusion: Z_AXIS,
                has_attributes: false,
                attributes: 0,
            },
            "POINT" => Self::Point,
            "TEXT" | "MTEXT" | "ATTDEF" => Self::Text,
            "ATTRIB" => Self::Attrib,
            "DIMENSION" => Self::Dimension,
            "LEADER" => Self::Leader,
            "IMAGE" | "WIPEOUT" => Self::Image,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entity {
    pub common: Common,
    pub kind: Kind,
}

/// One LAYER table record (`dxf` 0.6.2 `Layer`: colour default 7, linetype CONTINUOUS).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Layer {
    pub name: String,
    /// Absolute value of code 62.
    pub colour: Colour,
    /// Code 62 was not negative.
    pub on: bool,
    pub line_type: String,
    pub line_weight: i16,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Block {
    pub name: String,
    pub base: P3,
    pub entities: Vec<Entity>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Drawing {
    /// `$INSUNITS` (0 unitless when absent).
    pub insunits: i16,
    /// `$MEASUREMENT` (0 English when absent, 1 metric).
    pub measurement: i16,
    pub layers: Vec<Layer>,
    pub blocks: Vec<Block>,
    pub entities: Vec<Entity>,
}

fn set3(p: &mut P3, axis: usize, v: f64) {
    if let Some(slot) = p.get_mut(axis) {
        *slot = v;
    }
}

/// Axis of a point code: `10 + 10 * axis + slot` (axis 0..=2) for the first coordinate codes.
fn axis_of(code: i32, base: i32) -> Option<usize> {
    let d = code.checked_sub(base)?;
    if d >= 0 && d.checked_rem(10) == Some(0) { usize::try_from(d.checked_div(10)?).ok().filter(|a| *a <= 2) } else { None }
}

/// The record a continuation code (y, width, bulge) applies to; none yet is a damaged file.
fn last<'v, T>(v: &'v mut [T], what: &'static str) -> Result<&'v mut T> {
    v.last_mut().ok_or(VectorError::MalformedDxf(what))
}

/// Applies one pair to the entity: entity-specific codes first, then the common ones.
fn apply(e: &mut Entity, pair: Pair, r: &mut Reader<'_, '_>) -> Result<()> {
    let code = pair.code;
    let handled = match &mut e.kind {
        Kind::Line { p1, p2 } => {
            if let Some(a) = axis_of(code, 10) {
                set3(p1, a, pair.f64()?);
                true
            } else if let Some(a) = axis_of(code, 11) {
                set3(p2, a, pair.f64()?);
                true
            } else {
                false
            }
        }
        Kind::Circle { centre, radius, normal } => {
            if let Some(a) = axis_of(code, 10) {
                set3(centre, a, pair.f64()?);
                true
            } else if let Some(a) = axis_of(code, 210) {
                set3(normal, a, pair.f64()?);
                true
            } else if code == 40 {
                *radius = pair.f64()?;
                true
            } else {
                false
            }
        }
        Kind::Arc { centre, radius, normal, start, end } => {
            if let Some(a) = axis_of(code, 10) {
                set3(centre, a, pair.f64()?);
                true
            } else if let Some(a) = axis_of(code, 210) {
                set3(normal, a, pair.f64()?);
                true
            } else {
                match code {
                    40 => *radius = pair.f64()?,
                    50 => *start = pair.f64()?,
                    51 => *end = pair.f64()?,
                    _ => {}
                }
                matches!(code, 40 | 50 | 51)
            }
        }
        Kind::Ellipse { centre, major, normal, ratio, start, end } => {
            if let Some(a) = axis_of(code, 10) {
                set3(centre, a, pair.f64()?);
                true
            } else if let Some(a) = axis_of(code, 11) {
                set3(major, a, pair.f64()?);
                true
            } else if let Some(a) = axis_of(code, 210) {
                set3(normal, a, pair.f64()?);
                true
            } else {
                match code {
                    40 => *ratio = pair.f64()?,
                    41 => *start = pair.f64()?,
                    42 => *end = pair.f64()?,
                    _ => {}
                }
                matches!(code, 40..=42)
            }
        }
        Kind::LwPolyline { flags, constant_width, vertices, extrusion } => {
            match code {
                10 => {
                    r.value()?;
                    vertices.push(LwVertex { x: pair.f64()?, ..LwVertex::default() });
                }
                20 => last(vertices, "lwpolyline_vertex")?.y = pair.f64()?,
                40 => last(vertices, "lwpolyline_vertex")?.start_width = pair.f64()?,
                41 => last(vertices, "lwpolyline_vertex")?.end_width = pair.f64()?,
                42 => last(vertices, "lwpolyline_vertex")?.bulge = pair.f64()?,
                43 => *constant_width = pair.f64()?,
                70 => *flags = i32::from(pair.i16()?),
                210 | 220 | 230 => set3(extrusion, axis_of(code, 210).unwrap_or(0), pair.f64()?),
                _ => {}
            }
            matches!(code, 10 | 20 | 40 | 41 | 42 | 43 | 70 | 210 | 220 | 230 | 39 | 91)
        }
        Kind::Polyline { flags, start_width, end_width, normal, .. } => {
            if let Some(a) = axis_of(code, 210) {
                set3(normal, a, pair.f64()?);
                true
            } else {
                match code {
                    70 => *flags = i32::from(pair.i16()?),
                    40 => *start_width = pair.f64()?,
                    41 => *end_width = pair.f64()?,
                    _ => {}
                }
                matches!(code, 10 | 20 | 30 | 39 | 40 | 41 | 66 | 70..=75)
            }
        }
        Kind::Vertex(v) => {
            if let Some(a) = axis_of(code, 10) {
                set3(&mut v.location, a, pair.f64()?);
                true
            } else {
                match code {
                    40 => v.start_width = pair.f64()?,
                    41 => v.end_width = pair.f64()?,
                    42 => v.bulge = pair.f64()?,
                    70 => v.flags = i32::from(pair.i16()?),
                    _ => {}
                }
                matches!(code, 40..=42 | 50 | 70..=74 | 91)
            }
        }
        Kind::Spline { normal, flags, degree, knots, weights, control, fit } => {
            match code {
                210 | 220 | 230 => set3(normal, axis_of(code, 210).unwrap_or(0), pair.f64()?),
                70 => *flags = i32::from(pair.i16()?),
                71 => *degree = i32::from(pair.i16()?),
                40 => {
                    r.value()?;
                    knots.push(pair.f64()?);
                }
                41 => {
                    r.value()?;
                    weights.push(pair.f64()?);
                }
                10 => {
                    r.value()?;
                    control.push([pair.f64()?, 0.0, 0.0]);
                }
                20 => last(control, "spline_point")?[1] = pair.f64()?,
                30 => last(control, "spline_point")?[2] = pair.f64()?,
                11 => {
                    r.value()?;
                    fit.push([pair.f64()?, 0.0, 0.0]);
                }
                21 => last(fit, "spline_point")?[1] = pair.f64()?,
                31 => last(fit, "spline_point")?[2] = pair.f64()?,
                _ => {}
            }
            matches!(code, 10 | 20 | 30 | 11 | 21 | 31 | 12 | 22 | 32 | 13 | 23 | 33 | 40..=44 | 70..=74 | 210 | 220 | 230)
        }
        Kind::Solid { corners, extrusion } | Kind::Trace { corners, extrusion } => {
            let corner = (0..4usize).find_map(|i| {
                let base = 10i32.saturating_add(i32::try_from(i).unwrap_or(0));
                axis_of(code, base).map(|a| (i, a))
            });
            if let Some((i, a)) = corner {
                if let Some(c) = corners.get_mut(i) {
                    set3(c, a, pair.f64()?);
                }
                true
            } else if let Some(a) = axis_of(code, 210) {
                set3(extrusion, a, pair.f64()?);
                true
            } else {
                code == 39
            }
        }
        Kind::Insert { name, location, x_scale, y_scale, rotation, columns, rows, column_spacing, row_spacing, extrusion, has_attributes, .. } => {
            if let Some(a) = axis_of(code, 10) {
                set3(location, a, pair.f64()?);
                true
            } else if let Some(a) = axis_of(code, 210) {
                set3(extrusion, a, pair.f64()?);
                true
            } else {
                match code {
                    2 => *name = pair.str()?.to_owned(),
                    41 => *x_scale = pair.f64()?,
                    42 => *y_scale = pair.f64()?,
                    50 => *rotation = pair.f64()?,
                    70 => *columns = pair.i16()?,
                    71 => *rows = pair.i16()?,
                    44 => *column_spacing = pair.f64()?,
                    45 => *row_spacing = pair.f64()?,
                    66 => *has_attributes = pair.i16()? != 0,
                    _ => {}
                }
                matches!(code, 2 | 41..=45 | 50 | 66 | 70 | 71)
            }
        }
        _ => false,
    };
    if handled {
        return Ok(());
    }
    let c = &mut e.common;
    match code {
        8 => c.layer = pair.string()?,
        6 => c.line_type = pair.string()?,
        62 => c.colour = Colour(pair.i16()?),
        370 => c.lineweight = pair.i16()?,
        420 => c.true_colour = pair.i32()?,
        60 => c.visible = pair.i16()? == 0,
        67 => c.paper_space = pair.i16()? != 0,
        _ => {}
    }
    Ok(())
}

/// Reads one entity after its `0/TYPE` pair; stops before the next code-0 pair.
fn read_entity_body(r: &mut Reader<'_, '_>, type_name: &str) -> Result<Entity> {
    let mut e = Entity { common: Common::default(), kind: Kind::for_type(type_name) };
    loop {
        let pair = r.expect()?;
        if pair.code == 0 {
            r.put_back(pair);
            return Ok(e);
        }
        apply(&mut e, pair, r)?;
    }
}

/// The next raw entity, or `None` at `ENDSEC`, `ENDBLK` or `EOF` (put back).
fn read_raw(r: &mut Reader<'_, '_>) -> Result<Option<Entity>> {
    let pair = r.expect()?;
    if pair.code != 0 {
        return Err(VectorError::MalformedDxf("entity"));
    }
    let name = pair.str()?.to_owned();
    if matches!(name.as_str(), "ENDSEC" | "ENDBLK" | "EOF") {
        r.put_back(pair);
        return Ok(None);
    }
    read_entity_body(r, &name).map(Some)
}

/// Reads entities up to `ENDSEC` / `ENDBLK` / `EOF`, gathering POLYLINE vertices and INSERT
/// attributes as `dxf` 0.6.2 `collect_entities` does (each run ends at the first other record; a
/// SEQEND right there is swallowed).
fn read_entities(r: &mut Reader<'_, '_>, out: &mut Vec<Entity>) -> Result<()> {
    let mut pending: Option<Entity> = None;
    loop {
        let next = match pending.take() {
            Some(e) => Some(e),
            None => read_raw(r)?,
        };
        let Some(mut e) = next else { return Ok(()) };
        let gathers = match &e.kind {
            Kind::Polyline { .. } => true,
            Kind::Insert { has_attributes, .. } => *has_attributes,
            _ => false,
        };
        if gathers {
            loop {
                let Some(child) = read_raw(r)? else { break };
                match (&mut e.kind, child.kind) {
                    (Kind::Polyline { vertices, .. }, Kind::Vertex(v)) => {
                        r.value()?;
                        vertices.push(v);
                    }
                    (Kind::Insert { attributes, .. }, Kind::Attrib) => *attributes = attributes.saturating_add(1),
                    (_, Kind::Seqend) => break,
                    (_, kind) => {
                        pending = Some(Entity { common: child.common, kind });
                        break;
                    }
                }
            }
        }
        out.push(e);
    }
}

fn read_header(r: &mut Reader<'_, '_>, d: &mut Drawing) -> Result<()> {
    let mut var = String::new();
    loop {
        let pair = r.expect()?;
        match pair.code {
            0 => {
                r.put_back(pair);
                return Ok(());
            }
            9 => var = pair.string()?,
            70 if var == "$INSUNITS" => d.insunits = pair.i16()?,
            70 if var == "$MEASUREMENT" => d.measurement = pair.i16()?,
            _ => {}
        }
    }
}

fn read_layer(r: &mut Reader<'_, '_>) -> Result<Layer> {
    let mut l = Layer { name: String::new(), colour: Colour(7), on: true, line_type: "CONTINUOUS".into(), line_weight: 0 };
    loop {
        let pair = r.expect()?;
        match pair.code {
            0 => {
                r.put_back(pair);
                return Ok(l);
            }
            2 => l.name = pair.string()?,
            62 => {
                let v = pair.i16()?;
                l.on = v >= 0;
                l.colour = Colour(v.saturating_abs());
            }
            6 => l.line_type = pair.string()?,
            370 => l.line_weight = pair.i16()?,
            _ => {}
        }
    }
}

/// Skips the pairs of a record up to the next code-0 pair.
fn skip_record(r: &mut Reader<'_, '_>) -> Result<()> {
    loop {
        let pair = r.expect()?;
        if pair.code == 0 {
            r.put_back(pair);
            return Ok(());
        }
    }
}

fn read_tables(r: &mut Reader<'_, '_>, d: &mut Drawing) -> Result<()> {
    loop {
        let pair = r.expect()?;
        if pair.is_zero("ENDSEC") {
            r.put_back(pair);
            return Ok(());
        }
        if !pair.is_zero("TABLE") {
            return Err(VectorError::MalformedDxf("tables"));
        }
        let mut table = String::new();
        loop {
            let p = r.expect()?;
            if p.code == 0 {
                r.put_back(p);
                break;
            }
            if p.code == 2 && table.is_empty() {
                table = p.string()?;
            }
        }
        loop {
            let p = r.expect()?;
            if p.is_zero("ENDTAB") {
                skip_record(r)?;
                break;
            }
            if p.is_zero("ENDSEC") {
                r.put_back(p);
                break;
            }
            if table == "LAYER" && p.is_zero("LAYER") {
                let layer = read_layer(r)?;
                d.layers.push(layer);
            } else {
                skip_record(r)?;
            }
        }
    }
}

fn read_blocks(r: &mut Reader<'_, '_>, d: &mut Drawing) -> Result<()> {
    loop {
        let pair = r.expect()?;
        if pair.is_zero("ENDSEC") {
            r.put_back(pair);
            return Ok(());
        }
        if !pair.is_zero("BLOCK") {
            return Err(VectorError::MalformedDxf("blocks"));
        }
        let mut block = Block { name: String::new(), base: ORIGIN, entities: Vec::new() };
        loop {
            let p = r.expect()?;
            if p.code == 0 {
                r.put_back(p);
                break;
            }
            if p.code == 2 {
                block.name = p.string()?;
            } else if let Some(a) = axis_of(p.code, 10) {
                set3(&mut block.base, a, p.f64()?);
            }
        }
        read_entities(r, &mut block.entities)?;
        let end = r.expect()?;
        if end.is_zero("ENDBLK") {
            skip_record(r)?;
        } else {
            // ENDSEC or EOF before ENDBLK: the block keeps what was read (as `dxf` 0.6.2 does).
            r.put_back(end);
        }
        d.blocks.push(block);
    }
}

/// Reads an ASCII or binary DXF drawing (module docs). `value_cap` bounds the stored vertices,
/// knots, weights and points.
pub(crate) fn read_drawing(bytes: &[u8], binary: bool, value_cap: usize, cancel: &CancellationToken) -> Result<Drawing> {
    let mut r = Reader::new(bytes, binary, value_cap, cancel)?;
    let mut d = Drawing::default();
    loop {
        let Some(pair) = r.next()? else { return Ok(d) };
        if pair.is_zero("EOF") {
            return Ok(d);
        }
        if !pair.is_zero("SECTION") {
            return Err(VectorError::MalformedDxf("section"));
        }
        let name = r.expect()?;
        if name.code != 2 {
            return Err(VectorError::MalformedDxf("section_name"));
        }
        match name.str()? {
            "HEADER" => read_header(&mut r, &mut d)?,
            "TABLES" => read_tables(&mut r, &mut d)?,
            "BLOCKS" => read_blocks(&mut r, &mut d)?,
            "ENTITIES" => read_entities(&mut r, &mut d.entities)?,
            _ => loop {
                let p = r.expect()?;
                if p.is_zero("ENDSEC") {
                    r.put_back(p);
                    break;
                }
            },
        }
        if !r.expect()?.is_zero("ENDSEC") {
            return Err(VectorError::MalformedDxf("endsec"));
        }
    }
}
