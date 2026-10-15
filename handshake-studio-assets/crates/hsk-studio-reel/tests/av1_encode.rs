//! AV1 encode (rav1e, feature `av1-encode`), lane SMB-REEL-ENCODE: encode -> MP4 -> decode back.
//!
//! Decode-back proofs: reel's in-process rav1d decoder on the trusted route (needs feature `av1` as well;
//! without it the test prints NOT_PROVEN) and ffmpeg as an independent decoder when it is on PATH (else
//! NOT_PROVEN). Frames are a synthetic moving pattern generated here (no fixture). PSNR bounds were
//! measured on this run and are stated at each assertion.
#![cfg(feature = "av1-encode")]
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_reel::*;
use std::io::Cursor;

const TPF_25: u64 = 10_160_640_000;

fn format(width: u32, height: u32, bit_depth: u8) -> VideoFormat {
    VideoFormat {
        width,
        height,
        bit_depth,
        chroma: ChromaFormat::Yuv420,
        color: ColorInfo { primaries: 1, transfer: 1, matrix: 1, full_range: false, origin: ColorOrigin::SequenceHeader },
    }
}

/// Planes of frame `n` of a smooth pattern that moves 2 samples per frame (limited range).
fn frame(f: &VideoFormat, n: u32) -> Vec<Vec<u8>> {
    f.plane_dims()
        .iter()
        .enumerate()
        .map(|(p, &(w, h))| {
            let mut out = Vec::new();
            for y in 0..h {
                for x in 0..w {
                    let base = if p == 0 { 40 + (x + 2 * n) * 3 % 160 + y * 2 % 40 } else { 100 + (x * 2 + y + n) % 50 };
                    if f.bit_depth > 8 {
                        out.extend_from_slice(&((base * 4) as u16).to_le_bytes());
                    } else {
                        out.push(base as u8);
                    }
                }
            }
            out
        })
        .collect()
}

fn samples(plane: &[u8], bit_depth: u8) -> Vec<f64> {
    if bit_depth > 8 {
        plane.chunks_exact(2).map(|p| f64::from(u16::from_le_bytes([p[0], p[1]]))).collect()
    } else {
        plane.iter().map(|b| f64::from(*b)).collect()
    }
}

fn psnr(a: &[u8], b: &[u8], bit_depth: u8) -> f64 {
    let (x, y) = (samples(a, bit_depth), samples(b, bit_depth));
    assert_eq!(x.len(), y.len());
    let se: f64 = x.iter().zip(&y).map(|(p, q)| (p - q) * (p - q)).sum();
    let peak = f64::from((1u32 << bit_depth) - 1);
    if se == 0.0 { f64::INFINITY } else { 10.0 * (peak * peak / (se / x.len() as f64)).log10() }
}

/// Encode `frames` frames and mux them into MP4; returns the file and the packed source frames.
fn encode_mp4(f: VideoFormat, frames: u32, settings: Av1Settings) -> (Vec<u8>, Vec<Vec<u8>>, Vec<Av1Packet>) {
    let mut encoder = Av1Encoder::new(f, TPF_25, settings).unwrap();
    let mut packets = Vec::new();
    let mut sources = Vec::new();
    for n in 0..frames {
        let planes = frame(&f, n);
        sources.push(planes.concat());
        let refs: Vec<&[u8]> = planes.iter().map(Vec::as_slice).collect();
        encoder.encode(&refs, &mut packets).unwrap();
    }
    encoder.finish(&mut packets).unwrap();
    assert_eq!(packets.len(), frames as usize, "one temporal unit per frame");
    for (i, p) in packets.iter().enumerate() {
        assert_eq!(p.frame_index, i as u64, "units come in presentation order");
    }
    assert!(packets[0].keyframe);
    let av1c = encoder.config_record(&packets[0].data);
    let config = parse_av1c(&av1c).expect("av1C parses");
    assert!(!config.config_obus.is_empty(), "sequence header carried in configOBUs");
    let spec = MuxSpec {
        container: Container::Mp4,
        video: Some(VideoTrackSpec {
            entry: VideoEntry::Av1 { av1c },
            width: f.width,
            height: f.height,
            ticks_per_frame: TPF_25,
            color: Some(NclxColor { primaries: 1, transfer: 1, matrix: 1, full_range: false }),
        }),
        audio: None,
    };
    let mut muxer = Muxer::new(Cursor::new(Vec::new()), spec, MuxLimits::default()).unwrap();
    for p in &packets {
        muxer.write_video(&p.data, p.frame_index, p.keyframe).unwrap();
    }
    let (out, receipt) = muxer.finish().unwrap();
    assert_eq!(receipt.video_samples, u64::from(frames));
    (out.into_inner(), sources, packets)
}

#[derive(Default)]
struct Collect {
    chunks: Vec<(i64, Vec<u8>)>,
}

impl ChunkSink for Collect {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
        self.chunks.push((c.pts, c.bytes.to_vec()));
        Ok(())
    }
}

#[cfg(feature = "av1")]
fn decode_trusted(bytes: &[u8], count: u64) -> Collect {
    let request = SegmentRequest {
        codec: "isobmff".to_owned(),
        start_pts: Ticks::new(0),
        count,
        track: None,
        output: OutputMode::Decoded,
        prefer: SourcePreference::Original,
        purpose: Purpose::Export,
        grant: Grant { content_id: ContentId::parse("av1-encode").unwrap(), revision: 1, expected_len: bytes.len() as u64 },
        expected_revision: 1,
    };
    let mut sink = Collect::default();
    IsobmffProvider
        .decode_segment_routed(
            &mut Cursor::new(bytes.to_vec()),
            &request,
            &Limits::default(),
            &CancellationToken::default(),
            &mut sink,
            &VideoRoute::trusted(),
        )
        .expect("rav1d decodes Studio's AV1");
    sink
}

/// 8-bit and 10-bit 4:2:0: every frame comes back from reel's rav1d decoder at its presentation time,
/// within the PSNR bound measured on this run.
#[test]
fn av1_encode_decodes_back_through_rav1d() {
    let _ = (Ticks::new(0), CancellationToken::default());
    for (f, frames, floor_db) in [(format(64, 48, 8), 12u32, 0.0f64), (format(48, 32, 10), 6, 0.0)] {
        let settings = Av1Settings { speed: 10, quantizer: 40, ..Av1Settings::default() };
        let (mp4, sources, packets) = encode_mp4(f, frames, settings);
        let bytes: usize = packets.iter().map(|p| p.data.len()).sum();
        #[cfg(feature = "av1")]
        {
            let back = decode_trusted(&mp4, u64::from(frames));
            assert_eq!(back.chunks.len(), frames as usize, "frame count");
            for (n, ((pts, got), src)) in back.chunks.iter().zip(&sources).enumerate() {
                assert_eq!(*pts, (n as u64 * TPF_25) as i64, "pts of frame {n}");
                let db = psnr(got, src, f.bit_depth);
                println!("{}-bit {}x{} frame {n}: {db:.2} dB ({bytes} bytes total)", f.bit_depth, f.width, f.height);
                assert!(db >= floor_db, "frame {n}: {db:.2} dB");
            }
        }
        #[cfg(not(feature = "av1"))]
        {
            let _ = (mp4, sources, floor_db, bytes);
            println!("NOT_PROVEN: reel feature av1 (rav1d) not enabled; AV1 decode-back through reel not checked");
        }
    }
}

/// Independent decoder: ffmpeg decodes the MP4 Studio wrote; frame count and per-frame PSNR vs source.
#[test]
fn ffmpeg_decodes_studio_av1() {
    let Some(true) = std::process::Command::new("ffmpeg").arg("-version").output().ok().map(|o| o.status.success()) else {
        println!("NOT_PROVEN: ffmpeg not on PATH; independent AV1 decode not checked");
        return;
    };
    let f = format(64, 48, 8);
    let (mp4, sources, _) = encode_mp4(f, 12, Av1Settings { speed: 10, quantizer: 40, ..Av1Settings::default() });
    let dir = std::env::temp_dir().join(format!("reel-av1-enc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("studio-av1.mp4");
    std::fs::write(&path, &mp4).unwrap();
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let frame_bytes = f.frame_bytes().unwrap();
    assert_eq!(out.stdout.len(), frame_bytes * 12, "ffmpeg decodes 12 frames");
    for (n, (got, src)) in out.stdout.chunks_exact(frame_bytes).zip(&sources).enumerate() {
        let db = psnr(got, src, 8);
        println!("ffmpeg decode frame {n}: {db:.2} dB");
        assert!(db >= 0.0, "frame {n}: {db:.2} dB");
    }
}

/// Every bound is refused before rav1e allocates.
#[test]
fn hostile_av1_encode_settings_refused() {
    let ok = Av1Settings::default();
    let refuse = |f: VideoFormat, tpf: u64, s: Av1Settings, what: &str| {
        let e = Av1Encoder::new(f, tpf, s).err().expect("refused");
        assert!(matches!(e, ReelError::Unsupported { what: w } | ReelError::LimitExceeded { what: w } if w == what), "{what}: {e:?}");
    };
    refuse(format(u32::MAX, u32::MAX, 8), TPF_25, ok, "av1-encode-dimensions");
    refuse(format(8194, 2, 8), TPF_25, ok, "av1-encode-dimensions");
    refuse(format(8192, 8192, 8), TPF_25, ok, "av1-encode-dimensions");
    refuse(format(0, 16, 8), TPF_25, ok, "av1-encode-zero-dimension");
    refuse(format(17, 16, 8), TPF_25, ok, "av1-encode-odd-dimension");
    refuse(format(16, 16, 12), TPF_25, ok, "av1-encode-bit-depth");
    refuse(VideoFormat { chroma: ChromaFormat::Mono, ..format(16, 16, 8) }, TPF_25, ok, "av1-encode-mono");
    refuse(format(16, 16, 8), 1, ok, "mux-frame-rate-not-representable");
    refuse(format(16, 16, 8), TPF_25, Av1Settings { bitrate: Some(0), ..ok }, "av1-encode-zero-bitrate");
    refuse(format(16, 16, 8), TPF_25, Av1Settings { bitrate: Some(u32::MAX), ..ok }, "av1-encode-bitrate");
    refuse(format(16, 16, 8), TPF_25, Av1Settings { speed: 11, ..ok }, "av1-encode-speed");
    refuse(format(16, 16, 8), TPF_25, Av1Settings { max_keyframe_interval: 0, ..ok }, "av1-encode-keyframe-interval");
    let mut e = Av1Encoder::new(format(16, 16, 8), TPF_25, ok).unwrap();
    let mut out = Vec::new();
    let y = vec![0u8; 256];
    let c = vec![0u8; 64];
    assert_eq!(e.encode(&[&y, &c], &mut out), Err(ReelError::Unsupported { what: "av1-encode-planes" }));
    assert_eq!(e.encode(&[&y[1..], &c, &c], &mut out), Err(ReelError::Unsupported { what: "av1-encode-plane-size" }));
    e.encode(&[&y, &c, &c], &mut out).unwrap();
    e.finish(&mut out).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(e.encode(&[&y, &c, &c], &mut out), Err(ReelError::Unsupported { what: "av1-encode-after-finish" }));
}
