#![cfg(all(feature = "mf-worker", feature = "test-faults", feature = "os-encode", windows))]
//! Operator decision D24-A (lane SMB-D24A-CODEC-DEFAULTS): H.264 and HEVC ENCODE only through the OS encoder
//! MFT in the isolated `reel-native-mf-worker --encode=h264|hevc` process (feature `os-encode`), as an output
//! format of reel's ExportQueue. Proof: decode-back of the exported MP4 through the OS H.264 decoder
//! (`WorkerMfProvider`): frame count, presentation times and a PSNR bound per frame against the source frames.
//! HEVC: a typed refusal where the OS has no HEVC encoder; where it has one the file is written and re-indexed
//! (decode-back NOT_PROVEN: reel-native has no HEVC decoder).
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_reel::*;
use hsk_studio_reel_native::*;
use std::{io::Cursor, path::PathBuf, sync::Arc, thread, time::Duration};

const WORKER: &str = env!("CARGO_BIN_EXE_reel-native-mf-worker");
/// 25 fps in Pulse ticks.
const TPF: u64 = 10_160_640_000;
const W: u32 = 320;
const H: u32 = 192;
const FRAMES: u64 = 30;
/// Lowest luma PSNR of any decoded-back frame at 4 Mbit/s, set from the measured runs (lane notes) with margin.
const MIN_PSNR_DB: f64 = 30.0;

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(180)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 180s deadline: {e}"),
    }
}

fn format(bit_depth: u8, width: u32) -> VideoFormat {
    VideoFormat {
        width,
        height: H,
        bit_depth,
        chroma: ChromaFormat::Yuv420,
        color: ColorInfo {
            primaries: 1,
            transfer: 1,
            matrix: 1,
            full_range: false,
            origin: ColorOrigin::Container,
        },
    }
}

/// A moving gradient with a sliding block: smooth and edged content, different every frame.
fn planes(index: u64) -> Vec<Vec<u8>> {
    let (w, h) = (W as usize, H as usize);
    let t = index as usize;
    let mut y = vec![0u8; w * h];
    for r in 0..h {
        for c in 0..w {
            let block = (c + 4 * t) % w < 48 && r > 60 && r < 130;
            y[r * w + c] = if block { 220 } else { (16 + (c + r + 3 * t) % 200) as u8 };
        }
    }
    let u = (0..w * h / 4).map(|i| (64 + (i + t) % 128) as u8).collect();
    let v = (0..w * h / 4).map(|i| (192 - (i / 7 + 2 * t) % 128) as u8).collect();
    vec![y, u, v]
}

struct Source {
    format: VideoFormat,
}
impl ExportVideoSource for Source {
    fn format(&self) -> VideoFormat {
        self.format
    }
    fn ticks_per_frame(&self) -> u64 {
        TPF
    }
    fn frame_count(&self) -> u64 {
        FRAMES
    }
    fn frame(&mut self, index: u64, _: &CancellationToken) -> Result<Vec<Vec<u8>>, ReelError> {
        Ok(planes(index))
    }
}

fn job(video: VideoCodecChoice, format: VideoFormat, output: PathBuf) -> ExportJob {
    ExportJob {
        recipe: ExportRecipe {
            recipe_id: "d24a-os-encode".into(),
            container: Container::Mp4,
            video: Some(video),
            audio: None,
        },
        video: Some(Box::new(Source { format })),
        audio: None,
        span: SourceSpan {
            start_ticks: 0,
            end_ticks: FRAMES * TPF,
        },
        output,
        overwrite: true,
        artifact: "os-encode.mp4".into(),
        limits: ExportLimits::default(),
    }
}

fn out_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("d24a-{}-{name}.mp4", std::process::id()))
}

fn settings() -> OsVideoSettings {
    OsVideoSettings {
        bitrate: 4_000_000,
        ..OsVideoSettings::default()
    }
}

#[derive(Default)]
struct Collect(Vec<(i64, Vec<u8>)>);
impl ChunkSink for Collect {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
        self.0.push((c.pts, c.bytes.to_vec()));
        Ok(())
    }
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a.iter().zip(b).map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2)).sum::<f64>() / a.len() as f64;
    if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0 * 255.0 / mse).log10() }
}

/// H.264 through the OS encoder as an ExportQueue output format: the receipt names `mf-h264-encode` and the
/// MFT (HELLO version), the published MP4 is re-indexed (verified), and the OS H.264 decoder decodes it back
/// to exactly 30 frames at n x frame duration whose luma PSNR against the source frames is >= `MIN_PSNR_DB`
/// (minimum and mean printed). A host without the OS H.264 encoder prints NOT_PROVEN and must answer
/// MEDIA_CODEC_UNAVAILABLE h264.
#[test]
fn h264_export_through_the_os_encoder_decodes_back() {
    bounded_case("h264_export", || {
        let encoder = WorkerMfEncoder::new(MfWorkerOptions::default().with_worker_program(WORKER)).unwrap();
        let path = out_path("h264");
        let mut queue = ExportQueue::new().with_video_encoder(Arc::new(encoder.clone()));
        queue.submit(job(VideoCodecChoice::H264(settings()), format(8, W), path.clone()));
        let receipt = queue.run().remove(0);
        println!("h264 encoder: {:?} {:?}", receipt.encoder_id, receipt.encoder_version);
        if let ExportOutcome::Failed { code: "MEDIA_CODEC_UNAVAILABLE", detail } = &receipt.outcome {
            println!("NOT_PROVEN: no OS H.264 encoder on this host ({detail})");
            return;
        }
        assert_eq!(receipt.outcome, ExportOutcome::Complete, "{receipt:?}");
        assert_eq!((receipt.video_codec, receipt.encoder_id), (Some("h264"), Some("mf-h264-encode")));
        assert!(receipt.encoder_version.unwrap().ends_with("|os-licensed"));
        assert!(receipt.verified && receipt.frames_encoded == FRAMES);
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let decoder = WorkerMfProvider::new(MfWorkerOptions::default().with_worker_program(WORKER)).unwrap();
        let registry = Registry::standard().with_video_decoder(Arc::new(decoder));
        let req = SegmentRequest {
            codec: ISOBMFF_PROVIDER_ID.to_owned(),
            start_pts: Ticks::new(0),
            count: FRAMES,
            track: None,
            output: OutputMode::Decoded,
            prefer: SourcePreference::Original,
            purpose: Purpose::Export,
            grant: Grant {
                content_id: ContentId::parse("os-encode-h264").unwrap(),
                revision: 1,
                expected_len: bytes.len() as u64,
            },
            expected_revision: 1,
        };
        let index = build_index(&mut Cursor::new(bytes.clone()), bytes.len() as u64, &Limits::default(), &CancellationToken::default()).unwrap();
        let table: Vec<(i64, i64, bool, u32)> = index.tracks[0].samples().iter().map(|s| (s.dts, s.pts, s.sync, s.size)).collect();
        println!("h264 sample table (dts, pts, sync, size): {table:?}");
        let mut sink = Collect::default();
        let back = registry.decode_segment(&mut Cursor::new(bytes), None, &req, &Limits::default(), &CancellationToken::default(), &mut sink);
        println!("decode-back pts: {:?}", sink.0.iter().map(|f| f.0).collect::<Vec<_>>());
        let back = back.unwrap();
        assert!(back.complete && back.decoder_id == "mf-h264", "{back:?}");
        assert_eq!(sink.0.len() as u64, FRAMES);
        let (mut min, mut sum) = (f64::INFINITY, 0.0);
        for (n, (pts, frame)) in sink.0.iter().enumerate() {
            assert_eq!(*pts, (n as u64 * TPF) as i64, "frame {n} pts");
            let source = planes(n as u64);
            assert_eq!(frame.len(), source.iter().map(Vec::len).sum::<usize>(), "frame {n} size");
            let p = psnr(&frame[..(W * H) as usize], &source[0]);
            min = min.min(p);
            sum += p;
        }
        println!("h264 decode-back: {} frames, luma PSNR min {min:.2} dB mean {:.2} dB", sink.0.len(), sum / FRAMES as f64);
        assert!(min >= MIN_PSNR_DB, "min PSNR {min}");
        let stats = encoder.stats(OsVideoCodec::H264);
        assert_eq!((stats.crashed, stats.protocol_faults), (0, 0), "{stats:?}");
    });
}

/// HEVC: the OS HEVC encoder is used where it exists (the Windows HEVC Video Extensions); the result is either
/// a verified hvc1 MP4 (decode-back NOT_PROVEN, no HEVC decoder in reel-native) or the typed refusal
/// MEDIA_CODEC_UNAVAILABLE hevc, never a silent fallback to another codec. The host's answer is printed.
/// Hostile, all refused before a frame is requested and before any worker starts: odd width, 10-bit frames,
/// a zero bitrate, and an H.264 recipe on a queue without an OS encoder (MEDIA_CODEC_UNAVAILABLE h264).
#[test]
fn hevc_is_os_encoded_or_typed_unavailable_and_bad_recipes_are_refused() {
    bounded_case("hevc_and_refusals", || {
        let encoder = WorkerMfEncoder::new(MfWorkerOptions::default().with_worker_program(WORKER)).unwrap();
        let path = out_path("hevc");
        let mut queue = ExportQueue::new().with_video_encoder(Arc::new(encoder.clone()));
        queue.submit(job(VideoCodecChoice::Hevc(settings()), format(8, W), path.clone()));
        let receipt = queue.run().remove(0);
        println!("HEVC on this host: {:?} encoder {:?} {:?}", receipt.outcome, receipt.encoder_id, receipt.encoder_version);
        match &receipt.outcome {
            ExportOutcome::Complete => {
                assert!(receipt.verified && receipt.frames_encoded == FRAMES);
                let bytes = std::fs::read(&path).unwrap();
                assert!(bytes.windows(4).any(|w| w == b"hvcC"));
                println!("NOT_PROVEN: HEVC decode-back (no HEVC decoder in reel-native)");
            }
            ExportOutcome::Failed { code, .. } => assert_eq!(*code, "MEDIA_CODEC_UNAVAILABLE"),
            other => panic!("unexpected {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
        // Refusals before any frame or worker.
        let fresh = WorkerMfEncoder::new(MfWorkerOptions::default().with_worker_program(WORKER)).unwrap();
        let cases = [
            (VideoCodecChoice::H264(settings()), format(8, W + 1), "MEDIA_UNSUPPORTED"),
            (VideoCodecChoice::H264(settings()), format(10, W), "MEDIA_UNSUPPORTED"),
            (VideoCodecChoice::H264(OsVideoSettings { bitrate: 0, ..settings() }), format(8, W), "MEDIA_LIMIT_EXCEEDED"),
        ];
        for (n, (choice, fmt, code)) in cases.into_iter().enumerate() {
            let mut queue = ExportQueue::new().with_video_encoder(Arc::new(fresh.clone()));
            queue.submit(job(choice, fmt, out_path(&format!("bad{n}"))));
            let r = queue.run().remove(0);
            assert!(matches!(&r.outcome, ExportOutcome::Failed { code: c, .. } if *c == code), "case {n}: {r:?}");
            assert_eq!(r.frames_encoded, 0);
        }
        assert_eq!(fresh.stats(OsVideoCodec::H264).spawned, 0, "refused before any worker started");
        let mut bare = ExportQueue::new();
        bare.submit(job(VideoCodecChoice::H264(settings()), format(8, W), out_path("bare")));
        let r = bare.run().remove(0);
        assert!(matches!(&r.outcome, ExportOutcome::Failed { code: "MEDIA_CODEC_UNAVAILABLE", detail } if detail.contains("h264")), "{r:?}");
    });
}
