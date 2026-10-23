//! AIFF / AIFF-C read and write, lane SMB-AUDIO-CODECS. Oracle: sha256 of libavcodec's decode of
//! ffmpeg-written files as raw f32le (tests/fixtures/audio/provenance.json); the writer is proved by
//! round trips through the reader.
mod audio_support;
use audio_support::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_reel::audio::*;
use hsk_studio_reel::*;
use std::io::Cursor;

fn decode(bytes: &[u8]) -> Result<DecodedAudio, ReelError> {
    decode_audio_file(
        &mut Cursor::new(bytes.to_vec()),
        &Limits::default(),
        1 << 22,
        &CancellationToken::default(),
    )
}

fn f32le(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// 16-bit and 24-bit AIFF, AIFF-C fl32, ulaw and alaw decode bit-exactly as libavcodec does (sha256 of
/// the f32le samples).
#[test]
fn aiff_and_aifc_decode_bit_exact_against_libavcodec() {
    for (bytes, rate, channels, sha) in [
        (
            &include_bytes!("fixtures/audio/aiff-s16-44k1-2ch.aiff")[..],
            44_100u32,
            2u16,
            "e683f2ba74351113188ab3aebb602e9989eac6bf6fb62e50b4094b496659cece",
        ),
        (
            &include_bytes!("fixtures/audio/aiff-s24-48k-1ch.aiff")[..],
            48_000,
            1,
            "0b7792a2717e961aa3071fcb59055edcd658c1a85f0ed819b1e88d98f8b8b5ab",
        ),
        (
            &include_bytes!("fixtures/audio/aifc-f32-48k-2ch.aifc")[..],
            48_000,
            2,
            "f2cfab372a6ec1e1a899121412d63bcae5de740f7ae84576edfc3d80b3a141de",
        ),
        (
            &include_bytes!("fixtures/audio/aifc-ulaw-8k-1ch.aifc")[..],
            8_000,
            1,
            "0df1218708e3e16a72c9332666d80b1bdf1ff611cb7f8b9926f8c6c5f00ebb4d",
        ),
        (
            &include_bytes!("fixtures/audio/aifc-alaw-8k-1ch.aifc")[..],
            8_000,
            1,
            "4860364f15bf5e78ebc0a78100976d8cafb9639ae2df47f3db4ef22ee2850364",
        ),
    ] {
        let got = decode(bytes).unwrap();
        assert_eq!((got.sample_rate, got.channels), (rate, channels));
        assert_eq!(
            sha256_hex(&f32le(&got.interleaved)),
            sha,
            "{rate} {channels}"
        );
    }
}

struct Once(Option<(Vec<u8>, u64)>);
impl ChunkSource for Once {
    fn next_chunk(&mut self, buf: &mut Vec<u8>) -> Result<Option<u64>, ReelError> {
        Ok(self.0.take().map(|(b, n)| {
            buf.extend_from_slice(&b);
            n
        }))
    }
}

/// The AIFF writer (8/16/24/32-bit integer, AIFF-C fl32/fl64) round-trips through the reader: integer
/// samples come back as v / 2^(bits-1), floats unchanged; odd data lengths get the pad byte.
#[test]
fn aiff_writer_round_trips() {
    let provider = AudioFileProvider::aiff();
    for (bits, float, channels, frames) in [
        (8u16, false, 1u16, 1001u64),
        (16, false, 2, 1000),
        (24, false, 3, 777),
        (32, false, 2, 500),
        (32, true, 2, 640),
        (64, true, 1, 300),
    ] {
        let mut r = Rng(u64::from(bits) * 31 + u64::from(channels));
        let n = (frames * u64::from(channels)) as usize;
        let width = usize::from(bits / 8);
        let mut wav = Vec::new();
        let mut want = Vec::new();
        for _ in 0..n {
            let x = r.next();
            match (bits, float) {
                (8, _) => {
                    let u = (x & 0xFF) as u8;
                    wav.push(u);
                    want.push((f32::from(u) - 128.0) / 128.0);
                }
                (32, true) => {
                    let v = ((x % 20_001) as f32 - 10_000.0) / 10_000.0;
                    wav.extend_from_slice(&v.to_le_bytes());
                    want.push(v);
                }
                (64, true) => {
                    let v = ((x % 20_001) as f64 - 10_000.0) / 10_000.0;
                    wav.extend_from_slice(&v.to_le_bytes());
                    want.push(v as f32);
                }
                _ => {
                    let shift = 64 - u32::from(bits);
                    let v = ((x << shift) as i64) >> shift;
                    wav.extend_from_slice(&v.to_le_bytes()[..width]);
                    want.push((v as f64 / 2f64.powi(i32::from(bits) - 1)) as f32);
                }
            }
        }
        let mut out = Vec::new();
        let receipt = provider
            .encode_segment(
                &EncodeSpec {
                    codec: AIFF_PROVIDER_ID.into(),
                    params: EncodeParams::Audio {
                        rate: 44_100,
                        channels,
                        bits,
                        float,
                    },
                    total_units: frames,
                },
                &mut Once(Some((wav, frames))),
                &mut out,
                &Limits::default(),
                &CancellationToken::default(),
            )
            .unwrap();
        assert!(receipt.complete);
        assert_eq!(receipt.bytes_written, out.len() as u64);
        assert_eq!(out.len() % 2, 0, "IFF chunks are even-padded");
        let got = decode(&out).unwrap();
        assert_eq!((got.sample_rate, got.channels), (44_100, channels));
        assert_eq!(got.interleaved, want, "bits {bits} float {float}");
    }
}

/// Hostile AIFF headers are typed refusals before any sample buffer is sized: absurd rate, zero and
/// absurd channel counts, unknown AIFF-C compression, a chunk walk past the probe budget, a truncated
/// sound data chunk (Truncated after the whole frames that exist).
#[test]
fn aiff_hostile_headers_are_typed() {
    let base = include_bytes!("fixtures/audio/aiff-s16-44k1-2ch.aiff").to_vec();
    let comm = base.windows(4).position(|w| w == b"COMM").unwrap() + 8;
    let set = |at: usize, bytes: &[u8]| {
        let mut b = base.clone();
        b[comm + at..comm + at + bytes.len()].copy_from_slice(bytes);
        b
    };
    // 2^40 Hz (exponent 16383 + 40).
    let e = (16_383u16 + 40).to_be_bytes();
    assert_eq!(
        decode(&set(8, &[e[0], e[1], 0x80, 0, 0, 0, 0, 0, 0, 0])).unwrap_err(),
        ReelError::LimitExceeded {
            what: "aiff-sample-rate"
        }
    );
    assert!(matches!(
        decode(&set(0, &[0, 0])),
        Err(ReelError::Corrupt { .. })
    ));
    assert_eq!(
        decode(&set(0, &[0xFF, 0xFF])).unwrap_err(),
        ReelError::LimitExceeded {
            what: "aiff-channels"
        }
    );
    let mut aifc = include_bytes!("fixtures/audio/aifc-ulaw-8k-1ch.aifc").to_vec();
    let at = aifc.windows(4).position(|w| w == b"ulaw").unwrap();
    aifc[at..at + 4].copy_from_slice(b"ima4");
    assert_eq!(
        decode(&aifc).unwrap_err(),
        ReelError::CodecUnavailable {
            codec: "aifc-ima4".into()
        }
    );
    let tight = Limits {
        max_probe_bytes: 8,
        ..Limits::default()
    };
    assert!(matches!(
        decode_audio_file(
            &mut Cursor::new(base.clone()),
            &tight,
            1 << 22,
            &CancellationToken::default()
        ),
        Err(ReelError::LimitExceeded { .. })
    ));
    assert!(matches!(
        decode(&base[..base.len() - 1000]),
        Err(ReelError::Truncated { .. })
    ));
}
