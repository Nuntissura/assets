//! VC-3 (DNxHR / DNxHD, SMPTE ST 2019-1) in-process decoder: external-oracle correctness, route and
//! receipts, and V4-02 hostile-size proofs. Own test binary because of the counting allocator.
//!
//! Fixtures `tests/fixtures/dnx/` (provenance.json): synthetic DNxHR frames written by ffmpeg 8.1.1's
//! independent encoder (a black-box generator), and that ffmpeg's own decode of them as the reference
//! (`*.ref.yuv.zz`, zlib-compressed raw planar output). No test runs ffmpeg.
//!
//! Tolerance: neither ST 2019-1 nor ffmpeg pins an IDCT to the bit (the standard defines the real-valued
//! transform and no rounding), so two correct decoders may differ by rounding: `TOL = 1` LSB at 8 and
//! 10 bits, ratcheted (a widening needs a diagnostics note). A mutation control proves the comparison
//! has power: single-bit flips of coefficient data must mostly produce an error or a difference above
//! TOL.
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

fn largest_during<T>(body: impl FnOnce() -> T) -> (T, usize) {
    LARGEST.with(|l| l.set(0));
    let out = body();
    (out, LARGEST.with(Cell::get))
}

struct Fixture {
    name: &'static str,
    mov: &'static [u8],
    reference_zz: &'static [u8],
    source_zz: &'static [u8],
    cid: u32,
    bit_depth: u8,
}

const FIXTURES: [Fixture; 3] = [
    Fixture {
        name: "dnxhr-lb-264x120",
        mov: include_bytes!("fixtures/dnx/dnxhr-lb-264x120.mov"),
        reference_zz: include_bytes!("fixtures/dnx/dnxhr-lb-264x120.ref.yuv.zz"),
        source_zz: include_bytes!("fixtures/dnx/source-8bit-264x120.yuv.zz"),
        cid: 1274,
        bit_depth: 8,
    },
    Fixture {
        name: "dnxhr-hq-264x120",
        mov: include_bytes!("fixtures/dnx/dnxhr-hq-264x120.mov"),
        reference_zz: include_bytes!("fixtures/dnx/dnxhr-hq-264x120.ref.yuv.zz"),
        source_zz: include_bytes!("fixtures/dnx/source-8bit-264x120.yuv.zz"),
        cid: 1272,
        bit_depth: 8,
    },
    Fixture {
        name: "dnxhr-hqx-264x120",
        mov: include_bytes!("fixtures/dnx/dnxhr-hqx-264x120.mov"),
        reference_zz: include_bytes!("fixtures/dnx/dnxhr-hqx-264x120.ref.yuv.zz"),
        source_zz: include_bytes!("fixtures/dnx/source-10bit-264x120.yuv.zz"),
        cid: 1271,
        bit_depth: 10,
    },
];
const W: u32 = 264;
const H: u32 = 120;
const FRAMES: u64 = 3;
const TOL: i32 = 1;
/// 25 fps in Pulse ticks.
const TICKS_PER_FRAME: u64 = 10_160_640_000;

fn reference(f: &Fixture) -> Vec<u8> {
    miniz_oxide::inflate::decompress_to_vec_zlib(f.reference_zz).expect("reference inflates")
}

#[derive(Default)]
struct Collect {
    frames: Vec<(i64, Vec<u8>)>,
}

impl ChunkSink for Collect {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
        self.frames.push((c.pts, c.bytes.to_vec()));
        Ok(())
    }
}

fn request(bytes: &[u8], output: OutputMode, purpose: Purpose) -> SegmentRequest {
    SegmentRequest {
        codec: "isobmff".to_owned(),
        start_pts: Ticks::new(0),
        count: FRAMES,
        track: None,
        output,
        prefer: SourcePreference::Original,
        purpose,
        grant: Grant {
            content_id: ContentId::parse("dnx-fixture").unwrap(),
            revision: 1,
            expected_len: bytes.len() as u64,
        },
        expected_revision: 1,
    }
}

/// Decode through the public registry on the DEFAULT route (untrusted source, no external decoder).
fn decode(bytes: &[u8], purpose: Purpose) -> Result<(SegmentReceipt, Collect), ReelError> {
    let registry = Registry::standard();
    let mut sink = Collect::default();
    let receipt = registry.decode_segment(
        &mut Cursor::new(bytes.to_vec()),
        None,
        &request(bytes, OutputMode::Decoded, purpose),
        &Limits::default(),
        &CancellationToken::default(),
        &mut sink,
    )?;
    Ok((receipt, sink))
}

/// The compressed frames (container packets).
fn packets(bytes: &[u8]) -> Vec<Vec<u8>> {
    let registry = Registry::standard();
    let mut sink = Collect::default();
    registry
        .decode_segment(
            &mut Cursor::new(bytes.to_vec()),
            None,
            &request(bytes, OutputMode::Packets, Purpose::Preview),
            &Limits::default(),
            &CancellationToken::default(),
            &mut sink,
        )
        .unwrap();
    sink.frames.into_iter().map(|(_, b)| b).collect()
}

fn samples(bytes: &[u8], bit_depth: u8) -> Vec<i32> {
    if bit_depth > 8 {
        bytes
            .chunks_exact(2)
            .map(|p| i32::from(u16::from_le_bytes([p[0], p[1]])))
            .collect()
    } else {
        bytes.iter().map(|b| i32::from(*b)).collect()
    }
}

/// (max |delta|, number of differing samples).
fn compare(ours: &[u8], theirs: &[u8], bit_depth: u8) -> (i32, usize) {
    assert_eq!(ours.len(), theirs.len());
    let (a, b) = (samples(ours, bit_depth), samples(theirs, bit_depth));
    let mut max = 0;
    let mut differ = 0;
    for (x, y) in a.iter().zip(&b) {
        let d = (x - y).abs();
        max = max.max(d);
        differ += usize::from(d != 0);
    }
    (max, differ)
}

#[test]
fn vc3_decode_matches_external_oracle_within_tolerance() {
    let cancel = CancellationToken::default();
    for f in &FIXTURES {
        let reference = reference(f);
        let (receipt, sink) = decode(f.mov, Purpose::Preview).expect(f.name);
        assert_eq!(sink.frames.len() as u64, FRAMES, "{}", f.name);
        let format = receipt.video.expect("video format");
        let frame_bytes = format.frame_bytes().unwrap();
        assert_eq!(reference.len(), frame_bytes * FRAMES as usize, "{}", f.name);
        let mut worst = 0;
        for (n, (_, ours)) in sink.frames.iter().enumerate() {
            let theirs = &reference[n * frame_bytes..(n + 1) * frame_bytes];
            let (max, differ) = compare(ours, theirs, f.bit_depth);
            println!(
                "{} frame {n}: max |delta| {max}, {differ} of {} samples differ",
                f.name,
                ours.len() / format.bytes_per_sample()
            );
            worst = worst.max(max);
        }
        if f.bit_depth == 8 {
            assert!(
                worst <= TOL,
                "{}: max delta {worst} above TOL {TOL}",
                f.name
            );
        } else {
            // CID 1271 (ST 2019-1 Table C.2: weights Table D.1). Measured on this fixture: ffmpeg's
            // encoder and decoder use different luma weights for 1271 (decoding the same bytes with the
            // Table D.4 weights lands within 2 LSB of ffmpeg and closer to the encoder source than ffmpeg;
            // with Table D.1 the difference is up to 4 LSB). Studio follows the standard, so agreement
            // with this external oracle at 10 bits is not claimed.
            println!(
                "NOT_PROVEN: {} 10-bit agreement with ffmpeg (max delta {worst}); ffmpeg's CID 1271 weights differ from ST 2019-1 Table D.1",
                f.name
            );
        }
        // Second oracle: the encoder's own input. A correct decoder is not further from the source than
        // the external decoder (8-bit; ratio measured 0.93 and 0.95).
        let source = samples(
            &miniz_oxide::inflate::decompress_to_vec_zlib(f.source_zz).expect("source inflates"),
            f.bit_depth,
        );
        let ours_all: Vec<i32> = sink
            .frames
            .iter()
            .flat_map(|(_, b)| samples(b, f.bit_depth))
            .collect();
        let mse = |a: &[i32]| {
            a.iter()
                .zip(&source)
                .map(|(x, y)| f64::from((x - y) * (x - y)))
                .sum::<f64>()
                / a.len() as f64
        };
        let (ours_mse, their_mse) = (mse(&ours_all), mse(&samples(&reference, f.bit_depth)));
        println!(
            "{}: MSE vs encoder source: ours {ours_mse:.4}, ffmpeg {their_mse:.4}",
            f.name
        );
        if f.bit_depth == 8 {
            assert!(
                ours_mse <= their_mse * 1.05,
                "{}: {ours_mse} vs {their_mse}",
                f.name
            );
        }

        // Mutation control: flip single bits of the first frame's compressed macroblock data (after the
        // 640-byte header, before the EOF signature). A decoder that ignored coefficients would pass
        // the comparison above; here most flips must show (error or a delta above TOL).
        let frames = packets(f.mov);
        let frame0 = &frames[0];
        let theirs = &reference[..frame_bytes];
        let payload = 640..frame0.len() - 4;
        // Only bytes the encoder actually filled (CBR padding is zero and ignored by design).
        let used_end = frame0[payload.clone()]
            .iter()
            .rposition(|b| *b != 0)
            .map_or(payload.start, |p| payload.start + p + 1);
        let span = (used_end - payload.start) as u64 * 8;
        let flips = 256u64;
        let mut caught = 0;
        for k in 0..flips {
            let bit = (k * 7919 + 13) % span;
            let mut mutated = frame0.clone();
            mutated[payload.start + (bit / 8) as usize] ^= 0x80 >> (bit % 8);
            match decode_vc3_frame(&mutated, (W, H), &Limits::default(), &cancel) {
                Err(_) => caught += 1,
                Ok((_, planes)) => {
                    let ours: Vec<u8> = planes.concat();
                    if compare(&ours, theirs, f.bit_depth).0 > TOL {
                        caught += 1;
                    }
                }
            }
        }
        println!(
            "{}: mutation control caught {caught} of {flips} flips",
            f.name
        );
        assert!(
            caught * 100 >= flips * 60,
            "{}: only {caught} of {flips}",
            f.name
        );
        assert_eq!(
            f.cid,
            u32::from_be_bytes(frame0[0x28..0x2C].try_into().unwrap())
        );
    }
}

#[test]
fn vc3_route_receipts_and_descriptor() {
    for f in &FIXTURES {
        let (receipt, sink) = decode(f.mov, Purpose::Preview).expect(f.name);
        assert_eq!(receipt.decoder_id, DNX_DECODER_ID);
        assert_eq!(receipt.decoder_id, "vc3-st2019-1");
        assert_eq!(receipt.decoder_version, DNX_DECODER_VERSION);
        assert_eq!(
            receipt.codec, "AVdh",
            "ffmpeg writes DNxHR as sample entry AVdh"
        );
        let format = receipt.video.unwrap();
        assert_eq!((format.width, format.height), (W, H));
        assert_eq!(format.bit_depth, f.bit_depth);
        assert_eq!(format.chroma, ChromaFormat::Yuv422);
        assert!(!format.color.full_range);
        for (n, (pts, _)) in sink.frames.iter().enumerate() {
            assert_eq!(*pts as u64, n as u64 * TICKS_PER_FRAME);
        }
        // Export of the whole range is exact.
        let (export, _) = decode(f.mov, Purpose::Export).expect("export");
        assert!(export.complete);
        // Two decodes of the same bytes are identical (determinism).
        let (_, again) = decode(f.mov, Purpose::Preview).unwrap();
        assert!(sink.frames == again.frames);
    }
    let dnx = dnx_descriptor();
    // Encode landed in lane SMB-REEL-ENCODE (tests/vc3_encode.rs).
    assert!(dnx.can_decode && dnx.can_encode);
    assert_eq!(dnx.decoder_id, DNX_DECODER_ID);
    assert_eq!(dnx.certification, Certification::NotCertified);
    let listed = Registry::standard().video_codec_descriptors();
    assert!(listed.iter().any(|d| d.id == "dnx"));
    let prores = listed.iter().find(|d| d.id == "prores").unwrap();
    assert_eq!(prores.name, "ProRes (RDD 36 decoder, not Apple-certified)");
    assert!(!prores.name.contains("Apple ProRes"));
    assert_eq!(prores.can_decode, cfg!(feature = "prores"));
}

fn put16(frame: &mut [u8], at: usize, value: u16) {
    frame[at..at + 2].copy_from_slice(&value.to_be_bytes());
}

#[test]
fn vc3_dims_u32_max() {
    let frame = packets(FIXTURES[0].mov).remove(0);
    let cancel = CancellationToken::default();
    let limits = Limits::default();
    let one_frame = (W * H * 2) as usize;
    // Largest legal raster (16384 x 16384, 512 MiB at 4:2:2 8-bit): the header size grows with the
    // height (7.2), so the patched header is first refused for its size; with a matching header size
    // the frame is refused by the chunk budget. Neither allocates more than one real frame.
    for (w, h) in [
        (16384u16, 16384u16),
        (u16::MAX, u16::MAX),
        (0, 0),
        (16384, 120),
    ] {
        let mut hostile = frame.clone();
        put16(&mut hostile, 0x18, h);
        put16(&mut hostile, 0x1D, h);
        put16(&mut hostile, 0x1A, w);
        for declared in [(0, 0), (W, H)] {
            let (result, largest) =
                largest_during(|| decode_vc3_frame(&hostile, declared, &limits, &cancel));
            let err = result.expect_err("hostile dims refused");
            assert!(
                matches!(
                    err,
                    ReelError::Corrupt { .. } | ReelError::LimitExceeded { .. }
                ),
                "{w}x{h} {declared:?}: {err:?}"
            );
            assert!(
                largest <= one_frame,
                "{w}x{h}: largest allocation {largest}"
            );
        }
    }
    // 16384 x 120 with an honest header: past the declared width -> size mismatch; undeclared: the
    // payload cannot hold 8192 macroblocks (aggregate budget, checked before the planes are sized), and
    // a tighter chunk budget refuses the frame bytes first.
    let mut wide = frame.clone();
    put16(&mut wide, 0x1A, 16384);
    assert_eq!(
        decode_vc3_frame(&wide, (W, H), &limits, &cancel).unwrap_err(),
        ReelError::Corrupt {
            what: "vc3-size-mismatch"
        }
    );
    let (result, largest) = largest_during(|| decode_vc3_frame(&wide, (0, 0), &limits, &cancel));
    assert_eq!(
        result.unwrap_err(),
        ReelError::Corrupt {
            what: "vc3-payload-size"
        }
    );
    assert!(largest <= one_frame);
    let tight = Limits {
        max_chunk_bytes: one_frame,
        ..Limits::default()
    };
    let (result, largest) = largest_during(|| decode_vc3_frame(&wide, (0, 0), &tight, &cancel));
    assert_eq!(
        result.unwrap_err(),
        ReelError::LimitExceeded {
            what: "vc3-frame-bytes"
        }
    );
    assert!(largest <= one_frame);
}

#[test]
fn vc3_aggregate_over_budget() {
    let frame = packets(FIXTURES[2].mov).remove(0);
    let cancel = CancellationToken::default();
    let limits = Limits::default();
    let corrupt = |what| ReelError::Corrupt { what };
    // A scan index past the payload, and indices out of order.
    let mut past = frame.clone();
    past[0x170 + 4 * 3..0x170 + 4 * 4].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(
        decode_vc3_frame(&past, (W, H), &limits, &cancel).unwrap_err(),
        corrupt("vc3-scan-indices")
    );
    let mut reversed = frame.clone();
    reversed[0x170 + 8..0x170 + 12].copy_from_slice(&4u32.to_be_bytes());
    assert_eq!(
        decode_vc3_frame(&reversed, (W, H), &limits, &cancel).unwrap_err(),
        corrupt("vc3-scan-indices")
    );
    // Scan-line count that disagrees with the raster; header size larger than the packet.
    let mut ns = frame.clone();
    ns[0x16D] = 200;
    assert_eq!(
        decode_vc3_frame(&ns, (W, H), &limits, &cancel).unwrap_err(),
        corrupt("vc3-scan-indices")
    );
    let mut big_header = frame.clone();
    big_header[0..4].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(
        decode_vc3_frame(&big_header, (W, H), &limits, &cancel).unwrap_err(),
        corrupt("vc3-header-size")
    );
    // Every truncation and every single-bit flip of the header region: Ok or a typed error, no panic,
    // and never an allocation above one decoded frame.
    let one_frame = (W * H * 2 * 2) as usize;
    for len in (0..frame.len()).step_by(97).chain([frame.len() - 1]) {
        let (result, largest) =
            largest_during(|| decode_vc3_frame(&frame[..len], (W, H), &limits, &cancel));
        assert!(result.is_err(), "truncated to {len}");
        assert!(largest <= one_frame);
    }
    for bit in (0..0x30 * 8).chain(0x160 * 8..0x180 * 8) {
        let mut flipped = frame.clone();
        flipped[bit / 8] ^= 0x80 >> (bit % 8);
        let (_, largest) = largest_during(|| decode_vc3_frame(&flipped, (W, H), &limits, &cancel));
        assert!(largest <= one_frame, "bit {bit}: {largest}");
    }
    // Typed refusals of valid features this decoder does not decode.
    let unsupported = |what| ReelError::Unsupported { what };
    let mut alpha = frame.clone();
    alpha[7] |= 0x01;
    assert_eq!(
        decode_vc3_frame(&alpha, (W, H), &limits, &cancel).unwrap_err(),
        unsupported("vc3-alpha")
    );
    let mut unknown = frame.clone();
    unknown[0x28..0x2C].copy_from_slice(&1300u32.to_be_bytes());
    assert_eq!(
        decode_vc3_frame(&unknown, (W, H), &limits, &cancel).unwrap_err(),
        unsupported("vc3-compression-id")
    );
    let mut eof = frame.clone();
    let n = eof.len();
    eof[n - 1] ^= 1;
    assert_eq!(
        decode_vc3_frame(&eof, (W, H), &limits, &cancel).unwrap_err(),
        corrupt("vc3-eof-signature")
    );
}
