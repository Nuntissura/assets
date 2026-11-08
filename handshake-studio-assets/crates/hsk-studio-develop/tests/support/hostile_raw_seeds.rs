//! Seed files for the camera RAW mutation sweep (lane SMB-PARSER-HOSTILE-1, item 6): one or more
//! small synthetic RAW files per container format, each accepted by
//! `hsk_studio_develop::decode_container`. Included by `tests/hostile_mutation_raw.rs` through
//! `#[path = "support/hostile_raw_seeds.rs"] mod hostile_raw_seeds;`.
//!
//! The file constructors and the helpers they need are copied verbatim, one private inline module
//! per format, from `tests/develop_dng.rs`, `develop_nef.rs`, `develop_nef_compressed.rs`,
//! `develop_arw.rs`, `develop_cr2.rs`, `develop_raf.rs`, `develop_cr3.rs`, `develop_rw2.rs` and
//! `develop_orf.rs`; the originals are unchanged. Each module's `seeds()` calls its constructor with
//! the arguments of an existing test in the same original file that asserts the decode succeeds
//! (the test is named above each call). Everything is built in memory, deterministically.
#![allow(dead_code, unused_imports)]

/// Every seed as (name, file bytes).
pub fn seeds() -> Vec<(&'static str, Vec<u8>)> {
    let mut out = Vec::new();
    out.extend(dng::seeds());
    out.extend(nef::seeds());
    out.extend(nef_compressed::seeds());
    out.extend(arw::seeds());
    out.extend(cr2::seeds());
    out.extend(raf::seeds());
    out.extend(cr3::seeds());
    out.extend(rw2::seeds());
    out.extend(orf::seeds());
    out
}

// ---- DNG (copied from tests/develop_dng.rs) -----------------------------------------------------
mod dng {
    use hsk_studio_develop::color::{self, Mat3};
    use hsk_studio_develop::*;

    pub(super) fn seeds() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            // dng_uncompressed_end_to_end: `decode_container(&build(&base()).bytes).unwrap()`.
            ("dng_uncompressed_strips", build(&base()).bytes),
            // dng_lossless_jpeg_declared_frame_cannot_outgrow_the_payload:
            // `assert!(decode_container(&tiled.bytes).is_ok())`.
            ("dng_lossless_jpeg_tiles", build(&Spec { tiles: Some((4, 4)), compression: 7, ..base() }).bytes),
        ]
    }

    /// sRGB -> XYZ D50 colorants (ICC sRGB profile / Lindbloom D50 table), literal, independent of the crate.
    const SRGB_D50: Mat3 = [
        0.4360747, 0.3850649, 0.1430804, 0.2225045, 0.7168786, 0.0606169, 0.0139322, 0.0971045,
        0.7141733,
    ];

    // ---- minimal TIFF/DNG writer -------------------------------------------------------------
    #[derive(Clone)]
    struct T {
        tag: u16,
        typ: u16,
        count: u32,
        data: Vec<u8>,
    }

    struct Enc {
        be: bool,
    }

    impl Enc {
        fn u16(&self, v: u16) -> [u8; 2] {
            if self.be { v.to_be_bytes() } else { v.to_le_bytes() }
        }
        fn u32(&self, v: u32) -> [u8; 4] {
            if self.be { v.to_be_bytes() } else { v.to_le_bytes() }
        }
        fn byte(&self, tag: u16, v: &[u8]) -> T {
            T { tag, typ: 1, count: v.len() as u32, data: v.to_vec() }
        }
        fn ascii(&self, tag: u16, s: &str) -> T {
            let mut d = s.as_bytes().to_vec();
            d.push(0);
            T { tag, typ: 2, count: d.len() as u32, data: d }
        }
        fn short(&self, tag: u16, v: &[u16]) -> T {
            T { tag, typ: 3, count: v.len() as u32, data: v.iter().flat_map(|x| self.u16(*x)).collect() }
        }
        fn long(&self, tag: u16, v: &[u32]) -> T {
            T { tag, typ: 4, count: v.len() as u32, data: v.iter().flat_map(|x| self.u32(*x)).collect() }
        }
        fn rational(&self, tag: u16, v: &[f64], den: u32) -> T {
            let data = v
                .iter()
                .flat_map(|x| {
                    let n = (x * f64::from(den)).round() as u32;
                    [self.u32(n), self.u32(den)].concat()
                })
                .collect();
            T { tag, typ: 5, count: v.len() as u32, data }
        }
        fn srational(&self, tag: u16, v: &[f64], den: i32) -> T {
            let data = v
                .iter()
                .flat_map(|x| {
                    let n = (x * f64::from(den)).round() as i32;
                    [self.u32(n as u32), self.u32(den as u32)].concat()
                })
                .collect();
            T { tag, typ: 10, count: v.len() as u32, data }
        }
        fn double(&self, tag: u16, v: &[f64]) -> T {
            let data = v
                .iter()
                .flat_map(|x| if self.be { x.to_be_bytes() } else { x.to_le_bytes() })
                .collect();
            T { tag, typ: 12, count: v.len() as u32, data }
        }
        fn ifd_len(&self, tags: &[T]) -> usize {
            2 + 12 * tags.len() + 4 + tags.iter().map(|t| if t.data.len() > 4 { t.data.len() + (t.data.len() & 1) } else { 0 }).sum::<usize>()
        }
        fn ifd(&self, at: usize, tags: &[T], next: u32) -> Vec<u8> {
            let mut sorted = tags.to_vec();
            sorted.sort_by_key(|t| t.tag);
            let mut out = Vec::new();
            out.extend_from_slice(&self.u16(sorted.len() as u16));
            let mut extra: Vec<u8> = Vec::new();
            let extra_base = at + 2 + 12 * sorted.len() + 4;
            for t in &sorted {
                out.extend_from_slice(&self.u16(t.tag));
                out.extend_from_slice(&self.u16(t.typ));
                out.extend_from_slice(&self.u32(t.count));
                if t.data.len() <= 4 {
                    let mut field = t.data.clone();
                    field.resize(4, 0);
                    out.extend_from_slice(&field);
                } else {
                    out.extend_from_slice(&self.u32((extra_base + extra.len()) as u32));
                    extra.extend_from_slice(&t.data);
                    if t.data.len() & 1 == 1 {
                        extra.push(0);
                    }
                }
            }
            out.extend_from_slice(&self.u32(next));
            out.extend_from_slice(&extra);
            out
        }
    }

    #[derive(Clone)]
    struct Spec {
        be: bool,
        width: u32,
        height: u32,
        samples: Vec<u16>,
        black_dim: (u16, u16),
        black: Vec<f64>,
        white: Option<u32>,
        cfa: [u8; 4],
        neutral: Option<[f64; 3]>,
        forward: [Option<Mat3>; 2],
        color: [Option<Mat3>; 2],
        illuminant: [Option<u16>; 2],
        tiles: Option<(u32, u32)>,
        active: Option<[u32; 4]>,
        crop: Option<([u16; 2], [u16; 2])>,
        compression: u16,
        bits: u16,
        photometric: u16,
        dng_version: Option<[u8; 4]>,
        backward: [u8; 4],
        lin_table: Option<Vec<u16>>,
        extra: Vec<(u16, u32)>,
        baseline: Option<f64>,
        analog: Option<[f64; 3]>,
        calib: [Option<Mat3>; 2],
        delta_h: Option<Vec<f64>>,
        delta_v: Option<Vec<f64>>,
        /// Encode the BlackLevelDelta arrays as DOUBLE instead of SRATIONAL (hostile magnitudes).
        delta_double: bool,
        lj: (usize, u8, u8, u32, usize),
        /// Lossless-JPEG frame shape per block as (samples per JPEG row, JPEG rows); `None` = the
        /// block's own (width, height). The samples stay in block raster order.
        lj_shape: Option<(usize, usize)>,
    }

    struct Built {
        bytes: Vec<u8>,
        ifd0_at: usize,
        ifd0_entries: usize,
        raw_at: usize,
    }

    fn inverse(m: &Mat3) -> Mat3 {
        color::invert(m).unwrap()
    }

    fn base() -> Spec {
        Spec {
            be: false,
            width: 4,
            height: 4,
            samples: matrix_samples(),
            black_dim: (1, 1),
            black: vec![0.0],
            white: Some(64),
            cfa: [0, 1, 1, 2],
            neutral: Some([1.0, 1.0, 1.0]),
            forward: [Some(SRGB_D50), None],
            color: [Some(inverse(&SRGB_D50)), None],
            illuminant: [Some(23), None],
            tiles: None,
            active: None,
            crop: None,
            compression: 1,
            bits: 16,
            photometric: 32803,
            dng_version: Some([1, 4, 0, 0]),
            backward: [1, 1, 0, 0],
            lin_table: None,
            extra: vec![],
            baseline: None,
            analog: None,
            calib: [None, None],
            delta_h: None,
            delta_v: None,
            delta_double: false,
            lj: (2, 1, 16, 0, 0),
            lj_shape: None,
        }
    }

    /// RGGB 4x4 (black 0, white 64) with hand-computed demosaic values (see develop_recipe.rs).
    fn matrix_samples() -> Vec<u16> {
        vec![8, 4, 16, 12, 20, 40, 28, 48, 24, 36, 32, 44, 52, 56, 60, 2]
    }

    fn build(s: &Spec) -> Built {
        let enc = Enc { be: s.be };
        let (w, h) = (s.width as usize, s.height as usize);
        let sample_bytes = |v: u16| -> Vec<u8> {
            if s.bits == 8 { vec![v as u8] } else { enc.u16(v).to_vec() }
        };
        // Per-block sample arrays (w, h, values) then their on-disk encoding.
        let raw_blocks: Vec<(usize, usize, Vec<u16>)> = match s.tiles {
            Some((tw, th)) => {
                let (tw, th) = (tw as usize, th as usize);
                let mut out = Vec::new();
                for ty in 0..h.div_ceil(th) {
                    for tx in 0..w.div_ceil(tw) {
                        let mut b = Vec::new();
                        for y in 0..th {
                            for x in 0..tw {
                                let (sx, sy) = (tx * tw + x, ty * th + y);
                                b.push(if sx < w && sy < h { s.samples[sy * w + sx] } else { 0 });
                            }
                        }
                        out.push((tw, th, b));
                    }
                }
                out
            }
            None => (0..h / 2).map(|strip| (w, 2, s.samples[strip * 2 * w..(strip + 1) * 2 * w].to_vec())).collect(),
        };
        let blocks: Vec<Vec<u8>> = raw_blocks
            .iter()
            .map(|(bw, bh, v)| {
                if s.compression == 7 {
                    let (jw, jh) = s.lj_shape.unwrap_or((*bw, *bh));
                    ljpeg_encode(v, jw, jh, s.lj)
                } else {
                    v.iter().flat_map(|x| sample_bytes(*x)).collect()
                }
            })
            .collect();
        let make = |raw_at: u32, offsets: &[u32]| -> (Vec<T>, Vec<T>) {
            let mut t0 = vec![enc.short(254, &[1]), enc.ascii(50708, "Studio Test"), enc.long(330, &[raw_at]), enc.byte(50707, &s.backward)];
            if let Some(v) = s.dng_version {
                t0.push(enc.byte(50706, &v));
            }
            for (i, m) in s.color.iter().enumerate() {
                if let Some(m) = m {
                    t0.push(enc.srational(50721 + i as u16, m, 10_000_000));
                }
            }
            for (i, m) in s.forward.iter().enumerate() {
                if let Some(m) = m {
                    t0.push(enc.srational(50964 + i as u16, m, 10_000_000));
                }
            }
            for (i, c) in s.illuminant.iter().enumerate() {
                if let Some(c) = c {
                    t0.push(enc.short(50778 + i as u16, &[*c]));
                }
            }
            if let Some(ab) = s.analog {
                t0.push(enc.rational(50727, &ab, 10_000_000));
            }
            for (i, m) in s.calib.iter().enumerate() {
                if let Some(m) = m {
                    t0.push(enc.srational(50723 + i as u16, m, 10_000_000));
                }
            }
            if let Some(b) = s.baseline {
                t0.push(enc.srational(50730, &[b], 10_000));
            }
            if let Some(n) = s.neutral {
                t0.push(enc.rational(50728, &n, 10_000_000));
            }
            let counts: Vec<u32> = blocks.iter().map(|b| b.len() as u32).collect();
            let mut t1 = vec![
                enc.short(254, &[0]),
                enc.long(256, &[s.width]),
                enc.long(257, &[s.height]),
                enc.short(258, &[s.bits]),
                enc.short(259, &[s.compression]),
                enc.short(262, &[s.photometric]),
                enc.short(277, &[1]),
                enc.short(33421, &[2, 2]),
                enc.byte(33422, &s.cfa),
                enc.byte(50710, &[0, 1, 2]),
                enc.short(50711, &[1]),
                enc.short(50713, &[s.black_dim.0, s.black_dim.1]),
                enc.rational(50714, &s.black, 1),
            ];
            if let Some(wl) = s.white {
                t1.push(enc.long(50717, &[wl]));
            }
            match s.tiles {
                Some((tw, th)) => {
                    t1.extend([enc.long(322, &[tw]), enc.long(323, &[th]), enc.long(324, offsets), enc.long(325, &counts)]);
                }
                None => {
                    t1.extend([enc.long(273, offsets), enc.short(278, &[2]), enc.long(279, &counts)]);
                }
            }
            if let Some(a) = s.active {
                t1.push(enc.long(50829, &a));
            }
            if let Some((o, z)) = s.crop {
                t1.extend([enc.short(50719, &o), enc.short(50720, &z)]);
            }
            for (tag, v) in [(50715u16, &s.delta_h), (50716u16, &s.delta_v)] {
                if let Some(v) = v {
                    t1.push(if s.delta_double { enc.double(tag, v) } else { enc.srational(tag, v, 1) });
                }
            }
            for (tag, value) in &s.extra {
                t1.push(enc.long(*tag, &[*value]));
            }
            if let Some(table) = &s.lin_table {
                t1.push(enc.short(50712, table));
            }
            (t0, t1)
        };
        let zeros = vec![0u32; blocks.len()];
        let (a, b) = make(0, &zeros);
        let ifd0_at = 8;
        let raw_at = ifd0_at + enc.ifd_len(&a);
        let data_at = raw_at + enc.ifd_len(&b);
        let mut offsets = Vec::new();
        let mut p = data_at;
        for blk in &blocks {
            offsets.push(p as u32);
            p += blk.len();
        }
        let (t0, t1) = make(raw_at as u32, &offsets);
        let mut bytes = if s.be { b"MM\0*".to_vec() } else { b"II*\0".to_vec() };
        bytes.extend_from_slice(&enc.u32(ifd0_at as u32));
        bytes.extend(enc.ifd(ifd0_at, &t0, 0));
        bytes.extend(enc.ifd(raw_at, &t1, 0));
        for blk in &blocks {
            bytes.extend_from_slice(blk);
        }
        Built { bytes, ifd0_at, ifd0_entries: t0.len(), raw_at }
    }

    // ---- test-side lossless JPEG encoder (T.81 SOF3, one 5-bit-code Huffman table) -------------
    struct BitWriter {
        out: Vec<u8>,
        acc: u32,
        n: u32,
    }

    impl BitWriter {
        fn put(&mut self, value: u32, bits: u32) {
            for i in (0..bits).rev() {
                self.acc = (self.acc << 1) | ((value >> i) & 1);
                self.n += 1;
                if self.n == 8 {
                    self.flush_byte();
                }
            }
        }
        fn flush_byte(&mut self) {
            self.out.push(self.acc as u8);
            if self.acc as u8 == 0xFF {
                self.out.push(0);
            }
            self.acc = 0;
            self.n = 0;
        }
        fn pad(&mut self) {
            while self.n != 0 {
                self.put(1, 1);
            }
        }
    }

    /// `lj` = (components, predictor, precision, point transform, restart rows).
    fn ljpeg_encode(samples: &[u16], bw: usize, bh: usize, lj: (usize, u8, u8, u32, usize)) -> Vec<u8> {
        let (comps, predictor, precision, pt, restart_rows) = lj;
        let x_per_line = bw / comps;
        let be16 = |v: usize| (v as u16).to_be_bytes();
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xC3];
        out.extend(be16(8 + 3 * comps));
        out.push(precision);
        out.extend(be16(bh));
        out.extend(be16(x_per_line));
        out.push(comps as u8);
        for c in 0..comps {
            out.extend([c as u8 + 1, 0x11, 0]);
        }
        out.extend([0xFF, 0xC4]);
        out.extend(be16(2 + 1 + 16 + 17));
        out.push(0);
        let mut counts = [0u8; 16];
        counts[4] = 17; // seventeen 5-bit codes: symbol k has code k
        out.extend(counts);
        out.extend(0..=16u8);
        if restart_rows > 0 {
            out.extend([0xFF, 0xDD, 0, 4]);
            out.extend(be16(restart_rows * x_per_line));
        }
        out.extend([0xFF, 0xDA]);
        out.extend(be16(6 + 2 * comps));
        out.push(comps as u8);
        for c in 0..comps {
            out.extend([c as u8 + 1, 0]);
        }
        out.extend([predictor, 0, pt as u8]);
        let mut w = BitWriter { out, acc: 0, n: 0 };
        let v = |i: usize| i64::from(samples[i]) >> pt;
        let mut rst = 0u8;
        for y in 0..bh {
            let first_row = y == 0 || (restart_rows > 0 && y % restart_rows == 0);
            if restart_rows > 0 && y > 0 && y % restart_rows == 0 {
                w.pad();
                w.out.extend([0xFF, 0xD0 + (rst & 7)]);
                rst += 1;
            }
            for x in 0..x_per_line {
                for c in 0..comps {
                    let at = y * bw + x * comps + c;
                    let prediction = if first_row {
                        if x == 0 { 1i64 << (precision as u32 - pt - 1) } else { v(at - comps) }
                    } else if x == 0 {
                        v(at - bw)
                    } else {
                        let (a, b, cc) = (v(at - comps), v(at - bw), v(at - bw - comps));
                        match predictor {
                            1 => a,
                            2 => b,
                            3 => cc,
                            4 => a + b - cc,
                            5 => a + ((b - cc) >> 1),
                            6 => b + ((a - cc) >> 1),
                            _ => (a + b) >> 1,
                        }
                    };
                    let mut diff = (v(at) - prediction).rem_euclid(65536);
                    if diff > 32767 {
                        diff -= 65536;
                    }
                    if diff == -32768 {
                        w.put(16, 5); // category 16: no extra bits
                        continue;
                    }
                    let category = 64 - diff.unsigned_abs().leading_zeros();
                    w.put(category, 5);
                    if category > 0 {
                        let bits = if diff >= 0 { diff } else { diff - 1 };
                        w.put((bits as u32) & ((1u32 << category) - 1), category);
                    }
                }
            }
        }
        w.pad();
        let mut out = w.out;
        out.extend([0xFF, 0xD9]);
        out
    }
}

// ---- NEF uncompressed (copied from tests/develop_nef.rs) ----------------------------------------
mod nef {
    use hsk_studio_develop::*;

    pub(super) fn seeds() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            // nef_uncompressed_packed_and_16bit_strips_decode: `Nef::default()` (big-endian packed 12-bit).
            ("nef_uncompressed_be_12bit", build(&Nef::default())),
            // nef_uncompressed_packed_and_16bit_strips_decode: little-endian 16-bit.
            ("nef_uncompressed_le_16bit", build(&Nef { big_endian: false, bits: 16, ..Nef::default() })),
        ]
    }

    fn source(x: u32, y: u32, bits: u16) -> u16 {
        ((x * 517 + y * 1031 + 3) % (1u32 << bits)) as u16
    }

    struct Nef {
        big_endian: bool,
        width: u32,
        height: u32,
        bits: u16,
        compression: u16,
        rows_per_strip: u32,
        make: &'static [u8],
        dng_version: bool,
        /// Replace the strip payload by this many bytes and point every strip at it (hostile sizes).
        alias_payload: Option<usize>,
    }

    impl Default for Nef {
        fn default() -> Self {
            Self {
                big_endian: true,
                width: 6,
                height: 4,
                bits: 12,
                compression: 1,
                rows_per_strip: 3,
                make: b"NIKON CORPORATION\0",
                dng_version: false,
                alias_payload: None,
            }
        }
    }

    /// Packed MSB-first rows (8/16-bit byte aligned in file order), each row starting on a byte.
    fn pack(spec: &Nef) -> Vec<Vec<u8>> {
        (0..spec.height)
            .map(|y| {
                let mut row = Vec::new();
                match spec.bits {
                    8 => (0..spec.width).for_each(|x| row.push(source(x, y, 8) as u8)),
                    16 => (0..spec.width).for_each(|x| {
                        let v = source(x, y, 16);
                        row.extend_from_slice(&if spec.big_endian { v.to_be_bytes() } else { v.to_le_bytes() });
                    }),
                    bits => {
                        let (mut acc, mut have) = (0u64, 0u32);
                        for x in 0..spec.width {
                            acc = (acc << bits) | u64::from(source(x, y, bits));
                            have += u32::from(bits);
                            while have >= 8 {
                                have -= 8;
                                row.push((acc >> have) as u8);
                            }
                        }
                        if have > 0 {
                            row.push((acc << (8 - have)) as u8);
                        }
                    }
                }
                row
            })
            .collect()
    }

    struct Enc {
        be: bool,
    }
    impl Enc {
        fn u16(&self, v: u16) -> [u8; 2] {
            if self.be { v.to_be_bytes() } else { v.to_le_bytes() }
        }
        fn u32(&self, v: u32) -> [u8; 4] {
            if self.be { v.to_be_bytes() } else { v.to_le_bytes() }
        }
    }

    /// (tag, type, count, inline-or-offset value bytes)
    type Tag = (u16, u16, u32, Vec<u8>);

    fn ifd_bytes(e: &Enc, tags: &[Tag], next: u32) -> Vec<u8> {
        let mut b = e.u16(tags.len() as u16).to_vec();
        for (tag, typ, count, value) in tags {
            b.extend_from_slice(&e.u16(*tag));
            b.extend_from_slice(&e.u16(*typ));
            b.extend_from_slice(&e.u32(*count));
            let mut v = value.clone();
            v.resize(4, 0);
            b.extend_from_slice(&v);
        }
        b.extend_from_slice(&e.u32(next));
        b
    }

    fn build(spec: &Nef) -> Vec<u8> {
        let e = Enc { be: spec.big_endian };
        let rows = pack(spec);
        let strips: Vec<Vec<u8>> = rows.chunks(spec.rows_per_strip as usize).map(|c| c.concat()).collect();
        let n = strips.len() as u32;
        // Layout: header(8) | IFD0 at 8 (Make, Model, SubIFDs[, DNGVersion]) | raw IFD | arrays | Make text | strips.
        let ifd0_tags = if spec.dng_version { 4 } else { 3 };
        let ifd0_len = 2 + 12 * ifd0_tags + 4;
        let raw_at = 8 + ifd0_len;
        let raw_tags = 12u32;
        let raw_len = 2 + 12 * raw_tags + 4;
        let offsets_at = raw_at + raw_len;
        let counts_at = offsets_at + 4 * n;
        let make_at = counts_at + 4 * n;
        let mut data_at = make_at + spec.make.len() as u32;
        let mut payload = Vec::new();
        let mut offsets = Vec::new();
        let mut counts = Vec::new();
        match spec.alias_payload {
            Some(len) => {
                payload = vec![0u8; len];
                for s in &strips {
                    offsets.push(data_at);
                    counts.push(s.len() as u32);
                }
            }
            None => {
                for s in &strips {
                    offsets.push(data_at);
                    counts.push(s.len() as u32);
                    data_at += s.len() as u32;
                    payload.extend_from_slice(s);
                }
            }
        }
        let array = |values: &[u32]| -> Vec<u8> { values.iter().flat_map(|v| e.u32(*v)).collect() };
        let at_or_inline = |values: &[u32], at: u32| -> Vec<u8> {
            if values.len() == 1 { e.u32(values[0]).to_vec() } else { e.u32(at).to_vec() }
        };
        let mut b = if spec.big_endian { b"MM\0*".to_vec() } else { b"II*\0".to_vec() };
        b.extend_from_slice(&e.u32(8));
        let mut t0: Vec<Tag> = vec![
            (271, 2, spec.make.len() as u32, e.u32(make_at).to_vec()),
            (272, 2, 4, b"D1H\0".to_vec()),
            (330, 4, 1, e.u32(raw_at).to_vec()),
        ];
        if spec.dng_version {
            t0.push((50706, 1, 4, vec![1, 4, 0, 0]));
        }
        b.extend(ifd_bytes(&e, &t0, 0));
        let raw: Vec<Tag> = vec![
            (254, 4, 1, e.u32(0).to_vec()),
            (256, 4, 1, e.u32(spec.width).to_vec()),
            (257, 4, 1, e.u32(spec.height).to_vec()),
            (258, 3, 1, e.u16(spec.bits).to_vec()),
            (259, 3, 1, e.u16(spec.compression).to_vec()),
            (262, 3, 1, e.u16(32803).to_vec()),
            (273, 4, n, at_or_inline(&offsets, offsets_at)),
            (277, 3, 1, e.u16(1).to_vec()),
            (278, 4, 1, e.u32(spec.rows_per_strip).to_vec()),
            (279, 4, n, at_or_inline(&counts, counts_at)),
            (33421, 3, 2, [e.u16(2), e.u16(2)].concat()),
            (33422, 1, 4, vec![2, 1, 1, 0]),
        ];
        assert_eq!(raw.len() as u32, raw_tags);
        b.extend(ifd_bytes(&e, &raw, 0));
        b.extend(array(&offsets));
        b.extend(array(&counts));
        b.extend_from_slice(spec.make);
        b.extend(payload);
        b
    }
}

// ---- NEF Huffman-compressed (copied from tests/develop_nef_compressed.rs) -----------------------
mod nef_compressed {
    use hsk_studio_develop::*;

    pub(super) fn seeds() -> Vec<(&'static str, Vec<u8>)> {
        // nef_huffman_hostile_inputs_are_typed_refusals_before_allocation: `ok` (14-bit lossless),
        // `assert!(decode_container(&ok).is_ok())`.
        let (w, h) = (64usize, 16usize);
        let img = image(w, h, 16383, 3);
        let table = lossless_table(2048);
        let strip = encode(&img, w, LOSSLESS_14, 2048);
        let ok = nef(strip.clone(), w as u32, h as u32, 14, Some(&table));
        // nef_huffman_lossless_and_lossy_decode_to_the_source_cfa: lossy type 2 (12-bit),
        // `decode_container(&file).unwrap()`.
        let (w, h) = (40usize, 6usize);
        let img = image(w, h, 1023, 9); // lossy codes stay within category 10
        let lossy = nef(encode(&img, w, LOSSY_12, 300), w as u32, h as u32, 12, Some(&lossy_type2_table(300, 3500)));
        vec![("nef_huffman_lossless_14bit", ok), ("nef_huffman_lossy_12bit", lossy)]
    }

    const LOSSLESS_12: (&[u8], &[u8]) = (&[0, 1, 4, 2, 3, 1, 2], &[5, 4, 6, 3, 7, 2, 8, 1, 9, 0, 10, 11, 12]);
    const LOSSLESS_14: (&[u8], &[u8]) = (&[0, 1, 4, 2, 2, 3, 1, 2], &[7, 6, 8, 5, 9, 4, 10, 3, 11, 12, 2, 0, 1, 13, 14]);
    const LOSSY_12: (&[u8], &[u8]) = (&[0, 1, 5, 1, 1, 1, 1, 1, 1], &[5, 4, 3, 6, 2, 7, 1, 0, 8, 9, 255, 10]);

    /// (code, length) per category.
    fn codes(t: (&[u8], &[u8])) -> Vec<(u32, u32)> {
        let mut out = vec![(0, 0); 256];
        let (mut code, mut syms) = (0u32, t.1.iter());
        for (i, &n) in t.0.iter().enumerate() {
            for _ in 0..n {
                out[*syms.next().unwrap() as usize] = (code, i as u32 + 1);
                code += 1;
            }
            code <<= 1;
        }
        out
    }

    #[derive(Default)]
    struct Writer {
        out: Vec<u8>,
        acc: u32,
        n: u32,
    }

    impl Writer {
        fn put(&mut self, v: u32, len: u32) {
            for i in (0..len).rev() {
                self.acc = (self.acc << 1) | ((v >> i) & 1);
                self.n += 1;
                if self.n == 8 {
                    self.out.push(self.acc as u8);
                    (self.acc, self.n) = (0, 0);
                }
            }
        }
        fn finish(mut self) -> Vec<u8> {
            if self.n > 0 {
                self.out.push((self.acc << (8 - self.n)) as u8);
            }
            self.out.extend_from_slice(&[0; 4]);
            self.out
        }
    }

    /// Encode decoded-domain `values` the way the camera does.
    fn encode(values: &[i32], w: usize, t: (&[u8], &[u8]), seed: i32) -> Vec<u8> {
        let codes = codes(t);
        let mut wr = Writer::default();
        let mut vpred = [[seed; 2]; 2];
        for (y, row) in values.chunks(w).enumerate() {
            for (x, &v) in row.iter().enumerate() {
                let pred = if x < 2 { vpred[y & 1][x] } else { row[x - 2] };
                if x < 2 {
                    vpred[y & 1][x] = v;
                }
                let d = v - pred;
                let k = 32 - d.unsigned_abs().leading_zeros();
                let (code, len) = codes[k as usize];
                assert!(len > 0, "category {k}");
                wr.put(code, len);
                wr.put((if d < 0 { d + (1 << k) - 1 } else { d }) as u32, k);
            }
        }
        wr.finish()
    }

    fn image(w: usize, h: usize, max: i32, seed: u32) -> Vec<i32> {
        let mut s = seed;
        (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as i32, (i / w) as i32);
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let base = (x * 37 + y * 11) % (max / 2) + if (x / 7 + y / 5) % 3 == 0 { max / 3 } else { 0 };
                (base + (s >> 24) as i32 % 9 - 4).clamp(0, max)
            })
            .collect()
    }

    /// One IFD entry: (tag, type, count, value bytes).
    type Entry = (u16, u16, u32, Vec<u8>);

    /// Big-endian classic TIFF writer: IFDs of entries; data after each IFD.
    struct Tiff {
        ifds: Vec<Vec<Entry>>,
    }

    fn be16(v: &[u16]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }
    fn be32(v: &[u32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }

    impl Tiff {
        /// Layout: header, then each IFD followed by its out-of-line data. `link` entries (type 13/4
        /// with a placeholder) are patched to the offset of the IFD index stored in the value.
        fn write(&self, links: &[(usize, u16, usize)], strip_tag_ifd: Option<(usize, Vec<u8>)>) -> Vec<u8> {
            let mut out = b"MM\0*\0\0\0\x08".to_vec();
            let mut ifd_offsets = vec![0u32; self.ifds.len()];
            // two passes: sizes first
            let mut pos = 8u32;
            for (i, ifd) in self.ifds.iter().enumerate() {
                ifd_offsets[i] = pos;
                let extra: u32 = ifd.iter().map(|e| if e.3.len() > 4 { e.3.len() as u32 } else { 0 }).sum();
                pos += 2 + 12 * ifd.len() as u32 + 4 + extra;
            }
            let strip_at = pos;
            for (i, ifd) in self.ifds.iter().enumerate() {
                let mut data_at = ifd_offsets[i] + 2 + 12 * ifd.len() as u32 + 4;
                let mut tail = Vec::new();
                out.extend_from_slice(&(ifd.len() as u16).to_be_bytes());
                let mut sorted = ifd.clone();
                sorted.sort_by_key(|e| e.0);
                for (tag, ty, count, value) in sorted {
                    let mut value = value.clone();
                    if let Some(&(_, _, target)) = links.iter().find(|l| l.0 == i && l.1 == tag) {
                        value = be32(&[ifd_offsets[target]]);
                    }
                    if strip_tag_ifd.as_ref().is_some_and(|s| s.0 == i) && tag == 273 {
                        value = be32(&[strip_at]);
                    }
                    out.extend_from_slice(&tag.to_be_bytes());
                    out.extend_from_slice(&ty.to_be_bytes());
                    out.extend_from_slice(&count.to_be_bytes());
                    if value.len() > 4 {
                        out.extend_from_slice(&data_at.to_be_bytes());
                        data_at += value.len() as u32;
                        tail.extend_from_slice(&value);
                    } else {
                        let mut v = value.clone();
                        v.resize(4, 0);
                        out.extend_from_slice(&v);
                    }
                }
                out.extend_from_slice(&[0; 4]);
                out.extend_from_slice(&tail);
            }
            if let Some((_, strip)) = strip_tag_ifd {
                assert_eq!(out.len() as u32, strip_at);
                out.extend_from_slice(&strip);
            }
            out
        }
    }

    /// Maker note bytes: `Nikon\0` + version + a big-endian TIFF with one IFD holding 0x0096.
    /// With `black`, also 0x003D BlackLevel (4 SHORTs, 14-bit units).
    fn maker_note(table: &[u8], black: Option<[u16; 4]>) -> Vec<u8> {
        let mut note = b"Nikon\0\x02\x10\0\0".to_vec();
        let mut entries = vec![(0x0096, 7, table.len() as u32, table.to_vec())];
        if let Some(b) = black {
            entries.push((0x003d, 3, 4, be16(&b)));
        }
        let t = Tiff { ifds: vec![entries] };
        note.extend(t.write(&[], None));
        note
    }

    fn nef(strip: Vec<u8>, w: u32, h: u32, bits: u16, table: Option<&[u8]>) -> Vec<u8> {
        nef_with_black(strip, w, h, bits, table, None)
    }

    fn nef_with_black(strip: Vec<u8>, w: u32, h: u32, bits: u16, table: Option<&[u8]>, black: Option<[u16; 4]>) -> Vec<u8> {
        let mut ifd0 = vec![
            (271, 2, 18, b"NIKON CORPORATION\0".to_vec()),
            (272, 2, 11, b"NIKON TEST\0".to_vec()),
            (330, 4, 1, be32(&[0])),
        ];
        let mut links = vec![(0, 330, 1)];
        let raw = vec![
            (254, 4, 1, be32(&[0])),
            (256, 4, 1, be32(&[w])),
            (257, 4, 1, be32(&[h])),
            (258, 3, 1, be16(&[bits])),
            (259, 3, 1, be16(&[34713])),
            (262, 3, 1, be16(&[32803])),
            (273, 4, 1, be32(&[0])),
            (277, 3, 1, be16(&[1])),
            (278, 4, 1, be32(&[h])),
            (279, 4, 1, be32(&[strip.len() as u32])),
            (33421, 3, 2, be16(&[2, 2])),
            (33422, 1, 4, vec![0, 1, 1, 2]),
        ];
        let mut ifds = vec![ifd0.clone(), raw];
        if let Some(table) = table {
            ifd0.push((34665, 4, 1, be32(&[0])));
            ifds[0] = ifd0;
            let note = maker_note(table, black);
            ifds.push(vec![(37500, 7, note.len() as u32, note)]);
            links.push((0, 34665, 2));
        }
        Tiff { ifds }.write(&links, Some((1, strip)))
    }

    fn lossless_table(seed: u16) -> Vec<u8> {
        let mut t = vec![0x46, 0x30];
        for _ in 0..4 {
            t.extend_from_slice(&seed.to_be_bytes());
        }
        t.resize(46, 0);
        t
    }

    /// Version 0x44 0x20, 257 points rising by 16 (12-bit identity span) up to `white`, big-endian.
    fn lossy_type2_table(seed: u16, white: u16) -> Vec<u8> {
        let mut t = vec![0x44, 0x20];
        for _ in 0..4 {
            t.extend_from_slice(&seed.to_be_bytes());
        }
        t.extend_from_slice(&257u16.to_be_bytes());
        for k in 0..256u16 {
            t.extend_from_slice(&(k * 12).to_be_bytes());
        }
        t.extend_from_slice(&white.to_be_bytes());
        t.resize(624, 0);
        t
    }
}

// ---- ARW (copied from tests/develop_arw.rs) -----------------------------------------------------
mod arw {
    use hsk_studio_develop::*;

    pub(super) fn seeds() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            // arw_craw_decodes_groups_spans_and_curve: `build(&Arw::default())` (cRAW), unwrap.
            ("arw_craw", build(&Arw::default()).0),
            // arw_craw_decodes_groups_spans_and_curve: uncompressed 16-bit strips, unwrap.
            ("arw_uncompressed_16bit", build(&Arw { compression: 1, ..Arw::default() }).0),
        ]
    }

    const POINTS: [u16; 4] = [8000, 10400, 12900, 14100];

    /// Test-side curve per the documented formula: knots `[0, p>>2 & 0xfff .., 4095]`, slope 2^segment.
    fn curve(points: [u16; 4]) -> Vec<u32> {
        let mut knots = vec![0u32];
        knots.extend(points.iter().map(|p| u32::from(*p >> 2) & 0xfff));
        knots.push(4095);
        let mut table = vec![0u32; 4096];
        for s in 0..5 {
            for j in knots[s] + 1..=knots[s + 1] {
                table[j as usize] = table[j as usize - 1] + (1 << s);
            }
        }
        table
    }

    /// One 16-pixel group: (codes in group order, block bytes).
    fn group(g: usize) -> ([u32; 16], [u8; 16]) {
        let sh = (g % 5) as u32;
        let min: u32 = if sh == 4 { 7 } else { 100 + (g as u32 * 37) % 200 };
        let max = min + (127 << sh);
        let imax = (g * 5) % 16;
        let imin = (g * 5 + 3) % 16;
        let mut codes = [0u32; 16];
        let mut bits: u128 = u128::from(max) | u128::from(min) << 11 | (imax as u128) << 22 | (imin as u128) << 26;
        let mut at = 30;
        for (i, code) in codes.iter_mut().enumerate() {
            *code = if i == imax {
                max
            } else if i == imin {
                min
            } else {
                let delta = ((i * 9 + g) % 128) as u32;
                bits |= u128::from(delta) << at;
                at += 7;
                min + (delta << sh)
            };
        }
        assert_eq!(at, 30 + 14 * 7);
        (codes, bits.to_le_bytes())
    }

    struct Arw {
        width: u32,
        height: u32,
        compression: u16,
        /// StripByteCounts override.
        count: Option<u32>,
        curve: bool,
        dng_version: bool,
        /// Truncate the payload to this many bytes.
        payload: Option<usize>,
    }

    impl Default for Arw {
        fn default() -> Self {
            Self { width: 64, height: 2, compression: 32767, count: None, curve: true, dng_version: false, payload: None }
        }
    }

    /// (stream bytes, expected samples) for a cRAW or 16-bit uncompressed frame.
    fn frame(spec: &Arw) -> (Vec<u8>, Vec<u16>) {
        let (w, h) = (spec.width as usize, spec.height as usize);
        let table = curve(POINTS);
        let mut expected = vec![0u16; w * h];
        let mut stream = Vec::new();
        if spec.compression == 1 {
            for (i, slot) in expected.iter_mut().enumerate() {
                let v = ((i * 7919) % 65536) as u16;
                *slot = v;
                stream.extend_from_slice(&v.to_le_bytes());
            }
            return (stream, expected);
        }
        let mut g = 0;
        for y in 0..h {
            for span in 0..w / 32 {
                for parity in 0..2 {
                    let (codes, block) = group(g);
                    g += 1;
                    stream.extend_from_slice(&block);
                    for (i, code) in codes.iter().enumerate() {
                        let x = span * 32 + parity + 2 * i;
                        expected[y * w + x] = ((table[(*code as usize) << 1] >> 2) << 2) as u16;
                    }
                }
            }
        }
        (stream, expected)
    }

    fn build(spec: &Arw) -> (Vec<u8>, Vec<u16>) {
        let (mut stream, expected) = frame(spec);
        if let Some(len) = spec.payload {
            stream.truncate(len);
        }
        let le16 = |v: u16| v.to_le_bytes().to_vec();
        let le32 = |v: u32| v.to_le_bytes().to_vec();
        // header 8 | IFD0 at 8 (Make, Model, SubIFDs[, DNGVersion]) | raw IFD | Make text | curve | stream
        let n0: u32 = if spec.dng_version { 4 } else { 3 };
        let raw_at = 8 + 2 + 12 * n0 + 4;
        let mut raw_tags: Vec<(u16, u16, u32, Vec<u8>)> = vec![
            (254, 4, 1, le32(0)),
            (256, 4, 1, le32(spec.width)),
            (257, 4, 1, le32(spec.height)),
            (258, 3, 1, le16(if spec.compression == 1 { 16 } else { 14 })),
            (259, 3, 1, le16(spec.compression)),
            (262, 3, 1, le16(32803)),
            (273, 4, 1, vec![]),
            (279, 4, 1, le32(spec.count.unwrap_or(stream.len() as u32))),
            (33421, 3, 2, [le16(2), le16(2)].concat()),
            (33422, 1, 4, vec![0, 1, 1, 2]),
        ];
        if spec.curve {
            raw_tags.push((0x7010, 3, 4, vec![]));
        }
        let raw_len = 2 + 12 * raw_tags.len() as u32 + 4;
        let make_at = raw_at + raw_len;
        let curve_at = make_at + 5;
        let stream_at = curve_at + 8;
        for t in raw_tags.iter_mut() {
            if t.0 == 273 {
                t.3 = le32(stream_at);
            }
            if t.0 == 0x7010 {
                t.3 = le32(curve_at);
            }
        }
        let ifd = |tags: &[(u16, u16, u32, Vec<u8>)]| {
            let mut b = le16(tags.len() as u16);
            for (tag, typ, count, value) in tags {
                b.extend(le16(*tag));
                b.extend(le16(*typ));
                b.extend(le32(*count));
                let mut v = value.clone();
                v.resize(4, 0);
                b.extend(v);
            }
            b.extend(le32(0));
            b
        };
        let mut t0 = vec![(271u16, 2u16, 5u32, le32(make_at)), (272, 2, 4, b"T7S\0".to_vec()), (330, 4, 1, le32(raw_at))];
        if spec.dng_version {
            t0.push((50706, 1, 4, vec![1, 4, 0, 0]));
        }
        let mut b = b"II*\0".to_vec();
        b.extend(le32(8));
        b.extend(ifd(&t0));
        assert_eq!(b.len() as u32, raw_at);
        b.extend(ifd(&raw_tags));
        b.extend_from_slice(b"SONY\0");
        for p in POINTS {
            b.extend(le16(p));
        }
        assert_eq!(b.len() as u32, stream_at);
        b.extend(stream);
        (b, expected)
    }
}

// ---- CR2 (copied from tests/develop_cr2.rs) -----------------------------------------------------
mod cr2 {
    use hsk_studio_develop::*;

    pub(super) fn seeds() -> Vec<(&'static str, Vec<u8>)> {
        // cr2_decodes_and_reassembles_column_slices: `cr2(&stream, &Cr2::default())` and the same
        // stream with `slices: None`, both unwrap.
        let stream = sliced_stream();
        vec![
            ("cr2_sliced", cr2(&stream, &Cr2::default())),
            ("cr2_unsliced", cr2(&stream, &Cr2 { slices: None, ..Cr2::default() })),
        ]
    }

    const W: usize = 24;
    const H: usize = 4;

    fn source(x: usize, y: usize) -> u16 {
        ((x * 131 + y * 977 + 7) % 4096) as u16
    }

    struct BitWriter {
        out: Vec<u8>,
        acc: u32,
        n: u32,
    }

    impl BitWriter {
        fn put(&mut self, value: u32, bits: u32) {
            for i in (0..bits).rev() {
                self.acc = (self.acc << 1) | ((value >> i) & 1);
                self.n += 1;
                if self.n == 8 {
                    let byte = self.acc as u8;
                    self.out.push(byte);
                    if byte == 0xFF {
                        self.out.push(0); // T.81 byte stuffing
                    }
                    self.acc = 0;
                    self.n = 0;
                }
            }
        }
        fn finish(mut self) -> Vec<u8> {
            while self.n != 0 {
                self.put(1, 1);
            }
            self.out
        }
    }

    /// Lossless JPEG (SOF3, one Huffman table: symbols 0..=16 all 5-bit codes, predictor 1, Pt 0).
    /// `values` is the stored sample stream, `per_line * components` values per JPEG row.
    fn ljpeg(values: &[u16], precision: u8, per_line: u16, lines: u16, components: u8) -> Vec<u8> {
        let n = usize::from(components);
        let row = usize::from(per_line) * n;
        let mut s = vec![0xFF, 0xD8];
        s.extend_from_slice(&[0xFF, 0xC4, 0, 36, 0x00]);
        let mut counts = [0u8; 16];
        counts[4] = 17;
        s.extend_from_slice(&counts);
        s.extend((0u8..=16).collect::<Vec<_>>());
        let lf = 8 + 3 * u16::from(components);
        s.extend_from_slice(&[0xFF, 0xC3]);
        s.extend_from_slice(&lf.to_be_bytes());
        s.push(precision);
        s.extend_from_slice(&lines.to_be_bytes());
        s.extend_from_slice(&per_line.to_be_bytes());
        s.push(components);
        for c in 0..components {
            s.extend_from_slice(&[c + 1, 0x11, 0]);
        }
        let ls = 6 + 2 * u16::from(components);
        s.extend_from_slice(&[0xFF, 0xDA]);
        s.extend_from_slice(&ls.to_be_bytes());
        s.push(components);
        for c in 0..components {
            s.extend_from_slice(&[c + 1, 0x00]);
        }
        s.extend_from_slice(&[1, 0, 0]);
        let mut bits = BitWriter { out: Vec::new(), acc: 0, n: 0 };
        let initial = 1i32 << (precision - 1);
        for (i, v) in values.iter().enumerate() {
            let (y, x) = (i / row, i % row);
            let prediction = if x < n {
                if y == 0 { initial } else { i32::from(values[i - row]) }
            } else {
                i32::from(values[i - n])
            };
            let diff = i32::from(*v) - prediction;
            let ssss = 32 - diff.unsigned_abs().leading_zeros();
            bits.put(ssss, 5);
            if ssss > 0 {
                let coded = if diff > 0 { diff } else { diff + (1 << ssss) - 1 };
                bits.put(coded as u32, ssss);
            }
        }
        s.extend(bits.finish());
        s.extend_from_slice(&[0xFF, 0xD9]);
        s
    }

    struct Cr2 {
        slices: Option<[u16; 3]>,
        compression: u16,
        minor: u8,
        /// Override StripByteCounts (hostile payload claims).
        byte_count: Option<u32>,
    }

    impl Default for Cr2 {
        fn default() -> Self {
            Self { slices: Some([2, 8, 8]), compression: 6, minor: 0, byte_count: None }
        }
    }

    /// Little-endian CR2: header, IFD0 (Model "T1X"), raw IFD (Compression, StripOffsets,
    /// StripByteCounts, optional 0xC640), slice values, stream.
    fn cr2(stream: &[u8], spec: &Cr2) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"II*\0");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&[b'C', b'R', 2, spec.minor]);
        b.extend_from_slice(&34u32.to_le_bytes());
        // IFD0 at 16: Model (ASCII, 4 bytes inline).
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&0x0110u16.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&4u32.to_le_bytes());
        b.extend_from_slice(b"T1X\0");
        b.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(b.len(), 34);
        let entries: u16 = if spec.slices.is_some() { 4 } else { 3 };
        let slices_at = 34 + 2 + 12 * u32::from(entries) + 4;
        let stream_at = slices_at + 6 + 2;
        let entry = |b: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: u32| {
            b.extend_from_slice(&tag.to_le_bytes());
            b.extend_from_slice(&typ.to_le_bytes());
            b.extend_from_slice(&count.to_le_bytes());
            b.extend_from_slice(&value.to_le_bytes());
        };
        b.extend_from_slice(&entries.to_le_bytes());
        entry(&mut b, 259, 3, 1, u32::from(spec.compression));
        entry(&mut b, 273, 4, 1, stream_at);
        entry(&mut b, 279, 4, 1, spec.byte_count.unwrap_or(stream.len() as u32));
        if spec.slices.is_some() {
            entry(&mut b, 0xC640, 3, 3, slices_at);
        }
        b.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(b.len() as u32, slices_at);
        for v in spec.slices.unwrap_or([0; 3]) {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&[0, 0]);
        assert_eq!(b.len() as u32, stream_at);
        b.extend_from_slice(stream);
        b
    }

    /// The 24x4 source grid stored as CR2 column slices of widths 8, 8, 8: slice 0 all rows, then
    /// slice 1, then slice 2; encoded as 4 JPEG rows of 12 samples x 2 components, 12-bit.
    fn sliced_stream() -> Vec<u8> {
        let mut stored = Vec::new();
        for x0 in [0, 8, 16] {
            for y in 0..H {
                for x in x0..x0 + 8 {
                    stored.push(source(x, y));
                }
            }
        }
        ljpeg(&stored, 12, 12, 4, 2)
    }
}

// ---- RAF (copied from tests/develop_raf.rs) -----------------------------------------------------
mod raf {
    use hsk_studio_develop::*;

    pub(super) fn seeds() -> Vec<(&'static str, Vec<u8>)> {
        // raf_compressed_bayer_and_xtrans_decode_through_the_container: bits 12, Bayer, lossless
        // (mode 2), `decode_container(&file).unwrap_or_else(..)`.
        let (bits, xtrans, q): (u32, bool, Option<&[u8]>) = (12, false, None);
        let mode = if q.is_none() { 2 } else { 3 };
        let bayer = raf(780, 18, bits, &compressed(bits, xtrans, q), xtrans.then(layout), mode);
        // raf_hostile_and_unsupported_inputs_are_typed_refusals: `ok` (14-bit X-Trans lossy, mode 3),
        // `assert!(decode_container(&ok).is_ok())`.
        let strip = compressed(14, true, Some(&[1, 2, 0]));
        let ok = raf(780, 18, 14, &strip, Some(layout()), 3);
        // raf_uncompressed_packings_decode: 16-bit little-endian words (mode 0), unwrap_or_else.
        let (w, h) = (16usize, 6usize);
        let value = |x: usize, y: usize, max: usize| ((x * 251 + y * 977 + x * y * 13) % max) as u16;
        let words: Vec<u8> = (0..w * h).flat_map(|i| value(i % w, i / w, 16384).to_le_bytes()).collect();
        let uncompressed = raf(w as u32, h as u32, 14, &words, None, 0);
        vec![("raf_lossless_bayer_12bit", bayer), ("raf_lossy_xtrans_14bit", ok), ("raf_uncompressed_16bit_words", uncompressed)]
    }

    const XTRANS: [[u8; 6]; 6] = [[1, 1, 0, 1, 1, 2], [1, 1, 2, 1, 1, 0], [2, 0, 1, 0, 2, 1], [1, 1, 2, 1, 1, 0], [1, 1, 0, 1, 1, 2], [0, 2, 1, 2, 0, 1]];
    const MASKS: [[u8; 2]; 6] = [[10, 15], [15, 10], [14, 10], [15, 11], [11, 15], [10, 14]];

    fn zero_stream(bits: u32, xtrans: bool, quantizers: Option<&[u8]>) -> Vec<u8> {
        let groups = quantizers.map_or(3, <[u8]>::len);
        let seed = (1u32 << (bits - 6), 1u32);
        let mut main = [[seed; 2]; 3];
        let mut fine = main;
        let mut previous = None;
        let mut stream = Vec::new();
        for group in 0..groups {
            let q = quantizers.map_or(0, |q| q[group]);
            if previous != Some(q) {
                main = [[seed; 2]; 3];
                previous = Some(q);
            }
            for (pair, masks) in MASKS.iter().enumerate() {
                let width = if xtrans { 512 } else { 384 };
                for even in (0usize..width + 8).step_by(2) {
                    for (parity, column) in [(0, Some(even)), (1, even.checked_sub(7))] {
                        let Some(column) = column.filter(|&x| x < width) else { continue };
                        for mask in masks {
                            if xtrans && (mask >> (column % 4)) & 1 == 0 {
                                continue;
                            }
                            let (sum, count) = &mut if q == 0 { &mut main } else { &mut fine }[pair % 3][parity];
                            let k = 32 - (sum.saturating_sub(1) / *count).leading_zeros();
                            stream.push(true);
                            stream.extend(std::iter::repeat_n(false, k as usize));
                            if *count == 64 {
                                *sum /= 2;
                                *count /= 2;
                            }
                            *count += 1;
                        }
                    }
                }
            }
        }
        stream.chunks(8).map(|c| c.iter().enumerate().fold(0, |b, (i, bit)| b | (u8::from(*bit) << (7 - i)))).collect()
    }

    /// `IS` stream for a 780-wide frame of `groups * 6` rows (two stripes).
    fn compressed(bits: u32, xtrans: bool, quantizers: Option<&[u8]>) -> Vec<u8> {
        let groups = quantizers.map_or(3, <[u8]>::len);
        let stream = zero_stream(bits, xtrans, quantizers);
        let mut out = b"IS".to_vec();
        out.extend_from_slice(&[u8::from(quantizers.is_none()), if xtrans { 16 } else { 0 }, bits as u8]);
        for v in [groups as u16 * 6, 1536, 780, 768] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out.push(2);
        out.extend_from_slice(&(groups as u16).to_be_bytes());
        for _ in 0..2 {
            out.extend_from_slice(&(stream.len() as u32).to_be_bytes());
        }
        out.resize(32, 0);
        if let Some(q) = quantizers {
            for _ in 0..2 {
                out.extend_from_slice(q);
                out.resize(out.len() + q.len().div_ceil(16) * 16 - q.len(), 0);
            }
        }
        out.extend_from_slice(&stream);
        out.extend_from_slice(&stream);
        out
    }

    /// A RAF: header (pointers at 84.., mode at 108), records (optional X-Trans layout), raw block.
    fn raf(w: u32, h: u32, bits: u32, strip: &[u8], layout: Option<[u8; 36]>, mode: u32) -> Vec<u8> {
        // Raw block: LE TIFF, IFD0 at 8 with one entry 0xF000 (type IFD) -> raw IFD at 26.
        let mut block = b"II*\0\x08\0\0\0".to_vec();
        block.extend(1u16.to_le_bytes());
        block.extend([0x00, 0xf0, 13, 0, 1, 0, 0, 0]);
        block.extend(26u32.to_le_bytes());
        block.extend(0u32.to_le_bytes());
        let entries: [(u16, u32); 5] = [(0xf001, w), (0xf002, h), (0xf003, bits), (0xf007, 26 + 2 + 5 * 12 + 4), (0xf008, strip.len() as u32)];
        block.extend((entries.len() as u16).to_le_bytes());
        for (tag, v) in entries {
            block.extend(tag.to_le_bytes());
            block.extend(4u16.to_le_bytes());
            block.extend(1u32.to_le_bytes());
            block.extend(v.to_le_bytes());
        }
        block.extend(0u32.to_le_bytes());
        block.extend_from_slice(strip);
        let mut records = Vec::new();
        let mut count = 0u32;
        if let Some(l) = layout {
            records.extend(0x0131u16.to_be_bytes());
            records.extend(36u16.to_be_bytes());
            records.extend(l);
            count += 1;
        }
        let mut dir = count.to_be_bytes().to_vec();
        dir.extend(records);
        let mut file = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
        file.resize(28, 0);
        file.extend(b"X-TEST");
        file.resize(112, 0);
        let dir_at = 112u32;
        let raw_at = dir_at + dir.len() as u32;
        for (at, v) in [(92, dir_at), (96, dir.len() as u32), (100, raw_at), (104, block.len() as u32), (108, mode)] {
            file[at..at + 4].copy_from_slice(&v.to_be_bytes());
        }
        file.extend(dir);
        file.extend(block);
        file
    }

    fn layout() -> [u8; 36] {
        let mut l = [0; 36];
        for (to, from) in l.iter_mut().zip(XTRANS.iter().flatten().rev()) {
            *to = *from;
        }
        l
    }
}

// ---- CR3 (copied from tests/develop_cr3.rs) -----------------------------------------------------
mod cr3 {
    use hsk_studio_develop::*;

    pub(super) fn seeds() -> Vec<(&'static str, Vec<u8>)> {
        // cr3_lossless_crx_decodes_the_sensor_mosaic_and_refuses_hostile_input: first call, unwrap.
        vec![("cr3_crx_lossless", cr3(&cmp1(0x100, 4, 4, 0, 3, 108), &flat_sample(), None))]
    }

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    fn full(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 4];
        b.extend_from_slice(body);
        bx(kind, &b)
    }

    /// CMP1 payload: version, sensor/tile size, bit depth, planes<<4|cfa, levels, header size.
    fn cmp1(version: u16, w: u32, h: u32, levels: u8, cfa: u8, header: u32) -> Vec<u8> {
        let mut b = vec![0u8; 52];
        b[0..2].copy_from_slice(&0xff00u16.to_be_bytes());
        b[2..4].copy_from_slice(&48u16.to_be_bytes());
        b[4..6].copy_from_slice(&version.to_be_bytes());
        for (at, v) in [(8, w), (12, h), (16, w), (20, h), (28, header)] {
            b[at..at + 4].copy_from_slice(&v.to_be_bytes());
        }
        b[24] = 14;
        b[25] = 0x40 | cfa;
        b[26] = levels;
        b
    }

    fn tiff_model(model: &str) -> Vec<u8> {
        // Little-endian TIFF, one IFD with Model (ASCII, out of line).
        let mut t = b"II*\0\x08\0\0\0".to_vec();
        t.extend(1u16.to_le_bytes());
        t.extend(272u16.to_le_bytes());
        t.extend(2u16.to_le_bytes());
        t.extend(((model.len() + 1) as u32).to_le_bytes());
        t.extend(26u32.to_le_bytes());
        t.extend(0u32.to_le_bytes());
        t.extend(model.as_bytes());
        t.push(0);
        t
    }

    /// A CR3 file whose raw track holds `sample` (stsz may claim `claimed` bytes instead).
    fn cr3(coding: &[u8], sample: &[u8], claimed: Option<u32>) -> Vec<u8> {
        let mut craw = vec![0u8; 82];
        craw[24..26].copy_from_slice(&4u16.to_be_bytes());
        craw[26..28].copy_from_slice(&4u16.to_be_bytes());
        craw.extend(bx(b"CMP1", coding));
        let mut canon_uuid = vec![0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48];
        canon_uuid.extend(bx(b"CMT1", &tiff_model("Canon EOS Test")));
        let canon = bx(b"uuid", &canon_uuid);
        let build = |offset: u64| {
            let stsd = full(b"stsd", &[1u32.to_be_bytes().to_vec(), bx(b"CRAW", &craw)].concat());
            let stsz = full(b"stsz", &[0u32.to_be_bytes(), 1u32.to_be_bytes(), claimed.unwrap_or(sample.len() as u32).to_be_bytes()].concat());
            let co64 = full(b"co64", &[1u32.to_be_bytes().to_vec(), offset.to_be_bytes().to_vec()].concat());
            let trak = bx(b"trak", &bx(b"mdia", &bx(b"minf", &bx(b"stbl", &[stsd, stsz, co64].concat()))));
            let mut file = bx(b"ftyp", b"crx \0\0\0\x01crx isom");
            file.extend(bx(b"moov", &[canon.clone(), trak].concat()));
            file
        };
        let head = build(0).len() as u64 + 8;
        let mut file = build(head);
        file.extend(bx(b"mdat", sample));
        file
    }

    fn marker(out: &mut Vec<u8>, code: u16, size: u32, flags: u32) {
        out.extend(code.to_be_bytes());
        out.extend(8u16.to_be_bytes());
        out.extend(size.to_be_bytes());
        out.extend(flags.to_be_bytes());
    }

    fn flat_sample() -> Vec<u8> {
        let mut out = Vec::new();
        marker(&mut out, 0xff01, 4, 0);
        for p in 0..4u32 {
            marker(&mut out, 0xff02, 1, (p << 28) | 0x0800_0000);
            marker(&mut out, 0xff03, 1, 0x0020_0000);
        }
        out.extend([0x1e; 4]);
        out
    }
}

// ---- RW2 (copied from tests/develop_rw2.rs) -----------------------------------------------------
mod rw2 {
    use hsk_studio_develop::*;

    pub(super) fn seeds() -> Vec<(&'static str, Vec<u8>)> {
        // rw2_format4_known_answers_and_format5_rotated_chunks: format 4, one 14 x 1 row, unwrap.
        let (block, _want) = format4_block();
        let format4 = rw2(14, 1, 2, 4, &block);
        // rw2_format4_known_answers_and_format5_rotated_chunks: format 5, 128 x 100, first 0x4000
        // bytes stored rotated, unwrap.
        let (w, h) = (128usize, 100usize);
        let value = |i: usize| (1 + (i * 7 + i / 128 * 13) % 4095) as u16;
        let mut logical = Vec::new();
        for y in 0..h {
            for block in 0..w.div_ceil(10) {
                let mut word = 0u128;
                for k in 0..10 {
                    let x = block * 10 + k;
                    word |= u128::from(if x < w { value(y * w + x) } else { 0 }) << (12 * k);
                }
                logical.extend(word.to_le_bytes());
            }
        }
        let mut stored = logical[0x4000 - 0x1ff8..0x4000].to_vec();
        stored.extend_from_slice(&logical[..0x4000 - 0x1ff8]);
        stored.extend_from_slice(&logical[0x4000..]);
        let format5 = rw2(w as u32, h as u32, 1, 5, &stored);
        vec![("rw2_format4", format4), ("rw2_format5_rotated", format5)]
    }

    fn entry(tag: u16, ty: u16, count: u32, value: u32) -> Vec<u8> {
        [tag.to_le_bytes().to_vec(), ty.to_le_bytes().to_vec(), count.to_le_bytes().to_vec(), value.to_le_bytes().to_vec()].concat()
    }

    fn rw2(w: u32, h: u32, cfa: u32, format: u32, data: &[u8]) -> Vec<u8> {
        let entries = [
            entry(0x0002, 3, 1, w),
            entry(0x0003, 3, 1, h),
            entry(0x0009, 3, 1, cfa),
            entry(0x000a, 3, 1, 12),
            entry(0x002d, 3, 1, format),
            entry(0x0118, 4, 1, 256),
            entry(272, 2, 4, u32::from_le_bytes(*b"LX\0\0")),
        ];
        let mut file = b"IIU\0\x08\0\0\0".to_vec();
        file.extend((entries.len() as u16).to_le_bytes());
        for e in entries {
            file.extend(e);
        }
        file.extend(0u32.to_le_bytes());
        file.resize(256, 0);
        file.extend_from_slice(data);
        file
    }

    /// MSB-first field writer into a 128-bit block (bit 127 down), stored little-endian.
    struct Block {
        word: u128,
        left: u32,
    }

    impl Block {
        fn put(&mut self, v: u32, n: u32) {
            self.left -= n;
            self.word |= u128::from(v) << self.left;
        }
    }

    /// Seeds 0x10/5 and 0x20/3; scales 0 (step 1), 3 (step 16, absolute), 1 (step 2), 2 (step 4).
    fn format4_block() -> ([u8; 16], [u16; 14]) {
        let mut b = Block { word: 0, left: 128 };
        b.put(0x10, 8);
        b.put(0x5, 4); // i0 = 261
        b.put(0x20, 8);
        b.put(0x3, 4); // i1 = 515
        b.put(0, 2);
        for j in [130, 0, 120] {
            b.put(j, 8); // i2 = 263, i3 = 515 (kept), i4 = 255
        }
        b.put(3, 2);
        for j in [40, 20, 0] {
            b.put(j, 8); // i5 = 40*16 + (515 & 15) = 643, i6 = 20*16 + (255 & 15) = 335, i7 = 643
        }
        b.put(1, 2);
        for j in [129, 127, 128] {
            b.put(j, 8); // i8 = 337, i9 = 641, i10 = 337
        }
        b.put(2, 2);
        for j in [130, 126, 0] {
            b.put(j, 8); // i11 = 649, i12 = 126*4 + (337 & 3) = 505 (337 < 128*4: absolute), i13 = 649
        }
        assert_eq!(b.left, 0);
        (b.word.to_le_bytes(), [261, 515, 263, 515, 255, 643, 335, 643, 337, 641, 337, 649, 505, 649])
    }
}

// ---- ORF (copied from tests/develop_orf.rs) -----------------------------------------------------
mod orf {
    use hsk_studio_develop::*;

    pub(super) fn seeds() -> Vec<(&'static str, Vec<u8>)> {
        // orf_two_field_layout_decodes_and_hostile_variants_are_typed: first call, unwrap.
        vec![("orf_two_field_12bit", orf(true, true, 1, (W as u32, H as u32)))]
    }

    const W: usize = 8;
    const H: usize = 5;

    fn px(x: usize, y: usize) -> u16 {
        (y * 640 + x * 37 + 5) as u16
    }

    fn packed_row(y: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for x in (0..W).step_by(2) {
            let (a, b) = (px(x, y), px(x + 1, y));
            out.extend([(a >> 4) as u8, (((a & 15) << 4) | (b >> 8)) as u8, b as u8]);
        }
        out
    }

    /// Little-endian IFD entry.
    fn entry(tag: u16, ty: u16, count: u32, value: u32) -> Vec<u8> {
        [tag.to_le_bytes().to_vec(), ty.to_le_bytes().to_vec(), count.to_le_bytes().to_vec(), value.to_le_bytes().to_vec()].concat()
    }

    /// `gap`: leave the seam gap between the fields; `cfa`: write the Exif CFAPattern; `compression`.
    fn orf(gap: bool, cfa: bool, compression: u16, dims: (u32, u32)) -> Vec<u8> {
        let rb = W * 3 / 2;
        // Data area after the IFDs at 512: A1 (rows 0, 2), A2 (row 4), [gap 7 bytes], B (rows 1, 3), then
        // the padding row of the second field.
        let base = 512usize;
        let a1 = base;
        let a2 = a1 + 2 * rb;
        let b = a2 + rb + if gap { 7 } else { 0 };
        let mut data = vec![0xffu8; b + 3 * rb - base];
        for (at, y) in [(a1, 0), (a1 + rb, 2), (a2, 4), (b, 1), (b + rb, 3)] {
            data[at - base..at - base + rb].copy_from_slice(&packed_row(y));
        }
        let offsets_at = 300u32;
        let counts_at = 320u32;
        let exif_at = 400u32;
        let mut ifd = Vec::new();
        let entries = [
            entry(256, 4, 1, dims.0),
            entry(257, 4, 1, dims.1),
            entry(258, 3, 1, 12),
            entry(259, 3, 1, u32::from(compression)),
            entry(262, 3, 1, 32803),
            entry(272, 2, 4, u32::from_le_bytes(*b"T1\0\0")),
            entry(273, 4, 3, offsets_at),
            entry(277, 3, 1, 1),
            entry(278, 4, 1, 2),
            entry(279, 4, 3, counts_at),
            entry(34665, 4, 1, exif_at),
        ];
        ifd.extend((entries.len() as u16).to_le_bytes());
        for e in entries {
            ifd.extend(e);
        }
        ifd.extend(0u32.to_le_bytes());
        let mut file = b"IIRS\x08\0\0\0".to_vec();
        file.extend(ifd);
        file.resize(offsets_at as usize, 0);
        for v in [a1, a2, b] {
            file.extend((v as u32).to_le_bytes());
        }
        file.resize(counts_at as usize, 0);
        for v in [2 * rb, rb, 2 * rb] {
            file.extend((v as u32).to_le_bytes());
        }
        file.resize(exif_at as usize, 0);
        if cfa {
            file.extend(1u16.to_le_bytes());
            file.extend(entry(0xa302, 7, 8, 420));
            file.extend(0u32.to_le_bytes());
            file.resize(420, 0);
            file.extend([2, 0, 2, 0, 1, 0, 2, 1]);
        } else {
            file.extend(0u16.to_le_bytes());
            file.extend(0u32.to_le_bytes());
        }
        file.resize(base, 0);
        file.extend(data);
        file
    }
}
