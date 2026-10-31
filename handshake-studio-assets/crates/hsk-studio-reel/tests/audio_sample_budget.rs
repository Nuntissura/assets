//! R47 F-R47-1 (lane SMB-R47-FOLLOWUPS-1): the whole-file audio decode bounds kept SAMPLES (frames x
//! channels), not only frames. An 8-channel FLAC of constant subframes is a few KiB yet decodes to
//! millions of samples; with a sample budget the decode refuses `audio-sample-budget` and never makes a
//! single allocation above the budget's bytes. Own test binary: the counting allocator below records the
//! largest single request of the current thread (as `tests/v4_alloc_budget.rs` does), so a refusal that
//! merely returns the right error after holding the whole stream still fails.
use hsk_studio_accord::CancellationToken;
use hsk_studio_reel::audio::decode_audio_file_within;
use hsk_studio_reel::{Limits, ReelError, decode_audio_file, encode_flac};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::Cursor;

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

const CHANNELS: u8 = 8;
const FRAMES: usize = 500_000;
/// 1 000 000 samples (4 MB of f32): the 8-channel stream holds 4 000 000 (16 MB).
const BUDGET_SAMPLES: u64 = 1_000_000;

/// A constant 8-channel 16-bit FLAC (every subframe CONSTANT) of `FRAMES` frames; with
/// `declared_total` false the STREAMINFO total-samples field is 0 (unknown, RFC 9639 8.2), so only the
/// per-block check can stop it.
fn constant_flac(declared_total: bool) -> Vec<u8> {
    let mut bytes = encode_flac(48_000, CHANNELS, 16, &vec![1234; FRAMES * usize::from(CHANNELS)]).unwrap();
    assert_eq!(&bytes[..4], b"fLaC");
    assert!(bytes.len() < 64 * 1024, "a few KiB: {} bytes", bytes.len());
    if !declared_total {
        // STREAMINFO starts at byte 8; its 36-bit total is the low nibble of byte 21 and bytes 22..26.
        bytes[21] &= 0xf0;
        bytes[22..26].fill(0);
    }
    bytes
}

/// Decodes `bytes` within the budget on this thread and returns the result and the largest single
/// allocation the decode made.
fn decode_measured(bytes: &[u8], max_frames: u64, max_samples: u64) -> (Result<usize, ReelError>, usize) {
    let mut src = Cursor::new(bytes.to_vec());
    LARGEST.with(|l| l.set(0));
    let result = decode_audio_file_within(&mut src, &Limits::default(), max_frames, max_samples, &CancellationToken::default())
        .map(|d| d.interleaved.len());
    (result, LARGEST.with(Cell::get))
}

#[test]
fn eight_channel_flac_over_the_sample_budget_is_refused_without_allocating_above_it() {
    let budget_bytes = usize::try_from(BUDGET_SAMPLES * 4).unwrap();
    // The frame limit alone admits the stream (500 000 frames < 1 000 000): only samples refuse it.
    for declared in [false, true] {
        let flac = constant_flac(declared);
        let (result, largest) = decode_measured(&flac, BUDGET_SAMPLES, BUDGET_SAMPLES);
        assert_eq!(result, Err(ReelError::LimitExceeded { what: "audio-sample-budget" }), "declared total {declared}");
        assert!(largest <= budget_bytes, "declared total {declared}: largest allocation {largest} > budget {budget_bytes}");
    }
    // Without the sample budget the same stream decodes whole (the frame argument keeps its meaning).
    let flac = constant_flac(true);
    let whole = decode_audio_file(&mut Cursor::new(flac), &Limits::default(), BUDGET_SAMPLES, &CancellationToken::default()).unwrap();
    assert_eq!((whole.channels, whole.interleaved.len()), (u16::from(CHANNELS), FRAMES * usize::from(CHANNELS)));
}

#[test]
fn a_stream_within_the_sample_budget_decodes_exactly_as_without_it() {
    let flac = constant_flac(false);
    let samples = u64::try_from(FRAMES * usize::from(CHANNELS)).unwrap();
    let (result, largest) = decode_measured(&flac, samples, samples);
    assert_eq!(result, Ok(FRAMES * usize::from(CHANNELS)));
    assert!(largest <= usize::try_from(samples * 4).unwrap(), "largest allocation {largest}");
    let bounded = decode_audio_file_within(&mut Cursor::new(flac.clone()), &Limits::default(), samples, samples, &CancellationToken::default()).unwrap();
    let plain = decode_audio_file(&mut Cursor::new(flac), &Limits::default(), samples, &CancellationToken::default()).unwrap();
    assert_eq!(bounded, plain);
    // One sample short of the stream: refused.
    let (short, _) = decode_measured(&constant_flac(false), samples, samples - 1);
    assert_eq!(short, Err(ReelError::LimitExceeded { what: "audio-sample-budget" }));
}
