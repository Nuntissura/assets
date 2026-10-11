//! SMB-REEL-ENG-1: export is exact or it fails (every delivery site), a zero-duration edit-list entry,
//! raw `fiel`/`clap` sample-entry metadata, and one independent-writer oracle (an ffmpeg-written MOV).
//!
//! The partial-video-decode half of the export rule lives next to the fake decoder it needs
//! (`tests/video_route.rs`, `export_refuses_partial_video_decode`).
//!
//! Every container fixture here is built by this repository's own synthetic builders (`tests/common`),
//! except the MOV of the last test: it was written by ffmpeg, so a reading shared by the builder and the
//! reader cannot hide in it (`tests/fixtures/mov/provenance.json` records the generator and the oracle).
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_observe::{FailureCode, Outcome};
mod common;
use common::*;
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

/// Ticks per 1001/30000 s frame (29.97 fps), per 1/48000 s sample, per 1/30 s and 1/25 s frame, per ms.
const TPF_2997: u64 = 8_475_667_200;
const TPS_48K: u64 = 5_292_000;
const TPF_30: u64 = 8_467_200_000;
const TPF_25: u64 = 10_160_640_000;
const MS: u64 = 254_016_000;
/// Ticks per 1/30000 s media unit.
const UNIT_30000: u64 = 8_467_200;

#[derive(Default)]
struct Count {
    chunks: u32,
}

impl ChunkSink for Count {
    fn accept(&mut self, _: Chunk<'_>) -> Result<(), ReelError> {
        self.chunks += 1;
        Ok(())
    }
}

/// A sink that must never be reached: a refused export delivers nothing.
struct NeverReached;
impl ChunkSink for NeverReached {
    fn accept(&mut self, _: Chunk<'_>) -> Result<(), ReelError> {
        panic!("sink reached although the export must be refused first");
    }
}

fn index_of(bytes: &[u8]) -> Result<MediaIndex, ReelError> {
    build_index(
        &mut Cursor::new(bytes.to_vec()),
        bytes.len() as u64,
        &Limits::default(),
        &CancellationToken::default(),
    )
}

// ---- 1. export is exact or it fails -------------------------------------------------------------------

/// One delivery site of one provider: an aligned start, a start inside a unit, and the start of the unit
/// a preview delivers for it (`nearest`).
struct Case {
    name: &'static str,
    provider: &'static str,
    bytes: Vec<u8>,
    track: Option<u32>,
    output: OutputMode,
    count: u64,
    aligned: u64,
    unaligned: u64,
    nearest: u64,
}

fn run(
    registry: &Registry,
    case: &Case,
    start: u64,
    purpose: Purpose,
    sink: &mut dyn ChunkSink,
) -> Result<SegmentReceipt, ReelError> {
    let request = SegmentRequest {
        codec: case.provider.into(),
        start_pts: Ticks::new(start),
        count: case.count,
        track: case.track,
        output: case.output,
        prefer: SourcePreference::Original,
        purpose,
        grant: Grant {
            content_id: ContentId::parse("export-case").unwrap(),
            revision: 1,
            expected_len: case.bytes.len() as u64,
        },
        expected_revision: 1,
    };
    registry.decode_segment(
        &mut Cursor::new(case.bytes.clone()),
        None,
        &request,
        &Limits::default(),
        &CancellationToken::default(),
        sink,
    )
}

fn mkv_blocks() -> Vec<u8> {
    let video = |rel_ts, key| MkvBlock {
        track: 1,
        rel_ts,
        key,
        frames: vec![100],
        lacing: Lacing::None,
        group: None,
        raw_lace_header: None,
    };
    mkv_bytes(&MkvSpec {
        doc_type: "matroska",
        timecode_scale: 1_000_000,
        unknown_sizes: false,
        clusters: vec![MkvCluster {
            timecode: 0,
            blocks: vec![
                video(0, true),
                video(40, false),
                video(80, false),
                video(120, false),
            ],
        }],
        with_cues: true,
        video_default_duration_ns: Some(40_000_000),
        content_encoding_on_video: false,
    })
    .bytes
}

fn video_mov(edits: Option<Vec<(u64, i64)>>) -> Vec<u8> {
    mp4_bytes(&Mp4Spec {
        movie_timescale: 30000,
        media_timescale: 30000,
        codec: *b"avc1",
        width: 64,
        height: 36,
        samples: (0..6)
            .map(|_| Mp4Sample {
                duration: 1001,
                cts_offset: 0,
                size: 40,
                sync: true,
            })
            .collect(),
        edits,
        samples_per_chunk: 3,
        co64: false,
        ctts_version: 0,
        moov_first: true,
        colr_nclx: None,
        pasp: None,
    })
}

fn pcm_mov() -> Vec<u8> {
    mov_pcm_bytes(&PcmMovSpec {
        fourcc: *b"twos",
        kind: PcmKind::S16,
        big_endian: true,
        lpcm_v2: false,
        non_interleaved: false,
        channels: 2,
        rate: 48000,
        frames: 300,
        frames_per_chunk: 128,
        movie_timescale: 48000,
        edits: None,
        moov_first: true,
        sound_version: None,
    })
}

fn export_cases() -> Vec<Case> {
    vec![
        Case {
            name: "wav samples",
            provider: WAV_PROVIDER_ID,
            bytes: wav_pcm16(48000, 1, 480),
            track: None,
            output: OutputMode::Decoded,
            count: 10,
            aligned: 100 * TPS_48K,
            unaligned: 100 * TPS_48K + 1,
            nearest: 100 * TPS_48K,
        },
        Case {
            name: "y4m frames",
            provider: Y4M_PROVIDER_ID,
            bytes: y4m_bytes(8, 8, (30, 1), "420", 6),
            track: None,
            output: OutputMode::Decoded,
            count: 2,
            aligned: 2 * TPF_30,
            unaligned: 2 * TPF_30 + 1,
            nearest: 2 * TPF_30,
        },
        Case {
            name: "isobmff packets",
            provider: ISOBMFF_PROVIDER_ID,
            bytes: video_mov(None),
            track: None,
            output: OutputMode::Packets,
            count: 2,
            aligned: 2 * TPF_2997,
            unaligned: 2 * TPF_2997 + TPF_2997 / 2,
            nearest: 2 * TPF_2997,
        },
        // The edit list starts 500 units inside sample 0: the first presented sample is cut, so even
        // a request at its own presented start is not exact. The next sample is whole.
        Case {
            name: "isobmff packets, edit-trimmed first sample",
            provider: ISOBMFF_PROVIDER_ID,
            bytes: video_mov(Some(vec![(4 * 1001, 500)])),
            track: None,
            output: OutputMode::Packets,
            count: 2,
            aligned: 501 * UNIT_30000,
            unaligned: 0,
            nearest: 0,
        },
        Case {
            name: "isobmff pcm",
            provider: ISOBMFF_PROVIDER_ID,
            bytes: pcm_mov(),
            track: None,
            output: OutputMode::Decoded,
            count: 10,
            aligned: 100 * TPS_48K,
            unaligned: 100 * TPS_48K + 1,
            nearest: 100 * TPS_48K,
        },
        Case {
            name: "matroska packets",
            provider: MATROSKA_PROVIDER_ID,
            bytes: mkv_blocks(),
            track: Some(1),
            output: OutputMode::Packets,
            count: 2,
            aligned: 40 * MS,
            unaligned: 41 * MS,
            nearest: 40 * MS,
        },
        Case {
            name: "matroska pcm",
            provider: MATROSKA_PROVIDER_ID,
            bytes: include_bytes!("fixtures/mkv/pcm-s16le-48k-2ch.mkv").to_vec(),
            track: Some(1),
            output: OutputMode::Decoded,
            count: 100,
            aligned: 1000 * TPS_48K,
            unaligned: 1000 * TPS_48K + 1,
            nearest: 1000 * TPS_48K,
        },
        Case {
            name: "mxf packets",
            provider: MXF_PROVIDER_ID,
            bytes: include_bytes!("fixtures/mxf/mpeg2-pcm-op1a.mxf").to_vec(),
            track: Some(2),
            output: OutputMode::Packets,
            count: 2,
            aligned: 2 * TPF_25,
            unaligned: 2 * TPF_25 + TPF_25 / 2,
            nearest: 2 * TPF_25,
        },
    ]
}

/// STU-ARC-021 for export: `Purpose::Export` is exact or it fails, before the first payload byte reaches
/// the sink. One case per delivery site (isobmff packets, isobmff and Matroska PCM, Matroska packets, WAV,
/// Y4M, MXF); the test enumerates the registry's providers and fails when one has no case, so a provider
/// added later cannot skip the rule. `Purpose::Preview` keeps its `Approximate` receipt, and the
/// `nearest` of the refusal is the start of the unit that preview delivers.
#[test]
fn export_is_exact_or_error_for_every_provider() {
    bounded_case("export_is_exact_or_error_for_every_provider", || {
        let mut registry = Registry::standard();
        registry.register(Box::new(MxfProvider)).unwrap();
        let cases = export_cases();

        let ids: Vec<&str> = registry.descriptors().iter().map(|d| d.id).collect();
        assert!(ids.len() >= 5, "standard providers plus mxf: {ids:?}");
        for id in &ids {
            assert!(
                cases.iter().any(|c| c.provider == *id),
                "provider {id} has no export case: add one before registering a provider"
            );
        }
        for case in &cases {
            assert!(
                ids.contains(&case.provider),
                "{}: unknown provider",
                case.name
            );
        }

        for case in &cases {
            let name = case.name;
            // Preview: an aligned start is Exact, a start inside a unit is Approximate and delivers the
            // containing unit.
            let preview_aligned = run(
                &registry,
                case,
                case.aligned,
                Purpose::Preview,
                &mut Count::default(),
            )
            .unwrap_or_else(|e| panic!("{name}: preview aligned: {e}"));
            assert_eq!(preview_aligned.disposition, Disposition::Exact, "{name}");
            assert_eq!(preview_aligned.actual.start_pts, Ticks::new(case.aligned));
            let preview = run(
                &registry,
                case,
                case.unaligned,
                Purpose::Preview,
                &mut Count::default(),
            )
            .unwrap_or_else(|e| panic!("{name}: preview unaligned: {e}"));
            assert_eq!(preview.disposition, Disposition::Approximate, "{name}");
            assert!(preview.complete, "{name}");
            assert_eq!(
                (preview.requested.start_pts, preview.actual.start_pts),
                (Ticks::new(case.unaligned), Ticks::new(case.nearest)),
                "{name}: the unit a preview delivers is the `nearest` of the refusal"
            );

            // Export: the same aligned start is delivered Exact ...
            let mut sink = Count::default();
            let exact = run(&registry, case, case.aligned, Purpose::Export, &mut sink)
                .unwrap_or_else(|e| panic!("{name}: export aligned: {e}"));
            assert_eq!(exact.disposition, Disposition::Exact, "{name}");
            assert!(exact.complete && sink.chunks > 0, "{name}");
            assert_eq!(exact.actual, preview_aligned.actual, "{name}");

            // ... the start inside a unit is refused with the typed error, sink untouched.
            let error = run(
                &registry,
                case,
                case.unaligned,
                Purpose::Export,
                &mut NeverReached,
            )
            .unwrap_err();
            assert_eq!(
                error,
                ReelError::NotExact {
                    requested: Ticks::new(case.unaligned),
                    nearest: Ticks::new(case.nearest),
                },
                "{name}"
            );
            assert_eq!(error.code(), "MEDIA_NOT_EXACT", "{name}");
            assert_eq!(
                error.to_string(),
                format!(
                    "MEDIA_NOT_EXACT requested={} nearest={}",
                    case.unaligned, case.nearest
                ),
                "{name}"
            );
        }

        // The refusal is a loss of the export (nothing was written), not a media or limit error.
        let refused = ReelError::NotExact {
            requested: Ticks::new(1),
            nearest: Ticks::new(0),
        };
        assert_eq!(
            outcome_for(&Err(refused)),
            Outcome::Failure(FailureCode::Loss)
        );
        for code in ["MEDIA_NOT_EXACT", "MEDIA_INCOMPLETE"] {
            assert!(DESCRIPTOR.contains(code), "DESCRIPTOR lists {code}");
        }
    });
}

// ---- 3. zero-duration edit-list entries ---------------------------------------------------------------

fn frag_spec(edits: Option<Vec<(u64, i64)>>) -> FragSpec {
    // Display order inside each 4-sample fragment is decode indexes [0, 2, 3, 1] (B-frame style).
    let fragment: Vec<Mp4Sample> = [2002, 4004, 1001, 1001]
        .iter()
        .enumerate()
        .map(|(i, cts_offset)| Mp4Sample {
            duration: 1001,
            cts_offset: *cts_offset,
            size: 2000,
            sync: i == 0,
        })
        .collect();
    FragSpec {
        movie_timescale: 30000,
        media_timescale: 30000,
        codec: *b"avc1",
        width: 64,
        height: 36,
        fragments: vec![fragment; 3],
        tfdt: vec![true, true, false],
        default_base_is_moof: true,
        with_sidx: true,
        edits,
    }
}

fn four_sample_mov(edits: Option<Vec<(u64, i64)>>) -> Vec<u8> {
    mp4_bytes(&Mp4Spec {
        movie_timescale: 600,
        media_timescale: 600,
        codec: *b"avc1",
        width: 64,
        height: 36,
        samples: (0..4)
            .map(|k| Mp4Sample {
                duration: 20,
                cts_offset: 0,
                size: 16,
                sync: k == 0,
            })
            .collect(),
        edits,
        samples_per_chunk: 2,
        co64: false,
        ctts_version: 0,
        moov_first: true,
        colr_nclx: None,
        pasp: None,
    })
}

/// ISO/IEC 14496-12 lets the initial edit of a fragmented file carry `segment_duration` 0 (W3C bug 26066
/// quotes the 2012 text; the standard itself was not read). Such an edit is open-ended only as the LAST
/// edit of a FRAGMENTED file; an empty edit of no length is no segment; anywhere else a zero length is
/// `Unsupported` instead of a guess (a progressive file with a truncated edit list must not look valid).
#[test]
fn fragmented_zero_duration_edit() {
    bounded_case("fragmented_zero_duration_edit", || {
        let frag = |edits: Option<Vec<(u64, i64)>>| fmp4_bytes(&frag_spec(edits)).bytes;
        let full = 12 * 1001;
        let reference = index_of(&frag(Some(vec![(full, 2002)]))).unwrap();
        let reference = &reference.tracks[0];
        assert_eq!(reference.presented().len(), 12);
        assert!(reference.ticks_exact);

        // Open-ended last edit == the full-length edit: same presented samples, same seeks.
        let open = index_of(&frag(Some(vec![(0, 2002)]))).unwrap();
        assert!(open.fragmented);
        assert_eq!(
            open.tracks[0].edits,
            [EditSegment {
                presentation_start: 0,
                duration: i64::MAX,
                media_time: Some(2002),
            }]
        );
        // An empty edit of no length shifts nothing, first or last.
        let skipped = [
            ("zero-length empty edit first", vec![(0, -1), (full, 2002)]),
            ("zero-length empty edit last", vec![(full, 2002), (0, -1)]),
            ("open-ended edit", vec![(0, 2002)]),
            ("empty edit then open-ended edit", vec![(0, -1), (0, 2002)]),
        ];
        for (label, edits) in skipped {
            let index = index_of(&frag(Some(edits.clone()))).unwrap();
            let track = &index.tracks[0];
            assert_eq!(track.presented(), reference.presented(), "{label}");
            assert!(track.ticks_exact, "{label}");
            for ticks in [
                0,
                5 * TPF_2997,
                5 * TPF_2997 + 1,
                11 * TPF_2997,
                12 * TPF_2997,
            ] {
                assert_eq!(
                    track.seek(Ticks::new(ticks)),
                    reference.seek(Ticks::new(ticks)),
                    "{label} seek {ticks}"
                );
            }
            // The zero-length empty edit is not a segment of the edit list.
            assert!(
                track.edits.iter().all(|e| e.media_time.is_some()),
                "{label}: {:?}",
                track.edits
            );
        }

        // Anywhere else a zero length is not guessed.
        let refused = ReelError::Unsupported {
            what: "elst-zero-duration",
        };
        // ... not the last edit of a fragmented file ...
        assert_eq!(
            index_of(&frag(Some(vec![(0, 2002), (full, 2002)]))).unwrap_err(),
            refused
        );
        // ... and any non-empty zero-length edit of a progressive file.
        for edits in [
            vec![(0, 0)],
            vec![(0, 0), (80, 0)],
            vec![(80, 0), (0, 0)],
            vec![(0, -1), (0, 20)],
        ] {
            assert_eq!(
                index_of(&four_sample_mov(Some(edits.clone()))).unwrap_err(),
                refused,
                "progressive {edits:?}"
            );
        }
        // A zero-length empty edit in a progressive file is still no segment.
        let plain = index_of(&four_sample_mov(Some(vec![(80, 0)]))).unwrap();
        let skipped = index_of(&four_sample_mov(Some(vec![(0, -1), (80, 0)]))).unwrap();
        assert_eq!(plain.tracks[0].presented().len(), 4);
        assert_eq!(skipped.tracks[0].presented(), plain.tracks[0].presented());
        assert_eq!(skipped.tracks[0].edits, plain.tracks[0].edits);
        // A real empty edit still shifts the presentation (existing behaviour, unchanged).
        let shifted = index_of(&four_sample_mov(Some(vec![(40, -1), (80, 0)]))).unwrap();
        assert_eq!(shifted.tracks[0].edits.len(), 2);
        assert_eq!(shifted.tracks[0].presented()[0].pts, 40 * 423_360_000);
    });
}

// ---- 4. raw fiel / clap --------------------------------------------------------------------------------

fn clap(fields: [i32; 8]) -> Vec<u8> {
    fields.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// `fiel` and `clap` are stored as written, without interpretation (the meaning of `fiel` detail 9 and 14
/// is disputed, so reel makes no field-order claim; no division by a `clap` denominator happens), and a
/// damaged one never costs a track its index. Colour and aspect written before them are read as before.
#[test]
fn fiel_and_clap_are_raw_and_advisory() {
    bounded_case("fiel_and_clap_are_raw_and_advisory", || {
        let spec = Mp4Spec {
            movie_timescale: 600,
            media_timescale: 600,
            codec: *b"avc1",
            width: 64,
            height: 36,
            samples: (0..4)
                .map(|k| Mp4Sample {
                    duration: 20,
                    cts_offset: 0,
                    size: 16,
                    sync: k == 0,
                })
                .collect(),
            edits: None,
            samples_per_chunk: 2,
            co64: false,
            ctts_version: 0,
            moov_first: true,
            colr_nclx: Some((1, 1, 1, false)),
            pasp: Some((4, 3)),
        };
        let track_of = |children: &[u8]| {
            let index = index_of(&mp4_bytes_with_children(&spec, children)).unwrap();
            let track = index.tracks[0].clone();
            // Whatever the children hold, the track is indexed and its other facts are unchanged.
            assert_eq!(track.samples().len(), 4);
            assert_eq!(track.presented().len(), 4);
            assert_eq!(track.pixel_aspect, Some((4, 3)));
            assert_eq!((track.width, track.height), (64, 36));
            track
        };

        // Absent: nothing is invented.
        let none = track_of(&[]);
        assert_eq!((none.field_info, none.clean_aperture), (None, None));

        // Present: raw values, including a direction nobody should rely on (2, 14), a value outside
        // the table (255, 0), a zero denominator, negative offsets and the i32 extremes.
        for (fields, detail) in [(2, 14), (2, 9), (1, 0), (255, 0)] {
            let track = track_of(&visual_child(b"fiel", &[fields, detail]));
            assert_eq!(
                track.field_info,
                Some(FieldInfo { fields, detail }),
                "{fields} {detail}"
            );
            assert_eq!(track.clean_aperture, None);
        }
        let values = [1920, 1, 1080, 1, -4, 2, 3, 0];
        let track = track_of(&[visual_child(b"clap", &clap(values))].concat());
        assert_eq!(
            track.clean_aperture,
            Some(CleanAperture {
                width: (1920, 1),
                height: (1080, 1),
                horiz_off: (-4, 2),
                vert_off: (3, 0),
            })
        );
        let extremes = [i32::MIN, i32::MAX, 0, -1, i32::MAX, i32::MIN, -1, 0];
        let track = track_of(&visual_child(b"clap", &clap(extremes)));
        assert_eq!(
            track.clean_aperture,
            Some(CleanAperture {
                width: (i32::MIN, i32::MAX),
                height: (0, -1),
                horiz_off: (i32::MAX, i32::MIN),
                vert_off: (-1, 0),
            })
        );
        // Both together, and the colour written before them is still read.
        let both = track_of(
            &[
                visual_child(b"fiel", &[2, 14]),
                visual_child(b"clap", &clap(values)),
            ]
            .concat(),
        );
        assert_eq!(
            both.field_info,
            Some(FieldInfo {
                fields: 2,
                detail: 14
            })
        );
        assert!(both.clean_aperture.is_some());
        assert_eq!(
            both.color,
            ColorTag::Nclx {
                primaries: 1,
                transfer: 1,
                matrix: 1,
                full_range: Some(false),
            }
        );
        // Longer payloads than the layout: the layout's bytes are read, the rest is ignored.
        let longer = track_of(
            &[
                visual_child(b"fiel", &[2, 9, 0xAA, 0xBB]),
                visual_child(b"clap", &[clap(values), vec![0xEE; 8]].concat()),
            ]
            .concat(),
        );
        assert_eq!(
            longer.field_info,
            Some(FieldInfo {
                fields: 2,
                detail: 9
            })
        );
        assert_eq!(longer.clean_aperture.map(|c| c.width), Some((1920, 1)));

        // Truncated children leave that field None and the scan continues with the next child.
        let mut short_clap = clap(values);
        short_clap.truncate(24);
        let cut = track_of(
            &[
                visual_child(b"clap", &short_clap),
                visual_child(b"fiel", &[2, 9]),
            ]
            .concat(),
        );
        assert_eq!(cut.clean_aperture, None, "a 24 byte clap is not a clap");
        assert_eq!(
            cut.field_info,
            Some(FieldInfo {
                fields: 2,
                detail: 9
            })
        );
        let cut = track_of(
            &[
                visual_child(b"fiel", &[2]),
                visual_child(b"clap", &clap(values)),
            ]
            .concat(),
        );
        assert_eq!(cut.field_info, None, "a 1 byte fiel is not a fiel");
        assert!(cut.clean_aperture.is_some());
        let empty = track_of(&[visual_child(b"fiel", &[]), visual_child(b"clap", &[])].concat());
        assert_eq!((empty.field_info, empty.clean_aperture), (None, None));

        // A child whose declared size runs past the entry ends the scan; the track stays indexable.
        let mut lying = u32::MAX.to_be_bytes().to_vec();
        lying.extend_from_slice(b"clap");
        lying.extend_from_slice(&clap(values));
        let hostile = track_of(&[visual_child(b"fiel", &[2, 9]), lying].concat());
        assert_eq!(
            hostile.field_info,
            Some(FieldInfo {
                fields: 2,
                detail: 9
            })
        );
        assert_eq!(hostile.clean_aperture, None);
        let mut lying = 16u32.to_be_bytes().to_vec();
        lying.extend_from_slice(b"fiel");
        let hostile = track_of(&lying);
        assert_eq!((hostile.field_info, hostile.clean_aperture), (None, None));
    });
}

// ---- 5. an independent writer ---------------------------------------------------------------------------

const FFMPEG_MOV: &[u8] = include_bytes!("fixtures/mov/mov-h264-bf2-negcts-tc-df-64x36.mov");

/// Decode-order composition times (units of 1/30000 s) as `ffprobe -show_entries packet=pts` prints them
/// for `mov-h264-bf2-negcts-tc-df-64x36.mov` (`tests/fixtures/mov/provenance.json`). ffprobe's own `dts`
/// column is shifted by -1001 and is not used.
const FFPROBE_PTS: [i64; 24] = [
    0, 3003, 1001, 2002, 6006, 4004, 5005, 9009, 7007, 8008, 11011, 10010, 12012, 15015, 13013,
    14014, 18018, 16016, 17017, 21021, 19019, 20020, 23023, 22022,
];

/// Every other container fixture comes from this repository's own builder, so a misreading shared by the
/// builder and the reader would pass. This one was written by ffmpeg/libx264 (B-frames, version 1 `ctts`
/// with negative offsets, an edit list, a drop-frame `tmcd` track) and judged by ffprobe: the expected
/// values below are ffprobe's, or derived by hand (the timecode sample).
#[test]
fn ffmpeg_made_mov_matches_independent_oracle() {
    bounded_case("ffmpeg_made_mov_matches_independent_oracle", || {
        let index = index_of(FFMPEG_MOV).unwrap();
        assert_eq!(index.movie_timescale, 1000);
        assert_eq!(index.tracks.len(), 2);
        let video = index.track(1).unwrap();
        assert_eq!(
            (video.kind, video.codec, video.width, video.height),
            (TrackKind::Video, *b"avc1", 64, 36)
        );
        assert_eq!(video.media_timescale, 30000);
        assert!(video.ticks_exact);
        assert_eq!(video.timecode_ref, Some(2));
        assert_eq!(index.track(2).unwrap().kind, TrackKind::Timecode);

        // Decode-order composition times: dts is the 1001 grid, pts = dts + ctts (version 1, negative).
        let samples = video.samples();
        assert_eq!(samples.len(), 24);
        assert_eq!(
            samples.iter().map(|s| s.pts).collect::<Vec<_>>(),
            FFPROBE_PTS
        );
        for (k, s) in samples.iter().enumerate() {
            assert_eq!(s.dts, k as i64 * 1001, "dts of sample {k}");
            assert_eq!(s.duration, 1001);
        }
        assert!(
            samples.iter().any(|s| s.pts - s.dts == -1001),
            "a negative composition offset exists"
        );
        assert_eq!(
            video.sync_samples(),
            &[0, 12],
            "ffprobe flags K at packets 0 and 12"
        );

        // One edit of 801 ms from media time 0; every sample is presented, in composition order, on the
        // exact 1001/30000 s grid.
        assert_eq!(
            video.edits,
            [EditSegment {
                presentation_start: 0,
                duration: 801 * MS as i64,
                media_time: Some(0),
            }]
        );
        let presented = video.presented();
        assert_eq!(presented.len(), 24);
        for (n, p) in presented.iter().enumerate() {
            assert_eq!(p.pts, (n as u64 * TPF_2997) as i64, "presented frame {n}");
            assert_eq!(p.duration, TPF_2997);
            assert_eq!(FFPROBE_PTS[p.sample as usize], n as i64 * 1001);
        }
        // Random access: frame 14 (composition 14014) is decoded from the sync sample 12.
        let seek = video.seek(Ticks::new(14 * TPF_2997)).unwrap();
        assert_eq!(seek.disposition, SeekDisposition::Exact);
        assert_eq!(FFPROBE_PTS[seek.target.sample as usize], 14014);
        assert_eq!(seek.decode_from.sample, 12);

        // Timecode: drop frame, 30000/1001, 30 nominal fps, start 00:59:59;29 = frame 107891
        // (1800 * 59 + 30 * 59 + 29 - 2 * (59 - 59 / 10) = 107891; ffprobe -show_data prints 0001a573).
        let start = read_timecode_start(
            &mut Cursor::new(FFMPEG_MOV.to_vec()),
            &index,
            1,
            &Limits::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(start.track_id, 2);
        assert_eq!(start.raw_sample, 107_891);
        assert_eq!(start.frame_number, 107_891);
        assert_eq!(start.pts, 0);
        assert_eq!(
            start.description,
            TimecodeDescription {
                drop_frame: true,
                wraps_24h: false,
                negative_ok: false,
                counter: false,
                timescale: 30000,
                frame_duration: 1001,
                frames_per_second: 30,
            }
        );
    });
}
