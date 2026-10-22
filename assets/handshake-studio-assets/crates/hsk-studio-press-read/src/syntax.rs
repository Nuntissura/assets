//! PDF object model and tokenizer (ISO 32000 7.3). Bounded: nesting depth is limited (and clamped to
//! `HARD_MAX_DEPTH` whatever the caller asks, so recursion cannot exhaust the stack), the number of
//! produced objects can be metered, numeric tokens are length-capped, every loop advances over the
//! input slice, nothing is executed. Hostile-input tests: `hostile_parser_dims_u32_max`,
//! `hostile_nesting_cycles_and_overlap`.
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{HARD_MAX_DEPTH, PressError, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjRef {
    pub num: u32,
    pub generation: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Object {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Name(Vec<u8>),
    Str(Vec<u8>),
    Array(Vec<Object>),
    Dict(Dict),
    Stream(Box<Stream>),
    Ref(ObjRef),
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Dict {
    pub entries: Vec<(Vec<u8>, Object)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stream {
    pub dict: Dict,
    /// Raw (still encoded) stream bytes.
    pub data: Vec<u8>,
}

impl Dict {
    pub fn get(&self, key: &str) -> Option<&Object> {
        self.entries.iter().find(|(k, _)| k == key.as_bytes()).map(|(_, v)| v)
    }
    pub fn set(&mut self, key: &str, value: Object) {
        if let Some(slot) = self.entries.iter_mut().find(|(k, _)| k == key.as_bytes()) {
            slot.1 = value;
        } else {
            self.entries.push((key.as_bytes().to_vec(), value));
        }
    }
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Object> {
        self.entries.iter_mut().find(|(k, _)| k == key.as_bytes()).map(|(_, v)| v)
    }
    pub fn get_key(&self, key: &[u8]) -> Option<&Object> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn set_key(&mut self, key: &[u8], value: Object) {
        if let Some(slot) = self.entries.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value;
        } else {
            self.entries.push((key.to_vec(), value));
        }
    }
    pub fn remove_key(&mut self, key: &[u8]) -> Option<Object> {
        let at = self.entries.iter().position(|(k, _)| k == key)?;
        Some(self.entries.remove(at).1)
    }
    pub fn remove(&mut self, key: &str) -> Option<Object> {
        let at = self.entries.iter().position(|(k, _)| k == key.as_bytes())?;
        Some(self.entries.remove(at).1)
    }
}

impl Object {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(v) => Some(*v),
            Self::Real(v) if v.is_finite() && v.fract() == 0.0 && v.abs() < 9.0e15 => Some(*v as i64),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Int(v) => Some(*v as f64),
            Self::Real(v) if v.is_finite() => Some(*v),
            _ => None,
        }
    }
    pub fn as_name(&self) -> Option<&[u8]> {
        match self {
            Self::Name(n) => Some(n),
            _ => None,
        }
    }
    pub fn is_name(&self, name: &str) -> bool {
        self.as_name() == Some(name.as_bytes())
    }
    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            Self::Dict(d) => Some(d),
            Self::Stream(s) => Some(&s.dict),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Object]> {
        match self {
            Self::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_ref(&self) -> Option<ObjRef> {
        match self {
            Self::Ref(r) => Some(*r),
            _ => None,
        }
    }
}

pub(crate) fn is_ws(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}
pub(crate) fn is_delim(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}
pub(crate) fn is_regular(b: u8) -> bool {
    !is_ws(b) && !is_delim(b)
}

pub(crate) fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b.saturating_sub(b'0')),
        b'a'..=b'f' => Some(b.saturating_sub(b'a').saturating_add(10)),
        b'A'..=b'F' => Some(b.saturating_sub(b'A').saturating_add(10)),
        _ => None,
    }
}

/// Value of one ASCII decimal digit (callers match `b'0'..=b'9'` first).
fn digit(b: u8) -> u8 {
    b.saturating_sub(b'0')
}

/// Longest numeric token the tokenizer reads (a PDF real never needs more than about 40 characters).
const MAX_NUMBER_LEN: usize = 64;

pub(crate) struct Lexer<'a> {
    pub data: &'a [u8],
    pub pos: usize,
    pub max_depth: usize,
    /// Objects the lexer may still produce (every scalar, array element and dictionary value);
    /// `usize::MAX` means unmetered. Callers that meter copy it back after a parse.
    pub nodes_left: usize,
}

impl<'a> Lexer<'a> {
    /// `max_depth` is clamped to the hard stack ceiling (`HARD_MAX_DEPTH`).
    pub fn new(data: &'a [u8], pos: usize, max_depth: usize) -> Self {
        Self { data, pos, max_depth: max_depth.min(HARD_MAX_DEPTH), nodes_left: usize::MAX }
    }

    pub fn with_nodes(mut self, nodes_left: usize) -> Self {
        self.nodes_left = nodes_left;
        self
    }

    pub fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    /// Moves forward by `n` bytes (saturating; positions past the end read as end of input).
    pub fn advance(&mut self, n: usize) {
        self.pos = self.pos.saturating_add(n);
    }

    fn byte_at(&self, offset: usize) -> Option<u8> {
        self.data.get(self.pos.saturating_add(offset)).copied()
    }

    pub fn skip_ws(&mut self) {
        loop {
            while self.peek().is_some_and(is_ws) {
                self.advance(1);
            }
            if self.peek() == Some(b'%') {
                while self.peek().is_some_and(|b| !matches!(b, 10 | 13)) {
                    self.advance(1);
                }
            } else {
                break;
            }
        }
    }

    pub fn starts_with(&self, kw: &[u8]) -> bool {
        self.data.get(self.pos..).is_some_and(|rest| rest.starts_with(kw))
    }

    /// Reads a run of regular characters (keyword or operator).
    pub fn read_keyword(&mut self) -> &'a [u8] {
        self.pos = self.pos.min(self.data.len());
        let start = self.pos;
        while self.peek().is_some_and(is_regular) {
            self.advance(1);
        }
        self.data.get(start..self.pos).unwrap_or(&[])
    }

    pub fn read_uint(&mut self) -> Result<u64> {
        self.skip_ws();
        let start = self.pos;
        let mut v: u64 = 0;
        while let Some(b @ b'0'..=b'9') = self.peek() {
            v = v
                .checked_mul(10)
                .and_then(|v| v.checked_add(u64::from(digit(b))))
                .ok_or(PressError::Malformed("integer_overflow"))?;
            self.advance(1);
        }
        if self.pos == start {
            return Err(PressError::Malformed("expected_integer"));
        }
        Ok(v)
    }

    /// Parses one object. `allow_refs` enables `n g R` (off for content streams).
    pub fn parse_object(&mut self, depth: usize, allow_refs: bool) -> Result<Object> {
        if depth > self.max_depth {
            return Err(PressError::InputLimit("depth"));
        }
        self.nodes_left = self.nodes_left.checked_sub(1).ok_or(PressError::InputLimit("nodes"))?;
        self.skip_ws();
        let Some(b) = self.peek() else {
            return Err(PressError::Malformed("unexpected_eof"));
        };
        match b {
            b'/' => Ok(Object::Name(self.parse_name())),
            b'(' => self.parse_literal_string(),
            b'<' => {
                if self.byte_at(1) == Some(b'<') {
                    Ok(Object::Dict(self.parse_dict(depth)?))
                } else {
                    self.parse_hex_string()
                }
            }
            b'[' => {
                self.advance(1);
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    match self.peek() {
                        None => return Err(PressError::Malformed("unterminated_array")),
                        Some(b']') => {
                            self.advance(1);
                            break;
                        }
                        _ => items.push(self.parse_object(depth.saturating_add(1), allow_refs)?),
                    }
                }
                Ok(Object::Array(items))
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => self.parse_number(allow_refs),
            _ if is_regular(b) => {
                let kw = self.read_keyword();
                match kw {
                    b"true" => Ok(Object::Bool(true)),
                    b"false" => Ok(Object::Bool(false)),
                    b"null" => Ok(Object::Null),
                    _ => Err(PressError::Malformed("unexpected_keyword")),
                }
            }
            _ => Err(PressError::Malformed("unexpected_delimiter")),
        }
    }

    pub fn parse_dict(&mut self, depth: usize) -> Result<Dict> {
        // positioned at "<<"
        self.advance(2);
        let mut dict = Dict::default();
        // Duplicate-key lookups stay linear for small dictionaries; large ones get a hash index so
        // a hostile dictionary of millions of keys cannot cost quadratic time.
        let mut index: Option<std::collections::HashMap<Vec<u8>, usize>> = None;
        loop {
            self.skip_ws();
            match self.peek() {
                None => return Err(PressError::Malformed("unterminated_dict")),
                Some(b'>') => {
                    if self.byte_at(1) == Some(b'>') {
                        self.advance(2);
                        break;
                    }
                    return Err(PressError::Malformed("dict_close"));
                }
                Some(b'/') => {
                    let key = self.parse_name();
                    let value = self.parse_object(depth.saturating_add(1), true)?;
                    let existing = match &index {
                        Some(m) => m.get(&key).copied(),
                        None => dict.entries.iter().position(|(k, _)| *k == key),
                    };
                    match existing {
                        Some(i) => {
                            if let Some(slot) = dict.entries.get_mut(i) {
                                slot.1 = value;
                            }
                        }
                        None => {
                            if let Some(m) = &mut index {
                                m.insert(key.clone(), dict.entries.len());
                            }
                            dict.entries.push((key, value));
                            if index.is_none() && dict.entries.len() >= 32 {
                                index = Some(dict.entries.iter().enumerate().map(|(i, (k, _))| (k.clone(), i)).collect());
                            }
                        }
                    }
                }
                Some(_) => return Err(PressError::Malformed("dict_key")),
            }
        }
        Ok(dict)
    }

    fn parse_name(&mut self) -> Vec<u8> {
        self.advance(1); // '/'
        let mut out = Vec::new();
        while let Some(b) = self.peek().filter(|b| is_regular(*b)) {
            if b == b'#'
                && let (Some(h), Some(l)) = (self.byte_at(1).and_then(hex_val), self.byte_at(2).and_then(hex_val))
            {
                out.push(h.wrapping_mul(16).wrapping_add(l));
                self.advance(3);
                continue;
            }
            out.push(b);
            self.advance(1);
        }
        out
    }

    fn parse_literal_string(&mut self) -> Result<Object> {
        self.advance(1);
        let mut out = Vec::new();
        let mut nest = 1usize;
        loop {
            let Some(b) = self.peek() else {
                return Err(PressError::Malformed("unterminated_string"));
            };
            self.advance(1);
            match b {
                b'(' => {
                    nest = nest.saturating_add(1);
                    out.push(b);
                }
                b')' => {
                    nest = nest.saturating_sub(1);
                    if nest == 0 {
                        break;
                    }
                    out.push(b);
                }
                b'\\' => {
                    let Some(e) = self.peek() else {
                        return Err(PressError::Malformed("unterminated_string"));
                    };
                    self.advance(1);
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'0'..=b'7' => {
                            let mut v = u32::from(digit(e));
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v.saturating_mul(8).saturating_add(u32::from(digit(d)));
                                        self.advance(1);
                                    }
                                    _ => break,
                                }
                            }
                            out.push((v & 0xff) as u8);
                        }
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.advance(1);
                            }
                        }
                        b'\n' => {}
                        other => out.push(other),
                    }
                }
                _ => out.push(b),
            }
        }
        Ok(Object::Str(out))
    }

    fn parse_hex_string(&mut self) -> Result<Object> {
        self.advance(1);
        let mut out = Vec::new();
        let mut hi: Option<u8> = None;
        loop {
            let Some(b) = self.peek() else {
                return Err(PressError::Malformed("unterminated_hex"));
            };
            self.advance(1);
            if b == b'>' {
                break;
            }
            if is_ws(b) {
                continue;
            }
            let Some(v) = hex_val(b) else {
                return Err(PressError::Malformed("hex_digit"));
            };
            match hi.take() {
                None => hi = Some(v),
                Some(h) => out.push(h.wrapping_mul(16).wrapping_add(v)),
            }
        }
        if let Some(h) = hi {
            out.push(h.wrapping_mul(16));
        }
        Ok(Object::Str(out))
    }

    fn parse_number(&mut self, allow_refs: bool) -> Result<Object> {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if matches!(b, b'+' | b'-' | b'.' | b'0'..=b'9') {
                self.advance(1);
                if self.pos.saturating_sub(start) > MAX_NUMBER_LEN {
                    return Err(PressError::Malformed("number_length"));
                }
            } else {
                break;
            }
        }
        let text = self.data.get(start..self.pos).and_then(|t| std::str::from_utf8(t).ok()).ok_or(PressError::Malformed("number"))?;
        if text.contains('.') {
            let v: f64 = text.parse().map_err(|_| PressError::Malformed("number"))?;
            return Ok(Object::Real(v));
        }
        let Ok(v) = text.parse::<i64>() else {
            // Overflowing integers degrade to reals; garbage is malformed.
            return text.parse::<f64>().map(Object::Real).map_err(|_| PressError::Malformed("number"));
        };
        if allow_refs && (0..=i64::from(u32::MAX)).contains(&v) && text.bytes().all(|b| b.is_ascii_digit()) {
            let save = self.pos;
            self.skip_ws();
            let gstart = self.pos;
            let mut g: u32 = 0;
            while let Some(d @ b'0'..=b'9') = self.peek() {
                g = g.saturating_mul(10).saturating_add(u32::from(digit(d)));
                self.advance(1);
            }
            if self.pos > gstart && g <= u32::from(u16::MAX) {
                self.skip_ws();
                if self.peek() == Some(b'R') && self.byte_at(1).is_none_or(|b| !is_regular(b)) {
                    self.advance(1);
                    return Ok(Object::Ref(ObjRef { num: u32::try_from(v).unwrap_or(u32::MAX), generation: u16::try_from(g).unwrap_or(u16::MAX) }));
                }
            }
            self.pos = save;
        }
        Ok(Object::Int(v))
    }
}

/// Parses a standalone object from `data` (object-stream members) and returns it with the number of
/// bytes it spanned. `nodes_left` meters the produced objects across calls.
pub(crate) fn parse_standalone(data: &[u8], pos: usize, max_depth: usize, nodes_left: &mut usize) -> Result<(Object, usize)> {
    let mut lx = Lexer::new(data, pos, max_depth).with_nodes(*nodes_left);
    let parsed = lx.parse_object(0, true);
    *nodes_left = lx.nodes_left;
    Ok((parsed?, lx.pos.saturating_sub(pos)))
}
