//! R49 F-R49-2 (lane SMB-R49-FOLLOWUPS-1): the viewer's audio import bounds decoded SAMPLES (frames x
//! channels), not only frames. An 8-channel FLAC of constant subframes is a few KiB yet decodes to millions
//! of samples; with `ImportLimits::max_samples` the import refuses `audio_too_large` and never makes a single
//! allocation above the budget's bytes. The refusal is asserted, not timing. Own test binary: the counting
//! allocator below records the largest single request of the current thread (as reel's
//! `tests/audio_sample_budget.rs` does), so a refusal that merely returns the right code after holding the
//! whole stream still fails. Feature `media` (the import module's feature; the round's `desktop`+`audio-batch`
//! unit of `.config/round-features.toml` enables it for this crate, rule 4e).
#![cfg(feature = "media")]

use hsk_studio_accord::CancellationToken;
use hsk_studio_harness_view::view::audio_import::{
    DEFAULT_MAX_FRAMES, DEFAULT_MAX_SAMPLES, ImportLimits, ImportOptions, Speaker, import_audio_external,
    import_audio_with,
};
use hsk_studio_reel::audio::{AudioCodec, AudioDecodeSession, AudioStreamSpec, ExternalAudioDecoder};
use hsk_studio_reel::{DecoderIdentity, ReelError, encode_flac};
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
/// The 8-channel stream holds 4 000 000 samples (16 MB of f32).
const STREAM_SAMPLES: u64 = 4_000_000;
/// 1 000 000 samples (4 MB of f32): under the 500 000-frame limit, so only samples refuse the stream.
const BUDGET_SAMPLES: u64 = 1_000_000;

/// The file's channels in the order the import's downmix is told (7.1; the map is the caller's).
fn channel_map() -> ImportOptions {
    ImportOptions {
        channel_map: Some(vec![
            Speaker::FrontLeft,
            Speaker::FrontRight,
            Speaker::FrontCenter,
            Speaker::LowFrequency,
            Speaker::BackLeft,
            Speaker::BackRight,
            Speaker::SideLeft,
            Speaker::SideRight,
        ]),
        normalize_downmix: false,
    }
}

/// A constant 8-channel 16-bit FLAC (every subframe CONSTANT) of `FRAMES` frames; with `declared_total`
/// false the STREAMINFO total-samples field is 0 (unknown, RFC 9639 8.2), so only the per-block check can
/// stop it.
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

/// Imports `bytes` under `limits` on this thread: the refusal code (`None` when it imported) and the
/// largest single allocation the import made.
fn import_measured(
    bytes: &[u8],
    limits: &ImportLimits,
    external: Option<&dyn ExternalAudioDecoder>,
) -> (Option<&'static str>, usize) {
    let mut src = Cursor::new(bytes.to_vec());
    LARGEST.with(|l| l.set(0));
    let result = import_audio_external(&mut src, limits, &channel_map(), external, &CancellationToken::default());
    let largest = LARGEST.with(Cell::get);
    (result.err().map(|e| e.code), largest)
}

#[test]
fn the_default_sample_budget_is_the_stereo_bound_of_the_frame_limit() {
    let limits = ImportLimits::default();
    assert_eq!((limits.max_frames, limits.max_samples), (DEFAULT_MAX_FRAMES, DEFAULT_MAX_SAMPLES));
    assert_eq!(limits.max_samples, limits.max_frames * 2);
    // 4 bytes a sample: the decoded budget of the export-sequence path (MAX_EXPORT_AUDIO_DECODED_BYTES).
    assert_eq!(limits.max_samples, 1_036_800_000);
    assert_eq!(limits.max_samples * 4, 4_147_200_000);
}

#[test]
fn eight_channel_flac_over_the_sample_budget_is_refused_without_allocating_above_it() {
    let budget_bytes = usize::try_from(BUDGET_SAMPLES * 4).unwrap();
    let limits = ImportLimits { max_samples: BUDGET_SAMPLES, ..ImportLimits::default() };
    // The frame limit alone admits the stream: only samples refuse it.
    assert!(u64::try_from(FRAMES).unwrap() < limits.max_frames);
    for declared in [false, true] {
        let flac = constant_flac(declared);
        let (code, largest) = import_measured(&flac, &limits, None);
        assert_eq!(code, Some("audio_too_large"), "declared total {declared}");
        assert!(largest <= budget_bytes, "declared total {declared}: largest allocation {largest} > {budget_bytes}");
    }
    // An import entry that holds an external decoder (the viewer's `media-native` registration) is bounded
    // the same way for a format the external decoder does not serve.
    let (code, largest) = import_measured(&constant_flac(false), &limits, Some(&FakeOs));
    assert_eq!(code, Some("audio_too_large"));
    assert!(largest <= budget_bytes, "with an external decoder: largest allocation {largest} > {budget_bytes}");
}

#[test]
fn the_sample_budget_is_inclusive_and_default_limits_admit_the_stream() {
    let flac = constant_flac(false);
    let exact = ImportLimits { max_samples: STREAM_SAMPLES, ..ImportLimits::default() };
    assert_eq!(import_measured(&flac, &exact, None).0, None, "exactly the stream's samples");
    let short = ImportLimits { max_samples: STREAM_SAMPLES - 1, ..ImportLimits::default() };
    assert_eq!(import_measured(&flac, &short, None).0, Some("audio_too_large"), "one sample short");
    assert_eq!(import_measured(&flac, &ImportLimits::default(), None).0, None, "default limits");
    // The frame limit keeps its own code: audio_decode with reel's MEDIA_LIMIT_EXCEEDED (existing contract).
    let frames = ImportLimits { max_frames: 1000, ..ImportLimits::default() };
    let refused = import_audio_with(&mut Cursor::new(flac), &frames, &channel_map(), &CancellationToken::default())
        .err()
        .unwrap();
    assert_eq!(refused.code, "audio_decode");
    assert!(refused.detail.contains("MEDIA_LIMIT_EXCEEDED"), "{}", refused.detail);
}

/// A stand-in for the OS AAC decoder: 1024 frames of a constant per channel for every access unit.
struct FakeOs;
struct FakeSession {
    channels: usize,
}
impl AudioDecodeSession for FakeSession {
    fn decode(&mut self, _: &[u8], _: &CancellationToken, out: &mut Vec<f32>) -> Result<usize, ReelError> {
        out.extend(std::iter::repeat_n(0.25, 1024 * self.channels));
        Ok(1024)
    }
}
impl ExternalAudioDecoder for FakeOs {
    fn identity(&self) -> DecoderIdentity {
        DecoderIdentity { id: "fake-os", version: "test" }
    }
    fn supports(&self, codec: AudioCodec) -> bool {
        codec == AudioCodec::AacLc
    }
    fn open(&self, spec: &AudioStreamSpec<'_>, _: &CancellationToken) -> Result<Box<dyn AudioDecodeSession>, ReelError> {
        Ok(Box::new(FakeSession { channels: usize::from(spec.channels) }))
    }
}

const AAC_UNITS: usize = 100;
/// 8-channel (channel configuration 7) AAC-LC at 48 kHz in ADTS: `AAC_UNITS` frames of 10 payload bytes.
fn adts_eight_channel() -> Vec<u8> {
    let frame_len: usize = 7 + 10;
    let mut out = Vec::new();
    for _ in 0..AAC_UNITS {
        // sync 0xFFF, MPEG-4, layer 00, no CRC; LC (profile 1), rate index 3 (48 kHz), channel config 7.
        out.extend_from_slice(&[
            0xFF,
            0xF1,
            0x4D,
            0xC0 | u8::try_from((frame_len >> 11) & 3).unwrap(),
            u8::try_from((frame_len >> 3) & 0xFF).unwrap(),
            u8::try_from((frame_len & 7) << 5).unwrap() | 0x1F,
            0xFC,
        ]);
        out.extend_from_slice(&[0x11; 10]);
    }
    out
}

#[test]
fn eight_channel_aac_through_an_external_decoder_is_bounded_by_the_sample_budget() {
    let adts = adts_eight_channel();
    let stream = u64::try_from(AAC_UNITS).unwrap() * 1024 * 8;
    let tight = ImportLimits { max_samples: stream / 2, ..ImportLimits::default() };
    assert_eq!(import_measured(&adts, &tight, Some(&FakeOs)).0, Some("audio_too_large"));
    let short = ImportLimits { max_samples: stream - 1, ..ImportLimits::default() };
    assert_eq!(import_measured(&adts, &short, Some(&FakeOs)).0, Some("audio_too_large"), "one sample short");
    let exact = ImportLimits { max_samples: stream, ..ImportLimits::default() };
    assert_eq!(import_measured(&adts, &exact, Some(&FakeOs)).0, None, "exactly the stream's samples");
    assert_eq!(import_measured(&adts, &ImportLimits::default(), Some(&FakeOs)).0, None, "default limits");
}
