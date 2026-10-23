//! Export queue (lane SMB-REEL-ENCODE; STU-VID-062/063/064): queued, headless, determinate progress,
//! cancellable, one receipt per item; refusals before the first frame is requested; files read back
//! through reel's demuxer and decoders. Sources are synthetic (generated here).
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_reel::*;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const TPF_25: u64 = 10_160_640_000;
const W: u32 = 264;
const H: u32 = 120;

fn format(bit_depth: u8, chroma: ChromaFormat) -> VideoFormat {
    VideoFormat {
        width: W,
        height: H,
        bit_depth,
        chroma,
        color: ColorInfo { primaries: 1, transfer: 1, matrix: 1, full_range: false, origin: ColorOrigin::SequenceHeader },
    }
}

/// Smooth moving pattern, frame `n`.
fn pattern(f: &VideoFormat, n: u64) -> Vec<Vec<u8>> {
    f.plane_dims()
        .iter()
        .enumerate()
        .map(|(p, &(w, h))| {
            let mut out = Vec::new();
            for y in 0..u64::from(h) {
                for x in 0..u64::from(w) {
                    let v = if p == 0 { 32 + (x * 3 + y + n * 4) % 180 } else { 96 + (x + 2 * y + n) % 64 };
                    if f.bit_depth > 8 {
                        out.extend_from_slice(&((v * 4) as u16).to_le_bytes());
                    } else {
                        out.push(v as u8);
                    }
                }
            }
            out
        })
        .collect()
}

struct Video {
    format: VideoFormat,
    frames: u64,
    calls: Arc<AtomicU64>,
    /// Cancel this handle when frame `.1` is requested.
    cancel_at: Option<(Arc<Mutex<Option<ExportHandle>>>, u64)>,
}

impl ExportVideoSource for Video {
    fn format(&self) -> VideoFormat {
        self.format
    }
    fn ticks_per_frame(&self) -> u64 {
        TPF_25
    }
    fn frame_count(&self) -> u64 {
        self.frames
    }
    fn frame(&mut self, index: u64, _cancel: &CancellationToken) -> Result<Vec<Vec<u8>>, ReelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some((handle, at)) = &self.cancel_at
            && index == *at
        {
            handle.lock().unwrap().as_ref().unwrap().cancel();
        }
        Ok(pattern(&self.format, index))
    }
}

struct Tone {
    rate: u32,
    channels: u16,
    frames: u64,
}

fn tone(frame: u64, channel: u16) -> f32 {
    let t = frame as f64 / 48_000.0;
    (0.5 * (2.0 * std::f64::consts::PI * (440.0 + 110.0 * f64::from(channel)) * t).sin()) as f32
}

impl ExportAudioSource for Tone {
    fn rate(&self) -> u32 {
        self.rate
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn frame_count(&self) -> u64 {
        self.frames
    }
    fn render(&mut self, first: u64, frames: u32, out: &mut Vec<f32>, _cancel: &CancellationToken) -> Result<(), ReelError> {
        for f in first..first + u64::from(frames) {
            for c in 0..self.channels {
                out.push(tone(f, c));
            }
        }
        Ok(())
    }
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("reel-export-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn entries(d: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

fn job(recipe: ExportRecipe, video: Option<Video>, audio: Option<Tone>, output: PathBuf) -> ExportJob {
    let frames = video.as_ref().map_or(0, |v| v.frames);
    ExportJob {
        recipe,
        video: video.map(|v| Box::new(v) as Box<dyn ExportVideoSource>),
        audio: audio.map(|a| Box::new(a) as Box<dyn ExportAudioSource>),
        span: SourceSpan { start_ticks: 0, end_ticks: frames.saturating_mul(TPF_25) },
        output,
        overwrite: false,
        artifact: "program-out".to_owned(),
        limits: ExportLimits::default(),
    }
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

fn read(bytes: &[u8], track: u32, count: u64) -> Collect {
    let request = SegmentRequest {
        codec: "isobmff".to_owned(),
        start_pts: Ticks::new(0),
        count,
        track: Some(track),
        output: OutputMode::Decoded,
        prefer: SourcePreference::Original,
        purpose: Purpose::Export,
        grant: Grant { content_id: ContentId::parse("export-queue").unwrap(), revision: 1, expected_len: bytes.len() as u64 },
        expected_revision: 1,
    };
    let mut sink = Collect::default();
    Registry::standard()
        .decode_segment(&mut Cursor::new(bytes.to_vec()), None, &request, &Limits::default(), &CancellationToken::default(), &mut sink)
        .expect("reel reads the export");
    sink
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let se: f64 = a.iter().zip(b).map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2)).sum();
    if se == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64 * 255.0 / (se / a.len() as f64)).log10() }
}

/// VC-3 HQ + 24-bit PCM into MOV through the queue: receipt (recipe, span, artifact, outcome, encoder,
/// SHA-256 of the published file), determinate progress 10/10, 10 frames back at exact presentation
/// times within the PSNR bound measured on this run, and exactly 19200 audio frames equal to the
/// quantized source.
#[test]
fn queue_exports_vc3_mov_with_pcm_and_reads_back() {
    let d = dir("vc3");
    let out = d.join("program.mov");
    let f = format(8, ChromaFormat::Yuv422);
    let recipe = ExportRecipe {
        recipe_id: "studio.dnxhr-hq.pcm24.mov@1".to_owned(),
        container: Container::Mov,
        video: Some(VideoCodecChoice::Vc3(Vc3Profile::DnxhrHq)),
        audio: Some(PcmChoice { bits: 24, float: false }),
    };
    let calls = Arc::new(AtomicU64::new(0));
    let mut queue = ExportQueue::new();
    let handle = queue.submit(job(
        recipe,
        Some(Video { format: f, frames: 10, calls: calls.clone(), cancel_at: None }),
        Some(Tone { rate: 48_000, channels: 2, frames: 19_200 }),
        out.clone(),
    ));
    assert_eq!(handle.progress().state, ExportState::Queued);
    let receipts = queue.run();
    assert_eq!(receipts.len(), 1);
    let r = &receipts[0];
    assert_eq!(r.outcome, ExportOutcome::Complete, "{r:?}");
    assert_eq!((r.recipe_id.as_str(), r.artifact.as_str()), ("studio.dnxhr-hq.pcm24.mov@1", "program-out"));
    assert_eq!((r.span.start_ticks, r.span.end_ticks), (0, 10 * TPF_25));
    assert_eq!((r.video_codec, r.encoder_id), (Some("vc3"), Some(VC3_ENCODER_ID)));
    assert_eq!((r.frames_encoded, r.audio_frames, r.audio_clipped), (10, 19_200, 0));
    assert!(r.verified, "re-indexed by reel before publishing");
    assert_eq!(handle.progress(), ExportProgress { state: ExportState::Complete, done: 10, total: 10 });
    assert_eq!(calls.load(Ordering::SeqCst), 10);
    let bytes = std::fs::read(&out).unwrap();
    assert_eq!(r.output_bytes, bytes.len() as u64);
    let sha = hex(&full_digest(&mut Cursor::new(bytes.clone()), 1 << 20, &CancellationToken::default()).unwrap());
    assert_eq!(r.output_sha256.as_deref(), Some(sha.as_str()));
    assert!(r.to_json().contains("\"outcome\":\"complete\""));
    assert_eq!(entries(&d), vec!["program.mov".to_owned()], "no temp file left");

    let video = read(&bytes, 1, 10);
    assert_eq!(video.chunks.len(), 10);
    for (n, (pts, got)) in video.chunks.iter().enumerate() {
        assert_eq!(*pts, (n as u64 * TPF_25) as i64);
        let src: Vec<u8> = pattern(&f, n as u64).concat();
        let db = psnr(got, &src);
        println!("frame {n}: {db:.2} dB");
        assert!(db >= 40.0, "frame {n}: {db:.2} dB");
    }
    let audio = read(&bytes, 2, 19_200);
    let got: Vec<u8> = audio.chunks.iter().flat_map(|c| c.1.clone()).collect();
    let mut want = Vec::new();
    for fr in 0..19_200u64 {
        for c in 0..2 {
            want.extend_from_slice(&((f64::from(tone(fr, c)) * 8_388_607.0).round() as i32).to_le_bytes()[..3]);
        }
    }
    assert_eq!(got, want, "PCM is the quantized mixdown, every frame");
    let _ = std::fs::remove_dir_all(&d);
}

/// One queue, four items: one runs, one is canceled before it starts (its source is never called), one is
/// refused before its first frame (VC-3 cannot go into MP4: STU-VID-062), one is canceled mid-run.
/// Refused and canceled items publish nothing and leave no temp file; receipts come per item.
#[test]
fn batch_queue_cancel_and_refusals_publish_nothing() {
    let d = dir("batch");
    let f = format(8, ChromaFormat::Yuv422);
    let mov = |id: &str| ExportRecipe {
        recipe_id: id.to_owned(),
        container: Container::Mov,
        video: Some(VideoCodecChoice::Vc3(Vc3Profile::DnxhrLb)),
        audio: None,
    };
    let counters: Vec<Arc<AtomicU64>> = (0..4).map(|_| Arc::new(AtomicU64::new(0))).collect();
    let late = Arc::new(Mutex::new(None));
    let mut queue = ExportQueue::new();
    let ok = queue.submit(job(mov("a"), Some(Video { format: f, frames: 3, calls: counters[0].clone(), cancel_at: None }), None, d.join("a.mov")));
    let early = queue.submit(job(mov("b"), Some(Video { format: f, frames: 3, calls: counters[1].clone(), cancel_at: None }), None, d.join("b.mov")));
    let mut mp4 = mov("c");
    mp4.container = Container::Mp4;
    queue.submit(job(mp4, Some(Video { format: f, frames: 3, calls: counters[2].clone(), cancel_at: None }), None, d.join("c.mp4")));
    let mid = queue.submit(job(mov("d"), Some(Video { format: f, frames: 8, calls: counters[3].clone(), cancel_at: Some((late.clone(), 3)) }), None, d.join("d.mov")));
    *late.lock().unwrap() = Some(mid.clone());
    early.cancel();
    let receipts = queue.run();
    let outcomes: Vec<_> = receipts.iter().map(|r| (r.recipe_id.clone(), r.outcome.clone())).collect();
    assert_eq!(receipts.len(), 4);
    assert_eq!(outcomes[0], ("a".to_owned(), ExportOutcome::Complete));
    assert_eq!(outcomes[1], ("b".to_owned(), ExportOutcome::Canceled));
    assert!(matches!(&outcomes[2].1, ExportOutcome::Failed { code: "MEDIA_UNSUPPORTED", detail } if detail.contains("mux-mp4-cannot-carry-vc3")), "{:?}", outcomes[2]);
    assert_eq!(outcomes[3], ("d".to_owned(), ExportOutcome::Canceled));
    assert_eq!(counters[1].load(Ordering::SeqCst), 0, "canceled before start: source never called");
    assert_eq!(counters[2].load(Ordering::SeqCst), 0, "refused before the first frame");
    assert_eq!(counters[3].load(Ordering::SeqCst), 4, "stops at the next frame after the cancel");
    assert_eq!(ok.progress().state, ExportState::Complete);
    assert_eq!(early.progress().state, ExportState::Canceled);
    let p = mid.progress();
    assert_eq!((p.state, p.total), (ExportState::Canceled, 8));
    assert!(p.done < 8);
    assert_eq!(entries(&d), vec!["a.mov".to_owned()], "only the complete item is published, no temp files");
    assert_eq!(queue.receipts().len(), 4);
    assert!(queue.run().is_empty(), "items run once");
    let _ = std::fs::remove_dir_all(&d);
}

/// Hostile and inconsistent jobs are refused before the source is asked for a frame.
#[test]
fn hostile_export_jobs_refused_before_any_frame() {
    let d = dir("hostile");
    let recipe = ExportRecipe {
        recipe_id: "r".to_owned(),
        container: Container::Mov,
        video: Some(VideoCodecChoice::Vc3(Vc3Profile::DnxhrHq)),
        audio: Some(PcmChoice { bits: 16, float: false }),
    };
    let calls = Arc::new(AtomicU64::new(0));
    let v = |format: VideoFormat, frames: u64| Video { format, frames, calls: calls.clone(), cancel_at: None };
    let good = format(8, ChromaFormat::Yuv422);
    let huge = VideoFormat { width: u32::MAX, height: u32::MAX, ..good };
    std::fs::write(d.join("exists.mov"), b"keep").unwrap();
    let mut cases: Vec<(ExportJob, &str)> = vec![
        (job(recipe.clone(), Some(v(good, u64::MAX)), Some(Tone { rate: 48_000, channels: 2, frames: 10 }), d.join("1.mov")), "export-frames"),
        (job(recipe.clone(), Some(v(huge, 2)), Some(Tone { rate: 48_000, channels: 2, frames: 10 }), d.join("2.mov")), "vc3-encode-dimensions"),
        (job(recipe.clone(), Some(v(good, 2)), Some(Tone { rate: u32::MAX, channels: 2, frames: 10 }), d.join("3.mov")), "mux-pcm-layout"),
        (job(recipe.clone(), Some(v(good, 2)), Some(Tone { rate: 48_000, channels: 2, frames: u64::MAX }), d.join("4.mov")), "export-audio-frames"),
        (job(recipe.clone(), Some(v(good, 2)), None, d.join("5.mov")), "export-audio-source-mismatch"),
        (job(recipe.clone(), Some(v(good, 0)), Some(Tone { rate: 48_000, channels: 2, frames: 10 }), d.join("6.mov")), "export-no-frames"),
        (job(recipe.clone(), Some(v(good, 2)), Some(Tone { rate: 48_000, channels: 2, frames: 10 }), d.join("exists.mov")), "export-output-exists"),
    ];
    let mut av1 = recipe.clone();
    av1.video = Some(VideoCodecChoice::Av1(Av1Settings { bitrate: Some(u32::MAX), ..Av1Settings::default() }));
    av1.container = Container::Mp4;
    #[cfg(feature = "av1-encode")]
    cases.push((job(av1, Some(v(format(8, ChromaFormat::Yuv420), 2)), Some(Tone { rate: 48_000, channels: 2, frames: 10 }), d.join("7.mp4")), "av1-encode-bitrate"));
    #[cfg(not(feature = "av1-encode"))]
    cases.push((job(av1, Some(v(format(8, ChromaFormat::Yuv420), 2)), Some(Tone { rate: 48_000, channels: 2, frames: 10 }), d.join("7.mp4")), "MEDIA_CODEC_UNAVAILABLE codec=av1"));
    let mut queue = ExportQueue::new();
    let expected: Vec<&str> = cases.iter().map(|c| c.1).collect();
    for (j, _) in cases.drain(..) {
        queue.submit(j);
    }
    for (r, what) in queue.run().iter().zip(expected) {
        match &r.outcome {
            ExportOutcome::Failed { detail, .. } => assert!(detail.contains(what), "{what}: {detail}"),
            other => panic!("{what}: {other:?}"),
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no frame requested by any refused job");
    assert_eq!(std::fs::read(d.join("exists.mov")).unwrap(), b"keep");
    assert_eq!(entries(&d), vec!["exists.mov".to_owned()]);
    let _ = std::fs::remove_dir_all(&d);
}

/// AV1 + float PCM into MP4 through the queue (feature av1-encode); decoded back through rav1d when the
/// av1 feature is on as well.
#[cfg(feature = "av1-encode")]
#[test]
fn queue_exports_av1_mp4() {
    let d = dir("av1");
    let out = d.join("program.mp4");
    let f = VideoFormat { width: 64, height: 48, ..format(8, ChromaFormat::Yuv420) };
    let recipe = ExportRecipe {
        recipe_id: "studio.av1.fpcm.mp4@1".to_owned(),
        container: Container::Mp4,
        video: Some(VideoCodecChoice::Av1(Av1Settings { speed: 10, quantizer: 40, ..Av1Settings::default() })),
        audio: Some(PcmChoice { bits: 32, float: true }),
    };
    let mut queue = ExportQueue::new();
    queue.submit(job(recipe, Some(Video { format: f, frames: 8, calls: Arc::new(AtomicU64::new(0)), cancel_at: None }), Some(Tone { rate: 48_000, channels: 2, frames: 15_360 }), out.clone()));
    let r = queue.run().remove(0);
    assert_eq!(r.outcome, ExportOutcome::Complete, "{r:?}");
    assert_eq!((r.video_codec, r.encoder_id, r.frames_encoded, r.audio_frames), (Some("av1"), Some(AV1_ENCODER_ID), 8, 15_360));
    assert!(r.verified, "MP4 fpcm item re-indexed (one table entry per audio frame)");
    let bytes = std::fs::read(&out).unwrap();
    let idx = build_index(&mut Cursor::new(bytes.clone()), bytes.len() as u64, &Limits::default(), &CancellationToken::default()).unwrap();
    assert_eq!((idx.tracks[0].codec, idx.tracks[1].codec), (*b"av01", *b"fpcm"));
    assert!(!parse_av1c(&idx.tracks[0].codec_config).unwrap().config_obus.is_empty());
    #[cfg(feature = "av1")]
    {
        let request = SegmentRequest {
            codec: "isobmff".to_owned(),
            start_pts: Ticks::new(0),
            count: 8,
            track: Some(1),
            output: OutputMode::Decoded,
            prefer: SourcePreference::Original,
            purpose: Purpose::Export,
            grant: Grant { content_id: ContentId::parse("export-av1").unwrap(), revision: 1, expected_len: bytes.len() as u64 },
            expected_revision: 1,
        };
        let mut sink = Collect::default();
        IsobmffProvider
            .decode_segment_routed(&mut Cursor::new(bytes.clone()), &request, &Limits::default(), &CancellationToken::default(), &mut sink, &VideoRoute::trusted())
            .unwrap();
        assert_eq!(sink.chunks.len(), 8);
        for (n, (pts, got)) in sink.chunks.iter().enumerate() {
            assert_eq!(*pts, (n as u64 * TPF_25) as i64);
            let db = psnr(got, &pattern(&f, n as u64).concat());
            println!("av1 export frame {n}: {db:.2} dB");
            assert!(db >= 30.0, "frame {n}: {db:.2} dB");
        }
    }
    #[cfg(not(feature = "av1"))]
    println!("NOT_PROVEN: reel feature av1 (rav1d) not enabled; AV1 export decode-back not checked");
    let _ = std::fs::remove_dir_all(&d);
}
