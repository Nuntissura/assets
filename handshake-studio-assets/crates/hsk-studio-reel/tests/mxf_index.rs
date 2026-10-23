//! MXF OP1a / OP-Atom index reader (SMPTE ST 377-1): partitions, Header Metadata tracks and descriptors,
//! constant and variable edit unit index tables, frame-accurate byte offsets, essence elements as packets.
//!
//! Fixtures (`tests/fixtures/mxf/`, generated once offline by `generate-mxf.sh`, see
//! `tests/fixtures/README.md`):
//! * `mpeg2-pcm-op1a.mxf`: OP1a, frame wrapped MPEG-2 long GOP (display order differs from coded order)
//!   interleaved with PCM, variable edit unit size, two slices, delta entries, temporal offsets.
//! * `pcm-opatom.mxf`: OP-Atom, clip wrapped PCM, constant edit unit size (2 bytes), no index entries.
//!
//! Oracles: `ffprobe -show_packets` positions, sizes, timestamps and key flags (libavformat's reading of
//! the same files), and a second parse written in Python straight from ST 377-1 for the index entries.
//! Expected values below are those oracles' output, not the reader's.
use hsk_studio_accord::{CancellationToken, Ticks};
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
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost 120s deadline: {e}"),
    }
}

const OP1A: &[u8] = include_bytes!("fixtures/mxf/mpeg2-pcm-op1a.mxf");
const ATOM: &[u8] = include_bytes!("fixtures/mxf/pcm-opatom.mxf");

fn index_of(bytes: &[u8]) -> Result<MxfIndex, ReelError> {
    build_mxf_index(
        &mut Cursor::new(bytes.to_vec()),
        bytes.len() as u64,
        &Limits::default(),
        &CancellationToken::default(),
    )
}

#[derive(Default)]
struct Collect {
    /// (pts, index, units, sync, preroll, bytes)
    chunks: Vec<(i64, u64, u64, bool, bool, Vec<u8>)>,
}

impl ChunkSink for Collect {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
        assert_eq!(c.kind, ChunkKind::Packet);
        self.chunks
            .push((c.pts, c.index, c.units, c.sync, c.preroll, c.bytes.to_vec()));
        Ok(())
    }
}

struct NeverReached;
impl ChunkSink for NeverReached {
    fn accept(&mut self, _: Chunk<'_>) -> Result<(), ReelError> {
        panic!("the sink must not be reached");
    }
}

fn request(bytes: &[u8], track: u32, start: u64, count: u64, output: OutputMode) -> SegmentRequest {
    SegmentRequest {
        codec: MXF_PROVIDER_ID.into(),
        start_pts: Ticks::new(start),
        count,
        track: Some(track),
        output,
        prefer: SourcePreference::Original,
        purpose: Purpose::Preview,
        grant: Grant {
            content_id: ContentId::parse("mxf-fixture").unwrap(),
            revision: 1,
            expected_len: bytes.len() as u64,
        },
        expected_revision: 1,
    }
}

fn deliver(
    bytes: &[u8],
    req: &SegmentRequest,
    limits: &Limits,
    sink: &mut dyn ChunkSink,
) -> Result<SegmentReceipt, ReelError> {
    MxfProvider.decode_segment(
        &mut Cursor::new(bytes.to_vec()),
        req,
        limits,
        &CancellationToken::default(),
        sink,
    )
}

/// Value bytes of the KLV whose key starts at `pos` (BER parsed here, independently of the reader).
fn klv_value(bytes: &[u8], pos: u64) -> &[u8] {
    let at = pos as usize + 16;
    let (len, used) = if bytes[at] < 0x80 {
        (u64::from(bytes[at]), 1)
    } else {
        let n = (bytes[at] & 0x7F) as usize;
        let len = bytes[at + 1..at + 1 + n]
            .iter()
            .fold(0u64, |a, b| (a << 8) | u64::from(*b));
        (len, 1 + n)
    };
    &bytes[at + used..at + used + len as usize]
}

fn patched(bytes: &[u8], pattern: &[u8], replacement: &[u8]) -> Vec<u8> {
    assert_eq!(pattern.len(), replacement.len());
    let hits: Vec<usize> = bytes
        .windows(pattern.len())
        .enumerate()
        .filter(|(_, w)| *w == pattern)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(hits.len(), 1, "pattern {pattern:02x?} must occur once");
    let mut out = bytes.to_vec();
    out[hits[0]..hits[0] + pattern.len()].copy_from_slice(replacement);
    out
}

const TICKS_PER_FRAME_25: u64 = 10_160_640_000; // 254_016_000_000 / 25
const TICKS_PER_SAMPLE_48K: u64 = 5_292_000; // 254_016_000_000 / 48_000
const PICTURE_TRACK: u32 = 2;
const SOUND_TRACK: u32 = 3;
/// ffprobe `-show_packets` of the picture stream in decode (coded) order: (pts, size, pos, key flag).
const PICTURE: [(u64, u64, u64, bool); 10] = [
    (0, 4558, 7168, true),
    (3, 4443, 20480, false),
    (1, 1884, 33792, false),
    (2, 1768, 44544, false),
    (6, 5745, 55296, true),
    (4, 1894, 70144, false),
    (5, 2067, 80896, false),
    (9, 4179, 92160, false),
    (7, 1653, 105472, false),
    (8, 1664, 116224, false),
];
/// ffprobe positions of the ten 7680-byte sound packets (display order = stored order).
const SOUND_POS: [u64; 10] = [
    11776, 25088, 35840, 46592, 61440, 72192, 83456, 96768, 107520, 118272,
];

/// Picture element key position for DISPLAY position `d` (the coded packet whose pts is `d`).
fn picture_pos(d: u64) -> (u64, u64, usize) {
    let coded = PICTURE.iter().position(|p| p.0 == d).unwrap();
    (PICTURE[coded].2, PICTURE[coded].1, coded)
}

#[test]
fn mxf_op1a_variable_index_matches_ffprobe_positions_and_decodes_order() {
    bounded_case("mxf_op1a_variable_index", || {
        let index = index_of(OP1A).unwrap();
        assert!(index.header_metadata_complete && !index.truncated_tail && index.run_in == 0);
        assert_eq!(
            index.operational_pattern,
            [6, 14, 43, 52, 4, 1, 1, 1, 13, 1, 2, 1, 1, 1, 9, 0]
        );

        // Partitions: header (metadata), body (essence, BodySID 1), footer (index, IndexSID 2).
        let shape: Vec<_> = index
            .partitions
            .iter()
            .map(|p| {
                (
                    p.kind,
                    p.offset,
                    p.body_sid,
                    p.index_sid,
                    p.header_byte_count,
                    p.index_byte_count,
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                (MxfPartitionKind::Header, 0, 0, 0, 5632, 0),
                (MxfPartitionKind::Body, 6144, 1, 0, 0, 0),
                (MxfPartitionKind::Footer, 126_464, 0, 2, 0, 512),
            ]
        );
        let body = &index.partitions[1];
        assert_eq!(
            (body.essence_start, body.end, body.kag_size),
            (Some(6656), 126_464, 512)
        );
        assert!(index.partitions.iter().all(|p| p.status == 4));
        assert_eq!(index.partitions[2].footer_partition, 126_464);

        // Tracks: file package picture (id 2) and sound (id 3); descriptors agree with ffprobe's streams
        // (128x72 25 fps; pcm_s16le 48000 Hz 2 ch).
        let picture = index.essence_track(PICTURE_TRACK).unwrap();
        assert_eq!(
            (picture.kind, picture.track_number, picture.edit_rate),
            (MxfTrackKind::Picture, 0x1501_0500, Some((25, 1)))
        );
        assert_eq!(
            (picture.body_sid, picture.index_sid, picture.duration),
            (Some(1), Some(2), Some(10))
        );
        let d = picture.descriptor.as_ref().unwrap();
        assert_eq!((d.sampled_width, d.sampled_height), (Some(128), Some(72)));
        assert_eq!((d.stored_width, d.stored_height), (Some(128), Some(80)));
        assert_eq!(
            (d.aspect_ratio, d.sample_rate, d.component_depth),
            (Some((16, 9)), Some((25, 1)), Some(8))
        );
        assert_eq!(
            (d.horizontal_subsampling, d.vertical_subsampling),
            (Some(2), Some(1))
        );
        let sound = index.essence_track(SOUND_TRACK).unwrap();
        assert_eq!(
            (sound.kind, sound.track_number),
            (MxfTrackKind::Sound, 0x1601_0300)
        );
        let s = sound.descriptor.as_ref().unwrap();
        assert_eq!(
            (
                s.audio_sampling_rate,
                s.channel_count,
                s.quantization_bits,
                s.block_align
            ),
            (Some((48000, 1)), Some(2), Some(16), Some(4))
        );
        assert_eq!(index.default_track().unwrap().track_id, PICTURE_TRACK);
        // The material package lists the same tracks without essence numbers.
        assert_eq!(
            index
                .tracks
                .iter()
                .filter(|t| t.package_kind == MxfPackageKind::Material)
                .map(|t| (t.kind, t.track_number))
                .collect::<Vec<_>>(),
            [
                (MxfTrackKind::Timecode, 0),
                (MxfTrackKind::Picture, 0),
                (MxfTrackKind::Sound, 0)
            ]
        );

        // Index table: values of the Python parse of the same footer segment.
        let table = index.index_table(2).unwrap();
        assert_eq!(
            (table.body_sid, table.edit_rate, table.slice_count),
            (1, (25, 1), 1)
        );
        assert_eq!(
            table.delta_entries,
            [
                MxfDeltaEntry {
                    pos_table_index: 0,
                    slice: 0,
                    element_delta: 0
                },
                MxfDeltaEntry {
                    pos_table_index: -1,
                    slice: 0,
                    element_delta: 512
                },
                MxfDeltaEntry {
                    pos_table_index: 0,
                    slice: 1,
                    element_delta: 0
                },
            ]
        );
        let seg = &table.segments[0];
        assert_eq!(
            (seg.start_position, seg.duration, seg.edit_unit_byte_count),
            (0, 10, 0)
        );
        let columns = |f: fn(&MxfIndexEntry) -> i64| seg.entries.iter().map(f).collect::<Vec<_>>();
        assert_eq!(
            columns(|e| i64::from(e.temporal_offset)),
            [0, 1, 1, -2, 1, 1, -2, 1, 1, -2]
        );
        assert_eq!(
            columns(|e| i64::from(e.key_frame_offset)),
            [0, -1, -2, -3, 0, -5, -6, -3, -4, -5]
        );
        assert_eq!(
            columns(|e| i64::from(e.flags)),
            [192, 34, 51, 51, 64, 51, 51, 34, 51, 51]
        );
        assert_eq!(
            columns(|e| e.stream_offset as i64),
            [
                0, 13312, 26624, 37376, 48128, 62976, 73728, 84992, 98304, 109056
            ]
        );
        assert_eq!(
            seg.slice_offsets,
            [5120, 5120, 2560, 2560, 6656, 2560, 3072, 5120, 2560, 2560]
        );
        assert_eq!(table.indexed_units(), Some(10));

        // Frame-accurate offsets against ffprobe: temporal reordering, delta entries and slices.
        let mut file = Cursor::new(OP1A.to_vec());
        for d in 0..10u64 {
            let (pos, size, coded) = picture_pos(d);
            let unit = index.locate(2, d).unwrap();
            assert_eq!(unit.coded_position, coded as u64, "display {d}");
            assert_eq!(unit.wrapping, MxfWrapping::Frame);
            // Key frame offset 0 is exactly ffprobe's key flag; the random access bit is only on coded 0
            // (every later I picture is open GOP).
            assert_eq!(unit.key_frame_offset == 0, PICTURE[coded].3, "display {d}");
            assert_eq!(unit.random_access(), coded == 0, "display {d}");
            // Spec 11.4: entry stream offset + slice offset + element delta.
            assert_eq!(index.element_offset(2, d, 1).unwrap(), pos, "picture {d}");
            assert_eq!(
                index.element_offset(2, d, 2).unwrap(),
                SOUND_POS[d as usize],
                "sound {d}"
            );
            // The same elements found by inspecting the unit's KLV keys.
            let element = index
                .edit_unit_element(&mut file, &unit, picture.track_number, &Limits::default())
                .unwrap();
            assert_eq!(
                (element.key_offset, element.value_len),
                (pos, size),
                "picture {d}"
            );
            // Sound is not reordered: its unit is the display position itself.
            let own = index.locate_coded(2, d).unwrap();
            let sound_el = index
                .edit_unit_element(&mut file, &own, sound.track_number, &Limits::default())
                .unwrap();
            assert_eq!(
                (sound_el.key_offset, sound_el.value_len),
                (SOUND_POS[d as usize], 7680),
                "sound {d}"
            );
            assert_eq!(index.display_position_of(2, coded as u64).unwrap(), d);
        }
        // Past the index: typed.
        assert!(matches!(
            index.locate(2, 10),
            Err(ReelError::OutOfRange { .. })
        ));
        assert!(index.locate(99, 0).is_err());

        // Provider: packets of display positions 3..5 in decode order from the random access point:
        // coded 0..=5, of which coded 1 (display 3) and 5 (display 4) are requested.
        let mut sink = Collect::default();
        let req = request(
            OP1A,
            PICTURE_TRACK,
            3 * TICKS_PER_FRAME_25,
            2,
            OutputMode::Packets,
        );
        let receipt = deliver(OP1A, &req, &Limits::default(), &mut sink).unwrap();
        assert_eq!(sink.chunks.len(), 6);
        for (k, chunk) in sink.chunks.iter().enumerate() {
            let (pts, size, pos, _) = PICTURE[k];
            assert_eq!(chunk.1, k as u64, "coded index");
            assert_eq!(chunk.0 as u64, pts * TICKS_PER_FRAME_25, "pts of coded {k}");
            assert_eq!(chunk.5.len() as u64, size, "size of coded {k}");
            assert_eq!(
                chunk.5.as_slice(),
                klv_value(OP1A, pos),
                "bytes of coded {k}"
            );
            assert_eq!(chunk.4, !(k == 1 || k == 5), "preroll flag of coded {k}");
            assert_eq!(chunk.3, k == 0, "random access of coded {k}");
        }
        assert_eq!(receipt.disposition, Disposition::Exact);
        assert!(receipt.complete && receipt.ticks_exact);
        assert_eq!((receipt.actual.units, receipt.chunks), (2, 6));
        assert_eq!(receipt.actual.start_pts, Ticks::new(3 * TICKS_PER_FRAME_25));
        assert_eq!(receipt.actual.end_pts, Ticks::new(5 * TICKS_PER_FRAME_25));
        // A start inside a frame is the containing frame, reported as approximate.
        let req = request(
            OP1A,
            PICTURE_TRACK,
            3 * TICKS_PER_FRAME_25 + 1,
            1,
            OutputMode::Packets,
        );
        assert_eq!(
            deliver(OP1A, &req, &Limits::default(), &mut Collect::default())
                .unwrap()
                .disposition,
            Disposition::Approximate
        );
        // Sound packets: not reordered, every unit independent.
        let mut sink = Collect::default();
        let req = request(
            OP1A,
            SOUND_TRACK,
            2 * TICKS_PER_FRAME_25,
            3,
            OutputMode::Packets,
        );
        deliver(OP1A, &req, &Limits::default(), &mut sink).unwrap();
        assert_eq!(sink.chunks.len(), 3);
        for (k, chunk) in sink.chunks.iter().enumerate() {
            let d = 2 + k;
            assert_eq!(chunk.1, d as u64);
            assert_eq!(chunk.5.as_slice(), klv_value(OP1A, SOUND_POS[d]));
            assert!(chunk.3 && !chunk.4);
            assert_eq!(chunk.0 as u64, d as u64 * TICKS_PER_FRAME_25);
        }
        // Past the end of the track: typed, never clamped.
        let req = request(
            OP1A,
            PICTURE_TRACK,
            9 * TICKS_PER_FRAME_25,
            2,
            OutputMode::Packets,
        );
        assert_eq!(
            deliver(OP1A, &req, &Limits::default(), &mut NeverReached)
                .unwrap_err()
                .code(),
            "MEDIA_OUT_OF_RANGE"
        );
        // Essence stays opaque: a decoded request names the coding and decodes nothing.
        let req = request(OP1A, PICTURE_TRACK, 0, 1, OutputMode::Decoded);
        let err = deliver(OP1A, &req, &Limits::default(), &mut NeverReached).unwrap_err();
        assert!(
            matches!(&err, ReelError::CodecUnavailable { codec } if codec.starts_with("mxf:06.0e.2b.34.04.01.01.")),
            "{err:?}"
        );

        // probe + registry detection (MXF is opt-in: registered by the caller).
        let provider = MxfProvider;
        let layout = provider
            .probe(
                &mut Cursor::new(OP1A.to_vec()),
                &Limits::default(),
                &CancellationToken::default(),
            )
            .unwrap();
        assert_eq!(
            (
                layout.kind,
                layout.width,
                layout.height,
                layout.units,
                layout.ticks_per_unit,
                layout.tracks
            ),
            (MediaKind::Video, 128, 72, 10, TICKS_PER_FRAME_25, 2)
        );
        let mut registry = Registry::standard();
        assert!(registry.detect(&mut Cursor::new(OP1A.to_vec())).is_err());
        registry.register(Box::new(MxfProvider)).unwrap();
        assert_eq!(
            registry
                .detect(&mut Cursor::new(OP1A.to_vec()))
                .unwrap()
                .descriptor()
                .id,
            MXF_PROVIDER_ID
        );
    });
}

#[test]
fn mxf_constant_size_clip_wrapped_offsets_match_ffprobe_and_multi_partition_synthetic_file() {
    bounded_case("mxf_constant_size", || {
        let index = index_of(ATOM).unwrap();
        assert_eq!(index.operational_pattern[12], 0x10, "OP-Atom label");
        let kinds: Vec<_> = index
            .partitions
            .iter()
            .map(|p| (p.kind, p.offset))
            .collect();
        assert_eq!(
            kinds,
            [
                (MxfPartitionKind::Header, 0),
                (MxfPartitionKind::Body, 5120),
                (MxfPartitionKind::Footer, 17408)
            ]
        );
        let body = &index.partitions[1];
        assert_eq!(body.essence_start, Some(5632));
        // The single clip element: 25-byte KL (9-byte BER), 11520 bytes = 5760 samples x 2.
        let klv = body.essence_first_klv.unwrap();
        assert_eq!((klv.header_len, klv.value_len), (25, 11520));

        let track = index.essence_track(2).unwrap();
        assert_eq!(
            (track.kind, track.track_number),
            (MxfTrackKind::Sound, 0x1601_0100)
        );
        assert_eq!(track.edit_rate, Some((48000, 1)));
        let d = track.descriptor.as_ref().unwrap();
        assert_eq!(
            (
                d.set_kind,
                d.sample_rate,
                d.container_duration,
                d.channel_count,
                d.quantization_bits
            ),
            (0x48, Some((48000, 1)), Some(5760), Some(1), Some(16))
        );
        let table = index.index_table(2).unwrap();
        assert_eq!((table.body_sid, table.edit_rate), (1, (48000, 1)));
        let seg = &table.segments[0];
        assert_eq!(
            (
                seg.edit_unit_byte_count,
                seg.duration,
                seg.entries.len(),
                table.indexed_units()
            ),
            (2, 0, 0, None)
        );
        assert_eq!(index.wrapping_of(table), MxfWrapping::Clip);

        // ffprobe packet positions: 5657, 9497, 13337 are samples 0, 1920, 3840 (3840 bytes each).
        for (sample, pos) in [(0u64, 5657u64), (1920, 9497), (3840, 13337)] {
            let unit = index.locate(2, sample).unwrap();
            assert_eq!(
                (unit.file_offset, unit.stream_offset),
                (pos, sample * 2),
                "sample {sample}"
            );
            assert_eq!(unit.edit_unit_byte_count, Some(2));
            assert!(unit.random_access());
        }
        // The last sample is the last two bytes of the clip; one past it is outside the clip element.
        assert_eq!(index.locate(2, 5759).unwrap().file_offset, 5657 + 5759 * 2);
        assert!(index.locate(2, 5760).is_err());

        // Packets of whole edit units: one slice of the clip, bytes equal to the file.
        let mut sink = Collect::default();
        let req = request(
            ATOM,
            2,
            1920 * TICKS_PER_SAMPLE_48K,
            3840,
            OutputMode::Packets,
        );
        let receipt = deliver(ATOM, &req, &Limits::default(), &mut sink).unwrap();
        assert_eq!(receipt.disposition, Disposition::Exact);
        assert_eq!(sink.chunks.len(), 1);
        assert_eq!(sink.chunks[0].2, 3840);
        assert_eq!(sink.chunks[0].5.as_slice(), &ATOM[9497..9497 + 7680]);
        assert_eq!(receipt.actual.units, 3840);
        // A small chunk limit splits the range into slices of whole edit units.
        let small = Limits {
            max_chunk_bytes: 1000,
            ..Limits::default()
        };
        let mut sink = Collect::default();
        deliver(
            ATOM,
            &request(ATOM, 2, 0, 1920, OutputMode::Packets),
            &small,
            &mut sink,
        )
        .unwrap();
        let lens: Vec<usize> = sink.chunks.iter().map(|c| c.5.len()).collect();
        assert_eq!(lens, [1000, 1000, 1000, 840]);
        let all: Vec<u8> = sink.chunks.iter().flat_map(|c| c.5.clone()).collect();
        assert_eq!(all.as_slice(), &ATOM[5657..5657 + 3840]);
        // Beyond the 5760 samples of the track: typed.
        let err = deliver(
            ATOM,
            &request(
                ATOM,
                2,
                5000 * TICKS_PER_SAMPLE_48K,
                1000,
                OutputMode::Packets,
            ),
            &Limits::default(),
            &mut NeverReached,
        )
        .unwrap_err();
        assert_eq!(err.code(), "MEDIA_OUT_OF_RANGE");
        let err = deliver(
            ATOM,
            &request(ATOM, 2, 0, 1, OutputMode::Decoded),
            &Limits::default(),
            &mut NeverReached,
        )
        .unwrap_err();
        assert!(matches!(err, ReelError::CodecUnavailable { .. }));

        // Synthetic frame wrapped file, constant edit unit size, essence split over two body partitions
        // of the same BodySID (BodyOffset 360 for the second) and one footer index segment, laid out
        // from ST 377-1 7.1 / 11.2.3. No ffprobe oracle: the expected offsets are computed from the
        // layout below, which this test builds independently of the reader.
        const UNIT: usize = 100;
        let ber = |n: usize| vec![0x83, (n >> 16) as u8, (n >> 8) as u8, n as u8];
        let klv = |key: [u8; 16], value: &[u8]| {
            let mut out = key.to_vec();
            out.extend(ber(value.len()));
            out.extend_from_slice(value);
            out
        };
        let pack_key = |kind: u8| [6u8, 14, 43, 52, 2, 5, 1, 1, 13, 1, 2, 1, 1, kind, 4, 0];
        let pack = |kind: u8,
                    this: u64,
                    prev: u64,
                    footer: u64,
                    ibc: u64,
                    isid: u32,
                    boff: u64,
                    bsid: u32| {
            let mut v = Vec::new();
            v.extend_from_slice(&1u16.to_be_bytes());
            v.extend_from_slice(&3u16.to_be_bytes());
            v.extend_from_slice(&1u32.to_be_bytes()); // KAG: no grid
            for n in [this, prev, footer, 0, ibc] {
                v.extend_from_slice(&n.to_be_bytes());
            }
            v.extend_from_slice(&isid.to_be_bytes());
            v.extend_from_slice(&boff.to_be_bytes());
            v.extend_from_slice(&bsid.to_be_bytes());
            v.extend_from_slice(&[6, 14, 43, 52, 4, 1, 1, 1, 13, 1, 2, 1, 1, 1, 9, 0]);
            v.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 16]); // empty essence container batch
            klv(pack_key(kind), &v)
        };
        let element = |n: usize| {
            klv(
                [6, 14, 43, 52, 1, 2, 1, 1, 13, 1, 3, 1, 0x15, 1, 5, 0],
                &[n as u8; UNIT],
            )
        };
        let unit_bytes = element(0).len(); // 16 + 4 + 100
        let pack_len = pack(2, 0, 0, 0, 0, 0, 0, 0).len();
        let b1 = pack_len;
        let b2 = b1 + pack_len + 3 * unit_bytes;
        let footer = b2 + pack_len + 2 * unit_bytes;
        let mut index_set = Vec::new();
        for (tag, value) in [
            (
                0x3F0Bu16,
                [25u32.to_be_bytes(), 1u32.to_be_bytes()].concat(),
            ),
            (0x3F0C, 0u64.to_be_bytes().to_vec()),
            (0x3F0D, 5u64.to_be_bytes().to_vec()),
            (0x3F05, (unit_bytes as u32).to_be_bytes().to_vec()),
            (0x3F06, 2u32.to_be_bytes().to_vec()),
            (0x3F07, 1u32.to_be_bytes().to_vec()),
            (0x3F08, vec![0]),
        ] {
            index_set.extend_from_slice(&tag.to_be_bytes());
            index_set.extend_from_slice(&(value.len() as u16).to_be_bytes());
            index_set.extend_from_slice(&value);
        }
        let index_klv = klv(
            [6, 14, 43, 52, 2, 0x53, 1, 1, 13, 1, 2, 1, 1, 0x10, 1, 0],
            &index_set,
        );
        let mut file = pack(2, 0, 0, footer as u64, 0, 0, 0, 0);
        file.extend(pack(3, b1 as u64, 0, footer as u64, 0, 0, 0, 1));
        (0..3).for_each(|n| file.extend(element(n)));
        file.extend(pack(
            3,
            b2 as u64,
            b1 as u64,
            footer as u64,
            0,
            0,
            3 * unit_bytes as u64,
            1,
        ));
        (3..5).for_each(|n| file.extend(element(n)));
        file.extend(pack(
            4,
            footer as u64,
            b2 as u64,
            footer as u64,
            index_klv.len() as u64,
            2,
            0,
            0,
        ));
        file.extend(index_klv);
        let synthetic = index_of(&file).unwrap();
        assert_eq!(synthetic.partitions.len(), 4);
        assert!(!synthetic.truncated_tail && !synthetic.header_metadata_complete);
        assert_eq!(
            synthetic.wrapping_of(synthetic.index_table(2).unwrap()),
            MxfWrapping::Frame
        );
        assert_eq!(synthetic.index_table(2).unwrap().indexed_units(), Some(5));
        for n in 0..5u64 {
            // Units 0-2 sit in the first body partition, 3-4 in the second, `unit_bytes` apart.
            let (partition, within) = if n < 3 { (b1, n) } else { (b2, n - 3) };
            let expected = (partition + pack_len) as u64 + within * unit_bytes as u64;
            let unit = synthetic.locate(2, n).unwrap();
            assert_eq!(
                (unit.file_offset, unit.stream_offset),
                (expected, n * unit_bytes as u64),
                "unit {n}"
            );
            let el = synthetic
                .edit_unit_element(
                    &mut Cursor::new(file.clone()),
                    &unit,
                    0x1501_0500,
                    &Limits::default(),
                )
                .unwrap();
            assert_eq!(
                (el.key_offset, el.value_len),
                (expected, UNIT as u64),
                "unit {n}"
            );
            assert_eq!(klv_value(&file, expected), &[n as u8; UNIT][..]);
        }
        assert!(matches!(
            synthetic.locate(2, 5),
            Err(ReelError::OutOfRange { .. })
        ));
        // The scan fallback (no footer pointer in any pack) finds the same four partitions.
        let mut scanned = file.clone();
        for at in [20 + 24, b1 + 20 + 24, b2 + 20 + 24, footer + 20 + 24] {
            scanned[at..at + 8].copy_from_slice(&[0u8; 8]);
        }
        let again = index_of(&scanned).unwrap();
        assert_eq!(
            again
                .partitions
                .iter()
                .map(|p| p.offset)
                .collect::<Vec<_>>(),
            [0, b1 as u64, b2 as u64, footer as u64]
        );
        assert_eq!(again.index_tables, synthetic.index_tables);
    });
}

/// Hostile inputs (V4-02): u32/u64-max counts and lengths, aggregate budgets, truncation at every length,
/// a broken partition chain, index offsets that point at nothing, single-byte corruption.
#[test]
fn mxf_hostile_inputs_refuse_cleanly() {
    bounded_case("mxf_hostile", || {
        // BER lengths: past the source, unspecified (0x80), wider than 8 octets, cut short.
        let key = [6u8, 14, 43, 52, 2, 5, 1, 1, 13, 1, 2, 1, 1, 2, 4, 0];
        let with_ber = |ber: &[u8]| {
            let mut bytes = key.to_vec();
            bytes.extend_from_slice(ber);
            bytes.extend_from_slice(&[0u8; 64]);
            bytes
        };
        for (ber, what) in [
            (
                &[0x88u8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF][..],
                "klv-length-exceeds-source",
            ),
            (&[0x80][..], "klv-ber-unspecified"),
            (&[0x89, 0, 0, 0, 0, 0, 0, 0, 0, 1][..], "klv-ber-length"),
            (
                &[0x84, 0x7F, 0xFF, 0xFF, 0xFF][..],
                "klv-length-exceeds-source",
            ),
            (&[0x30][..], "mxf-partition-pack-length"),
        ] {
            assert_eq!(
                index_of(&with_ber(ber)).unwrap_err(),
                ReelError::Corrupt { what },
                "{ber:02x?}"
            );
        }
        assert_eq!(
            index_of(b"RIFF....").unwrap_err(),
            ReelError::ImportUnsupported {
                detected: "not-mxf".into()
            }
        );

        // Counts at u32::MAX: essence container batch, index entry array, delta array, primer.
        let first_entry_array = [0x3F, 0x0A, 0x00, 0x9E, 0, 0, 0, 0x0A, 0, 0, 0, 0x0F];
        let count_max = [
            0x3F, 0x0A, 0x00, 0x9E, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0x0F,
        ];
        assert_eq!(
            index_of(&patched(OP1A, &first_entry_array, &count_max)).unwrap_err(),
            ReelError::Corrupt {
                what: "mxf-entry-array"
            }
        );
        let deltas = [0x3F, 0x09, 0x00, 0x1A, 0, 0, 0, 0x03, 0, 0, 0, 0x06];
        let deltas_max = [
            0x3F, 0x09, 0x00, 0x1A, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0x06,
        ];
        assert_eq!(
            index_of(&patched(OP1A, &deltas, &deltas_max)).unwrap_err(),
            ReelError::Corrupt {
                what: "mxf-delta-array"
            }
        );
        let mut bad_batch = OP1A.to_vec();
        bad_batch[16 + 4 + 80..16 + 4 + 84].copy_from_slice(&[0xFF; 4]);
        assert_eq!(
            index_of(&bad_batch).unwrap_err(),
            ReelError::Corrupt {
                what: "mxf-essence-container-batch"
            }
        );

        // Aggregate budgets: the index entries across all segments, and the metadata / index bytes read.
        let few = Limits {
            max_samples: 5,
            ..Limits::default()
        };
        assert_eq!(
            build_mxf_index(
                &mut Cursor::new(OP1A.to_vec()),
                OP1A.len() as u64,
                &few,
                &CancellationToken::default()
            )
            .unwrap_err(),
            ReelError::LimitExceeded {
                what: "mxf-index-entries"
            }
        );
        let tiny = Limits {
            max_probe_bytes: 1000,
            ..Limits::default()
        };
        assert_eq!(
            build_mxf_index(
                &mut Cursor::new(OP1A.to_vec()),
                OP1A.len() as u64,
                &tiny,
                &CancellationToken::default()
            )
            .unwrap_err(),
            ReelError::LimitExceeded {
                what: "probe-bytes"
            }
        );

        // A broken partition chain (footer pointer zeroed or absurd) falls back to a KLV scan and reaches
        // the same partitions.
        let reference = index_of(OP1A).unwrap();
        // (the third value names the body partition, a real pack that is not a footer.)
        for footer in [[0u8; 8], [0xFF; 8], 6144u64.to_be_bytes()] {
            let mut bytes = OP1A.to_vec();
            bytes[20 + 24..20 + 32].copy_from_slice(&footer);
            let scanned = index_of(&bytes).unwrap();
            let offsets = |i: &MxfIndex| {
                i.partitions
                    .iter()
                    .map(|p| (p.kind, p.offset))
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                offsets(&scanned),
                offsets(&reference),
                "footer {footer:02x?}"
            );
            assert_eq!(scanned.index_tables, reference.index_tables);
        }

        // Index entries that point at nothing: outside the essence, or into the middle of a KLV.
        let entry2 = |index_bytes: &[u8]| {
            let at = index_bytes
                .windows(first_entry_array.len())
                .position(|w| w == first_entry_array)
                .unwrap();
            at + 12 + 15 * 2 + 3
        };
        let mut bytes = OP1A.to_vec();
        let at = entry2(&bytes);
        bytes[at..at + 8].copy_from_slice(&0x00FF_FFFF_FFFF_u64.to_be_bytes());
        let hostile = index_of(&bytes).unwrap();
        assert_eq!(
            hostile.locate_coded(2, 2).unwrap_err(),
            ReelError::Corrupt {
                what: "mxf-stream-offset-outside-essence"
            }
        );
        let mut bytes = OP1A.to_vec();
        let at = entry2(&bytes);
        bytes[at..at + 8].copy_from_slice(&(26624u64 + 3).to_be_bytes());
        let hostile = index_of(&bytes).unwrap();
        let unit = hostile.locate_coded(2, 2).unwrap();
        let err = hostile
            .edit_unit_element(
                &mut Cursor::new(bytes.clone()),
                &unit,
                0x1501_0500,
                &Limits::default(),
            )
            .unwrap_err();
        assert!(matches!(err, ReelError::Corrupt { .. }), "{err:?}");

        // Stale grant before any read.
        let mut req = request(OP1A, PICTURE_TRACK, 0, 1, OutputMode::Packets);
        req.grant.expected_len += 1;
        assert_eq!(
            deliver(OP1A, &req, &Limits::default(), &mut NeverReached).unwrap_err(),
            ReelError::StaleGrant
        );

        // Truncation at many lengths and single-byte corruption of the structure: a typed result or
        // success, never a panic (a prefix that still holds the metadata can index fine).
        for len in (0..OP1A.len())
            .step_by(211)
            .chain([6143, 6144, 6145, 126_463, 126_464, 126_465, 127_544])
        {
            let _ = index_of(&OP1A[..len]);
            let _ = index_of(&ATOM[..len.min(ATOM.len())]);
        }
        for at in (0..6144).step_by(3).chain(126_464..127_545) {
            let mut bytes = OP1A.to_vec();
            bytes[at] ^= 0xFF;
            if let Ok(index) = index_of(&bytes) {
                for d in 0..10 {
                    let _ = index.locate(2, d);
                    if let Ok(unit) = index.locate(2, d) {
                        let _ = index.edit_unit_element(
                            &mut Cursor::new(bytes.clone()),
                            &unit,
                            0x1501_0500,
                            &Limits::default(),
                        );
                    }
                }
                let _ = deliver(
                    &bytes,
                    &request(&bytes, PICTURE_TRACK, 0, 3, OutputMode::Packets),
                    &Limits::default(),
                    &mut Collect::default(),
                );
            }
        }
    });
}

/// Temporal offsets are untrusted bytes and a conformant index maps display positions to coded
/// positions one-to-one. When two display positions land on one coded unit, the request that contains the
/// later one cannot be delivered whole: it used to return Ok with `complete == false` and an `Exact`
/// disposition, for `Purpose::Export` too (SMB-REEL-ENG-1 R9 pre-review). It is a corrupt index, refused
/// before any essence is read (sink untouched), for a preview and an export alike; requests that do not
/// touch the damaged entries deliver completely.
#[test]
fn mxf_temporal_offsets_that_are_not_a_permutation_are_refused() {
    bounded_case("mxf_temporal_offset_permutation", || {
        let entry_array = [0x3F, 0x0A, 0x00, 0x9E, 0, 0, 0, 0x0A, 0, 0, 0, 0x0F];
        let at = OP1A
            .windows(entry_array.len())
            .position(|w| w == entry_array)
            .unwrap();
        // The picture index entry for display position `k`: 15 bytes after the 12 byte array header; its
        // first byte is the temporal offset.
        let offset_byte = |k: usize| at + 12 + 15 * k;
        // Decode order of the fixture is pts 0,3,1,2,6,4,5,9,7,8, so the offsets by display position are:
        let fixture_offsets: [i8; 10] = [0, 1, 1, -2, 1, 1, -2, 1, 1, -2];
        for (k, want) in fixture_offsets.iter().enumerate() {
            assert_eq!(OP1A[offset_byte(k)] as i8, *want, "fixture entry {k}");
        }
        let with_offset = |k: usize, offset: i8| {
            let mut bytes = OP1A.to_vec();
            bytes[offset_byte(k)] = offset as u8;
            bytes
        };
        // Display 1 now lands on coded 0, where display 0 already is.
        let first_collides = with_offset(1, -1);
        // Display 3 now lands on coded 2, where display 1 already is (the earlier position keeps it).
        let later_collides = with_offset(3, -1);
        // Display 9 now lands on coded 8, where display 7 already is. Only this variant leaves coded
        // positions 0..=6 intact: delivery of a reordered picture starts from the single random access
        // point, coded 0, so a hole before the requested range would be refused too.
        let tail_collides = with_offset(9, -1);

        for purpose in [Purpose::Preview, Purpose::Export] {
            let run = |bytes: &[u8], start: u64, count: u64, sink: &mut dyn ChunkSink| {
                let start_pts = start * TICKS_PER_FRAME_25;
                let mut req = request(bytes, PICTURE_TRACK, start_pts, count, OutputMode::Packets);
                req.purpose = purpose;
                deliver(bytes, &req, &Limits::default(), sink)
            };
            let refused = ReelError::Corrupt {
                what: "mxf-temporal-offset",
            };
            for (label, bytes, start, count) in [
                ("display 1 on coded 0: 0..2", &first_collides, 0, 2),
                ("display 1 on coded 0: 1..2", &first_collides, 1, 1),
                ("display 1 on coded 0: whole clip", &first_collides, 0, 10),
                ("display 3 on coded 2: 3..4", &later_collides, 3, 1),
                ("display 3 on coded 2: 1..4", &later_collides, 1, 3),
                ("display 9 on coded 8: 9..10", &tail_collides, 9, 1),
                ("display 9 on coded 8: 7..10", &tail_collides, 7, 3),
            ] {
                assert_eq!(
                    run(bytes, start, count, &mut NeverReached).unwrap_err(),
                    refused,
                    "{purpose:?} {label}"
                );
            }

            // Requests that avoid the damaged entries, and the undamaged fixture, deliver completely.
            for (label, bytes, start, count) in [
                ("display 9 on coded 8: 0..7", &tail_collides, 0, 7),
                ("display 9 on coded 8: 4..7", &tail_collides, 4, 3),
                ("fixture: whole clip", &OP1A.to_vec(), 0, 10),
            ] {
                let mut sink = Collect::default();
                let receipt = run(bytes, start, count, &mut sink)
                    .unwrap_or_else(|e| panic!("{purpose:?} {label}: {e}"));
                assert!(receipt.complete, "{purpose:?} {label}");
                assert_eq!(receipt.actual.units, count, "{purpose:?} {label}");
                assert_eq!(
                    receipt.disposition,
                    Disposition::Exact,
                    "{purpose:?} {label}"
                );
                let delivered = sink.chunks.iter().filter(|c| !c.4).count() as u64;
                assert_eq!(delivered, count, "{purpose:?} {label}");
            }
        }
    });
}
