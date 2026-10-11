use hsk_studio_accord::{ActorContext, CancellationToken, DomainId, TICKS_PER_SECOND, Ticks};
use hsk_studio_observe::{
    Budget, DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort,
};
use hsk_studio_reel::fixtures::*;
use hsk_studio_reel::*;
use std::io::Cursor;

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

struct Got {
    pts: i64,
    index: u64,
    units: u64,
    preroll: bool,
    sync: bool,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct Collect {
    chunks: Vec<Got>,
    max_len: usize,
}

impl ChunkSink for Collect {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
        self.max_len = self.max_len.max(c.bytes.len());
        self.chunks.push(Got {
            pts: c.pts,
            index: c.index,
            units: c.units,
            preroll: c.preroll,
            sync: c.sync,
            bytes: c.bytes.to_vec(),
        });
        Ok(())
    }
}

struct NeverReached;
impl ChunkSink for NeverReached {
    fn accept(&mut self, _: Chunk<'_>) -> Result<(), ReelError> {
        panic!("sink reached although the request must fail first");
    }
}

fn grant(id: &str, bytes: &[u8]) -> Grant {
    Grant {
        content_id: ContentId::parse(id).unwrap(),
        revision: 1,
        expected_len: bytes.len() as u64,
    }
}

fn request(codec: &str, start: u64, count: u64, grant: Grant) -> SegmentRequest {
    SegmentRequest {
        codec: codec.into(),
        start_pts: Ticks::new(start),
        count,
        track: None,
        output: OutputMode::Decoded,
        prefer: SourcePreference::Original,
        purpose: Purpose::Preview,
        grant,
        expected_revision: 1,
    }
}

fn decode(
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

const TPF_2997: u64 = 8_475_667_200;
/// Presentation index of each decode-order sample (open-GOP: samples 8, 9 lead sync sample 7).
const PRESENT: [i64; 12] = [0, 3, 1, 2, 6, 4, 5, 9, 7, 8, 11, 10];
const DECODE_FROM: [u32; 12] = [0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 7, 7];

fn gop_spec() -> Mp4Spec {
    Mp4Spec {
        movie_timescale: 30000,
        media_timescale: 30000,
        codec: *b"avc1",
        width: 64,
        height: 36,
        samples: PRESENT
            .iter()
            .enumerate()
            .map(|(k, p)| Mp4Sample {
                duration: 1001,
                cts_offset: ((p - k as i64) * 1001 + 2002) as i32,
                size: 2000,
                sync: k == 0 || k == 7,
            })
            .collect(),
        edits: Some(vec![(12 * 1001, 2002)]),
        samples_per_chunk: 5,
        co64: false,
        ctts_version: 0,
        moov_first: true,
    }
}

#[test]
fn isobmff_index_seek_and_packets() {
    bounded_case("isobmff_index_seek_and_packets", || {
        let bytes = mp4_bytes(&gop_spec());
        let limits = Limits::default();
        let mut src = Cursor::new(bytes.clone());
        let index = build_index(
            &mut src,
            bytes.len() as u64,
            &limits,
            &CancellationToken::default(),
        )
        .unwrap();
        let track = index.default_track().unwrap();
        assert_eq!((track.codec, track.width, track.height), (*b"avc1", 64, 36));
        assert!(track.ticks_exact && !track.is_all_sync());
        assert_eq!(track.sync_samples(), &[0, 7]);

        // Exact PTS for every frame: edit list shifts the 2-frame B-frame delay to presentation zero.
        let mut by_pts: Vec<(i64, u32)> = track
            .presented()
            .iter()
            .map(|p| (p.pts, p.sample))
            .collect();
        by_pts.sort();
        for (j, (pts, sample)) in by_pts.iter().enumerate() {
            assert_eq!(*pts, j as i64 * TPF_2997 as i64);
            assert_eq!(PRESENT[*sample as usize], j as i64);
        }
        for (k, expected_from) in DECODE_FROM.iter().enumerate() {
            let r = track
                .seek(Ticks::new(PRESENT[k] as u64 * TPF_2997))
                .unwrap();
            assert_eq!(r.target.sample, k as u32);
            assert_eq!(r.disposition, SeekDisposition::Exact);
            assert_eq!(
                r.decode_from.sample, *expected_from,
                "decode_from for sample {k}"
            );
            assert_eq!(r.samples_to_decode, k as u32 - expected_from + 1);
            assert_eq!(r.target.pts, PRESENT[k] * TPF_2997 as i64);
        }
        // Mid-frame request: containing frame, actual PTS differs from requested.
        let mid = Ticks::new(2 * TPF_2997 + TPF_2997 / 2);
        let r = track.seek(mid).unwrap();
        assert_eq!(r.disposition, SeekDisposition::Containing);
        assert_eq!((r.target.pts, r.requested), (2 * TPF_2997 as i64, mid));
        // Last tick of the last frame is in range, the end instant is not (no silent clamp).
        track.seek(Ticks::new(12 * TPF_2997 - 1)).unwrap();
        assert_eq!(
            track.seek(Ticks::new(12 * TPF_2997)).unwrap_err().code(),
            "MEDIA_OUT_OF_RANGE"
        );

        // Packet delivery for presentation frames 4..7 in decode order, with pre-roll from the sync sample.
        let g = grant("orig-mp4", &bytes);
        let mut req = request(ISOBMFF_PROVIDER_ID, 4 * TPF_2997, 3, g.clone());
        req.output = OutputMode::Packets;
        let mut sink = Collect::default();
        let receipt = decode(&IsobmffProvider, &bytes, &req, &limits, &mut sink).unwrap();
        let decode_order: Vec<u64> = sink.chunks.iter().map(|c| c.index).collect();
        assert_eq!(decode_order, (0..=6).collect::<Vec<_>>());
        let preroll: Vec<bool> = sink.chunks.iter().map(|c| c.preroll).collect();
        assert_eq!(preroll, [true, true, true, true, false, false, false]);
        for c in &sink.chunks {
            let k = c.index as u32;
            assert_eq!(c.pts, PRESENT[k as usize] * TPF_2997 as i64);
            assert_eq!(c.units, 1);
            assert_eq!(c.sync, k == 0 || k == 7);
            assert_eq!(c.bytes.len(), 2000);
            assert!(
                c.bytes
                    .iter()
                    .enumerate()
                    .all(|(j, b)| *b == mp4_sample_byte(k, j as u32))
            );
        }
        assert_eq!(receipt.disposition, Disposition::Exact);
        assert_eq!(receipt.requested.start_pts, Ticks::new(4 * TPF_2997));
        assert_eq!(receipt.actual.start_pts, Ticks::new(4 * TPF_2997));
        assert_eq!(receipt.actual.end_pts, Ticks::new(7 * TPF_2997));
        assert_eq!((receipt.actual.units, receipt.chunks), (3, 7));
        assert!(receipt.complete && receipt.ticks_exact);
        assert_eq!(receipt.codec, "avc1");
        assert_eq!(receipt.decoder_id, "isobmff-demux");
        assert!(
            receipt.bytes_read < bytes.len() as u64,
            "mdat must not be loaded whole"
        );

        // Past the presentable range is a typed error, never a short success.
        let late = request(ISOBMFF_PROVIDER_ID, 10 * TPF_2997, 3, g.clone());
        let late = SegmentRequest {
            output: OutputMode::Packets,
            ..late
        };
        assert_eq!(
            decode(&IsobmffProvider, &bytes, &late, &limits, &mut NeverReached)
                .unwrap_err()
                .code(),
            "MEDIA_OUT_OF_RANGE"
        );
        // Typed unsupported-codec loss: no decoder for the `avc1` sample entry.
        let dec = request(ISOBMFF_PROVIDER_ID, 0, 1, g);
        let err = decode(&IsobmffProvider, &bytes, &dec, &limits, &mut NeverReached).unwrap_err();
        assert_eq!(
            err,
            ReelError::CodecUnavailable {
                codec: "avc1".into()
            }
        );
        assert_eq!(err.code(), "MEDIA_CODEC_UNAVAILABLE");
    });
}

#[test]
fn isobmff_layouts_corruption_and_limits() {
    bounded_case("isobmff_layouts_corruption_and_limits", || {
        let limits = Limits::default();
        let cancel = CancellationToken::default();
        let index_of = |bytes: &[u8], limits: &Limits| {
            build_index(
                &mut Cursor::new(bytes.to_vec()),
                bytes.len() as u64,
                limits,
                &cancel,
            )
        };
        let reference = index_of(&mp4_bytes(&gop_spec()), &limits).unwrap();
        // moov after mdat + 64-bit offsets + one sample per chunk give the same presentation.
        let mut other = gop_spec();
        (other.moov_first, other.co64, other.samples_per_chunk) = (false, true, 1);
        let other_bytes = mp4_bytes(&other);
        let moved = index_of(&other_bytes, &limits).unwrap();
        assert_eq!(moved.tracks[0].presented(), reference.tracks[0].presented());
        let s = moved.tracks[0].samples()[9];
        let mut buf = Vec::new();
        read_sample(
            &mut Cursor::new(other_bytes.clone()),
            &s,
            &mut buf,
            &limits,
            0,
        )
        .unwrap();
        assert!(
            buf.iter()
                .enumerate()
                .all(|(j, b)| *b == mp4_sample_byte(9, j as u32))
        );

        // Cut inside moov / cut mdat with moov at the end / fragmented / sample budget.
        let first = mp4_bytes(&gop_spec());
        let ftyp_len = u32::from_be_bytes(first[0..4].try_into().unwrap()) as usize;
        for cut in [
            &first[..ftyp_len + 100],
            &other_bytes[..other_bytes.len() / 2],
        ] {
            assert_eq!(index_of(cut, &limits).unwrap_err().code(), "MEDIA_CORRUPT");
        }
        let mut fragmented = first[..ftyp_len].to_vec();
        fragmented.extend_from_slice(&[0, 0, 0, 8, b'm', b'o', b'o', b'f']);
        assert_eq!(
            index_of(&fragmented, &limits).unwrap_err(),
            ReelError::Unsupported {
                what: "fragmented-isobmff"
            }
        );
        let tight = Limits {
            max_samples: 4,
            ..limits
        };
        assert_eq!(
            index_of(&first, &tight).unwrap_err(),
            ReelError::LimitExceeded {
                what: "sample-count"
            }
        );
        let tiny_probe = Limits {
            max_probe_bytes: 64,
            ..limits
        };
        assert_eq!(
            index_of(&first, &tiny_probe).unwrap_err(),
            ReelError::LimitExceeded { what: "moov-bytes" }
        );

        // A stale grant is rejected before any payload is read.
        let mut stale = grant("orig-mp4", &first);
        stale.expected_len += 1;
        let mut req = request(ISOBMFF_PROVIDER_ID, 0, 1, stale);
        req.output = OutputMode::Packets;
        assert_eq!(
            decode(&IsobmffProvider, &first, &req, &limits, &mut NeverReached).unwrap_err(),
            ReelError::StaleGrant
        );
        let mut moved_revision = request(ISOBMFF_PROVIDER_ID, 0, 1, grant("orig-mp4", &first));
        (moved_revision.output, moved_revision.expected_revision) = (OutputMode::Packets, 2);
        assert_eq!(
            decode(
                &IsobmffProvider,
                &first,
                &moved_revision,
                &limits,
                &mut NeverReached
            )
            .unwrap_err()
            .code(),
            "MEDIA_STALE_GRANT"
        );
    });
}

#[test]
fn wav_and_y4m_exact_truncated_and_bounded() {
    bounded_case("wav_and_y4m_exact_truncated_and_bounded", || {
        // --- WAV: 48 kHz stereo s16, 1000 frames, 256-byte chunks (64 frames each).
        let tps = TICKS_PER_SECOND / 48000;
        assert_eq!(tps, 5_292_000);
        let wav = wav_pcm16(48000, 2, 1000);
        let small = Limits {
            max_chunk_bytes: 256,
            ..Limits::default()
        };
        let g = grant("wav-1", &wav);
        let mut sink = Collect::default();
        let r = decode(
            &WavProvider,
            &wav,
            &request(WAV_PROVIDER_ID, 100 * tps, 300, g.clone()),
            &small,
            &mut sink,
        )
        .unwrap();
        assert_eq!(
            (r.chunks, r.disposition, r.actual.units),
            (5, Disposition::Exact, 300)
        );
        assert!(sink.max_len <= 256);
        let joined: Vec<u8> = sink.chunks.iter().flat_map(|c| c.bytes.clone()).collect();
        let expected: Vec<u8> = (100..400u64)
            .flat_map(|f| (0..2u16).flat_map(move |c| pcm16_sample(f, c).to_le_bytes()))
            .collect();
        assert_eq!(joined, expected);
        assert_eq!(sink.chunks[0].pts, 100 * tps as i64);
        assert_eq!(sink.chunks[1].pts, 164 * tps as i64);
        assert_eq!(
            (r.actual.start_pts, r.actual.end_pts),
            (Ticks::new(100 * tps), Ticks::new(400 * tps))
        );
        assert_eq!(r.bytes_read, 44 + 300 * 4);
        let off = request(WAV_PROVIDER_ID, 100 * tps + 1, 10, g.clone());
        let r = decode(&WavProvider, &wav, &off, &small, &mut Collect::default()).unwrap();
        assert_eq!(
            (r.disposition, r.actual.start_pts),
            (Disposition::Approximate, Ticks::new(100 * tps))
        );
        assert_eq!(
            decode(
                &WavProvider,
                &wav,
                &request(WAV_PROVIDER_ID, 900 * tps, 200, g),
                &small,
                &mut NeverReached
            )
            .unwrap_err()
            .code(),
            "MEDIA_OUT_OF_RANGE"
        );
        // Header declares 1000 frames but only 300 are present: delivered 50 then Truncated.
        let cut = &wav[..44 + 300 * 4];
        let mut sink = Collect::default();
        let err = decode(
            &WavProvider,
            cut,
            &request(WAV_PROVIDER_ID, 250 * tps, 100, grant("wav-cut", cut)),
            &small,
            &mut sink,
        )
        .unwrap_err();
        assert_eq!(err, ReelError::Truncated { delivered: 50 });
        assert_eq!(sink.chunks.iter().map(|c| c.units).sum::<u64>(), 50);
        let adpcm = wav_with_tag(2, 48000, 2, 4, &[0; 64]);
        assert_eq!(
            decode(
                &WavProvider,
                &adpcm,
                &request(WAV_PROVIDER_ID, 0, 1, grant("adpcm", &adpcm)),
                &small,
                &mut NeverReached
            )
            .unwrap_err(),
            ReelError::CodecUnavailable {
                codec: "wav-tag-0x0002".into()
            }
        );

        // --- Y4M: 10 frames 64x36 4:2:0 @ 25 fps.
        let tpf = 10_160_640_000u64;
        let y4m = y4m_bytes(64, 36, (25, 1), "420", 10);
        let frame = y4m_frame_bytes(64, 36, "420");
        let header = y4m.iter().position(|b| *b == b'\n').unwrap() + 1;
        let limits = Limits::default();
        let mut sink = Collect::default();
        let r = decode(
            &Y4mProvider,
            &y4m,
            &request(Y4M_PROVIDER_ID, 3 * tpf, 3, grant("y4m-1", &y4m)),
            &limits,
            &mut sink,
        )
        .unwrap();
        assert_eq!(sink.chunks.len(), 3);
        for (i, c) in sink.chunks.iter().enumerate() {
            assert_eq!(c.pts, (3 + i as u64) as i64 * tpf as i64);
            assert_eq!(c.bytes, y4m_frame(3 + i as u64, 64, 36, "420"));
        }
        assert_eq!(sink.max_len, frame, "peak chunk is exactly one frame");
        assert_eq!(r.disposition, Disposition::Exact);
        assert_eq!(r.bytes_read, (header + 3 * (frame + 6)) as u64);
        assert!(r.bytes_read < y4m.len() as u64 / 2);
        // One whole frame missing: frames 5..8 delivered intact, then Truncated, never Ok.
        let short = &y4m[..y4m.len() - (frame + 6)];
        let mut sink = Collect::default();
        let err = decode(
            &Y4mProvider,
            short,
            &request(Y4M_PROVIDER_ID, 5 * tpf, 5, grant("y4m-s", short)),
            &limits,
            &mut sink,
        )
        .unwrap_err();
        assert_eq!(err, ReelError::Truncated { delivered: 4 });
        assert_eq!(sink.chunks.len(), 4);
        // Corrupt marker, high bit depth, oversize frame are typed before any frame buffer is trusted.
        let mut bad = y4m.clone();
        bad[header + 2 * (frame + 6)] = b'X';
        assert_eq!(
            decode(
                &Y4mProvider,
                &bad,
                &request(Y4M_PROVIDER_ID, 0, 4, grant("y4m-bad", &bad)),
                &limits,
                &mut Collect::default()
            )
            .unwrap_err()
            .code(),
            "MEDIA_CORRUPT"
        );
        let deep = b"YUV4MPEG2 W64 H36 F25:1 Ip C420p10\n".to_vec();
        assert_eq!(
            decode(
                &Y4mProvider,
                &deep,
                &request(Y4M_PROVIDER_ID, 0, 1, grant("deep", &deep)),
                &limits,
                &mut NeverReached
            )
            .unwrap_err()
            .code(),
            "MEDIA_UNSUPPORTED"
        );
        let huge = b"YUV4MPEG2 W65536 H65536 F25:1 Ip C444\n".to_vec();
        assert_eq!(
            decode(
                &Y4mProvider,
                &huge,
                &request(Y4M_PROVIDER_ID, 0, 1, grant("huge", &huge)),
                &limits,
                &mut NeverReached
            )
            .unwrap_err(),
            ReelError::LimitExceeded {
                what: "frame-bytes"
            }
        );
    });
}

struct Frames(Vec<(DeliveryClass, Vec<u8>)>);
impl SinkPort for Frames {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push((class, bytes.to_vec()));
        Ok(())
    }
}

fn terminal(result: &Result<SegmentReceipt, ReelError>) -> (Outcome, usize) {
    let actor = ActorContext::new(
        "account",
        "principal",
        "owner-account",
        "owner-principal",
        "space",
        "session",
    )
    .unwrap();
    let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000003").unwrap();
    let mut observe = Observe::new(1, 1, resource, actor, Budget::new(0, 0).unwrap());
    let mut sink = Frames(Vec::new());
    let receipt = report(
        1,
        1,
        result,
        &CancellationToken::default(),
        &mut observe,
        &mut sink,
    )
    .unwrap();
    assert!(
        sink.0
            .iter()
            .all(|(class, _)| *class == DeliveryClass::Terminal)
    );
    (receipt.outcome, sink.0.len())
}

#[test]
fn registry_is_enumerable_instance_owned_and_reports() {
    bounded_case("registry_is_enumerable_instance_owned_and_reports", || {
        let registry = Registry::standard();
        let ids: Vec<&str> = registry.descriptors().iter().map(|d| d.id).collect();
        assert_eq!(ids, ["wav-pcm", "y4m", "isobmff"]);
        assert!(registry.descriptors_json().contains("\"id\":\"y4m\""));
        // Two registries share nothing; registering twice in one is rejected.
        let mut lone = Registry::new();
        lone.register(Box::new(WavProvider)).unwrap();
        assert!(lone.register(Box::new(WavProvider)).is_err());
        assert_eq!(lone.descriptors().len(), 1);
        assert_eq!(registry.descriptors().len(), 3);

        let missing = registry
            .resolve_id("h264")
            .err()
            .expect("no h264 adapter is registered");
        assert_eq!(
            missing,
            ReelError::CodecUnavailable {
                codec: "h264".into()
            }
        );
        assert_eq!(missing.code(), "MEDIA_CODEC_UNAVAILABLE");

        // Magic-number detection; unknown containers name the detected type.
        let wav = wav_pcm16(48000, 1, 4);
        let y4m = y4m_bytes(16, 16, (25, 1), "mono", 1);
        let mp4 = mp4_bytes(&gop_spec());
        for (bytes, id) in [(&wav, "wav-pcm"), (&y4m, "y4m"), (&mp4, "isobmff")] {
            let found = registry.detect(&mut Cursor::new(bytes.clone())).unwrap();
            assert_eq!(found.descriptor().id, id);
        }
        let mkv = vec![0x1A, 0x45, 0xDF, 0xA3, 0, 0, 0, 0];
        let err = registry.detect(&mut Cursor::new(mkv)).err().unwrap();
        assert_eq!(
            err,
            ReelError::ImportUnsupported {
                detected: "matroska-webm".into()
            }
        );
        assert_eq!(err.code(), "MEDIA_IMPORT_UNSUPPORTED");

        // One terminal frame per outcome class.
        let g = grant("y4m-1", &y4m);
        let ok = registry.decode_segment(
            &mut Cursor::new(y4m.clone()),
            None,
            &request("y4m", 0, 1, g.clone()),
            &Limits::default(),
            &CancellationToken::default(),
            &mut Collect::default(),
        );
        assert_eq!(terminal(&ok), (Outcome::Success, 1));
        let truncated = registry.decode_segment(
            &mut Cursor::new(y4m.clone()),
            None,
            &request("y4m", 0, 3, g),
            &Limits::default(),
            &CancellationToken::default(),
            &mut Collect::default(),
        );
        assert_eq!(truncated.as_ref().unwrap_err().code(), "MEDIA_TRUNCATED");
        assert_eq!(
            terminal(&truncated),
            (Outcome::Failure(FailureCode::Loss), 1)
        );
        let unavailable: Result<SegmentReceipt, ReelError> = Err(ReelError::CodecUnavailable {
            codec: "hevc".into(),
        });
        assert_eq!(
            terminal(&unavailable),
            (Outcome::Failure(FailureCode::Unsupported), 1)
        );
        assert_eq!(
            terminal(&Err(ReelError::StaleGrant)),
            (Outcome::Failure(FailureCode::Validation), 1)
        );
        // Cancelled before start: no provider work, Canceled outcome.
        let cancel = CancellationToken::default();
        cancel.cancel();
        let canceled = registry.decode_segment(
            &mut Cursor::new(y4m.clone()),
            None,
            &request("y4m", 0, 1, grant("y4m-1", &y4m)),
            &Limits::default(),
            &cancel,
            &mut NeverReached,
        );
        assert_eq!(canceled.as_ref().unwrap_err(), &ReelError::Canceled);
        assert_eq!(terminal(&canceled).0, Outcome::Canceled);
    });
}

struct Pcm {
    calls: usize,
    chunk: u64,
    cancel_on_call: Option<(usize, CancellationToken)>,
}
impl ChunkSource for Pcm {
    fn next_chunk(&mut self, buf: &mut Vec<u8>) -> Result<Option<u64>, ReelError> {
        if let Some((n, token)) = &self.cancel_on_call
            && self.calls == *n
        {
            token.cancel();
        }
        let first = self.calls as u64 * self.chunk;
        if first >= 400 {
            return Ok(None);
        }
        self.calls += 1;
        for f in first..first + self.chunk {
            for c in 0..2u16 {
                buf.extend_from_slice(&pcm16_sample(f, c).to_le_bytes());
            }
        }
        Ok(Some(self.chunk))
    }
}

struct Frames10 {
    next: u64,
    total: u64,
}
impl ChunkSource for Frames10 {
    fn next_chunk(&mut self, buf: &mut Vec<u8>) -> Result<Option<u64>, ReelError> {
        if self.next == self.total {
            return Ok(None);
        }
        buf.extend_from_slice(&y4m_frame(self.next, 64, 36, "420"));
        self.next += 1;
        Ok(Some(1))
    }
}

#[test]
fn encode_roundtrip_cancel_and_proxy_policy() {
    bounded_case("encode_roundtrip_cancel_and_proxy_policy", || {
        let limits = Limits::default();
        // WAV encode: exact bytes; a cancel mid-encode is a non-published partial (complete == false).
        let spec = EncodeSpec {
            codec: "wav-pcm".into(),
            params: EncodeParams::Audio {
                rate: 48000,
                channels: 2,
                bits: 16,
                float: false,
            },
            total_units: 400,
        };
        let mut out = Vec::new();
        let done = WavProvider
            .encode_segment(
                &spec,
                &mut Pcm {
                    calls: 0,
                    chunk: 100,
                    cancel_on_call: None,
                },
                &mut out,
                &limits,
                &CancellationToken::default(),
            )
            .unwrap();
        assert!(done.complete && !done.canceled);
        assert_eq!((done.units_written, done.bytes_written), (400, 44 + 1600));
        assert_eq!(out, wav_pcm16(48000, 2, 400));
        let token = CancellationToken::default();
        let mut partial = Vec::new();
        let cut = WavProvider
            .encode_segment(
                &spec,
                &mut Pcm {
                    calls: 0,
                    chunk: 100,
                    cancel_on_call: Some((2, token.clone())),
                },
                &mut partial,
                &limits,
                &token,
            )
            .unwrap();
        assert!(!cut.complete && cut.canceled);
        assert_eq!((cut.units_written, cut.units_declared), (200, 400));
        // Y4M encode reproduces the fixture byte for byte, then decodes back to the same frames.
        let y4m_spec = EncodeSpec {
            codec: "y4m".into(),
            params: EncodeParams::Video {
                width: 64,
                height: 36,
                ticks_per_frame: 10_160_640_000,
                chroma: "420",
            },
            total_units: 10,
        };
        let mut y4m = Vec::new();
        let done = Y4mProvider
            .encode_segment(
                &y4m_spec,
                &mut Frames10 { next: 0, total: 10 },
                &mut y4m,
                &limits,
                &CancellationToken::default(),
            )
            .unwrap();
        assert!(done.complete);
        assert_eq!(y4m, y4m_bytes(64, 36, (25, 1), "420", 10));
        let mut again = Collect::default();
        decode(
            &Y4mProvider,
            &y4m,
            &request("y4m", 0, 10, grant("rt", &y4m)),
            &limits,
            &mut again,
        )
        .unwrap();
        assert_eq!(again.chunks.len(), 10);
        assert!(again.chunks.iter().all(|c| c.sync
            && !c.preroll
            && c.units == 1
            && c.index as i64 * 10_160_640_000 == c.pts));

        // Proxy / original arbitration.
        let registry = Registry::standard();
        let original = y4m_bytes(64, 36, (25, 1), "420", 10);
        let proxy = y4m_bytes(32, 18, (25, 1), "420", 10);
        let short_proxy = y4m_bytes(32, 18, (25, 1), "420", 9);
        let run = |proxy_bytes: Option<&[u8]>, proxy_len_delta: u64, prefer, purpose| {
            let mut req = request("y4m", 0, 2, grant("orig-1", &original));
            (req.prefer, req.purpose) = (prefer, purpose);
            let mut sink = Collect::default();
            let mut owned = proxy_bytes.map(|b| Cursor::new(b.to_vec()));
            let source = owned.as_mut().map(|reader| {
                let mut g = grant("proxy-1", proxy_bytes.unwrap());
                g.expected_len += proxy_len_delta;
                ProxySource {
                    reader,
                    grant: g,
                    expected_revision: 1,
                }
            });
            let result = registry.decode_segment(
                &mut Cursor::new(original.clone()),
                source,
                &req,
                &limits,
                &CancellationToken::default(),
                &mut sink,
            );
            (result, sink.chunks.first().map_or(0, |c| c.bytes.len()))
        };
        let (r, frame_len) = run(Some(&proxy), 0, SourcePreference::Proxy, Purpose::Preview);
        let r = r.unwrap();
        assert_eq!(
            (r.source, r.proxy_note),
            (SourceKind::Proxy, ProxyNote::ProxyUsed)
        );
        assert_eq!(frame_len, y4m_frame_bytes(32, 18, "420"));
        assert_eq!(
            (
                r.original_id.as_str(),
                r.proxy_id.as_ref().map(|p| p.as_str())
            ),
            ("orig-1", Some("proxy-1"))
        );
        let (r, frame_len) = run(
            Some(&short_proxy),
            0,
            SourcePreference::Proxy,
            Purpose::Preview,
        );
        assert_eq!(r.as_ref().unwrap().source, SourceKind::Original);
        assert_eq!(r.unwrap().proxy_note, ProxyNote::LayoutMismatchUsedOriginal);
        assert_eq!(frame_len, y4m_frame_bytes(64, 36, "420"));
        let (r, _) = run(Some(&proxy), 1, SourcePreference::Proxy, Purpose::Preview);
        assert_eq!(r.unwrap().proxy_note, ProxyNote::ProxyUnusableUsedOriginal);
        let (r, _) = run(None, 0, SourcePreference::Proxy, Purpose::Preview);
        assert_eq!(r.unwrap().proxy_note, ProxyNote::NoProxyAvailable);
        let (r, _) = run(
            Some(&proxy),
            0,
            SourcePreference::Original,
            Purpose::Preview,
        );
        let r = r.unwrap();
        assert_eq!(
            (r.source, r.proxy_note, r.proxy_id),
            (SourceKind::Original, ProxyNote::NotRequested, None)
        );
        let (r, frame_len) = run(Some(&proxy), 0, SourcePreference::Proxy, Purpose::Export);
        assert_eq!(r.unwrap_err(), ReelError::StrictOriginalRequired);
        assert_eq!(frame_len, 0, "export refusal reads nothing");
    });
}
