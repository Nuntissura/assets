//! Lane SMB-R60-FOLLOWUPS-1 item 5 (validation round R60 attempt 2 probe k1): an output whose name Windows
//! resolves to `*.vtt` (trailing dots and spaces stripped, any case) with a captions sidecar is refused before
//! the first frame, as `x.vtt` is (review F1 of c79c858d), so the sidecar never replaces the media under a
//! Complete receipt. Sources are synthetic (generated here).
use hsk_studio_accord::CancellationToken;
use hsk_studio_reel::captions::{CaptionCue, CaptionLimits, ExportCaptions, TICKS_PER_MS};
use hsk_studio_reel::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const TPF_25: u64 = 10_160_640_000;

fn format() -> VideoFormat {
    VideoFormat {
        width: 264,
        height: 120,
        bit_depth: 8,
        chroma: ChromaFormat::Yuv422,
        color: ColorInfo { primaries: 1, transfer: 1, matrix: 1, full_range: false, origin: ColorOrigin::SequenceHeader },
    }
}

struct Video {
    frames: u64,
    calls: Arc<AtomicU64>,
}

impl ExportVideoSource for Video {
    fn format(&self) -> VideoFormat {
        format()
    }
    fn ticks_per_frame(&self) -> u64 {
        TPF_25
    }
    fn frame_count(&self) -> u64 {
        self.frames
    }
    fn frame(&mut self, index: u64, _cancel: &CancellationToken) -> Result<Vec<Vec<u8>>, ReelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(format().plane_dims().iter().map(|&(w, h)| (0..u64::from(w) * u64::from(h)).map(|i| (64 + (i + index * 7) % 128) as u8).collect()).collect())
    }
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("reel-r60-vtt-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn entries(d: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

fn job(frames: u64, calls: &Arc<AtomicU64>, output: PathBuf, overwrite: bool) -> ExportJob {
    ExportJob {
        recipe: ExportRecipe { recipe_id: "k1".to_owned(), container: Container::Mov, video: Some(VideoCodecChoice::Vc3(Vc3Profile::DnxhrLb)), audio: None },
        video: Some(Box::new(Video { frames, calls: calls.clone() })),
        audio: None,
        span: SourceSpan { start_ticks: 0, end_ticks: frames * TPF_25 },
        output,
        overwrite,
        artifact: "out".to_owned(),
        limits: ExportLimits::default(),
    }
}

fn captions() -> ExportCaptions {
    let cue = CaptionCue { start_ticks: 0, end_ticks: 40 * TICKS_PER_MS, text: "een".into() };
    ExportCaptions { language: "nld".into(), cues: vec![cue], embed: false, sidecar: true, limits: CaptionLimits::default() }
}

fn run_one(job: ExportJob) -> ExportReceipt {
    let mut q = ExportQueue::new();
    q.submit_with_captions(job, captions());
    q.run().remove(0)
}

/// Probe k1: `x.vtt ` (Windows strips the trailing space, so the output is `x.vtt`, the sidecar path) and its
/// dot and space variants, any case, with and without overwrite: refused `export-sidecar-path` before the
/// first frame, nothing written.
#[test]
fn r60_k1_an_output_windows_resolves_to_vtt_with_a_sidecar_is_refused() {
    for (k, name) in ["x.vtt ", "x.VTT ", "x.vtt.", "x.vtt. ", "x.Vtt . .", "x.vtt  "].into_iter().enumerate() {
        for overwrite in [true, false] {
            let d = dir(&format!("{k}-{overwrite}"));
            let calls = Arc::new(AtomicU64::new(0));
            let r = run_one(job(2, &calls, d.join(name), overwrite));
            match &r.outcome {
                ExportOutcome::Failed { detail, .. } => assert!(detail.contains("export-sidecar-path"), "{name:?} overwrite {overwrite}: {r:?}"),
                other => panic!("{name:?} overwrite {overwrite}: expected Failed export-sidecar-path, got {other:?} ({:?})", entries(&d)),
            }
            assert_eq!(calls.load(Ordering::SeqCst), 0, "{name:?}: refused before the first frame");
            assert!(!r.published_media && !r.published_sidecar, "{name:?}: {r:?}");
            assert!(entries(&d).is_empty(), "{name:?}: {:?}", entries(&d));
            let _ = std::fs::remove_dir_all(&d);
        }
    }
}

/// Control: a name that only contains `.vtt` (`x.vtt.mov`) and a plain one stay accepted, and the sidecar
/// is written beside the media.
#[test]
fn r60_k1_names_that_do_not_resolve_to_vtt_still_export() {
    for name in ["x.vtt.mov", "x.mov"] {
        let d = dir(&format!("ok-{name}"));
        let calls = Arc::new(AtomicU64::new(0));
        let r = run_one(job(2, &calls, d.join(name), false));
        assert_eq!(r.outcome, ExportOutcome::Complete, "{name}: {r:?}");
        assert!(r.published_media && r.published_sidecar, "{name}: {r:?}");
        assert_eq!(entries(&d).len(), 2, "{name}: {:?}", entries(&d));
        let _ = std::fs::remove_dir_all(&d);
    }
}
