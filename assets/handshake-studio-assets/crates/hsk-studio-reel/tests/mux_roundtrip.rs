//! ISO BMFF writer (lane SMB-REEL-ENCODE slice 1): files written by `Muxer` are read back through reel's
//! own demuxer and decoders (frame count, timestamps, bytes), and by ffprobe/ffmpeg as an independent
//! reader when they are on PATH (otherwise that test prints NOT_PROVEN). Hostile specs are refused before a
//! byte is written.
//!
//! Fixtures reused (read-only): `tests/fixtures/dnx/dnxhr-hq-264x120.mov` (VC-3, 3 frames, 25 fps),
//! `tests/fixtures/av1/av1-16x16-3f.mp4` (AV1, 3 frames) and `tests/fixtures/mov/mov-h264-bf2-negcts-tc-df-64x36.mov`
//! (H.264 with B-frames, 24 frames at 30000/1001), all with provenance files in their directories.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_reel::*;
use std::io::Cursor;

const DNX: &[u8] = include_bytes!("fixtures/dnx/dnxhr-hq-264x120.mov");
const AV1: &[u8] = include_bytes!("fixtures/av1/av1-16x16-3f.mp4");
const H264_BF: &[u8] = include_bytes!("fixtures/mov/mov-h264-bf2-negcts-tc-df-64x36.mov");
/// 25 fps and 30000/1001 fps in Pulse ticks.
const TPF_25: u64 = 10_160_640_000;
const TPF_2997: u64 = 8_475_667_200;

#[derive(Default)]
struct Collect {
    chunks: Vec<(i64, bool, Vec<u8>)>,
}

impl ChunkSink for Collect {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
        self.chunks.push((c.pts, c.sync, c.bytes.to_vec()));
        Ok(())
    }
}

fn request(bytes: &[u8], track: Option<u32>, count: u64, output: OutputMode) -> SegmentRequest {
    SegmentRequest {
        codec: "isobmff".to_owned(),
        start_pts: Ticks::new(0),
        count,
        track,
        output,
        prefer: SourcePreference::Original,
        purpose: Purpose::Export,
        grant: Grant {
            content_id: ContentId::parse("mux-roundtrip").unwrap(),
            revision: 1,
            expected_len: bytes.len() as u64,
        },
        expected_revision: 1,
    }
}

fn read(bytes: &[u8], track: Option<u32>, count: u64, output: OutputMode) -> (SegmentReceipt, Collect) {
    let mut sink = Collect::default();
    let receipt = Registry::standard()
        .decode_segment(
            &mut Cursor::new(bytes.to_vec()),
            None,
            &request(bytes, track, count, output),
            &Limits::default(),
            &CancellationToken::default(),
            &mut sink,
        )
        .expect("reel reads the file");
    (receipt, sink)
}

fn index(bytes: &[u8]) -> MediaIndex {
    build_index(
        &mut Cursor::new(bytes.to_vec()),
        bytes.len() as u64,
        &Limits::default(),
        &CancellationToken::default(),
    )
    .expect("index")
}

/// Deterministic interleaved PCM test signal (`frames` frames, `channels` channels) in `spec`'s layout.
fn pcm(spec: &PcmSpec, first: u64, frames: u64) -> Vec<u8> {
    let mut out = Vec::new();
    for f in first..first + frames {
        for c in 0..u64::from(spec.channels) {
            let phase = (f * 37 + c * 1013) % 4096;
            let v = phase as i64 * 2 - 4096; // -4096..4094
            match (spec.float, spec.bits) {
                (true, _) => out.extend_from_slice(&(v as f32 / 4096.0).to_le_bytes()),
                (false, 16) => out.extend_from_slice(&((v * 7) as i16).to_le_bytes()),
                (false, 24) => out.extend_from_slice(&((v * 2039) as i32).to_le_bytes()[..3]),
                _ => out.extend_from_slice(&((v * 524_287) as i32).to_le_bytes()),
            }
        }
    }
    out
}

fn mux(spec: MuxSpec, write: impl FnOnce(&mut Muxer<Cursor<Vec<u8>>>)) -> (Vec<u8>, MuxReceipt) {
    let mut muxer = Muxer::new(Cursor::new(Vec::new()), spec, MuxLimits::default()).expect("spec admitted");
    write(&mut muxer);
    let (out, receipt) = muxer.finish().expect("finish");
    (out.into_inner(), receipt)
}

/// VC-3 frames remuxed into MOV with PCM audio at 16/24/32-bit integer and 32-bit float: reel decodes
/// the video back to exactly the pictures of the source file (same decoder, same bytes), three frames at
/// 0, 1/25 s, 2/25 s, and the audio back to exactly the written samples.
#[test]
fn mov_vc3_with_pcm_reads_back_exact() {
    let (_, source) = read(DNX, None, 3, OutputMode::Decoded);
    let (_, packets) = read(DNX, None, 3, OutputMode::Packets);
    assert_eq!(packets.chunks.len(), 3);
    let layouts = [(16, false), (24, false), (32, false), (32, true)];
    for (bits, float) in layouts {
        let audio = PcmSpec { bits, float, rate: 48_000, channels: 2 };
        let spec = MuxSpec {
            container: Container::Mov,
            video: Some(VideoTrackSpec {
                entry: VideoEntry::Vc3 { dnxhr: true },
                width: 264,
                height: 120,
                ticks_per_frame: TPF_25,
                color: Some(NclxColor { primaries: 1, transfer: 1, matrix: 1, full_range: false }),
            }),
            audio: Some(audio),
        };
        let mut written = Vec::new();
        let (file, receipt) = mux(spec, |m| {
            for (i, (_, _, bytes)) in packets.chunks.iter().enumerate() {
                m.write_video(bytes, i as u64, true).unwrap();
                // 1920 frames of 48 kHz per 25 fps frame, written in two runs.
                for half in 0..2u64 {
                    let block = pcm(&audio, i as u64 * 1920 + half * 960, 960);
                    m.write_audio(&block).unwrap();
                    written.extend_from_slice(&block);
                }
            }
        });
        assert_eq!((receipt.video_samples, receipt.audio_frames), (3, 5760));
        assert_eq!((receipt.video_timescale, receipt.video_sample_delta), (25, 1));
        assert_eq!(receipt.file_bytes, file.len() as u64);
        let idx = index(&file);
        assert_eq!(idx.major_brand, *b"qt  ");
        assert_eq!(idx.tracks.len(), 2);
        assert_eq!(&idx.tracks[0].codec, b"AVdh");
        assert_eq!(&idx.tracks[1].codec, b"lpcm");

        let (vr, video) = read(&file, Some(1), 3, OutputMode::Decoded);
        assert_eq!(vr.disposition, Disposition::Exact);
        assert_eq!(video.chunks.len(), 3, "frame count");
        for (i, ((pts, _, got), (_, _, want))) in video.chunks.iter().zip(&source.chunks).enumerate() {
            assert_eq!(*pts, i as i64 * TPF_25 as i64, "pts of frame {i}");
            assert!(got == want, "frame {i} differs from the source decode ({bits}-bit float={float})");
        }
        let (ar, sound) = read(&file, Some(2), 5760, OutputMode::Decoded);
        assert_eq!(ar.actual.units, 5760);
        assert_eq!(sound.chunks[0].0, 0, "audio starts at zero");
        let decoded: Vec<u8> = sound.chunks.iter().flat_map(|c| c.2.clone()).collect();
        assert_eq!(decoded, written, "PCM {bits}-bit float={float} round trip");
    }
}

/// AV1 temporal units (MP4 `av01`) and H.264 with B-frames (`avc1` remux, decode order with presentation
/// indices) come back from reel's demuxer with the same bytes, in the same decode order, at the same
/// presentation times; the reordered stream gets `ctts` + an edit list that starts presentation at zero.
#[test]
fn mp4_av1_and_reordered_h264_remux_keep_bytes_and_times() {
    // AV1.
    let av1_track = &index(AV1).tracks[0];
    let (_, packets) = read(AV1, None, 3, OutputMode::Packets);
    let tpf = (packets.chunks[1].0 - packets.chunks[0].0) as u64;
    let spec = MuxSpec {
        container: Container::Mp4,
        video: Some(VideoTrackSpec {
            entry: VideoEntry::Av1 { av1c: av1_track.codec_config.clone() },
            width: av1_track.width,
            height: av1_track.height,
            ticks_per_frame: tpf,
            color: None,
        }),
        audio: None,
    };
    let (file, receipt) = mux(spec, |m| {
        for (i, (_, sync, bytes)) in packets.chunks.iter().enumerate() {
            m.write_video(bytes, i as u64, *sync).unwrap();
        }
    });
    assert_eq!(receipt.reorder_delay, 0);
    let idx = index(&file);
    assert_eq!((idx.major_brand, idx.tracks[0].codec), (*b"isom", *b"av01"));
    assert_eq!(idx.tracks[0].codec_config, av1_track.codec_config);
    let (_, back) = read(&file, None, 3, OutputMode::Packets);
    assert_eq!(back.chunks, packets.chunks.iter().enumerate().map(|(i, c)| (i as i64 * tpf as i64, c.1, c.2.clone())).collect::<Vec<_>>());

    // H.264 with two B-frames: decode order differs from presentation order.
    let h264_track = &index(H264_BF).tracks[0];
    let (_, packets) = read(H264_BF, Some(h264_track.track_id), 24, OutputMode::Packets);
    assert_eq!(packets.chunks.len(), 24);
    let mut sorted: Vec<i64> = packets.chunks.iter().map(|c| c.0).collect();
    sorted.sort_unstable();
    let rank = |pts: i64| sorted.iter().position(|p| *p == pts).unwrap() as u64;
    assert!(packets.chunks.iter().enumerate().any(|(i, c)| rank(c.0) != i as u64), "fixture reorders");
    let spec = MuxSpec {
        container: Container::Mp4,
        video: Some(VideoTrackSpec {
            entry: VideoEntry::H264Remux { avcc: h264_track.codec_config.clone() },
            width: 64,
            height: 36,
            ticks_per_frame: TPF_2997,
            color: None,
        }),
        audio: None,
    };
    let (file, receipt) = mux(spec, |m| {
        for (pts, sync, bytes) in &packets.chunks {
            m.write_video(bytes, rank(*pts), *sync).unwrap();
        }
    });
    assert!(receipt.reorder_delay > 0);
    assert_eq!((receipt.video_timescale, receipt.video_sample_delta), (30000, 1001));
    let (_, back) = read(&file, None, 24, OutputMode::Packets);
    assert_eq!(back.chunks.len(), 24);
    for (i, ((pts, sync, bytes), (src_pts, src_sync, src_bytes))) in back.chunks.iter().zip(&packets.chunks).enumerate() {
        assert_eq!(bytes, src_bytes, "packet {i} bytes (decode order)");
        assert_eq!(sync, src_sync, "packet {i} sync flag");
        assert_eq!(*pts as u64, rank(*src_pts) * TPF_2997, "packet {i} presentation time");
    }
    let presented: Vec<i64> = {
        let mut p: Vec<i64> = back.chunks.iter().map(|c| c.0).collect();
        p.sort_unstable();
        p
    };
    assert_eq!(presented.first(), Some(&0), "edit list starts presentation at zero");
}

/// Hostile and unsupported specs are refused by `Muxer::new` before anything is written; per-sample
/// bounds are checked before the sample is written.
#[test]
fn hostile_mux_specs_refused_before_writing() {
    let video = |w: u32, h: u32, tpf: u64, entry: VideoEntry| VideoTrackSpec {
        entry,
        width: w,
        height: h,
        ticks_per_frame: tpf,
        color: None,
    };
    let vc3 = VideoEntry::Vc3 { dnxhr: true };
    let cases: Vec<(MuxSpec, &str)> = vec![
        (MuxSpec { container: Container::Mov, video: Some(video(u32::MAX, u32::MAX, TPF_25, vc3.clone())), audio: None }, "mux-dimensions"),
        (MuxSpec { container: Container::Mov, video: Some(video(0, 120, TPF_25, vc3.clone())), audio: None }, "mux-zero-dimension"),
        (MuxSpec { container: Container::Mov, video: Some(video(64, 64, 1, vc3.clone())), audio: None }, "mux-frame-rate-not-representable"),
        (MuxSpec { container: Container::Mov, video: Some(video(64, 64, 0, vc3.clone())), audio: None }, "mux-zero-frame-duration"),
        (MuxSpec { container: Container::Mp4, video: Some(video(64, 64, TPF_25, vc3.clone())), audio: None }, "mux-mp4-cannot-carry-vc3"),
        (MuxSpec { container: Container::Mov, video: None, audio: Some(PcmSpec { bits: 16, float: false, rate: 48_000, channels: u16::MAX }) }, "mux-pcm-layout"),
        (MuxSpec { container: Container::Mov, video: None, audio: Some(PcmSpec { bits: 16, float: false, rate: u32::MAX, channels: 2 }) }, "mux-pcm-layout"),
        (MuxSpec { container: Container::Mov, video: None, audio: Some(PcmSpec { bits: 12, float: false, rate: 48_000, channels: 2 }) }, "mux-pcm-bit-depth"),
        (MuxSpec { container: Container::Mp4, video: None, audio: Some(PcmSpec { bits: 16, float: false, rate: 96_000, channels: 2 }) }, "mux-mp4-pcm-rate-above-65535"),
        (MuxSpec { container: Container::Mov, video: None, audio: None }, "mux-no-tracks"),
    ];
    for (spec, what) in cases {
        let mut out = Cursor::new(Vec::new());
        let err = Muxer::new(&mut out, spec.clone(), MuxLimits::default()).err().expect("refused");
        assert!(matches!(err, ReelError::Unsupported { what: w } | ReelError::LimitExceeded { what: w } if w == what), "{spec:?}: {err:?}");
        assert!(out.get_ref().is_empty(), "nothing written for {what}");
    }
    // A config record that is not an av1C, and an oversized one.
    let bad = MuxSpec { container: Container::Mp4, video: Some(video(64, 64, TPF_25, VideoEntry::Av1 { av1c: vec![0; 3] })), audio: None };
    assert!(Muxer::new(Cursor::new(Vec::new()), bad, MuxLimits::default()).is_err());
    let huge = MuxSpec { container: Container::Mp4, video: Some(video(64, 64, TPF_25, VideoEntry::Av1 { av1c: vec![0x81; 70_000] })), audio: None };
    assert!(matches!(Muxer::new(Cursor::new(Vec::new()), huge, MuxLimits::default()), Err(ReelError::LimitExceeded { what: "mux-config-bytes" })));

    // Per-sample bounds: sample bytes, sample count, file bytes, partial PCM frames, empty tracks.
    let limits = MuxLimits { max_sample_bytes: 100, max_video_samples: 2, max_file_bytes: 4096, ..MuxLimits::default() };
    let spec = MuxSpec { container: Container::Mov, video: Some(video(64, 64, TPF_25, vc3.clone())), audio: Some(PcmSpec { bits: 16, float: false, rate: 48_000, channels: 2 }) };
    let mut m = Muxer::new(Cursor::new(Vec::new()), spec.clone(), limits).unwrap();
    assert_eq!(m.write_video(&[1; 101], 0, true), Err(ReelError::LimitExceeded { what: "mux-sample-bytes" }));
    m.write_video(&[1; 100], 0, true).unwrap();
    m.write_video(&[1; 100], 1, true).unwrap();
    assert_eq!(m.write_video(&[1; 10], 2, true), Err(ReelError::LimitExceeded { what: "mux-video-samples" }));
    assert_eq!(m.write_audio(&[0; 3]), Err(ReelError::Unsupported { what: "mux-pcm-partial-frame" }));
    assert_eq!(m.write_audio(&vec![0; 8192]), Err(ReelError::LimitExceeded { what: "mux-file-bytes" }));
    let m = Muxer::new(Cursor::new(Vec::new()), spec, MuxLimits::default()).unwrap();
    assert_eq!(m.finish().err(), Some(ReelError::Unsupported { what: "mux-video-track-empty" }));
    // Presentation indices must be a permutation of 0..n.
    let spec = MuxSpec { container: Container::Mov, video: Some(video(64, 64, TPF_25, vc3)), audio: None };
    let mut m = Muxer::new(Cursor::new(Vec::new()), spec, MuxLimits::default()).unwrap();
    m.write_video(&[1; 10], 0, true).unwrap();
    m.write_video(&[1; 10], 0, true).unwrap();
    assert_eq!(m.finish().err(), Some(ReelError::Unsupported { what: "mux-pts-not-a-permutation" }));
}

fn tool(name: &str) -> Option<std::path::PathBuf> {
    let probe = std::process::Command::new(name).arg("-version").output().ok()?;
    probe.status.success().then(|| std::path::PathBuf::from(name))
}

/// Independent reader: ffprobe/ffmpeg (on PATH, black-box) read the MOV (`AVdh` + `lpcm`) and the MP4
/// (`av01` + `ipcm`) files; ffmpeg's decode of the PCM equals the written samples and it counts the same
/// number of video frames. NOT_PROVEN when ffmpeg is not installed.
#[test]
fn ffmpeg_reads_muxed_files_as_written() {
    let (Some(ffprobe), Some(ffmpeg)) = (tool("ffprobe"), tool("ffmpeg")) else {
        println!("NOT_PROVEN: ffprobe/ffmpeg not on PATH; independent-reader check skipped");
        return;
    };
    let dir = std::env::temp_dir().join(format!("reel-mux-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (_, dnx_packets) = read(DNX, None, 3, OutputMode::Packets);
    let av1_track = &index(AV1).tracks[0];
    let (_, av1_packets) = read(AV1, None, 3, OutputMode::Packets);
    let audio = PcmSpec { bits: 24, float: false, rate: 48_000, channels: 2 };
    for (container, entry, packets, (w, h)) in [
        (Container::Mov, VideoEntry::Vc3 { dnxhr: true }, &dnx_packets, (264, 120)),
        (Container::Mp4, VideoEntry::Av1 { av1c: av1_track.codec_config.clone() }, &av1_packets, (av1_track.width, av1_track.height)),
    ] {
        let spec = MuxSpec {
            container,
            video: Some(VideoTrackSpec { entry, width: w, height: h, ticks_per_frame: TPF_25, color: None }),
            audio: Some(audio),
        };
        let mut written = Vec::new();
        let (file, _) = mux(spec, |m| {
            for (i, (_, sync, bytes)) in packets.chunks.iter().enumerate() {
                m.write_video(bytes, i as u64, *sync).unwrap();
                let block = pcm(&audio, i as u64 * 1920, 1920);
                m.write_audio(&block).unwrap();
                written.extend_from_slice(&block);
            }
        });
        let path = dir.join(format!("roundtrip.{}", container.extension()));
        std::fs::write(&path, &file).unwrap();
        let probe = std::process::Command::new(&ffprobe)
            .args(["-v", "error", "-count_packets", "-show_entries", "stream=codec_name,nb_read_packets,sample_rate,channels", "-of", "csv=p=0"])
            .arg(&path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&probe.stdout).to_string();
        assert!(probe.status.success(), "ffprobe {container:?}: {}", String::from_utf8_lossy(&probe.stderr));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{container:?}: {text}");
        assert!(lines[0].ends_with(",3"), "{container:?} video packets: {text}");
        assert!(lines[1].starts_with("pcm_s24le,48000,2,"), "{container:?} audio: {text}");
        let decoded = std::process::Command::new(&ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-map", "0:a", "-f", "s24le", "-"])
            .output()
            .unwrap();
        assert!(decoded.status.success(), "ffmpeg {container:?}: {}", String::from_utf8_lossy(&decoded.stderr));
        assert_eq!(decoded.stdout, written, "{container:?}: ffmpeg's PCM decode equals the written samples");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
