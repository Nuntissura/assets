//! AAC-LC (Studio-owned ISO/IEC 14496-3 decoder) in ADTS, lane SMB-AUDIO-CODECS. Oracle: libavcodec's
//! native AAC decoder on files written by ffmpeg's native AAC-LC encoder (perceptual noise substitution
//! off: PNS bands are random noise by definition), stored as raw s16le
//! (tests/fixtures/audio/provenance.json). Tolerances are stated per test.
// Studio's own AAC-LC decoder is opt-in since Operator decision D24-A (lane SMB-D24A-CODEC-DEFAULTS):
// run with `--features studio-aac` (round unit reel [studio-aac]).
#![cfg(feature = "studio-aac")]
mod audio_support;
use audio_support::*;
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND};
use hsk_studio_reel::audio::*;
use hsk_studio_reel::*;
use std::io::Cursor;

const STEREO: &[u8] = include_bytes!("fixtures/audio/aac-lc-44k1-2ch.aac");
const STEREO_REF: &[u8] = include_bytes!("fixtures/audio/aac-lc-44k1-2ch.aac.ref-s16.zz");
const MONO: &[u8] = include_bytes!("fixtures/audio/aac-lc-48k-1ch.aac");
const MONO_REF: &[u8] = include_bytes!("fixtures/audio/aac-lc-48k-1ch.aac.ref-s16.zz");
const SURROUND: &[u8] = include_bytes!("fixtures/audio/aac-lc-48k-6ch.aac");
const SURROUND_REF: &[u8] = include_bytes!("fixtures/audio/aac-lc-48k-6ch.aac.ref-s16.zz");

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

/// Stereo 44.1 kHz (M/S and intensity stereo), mono 48 kHz (click train: LONG_START / EIGHT_SHORT /
/// LONG_STOP runs) and 5.1 48 kHz (channel configuration 6: SCE, CPE, CPE, LFE; libavcodec reorders to
/// FL FR FC LFE BL BR, reel keeps the AAC element order C L R Ls Rs LFE). Exact frame counts (ADTS
/// carries no priming information, so every decoded sample is presented, as libavcodec does), every
/// sample within 2/32768 of libavcodec's decoder rounded to s16 (measured values printed).
#[test]
fn aac_lc_adts_matches_libavcodec() {
    for (name, bytes, zz, rate, channels, frames) in [
        ("stereo", STEREO, STEREO_REF, 44_100u32, 2u16, 14usize),
        ("mono", MONO, MONO_REF, 48_000, 1, 16),
        ("5.1", SURROUND, SURROUND_REF, 48_000, 6, 6),
    ] {
        let mut got = decode(bytes).unwrap();
        assert_eq!((got.sample_rate, got.channels), (rate, channels), "{name}");
        assert_eq!(got.decoder_id, AAC_DECODER_ID);
        if channels == 6 {
            let order = [1usize, 2, 0, 5, 3, 4];
            got.interleaved = got
                .interleaved
                .chunks_exact(6)
                .flat_map(|f| order.map(|c| f[c]))
                .collect();
        }
        let want = reference(zz);
        assert_eq!(got.interleaved.len(), want.len(), "{name}");
        assert_eq!(got.frames(), frames * 1024, "{name}");
        let d = got
            .interleaved
            .iter()
            .zip(&want)
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        println!(
            "aac {name}: max_diff {} LSB, snr {:.1} dB",
            d * 32768.0,
            snr_db(&want, &got.interleaved)
        );
        assert!(d <= 2.0 / 32768.0, "{name}: max diff {d}");
    }
}

/// Segments through the registry are positioned exactly; past-the-end is OutOfRange before reads.
#[test]
fn aac_adts_segments_are_positioned() {
    let whole = decode(STEREO).unwrap();
    let tps = TICKS_PER_SECOND / 44_100;
    let (receipt, seg) = decode_segment(
        STEREO,
        &request(AAC_PROVIDER_ID, 2048 * tps, 4096, STEREO),
        &Limits::default(),
    )
    .unwrap();
    assert!(receipt.complete);
    assert_eq!(receipt.decoder_id, AAC_DECODER_ID);
    assert_eq!(seg.samples, whole.interleaved[4096..12_288]);
    let past = request(AAC_PROVIDER_ID, 14_000 * tps, 337, STEREO);
    assert!(matches!(
        decode_segment(STEREO, &past, &Limits::default()),
        Err(ReelError::OutOfRange { .. })
    ));
}

/// Hostile AAC: configurations outside AAC-LC (HE-AAC object type 5, 960-sample frames, program config
/// element layouts, channel configuration 8+) are typed refusals before a decoder exists; an ADTS
/// header that declares 5.1 over stereo access units is MEDIA_CORRUPT; every truncation and byte flip of
/// the stereo file decodes or fails typed, never a panic or a non-finite sample.
#[test]
fn aac_hostile_input_is_typed() {
    for (asc, what) in [
        (&[0x2B, 0x92, 0x08, 0x00][..], "aac-he-aac-sbr-ps"),
        (&[0x12, 0x14][..], "aac-960-sample-frames"),
        (&[0x12, 0x00][..], "aac-program-config-element"),
        (&[0x12, 0x40][..], "aac-channel-configuration"),
    ] {
        assert_eq!(
            aac::parse_audio_specific_config(asc).unwrap_err(),
            ReelError::Unsupported { what },
            "{asc:02x?}"
        );
    }
    let mut lying = STEREO.to_vec();
    lying[2] = (lying[2] & !1) | 1;
    lying[3] = (lying[3] & 0x3F) | 0x80;
    assert!(matches!(decode(&lying), Err(ReelError::Corrupt { .. })));
    for cut in (0..STEREO.len()).step_by(61) {
        if let Ok(a) = decode(&STEREO[..cut]) {
            assert!(a.interleaved.iter().all(|v| v.is_finite()));
        }
    }
    for at in (0..STEREO.len()).step_by(17) {
        let mut b = STEREO.to_vec();
        b[at] ^= 0xA5;
        if let Ok(a) = decode(&b) {
            assert!(
                a.interleaved
                    .iter()
                    .all(|v| v.is_finite() && v.abs() <= 8.0)
            );
        }
    }
}
