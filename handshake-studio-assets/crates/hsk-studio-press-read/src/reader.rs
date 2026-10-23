//! Bounded PDF reader: header, `startxref`, classic xref tables, xref streams, hybrid
//! `/XRefStm`, `/Prev` chains (newest entry wins, loops and depth refused), object streams.
//! No repair/reconstruction heuristics: a damaged cross-reference structure is a typed refusal.
//!
//! Hostile-input bounds (ISO 32000-2 7.5.4 cross-reference table, 7.5.7 object streams, 7.5.8
//! cross-reference streams; the numeric limits are `Limits`): object numbers above
//! `max_object_number`, `/W` field widths above 8 bytes, `/Size` and `/Index` counts the stream data
//! cannot hold, `/Length` values past the end of the input, `/N` and `/First` of object streams
//! beyond the decoded data, overlapping objects (the bytes parsed over all indirect objects may not
//! exceed twice the input, the parsed bytes of one object stream may not exceed its decoded size)
//! and the object count. Tests: `hostile_parser_dims_u32_max`, `hostile_nesting_cycles_and_overlap`.
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{Limits, PressError, Result};
use crate::filters;
use crate::syntax::{Dict, Lexer, Object, ObjRef, Stream, is_regular, parse_standalone};
use hsk_studio_accord::CancellationToken;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub struct Entry {
    pub generation: u16,
    pub obj: Object,
    /// Byte range of the object body in the source (verbatim copy on save); `None` once the
    /// object is modified or when it came from an object stream.
    pub span: Option<(usize, usize)>,
    /// Offset of a stream's data in the parsed bytes (`None` for other objects, object-stream
    /// members and objects built in memory). Used by the payload proof to tie raw bytes to the
    /// decoder that consumed them.
    pub data_at: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
enum Xref {
    Free,
    Offset(usize, u16),
    InStream { stm: u32 },
}

pub(crate) struct Parsed {
    pub version: String,
    pub objects: BTreeMap<u32, Entry>,
    pub trailer: Dict,
    pub xref_sections: usize,
    pub encrypted: bool,
}

pub fn check_cancel(cancel: &CancellationToken) -> Result<()> {
    cancel.check().map_err(|_| PressError::Canceled)
}

pub(crate) fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).rposition(|w| w == needle)
}

/// Parsed indirect object: number, value, the verbatim body span when it can be copied, and the
/// offset of the stream data.
type Indirect = (u32, Object, Option<(usize, usize)>, Option<usize>);

struct Reader<'a> {
    bytes: &'a [u8],
    base: usize,
    limits: &'a Limits,
    /// Remaining bytes the `endstream` recovery scan may walk (quadratic-scan bound).
    scan_budget: usize,
    /// Bytes of indirect-object text that may still be parsed: objects that overlap would be
    /// parsed (and copied) once per overlap, so the total is bounded by twice the input.
    parse_left: usize,
    /// Objects the whole parse may still produce (`Limits::max_nodes`).
    nodes_left: usize,
}

impl<'a> Reader<'a> {
    fn lexer(&self, pos: usize) -> Lexer<'a> {
        Lexer::new(self.bytes, pos, self.limits.max_depth).with_nodes(self.nodes_left)
    }

    /// Object number `first + i` of a cross-reference subsection, within `Limits::max_object_number`.
    fn xref_number(&self, first: u64, i: u64) -> Result<u32> {
        let n = first.checked_add(i).and_then(|n| u32::try_from(n).ok()).ok_or(PressError::Malformed("xref_number"))?;
        if n > self.limits.max_object_number {
            return Err(PressError::InputLimit("object_number"));
        }
        Ok(n)
    }

    /// Value of an indirect `/Length`. Only a number is read: an arbitrary object here would let many
    /// streams point at one huge object and have it parsed once per stream.
    fn peek_int_object(&self, xref: &BTreeMap<u32, Xref>, r: ObjRef) -> Option<i64> {
        let Some(Xref::Offset(off, _)) = xref.get(&r.num) else {
            return None;
        };
        let mut lx = Lexer::new(self.bytes, off.checked_add(self.base)?, self.limits.max_depth);
        lx.read_uint().ok()?;
        lx.read_uint().ok()?;
        lx.skip_ws();
        if lx.read_keyword() != b"obj" {
            return None;
        }
        lx.skip_ws();
        if !matches!(lx.peek(), Some(b'0'..=b'9' | b'+' | b'-')) {
            return None;
        }
        lx.parse_object(0, false).ok()?.as_int()
    }

    /// `lx` is positioned just after the `stream` keyword. Returns the data and its offset.
    fn read_stream_data(&mut self, lx: &mut Lexer<'a>, dict: &Dict, xref: &BTreeMap<u32, Xref>) -> Result<(Vec<u8>, usize)> {
        let mut p = lx.pos;
        match self.bytes.get(p) {
            Some(b'\r') => {
                p = p.saturating_add(1);
                if self.bytes.get(p) == Some(&b'\n') {
                    p = p.saturating_add(1);
                }
            }
            Some(b'\n') => p = p.saturating_add(1),
            _ => {}
        }
        let declared = match dict.get("Length") {
            Some(Object::Int(n)) => Some(*n),
            Some(Object::Ref(r)) => self.peek_int_object(xref, *r),
            _ => None,
        };
        let valid = declared.and_then(|n| usize::try_from(n).ok()).filter(|n| {
            let Some(end) = p.checked_add(*n).filter(|e| *e <= self.bytes.len()) else {
                return false;
            };
            let mut probe = self.lexer(end);
            probe.skip_ws();
            probe.starts_with(b"endstream")
        });
        let end = match valid {
            Some(n) => p.saturating_add(n),
            None => {
                let tail = self.bytes.get(p..).unwrap_or(&[]);
                let window = tail.len().min(self.scan_budget);
                let Some(at) = find(tail.get(..window).unwrap_or(&[]), b"endstream") else {
                    return Err(if window < tail.len() {
                        PressError::InputLimit("scan_budget")
                    } else {
                        PressError::Malformed("endstream_missing")
                    });
                };
                self.scan_budget = self.scan_budget.saturating_sub(at);
                let mut end = p.saturating_add(at);
                // Drop the EOL that precedes `endstream`.
                if end > p && self.bytes.get(end.saturating_sub(1)) == Some(&b'\n') {
                    end = end.saturating_sub(1);
                }
                if end > p && self.bytes.get(end.saturating_sub(1)) == Some(&b'\r') {
                    end = end.saturating_sub(1);
                }
                end
            }
        };
        if end.saturating_sub(p) > self.parse_left {
            return Err(PressError::InputLimit("stream_bytes"));
        }
        let data = self.bytes.get(p..end).ok_or(PressError::Malformed("stream_range"))?.to_vec();
        lx.pos = end;
        lx.skip_ws();
        if !lx.starts_with(b"endstream") {
            return Err(PressError::Malformed("endstream_missing"));
        }
        lx.advance(9);
        Ok((data, p))
    }

    /// Charges the bytes an object spanned to the overlap budget.
    fn charge(&mut self, from: usize, to: usize) -> Result<()> {
        self.parse_left = self.parse_left.checked_sub(to.saturating_sub(from)).ok_or(PressError::InputLimit("parse_budget"))?;
        Ok(())
    }

    /// Parses `num gen obj <object> [stream ... endstream]` at `abs`.
    fn indirect_at(&mut self, abs: usize, xref: &BTreeMap<u32, Xref>) -> Result<Indirect> {
        let mut lx = self.lexer(abs);
        let num = lx.read_uint()?;
        let _generation = lx.read_uint()?;
        lx.skip_ws();
        if lx.read_keyword() != b"obj" {
            return Err(PressError::Malformed("obj_keyword"));
        }
        lx.skip_ws();
        let num = u32::try_from(num).map_err(|_| PressError::Malformed("object_number"))?;
        if num > self.limits.max_object_number {
            return Err(PressError::InputLimit("object_number"));
        }
        if lx.starts_with(b"endobj") {
            // `n g obj endobj` is the null object.
            self.charge(abs, lx.pos)?;
            return Ok((num, Object::Null, None, None));
        }
        let start = lx.pos;
        let obj = lx.parse_object(0, true)?;
        self.nodes_left = lx.nodes_left;
        let mut data_at = None;
        let obj = match obj {
            Object::Dict(dict) => {
                let after_dict = lx.pos;
                lx.skip_ws();
                if lx.starts_with(b"stream") {
                    lx.advance(6);
                    let (data, at) = self.read_stream_data(&mut lx, &dict, xref)?;
                    data_at = Some(at);
                    Object::Stream(Box::new(Stream { dict, data }))
                } else {
                    lx.pos = after_dict;
                    Object::Dict(dict)
                }
            }
            other => other,
        };
        self.charge(abs, lx.pos)?;
        Ok((num, obj, Some((start, lx.pos)), data_at))
    }

    fn read_section(&mut self, abs: usize, xref: &mut BTreeMap<u32, Xref>, budget: &mut usize) -> Result<Dict> {
        let mut lx = self.lexer(abs);
        lx.skip_ws();
        if lx.starts_with(b"xref") {
            lx.advance(4);
            loop {
                lx.skip_ws();
                if lx.starts_with(b"trailer") {
                    lx.advance(7);
                    break;
                }
                let first = lx.read_uint()?;
                let count = lx.read_uint()?;
                // An entry is 20 bytes; a subsection cannot list more entries than the input holds.
                let room = (self.bytes.len().saturating_sub(lx.pos) / 18).saturating_add(1);
                if count > u64::try_from(room).unwrap_or(u64::MAX) {
                    return Err(PressError::Malformed("xref_count"));
                }
                for i in 0..count {
                    let off = lx.read_uint()?;
                    let generation = lx.read_uint()?;
                    lx.skip_ws();
                    let kind = lx.peek();
                    lx.advance(1);
                    let num = self.xref_number(first, i)?;
                    let entry = match kind {
                        Some(b'n') => Xref::Offset(
                            usize::try_from(off).map_err(|_| PressError::Malformed("xref_offset"))?,
                            u16::try_from(generation).unwrap_or(u16::MAX),
                        ),
                        Some(b'f') => Xref::Free,
                        _ => return Err(PressError::Malformed("xref_entry")),
                    };
                    xref.entry(num).or_insert(entry);
                    if xref.len() > self.limits.max_objects {
                        return Err(PressError::InputLimit("max_objects"));
                    }
                }
            }
            let parsed = lx.parse_object(0, true);
            self.nodes_left = lx.nodes_left;
            return match parsed? {
                Object::Dict(d) => Ok(d),
                _ => Err(PressError::Malformed("trailer")),
            };
        }
        // Cross-reference stream.
        let (_num, obj, _span, _at) = self.indirect_at(abs, xref)?;
        let Object::Stream(stream) = obj else {
            return Err(PressError::Malformed("xref_section"));
        };
        if !stream.dict.get("Type").is_some_and(|t| t.is_name("XRef")) {
            return Err(PressError::Malformed("xref_stream_type"));
        }
        let data = filters::decode_stream(&stream, self.limits, budget)?;
        // /W: byte widths of the three fields; 0 omits a field (ISO 32000-2 7.5.8.2); 8 bytes holds a u64.
        let w: Vec<usize> = stream
            .dict
            .get("W")
            .and_then(Object::as_array)
            .and_then(|a| a.iter().map(|o| o.as_int().and_then(|v| usize::try_from(v).ok()).filter(|v| *v <= 8)).collect::<Option<Vec<_>>>())
            .ok_or(PressError::Malformed("xref_w"))?;
        let row = w.iter().sum::<usize>();
        if w.len() != 3 || row == 0 {
            return Err(PressError::Malformed("xref_w"));
        }
        let size = stream
            .dict
            .get("Size")
            .and_then(Object::as_int)
            .and_then(|v| u64::try_from(v).ok())
            .ok_or(PressError::Malformed("xref_size"))?;
        let index: Vec<(u64, u64)> = match stream.dict.get("Index").and_then(Object::as_array) {
            Some(a) => {
                let mut v = Vec::new();
                let mut it = a.iter();
                while let (Some(f), Some(c)) = (it.next(), it.next()) {
                    let parsed = (f.as_int().and_then(|x| u64::try_from(x).ok()), c.as_int().and_then(|x| u64::try_from(x).ok()));
                    let (Some(f), Some(c)) = parsed else {
                        return Err(PressError::Malformed("xref_index"));
                    };
                    v.push((f, c));
                }
                v
            }
            None => vec![(0, size)],
        };
        let mut cursor = 0usize;
        for (first, count) in index {
            // The stream data must hold every entry it announces (a huge /Size or /Index count is refused here).
            let room = data.len().saturating_sub(cursor).checked_div(row).unwrap_or(0);
            if count > u64::try_from(room).unwrap_or(u64::MAX) {
                return Err(PressError::Malformed("xref_stream_short"));
            }
            for i in 0..count {
                let mut field = [0u64; 3];
                for (slot, width) in field.iter_mut().zip(&w) {
                    let end = cursor.saturating_add(*width);
                    let chunk = data.get(cursor..end).ok_or(PressError::Malformed("xref_stream_short"))?;
                    *slot = chunk.iter().fold(0u64, |v, b| v.wrapping_shl(8) | u64::from(*b));
                    cursor = end;
                }
                let kind = if w.first() == Some(&0) { 1 } else { field[0] };
                let num = self.xref_number(first, i)?;
                let entry = match kind {
                    0 => Xref::Free,
                    1 => Xref::Offset(
                        usize::try_from(field[1]).map_err(|_| PressError::Malformed("xref_offset"))?,
                        u16::try_from(field[2]).unwrap_or(u16::MAX),
                    ),
                    2 => Xref::InStream { stm: u32::try_from(field[1]).map_err(|_| PressError::Malformed("xref_objstm"))? },
                    _ => continue, // unknown types are treated as null objects (ISO 32000 7.5.8.3)
                };
                xref.entry(num).or_insert(entry);
                if xref.len() > self.limits.max_objects {
                    return Err(PressError::InputLimit("max_objects"));
                }
            }
        }
        Ok(stream.dict)
    }
}

pub(crate) fn parse_document(bytes: &[u8], limits: &Limits, cancel: &CancellationToken) -> Result<Parsed> {
    check_cancel(cancel)?;
    if bytes.len() > limits.max_bytes {
        return Err(PressError::InputLimit("max_bytes"));
    }
    let head = bytes.get(..bytes.len().min(1024)).unwrap_or(&[]);
    let base = find(head, b"%PDF-").ok_or(PressError::Malformed("header"))?;
    let version: String = bytes
        .get(base.saturating_add(5)..)
        .unwrap_or(&[])
        .iter()
        .take(8)
        .take_while(|b| is_regular(**b))
        .map(|b| char::from(*b))
        .collect();
    let tail_start = bytes.len().saturating_sub(1536);
    let sx = rfind(bytes.get(tail_start..).unwrap_or(&[]), b"startxref").ok_or(PressError::Malformed("startxref"))?.saturating_add(tail_start);
    let mut lx = Lexer::new(bytes, sx.saturating_add(9), limits.max_depth);
    let start = usize::try_from(lx.read_uint()?).map_err(|_| PressError::Malformed("startxref"))?;

    let mut reader = Reader {
        bytes,
        base,
        limits,
        scan_budget: limits.max_bytes.saturating_mul(2),
        parse_left: bytes.len().saturating_mul(2),
        nodes_left: limits.max_nodes,
    };
    let mut xref: BTreeMap<u32, Xref> = BTreeMap::new();
    let mut trailer = Dict::default();
    let mut have_trailer = false;
    let mut visited = BTreeSet::new();
    let mut stack: Vec<(usize, bool)> = vec![(start, true)];
    let mut sections = 0usize;
    let mut budget = limits.max_decoded_total;
    while let Some((off, follow)) = stack.pop() {
        check_cancel(cancel)?;
        if !visited.insert(off) {
            return Err(PressError::Malformed("xref_loop"));
        }
        sections = sections.saturating_add(1);
        if sections > limits.max_xref_sections {
            return Err(PressError::InputLimit("xref_sections"));
        }
        let abs = off.checked_add(base).filter(|a| *a < bytes.len()).ok_or(PressError::Malformed("xref_offset"))?;
        let dict = reader.read_section(abs, &mut xref, &mut budget)?;
        if follow {
            for (key, follow_next) in [("Prev", true), ("XRefStm", false)] {
                if let Some(v) = dict.get(key).and_then(Object::as_int) {
                    let v = usize::try_from(v).map_err(|_| PressError::Malformed("xref_prev"))?;
                    stack.push((v, follow_next));
                }
            }
        }
        if !have_trailer {
            trailer = dict;
            have_trailer = true;
        } else {
            for key in ["Root", "Info", "ID", "Encrypt"] {
                if trailer.get(key).is_none()
                    && let Some(v) = dict.get(key)
                {
                    trailer.set(key, v.clone());
                }
            }
        }
    }
    let encrypted = trailer.get("Encrypt").is_some();

    let mut objects: BTreeMap<u32, Entry> = BTreeMap::new();
    let mut in_streams: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    for (i, (num, x)) in xref.iter().enumerate() {
        if i % 256 == 0 {
            check_cancel(cancel)?;
        }
        match *x {
            Xref::Free => {}
            Xref::Offset(off, generation) => {
                if objects.len() >= limits.max_objects {
                    return Err(PressError::InputLimit("max_objects"));
                }
                let abs = off.checked_add(base).filter(|a| *a < bytes.len()).ok_or(PressError::Malformed("object_offset"))?;
                let (n, obj, span, data_at) = reader.indirect_at(abs, &xref)?;
                if n != *num {
                    return Err(PressError::Malformed("object_number_mismatch"));
                }
                objects.insert(*num, Entry { generation, obj, span, data_at });
            }
            Xref::InStream { stm } => {
                in_streams.entry(stm).or_default().insert(*num);
            }
        }
    }
    if !encrypted {
        for (stm, members) in in_streams {
            check_cancel(cancel)?;
            let Some(Entry { obj: Object::Stream(s), .. }) = objects.get(&stm) else {
                return Err(PressError::Malformed("objstm_missing"));
            };
            if !s.dict.get("Type").is_some_and(|t| t.is_name("ObjStm")) {
                return Err(PressError::Malformed("objstm_type"));
            }
            let data = filters::decode_stream(s, limits, &mut budget)?;
            let count = |key: &str, error: &'static str| -> Result<usize> {
                s.dict.get(key).and_then(Object::as_int).and_then(|v| usize::try_from(v).ok()).ok_or(PressError::Malformed(error))
            };
            let n = count("N", "objstm_n")?;
            let first = count("First", "objstm_first")?;
            // Every member needs at least two bytes of header ("n o").
            if n > (data.len() / 2).saturating_add(1) {
                return Err(PressError::Malformed("objstm_n"));
            }
            let mut header = Lexer::new(&data, 0, limits.max_depth);
            let mut pairs = Vec::with_capacity(n.min(4096));
            for _ in 0..n {
                pairs.push((header.read_uint()?, header.read_uint()?));
            }
            // Members of one object stream never overlap: the bytes parsed may not exceed the decoded size.
            let mut parsed_bytes = 0usize;
            for (onum, ooff) in pairs {
                let Ok(onum) = u32::try_from(onum) else { continue };
                if !members.contains(&onum) || objects.contains_key(&onum) {
                    continue;
                }
                if objects.len() >= limits.max_objects {
                    return Err(PressError::InputLimit("max_objects"));
                }
                let at = first
                    .checked_add(usize::try_from(ooff).map_err(|_| PressError::Malformed("objstm_offset"))?)
                    .filter(|a| *a < data.len())
                    .ok_or(PressError::Malformed("objstm_offset"))?;
                let (obj, used) = parse_standalone(&data, at, limits.max_depth, &mut reader.nodes_left)?;
                parsed_bytes = parsed_bytes.saturating_add(used);
                if parsed_bytes > data.len() {
                    return Err(PressError::InputLimit("objstm_overlap"));
                }
                objects.insert(onum, Entry { generation: 0, obj, span: None, data_at: None });
            }
        }
    }
    Ok(Parsed { version, objects, trailer, xref_sections: sections, encrypted })
}
