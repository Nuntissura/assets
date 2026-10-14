//! Nikon NEF container tests (lane SMB-DEVELOP-RAW-VENDOR slice 2, uncompressed variant). Inputs
//! are synthesized in memory: a TIFF with IFD0 Make `NIKON CORPORATION` and a raw SubIFD
//! (NewSubfileType 0, Photometric 32803, TIFF/EP CFA tags, uncompressed strips), the layout of the
//! public NEF notes and the CC0 D1H sample cited in `src/nef.rs`.
use hsk_studio_develop::*;

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

#[test]
fn nef_uncompressed_packed_and_16bit_strips_decode() {
    for spec in [Nef::default(), Nef { bits: 16, ..Nef::default() }, Nef { bits: 14, width: 7, rows_per_strip: 1, ..Nef::default() }] {
        let raw = decode_container(&build(&spec)).unwrap();
        assert_eq!((raw.width, raw.height, raw.bits_per_sample), (spec.width, spec.height, spec.bits));
        assert_eq!(raw.white_level, (1u32 << spec.bits) - 1);
        for y in 0..spec.height {
            for x in 0..spec.width {
                assert_eq!(raw.sample(x, y), source(x, y, spec.bits), "bits {} site ({x},{y})", spec.bits);
            }
        }
        assert_eq!(raw.cfa_pattern, CfaPattern::Bayer(Bayer::Bggr));
        assert_eq!(raw.unique_camera_model.as_deref(), Some("D1H"));
        assert_eq!(raw.unapplied, ["nef_black_level_unread", "nef_white_level_unread", "nef_makernote_curve_unread"]);
        assert_eq!(raw.require_complete(), Err(DevelopError::UnsupportedContribution("nef_black_level_unread")));
        assert_eq!(raw.solve_colour().err(), Some(DevelopError::MissingColourData));
    }
    // Little-endian 8/16-bit is byte aligned and decodes.
    let le = decode_container(&build(&Nef { big_endian: false, bits: 16, ..Nef::default() })).unwrap();
    assert_eq!(le.sample(5, 3), source(5, 3, 16));
}

#[test]
fn nef_nikon_compressions_and_unverified_variants_are_typed_refusals() {
    let refuse = |spec: Nef, error: DevelopError| {
        assert_eq!(decode_container(&build(&spec)).err(), Some(error));
    };
    refuse(Nef { compression: 34713, ..Nef::default() }, DevelopError::UnsupportedCompression);
    refuse(Nef { compression: 32769, ..Nef::default() }, DevelopError::UnsupportedCompression);
    // Little-endian packed 12-bit: bit order not verified, refused.
    refuse(Nef { big_endian: false, ..Nef::default() }, DevelopError::UnsupportedContainer);
    // Not a vendor this crate decodes (Pentax), or a DNG converted from a Nikon file: never decoded
    // as NEF (DNG reader decides).
    refuse(Nef { make: b"PENTAX\0\0", ..Nef::default() }, DevelopError::UnsupportedContainer);
    assert!(decode_container(&build(&Nef { dng_version: true, ..Nef::default() })).is_err());
    // A strip shorter than its rows.
    let mut short = build(&Nef::default());
    short.truncate(short.len() - 1);
    assert_eq!(decode_container(&short).err(), Some(DevelopError::InvalidContainer));
}

/// V4-02 dims_u32_max: declared dimensions at and over the u32 / pixel caps are refused before the
/// frame is allocated (the file holds a few bytes of payload).
#[test]
fn nef_dims_u32_max_refused_before_allocation() {
    for (width, height) in [(u32::MAX, 4), (4, u32::MAX), (65_535, 65_535), (70_000, 2)] {
        let spec = Nef { width: 6, height: 4, ..Nef::default() };
        let mut bytes = build(&spec);
        // Patch the raw IFD width/height values (big-endian LONG at entry 1 and 2 of the raw IFD).
        let raw_at = 8 + 2 + 12 * 3 + 4;
        bytes[raw_at + 2 + 12 + 8..raw_at + 2 + 12 + 12].copy_from_slice(&width.to_be_bytes());
        bytes[raw_at + 2 + 24 + 8..raw_at + 2 + 24 + 12].copy_from_slice(&height.to_be_bytes());
        assert_eq!(decode_container(&bytes).err(), Some(DevelopError::InvalidDimensions), "{width}x{height}");
    }
}

/// V4-02 aggregate_over_budget: 4096 one-row strips that all alias the same 6144 bytes declare a
/// 16-Mpixel frame; the running payload is capped by the file length, so it is refused before the
/// frame is allocated.
#[test]
fn nef_aggregate_over_budget_refused_before_allocation() {
    let spec = Nef { width: 4096, height: 4096, rows_per_strip: 1, alias_payload: Some(6144), ..Nef::default() };
    assert_eq!(decode_container(&build(&spec)).err(), Some(DevelopError::InvalidDimensions));
}
