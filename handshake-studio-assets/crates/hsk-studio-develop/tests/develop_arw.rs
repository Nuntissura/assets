//! Sony ARW container tests (lane SMB-DEVELOP-RAW-ARW). Inputs are synthesized in memory: a
//! little-endian TIFF with IFD0 Make `SONY` and a raw SubIFD (Photometric 32803, TIFF/EP CFA tags,
//! SonyToneCurve 0x7010) holding a cRAW stream (Compression 32767, one byte per pixel) built by a
//! test-local encoder of the block layout described in `src/arw.rs`, or uncompressed 16-bit strips.
use hsk_studio_develop::*;

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

fn samples(raw: &DngRaw) -> Vec<u16> {
    (0..raw.height).flat_map(|y| (0..raw.width).map(move |x| (x, y))).map(|(x, y)| raw.sample(x, y)).collect()
}

#[test]
fn arw_craw_decodes_groups_spans_and_curve() {
    let (bytes, expected) = build(&Arw::default());
    let raw = decode_container(&bytes).unwrap();
    assert_eq!((raw.width, raw.height, raw.bits_per_sample), (64, 2, 14));
    assert_eq!(samples(&raw), expected);
    // Every shift 0..=4 occurs (8 groups, g % 5) and the curve's top segment is reached.
    assert_eq!(raw.white_level, (curve(POINTS)[4094] >> 2) << 2);
    assert_eq!(raw.white_level, 17204);
    assert_eq!(raw.cfa_pattern, CfaPattern::Bayer(Bayer::Rggb));
    assert_eq!(raw.unique_camera_model.as_deref(), Some("T7S"));
    assert_eq!(raw.unapplied, ["arw_black_level_unread", "arw_white_level_unread"]);
    assert_eq!(raw.require_complete(), Err(DevelopError::UnsupportedContribution("arw_black_level_unread")));
    assert_eq!(raw.solve_colour().err(), Some(DevelopError::MissingColourData));
    // Uncompressed 16-bit strips.
    let (bytes, expected) = build(&Arw { compression: 1, ..Arw::default() });
    let raw = decode_container(&bytes).unwrap();
    assert_eq!((raw.bits_per_sample, raw.white_level), (16, 65535));
    assert_eq!(samples(&raw), expected);
}

#[test]
fn arw_unknown_variants_are_typed_refusals() {
    let refuse = |spec: Arw, error: DevelopError| {
        assert_eq!(decode_container(&build(&spec).0).err(), Some(error));
    };
    // Not one byte per pixel (ARW1 or unknown), ragged width, other compressions.
    refuse(Arw { count: Some(64 * 2 + 16), payload: None, ..Arw::default() }, DevelopError::UnsupportedCompression);
    refuse(Arw { width: 48, ..Arw::default() }, DevelopError::UnsupportedCompression);
    refuse(Arw { compression: 7, ..Arw::default() }, DevelopError::UnsupportedCompression);
    // cRAW without its tone curve.
    refuse(Arw { curve: false, ..Arw::default() }, DevelopError::InvalidContainer);
    // A DNG converted from an ARW is not decoded as ARW (DNG reader decides).
    assert!(decode_container(&build(&Arw { dng_version: true, ..Arw::default() }).0).is_err());
}

/// V4-02 dims_u32_max: dimensions at the u32 and pixel caps are refused before allocation.
#[test]
fn arw_dims_u32_max_refused_before_allocation() {
    let (bytes, _) = build(&Arw::default());
    let raw_at = 8 + 2 + 12 * 3 + 4;
    for (w, h) in [(u32::MAX, 2u32), (64, u32::MAX), (65_504, 65_535)] {
        let mut b = bytes.clone();
        b[raw_at + 2 + 12 + 8..raw_at + 2 + 12 + 12].copy_from_slice(&w.to_le_bytes());
        b[raw_at + 2 + 24 + 8..raw_at + 2 + 24 + 12].copy_from_slice(&h.to_le_bytes());
        assert_eq!(decode_container(&b).err(), Some(DevelopError::InvalidDimensions), "{w}x{h}");
    }
}

/// V4-02 aggregate_over_budget: a 4096 x 4096 cRAW frame (count = one byte per pixel) in a file
/// holding 64 payload bytes is refused before the 16-Mpixel frame is allocated.
#[test]
fn arw_aggregate_over_budget_refused_before_allocation() {
    let (bytes, _) = build(&Arw { payload: Some(64), ..Arw::default() });
    let raw_at = 8 + 2 + 12 * 3 + 4;
    let mut b = bytes;
    b[raw_at + 2 + 12 + 8..raw_at + 2 + 12 + 12].copy_from_slice(&4096u32.to_le_bytes());
    b[raw_at + 2 + 24 + 8..raw_at + 2 + 24 + 12].copy_from_slice(&4096u32.to_le_bytes());
    // StripByteCounts is entry 7 of the raw IFD.
    b[raw_at + 2 + 84 + 8..raw_at + 2 + 84 + 12].copy_from_slice(&(4096u32 * 4096).to_le_bytes());
    assert_eq!(decode_container(&b).err(), Some(DevelopError::InvalidContainer));
}
