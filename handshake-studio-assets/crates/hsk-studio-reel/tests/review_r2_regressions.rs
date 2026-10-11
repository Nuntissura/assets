//! One hostile-input regression per confirmed STUDIO-R2 reel finding: typed errors, never panics.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_reel::fixtures::*;
use hsk_studio_reel::*;
use std::io::{Cursor, Read, Seek, SeekFrom};

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost 30s deadline: {e}"),
    }
}

#[derive(Default)]
struct Collect {
    chunks: Vec<(i64, u64, u64, bool, Vec<u8>)>,
}

impl ChunkSink for Collect {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
        self.chunks
            .push((c.pts, c.index, c.trim_start, c.preroll, c.bytes.to_vec()));
        Ok(())
    }
}

/// A source that must never be touched.
struct Tripwire;
impl Read for Tripwire {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("proxy was read before the original's grant was verified");
    }
}
impl Seek for Tripwire {
    fn seek(&mut self, _: SeekFrom) -> std::io::Result<u64> {
        panic!("proxy was seeked before the original's grant was verified");
    }
}

fn grant(id: &str, bytes: &[u8]) -> Grant {
    Grant {
        content_id: ContentId::parse(id).unwrap(),
        revision: 1,
        expected_len: bytes.len() as u64,
    }
}

fn request(
    codec: &str,
    start: u64,
    count: u64,
    output: OutputMode,
    grant: Grant,
) -> SegmentRequest {
    SegmentRequest {
        codec: codec.into(),
        start_pts: Ticks::new(start),
        count,
        track: None,
        output,
        prefer: SourcePreference::Original,
        purpose: Purpose::Preview,
        grant,
        expected_revision: 1,
    }
}

fn run(
    provider: &dyn MediaProvider,
    bytes: &[u8],
    req: &SegmentRequest,
    limits: &Limits,
    sink: &mut dyn ChunkSink,
) -> Result<SegmentReceipt, ReelError> {
    provider.decode_segment(
        &mut Cursor::new(bytes.to_vec()),
        req,
        limits,
        &CancellationToken::default(),
        sink,
    )
}

fn index_of(bytes: &[u8], limits: &Limits) -> Result<MediaIndex, ReelError> {
    build_index(
        &mut Cursor::new(bytes.to_vec()),
        bytes.len() as u64,
        limits,
        &CancellationToken::default(),
    )
}

const TPS_48K: u64 = 5_292_000;

fn video_spec(samples: usize, duration: u32, timescale: u32) -> Mp4Spec {
    Mp4Spec {
        movie_timescale: timescale,
        media_timescale: timescale,
        codec: *b"avc1",
        width: 64,
        height: 36,
        samples: (0..samples)
            .map(|_| Mp4Sample {
                duration,
                cts_offset: 0,
                size: 20,
                sync: true,
            })
            .collect(),
        edits: None,
        samples_per_chunk: 4,
        co64: false,
        ctts_version: 0,
        moov_first: true,
        colr_nclx: None,
        pasp: None,
    }
}

fn pcm_spec(frames: u64) -> PcmMovSpec {
    PcmMovSpec {
        fourcc: *b"twos",
        kind: PcmKind::S16,
        big_endian: true,
        lpcm_v2: false,
        non_interleaved: false,
        channels: 2,
        rate: 48000,
        frames,
        frames_per_chunk: 1000,
        movie_timescale: 48000,
        edits: None,
        moov_first: true,
        sound_version: None,
    }
}

#[test]
fn r2_1_wav_request_past_truncation_point_is_typed() {
    bounded_case("r2_1_wav_request_past_truncation_point_is_typed", || {
        // Header declares 1000 frames; the file is cut after 300.
        let wav = wav_pcm16(48000, 2, 1000);
        let cut = &wav[..44 + 300 * 4];
        let limits = Limits::default();
        for (start, count, delivered) in [(320u64, 10u64, 0u64), (300, 1, 0), (295, 10, 5)] {
            let req = request(
                WAV_PROVIDER_ID,
                start * TPS_48K,
                count,
                OutputMode::Decoded,
                grant("w", cut),
            );
            let mut sink = Collect::default();
            assert_eq!(
                run(&WavProvider, cut, &req, &limits, &mut sink).unwrap_err(),
                ReelError::Truncated { delivered },
                "start {start}"
            );
            assert_eq!(
                sink.chunks
                    .iter()
                    .map(|c| c.4.len() as u64 / 4)
                    .sum::<u64>(),
                delivered
            );
        }
    });
}

#[test]
fn r2_2_y4m_non_ascii_header_token_is_corrupt() {
    bounded_case("r2_2_y4m_non_ascii_header_token_is_corrupt", || {
        let limits = Limits::default();
        for header in [
            "YUV4MPEG2 W64 H36 F25:1 \u{e9}x\n",
            "YUV4MPEG2 \u{1F3AC} W64 H36 F25:1\n",
            "YUV4MPEG2 W64 H36 F25:1 \u{e9}\n",
        ] {
            let bytes = header.as_bytes().to_vec();
            let req = request(
                Y4M_PROVIDER_ID,
                0,
                1,
                OutputMode::Decoded,
                grant("y", &bytes),
            );
            assert_eq!(
                run(&Y4mProvider, &bytes, &req, &limits, &mut Collect::default()).unwrap_err(),
                ReelError::Corrupt {
                    what: "y4m-header-tag"
                },
                "{header:?}"
            );
            let probed = Y4mProvider.probe(
                &mut Cursor::new(bytes),
                &limits,
                &CancellationToken::default(),
            );
            assert_eq!(probed.unwrap_err().code(), "MEDIA_CORRUPT");
        }
    });
}

#[test]
fn r2_3_largesize_moov_cannot_overflow() {
    bounded_case("r2_3_largesize_moov_cannot_overflow", || {
        let limits = Limits::default();
        let mut spec = video_spec(6, 1001, 30000);
        spec.moov_first = false; // moov is the last box: rewriting its header moves no sample offsets
        let plain = mp4_bytes(&spec);
        let reference = index_of(&plain, &limits).unwrap();
        let start = plain.windows(4).rposition(|w| w == b"moov").unwrap() - 4;
        let true_size = u32::from_be_bytes(plain[start..start + 4].try_into().unwrap());
        let rebuild = |size: u64| {
            let mut out = plain[..start].to_vec();
            out.extend_from_slice(&1u32.to_be_bytes());
            out.extend_from_slice(b"moov");
            out.extend_from_slice(&size.to_be_bytes());
            out.extend_from_slice(&plain[start + 8..]);
            out
        };
        // A 64-bit largesize header (16 bytes) that is honest indexes the same samples.
        let wide = index_of(&rebuild(u64::from(true_size) + 8), &limits).unwrap();
        assert_eq!(wide.tracks[0].presented(), reference.tracks[0].presented());
        // Sizes that overflow `pos + size` or run past the file are Corrupt, not a panic or a wrap.
        for size in [
            u64::MAX,
            u64::MAX - 7,
            u64::MAX - start as u64 + 1,
            u64::from(true_size) + 9,
        ] {
            assert_eq!(
                index_of(&rebuild(size), &limits).unwrap_err(),
                ReelError::Corrupt {
                    what: "moov-exceeds-file"
                },
                "size {size:#x}"
            );
        }
    });
}

#[test]
fn r2_4_edit_starting_inside_a_sample_keeps_that_sample() {
    bounded_case(
        "r2_4_edit_starting_inside_a_sample_keeps_that_sample",
        || {
            // AAC-like: 1024-unit samples at 48 kHz, edit list media_time 2112 = 64 units into sample 2.
            let limits = Limits::default();
            let mut spec = video_spec(10, 1024, 48000);
            spec.edits = Some(vec![(5000, 2112)]);
            let bytes = mp4_bytes(&spec);
            let index = index_of(&bytes, &limits).unwrap();
            let track = &index.tracks[0];
            let p = track.presented();
            assert_eq!(p.len(), 5);
            assert_eq!(
                (p[0].sample, p[0].pts, p[0].trim_start),
                (2, 0, 64 * TPS_48K)
            );
            assert_eq!(p[0].duration, 960 * TPS_48K);
            assert_eq!(
                (p[1].sample, p[1].pts, p[1].trim_start),
                (3, 960 * TPS_48K as i64, 0)
            );
            let seek = track.seek(Ticks::new(0)).unwrap();
            assert_eq!((seek.target.sample, seek.trim_start), (2, 64 * TPS_48K));
            assert_eq!(
                seek.disposition,
                SeekDisposition::Containing,
                "requested is not the sample's own start"
            );
            // Packets start at the containing sample; the lead-in is reported, so the receipt is Approximate.
            let req = request(
                ISOBMFF_PROVIDER_ID,
                0,
                2,
                OutputMode::Packets,
                grant("aac", &bytes),
            );
            let mut sink = Collect::default();
            let receipt = run(&IsobmffProvider, &bytes, &req, &limits, &mut sink).unwrap();
            assert_eq!(sink.chunks.iter().map(|c| c.1).collect::<Vec<_>>(), [2, 3]);
            assert_eq!(
                sink.chunks.iter().map(|c| c.2).collect::<Vec<_>>(),
                [64 * TPS_48K, 0]
            );
            assert_eq!(receipt.disposition, Disposition::Approximate);
            assert_eq!(receipt.requested.start_pts, receipt.actual.start_pts);
            // An aligned edit start is unchanged: no trim, exact.
            spec.edits = Some(vec![(5000, 2048)]);
            let bytes = mp4_bytes(&spec);
            let aligned = index_of(&bytes, &limits).unwrap();
            let p = aligned.tracks[0].presented();
            assert_eq!((p[0].sample, p[0].pts, p[0].trim_start), (2, 0, 0));
            let req = request(
                ISOBMFF_PROVIDER_ID,
                0,
                1,
                OutputMode::Packets,
                grant("aac", &bytes),
            );
            let receipt = run(
                &IsobmffProvider,
                &bytes,
                &req,
                &limits,
                &mut Collect::default(),
            )
            .unwrap();
            assert_eq!(receipt.disposition, Disposition::Exact);
        },
    );
}

#[test]
fn r2_5_long_pcm_does_not_starve_the_other_tracks() {
    bounded_case("r2_5_long_pcm_does_not_starve_the_other_tracks", || {
        // 5000 PCM frames against a 60-entry budget: the PCM track is chunk runs, not 5000 entries.
        let limits = Limits {
            max_samples: 60,
            ..Limits::default()
        };
        let bytes = mov_av_bytes(12, &pcm_spec(5000));
        let index = index_of(&bytes, &limits).unwrap();
        assert_eq!(index.tracks.len(), 2);
        assert_eq!(index.tracks[0].samples().len(), 12);
        assert_eq!(index.tracks[1].pcm().unwrap().chunk_runs(), 5);
        let video = index.tracks[0]
            .seek(Ticks::new(5 * 10_160_640_000))
            .unwrap();
        assert_eq!(video.target.sample, 5);
        let mut req = request(
            ISOBMFF_PROVIDER_ID,
            100 * TPS_48K,
            10,
            OutputMode::Decoded,
            grant("av", &bytes),
        );
        req.track = Some(2);
        let mut sink = Collect::default();
        run(&IsobmffProvider, &bytes, &req, &limits, &mut sink).unwrap();
        let expected: Vec<u8> = (100..110u64)
            .flat_map(|f| {
                (0..2u16).flat_map(move |c| pcm_encode(PcmKind::S16, false, pcm_sample_value(f, c)))
            })
            .collect();
        assert_eq!(
            sink.chunks
                .iter()
                .flat_map(|c| c.4.clone())
                .collect::<Vec<_>>(),
            expected
        );
        // Chunk runs spend the shared budget: 12 video samples + 5 runs do not fit in 14.
        let tight = Limits {
            max_samples: 14,
            ..limits
        };
        assert_eq!(
            index_of(&bytes, &tight).unwrap_err(),
            ReelError::LimitExceeded {
                what: "chunk-count"
            }
        );
    });
}

#[test]
fn r2_6_unknown_quicktime_sound_version_is_typed() {
    bounded_case("r2_6_unknown_quicktime_sound_version_is_typed", || {
        let mut spec = pcm_spec(40);
        spec.sound_version = Some(3);
        let bytes = mov_pcm_bytes(&spec);
        assert_eq!(
            index_of(&bytes, &Limits::default()).unwrap_err(),
            ReelError::Unsupported {
                what: "quicktime-sound-version"
            }
        );
        // Versions 0 and 2 are implemented and indexed.
        for v2 in [false, true] {
            let mut ok = pcm_spec(40);
            (ok.lpcm_v2, ok.fourcc) = (v2, if v2 { *b"lpcm" } else { *b"twos" });
            let index = index_of(&mov_pcm_bytes(&ok), &Limits::default()).unwrap();
            assert_eq!(
                index.tracks[0].audio.unwrap().version,
                if v2 { 2 } else { 0 }
            );
        }
    });
}

#[test]
fn r2_7_index_and_proxy_are_bound_to_the_granted_source() {
    bounded_case(
        "r2_7_index_and_proxy_are_bound_to_the_granted_source",
        || {
            let limits = Limits::default();
            let (a, b) = (
                mp4_bytes(&video_spec(6, 1001, 30000)),
                mp4_bytes(&video_spec(7, 1001, 30000)),
            );
            let index_of_a = index_of(&a, &limits).unwrap();
            assert_eq!(index_of_a.source_len, a.len() as u64);
            // An index replayed against a different file (grant matches the live file, not the index).
            let req = request(
                ISOBMFF_PROVIDER_ID,
                0,
                1,
                OutputMode::Packets,
                grant("b", &b),
            );
            let err = IsobmffProvider
                .decode_packets(
                    &mut Cursor::new(b.clone()),
                    &index_of_a,
                    &req,
                    &limits,
                    &CancellationToken::default(),
                    &mut Collect::default(),
                )
                .unwrap_err();
            assert_eq!(err, ReelError::StaleGrant);
            // A stale original grant fails before anything is probed or read from the proxy.
            let original = y4m_bytes(64, 36, (25, 1), "420", 4);
            let mut stale = grant("orig", &original);
            stale.expected_len += 1;
            let mut req = request(Y4M_PROVIDER_ID, 0, 1, OutputMode::Decoded, stale);
            req.prefer = SourcePreference::Proxy;
            let mut tripwire = Tripwire;
            let proxy = ProxySource::new(&mut tripwire, grant("proxy", &original), 1);
            let result = Registry::standard().decode_segment(
                &mut Cursor::new(original),
                Some(proxy),
                &req,
                &limits,
                &CancellationToken::default(),
                &mut Collect::default(),
            );
            assert_eq!(result.unwrap_err(), ReelError::StaleGrant);
        },
    );
}
