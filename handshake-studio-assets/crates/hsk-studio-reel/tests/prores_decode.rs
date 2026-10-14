//! ProRes decoder written from SMPTE RDD 36 (cargo feature `prores`, default off): header facts,
//! external-oracle correctness, route and receipts, and V4-02 hostile-size proofs. Own test binary
//! (counting allocator).
//!
//! Fixtures `tests/fixtures/prores/` (provenance.json): synthetic frames written by ffmpeg 8.1.1's
//! independent `prores_ks` encoder from lavfi patterns, that ffmpeg's decode of them as the reference
//! (`*.ref.yuv`), and the encoder input (`source-*.yuv`) as a second oracle. No test runs ffmpeg.
//!
//! Tolerance (RDD 36 7.4: any IDCT passing Annex A is conformant, so two decoders may differ by
//! rounding): `TOL(b) = ceil(0.30 * 2^b / 512)` = 1 at b = 10 and 3 at b = 12, with the measured
//! maximum ratcheted at 1. A mutation control proves the comparison has power.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_reel::*;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    io::Cursor,
};

thread_local! {
    static LARGEST: Cell<usize> = const { Cell::new(0) };
}

fn note(size: usize) {
    let _ = LARGEST.try_with(|largest| largest.set(largest.get().max(size)));
}

struct Tracking;
// SAFETY: every method forwards to the system allocator unchanged; the counter only observes.
unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Tracking = Tracking;

#[cfg_attr(not(feature = "prores"), allow(dead_code))]
fn largest_during<T>(body: impl FnOnce() -> T) -> (T, usize) {
    LARGEST.with(|l| l.set(0));
    let out = body();
    (out, LARGEST.with(Cell::get))
}

#[cfg_attr(not(feature = "prores"), allow(dead_code))]
struct Fixture {
    name: &'static str,
    mov: &'static [u8],
    reference: &'static [u8],
    source: &'static [u8],
    fourcc: &'static str,
    bit_depth: u8,
    chroma: ChromaFormat,
}

const SOURCE_422: &[u8] = include_bytes!("fixtures/prores/source-422-10bit-40x24.yuv");
const SOURCE_444: &[u8] = include_bytes!("fixtures/prores/source-444-10bit-40x24.yuv");

const FIXTURES: [Fixture; 5] = [
    Fixture {
        name: "prores-422hq-40x24-p",
        mov: include_bytes!("fixtures/prores/prores-422hq-40x24-p.mov"),
        reference: include_bytes!("fixtures/prores/prores-422hq-40x24-p.ref.yuv"),
        source: SOURCE_422,
        fourcc: "apch",
        bit_depth: 10,
        chroma: ChromaFormat::Yuv422,
    },
    Fixture {
        name: "prores-422proxy-40x24-mbs1",
        mov: include_bytes!("fixtures/prores/prores-422proxy-40x24-mbs1.mov"),
        reference: include_bytes!("fixtures/prores/prores-422proxy-40x24-mbs1.ref.yuv"),
        source: SOURCE_422,
        fourcc: "apco",
        bit_depth: 10,
        chroma: ChromaFormat::Yuv422,
    },
    Fixture {
        name: "prores-422lt-40x24-tff",
        mov: include_bytes!("fixtures/prores/prores-422lt-40x24-tff.mov"),
        reference: include_bytes!("fixtures/prores/prores-422lt-40x24-tff.ref.yuv"),
        source: SOURCE_422,
        fourcc: "apcs",
        bit_depth: 10,
        chroma: ChromaFormat::Yuv422,
    },
    Fixture {
        name: "prores-422std-40x24-bff",
        mov: include_bytes!("fixtures/prores/prores-422std-40x24-bff.mov"),
        reference: include_bytes!("fixtures/prores/prores-422std-40x24-bff.ref.yuv"),
        source: SOURCE_422,
        fourcc: "apcn",
        bit_depth: 10,
        chroma: ChromaFormat::Yuv422,
    },
    Fixture {
        name: "prores-4444-40x24",
        mov: include_bytes!("fixtures/prores/prores-4444-40x24.mov"),
        reference: include_bytes!("fixtures/prores/prores-4444-40x24.ref.yuv"),
        source: SOURCE_444,
        fourcc: "ap4h",
        bit_depth: 12,
        chroma: ChromaFormat::Yuv444,
    },
];
#[cfg_attr(not(feature = "prores"), allow(dead_code))]
const W: u32 = 40;
#[cfg_attr(not(feature = "prores"), allow(dead_code))]
const H: u32 = 24;
const FRAMES: u64 = 4;
/// 25 fps in Pulse ticks.
#[cfg_attr(not(feature = "prores"), allow(dead_code))]
const TICKS_PER_FRAME: u64 = 10_160_640_000;

#[derive(Debug, Default)]
struct Collect {
    frames: Vec<(i64, Vec<u8>)>,
}

impl ChunkSink for Collect {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
        self.frames.push((c.pts, c.bytes.to_vec()));
        Ok(())
    }
}

fn request(bytes: &[u8], output: OutputMode) -> SegmentRequest {
    SegmentRequest {
        codec: "isobmff".to_owned(),
        start_pts: Ticks::new(0),
        count: FRAMES,
        track: None,
        output,
        prefer: SourcePreference::Original,
        purpose: Purpose::Preview,
        grant: Grant {
            content_id: ContentId::parse("prores-fixture").unwrap(),
            revision: 1,
            expected_len: bytes.len() as u64,
        },
        expected_revision: 1,
    }
}

/// Decode through the public registry on the DEFAULT route (untrusted source, no external decoder).
fn decode(bytes: &[u8], output: OutputMode) -> Result<(SegmentReceipt, Collect), ReelError> {
    let mut sink = Collect::default();
    let receipt = Registry::standard().decode_segment(
        &mut Cursor::new(bytes.to_vec()),
        None,
        &request(bytes, output),
        &Limits::default(),
        &CancellationToken::default(),
        &mut sink,
    )?;
    Ok((receipt, sink))
}

#[cfg_attr(not(feature = "prores"), allow(dead_code))]
fn packets(bytes: &[u8]) -> Vec<Vec<u8>> {
    decode(bytes, OutputMode::Packets)
        .unwrap()
        .1
        .frames
        .into_iter()
        .map(|(_, b)| b)
        .collect()
}

#[cfg_attr(not(feature = "prores"), allow(dead_code))]
fn samples(bytes: &[u8]) -> Vec<i32> {
    bytes
        .chunks_exact(2)
        .map(|p| i32::from(u16::from_le_bytes([p[0], p[1]])))
        .collect()
}

/// The build without feature `prores` keeps ProRes a typed unavailable codec (D24: opt-in), and the
/// descriptor says so; ProRes RAW is refused in every build.
#[cfg(not(feature = "prores"))]
#[test]
fn prores_default_build_is_typed_unavailable() {
    for f in &FIXTURES {
        assert_eq!(
            decode(f.mov, OutputMode::Decoded).unwrap_err(),
            ReelError::CodecUnavailable {
                codec: f.fourcc.to_owned()
            }
        );
    }
    let d = prores_descriptor();
    assert!(!d.can_decode);
    assert_eq!(d.name, "ProRes (RDD 36 decoder, not Apple-certified)");
    assert_eq!(d.certification, Certification::NotCertified);
    let raw = FIXTURES[0].mov.to_vec();
    let raw = replace_fourcc(&raw, b"apch", b"aprn");
    assert_eq!(
        decode(&raw, OutputMode::Decoded).unwrap_err(),
        ReelError::Unsupported { what: "prores-raw" }
    );
}

/// Rewrite the sample entry code (every occurrence; the fixture holds it once in `stsd`).
fn replace_fourcc(bytes: &[u8], from: &[u8; 4], to: &[u8; 4]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let mut hits = 0;
    for i in 0..out.len() - 3 {
        if &out[i..i + 4] == from {
            out[i..i + 4].copy_from_slice(to);
            hits += 1;
        }
    }
    assert_eq!(hits, 1, "sample entry code occurs once");
    out
}

/// Header facts read from a hexdump of the fixture bytes (not from the decoder under test).
#[cfg(feature = "prores")]
#[test]
fn prores_header_and_slice_table_exact() {
    let hq = packets(FIXTURES[0].mov);
    assert_eq!(
        hq.iter().map(Vec::len).collect::<Vec<_>>(),
        [986, 994, 987, 998]
    );
    let h = parse_prores_frame_header(&hq[0]).unwrap();
    assert_eq!(h.frame_size, 986);
    assert_eq!(h.frame_header_size, 148);
    assert_eq!(h.bitstream_version, 0);
    assert_eq!(&h.encoder_identifier, b"apl0");
    assert_eq!((h.width, h.height), (40, 24));
    assert_eq!(
        (h.chroma_format, h.interlace_mode, h.alpha_channel_type),
        (2, 0, 0)
    );
    assert!(h.luma_matrix_loaded && h.chroma_matrix_loaded);
    assert!(h.luma_matrix.iter().flatten().all(|w| (4..=7).contains(w)));
    // Picture header right after the frame header: size 8, picture_size 830, log2 slice size 3.
    let pic = &hq[0][8 + 148..];
    assert_eq!(pic[0] >> 3, 8);
    assert_eq!(u32::from_be_bytes(pic[1..5].try_into().unwrap()), 830);
    assert_eq!((pic[7] >> 4) & 3, 3);
    let proxy = packets(FIXTURES[1].mov);
    assert_eq!(proxy[0].len(), 392);
    let pic = &proxy[0][8 + usize::from(
        parse_prores_frame_header(&proxy[0])
            .unwrap()
            .frame_header_size,
    )..];
    assert_eq!(u32::from_be_bytes(pic[1..5].try_into().unwrap()), 236);
    assert_eq!((pic[7] >> 4) & 3, 0);
    // Interlaced: TFF header byte 0x84 (interlace_mode 1), BFF 0x88 (2); 4444: 0xC0, version 1.
    assert_eq!(packets(FIXTURES[2].mov)[0][20], 0x84);
    assert_eq!(packets(FIXTURES[3].mov)[0][20], 0x88);
    let x = packets(FIXTURES[4].mov);
    assert_eq!(x[0][20] & 0xC0, 0xC0);
    assert_eq!(
        parse_prores_frame_header(&x[0]).unwrap().bitstream_version,
        1
    );
}

#[cfg(feature = "prores")]
#[test]
fn prores_decode_matches_external_oracle_within_tolerance() {
    let cancel = CancellationToken::default();
    for f in &FIXTURES {
        let tol = if f.bit_depth == 12 { 3 } else { 1 };
        let (receipt, sink) = decode(f.mov, OutputMode::Decoded).expect(f.name);
        assert_eq!(sink.frames.len() as u64, FRAMES, "{}", f.name);
        let format = receipt.video.unwrap();
        assert_eq!(
            (format.bit_depth, format.chroma),
            (f.bit_depth, f.chroma),
            "{}",
            f.name
        );
        let frame_bytes = format.frame_bytes().unwrap();
        assert_eq!(
            f.reference.len(),
            frame_bytes * FRAMES as usize,
            "{}",
            f.name
        );
        let theirs = samples(f.reference);
        let ours: Vec<i32> = sink.frames.iter().flat_map(|(_, b)| samples(b)).collect();
        let (mut max, mut differ) = (0, 0);
        for (a, b) in ours.iter().zip(&theirs) {
            max = max.max((a - b).abs());
            differ += usize::from(a != b);
        }
        println!(
            "{}: max |delta| {max}, {differ} of {} samples differ",
            f.name,
            ours.len()
        );
        assert!(max <= tol, "{}: max {max} above TOL {tol}", f.name);
        assert!(max <= 1, "{}: ratchet: measured maximum was 1", f.name);
        // Second oracle: the encoder input (10-bit source; 12-bit output compared at source scale).
        let source = samples(f.source);
        let shift = f.bit_depth - 10;
        let mse = |a: &[i32]| {
            a.iter()
                .zip(&source)
                .map(|(x, y)| {
                    let d = f64::from(*x) / f64::from(1 << shift) - f64::from(*y);
                    d * d
                })
                .sum::<f64>()
                / a.len() as f64
        };
        println!(
            "{}: MSE vs encoder source (10-bit scale): ours {:.4}, ffmpeg {:.4}",
            f.name,
            mse(&ours),
            mse(&theirs)
        );
        // Mutation control: single-bit flips in the slice data of frame 0 must mostly show.
        let frame0 = &packets(f.mov)[0];
        let h = parse_prores_frame_header(frame0).unwrap();
        let data_start = 8 + usize::from(h.frame_header_size) + 8 + 16;
        let span = ((frame0.len() - data_start) * 8) as u64;
        let theirs0 = &theirs[..frame_bytes / 2];
        let flips = 256u64;
        let mut caught = 0;
        for k in 0..flips {
            let bit = (k * 7919 + 13) % span;
            let mut m = frame0.clone();
            m[data_start + (bit / 8) as usize] ^= 0x80 >> (bit % 8);
            match decode_prores_frame(&m, (W, H), &Limits::default(), &cancel) {
                Err(_) => caught += 1,
                Ok((_, planes)) => {
                    let o = samples(&planes.concat());
                    if o.iter().zip(theirs0).any(|(a, b)| (a - b).abs() > tol) {
                        caught += 1;
                    }
                }
            }
        }
        println!("{}: mutation control caught {caught} of {flips}", f.name);
        assert!(caught * 100 >= flips * 60, "{}: {caught}", f.name);
    }
}

#[cfg(feature = "prores")]
#[test]
fn prores_container_route_and_receipts() {
    for f in &FIXTURES {
        let (receipt, sink) = decode(f.mov, OutputMode::Decoded).unwrap();
        assert_eq!(receipt.decoder_id, "prores-rdd36");
        assert_eq!(receipt.decoder_version, PRORES_DECODER_VERSION);
        assert_eq!(receipt.codec, f.fourcc);
        let format = receipt.video.unwrap();
        assert_eq!((format.width, format.height), (W, H));
        assert!(!format.color.full_range);
        for (n, (pts, _)) in sink.frames.iter().enumerate() {
            assert_eq!(*pts as u64, n as u64 * TICKS_PER_FRAME, "{}", f.name);
        }
        let (_, again) = decode(f.mov, OutputMode::Decoded).unwrap();
        assert!(again.frames == sink.frames, "deterministic");
    }
    let d = prores_descriptor();
    assert!(d.can_decode);
    assert_eq!(d.name, PRORES_DISPLAY_NAME);
    assert!(!d.name.contains("Apple ProRes"));
    let raw = replace_fourcc(FIXTURES[0].mov, b"apch", b"aprh");
    assert_eq!(
        decode(&raw, OutputMode::Decoded).unwrap_err(),
        ReelError::Unsupported { what: "prores-raw" }
    );
    // The frame header wins over the sample entry code: an ap4h-labelled 4:2:2 stream decodes 4:2:2.
    let relabelled = replace_fourcc(FIXTURES[0].mov, b"apch", b"ap4h");
    let (receipt, _) = decode(&relabelled, OutputMode::Decoded).unwrap();
    assert_eq!(receipt.video.unwrap().chroma, ChromaFormat::Yuv422);
}

#[cfg(feature = "prores")]
#[test]
fn prores_dims_u16_max_and_aggregate_over_budget() {
    let cancel = CancellationToken::default();
    let limits = Limits::default();
    let frame = packets(FIXTURES[0].mov).remove(0);
    let one_frame = (W * H * 2 * 2) as usize + 64 * 1024;
    let corrupt = |what| ReelError::Corrupt { what };
    // dims_u16_max: 65535 x 65535 against an undeclared size (cap 16384) and a declared 40x24.
    let mut huge = frame.clone();
    huge[8 + 8..8 + 12].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    for declared in [(0, 0), (W, H)] {
        let (r, largest) =
            largest_during(|| decode_prores_frame(&huge, declared, &limits, &cancel));
        assert_eq!(r.unwrap_err(), corrupt("prores-size-mismatch"));
        assert!(largest <= one_frame, "{largest}");
    }
    // 16384 x 16384 undeclared: refused by the chunk budget before any plane exists.
    let mut big = frame.clone();
    big[8 + 8..8 + 12].copy_from_slice(&[0x40, 0x00, 0x40, 0x00]);
    let (r, largest) = largest_during(|| decode_prores_frame(&big, (0, 0), &limits, &cancel));
    assert_eq!(
        r.unwrap_err(),
        ReelError::LimitExceeded {
            what: "prores-frame-bytes"
        }
    );
    assert!(largest <= one_frame);
    // Aggregate: picture_size past the frame; slice sizes whose sum exceeds the picture.
    let pic = 8 + 148;
    let mut p = frame.clone();
    p[pic + 1..pic + 5].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(
        decode_prores_frame(&p, (W, H), &limits, &cancel).unwrap_err(),
        corrupt("prores-picture-size")
    );
    let mut s = frame.clone();
    s[pic + 8..pic + 10].copy_from_slice(&0xFFFFu16.to_be_bytes());
    assert_eq!(
        decode_prores_frame(&s, (W, H), &limits, &cancel).unwrap_err(),
        corrupt("prores-slice-size")
    );
    // frame_size larger than the packet; wrong identifier; version 2; quantization index 0 and 225.
    let mut fs = frame.clone();
    fs[0..4].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(
        decode_prores_frame(&fs, (W, H), &limits, &cancel).unwrap_err(),
        corrupt("prores-frame-size")
    );
    let mut id = frame.clone();
    id[4] = b'x';
    assert_eq!(
        decode_prores_frame(&id, (W, H), &limits, &cancel).unwrap_err(),
        corrupt("prores-frame-identifier")
    );
    let mut v2 = frame.clone();
    v2[8 + 3] = 2;
    assert_eq!(
        decode_prores_frame(&v2, (W, H), &limits, &cancel).unwrap_err(),
        ReelError::Unsupported {
            what: "prores-bitstream-version"
        }
    );
    // First slice header: after the picture header (8) and the slice table (2 rows x 2 slices x 2).
    let slice0 = pic + 8 + 8;
    for q in [0u8, 225] {
        let mut m = frame.clone();
        m[slice0 + 1] = q;
        assert_eq!(
            decode_prores_frame(&m, (W, H), &limits, &cancel).unwrap_err(),
            corrupt("prores-quantization-index")
        );
    }
    // Every truncation and every single-bit flip: Ok or a typed error, never a panic, never an
    // allocation above one output frame plus 64 KiB.
    for len in 0..frame.len() {
        let (r, largest) =
            largest_during(|| decode_prores_frame(&frame[..len], (W, H), &limits, &cancel));
        assert!(r.is_err(), "truncated to {len}");
        assert!(largest <= one_frame);
    }
    for bit in 0..frame.len() * 8 {
        let mut m = frame.clone();
        m[bit / 8] ^= 0x80 >> (bit % 8);
        let (_, largest) = largest_during(|| decode_prores_frame(&m, (W, H), &limits, &cancel));
        assert!(largest <= one_frame, "bit {bit}: {largest}");
    }
}
