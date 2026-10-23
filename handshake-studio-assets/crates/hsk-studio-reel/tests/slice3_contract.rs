use hsk_studio_accord::{CancellationToken, Ticks};
mod common;
use common::*;
use hsk_studio_reel::*;
use sha2::{Digest, Sha256};
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

#[derive(Default)]
struct Collect {
    /// (pts, index, preroll, sync, bytes)
    chunks: Vec<(i64, u64, bool, bool, Vec<u8>)>,
}

impl ChunkSink for Collect {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
        self.chunks
            .push((c.pts, c.index, c.preroll, c.sync, c.bytes.to_vec()));
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

fn request(start: u64, count: u64, track: u32, output: OutputMode, grant: Grant) -> SegmentRequest {
    SegmentRequest {
        codec: MATROSKA_PROVIDER_ID.into(),
        start_pts: Ticks::new(start),
        count,
        track: Some(track),
        output,
        prefer: SourcePreference::Original,
        purpose: Purpose::Preview,
        grant,
        expected_revision: 1,
    }
}

fn index_of(bytes: &[u8], limits: &Limits) -> Result<MkvIndex, ReelError> {
    build_matroska_index(
        &mut Cursor::new(bytes.to_vec()),
        bytes.len() as u64,
        limits,
        &CancellationToken::default(),
    )
}

fn layout_of(bytes: &[u8]) -> Result<MkvLayout, ReelError> {
    read_layout(
        &mut Cursor::new(bytes.to_vec()),
        bytes.len() as u64,
        &Limits::default(),
        &CancellationToken::default(),
    )
}

const MS: u64 = 254_016_000; // ticks per millisecond
const FRAME: u64 = 40 * MS;

fn block(track: u8, rel_ts: i16, key: bool, frames: &[u32], lacing: Lacing) -> MkvBlock {
    MkvBlock {
        track,
        rel_ts,
        key,
        frames: frames.to_vec(),
        lacing,
        group: None,
        raw_lace_header: None,
    }
}

fn video(rel_ts: i16, key: bool) -> MkvBlock {
    block(1, rel_ts, key, &[100], Lacing::None)
}

/// 13 video frames in decode order (open GOP in cluster 1) plus laced audio in every cluster.
fn spec() -> MkvSpec {
    let mut grouped = video(160, false);
    grouped.group = Some(MkvGroup {
        duration: Some(40),
        reference: true,
    });
    MkvSpec {
        doc_type: "matroska",
        timecode_scale: 1_000_000,
        unknown_sizes: false,
        with_cues: true,
        video_default_duration_ns: Some(40_000_000),
        content_encoding_on_video: false,
        clusters: vec![
            MkvCluster {
                timecode: 0,
                blocks: vec![
                    video(0, true),
                    video(120, false),
                    video(40, false),
                    video(80, false),
                    video(160, false),
                    block(2, 0, true, &[10, 10, 10], Lacing::Fixed),
                ],
            },
            MkvCluster {
                timecode: 200,
                blocks: vec![
                    video(80, true),
                    video(120, false),
                    video(0, false),
                    video(40, false),
                    grouped,
                    block(2, 0, true, &[5, 300, 7], Lacing::Xiph),
                ],
            },
            MkvCluster {
                timecode: 400,
                blocks: vec![
                    video(0, true),
                    video(40, false),
                    video(80, false),
                    block(2, 0, true, &[10, 12, 9], Lacing::Ebml),
                ],
            },
        ],
    }
}

/// Presentation time (ms) of each video frame in decode order.
const VIDEO_PTS_MS: [u64; 13] = [0, 120, 40, 80, 160, 280, 320, 200, 240, 360, 400, 440, 480];
const VIDEO_KEY: [bool; 13] = [
    true, false, false, false, false, true, false, false, false, false, true, false, false,
];
const AUDIO_PTS_MS: [u64; 9] = [0, 20, 40, 200, 220, 240, 400, 420, 440];
const AUDIO_SIZES: [u32; 9] = [10, 10, 10, 5, 300, 7, 10, 12, 9];

#[test]
fn matroska_index_exact_pts_lacing_cues_and_cued_seek() {
    bounded_case("matroska_index_exact_pts_lacing_cues_and_cued_seek", || {
        let mkv = mkv_bytes(&spec());
        let bytes = &mkv.bytes;
        let limits = Limits::default();
        let index = index_of(bytes, &limits).unwrap();
        let l = &index.layout;
        assert_eq!(
            (l.doc_type.as_str(), l.timestamp_scale_ns),
            ("matroska", 1_000_000)
        );
        assert_eq!(l.segment_data_start, mkv.segment_data_start);
        assert!(!l.truncated_tail && l.duration_units == Some(600.0));
        let v = &index.track(1).unwrap().info;
        assert_eq!(
            (v.codec_id.as_str(), v.width, v.height),
            ("V_MPEG4/ISO/AVC", 64, 36)
        );
        assert_eq!(
            (
                v.default_duration_ns,
                v.flag_lacing,
                v.codec_private.as_slice()
            ),
            (Some(40_000_000), false, &[1, 2, 3, 4][..])
        );
        let a = &index.track(2).unwrap().info;
        assert_eq!(
            (a.codec_id.as_str(), a.channels, a.sampling_frequency),
            ("A_OPUS", 2, 48000.0)
        );
        assert_eq!(
            (a.codec_delay_ns, a.seek_pre_roll_ns, a.flag_lacing),
            (6_500_000, 80_000_000, true)
        );
        // Clusters and Cues agree with the fixture's independent positions.
        let positions: Vec<u64> = l.clusters.iter().map(|c| c.position).collect();
        assert_eq!(positions, mkv.cluster_positions);
        assert_eq!(
            l.clusters.iter().map(|c| c.timecode).collect::<Vec<_>>(),
            [Some(0), Some(200), Some(400)]
        );
        let cues: Vec<(u64, u64, u64)> = l
            .cues
            .iter()
            .map(|c| {
                (
                    c.time,
                    c.positions[0].cluster_position,
                    c.positions[0].track,
                )
            })
            .collect();
        assert_eq!(
            cues,
            [
                (0, positions[0], 1),
                (280, positions[1], 1),
                (400, positions[2], 1)
            ]
        );
        assert!(
            l.cues
                .iter()
                .all(|c| c.positions[0].relative_position.is_some())
        );

        // Video samples: exact offsets, sizes, PTS and keyframes in decode order.
        let video_frames: Vec<&MkvFrameInfo> = mkv.frames.iter().filter(|f| f.track == 1).collect();
        let vt = &index.track(1).unwrap().index;
        assert_eq!(vt.samples().len(), 13);
        for (g, s) in vt.samples().iter().enumerate() {
            assert_eq!(
                (s.offset, s.size, s.pts),
                (video_frames[g].offset, 100, VIDEO_PTS_MS[g] as i64),
                "video frame {g}"
            );
            assert_eq!(s.sync, VIDEO_KEY[g], "key {g}");
            assert_eq!(s.duration, 40);
        }
        let mut shown: Vec<(i64, u32)> = vt.presented().iter().map(|p| (p.pts, p.sample)).collect();
        shown.sort();
        for (j, (pts, g)) in shown.iter().enumerate() {
            assert_eq!(*pts, (j as u64 * FRAME) as i64);
            assert_eq!(VIDEO_PTS_MS[*g as usize], j as u64 * 40);
        }
        assert!(vt.ticks_exact);
        // Open-GOP leading pictures (200 ms, 240 ms) decode from the previous keyframe; others from theirs.
        for (ms, target, from) in [
            (0u64, 0u32, 0u32),
            (120, 1, 0),
            (200, 7, 0),
            (240, 8, 0),
            (280, 5, 5),
            (360, 9, 5),
            (480, 12, 10),
        ] {
            let r = vt.seek(Ticks::new(ms * MS)).unwrap();
            assert_eq!(
                (r.target.sample, r.decode_from.sample),
                (target, from),
                "seek {ms} ms"
            );
            assert_eq!(r.disposition, SeekDisposition::Exact);
        }
        assert_eq!(
            vt.seek(Ticks::new(520 * MS)).unwrap_err().code(),
            "MEDIA_OUT_OF_RANGE"
        );

        // Laced audio: every frame has its own offset, size and PTS (Fixed, Xiph and EBML lacing).
        let audio_frames: Vec<&MkvFrameInfo> = mkv.frames.iter().filter(|f| f.track == 2).collect();
        let at = &index.track(2).unwrap().index;
        assert_eq!(at.samples().len(), 9);
        for (i, s) in at.samples().iter().enumerate() {
            assert_eq!(
                (s.offset, s.size, s.pts, s.duration),
                (
                    audio_frames[i].offset,
                    AUDIO_SIZES[i],
                    AUDIO_PTS_MS[i] as i64,
                    20
                ),
                "audio frame {i}"
            );
        }
        assert_eq!(at.audio.unwrap().sample_rate, 48000);

        // Packet delivery: display frames 200/240/280 ms, decode order from the previous keyframe.
        let provider = MatroskaProvider;
        let req = request(5 * FRAME, 3, 1, OutputMode::Packets, grant("mkv", bytes));
        let mut sink = Collect::default();
        let receipt = provider
            .decode_segment(
                &mut Cursor::new(bytes.clone()),
                &req,
                &limits,
                &CancellationToken::default(),
                &mut sink,
            )
            .unwrap();
        assert_eq!(
            sink.chunks.iter().map(|c| c.1).collect::<Vec<_>>(),
            (0..=8).collect::<Vec<u64>>()
        );
        let preroll: Vec<bool> = sink.chunks.iter().map(|c| c.2).collect();
        assert_eq!(
            preroll,
            [true, true, true, true, true, false, true, false, false]
        );
        for c in &sink.chunks {
            let g = c.1 as usize;
            assert_eq!(c.0, (VIDEO_PTS_MS[g] * MS) as i64);
            assert_eq!(c.3, VIDEO_KEY[g]);
            assert!(
                c.4.iter()
                    .enumerate()
                    .all(|(j, b)| *b == mkv_frame_byte(video_frames[g].id, j as u32))
            );
        }
        assert_eq!(
            (receipt.codec.as_str(), receipt.decoder_id),
            ("V_MPEG4/ISO/AVC", "matroska-demux")
        );
        assert_eq!(
            (receipt.disposition, receipt.chunks, receipt.actual.units),
            (Disposition::Exact, 9, 3)
        );
        assert!(receipt.complete && receipt.ticks_exact && receipt.bytes_read >= 900);
        // Laced audio packets: a Xiph-laced block's middle frame (300 bytes) at 220 ms.
        let req = request(220 * MS, 1, 2, OutputMode::Packets, grant("mkv", bytes));
        let mut sink = Collect::default();
        provider
            .decode_segment(
                &mut Cursor::new(bytes.clone()),
                &req,
                &limits,
                &CancellationToken::default(),
                &mut sink,
            )
            .unwrap();
        assert_eq!(
            (sink.chunks[0].0, sink.chunks[0].4.len()),
            ((220 * MS) as i64, 300)
        );
        assert!(
            sink.chunks[0]
                .4
                .iter()
                .enumerate()
                .all(|(j, b)| *b == mkv_frame_byte(audio_frames[4].id, j as u32))
        );
        let decoded = request(0, 1, 1, OutputMode::Decoded, grant("mkv", bytes));
        assert_eq!(
            provider
                .decode_segment(
                    &mut Cursor::new(bytes.clone()),
                    &decoded,
                    &limits,
                    &CancellationToken::default(),
                    &mut NeverReached
                )
                .unwrap_err(),
            ReelError::CodecUnavailable {
                codec: "V_MPEG4/ISO/AVC".into()
            }
        );

        // Cue-based seek equals the full-index seek and scans fewer clusters when it can.
        let layout = layout_of(bytes).unwrap();
        let cancel = CancellationToken::default();
        for (ms, cue, scanned) in [
            (0u64, Some(0u64), 2u32),
            (100, Some(0), 2),
            (200, Some(0), 3),
            (300, Some(280), 2),
            (480, Some(400), 1),
        ] {
            let want = vt.seek(Ticks::new(ms * MS)).unwrap();
            let got = cued_seek(
                &mut Cursor::new(bytes.clone()),
                &layout,
                1,
                Ticks::new(ms * MS),
                &limits,
                &cancel,
            )
            .unwrap();
            assert_eq!(
                (
                    got.seek.target.pts,
                    got.seek.decode_from.pts,
                    got.seek.disposition
                ),
                (want.target.pts, want.decode_from.pts, want.disposition),
                "cued seek {ms} ms"
            );
            assert_eq!(
                (got.cue_time, got.clusters_scanned),
                (cue, scanned),
                "{ms} ms"
            );
            assert_eq!(
                got.target_offset,
                video_frames[want.target.sample as usize].offset
            );
            assert_eq!(
                got.decode_from_offset,
                video_frames[want.decode_from.sample as usize].offset
            );
        }
        // Without Cues the scan starts at the first cluster and still agrees.
        let mut no_cues = spec();
        no_cues.with_cues = false;
        let plain = mkv_bytes(&no_cues);
        let layout = layout_of(&plain.bytes).unwrap();
        assert!(layout.cues.is_empty());
        let got = cued_seek(
            &mut Cursor::new(plain.bytes.clone()),
            &layout,
            1,
            Ticks::new(480 * MS),
            &limits,
            &cancel,
        )
        .unwrap();
        assert_eq!(
            (got.cue_time, got.clusters_scanned, got.seek.target.pts),
            (None, 3, (480 * MS) as i64)
        );

        // Unknown-size Segment and Clusters index identically.
        let mut open = spec();
        open.unknown_sizes = true;
        let open = mkv_bytes(&open);
        let open_index = index_of(&open.bytes, &limits).unwrap();
        assert_eq!(open_index.layout.clusters.len(), 3);
        let open_video: Vec<&MkvFrameInfo> = open.frames.iter().filter(|f| f.track == 1).collect();
        for (g, s) in open_index
            .track(1)
            .unwrap()
            .index
            .samples()
            .iter()
            .enumerate()
        {
            assert_eq!(
                (s.offset, s.pts, s.sync),
                (open_video[g].offset, VIDEO_PTS_MS[g] as i64, VIDEO_KEY[g])
            );
        }
        assert_eq!(open_index.track(2).unwrap().index.samples().len(), 9);
    });
}

fn tiny() -> MkvSpec {
    MkvSpec {
        clusters: vec![MkvCluster {
            timecode: 0,
            blocks: vec![
                video(0, true),
                video(40, false),
                block(2, 0, true, &[8, 8], Lacing::Fixed),
            ],
        }],
        ..spec()
    }
}

#[test]
fn matroska_hostile_unsupported_and_registry() {
    bounded_case("matroska_hostile_unsupported_and_registry", || {
        let limits = Limits::default();
        // Doc type / timestamp scale / content encodings are typed, not guessed.
        let mut avi = tiny();
        avi.doc_type = "avi";
        assert_eq!(
            layout_of(&mkv_bytes(&avi).bytes).unwrap_err(),
            ReelError::ImportUnsupported {
                detected: "ebml:avi".into()
            }
        );
        let mut webm = tiny();
        webm.doc_type = "webm";
        assert_eq!(layout_of(&mkv_bytes(&webm).bytes).unwrap().doc_type, "webm");
        let mut odd_scale = tiny();
        odd_scale.timecode_scale = 3_000_000;
        assert_eq!(
            layout_of(&mkv_bytes(&odd_scale).bytes).unwrap_err().code(),
            "MEDIA_UNSUPPORTED"
        );
        assert_eq!(
            layout_of(b"RIFF....").unwrap_err().code(),
            "MEDIA_IMPORT_UNSUPPORTED"
        );
        let mut encoded = tiny();
        encoded.content_encoding_on_video = true;
        let enc = mkv_bytes(&encoded);
        let provider = MatroskaProvider;
        let run = |bytes: &[u8], req: &SegmentRequest, sink: &mut dyn ChunkSink| {
            provider.decode_segment(
                &mut Cursor::new(bytes.to_vec()),
                req,
                &limits,
                &CancellationToken::default(),
                sink,
            )
        };
        let video_req = request(0, 1, 1, OutputMode::Packets, grant("enc", &enc.bytes));
        assert_eq!(
            run(&enc.bytes, &video_req, &mut NeverReached).unwrap_err(),
            ReelError::Unsupported {
                what: "content-encoding"
            }
        );
        let audio_req = request(0, 1, 2, OutputMode::Packets, grant("enc", &enc.bytes));
        run(&enc.bytes, &audio_req, &mut Collect::default()).unwrap();

        // A recording cut inside the last block: earlier frames stay indexed, the loss is flagged.
        let full = mkv_bytes(&spec());
        let last_audio = full.frames.iter().rfind(|f| f.track == 2).unwrap().offset as usize;
        let cut = &full.bytes[..last_audio + 3];
        let cut_index = index_of(cut, &limits).unwrap();
        assert!(cut_index.layout.truncated_tail && cut_index.layout.clusters[2].truncated);
        assert_eq!(cut_index.track(1).unwrap().index.samples().len(), 13);
        assert_eq!(cut_index.track(2).unwrap().index.samples().len(), 6);

        // Hostile EBML: bad ID length, bad size length, unknown-size child, unknown track.
        let t = mkv_bytes(&tiny());
        let cluster = (t.segment_data_start + t.cluster_positions[0]) as usize;
        let first_video = t.frames.iter().find(|f| f.track == 1).unwrap().offset as usize;
        let patched = |at: usize, value: u8| {
            let mut b = t.bytes.clone();
            b[at] = value;
            b
        };
        assert_eq!(t.bytes[cluster], 0x1F);
        assert_eq!(t.bytes[first_video - 6], 0xA3);
        assert_eq!(
            layout_of(&patched(cluster, 0x08)).unwrap_err(),
            ReelError::Corrupt {
                what: "ebml-id-length"
            }
        );
        assert_eq!(
            layout_of(&patched(cluster + 4, 0x00)).unwrap_err(),
            ReelError::Corrupt {
                what: "ebml-size-length"
            }
        );
        assert_eq!(
            index_of(&patched(first_video - 5, 0xFF), &limits).unwrap_err(),
            ReelError::Corrupt {
                what: "unknown-size-child"
            }
        );
        assert_eq!(
            index_of(&patched(first_video - 4, 0x89), &limits).unwrap_err(),
            ReelError::Corrupt {
                what: "block-unknown-track"
            }
        );
        // Hostile lacing: sizes beyond the block, a fixed lace that does not divide, lacing on a track
        // that forbids it.
        let laced = |blocks: Vec<MkvBlock>| {
            index_of(
                &mkv_bytes(&MkvSpec {
                    clusters: vec![MkvCluster {
                        timecode: 0,
                        blocks,
                    }],
                    ..tiny()
                })
                .bytes,
                &limits,
            )
        };
        let mut too_big = block(2, 0, true, &[4, 4, 4], Lacing::Xiph);
        too_big.raw_lace_header = Some(vec![200, 200]);
        assert_eq!(
            laced(vec![video(0, true), too_big]).unwrap_err(),
            ReelError::Corrupt {
                what: "lace-sizes-exceed-block"
            }
        );
        assert_eq!(
            laced(vec![
                video(0, true),
                block(2, 0, true, &[10, 10, 11], Lacing::Fixed)
            ])
            .unwrap_err(),
            ReelError::Corrupt {
                what: "fixed-lace-size"
            }
        );
        assert_eq!(
            laced(vec![block(1, 0, true, &[5, 5], Lacing::Fixed)]).unwrap_err(),
            ReelError::Corrupt {
                what: "lacing-on-unlaced-track"
            }
        );
        assert_eq!(
            index_of(
                &full.bytes,
                &Limits {
                    max_samples: 5,
                    ..limits
                }
            )
            .unwrap_err(),
            ReelError::LimitExceeded {
                what: "sample-count"
            }
        );

        // Stale grant before any read, and an index replayed against another file.
        let mut stale = grant("t", &t.bytes);
        stale.expected_len += 1;
        let req = request(0, 1, 1, OutputMode::Packets, stale);
        assert_eq!(
            run(&t.bytes, &req, &mut NeverReached).unwrap_err(),
            ReelError::StaleGrant
        );
        let index_of_tiny = index_of(&t.bytes, &limits).unwrap();
        let req = request(0, 1, 1, OutputMode::Packets, grant("full", &full.bytes));
        assert_eq!(
            provider
                .decode_packets(
                    &mut Cursor::new(full.bytes.clone()),
                    &index_of_tiny,
                    &req,
                    &limits,
                    &CancellationToken::default(),
                    &mut NeverReached
                )
                .unwrap_err(),
            ReelError::StaleGrant
        );

        // Registry: detected by the EBML magic, decoded through the instance-owned registry.
        let registry = Registry::standard();
        let found = registry
            .detect(&mut Cursor::new(full.bytes.clone()))
            .unwrap();
        assert_eq!(found.descriptor().id, "matroska");
        let mut req = request(
            5 * FRAME,
            1,
            1,
            OutputMode::Packets,
            grant("full", &full.bytes),
        );
        req.track = None;
        let mut sink = Collect::default();
        let receipt = registry
            .decode_segment(
                &mut Cursor::new(full.bytes.clone()),
                None,
                &req,
                &limits,
                &CancellationToken::default(),
                &mut sink,
            )
            .unwrap();
        assert_eq!(
            (receipt.codec.as_str(), sink.chunks.last().unwrap().0),
            ("V_MPEG4/ISO/AVC", (200 * MS) as i64)
        );
    });
}

fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .collect()
}

#[test]
fn sampled_identity_and_relink_report_confidence() {
    bounded_case("sampled_identity_and_relink_report_confidence", || {
        let cancel = CancellationToken::default();
        let params = IdentityParams::default();
        let data = noise(1 << 20, 7);
        let identity = |bytes: &[u8]| {
            sampled_identity(&mut Cursor::new(bytes.to_vec()), params, &cancel).unwrap()
        };
        let original = identity(&data);
        assert!(!original.exhaustive && original.len == data.len() as u64);
        assert_eq!(original.digest_hex().len(), 64);
        assert_eq!(
            original,
            identity(&data),
            "same content, same identity (a path is never part of it)"
        );
        // A change inside a sampled window changes the identity; one between windows does not.
        let mut in_window = data.clone();
        in_window[10] ^= 1;
        assert_ne!(identity(&in_window).digest, original.digest);
        let mut in_gap = data.clone();
        in_gap[100_000] ^= 1;
        assert_eq!(
            identity(&in_gap).digest,
            original.digest,
            "between windows is invisible to sampling"
        );
        // The full digest is the plain SHA-256 and does see the gap.
        let full = full_digest(&mut Cursor::new(data.clone()), 64 * 1024, &cancel).unwrap();
        assert_eq!(full.as_slice(), Sha256::digest(&data).as_slice());
        assert_ne!(
            full_digest(&mut Cursor::new(in_gap.clone()), 64 * 1024, &cancel).unwrap(),
            full
        );
        // Other window parameters are a different identity; short sources are hashed whole.
        assert_ne!(
            sampled_identity(
                &mut Cursor::new(data.clone()),
                IdentityParams {
                    window: 32 * 1024,
                    windows: 8
                },
                &cancel
            )
            .unwrap()
            .digest,
            original.digest
        );
        let small = noise(100 * 1024, 9);
        let small_id = identity(&small);
        assert!(small_id.exhaustive);
        let mut tweaked = small.clone();
        tweaked[50_000] ^= 1;
        assert_ne!(identity(&tweaked).digest, small_id.digest);
        assert_eq!(
            sampled_identity(
                &mut Cursor::new(data.clone()),
                IdentityParams {
                    window: 0,
                    windows: 8
                },
                &cancel
            )
            .unwrap_err(),
            ReelError::LimitExceeded {
                what: "identity-params"
            }
        );
        let canceled = CancellationToken::default();
        canceled.cancel();
        assert_eq!(
            sampled_identity(&mut Cursor::new(data.clone()), params, &canceled).unwrap_err(),
            ReelError::Canceled
        );

        // Relink: length prefilter, sampled confidence, ambiguity, optional full verification.
        let mut shorter = data.clone();
        shorter.pop();
        let mut window_changed = data.clone();
        window_changed[5] ^= 0x80;
        let query = RelinkQuery {
            expected: original,
            expected_full: Some(full),
            verify_full: false,
        };
        let relink_with = |query: &RelinkQuery, files: &[&Vec<u8>]| {
            let mut readers: Vec<Cursor<Vec<u8>>> =
                files.iter().map(|f| Cursor::new((*f).clone())).collect();
            let mut refs: Vec<&mut dyn ReadSeek> =
                readers.iter_mut().map(|r| r as &mut dyn ReadSeek).collect();
            relink(query, &mut refs, 64 * 1024, &cancel).unwrap()
        };
        let copy = data.clone();
        assert_eq!(
            relink_with(&query, &[&shorter, &window_changed, &data]),
            RelinkOutcome::Unique(RelinkMatch {
                candidate: 2,
                confidence: RelinkConfidence::Sampled
            })
        );
        // The gap-edited file is a sampled collision: ambiguous without full verification.
        let RelinkOutcome::Ambiguous(found) = relink_with(&query, &[&in_gap, &copy, &shorter])
        else {
            panic!("a sampled collision must be reported as ambiguous");
        };
        assert_eq!(
            found
                .iter()
                .map(|m| (m.candidate, m.confidence))
                .collect::<Vec<_>>(),
            [
                (0, RelinkConfidence::Sampled),
                (1, RelinkConfidence::Sampled)
            ]
        );
        let verified = RelinkQuery {
            verify_full: true,
            ..query
        };
        assert_eq!(
            relink_with(&verified, &[&in_gap, &copy, &shorter]),
            RelinkOutcome::Unique(RelinkMatch {
                candidate: 1,
                confidence: RelinkConfidence::Full
            })
        );
        assert_eq!(
            relink_with(&verified, &[&in_gap, &shorter, &window_changed]),
            RelinkOutcome::NotFound {
                examined: 3,
                length_mismatch: 1,
                content_mismatch: 2
            }
        );
        // Two identical copies are never silently resolved.
        let RelinkOutcome::Ambiguous(two) = relink_with(&verified, &[&data, &copy]) else {
            panic!("identical copies are ambiguous");
        };
        assert_eq!(two.len(), 2);
        assert!(two.iter().all(|m| m.confidence == RelinkConfidence::Full));
        // A whole-file identity of a short source is Full confidence without a second pass.
        let small_query = RelinkQuery {
            expected: small_id,
            expected_full: None,
            verify_full: false,
        };
        assert_eq!(
            relink_with(&small_query, &[&small]),
            RelinkOutcome::Unique(RelinkMatch {
                candidate: 0,
                confidence: RelinkConfidence::Full
            })
        );
        let mut empty: [&mut dyn ReadSeek; 0] = [];
        let needs_full = RelinkQuery {
            expected_full: None,
            verify_full: true,
            ..query
        };
        assert_eq!(
            relink(&needs_full, &mut empty, 4096, &cancel).unwrap_err(),
            ReelError::Unsupported {
                what: "relink-verify-needs-full-digest"
            }
        );
    });
}
