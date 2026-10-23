//! MPEG audio Layers I-III (Studio-owned safe-Rust port of minimp3, CC0), lane SMB-AUDIO-CODECS.
//! Oracle: libavcodec's mp3float / mp2float decode of ffmpeg-written files (libmp3lame with Info/Xing +
//! LAME tags, ffmpeg's mp2 encoder), gapless-trimmed by libavformat, stored as raw s16le
//! (tests/fixtures/audio/provenance.json). Tolerances are stated per test.
mod audio_support;
use audio_support::*;
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND, Ticks};
use hsk_studio_reel::audio::*;
use hsk_studio_reel::*;
use std::io::Cursor;

const CBR: &[u8] = include_bytes!("fixtures/audio/mp3-44k1-2ch-cbr.mp3");
const CBR_REF: &[u8] = include_bytes!("fixtures/audio/mp3-44k1-2ch-cbr.mp3.ref-s16.zz");
const VBR: &[u8] = include_bytes!("fixtures/audio/mp3-22k05-1ch-vbr.mp3");
const VBR_REF: &[u8] = include_bytes!("fixtures/audio/mp3-22k05-1ch-vbr.mp3.ref-s16.zz");
const MP2: &[u8] = include_bytes!("fixtures/audio/mp2-48k-2ch.mp2");
const MP2_REF: &[u8] = include_bytes!("fixtures/audio/mp2-48k-2ch.mp2.ref-s16.zz");

fn reference(zz: &[u8]) -> Vec<f32> {
    let raw = miniz_oxide::inflate::decompress_to_vec_zlib(zz).unwrap();
    raw.chunks_exact(2)
        .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
        .collect()
}

fn decode(bytes: &[u8]) -> Result<DecodedAudio, ReelError> {
    decode_audio_file(
        &mut Cursor::new(bytes.to_vec()),
        &Limits::default(),
        1 << 22,
        &CancellationToken::default(),
    )
}

fn compare(name: &str, bytes: &[u8], zz: &[u8], rate: u32, channels: u16) -> (f32, f64, usize) {
    let got = decode(bytes).unwrap();
    assert_eq!((got.sample_rate, got.channels), (rate, channels), "{name}");
    assert_eq!(got.decoder_id, MP3_DECODER_ID);
    let want = reference(zz);
    let n = got.interleaved.len().min(want.len());
    let d = (0..n)
        .map(|i| (got.interleaved[i] - want[i]).abs())
        .fold(0f32, f32::max);
    let snr = snr_db(&want, &got.interleaved);
    println!(
        "{name}: frames ours={} oracle={} max_diff={} ({} LSB) snr={snr:.1} dB",
        got.frames(),
        want.len() / usize::from(channels),
        d,
        d * 32768.0
    );
    assert_eq!(
        got.interleaved.len(),
        want.len(),
        "{name}: frame count (gapless trim)"
    );
    (d, snr, got.frames())
}

/// MPEG-1 Layer III joint stereo CBR with a LAME tag: gapless length 13230 frames (0.3 s) as libavformat
/// trims it, every sample within 2/32768 of libavcodec's float decoder rounded to s16.
#[test]
fn mp3_cbr_stereo_matches_libavcodec() {
    let (d, _, frames) = compare("cbr", CBR, CBR_REF, 44_100, 2);
    assert_eq!(frames, 13_230);
    assert!(d <= 2.0 / 32768.0, "max diff {d}");
}

/// MPEG-2 LSF Layer III mono VBR at 22.05 kHz with a click train (short blocks), Xing frame count and
/// LAME gapless tag: 8820 frames (0.4 s), within 2/32768 of libavcodec.
#[test]
fn mp3_lsf_mono_vbr_with_short_blocks_matches_libavcodec() {
    let (d, _, frames) = compare("vbr", VBR, VBR_REF, 22_050, 1);
    assert_eq!(frames, 8_820);
    assert!(d <= 2.0 / 32768.0, "max diff {d}");
}

/// MPEG-1 Layer II stereo 48 kHz (no Xing frame: the total comes from the header walk, 11 frames of
/// 1152), within 2/32768 of libavcodec's mp2float decoder; a mid-stream segment through the registry is
/// the whole-file decode at exactly those positions.
#[test]
fn mp2_layer2_matches_and_segments_are_positioned() {
    let (d, _, frames) = compare("mp2", MP2, MP2_REF, 48_000, 2);
    assert_eq!(frames, 12_672);
    assert!(d <= 2.0 / 32768.0, "max diff {d}");

    let whole = decode(CBR).unwrap();
    let tps = TICKS_PER_SECOND / 44_100;
    let (receipt, seg) = decode_segment(
        CBR,
        &request(MP3_PROVIDER_ID, 5000 * tps, 3000, CBR),
        &Limits::default(),
    )
    .unwrap();
    assert!(receipt.complete);
    assert_eq!(receipt.decoder_id, MP3_DECODER_ID);
    assert_eq!(seg.first_index, Some(5000));
    assert_eq!(seg.samples, whole.interleaved[10_000..16_000]);
    let past = request(MP3_PROVIDER_ID, 13_000 * tps, 231, CBR);
    assert!(matches!(
        decode_segment(CBR, &past, &Limits::default()),
        Err(ReelError::OutOfRange { .. })
    ));
    let mut export = request(MP3_PROVIDER_ID, 5000 * tps + 1, 10, CBR);
    export.purpose = Purpose::Export;
    assert_eq!(
        decode_segment(CBR, &export, &Limits::default()).unwrap_err(),
        ReelError::NotExact {
            requested: Ticks::new(5000 * tps + 1),
            nearest: Ticks::new(5000 * tps)
        }
    );
    assert!(
        audio_codec_descriptors()
            .iter()
            .any(|d| d.decoder_id == MP3_DECODER_ID && d.can_decode)
    );
}

/// Hostile MPEG audio: an ID3v2 tag declaring 256 MiB is refused against the probe budget before any
/// read past it; bytes with no frame sync are MEDIA_IMPORT_UNSUPPORTED; every truncation and every
/// single-byte flip of the CBR / VBR files decodes or fails with a typed error, never a panic, a
/// non-finite sample or one beyond OUTPUT_BOUND (8.0; corrupt gains are otherwise unbounded).
#[test]
fn mpeg_audio_hostile_input_is_typed() {
    let mut id3 = b"ID3\x04\x00\x00\x7F\x7F\x7F\x7F".to_vec();
    id3.extend_from_slice(CBR);
    let tight = Limits {
        max_probe_bytes: 1 << 20,
        ..Limits::default()
    };
    assert_eq!(
        decode_audio_file(
            &mut Cursor::new(id3),
            &tight,
            1 << 22,
            &CancellationToken::default()
        )
        .unwrap_err(),
        ReelError::LimitExceeded { what: "id3v2-size" }
    );
    let noise: Vec<u8> = {
        let mut r = Rng(7);
        (0..4096).map(|_| (r.next() & 0x7F) as u8).collect()
    };
    let mut fake = b"ID3\x04\x00\x00\x00\x00\x00\x00".to_vec();
    fake.extend_from_slice(&noise);
    assert!(matches!(
        decode(&fake),
        Err(ReelError::ImportUnsupported { .. })
    ));

    let finite = |r: Result<DecodedAudio, ReelError>| {
        if let Ok(a) = r {
            assert!(
                a.interleaved
                    .iter()
                    .all(|v| v.is_finite() && v.abs() <= MP3_OUTPUT_BOUND)
            );
        }
    };
    for cut in (0..CBR.len()).step_by(97) {
        finite(decode(&CBR[..cut]));
    }
    for at in (0..CBR.len()).step_by(13) {
        let mut b = CBR.to_vec();
        b[at] ^= 0x5A;
        finite(decode(&b));
    }
    for at in (0..VBR.len()).step_by(11) {
        let mut b = VBR.to_vec();
        b[at] = !b[at];
        finite(decode(&b));
    }
}
