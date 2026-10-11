use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND, Ticks};
use hsk_studio_reel::fixtures::*;
use hsk_studio_reel::*;
use std::{io::Cursor, sync::Arc};

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
}

impl ChunkSink for Collect {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
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

impl Collect {
    fn joined(&self) -> Vec<u8> {
        self.chunks.iter().flat_map(|c| c.bytes.clone()).collect()
    }
}

fn grant(id: &str, bytes: &[u8]) -> Grant {
    Grant {
        content_id: ContentId::parse(id).unwrap(),
        revision: 1,
        expected_len: bytes.len() as u64,
    }
}

fn request(start: u64, count: u64, output: OutputMode, grant: Grant) -> SegmentRequest {
    SegmentRequest {
        codec: ISOBMFF_PROVIDER_ID.into(),
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

fn index_of(bytes: &[u8], limits: &Limits) -> Result<MediaIndex, ReelError> {
    build_index(
        &mut Cursor::new(bytes.to_vec()),
        bytes.len() as u64,
        limits,
        &CancellationToken::default(),
    )
}

fn decode(
    bytes: &[u8],
    req: &SegmentRequest,
    limits: &Limits,
    sink: &mut dyn ChunkSink,
) -> Result<SegmentReceipt, ReelError> {
    IsobmffProvider.decode_segment(
        &mut Cursor::new(bytes.to_vec()),
        req,
        limits,
        &CancellationToken::default(),
        sink,
    )
}

const TPS_48K: u64 = 5_292_000;
const TPF_2997: u64 = 8_475_667_200;

fn pcm_spec(fourcc: &[u8; 4], kind: PcmKind, big_endian: bool, lpcm_v2: bool) -> PcmMovSpec {
    PcmMovSpec {
        fourcc: *fourcc,
        kind,
        big_endian,
        lpcm_v2,
        non_interleaved: false,
        channels: 2,
        rate: 48000,
        frames: 300,
        frames_per_chunk: 128,
        movie_timescale: 48000,
        edits: None,
        moov_first: true,
    }
}

fn expected_le(kind: PcmKind, from: u64, to: u64) -> Vec<u8> {
    (from..to)
        .flat_map(|f| (0..2u16).flat_map(move |c| pcm_encode(kind, false, pcm_sample_value(f, c))))
        .collect()
}

#[test]
fn pcm_in_mov_decodes_exactly_and_timecode_is_raw() {
    bounded_case("pcm_in_mov_decodes_exactly_and_timecode_is_raw", || {
        // 5000 frames in 5 chunk runs under max_samples = 100: the track must NOT be expanded per frame.
        let limits = Limits {
            max_samples: 100,
            max_chunk_bytes: 1024,
            ..Limits::default()
        };
        let mut spec = pcm_spec(b"twos", PcmKind::S16, true, false);
        (spec.frames, spec.frames_per_chunk) = (5000, 1000);
        let bytes = mov_pcm_bytes(&spec);
        let index = index_of(&bytes, &limits).unwrap();
        let track = &index.tracks[0];
        let stream = track.pcm().expect("regular PCM stays compact");
        assert_eq!((stream.chunk_runs(), stream.total_frames), (5, 5000));
        assert!(track.samples().is_empty());
        let format = pcm_format(track.codec, track.audio.as_ref())
            .unwrap()
            .unwrap();
        assert_eq!(
            (format.bits, format.big_endian, format.channels, format.rate),
            (16, true, 2, 48000)
        );
        // Frames 1500..2600 cross a chunk-run boundary and need five 256-frame-or-less slices.
        let req = request(
            1500 * TPS_48K,
            1100,
            OutputMode::Decoded,
            grant("pcm", &bytes),
        );
        let mut sink = Collect::default();
        let r = decode(&bytes, &req, &limits, &mut sink).unwrap();
        assert_eq!(sink.joined(), expected_le(PcmKind::S16, 1500, 2600));
        assert_eq!(sink.chunks.len(), 5);
        assert!(sink.chunks.iter().all(|c| c.bytes.len() <= 1024 && c.sync));
        assert_eq!(sink.chunks[0].pts, 1500 * TPS_48K as i64);
        assert_eq!(sink.chunks[1].pts, 1756 * TPS_48K as i64);
        assert_eq!(sink.chunks[1].index, 1756);
        assert_eq!(
            (r.codec.as_str(), r.decoder_id),
            ("pcm_s16le", "isobmff-pcm")
        );
        assert_eq!(
            (r.disposition, r.chunks, r.actual.units),
            (Disposition::Exact, 5, 1100)
        );
        assert_eq!(r.actual.start_pts, r.requested.start_pts);
        assert_eq!(r.actual.end_pts, Ticks::new(2600 * TPS_48K));
        assert_eq!(
            r.bytes_read,
            1100 * 4 + index.bytes_read,
            "payload plus the moov read"
        );
        let off = request(
            1500 * TPS_48K + 1,
            10,
            OutputMode::Decoded,
            grant("pcm", &bytes),
        );
        let r = decode(&bytes, &off, &limits, &mut Collect::default()).unwrap();
        assert_eq!(r.disposition, Disposition::Approximate);
        assert_eq!(r.actual.start_pts, Ticks::new(1500 * TPS_48K));
        for (start, count) in [(4900u64, 200u64), (5000, 1)] {
            let req = request(
                start * TPS_48K,
                count,
                OutputMode::Decoded,
                grant("pcm", &bytes),
            );
            let err = decode(&bytes, &req, &limits, &mut Collect::default()).unwrap_err();
            assert_eq!(err.code(), "MEDIA_OUT_OF_RANGE");
        }
        let packets = request(0, 1, OutputMode::Packets, grant("pcm", &bytes));
        assert_eq!(
            decode(&bytes, &packets, &limits, &mut Collect::default()).unwrap_err(),
            ReelError::Unsupported {
                what: "packets-of-compact-pcm-track"
            }
        );

        // Every uncompressed layout decodes to the same little-endian oracle.
        let small = Limits::default();
        let cases: [(&[u8; 4], PcmKind, bool, bool, &str); 6] = [
            (b"sowt", PcmKind::S16, false, false, "pcm_s16le"),
            (b"in24", PcmKind::S24, true, false, "pcm_s24le"),
            (b"fl32", PcmKind::F32, true, false, "pcm_f32le"),
            (b"lpcm", PcmKind::F64, false, true, "pcm_f64le"),
            (b"lpcm", PcmKind::S32, true, true, "pcm_s32le"),
            (b"raw ", PcmKind::U8, false, false, "pcm_u8"),
        ];
        for (fourcc, kind, big_endian, v2, name) in cases {
            let bytes = mov_pcm_bytes(&pcm_spec(fourcc, kind, big_endian, v2));
            let req = request(100 * TPS_48K, 150, OutputMode::Decoded, grant("p", &bytes));
            let mut sink = Collect::default();
            let r = decode(&bytes, &req, &small, &mut sink).unwrap();
            assert_eq!(r.codec, name, "{}", fourcc_string(*fourcc));
            assert_eq!(sink.joined(), expected_le(kind, 100, 250), "{name}");
        }

        // Edit list: media starts at frame 1000, presentation after an empty 480-frame edit.
        let mut edited = pcm_spec(b"twos", PcmKind::S16, true, false);
        (edited.frames, edited.edits) = (5000, Some(vec![(480, -1), (3000, 1000)]));
        let bytes = mov_pcm_bytes(&edited);
        let t = index_of(&bytes, &limits).unwrap();
        let w = t.tracks[0].pcm().unwrap().window.clone().unwrap();
        assert_eq!((w.first_frame, w.frames), (1000, 3000));
        assert_eq!(w.presentation_start, 480 * TPS_48K as i64);
        let req = request(480 * TPS_48K, 10, OutputMode::Decoded, grant("e", &bytes));
        let mut sink = Collect::default();
        decode(&bytes, &req, &limits, &mut sink).unwrap();
        assert_eq!(sink.joined(), expected_le(PcmKind::S16, 1000, 1010));
        let early = request(0, 10, OutputMode::Decoded, grant("e", &bytes));
        assert_eq!(
            decode(&bytes, &early, &limits, &mut Collect::default())
                .unwrap_err()
                .code(),
            "MEDIA_OUT_OF_RANGE"
        );

        // tmcd: raw fields and start sample only (no label; that belongs to pulse).
        let tc = TimecodeMovSpec {
            frames: 30,
            timescale: 30000,
            frame_duration: 1001,
            frame_bytes: 100,
            drop_frame: true,
            counter: false,
            frames_per_second: 30,
            start_value: 107_892,
        };
        let bytes = mov_timecode_bytes(&tc);
        let index = index_of(&bytes, &Limits::default()).unwrap();
        let video = index.track(1).unwrap();
        assert_eq!(
            (video.kind, video.timecode_ref),
            (TrackKind::Video, Some(2))
        );
        let tmcd = index.track(2).unwrap();
        assert_eq!(tmcd.kind, TrackKind::Timecode);
        assert_eq!(
            tmcd.timecode,
            Some(TimecodeDescription {
                drop_frame: true,
                wraps_24h: false,
                negative_ok: false,
                counter: false,
                timescale: 30000,
                frame_duration: 1001,
                frames_per_second: 30,
            })
        );
        let start = read_timecode_start(
            &mut Cursor::new(bytes.clone()),
            &index,
            1,
            &Limits::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (
                start.track_id,
                start.raw_sample,
                start.frame_number,
                start.pts
            ),
            (2, 107_892, 107_892, 0)
        );
        assert_eq!(
            read_timecode_start(&mut Cursor::new(bytes), &index, 2, &Limits::default()).unwrap(),
            None,
            "the tmcd track itself references no timecode"
        );
    });
}

fn frag_samples(sync_first: bool, cto: [i32; 4]) -> Vec<Mp4Sample> {
    cto.iter()
        .enumerate()
        .map(|(i, c)| Mp4Sample {
            duration: 1001,
            cts_offset: *c,
            size: 2000,
            sync: if sync_first { i == 0 } else { false },
        })
        .collect()
}

fn frag_spec(default_base_is_moof: bool) -> FragSpec {
    // Display order inside each 4-sample fragment is decode indexes [0, 2, 3, 1] (B-frame style).
    let cto = [2002, 4004, 1001, 1001];
    FragSpec {
        movie_timescale: 30000,
        media_timescale: 30000,
        codec: *b"avc1",
        width: 64,
        height: 36,
        fragments: vec![
            frag_samples(true, cto),
            frag_samples(true, cto),
            frag_samples(true, cto),
        ],
        tfdt: vec![true, true, false],
        default_base_is_moof,
        with_sidx: true,
        edits: Some(vec![(12 * 1001, 2002)]),
    }
}

/// Presentation index (in frames) of global decode index `g` for the fragment pattern above.
fn display(g: u32) -> u64 {
    u64::from(g / 4 * 4) + [0, 3, 1, 2][(g % 4) as usize]
}

#[test]
fn fragmented_mp4_exact_pts_sidx_and_bounds() {
    bounded_case("fragmented_mp4_exact_pts_sidx_and_bounds", || {
        let limits = Limits::default();
        for base_is_moof in [true, false] {
            let frag = fmp4_bytes(&frag_spec(base_is_moof));
            let bytes = &frag.bytes;
            let index = index_of(bytes, &limits).unwrap();
            assert!(index.fragmented && !index.truncated_tail);
            assert_eq!(
                index.fragment_offsets, frag.moof_offsets,
                "scanner vs fixture oracle"
            );
            assert_eq!(index.segment_indexes.len(), 1);
            let sidx = &index.segment_indexes[0];
            assert_eq!((sidx.reference_id, sidx.timescale), (1, 30000));
            let offsets: Vec<u64> = sidx.entries.iter().map(|e| e.offset).collect();
            assert_eq!(offsets, frag.sidx_entry_offsets);
            assert!(
                sidx.entries
                    .iter()
                    .all(|e| e.starts_with_sap && !e.is_index && e.duration == 4004)
            );
            let track = &index.tracks[0];
            assert_eq!(track.samples().len(), 12);
            // trun first-sample-flags (fragment 1) and per-sample flags (fragments 0, 2) mark the sync samples.
            assert_eq!(track.sync_samples(), &[0, 4, 8]);
            for p in track.presented() {
                assert_eq!(
                    p.pts,
                    display(p.sample) as i64 * TPF_2997 as i64,
                    "sample {}",
                    p.sample
                );
            }
            let r = track.seek(Ticks::new(5 * TPF_2997)).unwrap();
            assert_eq!(
                (r.target.sample, r.decode_from.sample, r.samples_to_decode),
                (6, 4, 3)
            );
            assert_eq!(r.disposition, SeekDisposition::Exact);
            let r = track.seek(Ticks::new(10 * TPF_2997 + 1)).unwrap();
            assert_eq!(
                (r.target.sample, r.disposition),
                (11, SeekDisposition::Containing)
            );

            // Packets for display frames 5..8 (fragment 1): decode order 4..=7 with one pre-roll.
            let mut req = request(5 * TPF_2997, 3, OutputMode::Packets, grant("frag", bytes));
            req.expected_revision = 1;
            let mut sink = Collect::default();
            let receipt = decode(bytes, &req, &limits, &mut sink).unwrap();
            let order: Vec<u64> = sink.chunks.iter().map(|c| c.index).collect();
            assert_eq!(order, [4, 5, 6, 7]);
            let preroll: Vec<bool> = sink.chunks.iter().map(|c| c.preroll).collect();
            assert_eq!(preroll, [true, false, false, false]);
            for c in &sink.chunks {
                let g = c.index as u32;
                assert_eq!(c.pts, display(g) as i64 * TPF_2997 as i64);
                assert_eq!(c.sync, g == 4);
                assert!(
                    c.bytes
                        .iter()
                        .enumerate()
                        .all(|(j, b)| *b == mp4_sample_byte(g, j as u32))
                );
            }
            assert_eq!(
                (receipt.actual.units, receipt.chunks, receipt.disposition),
                (3, 4, Disposition::Exact)
            );
            assert!(receipt.bytes_read < bytes.len() as u64);
        }

        // A recording cut inside the last mdat: indexed, flagged, and reading the lost sample is Truncated.
        let frag = fmp4_bytes(&frag_spec(true));
        let cut = &frag.bytes[..frag.bytes.len() - 1000];
        let index = index_of(cut, &limits).unwrap();
        assert!(index.truncated_tail);
        assert_eq!(index.tracks[0].samples().len(), 12);
        let req = request(10 * TPF_2997, 1, OutputMode::Packets, grant("cut", cut));
        assert_eq!(
            decode(cut, &req, &limits, &mut Collect::default()).unwrap_err(),
            ReelError::Truncated { delivered: 0 }
        );
        let tight = Limits {
            max_samples: 6,
            ..limits
        };
        assert_eq!(
            index_of(&frag.bytes, &tight).unwrap_err(),
            ReelError::LimitExceeded {
                what: "sample-count"
            }
        );
    });
}

#[test]
fn index_cache_keys_on_content_revision_and_limits() {
    bounded_case("index_cache_keys_on_content_revision_and_limits", || {
        assert!(IndexCache::new(0).is_err() && IndexCache::new(65).is_err());
        let spec = |samples: usize| Mp4Spec {
            movie_timescale: 30000,
            media_timescale: 30000,
            codec: *b"avc1",
            width: 64,
            height: 36,
            samples: (0..samples)
                .map(|_| Mp4Sample {
                    duration: 1001,
                    cts_offset: 0,
                    size: 50,
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
        };
        let (v1, v2) = (mp4_bytes(&spec(6)), mp4_bytes(&spec(8)));
        let cancel = CancellationToken::default();
        let limits = Limits::default();
        let other_limits = Limits {
            max_tracks: 3,
            ..limits
        };
        let mut cache = IndexCache::new(2).unwrap();
        let get = |cache: &mut IndexCache, id: &str, rev: u64, bytes: &[u8], l: &Limits| {
            let mut g = grant(id, bytes);
            g.revision = rev;
            cache.get_or_build(&mut Cursor::new(bytes.to_vec()), &g, rev, l, &cancel)
        };

        let first = get(&mut cache, "a", 1, &v1, &limits).unwrap();
        let again = get(&mut cache, "a", 1, &v1, &limits).unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        assert_eq!(
            (
                cache.stats().hits,
                cache.stats().misses,
                cache.stats().entries
            ),
            (1, 1, 1)
        );
        assert!(cache.stats().approx_bytes > 0);
        // Same revision under different limits is a different index (limits can change the outcome).
        let limited = get(&mut cache, "a", 1, &v1, &other_limits).unwrap();
        assert!(!Arc::ptr_eq(&first, &limited));
        assert_eq!(cache.stats().entries, 2);
        // A new revision drops that content id's older entries.
        let newer = get(&mut cache, "a", 2, &v2, &limits).unwrap();
        assert_eq!(newer.tracks[0].samples().len(), 8);
        let s = cache.stats();
        assert_eq!((s.invalidated, s.entries), (2, 1));
        // The live file no longer matches the grant, or the caller's revision moved on: StaleGrant.
        let mut wrong_len = grant("a", &v2);
        wrong_len.expected_len += 1;
        wrong_len.revision = 2;
        assert_eq!(
            cache
                .get_or_build(
                    &mut Cursor::new(v2.clone()),
                    &wrong_len,
                    2,
                    &limits,
                    &cancel
                )
                .unwrap_err(),
            ReelError::StaleGrant
        );
        assert_eq!(get(&mut cache, "a", 2, &v2, &limits).map(|_| ()), Ok(()));
        let mut g3 = grant("a", &v2);
        g3.revision = 3;
        assert_eq!(
            cache
                .get_or_build(&mut Cursor::new(v2.clone()), &g3, 2, &limits, &cancel)
                .unwrap_err(),
            ReelError::StaleGrant
        );
        // Capacity 2: a third distinct source evicts the least recently used.
        get(&mut cache, "b", 1, &v1, &limits).unwrap();
        get(&mut cache, "c", 1, &v1, &limits).unwrap();
        let s = cache.stats();
        assert_eq!((s.entries, s.evictions), (2, 1));

        // Cached delivery: same packets as the uncached path, payload-only byte accounting.
        let mut cache = IndexCache::new(4).unwrap();
        let req = request(0, 3, OutputMode::Packets, grant("a", &v1));
        let (mut cold, mut warm, mut plain) =
            (Collect::default(), Collect::default(), Collect::default());
        let mut src = Cursor::new(v1.clone());
        let r1 = IsobmffProvider
            .decode_cached(&mut cache, &mut src, &req, &limits, &cancel, &mut cold)
            .unwrap();
        let r2 = IsobmffProvider
            .decode_cached(&mut cache, &mut src, &req, &limits, &cancel, &mut warm)
            .unwrap();
        let r3 = decode(&v1, &req, &limits, &mut plain).unwrap();
        assert_eq!((cache.stats().hits, cache.stats().misses), (1, 1));
        assert_eq!(cold.joined(), plain.joined());
        assert_eq!(warm.joined(), plain.joined());
        assert_eq!((r1.bytes_read, r2.bytes_read), (150, 150));
        assert!(
            r3.bytes_read > 150,
            "uncached delivery also counts the moov read"
        );
        assert_eq!(
            warm.chunks.iter().map(|c| c.index).collect::<Vec<_>>(),
            [0, 1, 2]
        );
    });
}

#[test]
fn slice2_unsupported_and_hostile_inputs_are_typed() {
    bounded_case("slice2_unsupported_and_hostile_inputs_are_typed", || {
        let limits = Limits::default();
        // lpcm that cannot be honoured: non-interleaved layout.
        let mut non_interleaved = pcm_spec(b"lpcm", PcmKind::S16, false, true);
        non_interleaved.non_interleaved = true;
        let bytes = mov_pcm_bytes(&non_interleaved);
        let req = request(0, 4, OutputMode::Decoded, grant("l", &bytes));
        assert_eq!(
            decode(&bytes, &req, &limits, &mut Collect::default()).unwrap_err(),
            ReelError::Unsupported {
                what: "lpcm-non-interleaved"
            }
        );
        // Two non-empty edits, and an edit that is not a whole number of audio frames.
        let mut two = pcm_spec(b"twos", PcmKind::S16, true, false);
        two.edits = Some(vec![(100, 0), (100, 200)]);
        let bytes = mov_pcm_bytes(&two);
        let req = request(0, 4, OutputMode::Decoded, grant("two", &bytes));
        assert_eq!(
            decode(&bytes, &req, &limits, &mut Collect::default()).unwrap_err(),
            ReelError::Unsupported {
                what: "pcm-edit-list-shape"
            }
        );
        let mut odd = pcm_spec(b"twos", PcmKind::S16, true, false);
        (odd.rate, odd.movie_timescale, odd.edits) = (44100, 1000, Some(vec![(1, 0)]));
        let bytes = mov_pcm_bytes(&odd);
        let req = request(0, 4, OutputMode::Decoded, grant("odd", &bytes));
        assert_eq!(
            decode(&bytes, &req, &limits, &mut Collect::default()).unwrap_err(),
            ReelError::Unsupported {
                what: "pcm-edit-not-frame-aligned"
            }
        );
        // A compressed audio entry (`mp4a` here) is a typed codec loss, while its packets still demux.
        let mut aac = pcm_spec(b"mp4a", PcmKind::S16, true, false);
        aac.frames = 40;
        let bytes = mov_pcm_bytes(&aac);
        let req = request(0, 3, OutputMode::Decoded, grant("aac", &bytes));
        assert_eq!(
            decode(&bytes, &req, &limits, &mut Collect::default()).unwrap_err(),
            ReelError::CodecUnavailable {
                codec: "mp4a".into()
            }
        );
        let req = request(0, 3, OutputMode::Packets, grant("aac", &bytes));
        let mut sink = Collect::default();
        decode(&bytes, &req, &limits, &mut sink).unwrap();
        assert_eq!(
            sink.chunks
                .iter()
                .map(|c| (c.pts, c.bytes.len(), c.units, c.sync))
                .collect::<Vec<_>>(),
            (0..3)
                .map(|i| (i * TPS_48K as i64, 4usize, 1u64, true))
                .collect::<Vec<_>>()
        );

        // Hostile fragments: absurd trun count, unknown track, a second moov, tfdt beyond i64.
        let frag = fmp4_bytes(&frag_spec(true));
        let find = |bytes: &[u8], tag: &[u8; 4], from: usize| {
            from + bytes[from..]
                .windows(4)
                .position(|w| w == tag)
                .expect("box present")
        };
        let mut huge = frag.bytes.clone();
        let trun = find(&huge, b"trun", frag.moof_offsets[0] as usize);
        huge[trun + 8..trun + 12].copy_from_slice(&0xFFFF_FFF0u32.to_be_bytes());
        assert_eq!(
            index_of(&huge, &limits).unwrap_err(),
            ReelError::Corrupt {
                what: "trun-exceeds-box"
            },
            "per-sample fields cannot exceed the box"
        );
        // No per-sample fields (flags = data offset only): the count alone hits the sample budget first.
        huge[trun + 4..trun + 8].copy_from_slice(&1u32.to_be_bytes());
        assert_eq!(
            index_of(&huge, &limits).unwrap_err(),
            ReelError::LimitExceeded {
                what: "sample-count"
            }
        );
        let mut stray = frag.bytes.clone();
        let tfhd = find(&stray, b"tfhd", frag.moof_offsets[1] as usize);
        stray[tfhd + 8..tfhd + 12].copy_from_slice(&7u32.to_be_bytes());
        assert_eq!(
            index_of(&stray, &limits).unwrap_err(),
            ReelError::Corrupt {
                what: "traf-unknown-track"
            }
        );
        let mut twice = frag.bytes.clone();
        twice.extend_from_slice(&[0, 0, 0, 8, b'm', b'o', b'o', b'v']);
        assert_eq!(
            index_of(&twice, &limits).unwrap_err(),
            ReelError::Corrupt {
                what: "second-moov"
            }
        );
        let mut far = frag.bytes.clone();
        let tfdt = find(&far, b"tfdt", frag.moof_offsets[0] as usize);
        far[tfdt + 8..tfdt + 16].copy_from_slice(&u64::MAX.to_be_bytes());
        assert_eq!(
            index_of(&far, &limits).unwrap_err().code(),
            "MEDIA_LIMIT_EXCEEDED"
        );
        assert_eq!(
            TICKS_PER_SECOND / 48000,
            TPS_48K,
            "fixture tick constant matches accord"
        );
    });
}
