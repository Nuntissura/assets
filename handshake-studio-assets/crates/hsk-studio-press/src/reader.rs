//! Bounded PDF reader: header, `startxref`, classic xref tables, xref streams, hybrid
//! `/XRefStm`, `/Prev` chains (newest entry wins, loops and depth refused), object streams.
//! No repair/reconstruction heuristics: a damaged cross-reference structure is a typed refusal.
use crate::error::{Limits, PressError, Result};
use crate::filters;
use crate::syntax::{Dict, Lexer, Object, ObjRef, Stream, is_regular, parse_standalone};
use hsk_studio_accord::CancellationToken;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub generation: u16,
    pub obj: Object,
    /// Byte range of the object body in the source (verbatim copy on save); `None` once the
    /// object is modified or when it came from an object stream.
    pub span: Option<(usize, usize)>,
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

pub(crate) fn check_cancel(cancel: &CancellationToken) -> Result<()> {
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

struct Reader<'a> {
    bytes: &'a [u8],
    base: usize,
    limits: &'a Limits,
    /// Remaining bytes the `endstream` recovery scan may walk (quadratic-scan bound).
    scan_budget: usize,
}

impl<'a> Reader<'a> {
    fn lexer(&self, pos: usize) -> Lexer<'a> {
        Lexer::new(self.bytes, pos, self.limits.max_depth)
    }

    fn peek_int_object(&self, xref: &BTreeMap<u32, Xref>, r: ObjRef) -> Option<i64> {
        let Some(Xref::Offset(off, _)) = xref.get(&r.num) else {
            return None;
        };
        let mut lx = self.lexer(off.checked_add(self.base)?);
        lx.read_uint().ok()?;
        lx.read_uint().ok()?;
        lx.skip_ws();
        if lx.read_keyword() != b"obj" {
            return None;
        }
        lx.parse_object(0, false).ok()?.as_int()
    }

    /// `lx` is positioned just after the `stream` keyword.
    fn read_stream_data(&mut self, lx: &mut Lexer<'a>, dict: &Dict, xref: &BTreeMap<u32, Xref>) -> Result<Vec<u8>> {
        let mut p = lx.pos;
        match self.bytes.get(p) {
            Some(b'\r') => {
                p += 1;
                if self.bytes.get(p) == Some(&b'\n') {
                    p += 1;
                }
            }
            Some(b'\n') => p += 1,
            _ => {}
        }
        let declared = match dict.get("Length") {
            Some(Object::Int(n)) => Some(*n),
            Some(Object::Ref(r)) => self.peek_int_object(xref, *r),
            _ => None,
        };
        let valid = declared.filter(|n| *n >= 0).map(|n| n as usize).filter(|n| {
            let Some(end) = p.checked_add(*n).filter(|e| *e <= self.bytes.len()) else {
                return false;
            };
            let mut probe = self.lexer(end);
            probe.skip_ws();
            probe.starts_with(b"endstream")
        });
        let end = match valid {
            Some(n) => p + n,
            None => {
                let tail = self.bytes.get(p..).unwrap_or(&[]);
                let window = tail.len().min(self.scan_budget);
                let Some(at) = find(&tail[..window], b"endstream") else {
                    return Err(if window < tail.len() {
                        PressError::InputLimit("scan_budget")
                    } else {
                        PressError::Malformed("endstream_missing")
                    });
                };
                self.scan_budget -= at;
                let mut end = p + at;
                // Drop the EOL that precedes `endstream`.
                if end > p && self.bytes[end - 1] == b'\n' {
                    end -= 1;
                }
                if end > p && self.bytes[end - 1] == b'\r' {
                    end -= 1;
                }
                end
            }
        };
        let data = self.bytes[p..end].to_vec();
        lx.pos = end;
        lx.skip_ws();
        if !lx.starts_with(b"endstream") {
            return Err(PressError::Malformed("endstream_missing"));
        }
        lx.pos += 9;
        Ok(data)
    }

    /// Parses `num gen obj <object> [stream ... endstream]` at `abs`.
    fn indirect_at(&mut self, abs: usize, xref: &BTreeMap<u32, Xref>) -> Result<(u32, Object, (usize, usize))> {
        let mut lx = self.lexer(abs);
        let num = lx.read_uint()?;
        let _generation = lx.read_uint()?;
        lx.skip_ws();
        if lx.read_keyword() != b"obj" {
            return Err(PressError::Malformed("obj_keyword"));
        }
        lx.skip_ws();
        let start = lx.pos;
        let obj = lx.parse_object(0, true)?;
        let obj = match obj {
            Object::Dict(dict) => {
                lx.skip_ws();
                if lx.starts_with(b"stream") {
                    lx.pos += 6;
                    let data = self.read_stream_data(&mut lx, &dict, xref)?;
                    Object::Stream(Box::new(Stream { dict, data }))
                } else {
                    Object::Dict(dict)
                }
            }
            other => other,
        };
        let num = u32::try_from(num).map_err(|_| PressError::Malformed("object_number"))?;
        Ok((num, obj, (start, lx.pos)))
    }

    fn read_section(
        &mut self,
        abs: usize,
        xref: &mut BTreeMap<u32, Xref>,
        budget: &mut usize,
    ) -> Result<Dict> {
        let mut lx = self.lexer(abs);
        lx.skip_ws();
        if lx.starts_with(b"xref") {
            lx.pos += 4;
            loop {
                lx.skip_ws();
                if lx.starts_with(b"trailer") {
                    lx.pos += 7;
                    break;
                }
                let first = lx.read_uint()?;
                let count = lx.read_uint()?;
                if count > (self.bytes.len().saturating_sub(lx.pos) / 18 + 1) as u64 {
                    return Err(PressError::Malformed("xref_count"));
                }
                for i in 0..count {
                    let off = lx.read_uint()?;
                    let generation = lx.read_uint()?;
                    lx.skip_ws();
                    let kind = lx.peek();
                    lx.pos += 1;
                    let num = u32::try_from(first + i).map_err(|_| PressError::Malformed("xref_number"))?;
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
            return match lx.parse_object(0, true)? {
                Object::Dict(d) => Ok(d),
                _ => Err(PressError::Malformed("trailer")),
            };
        }
        // Cross-reference stream.
        let (_num, obj, _span) = self.indirect_at(abs, xref)?;
        let Object::Stream(stream) = obj else {
            return Err(PressError::Malformed("xref_section"));
        };
        if !stream.dict.get("Type").is_some_and(|t| t.is_name("XRef")) {
            return Err(PressError::Malformed("xref_stream_type"));
        }
        let data = filters::decode_stream(&stream, self.limits, budget)?;
        let w: Vec<usize> = stream
            .dict
            .get("W")
            .and_then(Object::as_array)
            .map(|a| a.iter().map(|o| o.as_int().filter(|v| (0..=8).contains(v)).map(|v| v as usize)).collect::<Option<Vec<_>>>())
            .and_then(|o| o)
            .ok_or(PressError::Malformed("xref_w"))?;
        if w.len() != 3 || w.iter().sum::<usize>() == 0 {
            return Err(PressError::Malformed("xref_w"));
        }
        let row = w.iter().sum::<usize>();
        let size = stream.dict.get("Size").and_then(Object::as_int).filter(|v| *v >= 0).ok_or(PressError::Malformed("xref_size"))?;
        let index: Vec<(u64, u64)> = match stream.dict.get("Index").and_then(Object::as_array) {
            Some(a) => {
                let mut v = Vec::new();
                let mut it = a.iter();
                while let (Some(f), Some(c)) = (it.next(), it.next()) {
                    let (Some(f), Some(c)) = (f.as_int().filter(|x| *x >= 0), c.as_int().filter(|x| *x >= 0)) else {
                        return Err(PressError::Malformed("xref_index"));
                    };
                    v.push((f as u64, c as u64));
                }
                v
            }
            None => vec![(0, size as u64)],
        };
        let mut cursor = 0usize;
        for (first, count) in index {
            if count > (data.len().saturating_sub(cursor) / row) as u64 {
                return Err(PressError::Malformed("xref_stream_short"));
            }
            for i in 0..count {
                let mut field = [0u64; 3];
                for (k, width) in w.iter().enumerate() {
                    let mut v = 0u64;
                    for _ in 0..*width {
                        v = (v << 8) | u64::from(data[cursor]);
                        cursor += 1;
                    }
                    field[k] = v;
                }
                let kind = if w[0] == 0 { 1 } else { field[0] };
                let num = u32::try_from(first + i).map_err(|_| PressError::Malformed("xref_number"))?;
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
    let head = &bytes[..bytes.len().min(1024)];
    let base = find(head, b"%PDF-").ok_or(PressError::Malformed("header"))?;
    let version: String = bytes[base + 5..]
        .iter()
        .take(8)
        .take_while(|b| is_regular(**b))
        .map(|b| char::from(*b))
        .collect();
    let tail_start = bytes.len().saturating_sub(1536);
    let sx = rfind(&bytes[tail_start..], b"startxref").ok_or(PressError::Malformed("startxref"))? + tail_start;
    let mut lx = Lexer::new(bytes, sx + 9, limits.max_depth);
    let start = usize::try_from(lx.read_uint()?).map_err(|_| PressError::Malformed("startxref"))?;

    let mut reader = Reader { bytes, base, limits, scan_budget: limits.max_bytes.saturating_mul(2) };
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
        sections += 1;
        if sections > limits.max_xref_sections {
            return Err(PressError::InputLimit("xref_sections"));
        }
        let abs = off
            .checked_add(base)
            .filter(|a| *a < bytes.len())
            .ok_or(PressError::Malformed("xref_offset"))?;
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
                if trailer.get(key).is_none() {
                    if let Some(v) = dict.get(key) {
                        trailer.set(key, v.clone());
                    }
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
                let abs = off
                    .checked_add(base)
                    .filter(|a| *a < bytes.len())
                    .ok_or(PressError::Malformed("object_offset"))?;
                let (n, obj, span) = reader.indirect_at(abs, &xref)?;
                if n != *num {
                    return Err(PressError::Malformed("object_number_mismatch"));
                }
                objects.insert(*num, Entry { generation, obj, span: Some(span) });
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
            let n = s.dict.get("N").and_then(Object::as_int).filter(|v| *v >= 0).ok_or(PressError::Malformed("objstm_n"))? as usize;
            let first = s.dict.get("First").and_then(Object::as_int).filter(|v| *v >= 0).ok_or(PressError::Malformed("objstm_first"))? as usize;
            if n > data.len() / 2 + 1 {
                return Err(PressError::Malformed("objstm_n"));
            }
            let mut header = Lexer::new(&data, 0, limits.max_depth);
            let mut pairs = Vec::with_capacity(n.min(4096));
            for _ in 0..n {
                pairs.push((header.read_uint()?, header.read_uint()?));
            }
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
                let obj = parse_standalone(&data, at, limits.max_depth)?;
                objects.insert(onum, Entry { generation: 0, obj, span: None });
            }
        }
    }
    Ok(Parsed { version, objects, trailer, xref_sections: sections, encrypted })
}
