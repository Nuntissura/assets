//! Operator decision D24-A (START_LOG 2026-10-15, "lowest-risk option"), lane SMB-D24A-CODEC-DEFAULTS:
//! MP3/MP2/MP1 decode is ON by default, so `Registry::standard()` holds the `mpeg-audio` provider.
mod audio_support;
use audio_support::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_reel::audio::{AAC_PROVIDER_ID, AIFF_PROVIDER_ID, MP3_DECODER_ID, MP3_PROVIDER_ID};
use hsk_studio_reel::{AudioCodec, AudioDecodeSession, AudioStreamSpec, ExternalAudioDecoder};
use hsk_studio_reel::*;
use std::io::Cursor;

const CBR: &[u8] = include_bytes!("fixtures/audio/mp3-44k1-2ch-cbr.mp3");
const MP2: &[u8] = include_bytes!("fixtures/audio/mp2-48k-2ch.mp2");
const ADTS: &[u8] = include_bytes!("fixtures/audio/aac-lc-44k1-2ch.aac");

/// Detect the provider by magic bytes, then decode through the registry (its external decoders apply).
fn segment(registry: &Registry, bytes: &[u8], req: &SegmentRequest) -> Result<(SegmentReceipt, Collect), ReelError> {
    let mut src = Cursor::new(bytes.to_vec());
    assert_eq!(registry.detect(&mut src)?.descriptor().id, req.codec);
    let mut sink = Collect::default();
    let receipt = registry.decode_segment(
        &mut src,
        None,
        req,
        &Limits::default(),
        &CancellationToken::default(),
        &mut sink,
    )?;
    Ok((receipt, sink))
}

/// `standard()` alone (no opt-in) detects and decodes MP3 and MP2 elementary streams, the receipt names
/// the Studio MPEG audio decoder and the samples equal `decode_audio_file`'s; `with_audio()` /
/// `with_audio_codecs()` still add the other audio providers once (no duplicate `mpeg-audio`, no refusal).
/// Hostile: ADTS AAC bytes (sync 0xFFF1, MPEG layer bits 00) are NOT taken by `mpeg-audio` (the standard set
/// answers MEDIA_IMPORT_UNSUPPORTED), a sync-only stream of garbage is a typed error, never a panic, and a
/// caller's own provider registered under `mpeg-audio` first is kept by `register_audio_codecs`.
#[test]
fn mp3_mp2_decode_with_the_standard_registry() {
    let standard = Registry::standard();
    let ids: Vec<&str> = standard.descriptors().iter().map(|d| d.id).collect();
    assert_eq!(ids.last(), Some(&MP3_PROVIDER_ID));
    for (bytes, frames) in [(CBR, 13_230u64), (MP2, 12_672)] {
        let whole = decode_audio_file(
            &mut Cursor::new(bytes.to_vec()),
            &Limits::default(),
            1 << 22,
            &CancellationToken::default(),
        )
        .unwrap();
        let req = request(MP3_PROVIDER_ID, 0, frames, bytes);
        let (receipt, sink) = segment(&standard, bytes, &req).unwrap();
        assert!(receipt.complete, "{receipt:?}");
        assert_eq!(receipt.decoder_id, MP3_DECODER_ID);
        assert_eq!(sink.samples, whole.interleaved);
        assert_eq!(whole.frames() as u64, frames);
    }
    // The opt-in constructors still work on top of the new default set.
    let all: Vec<&str> = Registry::with_audio().descriptors().iter().map(|d| d.id).collect();
    assert_eq!(all.iter().filter(|id| **id == MP3_PROVIDER_ID).count(), 1);
    for id in [FLAC_PROVIDER_ID, OGG_PROVIDER_ID, AAC_PROVIDER_ID, AIFF_PROVIDER_ID] {
        assert!(all.contains(&id) && !ids.contains(&id), "{id}");
    }
    let again = Registry::standard().with_audio_codecs().unwrap();
    assert_eq!(again.descriptors().len(), all.len());
    // Hostile: ADTS is not MPEG audio; with the standard set alone it is an unsupported import.
    let adts = standard.detect(&mut Cursor::new(ADTS.to_vec())).err().unwrap();
    assert_eq!(adts.code(), "MEDIA_IMPORT_UNSUPPORTED");
    // Hostile: one valid header followed by noise decodes or fails typed, never panics.
    let mut garbage = CBR[..4].to_vec();
    let mut rng = Rng(11);
    garbage.extend((0..8192).map(|_| rng.next() as u8));
    let req = request(MP3_PROVIDER_ID, 0, 1, &garbage);
    match segment(&standard, &garbage, &req) {
        Ok((receipt, sink)) => assert!(sink.samples.iter().all(|v| v.is_finite()), "{receipt:?}"),
        Err(error) => assert!(error.code().starts_with("MEDIA_"), "{error:?}"),
    }
    // A caller's provider under the id is kept (the opt-in never replaces or refuses it).
    let mut mine = Registry::new();
    mine.register(Box::new(AudioFileProvider::mpeg_audio())).unwrap();
    mine.register_audio_codecs().unwrap();
    assert_eq!(mine.descriptors().len(), 5);
}

/// A stand-in for the OS decoder: frames of a constant per channel (honest), or a lying block size.
struct FakeOs {
    lie: bool,
}
struct FakeSession {
    channels: usize,
    lie: bool,
}
impl AudioDecodeSession for FakeSession {
    fn decode(&mut self, au: &[u8], _: &CancellationToken, out: &mut Vec<f32>) -> Result<usize, ReelError> {
        assert!(!au.is_empty() && au[..2] != [0xFF, 0xF1], "access units arrive without ADTS headers");
        let frames = if self.lie { 1025 } else { 1024 };
        out.extend(std::iter::repeat_n(0.25, frames * self.channels));
        Ok(frames)
    }
}
impl ExternalAudioDecoder for FakeOs {
    fn identity(&self) -> DecoderIdentity {
        DecoderIdentity { id: AAC_OS_DECODER_ID, version: "fake" }
    }
    fn supports(&self, codec: AudioCodec) -> bool {
        codec == AudioCodec::AacLc
    }
    fn open(&self, spec: &AudioStreamSpec<'_>, _: &CancellationToken) -> Result<Box<dyn AudioDecodeSession>, ReelError> {
        assert_eq!((spec.sample_rate, spec.channels, spec.channel_config), (44_100, 2, 2));
        Ok(Box::new(FakeSession { channels: usize::from(spec.channels), lie: self.lie }))
    }
}

/// D24-A routing of AAC: with an external (OS) decoder registered, ADTS files, MP4 `mp4a` and Matroska
/// `A_AAC` tracks and `decode_audio_file_with` all decode through it (receipt decoder id `mf-aac`, raw access
/// units without ADTS headers); without one and without feature `studio-aac` they are the typed
/// MEDIA_CODEC_UNAVAILABLE aac-lc, and a probe never needs a decoder. Hostile: a decoder that returns more
/// frames than its access units carry is MEDIA_DECODER_FAULT decoder_protocol.
#[test]
fn aac_routes_to_the_external_os_decoder() {
    let mp4: &[u8] = include_bytes!("fixtures/audio/audio-tracks.mp4");
    let mkv: &[u8] = include_bytes!("fixtures/audio/audio-tracks.mkv");
    let os = std::sync::Arc::new(FakeOs { lie: false });
    let registry = Registry::with_audio().with_audio_decoder(os.clone());
    assert_eq!(registry.audio_decoder_identity().map(|i| i.id), Some(AAC_OS_DECODER_ID));
    let req = request(AAC_PROVIDER_ID, 0, 14_336, ADTS);
    let (receipt, sink) = segment(&registry, ADTS, &req).unwrap();
    assert!(receipt.complete);
    assert_eq!(receipt.decoder_id, AAC_OS_DECODER_ID);
    assert_eq!(sink.samples.len(), 14_336 * 2);
    for (bytes, id) in [(mp4, ISOBMFF_PROVIDER_ID), (mkv, MATROSKA_PROVIDER_ID)] {
        let mut req = request(id, 0, 4096, bytes);
        req.track = Some(1);
        let (receipt, sink) = segment(&registry, bytes, &req).unwrap();
        assert_eq!((receipt.decoder_id, sink.samples.len()), (AAC_OS_DECODER_ID, 8192), "{id}");
    }
    let whole = decode_audio_file_with(
        &mut Cursor::new(ADTS.to_vec()),
        &Limits::default(),
        1 << 22,
        &CancellationToken::default(),
        Some(os.as_ref()),
    )
    .unwrap();
    assert_eq!((whole.decoder_id, whole.frames()), (AAC_OS_DECODER_ID, 14_336));
    // No decoder: probe works, decode is typed unavailable (unless the Studio decoder is compiled in).
    let bare = Registry::with_audio();
    let provider = bare.detect(&mut Cursor::new(ADTS.to_vec())).unwrap();
    let layout = provider
        .probe(&mut Cursor::new(ADTS.to_vec()), &Limits::default(), &CancellationToken::default())
        .unwrap();
    assert_eq!((layout.units, layout.channels), (14_336, 2));
    if !cfg!(feature = "studio-aac") {
        let err = segment(&bare, ADTS, &req).unwrap_err();
        assert_eq!(err, ReelError::CodecUnavailable { codec: "aac-lc".into() });
    }
    // Hostile: more frames than the access unit carries.
    let liar = Registry::with_audio().with_audio_decoder(std::sync::Arc::new(FakeOs { lie: true }));
    let err = segment(&liar, ADTS, &req).unwrap_err();
    assert_eq!(err.code(), "MEDIA_DECODER_FAULT");
}
