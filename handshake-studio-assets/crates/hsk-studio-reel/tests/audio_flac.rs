//! FLAC (RFC 9639) decode and encode, lane SMB-AUDIO-CODECS. Oracles: libavcodec's decode of two
//! ffmpeg-written files (sha256 of its raw integer PCM, tests/fixtures/audio/provenance.json) and the
//! MD5 each file's STREAMINFO carries; the Studio encoder is proved by bit-exact round trips.
mod audio_support;
use audio_support::*;
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_reel::*;
use std::io::Cursor;

const S16: &[u8] = include_bytes!("fixtures/audio/flac-s16-44k1-2ch.flac");
const S24: &[u8] = include_bytes!("fixtures/audio/flac-s24-48k-1ch.flac");

fn ints_le(ints: &[i32], bytes: usize) -> Vec<u8> {
    ints.iter()
        .flat_map(|v| v.to_le_bytes()[..bytes].to_vec())
        .collect()
}

fn integers(bytes: &[u8]) -> Result<(audio::flac::StreamInfo, Vec<i32>, Option<bool>), ReelError> {
    decode_flac_integers(
        &mut Cursor::new(bytes.to_vec()),
        &Limits::default(),
        10_000_000,
        &CancellationToken::default(),
    )
}

/// Bit-exact against libavcodec: the integer samples, packed as ffmpeg's raw s16le / s24le output, hash
/// to the oracle's sha256, and the MD5 of STREAMINFO (computed by ffmpeg's encoder over its input)
/// matches what the decoder recomputed. The 16-bit file uses LPC subframes and stereo decorrelation.
#[test]
fn flac_decode_is_bit_exact_against_libavcodec_and_streaminfo_md5() {
    let (info, ints, md5) = integers(S16).unwrap();
    assert_eq!(
        (info.sample_rate, info.channels, info.bits_per_sample),
        (44_100, 2, 16)
    );
    assert_eq!(ints.len(), 22_050);
    assert_eq!(md5, Some(true), "STREAMINFO MD5");
    assert_eq!(
        sha256_hex(&ints_le(&ints, 2)),
        "38deace2c2c67a6388fc287290ed8705d5dd2fcca396fabb5cf4a97863b7403e"
    );
    let (info, ints, md5) = integers(S24).unwrap();
    assert_eq!(
        (info.sample_rate, info.channels, info.bits_per_sample),
        (48_000, 1, 24)
    );
    assert_eq!(ints.len(), 9_600);
    assert_eq!(md5, Some(true));
    assert_eq!(
        sha256_hex(&ints_le(&ints, 3)),
        "943d25f9124101846deb93f2bf02e75cc337d32e86d1d3dad6f1d152014e4445"
    );
}

/// Studio encoder -> Studio decoder is bit-exact for 16-bit stereo and 24-bit 3-channel signals with a
/// short final block, through both `encode_flac` (full STREAMINFO, MD5 verified) and the streaming
/// `encode_segment` port of the `flac` provider (MD5 unknown, samples equal).
#[test]
fn flac_encoder_round_trips_bit_exact() {
    for (rate, channels, bits, frames) in
        [(44_100u32, 2u8, 16u8, 10_001usize), (96_000, 3, 24, 5_000)]
    {
        let mut rng = Rng(0x5eed ^ u64::from(bits));
        let amp = (1i64 << (bits - 1)) - 1;
        let ints: Vec<i32> = (0..frames * usize::from(channels))
            .map(|i| {
                let t = (i / usize::from(channels)) as f64;
                let tone = (t * 0.05 * (1.0 + (i % usize::from(channels)) as f64)).sin() * 0.6;
                let noise = (rng.next() % 2001) as f64 / 1000.0 - 1.0;
                ((tone + noise * 0.05) * amp as f64) as i32
            })
            .collect();
        let bytes = encode_flac(rate, channels, bits, &ints).unwrap();
        assert!(
            bytes.len() < ints.len() * usize::from(bits / 8),
            "compresses"
        );
        let (info, decoded, md5) = integers(&bytes).unwrap();
        assert_eq!(
            (info.sample_rate, info.channels, info.total_samples),
            (rate, channels, frames as u64)
        );
        assert_eq!(md5, Some(true));
        assert_eq!(decoded, ints);

        // Streaming port: WAV byte layout in, FLAC out.
        let provider = AudioFileProvider::flac();
        let raw = ints_le(&ints, usize::from(bits / 8));
        struct Once(Option<(Vec<u8>, u64)>);
        impl ChunkSource for Once {
            fn next_chunk(&mut self, buf: &mut Vec<u8>) -> Result<Option<u64>, ReelError> {
                Ok(self.0.take().map(|(b, n)| {
                    buf.extend_from_slice(&b);
                    n
                }))
            }
        }
        let mut out = Vec::new();
        let receipt = provider
            .encode_segment(
                &EncodeSpec {
                    codec: FLAC_PROVIDER_ID.into(),
                    params: EncodeParams::Audio {
                        rate,
                        channels: u16::from(channels),
                        bits: u16::from(bits),
                        float: false,
                    },
                    total_units: frames as u64,
                },
                &mut Once(Some((raw, frames as u64))),
                &mut out,
                &Limits::default(),
                &CancellationToken::default(),
            )
            .unwrap();
        assert!(receipt.complete);
        assert_eq!(receipt.bytes_written, out.len() as u64);
        let (_, streamed, md5) = integers(&out).unwrap();
        assert_eq!(md5, None, "the streaming header leaves MD5 unknown");
        assert_eq!(streamed, ints);
    }
}

/// Segment delivery through the registry: the provider is found by magic bytes, a mid-stream range is
/// the decoder's samples at exactly those positions, an export start inside a sample is NotExact and a
/// range past the declared end is OutOfRange, both before the sink is touched.
#[test]
fn flac_segment_delivery_is_exact_and_bounded() {
    let (_, ints, _) = integers(S16).unwrap();
    let tps = TICKS_PER_SAMPLE_44K1;
    let (receipt, got) = decode_segment(
        S16,
        &request(FLAC_PROVIDER_ID, 1000 * tps, 2000, S16),
        &Limits::default(),
    )
    .unwrap();
    assert!(receipt.complete);
    assert_eq!(receipt.disposition, Disposition::Exact);
    assert_eq!(receipt.decoder_id, FLAC_DECODER_ID);
    assert_eq!(receipt.actual.units, 2000);
    assert_eq!(got.first_index, Some(1000));
    let want: Vec<f32> = ints[2000..6000]
        .iter()
        .map(|v| *v as f32 / 32768.0)
        .collect();
    assert_eq!(got.samples, want);

    // Small chunks: still exact, more chunks.
    let small = Limits {
        max_chunk_bytes: 8 * 100,
        ..Limits::default()
    };
    let (receipt, got) = decode_segment(
        S16,
        &request(FLAC_PROVIDER_ID, 1000 * tps, 2000, S16),
        &small,
    )
    .unwrap();
    assert_eq!(receipt.chunks, 20);
    assert_eq!(got.samples, want);

    let mut export = request(FLAC_PROVIDER_ID, 1000 * tps + 1, 10, S16);
    export.purpose = Purpose::Export;
    assert_eq!(
        decode_segment(S16, &export, &Limits::default()).unwrap_err(),
        ReelError::NotExact {
            requested: Ticks::new(1000 * tps + 1),
            nearest: Ticks::new(1000 * tps)
        }
    );
    let past = decode_segment(
        S16,
        &request(FLAC_PROVIDER_ID, 11_000 * tps, 26, S16),
        &Limits::default(),
    );
    assert!(
        matches!(past, Err(ReelError::OutOfRange { .. })),
        "{past:?}"
    );
    assert!(
        Registry::standard()
            .audio_codec_descriptors()
            .iter()
            .any(|d| d.decoder_id == FLAC_DECODER_ID && d.can_decode && d.can_encode)
    );
}

const TICKS_PER_SAMPLE_44K1: u64 = hsk_studio_accord::TICKS_PER_SECOND / 44_100;

/// dims_u32_max: header fields pushed to their limits are typed refusals before allocation; corrupt
/// and truncated frames are typed errors, never a panic.
#[test]
fn flac_hostile_headers_are_refused_before_allocation() {
    let set_streaminfo = |f: &dyn Fn(&mut audio::flac::StreamInfo)| {
        let (mut info, _, _) = integers(S16).unwrap();
        f(&mut info);
        let mut bytes = S16.to_vec();
        bytes[8..42].copy_from_slice(&info.to_bytes());
        bytes
    };
    // Absurd sample rate (20-bit field maximum, 1 048 575 Hz).
    let bytes = set_streaminfo(&|i| i.sample_rate = (1 << 20) - 1);
    assert_eq!(
        integers(&bytes).unwrap_err(),
        ReelError::LimitExceeded {
            what: "flac-sample-rate"
        }
    );
    // Zero sample rate and an inverted block-size range are corrupt headers.
    assert!(matches!(
        integers(&set_streaminfo(&|i| i.sample_rate = 0)),
        Err(ReelError::Corrupt { .. })
    ));
    assert!(matches!(
        integers(&set_streaminfo(&|i| i.min_block_size = u16::MAX)),
        Err(ReelError::Corrupt { .. })
    ));
    // A frame larger than STREAMINFO allows: declare a 16-sample maximum block.
    let bytes = set_streaminfo(&|i| {
        i.min_block_size = 16;
        i.max_block_size = 16;
    });
    assert_eq!(
        integers(&bytes).unwrap_err(),
        ReelError::LimitExceeded {
            what: "flac-block-size"
        }
    );
    // A metadata block whose 24-bit length runs past the probe budget.
    let mut bytes = S16.to_vec();
    bytes[4] &= 0x7F; // STREAMINFO no longer last
    bytes.splice(42..42, [0x81, 0xFF, 0xFF, 0xFF]); // PADDING block of 16 MiB - 1, last
    let tight = Limits {
        max_probe_bytes: 4096,
        ..Limits::default()
    };
    assert_eq!(
        decode_flac_integers(
            &mut Cursor::new(bytes),
            &tight,
            1 << 20,
            &CancellationToken::default()
        )
        .unwrap_err(),
        ReelError::LimitExceeded {
            what: "flac-metadata"
        }
    );
    // Truncated mid-frame and a flipped payload byte.
    assert!(matches!(
        integers(&S16[..S16.len() - 100]),
        Err(ReelError::Truncated { .. })
    ));
    let mut flipped = S16.to_vec();
    flipped[S16.len() - 500] ^= 0x10; // inside the last frame (the metadata ends with PADDING)
    assert!(
        matches!(
            integers(&flipped),
            Err(ReelError::Corrupt { .. } | ReelError::Truncated { .. })
        ),
        "a flipped bit is caught by the frame CRC or the frame parser"
    );
    // Every single-byte truncation of the first 200 bytes is a typed error.
    for cut in 0..200 {
        assert!(integers(&S16[..cut]).is_err(), "cut {cut}");
    }
}

/// aggregate_over_budget: a whole-file decode is refused before decoding when the declared total exceeds
/// the frame budget, and by the running total when the stream declares no total.
#[test]
fn flac_aggregate_over_budget() {
    let decode = |bytes: &[u8], max: u64| {
        decode_audio_file(
            &mut Cursor::new(bytes.to_vec()),
            &Limits::default(),
            max,
            &CancellationToken::default(),
        )
    };
    assert_eq!(
        decode(S16, 11_024).unwrap_err(),
        ReelError::LimitExceeded {
            what: "audio-frame-budget"
        }
    );
    let whole = decode(S16, 11_025).unwrap();
    assert_eq!(
        (whole.frames(), whole.channels, whole.checksum_verified),
        (11_025, 2, Some(true))
    );
    assert_eq!(whole.planar()[1].len(), 11_025);
    // Unknown total (0) and unknown MD5: the running total refuses.
    let mut bytes = S16.to_vec();
    let (mut info, _, _) = integers(S16).unwrap();
    info.total_samples = 0;
    info.md5 = [0; 16];
    bytes[8..42].copy_from_slice(&info.to_bytes());
    assert_eq!(
        decode(&bytes, 5_000).unwrap_err(),
        ReelError::LimitExceeded {
            what: "audio-frame-budget"
        }
    );
    assert_eq!(decode(&bytes, 20_000).unwrap().checksum_verified, None);
}
