//! Canon CR2 container tests (lane SMB-DEVELOP-RAW-VENDOR slice 1). Inputs are synthesized in
//! memory by a test-local lossless-JPEG encoder (ITU-T T.81 SOF3, predictor 1) and a minimal CR2
//! writer (TIFF header + `CR` 2.0 + raw IFD with Compression 6, one strip and RawImageSegmentation
//! 0xC640), following the public CR2 notes cited in `src/cr2.rs`.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::color::{self, Mat3};
use hsk_studio_develop::*;
use hsk_studio_pigment::Grid;
use sha2::{Digest, Sha256};

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

fn profile() -> ProfileBinding {
    ProfileBinding {
        profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(),
        sha256: [7; 32],
    }
}

#[test]
fn cr2_decodes_and_reassembles_column_slices() {
    let stream = sliced_stream();
    let bytes = cr2(&stream, &Cr2::default());
    let mut raw = decode_container(&bytes).unwrap();
    assert_eq!((raw.width, raw.height, raw.bits_per_sample, raw.white_level), (24, 4, 12, 4095));
    for y in 0..H {
        for x in 0..W {
            assert_eq!(raw.sample(x as u32, y as u32), source(x, y), "site ({x},{y})");
        }
    }
    assert_eq!(raw.cfa_pattern, CfaPattern::Bayer(Bayer::Rggb));
    assert_eq!(raw.unique_camera_model.as_deref(), Some("T1X"));
    assert_eq!(raw.container_sha256, <[u8; 32]>::from(Sha256::digest(&bytes)));
    assert_eq!(
        raw.unapplied,
        ["cr2_black_level_unread", "cr2_white_level_unread", "cr2_sensor_borders_unread", "cr2_cfa_pattern_unverified"]
    );
    assert_eq!(raw.require_complete(), Err(DevelopError::UnsupportedContribution("cr2_black_level_unread")));
    // Same stream without the slice tag: rows are taken as stored (no reassembly).
    let plain = decode_container(&cr2(&stream, &Cr2 { slices: None, ..Cr2::default() })).unwrap();
    // Stored row 0 = slice 0 rows 0..3; stored row 1 = slice 0 row 3, then slice 1 rows 0..2.
    assert_eq!(plain.sample(8, 0), source(0, 1));
    assert_eq!(plain.sample(0, 1), source(0, 3));
    assert_eq!(plain.sample(8, 1), source(8, 0));

    // No colour in a CR2: never a default matrix (STU-IO-003).
    let xyz2rgb = color::xyz_d50_to_linear_srgb().unwrap();
    assert_eq!(raw.solve_colour().err(), Some(DevelopError::MissingColourData));
    assert_eq!(
        raw.recipe(&xyz2rgb, profile(), BaselineExposure::Ignore).err(),
        Some(DevelopError::MissingColourData)
    );
    // Caller-supplied colour: the existing develop path produces an image.
    let mut caller = DngColor::empty();
    let cm: Mat3 = xyz2rgb;
    caller.color_matrix = [Some(cm), None];
    caller.calibration_illuminant = [Some(23), None];
    caller.as_shot_neutral = Some([1.0, 1.0, 1.0]);
    raw.color = caller;
    let (recipe, _) = raw.recipe(&xyz2rgb, profile(), BaselineExposure::Ignore).unwrap();
    let window = raw.default_window();
    assert_eq!(window, PlaneWindow { x: 0, y: 0, width: 24, height: 4 });
    let (plane, _) = raw.cfa_plane(window, RangePolicy::Refuse, 7, Limits::SPEC_V1).unwrap();
    let mut tiles = 0;
    develop(
        &DevelopRequest {
            plane: &plane,
            recipe: &recipe,
            recipe_revision: 1,
            expected_source_revision: 7,
            grid: Grid { origin_x: 0, origin_y: 0, tile_width: 8, tile_height: 4 },
            output_origin: (0, 0),
            execution_hint: ExecutionHint::Auto,
        },
        &CancellationToken::default(),
        &mut |_| {
            tiles += 1;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(tiles, 3);
}

#[test]
fn cr2_slice_mismatch_and_unknown_variants_are_typed_refusals() {
    let stream = sliced_stream();
    let refuse = |spec: Cr2, error: DevelopError| {
        assert_eq!(decode_container(&cr2(&stream, &spec)).err(), Some(error));
    };
    // Slice widths that do not sum to the 24-sample frame row.
    refuse(Cr2 { slices: Some([2, 8, 9]), ..Cr2::default() }, DevelopError::InvalidContainer);
    refuse(Cr2 { slices: Some([2, 8, 7]), ..Cr2::default() }, DevelopError::InvalidContainer);
    // Zero-width full slices.
    refuse(Cr2 { slices: Some([3, 0, 24]), ..Cr2::default() }, DevelopError::InvalidContainer);
    // A slice sum far above any raw width (65535 * 65535 + 65535) is refused before any decode.
    refuse(Cr2 { slices: Some([65535, 65535, 65535]), ..Cr2::default() }, DevelopError::InvalidDimensions);
    // Undocumented variants: other compression, CR minor version.
    refuse(Cr2 { compression: 7, ..Cr2::default() }, DevelopError::UnsupportedCompression);
    refuse(Cr2 { minor: 1, ..Cr2::default() }, DevelopError::UnsupportedContainer);
}

/// V4-02 dims_u32_max: frame headers at the u16 limits in a tiny file are refused before the
/// frame is allocated.
#[test]
fn cr2_dims_u32_max_refused_before_allocation() {
    let tiny = |per_line: u16, lines: u16, components: u8| {
        let mut s = ljpeg(&[], 12, per_line, lines, components);
        s.truncate(s.len() - 2);
        s.extend_from_slice(&[0u8; 32]);
        s
    };
    // Frame row 65535 * 4 samples: wider than any raw width.
    let wide = cr2(&tiny(65535, 65535, 4), &Cr2 { slices: None, ..Cr2::default() });
    assert_eq!(decode_container(&wide).err(), Some(DevelopError::InvalidDimensions));
    // Row 16383 * 4 = 65532 (allowed width), 65535 lines: above MAX_DNG_PIXELS.
    let tall = cr2(&tiny(16383, 65535, 4), &Cr2 { slices: Some([0, 0, 65532]), ..Cr2::default() });
    assert_eq!(decode_container(&tall).err(), Some(DevelopError::InvalidDimensions));
}

/// V4-02 aggregate_over_budget: a frame inside the pixel cap that the payload present cannot
/// encode (lossless JPEG spends at least one bit per sample) is refused before allocation; a
/// byte count beyond the file is a container error.
#[test]
fn cr2_aggregate_over_budget_refused_before_allocation() {
    let mut s = ljpeg(&[], 12, 4096, 16384, 4);
    s.truncate(s.len() - 2);
    s.extend_from_slice(&[0u8; 64]);
    let over = cr2(&s, &Cr2 { slices: Some([2, 8192, 0]), ..Cr2::default() });
    assert_eq!(decode_container(&over).err(), Some(DevelopError::InvalidDimensions));
    let stream = sliced_stream();
    let beyond = cr2(&stream, &Cr2 { byte_count: Some(stream.len() as u32 + 1), ..Cr2::default() });
    assert_eq!(decode_container(&beyond).err(), Some(DevelopError::InvalidContainer));
    // Truncated stream: payload too small for the declared 24x4 frame.
    let cut = &stream[..stream.len() / 2];
    assert!(decode_container(&cr2(cut, &Cr2::default())).is_err());
}

#[test]
fn container_magic_routing_names_the_family() {
    let unavailable = |bytes: &[u8], family: &'static str| {
        let error = decode_container(bytes).err().unwrap();
        assert_eq!(error, DevelopError::DecoderUnavailable(family));
        assert_eq!(error.code(), "decoder_unavailable");
        assert_eq!(error.to_string(), format!("decoder_unavailable:{family}"));
    };
    // ORF, RW2, RAF and CR3 magics now route to their decoders (SMB-DEVELOP-RAW-COMPRESSED): a file
    // that is only a magic prefix is a typed container refusal, never a decode.
    for magic in [&b"IIRO\x08\0\0\0"[..], b"IIRS\x08\0\0\0", b"MMOR\0\0\0\x08", b"IIU\0\x08\0\0\0", b"FUJIFILMCCD-RAW 0201FF383501"] {
        assert_eq!(decode_container(magic).err(), Some(DevelopError::InvalidContainer), "{magic:?}");
    }
    assert_eq!(decode_container(b"\0\0\0\x18ftypcrx \0\0\0\x01").err(), Some(DevelopError::UnsupportedContainer));
    unavailable(b"RIFF....WEBP", "unknown");
    unavailable(b"", "unknown");
    // TIFF without DNGVersion and without the CR marker (NEF/ARW today) stays unsupported.
    let mut tiff = b"II*\0\x08\0\0\0".to_vec();
    tiff.extend_from_slice(&1u16.to_le_bytes());
    tiff.extend_from_slice(&[0, 1, 3, 0, 1, 0, 0, 0, 4, 0, 0, 0]);
    tiff.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(decode_container(&tiff).err(), Some(DevelopError::UnsupportedContainer));
}
