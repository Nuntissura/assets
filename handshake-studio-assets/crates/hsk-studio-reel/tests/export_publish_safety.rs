//! Export publish safety (lane SMB-PR-EXPORT-RECIPE-1 item 1; STU-IO-162 "never by racing", module docs of
//! `src/export.rs`): temp names are unique per item in the process and created with `create_new`, an item
//! removes only temps it created, every failure leaves no temp, a Failed receipt never hides published media,
//! and an existing output is replaced only by the rename. Sources are synthetic (generated here).
use hsk_studio_accord::CancellationToken;
use hsk_studio_reel::captions::{CaptionCue, CaptionLimits, ExportCaptions, TICKS_PER_MS};
use hsk_studio_reel::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

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

type Hook = Box<dyn FnMut(u64) + Send>;

/// Frames of a fixed pattern; `hook` runs before frame `index` is returned.
struct Video {
    frames: u64,
    calls: Arc<AtomicU64>,
    hook: Option<Hook>,
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
        if let Some(hook) = self.hook.as_mut() {
            hook(index);
        }
        Ok(format()
            .plane_dims()
            .iter()
            .map(|&(w, h)| (0..u64::from(w) * u64::from(h)).map(|i| (64 + (i + index * 7) % 128) as u8).collect())
            .collect())
    }
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("reel-publish-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn entries(d: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

fn vc3_mov(id: &str) -> ExportRecipe {
    ExportRecipe { recipe_id: id.to_owned(), container: Container::Mov, video: Some(VideoCodecChoice::Vc3(Vc3Profile::DnxhrLb)), audio: None }
}

fn job(recipe: ExportRecipe, video: Video, output: PathBuf, overwrite: bool) -> ExportJob {
    let frames = video.frames;
    ExportJob {
        recipe,
        video: Some(Box::new(video)),
        audio: None,
        span: SourceSpan { start_ticks: 0, end_ticks: frames * TPF_25 },
        output,
        overwrite,
        artifact: "out".to_owned(),
        limits: ExportLimits::default(),
    }
}

fn video(frames: u64, calls: &Arc<AtomicU64>, hook: Option<Hook>) -> Video {
    Video { frames, calls: calls.clone(), hook }
}

fn captions() -> ExportCaptions {
    let cue = CaptionCue { start_ticks: 0, end_ticks: 40 * TICKS_PER_MS, text: "een".into() };
    ExportCaptions { language: "nld".into(), cues: vec![cue], embed: false, sidecar: true, limits: CaptionLimits::default() }
}

fn run_one(job: ExportJob, with_captions: bool) -> ExportReceipt {
    let mut q = ExportQueue::new();
    if with_captions {
        q.submit_with_captions(job, captions());
    } else {
        q.submit(job);
    }
    q.run().remove(0)
}

fn sha_of(path: &Path) -> String {
    hex(&<sha2::Sha256 as sha2::Digest>::digest(std::fs::read(path).unwrap()))
}

fn failed_detail(r: &ExportReceipt) -> String {
    match &r.outcome {
        ExportOutcome::Failed { detail, .. } => detail.clone(),
        other => panic!("expected Failed, got {other:?}"),
    }
}

/// Item 1a: a file at the old fixed sidecar temp name (`<stem>.vtt.partial`) survives a captioned export byte
/// for byte; the sidecar is published from a temp this item created.
#[test]
fn sidecar_temp_never_overwrites_an_existing_file() {
    let d = dir("a");
    let sentinel = d.join("out.vtt.partial");
    std::fs::write(&sentinel, b"sentinel: not the export's").unwrap();
    let calls = Arc::new(AtomicU64::new(0));
    let r = run_one(job(vc3_mov("a"), video(3, &calls, None), d.join("out.mov"), false), true);
    assert_eq!(r.outcome, ExportOutcome::Complete, "{r:?}");
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"sentinel: not the export's");
    assert!(std::fs::read_to_string(d.join("out.vtt")).unwrap().starts_with("WEBVTT\n"));
    assert_eq!(entries(&d), ["out.mov", "out.vtt", "out.vtt.partial"]);
    let _ = std::fs::remove_dir_all(&d);
}

/// Holds queue A inside frame 1 until released, in another thread; returns the release sender and A's join.
fn held_queue_a(out: PathBuf) -> (mpsc::Sender<()>, std::thread::JoinHandle<ExportReceipt>) {
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let hook: Hook = Box::new(move |index| {
        if index == 1 {
            started_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(120));
        }
    });
    let calls = Arc::new(AtomicU64::new(0));
    let a = std::thread::spawn(move || run_one(job(vc3_mov("a"), video(3, &calls, Some(hook)), out, false), false));
    started_rx.recv_timeout(Duration::from_secs(120)).expect("queue A reached frame 1");
    (release_tx, a)
}

/// Item 1b: in one process, queue B refused on queue A's output while A is mid-run (its temp file open)
/// removes nothing of A's: A ends Complete with its own file published.
#[test]
fn a_refused_item_never_removes_another_queues_live_temp() {
    let d = dir("b1");
    let out = d.join("shared.mov");
    let (release, a) = held_queue_a(out.clone());
    let calls = Arc::new(AtomicU64::new(0));
    let mut mp4 = vc3_mov("b");
    mp4.container = Container::Mp4;
    let b = run_one(job(mp4, video(3, &calls, None), out.clone(), false), false);
    release.send(()).unwrap();
    let a = a.join().unwrap();
    assert!(failed_detail(&b).contains("mux-mp4-cannot-carry-vc3"), "{b:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "B refused before its first frame");
    assert_eq!(a.outcome, ExportOutcome::Complete, "{a:?}");
    assert_eq!(a.output_sha256.as_deref(), Some(sha_of(&out).as_str()), "A's own bytes are published");
    assert_eq!(entries(&d), ["shared.mov"], "no temp left");
    let _ = std::fs::remove_dir_all(&d);
}

/// Item 1b, the publish side: queue C publishes A's output while A is mid-run; A, without `overwrite`, never
/// replaces C's file (STU-IO-162: never by racing) and leaves no temp.
#[test]
fn a_publish_without_overwrite_never_replaces_an_output_that_appeared() {
    let d = dir("b2");
    let out = d.join("shared.mov");
    let (release, a) = held_queue_a(out.clone());
    let calls = Arc::new(AtomicU64::new(0));
    let c = run_one(job(vc3_mov("c"), video(2, &calls, None), out.clone(), false), false);
    release.send(()).unwrap();
    let a = a.join().unwrap();
    assert_eq!(c.outcome, ExportOutcome::Complete, "{c:?}");
    assert!(failed_detail(&a).contains("export-output-exists"), "{a:?}");
    assert_eq!(c.output_sha256.as_deref(), Some(sha_of(&out).as_str()), "C's file is untouched");
    assert_eq!(entries(&d), ["shared.mov"], "no temp left");
    let _ = std::fs::remove_dir_all(&d);
}

/// Item 1c: an output path that is a directory is refused before the first frame, and nothing is left.
#[test]
fn output_directory_is_refused_before_the_first_frame() {
    let d = dir("c1");
    std::fs::create_dir(d.join("out.mov")).unwrap();
    let calls = Arc::new(AtomicU64::new(0));
    let r = run_one(job(vc3_mov("c1"), video(3, &calls, None), d.join("out.mov"), true), true);
    assert!(failed_detail(&r).contains("export-output-path"), "{r:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(entries(&d), ["out.mov"]);
    let _ = std::fs::remove_dir_all(&d);
}

/// Item 1c: a failure after the sidecar temp was written (the output turns into a directory mid-run, with
/// `overwrite`) leaves neither temp behind and publishes nothing.
#[test]
fn a_failure_after_the_sidecar_temp_leaves_no_temp() {
    let d = dir("c2");
    let out = d.join("out.mov");
    let at = out.clone();
    let hook: Hook = Box::new(move |index| {
        if index == 1 {
            std::fs::create_dir(&at).unwrap();
        }
    });
    let calls = Arc::new(AtomicU64::new(0));
    let r = run_one(job(vc3_mov("c2"), video(3, &calls, Some(hook)), out, true), true);
    let _ = failed_detail(&r);
    assert!(!r.published_media && !r.published_sidecar, "{r:?}");
    assert_eq!(entries(&d), ["out.mov"], "only the directory: no media, no sidecar, no temp");
    assert!(std::fs::metadata(d.join("out.mov")).unwrap().is_dir());
    let _ = std::fs::remove_dir_all(&d);
}

/// Item 1d: a sidecar path that is a directory is refused before the first frame: the media is never
/// published under a Failed receipt.
#[test]
fn sidecar_directory_is_refused_before_the_first_frame() {
    let d = dir("d1");
    std::fs::create_dir(d.join("out.vtt")).unwrap();
    let calls = Arc::new(AtomicU64::new(0));
    let r = run_one(job(vc3_mov("d1"), video(3, &calls, None), d.join("out.mov"), true), true);
    assert!(failed_detail(&r).contains("export-sidecar-path"), "{r:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(entries(&d), ["out.vtt"], "no media published");
    let _ = std::fs::remove_dir_all(&d);
}

/// Item 1d: the sidecar path turning into a directory mid-run is found before the media is renamed: Failed,
/// and nothing published, no temp.
#[test]
fn sidecar_directory_appearing_mid_run_publishes_nothing() {
    let d = dir("d2");
    let side = d.join("out.vtt");
    let hook: Hook = Box::new(move |index| {
        if index == 1 {
            std::fs::create_dir(&side).unwrap();
        }
    });
    let calls = Arc::new(AtomicU64::new(0));
    let r = run_one(job(vc3_mov("d2"), video(3, &calls, Some(hook)), d.join("out.mov"), true), true);
    assert!(failed_detail(&r).contains("export-sidecar-path"), "{r:?}");
    assert!(!r.published_media && r.output_sha256.is_none(), "{r:?}");
    assert_eq!(entries(&d), ["out.vtt"], "no media published, no temp");
    let _ = std::fs::remove_dir_all(&d);
}

/// Item 1d, the case no check can pre-empt: the sidecar rename fails after the media rename (the old sidecar
/// is held open without delete sharing, so Windows refuses to replace it). The receipt is Failed and states
/// that the media is published, with the facts of the file now at the output path; the old sidecar is intact
/// and no temp is left. Windows only: a POSIX rename replaces an open file.
#[cfg(windows)]
#[test]
fn a_sidecar_rename_failure_after_the_media_states_what_was_published() {
    use std::os::windows::fs::OpenOptionsExt;
    let d = dir("d3");
    std::fs::write(d.join("out.vtt"), b"old sidecar").unwrap();
    let lock = std::fs::OpenOptions::new().read(true).share_mode(0).open(d.join("out.vtt")).unwrap();
    let calls = Arc::new(AtomicU64::new(0));
    let r = run_one(job(vc3_mov("d3"), video(2, &calls, None), d.join("out.mov"), true), true);
    drop(lock);
    assert!(matches!(r.outcome, ExportOutcome::Failed { code: "MEDIA_IO", .. }), "{r:?}");
    assert!(r.published_media && !r.published_sidecar, "{r:?}");
    assert_eq!(r.output_sha256.as_deref(), Some(sha_of(&d.join("out.mov")).as_str()));
    assert!(r.to_json().contains("\"published\":{\"media\":true,\"sidecar\":false}"), "{}", r.to_json());
    assert_eq!(std::fs::read(d.join("out.vtt")).unwrap(), b"old sidecar");
    assert_eq!(entries(&d), ["out.mov", "out.vtt"], "no temp left");
    let _ = std::fs::remove_dir_all(&d);
}

/// Item 1e: with `overwrite`, an existing output and sidecar are replaced by the rename (no delete first).
#[test]
fn overwrite_replaces_output_and_sidecar() {
    let d = dir("e");
    std::fs::write(d.join("out.mov"), b"old media").unwrap();
    std::fs::write(d.join("out.vtt"), b"old sidecar").unwrap();
    let calls = Arc::new(AtomicU64::new(0));
    let r = run_one(job(vc3_mov("e"), video(2, &calls, None), d.join("out.mov"), true), true);
    assert_eq!(r.outcome, ExportOutcome::Complete, "{r:?}");
    assert!(r.published_media && r.published_sidecar, "{r:?}");
    assert_eq!(r.output_sha256.as_deref(), Some(sha_of(&d.join("out.mov")).as_str()));
    assert!(std::fs::read_to_string(d.join("out.vtt")).unwrap().starts_with("WEBVTT\n"));
    assert_eq!(entries(&d), ["out.mov", "out.vtt"]);
    let _ = std::fs::remove_dir_all(&d);
}
