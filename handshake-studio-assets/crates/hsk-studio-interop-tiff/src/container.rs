//! Bounded TIFF container facade for engines that decode their own pixel data (DNG, TIFF/EP RAW):
//! header, IFD walk (next-pointers plus SubIFDs, tag 330), raw entries, typed value accessors and
//! byte regions. No pixel decode and no compression handling; every range is checked against the
//! supplied bytes and every count against the caller's [`Limits`] before anything is allocated.
use crate::error::{to_usize, Result, TiffError};
use crate::limits::Limits;
use crate::model::ByteOrder;
use crate::scan::{header, read_ifd, type_size, Cur};
use std::collections::HashSet;

const TAG_SUB_IFDS: u16 = 330;
/// SubIFD nesting below the top chain that the walk follows.
const MAX_SUBIFD_DEPTH: usize = 3;

/// One IFD entry. `value_offset`/`value_len` locate the value bytes in the file (inline values point
/// into the entry itself); an entry of an unknown field type has an empty range and every accessor
/// refuses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IfdEntry {
    pub tag: u16,
    pub field_type: u16,
    pub count: u64,
    pub value_offset: u64,
    pub value_len: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ifd {
    pub offset: u64,
    pub entries: Vec<IfdEntry>,
    pub next: u64,
}

impl Ifd {
    pub fn find(&self, tag: u16) -> Option<&IfdEntry> {
        self.entries.iter().find(|e| e.tag == tag)
    }
}

/// A validated TIFF header over borrowed bytes.
pub struct Container<'a> {
    cur: Cur<'a>,
    big: bool,
    order: ByteOrder,
    first_ifd: u64,
    limits: Limits,
}

impl<'a> Container<'a> {
    /// Validate the header (classic or BigTIFF, both byte orders) and the input size.
    pub fn parse(bytes: &'a [u8], limits: &Limits) -> Result<Self> {
        if u64::try_from(bytes.len()).map_err(|_| TiffError::DimsOverflow)? > limits.max_input_bytes {
            return Err(TiffError::Limit("limit_input_bytes"));
        }
        let hdr = header(bytes)?;
        Ok(Self {
            cur: Cur { b: bytes, le: hdr.order == ByteOrder::Little },
            big: hdr.big,
            order: hdr.order,
            first_ifd: hdr.first_ifd,
            limits: limits.clone(),
        })
    }

    pub fn byte_order(&self) -> ByteOrder {
        self.order
    }

    pub fn is_big_tiff(&self) -> bool {
        self.big
    }

    pub fn first_ifd_offset(&self) -> u64 {
        self.first_ifd
    }

    /// One IFD at `offset`.
    pub fn ifd(&self, offset: u64) -> Result<Ifd> {
        let (entries, next) = read_ifd(&self.cur, self.big, offset, &self.limits, true)?;
        let entries = entries
            .into_iter()
            .map(|e| IfdEntry {
                tag: e.tag,
                field_type: e.ty,
                count: e.count,
                value_offset: e.data_pos,
                value_len: e.data_len,
            })
            .collect();
        Ok(Ifd { offset, entries, next })
    }

    /// Every IFD reachable from the first one through next-pointers and SubIFDs (tag 330, depth
    /// bounded), in walk order, without revisiting an offset and within `Limits::max_pages`.
    pub fn walk(&self) -> Result<Vec<Ifd>> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        self.walk_chain(self.first_ifd, 0, &mut seen, &mut out)?;
        Ok(out)
    }

    fn walk_chain(&self, mut offset: u64, depth: usize, seen: &mut HashSet<u64>, out: &mut Vec<Ifd>) -> Result<()> {
        let max = to_usize(u64::from(self.limits.max_pages))?;
        while offset != 0 {
            if !seen.insert(offset) {
                return Err(TiffError::IfdCycle);
            }
            if out.len() >= max {
                return Err(TiffError::Limit("limit_pages"));
            }
            let ifd = self.ifd(offset)?;
            let next = ifd.next;
            let subs = match ifd.find(TAG_SUB_IFDS) {
                Some(e) if depth < MAX_SUBIFD_DEPTH => self.uints(e)?,
                _ => Vec::new(),
            };
            out.push(ifd);
            let child = depth.checked_add(1).ok_or(TiffError::DimsOverflow)?;
            for sub in subs {
                self.walk_chain(sub, child, seen, out)?;
            }
            offset = next;
        }
        Ok(())
    }

    /// Bytes `[offset, offset + len)` of the input (strip/tile payloads, MakerNote bodies).
    pub fn region(&self, offset: u64, len: u64) -> Result<&'a [u8]> {
        self.cur.slice(offset, len)
    }

    fn value(&self, e: &IfdEntry) -> Result<(&'a [u8], usize)> {
        let size = type_size(e.field_type).ok_or(TiffError::Corrupt("bad_field_type"))?;
        if e.value_len > self.limits.max_tag_bytes {
            return Err(TiffError::Limit("limit_tag_bytes"));
        }
        Ok((self.cur.slice(e.value_offset, e.value_len)?, to_usize(size)?))
    }

    /// Raw value bytes of a BYTE, ASCII, SBYTE or UNDEFINED entry.
    pub fn bytes(&self, e: &IfdEntry) -> Result<&'a [u8]> {
        match e.field_type {
            1 | 2 | 6 | 7 => Ok(self.value(e)?.0),
            _ => Err(TiffError::Corrupt("bad_field_type")),
        }
    }

    /// ASCII text up to the first NUL (lossy UTF-8).
    pub fn ascii(&self, e: &IfdEntry) -> Result<String> {
        let raw = self.bytes(e)?;
        let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
        Ok(String::from_utf8_lossy(raw.get(..end).unwrap_or(raw)).into_owned())
    }

    /// Unsigned integers of a BYTE, UNDEFINED, SHORT, LONG, IFD, LONG8 or IFD8 entry.
    pub fn uints(&self, e: &IfdEntry) -> Result<Vec<u64>> {
        let (raw, size) = self.value(e)?;
        match e.field_type {
            1 | 7 | 3 | 4 | 13 | 16 | 18 => {}
            _ => return Err(TiffError::Corrupt("bad_field_type")),
        }
        let le = self.cur.le;
        let mut out = Vec::with_capacity(to_usize(e.count)?);
        for chunk in raw.chunks_exact(size) {
            let c = Cur { b: chunk, le };
            out.push(match size {
                1 => u64::from(c.slice(0, 1)?.first().copied().unwrap_or(0)),
                2 => u64::from(c.u16(0)?),
                4 => u64::from(c.u32(0)?),
                _ => c.u64(0)?,
            });
        }
        Ok(out)
    }

    /// Numeric entries as binary64: RATIONAL/SRATIONAL divide (a zero denominator is refused), floats
    /// must be finite (`non_finite`).
    pub fn f64s(&self, e: &IfdEntry) -> Result<Vec<f64>> {
        let (raw, size) = self.value(e)?;
        let le = self.cur.le;
        let mut out = Vec::with_capacity(to_usize(e.count)?);
        for chunk in raw.chunks_exact(size) {
            let c = Cur { b: chunk, le };
            let value = match e.field_type {
                1 | 7 => f64::from(c.slice(0, 1)?.first().copied().unwrap_or(0)),
                6 => f64::from(i8::from_ne_bytes([c.slice(0, 1)?.first().copied().unwrap_or(0)])),
                3 => f64::from(c.u16(0)?),
                8 => f64::from(i16::from_ne_bytes(c.u16(0)?.to_ne_bytes())),
                4 | 13 => f64::from(c.u32(0)?),
                9 => f64::from(i32::from_ne_bytes(c.u32(0)?.to_ne_bytes())),
                5 | 10 => {
                    let (num, den) = (c.u32(0)?, c.u32(4)?);
                    if den == 0 {
                        return Err(TiffError::Corrupt("zero_denominator"));
                    }
                    if e.field_type == 5 {
                        f64::from(num) / f64::from(den)
                    } else {
                        f64::from(i32::from_ne_bytes(num.to_ne_bytes())) / f64::from(i32::from_ne_bytes(den.to_ne_bytes()))
                    }
                }
                11 => f64::from(f32::from_bits(c.u32(0)?)),
                12 => f64::from_bits(c.u64(0)?),
                _ => return Err(TiffError::Corrupt("bad_field_type")),
            };
            if !value.is_finite() {
                return Err(TiffError::Corrupt("non_finite"));
            }
            out.push(value);
        }
        Ok(out)
    }
}
