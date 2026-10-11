//! V4-02 allocation proofs: a header (or box table) can declare more than the source can possibly hold,
//! and the decoder must not size a buffer from the declaration before checking it against the bytes that
//! exist. Own test binary so the counting allocator below does not touch the other contract tests.
//!
//! The allocator records the LARGEST single request made by the current thread, so a refusal that merely
//! happens to return the same error as a huge zeroed allocation still fails here.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_reel::fixtures::*;
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

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost 30s deadline: {e}"),
    }
}

struct Discard;
impl ChunkSink for Discard {
    fn accept(&mut self, _: Chunk<'_>) -> Result<(), ReelError> {
        panic!("no chunk may be delivered from a source that cannot hold one frame");
    }
}

/// A Y4M header that declares a 16384 x 16384 4:4:4 frame (805 MB) followed by one frame marker and no
/// pixels. The caller's limits admit the frame size; the 40-byte source does not hold it. The decoder
/// reports `Truncated` and never sizes a buffer for the declared frame.
#[test]
fn y4m_frame_buffer_is_bounded_by_the_source() {
    bounded_case("y4m_frame_buffer_is_bounded_by_the_source", || {
        let mut bytes = b"YUV4MPEG2 W16384 H16384 F25:1 C444\n".to_vec();
        bytes.extend_from_slice(b"FRAME\n");
        let limits = Limits {
            max_chunk_bytes: usize::MAX,
            ..Limits::default()
        };
        let req = SegmentRequest {
            codec: "y4m".into(),
            start_pts: Ticks::new(0),
            count: 1,
            track: None,
            output: OutputMode::Decoded,
            prefer: SourcePreference::Original,
            purpose: Purpose::Preview,
            grant: Grant {
                content_id: ContentId::parse("v4-alloc-y4m").unwrap(),
                revision: 1,
                expected_len: bytes.len() as u64,
            },
            expected_revision: 1,
        };
        LARGEST.with(|largest| largest.set(0));
        let result = Y4mProvider.decode_segment(
            &mut Cursor::new(bytes),
            &req,
            &limits,
            &CancellationToken::default(),
            &mut Discard,
        );
        let largest = LARGEST.with(Cell::get);
        assert_eq!(result.unwrap_err(), ReelError::Truncated { delivered: 0 });
        assert!(
            largest < (1 << 20),
            "largest single allocation was {largest} bytes for a source of a few dozen bytes"
        );
    });
}

// ----- ISOBMFF --------------------------------------------------------------------------------------

fn at(bytes: &[u8], tag: &[u8; 4]) -> usize {
    bytes
        .windows(4)
        .position(|w| w == tag)
        .unwrap_or_else(|| panic!("no {tag:?} box in the fixture"))
}

fn put32(bytes: &mut [u8], tag: &[u8; 4], rel: usize, value: u32) {
    let p = at(bytes, tag) + rel;
    bytes[p..p + 4].copy_from_slice(&value.to_be_bytes());
}

fn packet_request(bytes: &[u8]) -> SegmentRequest {
    SegmentRequest {
        codec: "isobmff".into(),
        start_pts: Ticks::new(0),
        count: 1,
        track: None,
        output: OutputMode::Packets,
        prefer: SourcePreference::Original,
        purpose: Purpose::Preview,
        grant: Grant {
            content_id: ContentId::parse("v4-alloc-isobmff").unwrap(),
            revision: 1,
            expected_len: bytes.len() as u64,
        },
        expected_revision: 1,
    }
}

/// A few hundred bytes of header declare 4,000,000 samples through a constant-size `stsz` (no table
/// bytes) while the chunk tables can address four. The default sample budget admits the count, so the
/// refusal must come from the chunk tables, before 176 MB of per-sample storage is reserved.
#[test]
fn isobmff_sample_storage_is_bounded_by_the_chunk_tables() {
    bounded_case(
        "isobmff_sample_storage_is_bounded_by_the_chunk_tables",
        || {
            let audio = PcmMovSpec {
                fourcc: *b"twos",
                kind: PcmKind::S16,
                big_endian: true,
                lpcm_v2: false,
                non_interleaved: false,
                channels: 2,
                rate: 48000,
                frames: 300,
                frames_per_chunk: 128,
                movie_timescale: 48000,
                edits: None,
                moov_first: true,
                sound_version: None,
            };
            let mut bytes = mov_av_bytes(4, &audio);
            put32(&mut bytes, b"stts", 12, 4_000_000);
            put32(&mut bytes, b"stsz", 12, 4_000_000);
            let limits = Limits::default();
            assert!(
                limits.max_samples >= 4_000_000,
                "the count passes the budget"
            );
            LARGEST.with(|largest| largest.set(0));
            let result = build_index(
                &mut Cursor::new(bytes.clone()),
                bytes.len() as u64,
                &limits,
                &CancellationToken::default(),
            );
            let largest = LARGEST.with(Cell::get);
            assert_eq!(
                result.unwrap_err(),
                ReelError::Corrupt { what: "stsc-short" }
            );
            assert!(
                largest < (1 << 20),
                "largest single allocation was {largest} bytes for a {}-byte file",
                bytes.len()
            );
        },
    );
}

/// A sample table that declares a 4 GiB sample inside a few hundred bytes. The caller's limits admit
/// the size; the source does not hold it, so the read is `Truncated` without a 4 GiB buffer.
#[test]
fn isobmff_sample_buffer_is_bounded_by_the_source() {
    bounded_case("isobmff_sample_buffer_is_bounded_by_the_source", || {
        let samples = (0..4u32)
            .map(|k| Mp4Sample {
                duration: 20,
                cts_offset: 0,
                size: 16,
                sync: k == 0,
            })
            .collect();
        let mut bytes = mp4_bytes(&Mp4Spec {
            movie_timescale: 600,
            media_timescale: 600,
            codec: *b"avc1",
            width: 64,
            height: 36,
            samples,
            edits: None,
            samples_per_chunk: 2,
            co64: false,
            ctts_version: 0,
            moov_first: true,
            colr_nclx: None,
            pasp: None,
        });
        put32(&mut bytes, b"stsz", 16, u32::MAX); // first table entry
        let limits = Limits {
            max_chunk_bytes: usize::MAX,
            ..Limits::default()
        };
        let index = build_index(
            &mut Cursor::new(bytes.clone()),
            bytes.len() as u64,
            &limits,
            &CancellationToken::default(),
        )
        .unwrap();
        LARGEST.with(|largest| largest.set(0));
        let result = IsobmffProvider.decode_packets(
            &mut Cursor::new(bytes.clone()),
            &index,
            &packet_request(&bytes),
            &limits,
            &CancellationToken::default(),
            &mut Discard,
        );
        let largest = LARGEST.with(Cell::get);
        assert_eq!(result.unwrap_err(), ReelError::Truncated { delivered: 0 });
        assert!(
            largest < (1 << 20),
            "largest single allocation was {largest} bytes for a {}-byte file",
            bytes.len()
        );
    });
}
