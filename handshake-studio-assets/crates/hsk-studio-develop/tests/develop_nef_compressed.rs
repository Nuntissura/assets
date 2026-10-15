//! Nikon Huffman-compressed NEF (Compression 34713) through `decode_container`: synthetic files
//! built here (big-endian TIFF, CFA SubIFD, Exif maker note `Nikon\0` + embedded TIFF holding tag
//! 0x0096) and encoded by a test-local encoder following the documented scheme (canonical Huffman
//! category + T.81 additional bits, same-colour two-left predictor, per-parity row seeds). The
//! Huffman BITS/HUFFVAL data below is the decoder's table data (LightCraft, MIT OR Apache-2.0, see
//! crate NOTICE), restated so the encoder is independent of the decoder's code.
use hsk_studio_develop::*;

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
fn maker_note(table: &[u8]) -> Vec<u8> {
    let mut note = b"Nikon\0\x02\x10\0\0".to_vec();
    let t = Tiff { ifds: vec![vec![(0x0096, 7, table.len() as u32, table.to_vec())]] };
    note.extend(t.write(&[], None));
    note
}

fn nef(strip: Vec<u8>, w: u32, h: u32, bits: u16, table: Option<&[u8]>) -> Vec<u8> {
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
        let note = maker_note(table);
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

#[test]
fn nef_huffman_lossless_and_lossy_decode_to_the_source_cfa() {
    let (w, h) = (40usize, 6usize);
    for (bits, table) in [(12u16, LOSSLESS_12), (14, LOSSLESS_14)] {
        let max = (1i32 << bits) - 1;
        let mut img = image(w, h, max, u32::from(bits));
        // extreme jumps exercise the longest categories
        (img[5], img[7], img[w + 9]) = (0, max, max);
        let seed = 1u16 << (bits - 3);
        let file = nef(encode(&img, w, table, i32::from(seed)), w as u32, h as u32, bits, Some(&lossless_table(seed)));
        let raw = decode_container(&file).unwrap();
        assert_eq!((raw.width, raw.height, raw.bits_per_sample, raw.white_level), (40, 6, bits, max as u32));
        assert_eq!(raw.cfa_pattern, CfaPattern::Bayer(Bayer::Rggb));
        let got: Vec<i32> = (0..h as u32).flat_map(|y| (0..w as u32).map(move |x| (x, y))).map(|(x, y)| i32::from(raw.sample(x, y))).collect();
        assert_eq!(got, img, "{bits}-bit lossless");
        assert!(raw.unapplied.contains(&"nef_black_level_unread"));
        assert_eq!(raw.solve_colour().err(), Some(DevelopError::MissingColourData));
    }
    // Lossy type 2 (12-bit): decoded values index the interpolated curve (point k at code 16k is 12k).
    let img = image(w, h, 1023, 9); // lossy codes stay within category 10
    let file = nef(encode(&img, w, LOSSY_12, 300), w as u32, h as u32, 12, Some(&lossy_type2_table(300, 3500)));
    let raw = decode_container(&file).unwrap();
    let curve = |c: i32| -> i32 {
        let (k, r) = (c / 16, c % 16);
        let (a, b) = (k * 12, if k == 255 { 3500 } else { (k + 1) * 12 });
        (a * (16 - r) + b * r) / 16
    };
    let got: Vec<i32> = (0..h as u32).flat_map(|y| (0..w as u32).map(move |x| (x, y))).map(|(x, y)| i32::from(raw.sample(x, y))).collect();
    assert_eq!(got, img.iter().map(|&c| curve(c)).collect::<Vec<_>>());
    assert_eq!(raw.white_level, 3500, "lossy white is the curve's largest output");
}

#[test]
fn nef_huffman_hostile_inputs_are_typed_refusals_before_allocation() {
    let (w, h) = (64usize, 16usize);
    let img = image(w, h, 16383, 3);
    let table = lossless_table(2048);
    let strip = encode(&img, w, LOSSLESS_14, 2048);
    let ok = nef(strip.clone(), w as u32, h as u32, 14, Some(&table));
    assert!(decode_container(&ok).is_ok());
    // Truncated strip (declared byte count shorter than the data needs), and a strip cut in half.
    for cut in [10usize, strip.len() / 2] {
        let e = decode_container(&nef(strip[..cut].to_vec(), w as u32, h as u32, 14, Some(&table)));
        assert_eq!(e.err(), Some(DevelopError::InvalidContainer), "cut {cut}");
    }
    // Bad Huffman data: all-ones never forms a valid lossy 12-bit code word.
    let e = decode_container(&nef(vec![0xff; 400], 32, 32, 12, Some(&lossy_type2_table(300, 3500))));
    assert_eq!(e.err(), Some(DevelopError::InvalidContainer));
    // Dimension bomb: 65535 x 4096 declared over a 1 KiB strip is refused by the size check before
    // the frame is allocated (every code word is >= 2 bits).
    let e = decode_container(&nef(vec![0x55; 1024], 65_535, 4096, 14, Some(&table)));
    assert_eq!(e.err(), Some(DevelopError::InvalidContainer));
    let e = decode_container(&nef(vec![0x55; 1024], 65_536, 65_536, 14, Some(&table)));
    assert_eq!(e.err(), Some(DevelopError::InvalidDimensions));
    // No maker-note table (e.g. a High Efficiency NEF): typed, never guessed.
    let e = decode_container(&nef(strip.clone(), w as u32, h as u32, 14, None));
    assert_eq!(e.err(), Some(DevelopError::DecoderUnavailable("nef_compressed")));
    // Unknown table version and an unsupported bit depth.
    let mut odd = table.clone();
    odd[0] = 0x49;
    assert_eq!(decode_container(&nef(strip.clone(), w as u32, h as u32, 14, Some(&odd))).err(), Some(DevelopError::UnsupportedCompression));
    assert_eq!(decode_container(&nef(strip, w as u32, h as u32, 16, Some(&table))).err(), Some(DevelopError::UnsupportedCompression));
    // Runaway predictor: a long run of the largest difference drives values out of range; the
    // out-of-range count refuses the strip (and an i32 overflow would be a checked refusal too).
    let codes = codes(LOSSLESS_14);
    let mut wr = Writer::default();
    for _ in 0..40_000 {
        wr.put(codes[14].0, codes[14].1);
        wr.put((1 << 14) - 1, 14);
    }
    let e = decode_container(&nef(wr.finish(), 40_000, 1, 14, Some(&lossless_table(0))));
    assert_eq!(e.err(), Some(DevelopError::InvalidContainer));
}
