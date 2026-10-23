//! Ogg (RFC 3533) with Vorbis, Opus and FLAC, lane SMB-AUDIO-CODECS. Oracles: libavcodec's decode of
//! ffmpeg-written files (libvorbis / libopus encoders; Vorbis decoded by libavcodec's native decoder,
//! Opus by libopus itself, the RFC 6716 reference decoder) stored as raw s16le
//! (tests/fixtures/audio/provenance.json). Tolerances are stated per test.
mod audio_support;
use audio_support::*;
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND, Ticks};
use hsk_studio_reel::audio::*;
use hsk_studio_reel::*;
use std::io::Cursor;

const VORBIS: &[u8] = include_bytes!("fixtures/audio/vorbis-44k1-2ch.ogg");
const VORBIS_REF: &[u8] = include_bytes!("fixtures/audio/vorbis-44k1-2ch.ogg.ref-s16.zz");
const OPUS2: &[u8] = include_bytes!("fixtures/audio/opus-48k-2ch.opus");
const OPUS2_REF: &[u8] = include_bytes!("fixtures/audio/opus-48k-2ch.opus.ref-s16.zz");
const OPUS6: &[u8] = include_bytes!("fixtures/audio/opus-48k-6ch.opus");
const OPUS6_REF: &[u8] = include_bytes!("fixtures/audio/opus-48k-6ch.opus.ref-s16.zz");
const OGG_FLAC: &[u8] = include_bytes!("fixtures/audio/flac-in-ogg-32k-1ch.oga");
const OGG_FLAC_REF: &[u8] = include_bytes!("fixtures/audio/flac-in-ogg-32k-1ch.oga.ref-s16.zz");

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

/// Largest |a - b| over the common length, and that length.
fn max_diff(a: &[f32], b: &[f32]) -> (f32, usize) {
    let n = a.len().min(b.len());
    let d = (0..n).map(|i| (a[i] - b[i]).abs()).fold(0f32, f32::max);
    (d, n)
}

/// Vorbis through lewton against libavcodec's Vorbis decoder: same frame count as the stream's last
/// granule position (11025 = 0.25 s at 44.1 kHz) and every sample within 2/32768 of the s16 oracle
/// (1 for the oracle's own s16 quantisation, 1 for float differences between two IMDCTs).
#[test]
fn vorbis_matches_libavcodec_within_two_lsb() {
    let got = decode(VORBIS).unwrap();
    assert_eq!((got.sample_rate, got.channels), (44_100, 2));
    assert_eq!(got.decoder_id, VORBIS_DECODER_ID);
    let want = reference(VORBIS_REF);
    println!(
        "vorbis frames ours={} oracle={}",
        got.frames(),
        want.len() / 2
    );
    // Measured: the oracle starts with our first sample (alignment searched over 0..2048 frames, best
    // offset 0) and ends 128 frames before the last granule position (11025) that reel honours.
    let (d, n) = max_diff(&got.interleaved, &want);
    println!(
        "vorbis max_diff={} ({} LSB) over {} frames",
        d,
        d * 32768.0,
        n / 2
    );
    assert_eq!(got.frames(), 11_025);
    assert_eq!(n / 2, 10_897);
    assert!(d <= 2.0 / 32768.0, "max diff {d}");
}

/// Opus through opus-decoder against libopus (the reference decoder), stereo (mapping family 0) and 5.1
/// (family 1, four streams, two coupled; reel keeps the stream's Vorbis channel order): pre-skip removed
/// and the end trimmed by the last granule (12000 and 9600 frames), SNR against libopus at least 60 dB
/// per file and every sample within 1/32768 of the s16 oracle (measured: 0.5 LSB, 80 dB).
#[test]
fn opus_stereo_and_surround_match_libopus() {
    for (bytes, zz, channels, frames) in [
        (OPUS2, OPUS2_REF, 2u16, 12_000usize),
        (OPUS6, OPUS6_REF, 6, 9_600),
    ] {
        let mut got = decode(bytes).unwrap();
        assert_eq!((got.sample_rate, got.channels), (48_000, channels));
        let want = reference(zz);
        if channels == 6 {
            // reel delivers the stream's own order (RFC 7845 family 1 = Vorbis order: FL FC FR BL BR
            // LFE); libavcodec reorders to FL FR FC LFE BL BR. Permute ours to the oracle's order.
            let order = [0usize, 2, 1, 5, 3, 4];
            got.interleaved = got
                .interleaved
                .chunks_exact(6)
                .flat_map(|f| order.map(|c| f[c]))
                .collect();
        }
        let (d, n) = max_diff(&got.interleaved, &want);
        let snr = snr_db(&want, &got.interleaved);
        println!(
            "opus {channels}ch frames ours={} oracle={} max_diff={} ({} LSB) snr={snr:.1} dB over {}",
            got.frames(),
            want.len() / usize::from(channels),
            d,
            d * 32768.0,
            n / usize::from(channels)
        );
        assert_eq!(got.frames(), frames);
        assert_eq!(want.len(), frames * usize::from(channels));
        assert!(snr >= 60.0, "snr {snr}");
        assert!(
            d <= 1.0 / 32768.0,
            "max diff {d} (measured 0.5 LSB: the oracle's s16 rounding)"
        );
    }
}

/// FLAC in Ogg (Ogg FLAC mapping) is bit-exact against libavcodec, and a mid-stream Ogg segment
/// through the registry is the whole-file decode at exactly those positions.
#[test]
fn ogg_flac_is_bit_exact_and_segments_are_positioned() {
    let got = decode(OGG_FLAC).unwrap();
    assert_eq!(
        (got.sample_rate, got.channels, got.decoder_id),
        (32_000, 1, FLAC_DECODER_ID)
    );
    assert_eq!(got.interleaved, reference(OGG_FLAC_REF));

    let whole = decode(VORBIS).unwrap();
    let tps = TICKS_PER_SECOND / 44_100;
    let (receipt, seg) = decode_segment(
        VORBIS,
        &request(OGG_PROVIDER_ID, 3000 * tps, 4000, VORBIS),
        &Limits::default(),
    )
    .unwrap();
    assert!(receipt.complete);
    assert_eq!(receipt.decoder_id, VORBIS_DECODER_ID);
    assert_eq!(seg.first_index, Some(3000));
    assert_eq!(seg.samples, whole.interleaved[6000..14_000]);
    let mut export = request(OGG_PROVIDER_ID, 3000 * tps + 5, 10, VORBIS);
    export.purpose = Purpose::Export;
    assert!(matches!(
        decode_segment(VORBIS, &export, &Limits::default()),
        Err(ReelError::NotExact { .. })
    ));
    let past = request(OGG_PROVIDER_ID, 11_000 * tps, 26, VORBIS);
    assert!(matches!(
        decode_segment(VORBIS, &past, &Limits::default()),
        Err(ReelError::OutOfRange { .. })
    ));
    let ids: Vec<&str> = audio_codec_descriptors()
        .iter()
        .map(|d| d.decoder_id)
        .collect();
    assert!(ids.contains(&VORBIS_DECODER_ID) && ids.contains(&OPUS_DECODER_ID));
    let _ = Ticks::new(0);
}

// ----- hostile Ogg -------------------------------------------------------------------------------------

fn ogg_crc(page: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in page {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// One Ogg page carrying `segments` lacing values over `body`.
fn page(flags: u8, granule: i64, serial: u32, seq: u32, lacing: &[u8], body: &[u8]) -> Vec<u8> {
    let mut p = b"OggS".to_vec();
    p.push(0);
    p.push(flags);
    p.extend_from_slice(&granule.to_le_bytes());
    p.extend_from_slice(&serial.to_le_bytes());
    p.extend_from_slice(&seq.to_le_bytes());
    p.extend_from_slice(&[0; 4]);
    p.push(lacing.len() as u8);
    p.extend_from_slice(lacing);
    p.extend_from_slice(body);
    let crc = ogg_crc(&p);
    p[22..26].copy_from_slice(&crc.to_le_bytes());
    p
}

fn opus_head(channels: u8, family: u8, table: &[u8]) -> Vec<u8> {
    let mut h = b"OpusHead".to_vec();
    h.extend_from_slice(&[1, channels, 0x38, 0x01, 0x80, 0xBB, 0, 0, 0, 0, family]);
    h.extend_from_slice(table);
    h
}

/// dims_u32_max and aggregate_over_budget for Ogg and its codec headers: a flipped page byte is a CRC
/// failure, a truncated file is Truncated, a packet laced across pages past the packet budget is refused
/// while it grows, absurd channel counts / rates / codebook sizes are refused before any decoder
/// allocates, and an unknown Ogg codec is MEDIA_CODEC_UNAVAILABLE naming it.
#[test]
fn ogg_hostile_input_is_refused_before_allocation() {
    let mut flipped = VORBIS.to_vec();
    let at = flipped.len() - 100;
    flipped[at] ^= 1;
    assert_eq!(
        decode(&flipped).unwrap_err(),
        ReelError::Corrupt {
            what: "ogg-page-crc"
        }
    );
    assert!(matches!(
        decode(&VORBIS[..VORBIS.len() - 50]),
        Err(ReelError::Truncated { .. } | ReelError::Corrupt { .. })
    ));

    // OpusHead declaring 255 channels (mapping family 255): refused before any decoder exists.
    let mut table = vec![255u8, 0];
    table.extend(std::iter::repeat_n(0u8, 255));
    let head = opus_head(255, 255, &table);
    let file = page(2, 0, 7, 0, &[255, (head.len() - 255) as u8], &head);
    assert_eq!(
        decode(&file).unwrap_err(),
        ReelError::LimitExceeded {
            what: "opus-channels"
        }
    );

    // Vorbis identification header with a 4 GHz sample rate, then a setup header whose one codebook
    // declares 2^24 - 1 entries in a few bytes (ordered lengths).
    let mut ident = vec![1u8];
    ident.extend_from_slice(b"vorbis");
    ident.extend_from_slice(&0u32.to_le_bytes());
    ident.push(2);
    ident.extend_from_slice(&u32::MAX.to_le_bytes());
    ident.extend_from_slice(&[0; 12]);
    ident.extend_from_slice(&[0xB8, 1]);
    let comment = [&[3u8][..], b"vorbis", &[0; 8], &[1]].concat();
    let setup = [
        &[5u8][..],
        b"vorbis",
        &[
            0x00, 0x42, 0x43, 0x56, 0x01, 0x00, 0xFF, 0xFF, 0xFF, 0x01, 0xFF,
        ],
    ]
    .concat();
    let headers = |ident: &[u8]| {
        let mut f = page(2, 0, 9, 0, &[ident.len() as u8], ident);
        f.extend(page(
            0,
            0,
            9,
            1,
            &[comment.len() as u8, setup.len() as u8],
            &[comment.as_slice(), &setup].concat(),
        ));
        f
    };
    assert_eq!(
        decode(&headers(&ident)).unwrap_err(),
        ReelError::LimitExceeded {
            what: "vorbis-sample-rate"
        }
    );
    ident[12..16].copy_from_slice(&44_100u32.to_le_bytes());
    assert_eq!(
        decode(&headers(&ident)).unwrap_err(),
        ReelError::LimitExceeded {
            what: "vorbis-setup-budget"
        }
    );

    // A packet laced over 19 full pages (255 x 255 bytes each, 1.2 MB) past the 1 MiB packet budget,
    // refused while it grows.
    assert_eq!(
        (OGG_FLAC[26], OGG_FLAC[27]),
        (1, 51),
        "fixture: first page holds the 51-byte header packet"
    );
    let mut bomb = page(2, 0, 5, 0, &[51], &OGG_FLAC[28..28 + 51]);
    let body = vec![0u8; 255 * 255];
    for seq in 1..20u32 {
        bomb.extend(page(
            if seq == 1 { 0 } else { 1 },
            -1,
            5,
            seq,
            &[255; 255],
            &body,
        ));
    }
    assert_eq!(
        decode(&bomb).unwrap_err(),
        ReelError::LimitExceeded {
            what: "ogg-packet-size"
        }
    );

    // An Ogg stream of a codec reel does not decode.
    let theora = page(2, 0, 3, 0, &[7], b"\x80theora");
    assert_eq!(
        decode(&theora).unwrap_err(),
        ReelError::CodecUnavailable {
            codec: "theora".into()
        }
    );
}
