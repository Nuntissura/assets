//! Compressed audio inside ISO BMFF and Matroska (codec dispatch hooks), lane SMB-AUDIO-CODECS. The
//! container files are REMUXES (-c copy) of this directory's elementary-stream fixtures, so the oracle
//! is the standalone decode of the same coded bytes, itself proven against libavcodec in
//! tests/audio_{aac,flac,ogg,mp3}.rs (tests/fixtures/audio/generate-containers.sh).
mod audio_support;
use audio_support::*;
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND, Ticks};
use hsk_studio_reel::audio::*;
use hsk_studio_reel::*;
use std::io::Cursor;

const MP4: &[u8] = include_bytes!("fixtures/audio/audio-tracks.mp4");
const MKV: &[u8] = include_bytes!("fixtures/audio/audio-tracks.mkv");
const AAC: &[u8] = include_bytes!("fixtures/audio/aac-lc-44k1-2ch.aac");
const FLAC: &[u8] = include_bytes!("fixtures/audio/flac-s16-44k1-2ch.flac");
const OPUS: &[u8] = include_bytes!("fixtures/audio/opus-48k-2ch.opus");
const VORBIS: &[u8] = include_bytes!("fixtures/audio/vorbis-44k1-2ch.ogg");
const MP3: &[u8] = include_bytes!("fixtures/audio/mp3-44k1-2ch-cbr.mp3");

fn standalone(bytes: &[u8]) -> DecodedAudio {
    decode_audio_file(
        &mut Cursor::new(bytes.to_vec()),
        &Limits::default(),
        1 << 22,
        &CancellationToken::default(),
    )
    .unwrap()
}

/// Decode `count` frames of container track `track` from presentation zero.
fn container(
    provider: &dyn MediaProvider,
    id: &str,
    bytes: &[u8],
    track: u32,
    rate: u32,
    count: u64,
) -> Result<(SegmentReceipt, Vec<f32>), ReelError> {
    let mut req = request(id, 0, count, bytes);
    req.track = Some(track);
    let mut sink = Collect::default();
    let receipt = provider.decode_segment(
        &mut Cursor::new(bytes.to_vec()),
        &req,
        &Limits::default(),
        &CancellationToken::default(),
        &mut sink,
    )?;
    assert_eq!(receipt.actual.start_pts, Ticks::new(0));
    let _ = rate;
    Ok((receipt, sink.samples))
}

/// MP4 tracks `mp4a` (AAC, esds 0x40), `fLaC` (dfLa), `Opus` (dOps; edit list media_time 312 = pre-skip)
/// and `mp4a` (MPEG audio, esds 0x6B; edit list media_time 1105 = LAME delay + 529): each container
/// decode equals the standalone decode of the same coded bytes sample for sample, and the presented
/// length follows the edit list bounded by the media (AAC: the 326 ms edit rounds past the 14336
/// decoded frames, so 14336 are presented).
#[test]
fn mp4_audio_tracks_decode_like_their_elementary_streams() {
    let provider = IsobmffProvider;
    for (track, name, bytes, rate, frames) in [
        (1u32, "aac", AAC, 44_100u32, 14_336u64),
        (2, "flac", FLAC, 44_100, 11_025),
        (3, "opus", OPUS, 48_000, 12_000),
        (4, "mp3", MP3, 44_100, 13_230),
    ] {
        let want = standalone(bytes);
        let (receipt, got) = container(&provider, ISOBMFF_PROVIDER_ID, MP4, track, rate, frames)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(receipt.decoder_id, want.decoder_id, "{name}");
        assert!(receipt.complete, "{name}");
        assert_eq!(got, want.interleaved[..got.len()], "{name}");
        // One frame past the presented end is out of range before any payload read (AAC / FLAC /
        // Opus); MPEG audio presents more than the gapless standalone length (the remux keeps the
        // encoder padding the LAME tag described).
        let past = container(&provider, ISOBMFF_PROVIDER_ID, MP4, track, rate, frames + 1);
        if name != "mp3" {
            assert!(
                matches!(past, Err(ReelError::OutOfRange { .. })),
                "{name}: {past:?}"
            );
        }
    }
}

/// Matroska tracks A_AAC, A_FLAC, A_OPUS (CodecDelay = pre-skip), A_VORBIS (Xiph-laced CodecPrivate)
/// and A_MPEG/L3 decode like their elementary streams sample for sample (the MPEG audio remux carries
/// no gapless information, so its decode starts 1105 = LAME delay + 529 frames before the gapless
/// standalone decode, measured); a mid-stream segment is the whole decode at exactly those positions.
/// Matroska declares no total: a range past the end is MEDIA_TRUNCATED with the delivered count.
#[test]
fn matroska_audio_tracks_decode_like_their_elementary_streams() {
    let provider = MatroskaProvider;
    for (track, name, bytes, rate, frames, lead) in [
        (1u32, "aac", AAC, 44_100u32, 14_336u64, 0usize),
        (2, "flac", FLAC, 44_100, 11_025, 0),
        (3, "opus", OPUS, 48_000, 12_000, 0),
        (4, "vorbis", VORBIS, 44_100, 11_000, 0),
        (5, "mp3", MP3, 44_100, 13_000, 1105),
    ] {
        let want = standalone(bytes);
        let (receipt, got) = container(&provider, MATROSKA_PROVIDER_ID, MKV, track, rate, frames)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(receipt.decoder_id, want.decoder_id, "{name}");
        let got = &got[lead * 2..];
        assert_eq!(got, &want.interleaved[..got.len()], "{name}");
    }
    let whole = standalone(FLAC);
    let tps = TICKS_PER_SECOND / 44_100;
    let mut req = request(MATROSKA_PROVIDER_ID, 4000 * tps, 2000, MKV);
    req.track = Some(2);
    let mut sink = Collect::default();
    provider
        .decode_segment(
            &mut Cursor::new(MKV.to_vec()),
            &req,
            &Limits::default(),
            &CancellationToken::default(),
            &mut sink,
        )
        .unwrap();
    assert_eq!(sink.samples, whole.interleaved[8000..12_000]);
    let past = container(&provider, MATROSKA_PROVIDER_ID, MKV, 2, 44_100, 11_026);
    assert_eq!(
        past.unwrap_err(),
        ReelError::Truncated { delivered: 11_025 }
    );
}

/// Hooks fail typed: an MP4 audio entry whose esds names an object type reel does not decode is
/// MEDIA_CODEC_UNAVAILABLE naming it, a damaged esds is MEDIA_CORRUPT, and packet output of the same
/// tracks is unchanged (still the coded samples).
#[test]
fn container_audio_hooks_fail_typed() {
    let esds = MP4.windows(4).position(|w| w == b"esds").unwrap();
    // ES_Descriptor (tag 3) ... DecoderConfigDescriptor (tag 4): objectTypeIndication follows its
    // length byte(s); find tag 4 after the esds header and patch the OTI to 0xA5 (AC-3).
    let tag4 = esds + 8 + MP4[esds + 8..].iter().position(|b| *b == 0x04).unwrap();
    let mut len_end = tag4 + 1;
    while MP4[len_end] & 0x80 != 0 {
        len_end += 1;
    }
    let mut ac3 = MP4.to_vec();
    ac3[len_end + 1] = 0xA5;
    let provider = IsobmffProvider;
    assert_eq!(
        container(&provider, ISOBMFF_PROVIDER_ID, &ac3, 1, 44_100, 10).unwrap_err(),
        ReelError::CodecUnavailable {
            codec: "mp4a.a5".into()
        }
    );
    let mut broken = MP4.to_vec();
    broken[esds + 8] = 0x07;
    assert!(matches!(
        container(&provider, ISOBMFF_PROVIDER_ID, &broken, 1, 44_100, 10),
        Err(ReelError::Corrupt { .. })
    ));
    let mut req = request(ISOBMFF_PROVIDER_ID, 0, 3, MP4);
    req.track = Some(2);
    req.output = OutputMode::Packets;
    let mut sink = Packets::default();
    provider
        .decode_segment(
            &mut Cursor::new(MP4.to_vec()),
            &req,
            &Limits::default(),
            &CancellationToken::default(),
            &mut sink,
        )
        .unwrap();
    assert_eq!(sink.0, 3);
}

#[derive(Default)]
struct Packets(u64);
impl ChunkSink for Packets {
    fn accept(&mut self, chunk: Chunk<'_>) -> Result<(), ReelError> {
        assert_eq!(chunk.kind, ChunkKind::Packet);
        self.0 += chunk.units;
        Ok(())
    }
}
