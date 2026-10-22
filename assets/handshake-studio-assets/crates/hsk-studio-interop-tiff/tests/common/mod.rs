//! Hand-assembled TIFF bytes for tests (no encoder involved).
#![allow(dead_code)]

pub fn p16(le: bool, v: u16) -> [u8; 2] {
    if le { v.to_le_bytes() } else { v.to_be_bytes() }
}

pub fn p32(le: bool, v: u32) -> [u8; 4] {
    if le { v.to_le_bytes() } else { v.to_be_bytes() }
}

pub fn shorts(le: bool, v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|x| p16(le, *x)).collect()
}

pub fn longs(le: bool, v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| p32(le, *x)).collect()
}

/// One IFD entry; `data` holds the value bytes already in file byte order.
#[derive(Clone)]
pub struct E {
    pub tag: u16,
    pub ty: u16,
    pub count: u32,
    pub data: Vec<u8>,
}

pub fn short(le: bool, tag: u16, v: u16) -> E {
    E { tag, ty: 3, count: 1, data: shorts(le, &[v]) }
}

pub fn long(le: bool, tag: u16, v: u32) -> E {
    E { tag, ty: 4, count: 1, data: longs(le, &[v]) }
}

pub fn shorts_e(le: bool, tag: u16, v: &[u16]) -> E {
    E { tag, ty: 3, count: v.len() as u32, data: shorts(le, v) }
}

pub fn bytes_e(tag: u16, v: &[u8]) -> E {
    E { tag, ty: 7, count: v.len() as u32, data: v.to_vec() }
}

pub struct Builder {
    pub le: bool,
    pub buf: Vec<u8>,
    last_next: Option<usize>,
}

impl Builder {
    pub fn new(le: bool) -> Self {
        let mut buf = vec![if le { b'I' } else { b'M' }; 2];
        buf.extend(p16(le, 42));
        buf.extend(p32(le, 0));
        Self { le, buf, last_next: Some(4) }
    }

    fn align(&mut self) {
        if self.buf.len() % 2 == 1 {
            self.buf.push(0);
        }
    }

    /// Appends pixel data, returns its offset.
    pub fn data(&mut self, bytes: &[u8]) -> u32 {
        self.align();
        let off = self.buf.len() as u32;
        self.buf.extend_from_slice(bytes);
        off
    }

    /// Appends an IFD (entries sorted by tag) and links it into the chain. Returns its offset.
    pub fn ifd(&mut self, mut entries: Vec<E>) -> u32 {
        entries.sort_by_key(|e| e.tag);
        self.align();
        let le = self.le;
        let off = self.buf.len() as u32;
        if let Some(p) = self.last_next {
            self.buf[p..p + 4].copy_from_slice(&p32(le, off));
        }
        let table_len = 2 + entries.len() * 12 + 4;
        let mut tail_off = off as usize + table_len;
        let mut table = Vec::new();
        let mut tail = Vec::new();
        table.extend(p16(le, entries.len() as u16));
        for e in &entries {
            table.extend(p16(le, e.tag));
            table.extend(p16(le, e.ty));
            table.extend(p32(le, e.count));
            if e.data.len() <= 4 {
                let mut v = e.data.clone();
                v.resize(4, 0);
                table.extend(v);
            } else {
                if tail_off % 2 == 1 {
                    tail.push(0);
                    tail_off += 1;
                }
                table.extend(p32(le, tail_off as u32));
                tail.extend_from_slice(&e.data);
                tail_off += e.data.len();
            }
        }
        let next_pos = off as usize + table.len();
        table.extend(p32(le, 0));
        self.buf.extend(table);
        self.buf.extend(tail);
        self.last_next = Some(next_pos);
        off
    }

    /// Single-strip page: pixels, then its IFD with `extra` entries.
    pub fn page(&mut self, w: u32, h: u32, pi: u16, bits: &[u16], pixels: &[u8], extra: Vec<E>) -> u32 {
        let le = self.le;
        let off = self.data(pixels);
        let mut e = vec![
            long(le, 256, w),
            long(le, 257, h),
            shorts_e(le, 258, bits),
            short(le, 259, 1),
            short(le, 262, pi),
            long(le, 273, off),
            short(le, 277, bits.len() as u16),
            long(le, 278, h),
            long(le, 279, pixels.len() as u32),
        ];
        e.extend(extra);
        self.ifd(e)
    }
}

/// Deterministic test image: gradients plus a hash-like term, so predictors and LZW have structure.
pub fn pattern(n: usize, seed: u32) -> Vec<u32> {
    (0..n)
        .map(|i| {
            let i = i as u32;
            (i.wrapping_mul(7).wrapping_add(seed.wrapping_mul(131))) ^ (i / 5).wrapping_mul(2_654_435_761) >> 20
        })
        .collect()
}
