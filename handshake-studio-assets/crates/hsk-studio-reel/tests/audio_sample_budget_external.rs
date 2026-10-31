//! R49 F-R49-2 (lane SMB-R49-FOLLOWUPS-1): `decode_audio_file_within_with` holds the sample budget of
//! `decode_audio_file_within` (tests/audio_sample_budget.rs, R47 F-R47-1) for a stream an EXTERNAL audio
//! decoder serves. An 8-channel (channel configuration 7) AAC-LC ADTS file of under 2 KiB decodes, through a
//! decoder that returns 1024 frames per access unit, to 819 200 samples; with a sample budget under the stream
//! the decode is refused `audio-sample-budget` before any access unit reaches the decoder, and with a budget of
//! exactly the stream's samples it decodes whole and equals the unbudgeted decode.
use hsk_studio_accord::CancellationToken;
use hsk_studio_reel::audio::{
    AudioCodec, AudioDecodeSession, AudioStreamSpec, ExternalAudioDecoder, decode_audio_file_within,
    decode_audio_file_within_with,
};
use hsk_studio_reel::{DecoderIdentity, Limits, ReelError, decode_audio_file_with};
use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const UNITS: usize = 100;
const CHANNELS: usize = 8;
/// 819 200 samples: 100 access units of 1024 frames of 8 channels.
const STREAM_SAMPLES: u64 = (UNITS * 1024 * CHANNELS) as u64;
const STREAM_FRAMES: u64 = (UNITS * 1024) as u64;

/// 8-channel AAC-LC at 48 kHz in ADTS: `UNITS` frames of 10 payload bytes (the decoder is a stand-in, so the
/// payload is not parsed).
fn adts() -> Vec<u8> {
    let frame_len: usize = 7 + 10;
    let mut out = Vec::new();
    for _ in 0..UNITS {
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

/// A stand-in for the OS decoder: 1024 frames of a constant per channel for every access unit; counts the
/// access units it was given.
struct FakeOs {
    units: Arc<AtomicU64>,
}
struct FakeSession {
    channels: usize,
    units: Arc<AtomicU64>,
}
impl AudioDecodeSession for FakeSession {
    fn decode(&mut self, _: &[u8], _: &CancellationToken, out: &mut Vec<f32>) -> Result<usize, ReelError> {
        self.units.fetch_add(1, Ordering::SeqCst);
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
        Ok(Box::new(FakeSession { channels: usize::from(spec.channels), units: self.units.clone() }))
    }
}

fn fake() -> (FakeOs, Arc<AtomicU64>) {
    let units = Arc::new(AtomicU64::new(0));
    (FakeOs { units: units.clone() }, units)
}

fn within(bytes: &[u8], max_samples: u64, external: &FakeOs) -> Result<usize, ReelError> {
    decode_audio_file_within_with(
        &mut Cursor::new(bytes.to_vec()),
        &Limits::default(),
        STREAM_FRAMES,
        max_samples,
        &CancellationToken::default(),
        Some(external),
    )
    .map(|d| d.interleaved.len())
}

#[test]
fn external_decoder_output_over_the_sample_budget_is_refused_before_any_access_unit_is_decoded() {
    let bytes = adts();
    assert!(bytes.len() < 2048, "{} bytes", bytes.len());
    // The frame limit alone admits the stream (it is exactly the stream's frames): only samples refuse it.
    for budget in [STREAM_SAMPLES / 2, STREAM_SAMPLES - 1] {
        let (os, units) = fake();
        assert_eq!(within(&bytes, budget, &os), Err(ReelError::LimitExceeded { what: "audio-sample-budget" }), "budget {budget}");
        assert_eq!(units.load(Ordering::SeqCst), 0, "budget {budget}: the decoder received no access unit");
    }
    // Without the external decoder the AAC stream is the typed unavailable (no budget error masks it).
    let none = decode_audio_file_within_with(
        &mut Cursor::new(bytes.clone()),
        &Limits::default(),
        STREAM_FRAMES,
        STREAM_SAMPLES,
        &CancellationToken::default(),
        None,
    );
    if !cfg!(feature = "studio-aac") {
        assert_eq!(none.err(), Some(ReelError::CodecUnavailable { codec: "aac-lc".into() }));
    }
}

#[test]
fn a_stream_within_the_sample_budget_decodes_as_without_it_and_none_is_the_plain_within() {
    let bytes = adts();
    let (os, units) = fake();
    let budgeted = decode_audio_file_within_with(
        &mut Cursor::new(bytes.clone()),
        &Limits::default(),
        STREAM_FRAMES,
        STREAM_SAMPLES,
        &CancellationToken::default(),
        Some(&os),
    )
    .unwrap();
    assert_eq!((budgeted.channels, budgeted.interleaved.len()), (8, usize::try_from(STREAM_SAMPLES).unwrap()));
    assert_eq!(units.load(Ordering::SeqCst), UNITS as u64);
    let (plain_os, _) = fake();
    let plain = decode_audio_file_with(&mut Cursor::new(bytes), &Limits::default(), STREAM_FRAMES, &CancellationToken::default(), Some(&plain_os)).unwrap();
    assert_eq!(budgeted, plain);
    // `None` is `decode_audio_file_within` on a format no external decoder serves.
    let flac = hsk_studio_reel::encode_flac(48_000, 2, 16, &vec![7; 2000]).unwrap();
    let a = decode_audio_file_within(&mut Cursor::new(flac.clone()), &Limits::default(), 1000, 2000, &CancellationToken::default()).unwrap();
    let b = decode_audio_file_within_with(&mut Cursor::new(flac.clone()), &Limits::default(), 1000, 2000, &CancellationToken::default(), None).unwrap();
    assert_eq!(a, b);
    let over = decode_audio_file_within_with(&mut Cursor::new(flac), &Limits::default(), 1000, 1999, &CancellationToken::default(), Some(&fake().0));
    assert_eq!(over.err(), Some(ReelError::LimitExceeded { what: "audio-sample-budget" }));
}
