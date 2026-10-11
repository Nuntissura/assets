//! Lossless JPEG (ITU-T T.81 process SOF3, Huffman, predictors 1..7, point transform) as used by
//! DNG compression 7. Written from the public T.81 text. Bounded: dimensions, tables and output
//! are capped; every read is checked; malformed streams return `InvalidContainer`.
use crate::DevelopError;

pub const MAX_COMPONENTS: usize = 4;
const LUT_BITS: u32 = 9;

pub struct LjpegImage {
    /// Samples per line (per component).
    pub width: u32,
    pub height: u32,
    pub components: usize,
    pub precision: u8,
    /// Interleaved, row-major: `width * components` values per line.
    pub samples: Vec<u16>,
}

struct Huffman {
    /// Fast path: index by the next `LUT_BITS` bits -> (code length, symbol); length 0 = slow path.
    lut: Vec<(u8, u8)>,
    min_code: [i32; 17],
    max_code: [i32; 18],
    val_ptr: [i32; 17],
    values: Vec<u8>,
}

impl Huffman {
    fn new(counts: &[u8; 16], values: &[u8]) -> Result<Self, DevelopError> {
        let total: usize = counts.iter().map(|c| usize::from(*c)).sum();
        if total != values.len() || total == 0 || total > 256 {
            return Err(DevelopError::InvalidContainer);
        }
        let mut min_code = [0i32; 17];
        let mut max_code = [-1i32; 18];
        let mut val_ptr = [0i32; 17];
        let mut lut = vec![(0u8, 0u8); 1 << LUT_BITS];
        let mut code = 0i32;
        let mut k = 0usize;
        for len in 1..=16usize {
            val_ptr[len] = k as i32;
            min_code[len] = code;
            for _ in 0..counts[len - 1] {
                if code >= (1 << len) {
                    return Err(DevelopError::InvalidContainer);
                }
                if len as u32 <= LUT_BITS {
                    let shift = LUT_BITS - len as u32;
                    let base = (code as usize) << shift;
                    for fill in 0..(1usize << shift) {
                        lut[base + fill] = (len as u8, values[k]);
                    }
                }
                code += 1;
                k += 1;
            }
            max_code[len] = if counts[len - 1] == 0 { -1 } else { code - 1 };
            code <<= 1;
        }
        Ok(Self {
            lut,
            min_code,
            max_code,
            val_ptr,
            values: values.to_vec(),
        })
    }
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u32,
    count: u32,
    /// A marker byte (not stuffing) was reached; further bits read as 1s until reset.
    marker: Option<u8>,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        Self {
            data,
            pos,
            acc: 0,
            count: 0,
            marker: None,
        }
    }

    fn fill(&mut self) {
        while self.count <= 24 {
            let byte = if self.marker.is_some() {
                0xFF
            } else {
                match self.data.get(self.pos) {
                    None => 0xFF,
                    Some(0xFF) => match self.data.get(self.pos + 1) {
                        Some(0) => {
                            self.pos += 2;
                            0xFF
                        }
                        Some(m) => {
                            self.marker = Some(*m);
                            0xFF
                        }
                        None => 0xFF,
                    },
                    Some(b) => {
                        self.pos += 1;
                        *b
                    }
                }
            };
            self.acc |= u32::from(byte) << (24 - self.count);
            self.count += 8;
        }
    }

    fn peek(&mut self, n: u32) -> u32 {
        if self.count < n {
            self.fill();
        }
        self.acc >> (32 - n)
    }

    fn skip(&mut self, n: u32) {
        self.acc <<= n;
        self.count -= n;
    }

    fn read(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let v = self.peek(n);
        self.skip(n);
        v
    }

    /// Align to the next restart marker and consume it; returns false when none is present.
    fn restart(&mut self) -> bool {
        self.acc = 0;
        self.count = 0;
        // pos points at 0xFF of a marker when `marker` was hit; otherwise scan forward.
        let mut p = self.pos;
        if self.marker.is_none() {
            while p + 1 < self.data.len() && !(self.data[p] == 0xFF && self.data[p + 1] != 0 && self.data[p + 1] != 0xFF) {
                p += 1;
            }
        }
        match (self.data.get(p), self.data.get(p + 1)) {
            (Some(0xFF), Some(m)) if (0xD0..=0xD7).contains(m) => {
                self.pos = p + 2;
                self.marker = None;
                true
            }
            _ => false,
        }
    }
}

fn decode_symbol(bits: &mut Bits<'_>, table: &Huffman) -> Result<u8, DevelopError> {
    let window = bits.peek(LUT_BITS) as usize;
    let (len, symbol) = table.lut[window];
    if len != 0 {
        bits.skip(u32::from(len));
        return Ok(symbol);
    }
    let code = bits.peek(16) as i32;
    for len in (LUT_BITS as usize + 1)..=16 {
        let candidate = code >> (16 - len);
        if table.max_code[len] >= 0 && candidate <= table.max_code[len] && candidate >= table.min_code[len] {
            bits.skip(len as u32);
            let index = (table.val_ptr[len] + candidate - table.min_code[len]) as usize;
            return table.values.get(index).copied().ok_or(DevelopError::InvalidContainer);
        }
    }
    Err(DevelopError::InvalidContainer)
}

fn be16(data: &[u8], at: usize) -> Result<usize, DevelopError> {
    match data.get(at..at + 2) {
        Some(b) => Ok(usize::from(u16::from_be_bytes([b[0], b[1]]))),
        None => Err(DevelopError::InvalidContainer),
    }
}

/// Decode one SOF3 stream. `max_samples` bounds the output allocation.
pub fn decode(data: &[u8], max_samples: usize) -> Result<LjpegImage, DevelopError> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return Err(DevelopError::InvalidContainer);
    }
    let mut pos = 2usize;
    let mut tables: [Option<Huffman>; 4] = [None, None, None, None];
    let mut frame: Option<(u8, u32, u32, usize)> = None; // precision, lines, samples/line, components
    let mut restart_interval = 0usize;
    loop {
        if pos + 4 > data.len() || data[pos] != 0xFF {
            return Err(DevelopError::InvalidContainer);
        }
        let marker = data[pos + 1];
        if marker == 0xFF {
            pos += 1;
            continue;
        }
        let len = be16(data, pos + 2)?;
        if len < 2 || pos + 2 + len > data.len() {
            return Err(DevelopError::InvalidContainer);
        }
        let seg = &data[pos + 4..pos + 2 + len];
        match marker {
            0xC3 => {
                // SOF3: P, Y, X, Nf, then (C, HV, Tq) per component.
                if seg.len() < 6 {
                    return Err(DevelopError::InvalidContainer);
                }
                let precision = seg[0];
                let lines = u32::from(u16::from_be_bytes([seg[1], seg[2]]));
                let samples = u32::from(u16::from_be_bytes([seg[3], seg[4]]));
                let n = usize::from(seg[5]);
                if !(2..=16).contains(&precision)
                    || lines == 0
                    || samples == 0
                    || n == 0
                    || n > MAX_COMPONENTS
                    || seg.len() != 6 + 3 * n
                {
                    return Err(DevelopError::InvalidContainer);
                }
                if (0..n).any(|i| seg[7 + 3 * i] != 0x11) {
                    return Err(DevelopError::UnsupportedCompression); // subsampled components
                }
                frame = Some((precision, lines, samples, n));
            }
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                return Err(DevelopError::UnsupportedCompression); // not lossless Huffman
            }
            0xC4 => {
                let mut at = 0usize;
                while at < seg.len() {
                    if at + 17 > seg.len() {
                        return Err(DevelopError::InvalidContainer);
                    }
                    let (class, id) = (seg[at] >> 4, usize::from(seg[at] & 15));
                    let mut counts = [0u8; 16];
                    counts.copy_from_slice(&seg[at + 1..at + 17]);
                    let total: usize = counts.iter().map(|c| usize::from(*c)).sum();
                    if class != 0 || id > 3 || at + 17 + total > seg.len() {
                        return Err(DevelopError::InvalidContainer);
                    }
                    tables[id] = Some(Huffman::new(&counts, &seg[at + 17..at + 17 + total])?);
                    at += 17 + total;
                }
            }
            0xDD => {
                if seg.len() != 2 {
                    return Err(DevelopError::InvalidContainer);
                }
                restart_interval = usize::from(u16::from_be_bytes([seg[0], seg[1]]));
            }
            0xDA => {
                let (precision, lines, per_line, n) = frame.ok_or(DevelopError::InvalidContainer)?;
                if seg.is_empty() || usize::from(seg[0]) != n || seg.len() != 1 + 2 * n + 3 {
                    return Err(DevelopError::UnsupportedCompression); // multi-scan
                }
                let selectors: Vec<usize> = (0..n).map(|i| usize::from(seg[2 + 2 * i] >> 4)).collect();
                let predictor = seg[1 + 2 * n];
                let point_transform = u32::from(seg[3 + 2 * n] & 15);
                if !(1..=7).contains(&predictor) || seg[2 + 2 * n] != 0 || point_transform >= u32::from(precision) {
                    return Err(DevelopError::InvalidContainer);
                }
                let total = (lines as usize)
                    .checked_mul(per_line as usize)
                    .and_then(|v| v.checked_mul(n))
                    .ok_or(DevelopError::InvalidContainer)?;
                if total > max_samples {
                    return Err(DevelopError::InvalidDimensions);
                }
                let used: Vec<&Huffman> = selectors
                    .iter()
                    .map(|s| tables.get(*s).and_then(|t| t.as_ref()).ok_or(DevelopError::InvalidContainer))
                    .collect::<Result<_, _>>()?;
                let samples = decode_scan(
                    data,
                    pos + 2 + len,
                    &Scan {
                        tables: used,
                        precision,
                        lines: lines as usize,
                        per_line: per_line as usize,
                        predictor,
                        point_transform,
                        restart_interval,
                    },
                )?;
                return Ok(LjpegImage {
                    width: per_line,
                    height: lines,
                    components: n,
                    precision,
                    samples,
                });
            }
            0xD9 => return Err(DevelopError::InvalidContainer),
            _ => {} // APPn, COM and others are skipped
        }
        pos += 2 + len;
    }
}

struct Scan<'a> {
    tables: Vec<&'a Huffman>,
    precision: u8,
    lines: usize,
    per_line: usize,
    predictor: u8,
    point_transform: u32,
    restart_interval: usize,
}

fn decode_scan(data: &[u8], start: usize, scan: &Scan<'_>) -> Result<Vec<u16>, DevelopError> {
    let n = scan.tables.len();
    let (lines, per_line, pt) = (scan.lines, scan.per_line, scan.point_transform);
    let ri = scan.restart_interval;
    let row_len = per_line * n;
    if ri != 0 && !ri.is_multiple_of(per_line) {
        return Err(DevelopError::UnsupportedCompression); // restart inside a line: not verified
    }
    let mut out = vec![0u16; lines * row_len];
    let mut bits = Bits::new(data, start);
    let initial = 1i64 << (u32::from(scan.precision) - pt - 1);
    let mut mcu = 0usize;
    for y in 0..lines {
        // First line of the scan or of a restart interval: horizontal prediction only.
        let first_row = y == 0 || (ri != 0 && (y * per_line).is_multiple_of(ri));
        for x in 0..per_line {
            if ri != 0 && mcu != 0 && mcu.is_multiple_of(ri) && !bits.restart() {
                return Err(DevelopError::InvalidContainer);
            }
            for c in 0..n {
                let s = decode_symbol(&mut bits, scan.tables[c])?;
                let diff: i64 = match s {
                    0 => 0,
                    16 => 32768,
                    1..=15 => {
                        let raw = i64::from(bits.read(u32::from(s)));
                        if raw < (1i64 << (s - 1)) {
                            raw - (1i64 << s) + 1
                        } else {
                            raw
                        }
                    }
                    _ => return Err(DevelopError::InvalidContainer),
                };
                let at = y * row_len + x * n + c;
                let sample = |index: usize| i64::from(out[index]) >> pt;
                let prediction = if first_row {
                    if x == 0 { initial } else { sample(at - n) }
                } else if x == 0 {
                    sample(at - row_len)
                } else {
                    let (left, above, corner) = (sample(at - n), sample(at - row_len), sample(at - row_len - n));
                    match scan.predictor {
                        1 => left,
                        2 => above,
                        3 => corner,
                        4 => left + above - corner,
                        5 => left + ((above - corner) >> 1),
                        6 => above + ((left - corner) >> 1),
                        _ => (left + above) >> 1,
                    }
                };
                let value = (prediction + diff) & 0xFFFF;
                out[at] = ((value as u32) << pt) as u16;
            }
            mcu += 1;
        }
    }
    Ok(out)
}
