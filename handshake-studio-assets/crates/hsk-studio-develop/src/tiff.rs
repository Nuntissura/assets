//! Bounded classic-TIFF IFD reader (TIFF 6.0 / TIFF-EP container used by DNG). Every offset and
//! count is checked against the supplied bytes; nothing allocates from an unchecked count.
use crate::DevelopError;

pub const MAX_ENTRIES_PER_IFD: usize = 1024;
pub const MAX_IFDS: usize = 64;
const MAX_SUBIFD_DEPTH: usize = 3;

#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub tag: u16,
    pub typ: u16,
    pub count: u32,
    /// Offset of the 4-byte value/offset field inside the file.
    field: usize,
}

#[derive(Clone, Debug)]
pub struct Ifd {
    pub entries: Vec<Entry>,
    pub next: u32,
}

impl Ifd {
    pub fn find(&self, tag: u16) -> Option<&Entry> {
        self.entries.iter().find(|e| e.tag == tag)
    }
}

pub struct Tiff<'a> {
    data: &'a [u8],
    big_endian: bool,
}

const fn type_size(typ: u16) -> Option<usize> {
    match typ {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 | 13 => Some(4),
        5 | 10 | 12 => Some(8),
        _ => None,
    }
}

impl<'a> Tiff<'a> {
    /// Validate the header; returns the reader and the first IFD offset.
    pub fn parse(data: &'a [u8]) -> Result<(Self, u32), DevelopError> {
        if data.len() < 8 {
            return Err(DevelopError::InvalidContainer);
        }
        let big_endian = match &data[0..2] {
            b"II" => false,
            b"MM" => true,
            _ => return Err(DevelopError::InvalidContainer),
        };
        let tiff = Self { data, big_endian };
        match tiff.u16(2)? {
            42 => {}
            43 => return Err(DevelopError::UnsupportedContainer), // BigTIFF
            _ => return Err(DevelopError::InvalidContainer),
        }
        let first = tiff.u32(4)?;
        Ok((tiff, first))
    }

    fn slice(&self, offset: usize, len: usize) -> Result<&'a [u8], DevelopError> {
        let end = offset.checked_add(len).ok_or(DevelopError::InvalidContainer)?;
        self.data.get(offset..end).ok_or(DevelopError::InvalidContainer)
    }

    pub fn u16(&self, offset: usize) -> Result<u16, DevelopError> {
        let b: [u8; 2] = self.slice(offset, 2)?.try_into().unwrap();
        Ok(if self.big_endian {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        })
    }

    pub fn u32(&self, offset: usize) -> Result<u32, DevelopError> {
        let b: [u8; 4] = self.slice(offset, 4)?.try_into().unwrap();
        Ok(if self.big_endian {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    }

    pub fn is_big_endian(&self) -> bool {
        self.big_endian
    }

    pub fn ifd(&self, offset: u32) -> Result<Ifd, DevelopError> {
        let base = offset as usize;
        let count = usize::from(self.u16(base)?);
        if count > MAX_ENTRIES_PER_IFD {
            return Err(DevelopError::InvalidContainer);
        }
        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let at = base + 2 + i * 12;
            entries.push(Entry {
                tag: self.u16(at)?,
                typ: self.u16(at + 2)?,
                count: self.u32(at + 4)?,
                field: at + 8,
            });
        }
        let next = self.u32(base + 2 + count * 12)?;
        Ok(Ifd { entries, next })
    }

    /// All IFDs reachable from `first` through next-pointers and SubIFDs (tag 330), without
    /// revisiting an offset.
    pub fn walk(&self, first: u32) -> Result<Vec<Ifd>, DevelopError> {
        let mut seen = Vec::new();
        let mut out = Vec::new();
        self.walk_chain(first, 0, &mut seen, &mut out)?;
        Ok(out)
    }

    fn walk_chain(
        &self,
        mut offset: u32,
        depth: usize,
        seen: &mut Vec<u32>,
        out: &mut Vec<Ifd>,
    ) -> Result<(), DevelopError> {
        while offset != 0 {
            if seen.contains(&offset) || out.len() >= MAX_IFDS {
                return Err(DevelopError::InvalidContainer);
            }
            seen.push(offset);
            let ifd = self.ifd(offset)?;
            let next = ifd.next;
            let subs = match ifd.find(330) {
                Some(e) if depth < MAX_SUBIFD_DEPTH => self.u32s(e)?,
                _ => Vec::new(),
            };
            out.push(ifd);
            for sub in subs {
                self.walk_chain(sub, depth + 1, seen, out)?;
            }
            offset = next;
        }
        Ok(())
    }

    fn data_of(&self, e: &Entry) -> Result<&'a [u8], DevelopError> {
        let size = type_size(e.typ).ok_or(DevelopError::InvalidContainer)?;
        let len = size
            .checked_mul(e.count as usize)
            .ok_or(DevelopError::InvalidContainer)?;
        if len <= 4 {
            self.slice(e.field, len)
        } else {
            let offset = self.u32(e.field)? as usize;
            self.slice(offset, len)
        }
    }

    pub fn bytes(&self, e: &Entry) -> Result<&'a [u8], DevelopError> {
        match e.typ {
            1 | 2 | 6 | 7 => self.data_of(e),
            _ => Err(DevelopError::InvalidContainer),
        }
    }

    pub fn ascii(&self, e: &Entry) -> Result<String, DevelopError> {
        let raw = self.bytes(e)?;
        let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
        Ok(String::from_utf8_lossy(&raw[..end]).into_owned())
    }

    /// Unsigned integer values (BYTE, SHORT, LONG, IFD).
    pub fn u32s(&self, e: &Entry) -> Result<Vec<u32>, DevelopError> {
        let raw = self.data_of(e)?;
        let size = type_size(e.typ).ok_or(DevelopError::InvalidContainer)?;
        let mut out = Vec::with_capacity(e.count as usize);
        for chunk in raw.chunks_exact(size) {
            out.push(match e.typ {
                1 | 7 => u32::from(chunk[0]),
                3 => u32::from(if self.big_endian {
                    u16::from_be_bytes([chunk[0], chunk[1]])
                } else {
                    u16::from_le_bytes([chunk[0], chunk[1]])
                }),
                4 | 13 => {
                    let b = [chunk[0], chunk[1], chunk[2], chunk[3]];
                    if self.big_endian {
                        u32::from_be_bytes(b)
                    } else {
                        u32::from_le_bytes(b)
                    }
                }
                _ => return Err(DevelopError::InvalidContainer),
            });
        }
        Ok(out)
    }

    /// Any numeric type as binary64 (RATIONAL/SRATIONAL divide; zero denominator refuses).
    pub fn f64s(&self, e: &Entry) -> Result<Vec<f64>, DevelopError> {
        let raw = self.data_of(e)?;
        let size = type_size(e.typ).ok_or(DevelopError::InvalidContainer)?;
        let be = self.big_endian;
        let rd32 = |b: &[u8]| {
            let a = [b[0], b[1], b[2], b[3]];
            if be { u32::from_be_bytes(a) } else { u32::from_le_bytes(a) }
        };
        let mut out = Vec::with_capacity(e.count as usize);
        for chunk in raw.chunks_exact(size) {
            let value = match e.typ {
                1 | 7 => f64::from(chunk[0]),
                6 => f64::from(chunk[0] as i8),
                3 => f64::from(if be {
                    u16::from_be_bytes([chunk[0], chunk[1]])
                } else {
                    u16::from_le_bytes([chunk[0], chunk[1]])
                }),
                8 => f64::from(if be {
                    i16::from_be_bytes([chunk[0], chunk[1]])
                } else {
                    i16::from_le_bytes([chunk[0], chunk[1]])
                }),
                4 | 13 => f64::from(rd32(chunk)),
                9 => f64::from(rd32(chunk) as i32),
                5 | 10 => {
                    let den = rd32(&chunk[4..8]);
                    if den == 0 {
                        return Err(DevelopError::InvalidContainer);
                    }
                    if e.typ == 5 {
                        f64::from(rd32(&chunk[0..4])) / f64::from(den)
                    } else {
                        f64::from(rd32(&chunk[0..4]) as i32) / f64::from(den as i32)
                    }
                }
                11 => f64::from(f32::from_bits(rd32(chunk))),
                12 => {
                    let mut a = [0u8; 8];
                    a.copy_from_slice(chunk);
                    if be { f64::from_be_bytes(a) } else { f64::from_le_bytes(a) }
                }
                _ => return Err(DevelopError::InvalidContainer),
            };
            if !value.is_finite() {
                return Err(DevelopError::NonFinite);
            }
            out.push(value);
        }
        Ok(out)
    }

    /// Raw bytes of a region of the file (strip/tile payloads).
    pub fn region(&self, offset: u64, len: u64) -> Result<&'a [u8], DevelopError> {
        let offset = usize::try_from(offset).map_err(|_| DevelopError::InvalidContainer)?;
        let len = usize::try_from(len).map_err(|_| DevelopError::InvalidContainer)?;
        self.slice(offset, len)
    }
}
