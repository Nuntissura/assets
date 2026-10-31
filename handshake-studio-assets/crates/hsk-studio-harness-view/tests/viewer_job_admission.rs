//! Lane SMB-JOB-ADMISSION-1 (RF-08 phase 0; feature `viewer`; the export and audio batch cases need
//! `media` / `audio-batch`, which the round unit `["desktop", "audio-batch"]` enables): the viewer's
//! local queues admit through the session `JobPort` with typed outcomes (STU-ARC-008 / -013,
//! GUI-DESKTOP-033..035), and the preview gate never shows a superseded frame as current.
//!
//! Oracles: port doubles that record every admit and release with its ticket; a frame source that
//! counts every frame it is asked for; the fixture's hand-picked frames (nested clip `a` is red);
//! hand-counted ranges (25 fps). No toolkit code: every step goes through Viewer commands.
#![cfg(feature = "viewer")]

#[path = "../../hsk-studio-harness/tests/common/mod.rs"]
mod common;
mod support_context;

#[cfg(feature = "media")]
use hsk_studio_accord::CancellationToken;
#[cfg(feature = "media")]
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::render::Prefer;
use serde_json::Value;
use std::path::PathBuf;
#[cfg(feature = "media")]
use std::path::Path;
#[cfg(feature = "media")]
use hsk_studio_session::{Cost, JobPort, JobRefused, JobScope, JobTicket};
#[cfg(feature = "media")]
use std::sync::{Arc, Mutex};

#[cfg(feature = "media")]
const DOC: &str = "SDOC-019abcde-0000-7000-8000-000000000009";
#[cfg(feature = "media")]
const LAYER: &str = "SLYR-019abcde-0000-7000-8000-000000000001";

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let base = std::env::temp_dir();
        let text = base.to_string_lossy().to_ascii_uppercase();
        assert!(!(cfg!(windows) && text.starts_with("C:")), "TMP is on C: ({}); run through studio-env.sh", base.display());
        let dir = base.join(format!("hsk-harness-view-job-admission-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Port double: admits (or refuses with `refuse`), records every admit and release.
#[cfg(feature = "media")]
#[derive(Default)]
struct Recording {
    refuse: Option<u64>,
    log: Mutex<(u64, Vec<u64>, Vec<u64>)>,
}
#[cfg(feature = "media")]
impl Recording {
    fn admitted(&self) -> Vec<u64> {
        self.log.lock().unwrap().1.clone()
    }
    fn released(&self) -> Vec<u64> {
        self.log.lock().unwrap().2.clone()
    }
    fn calls(&self) -> u64 {
        self.log.lock().unwrap().0
    }
}
#[cfg(feature = "media")]
impl JobPort for Recording {
    fn admit(&self, scope: &JobScope<'_>, _cost: Cost) -> Result<JobTicket, JobRefused> {
        let mut log = self.log.lock().unwrap();
        log.0 += 1;
        assert_eq!(scope.resource.as_str(), DOC, "the open document is the job's resource");
        if let Some(retry_after_ms) = self.refuse {
            return Err(JobRefused { retry_after_ms });
        }
        let ticket = log.0;
        log.1.push(ticket);
        Ok(JobTicket(ticket))
    }
    fn release(&self, ticket: JobTicket) {
        self.log.lock().unwrap().2.push(ticket.0);
    }
}

#[cfg(feature = "media")]
struct Any;
#[cfg(feature = "media")]
impl folio::Resolver for Any {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
    fn media(&self, _: &str, _: &ContentDigest) -> Resolution {
        Resolution::Available
    }
}

#[cfg(feature = "media")]
fn package(dir: &Path, sequences: Vec<StudioSequence>) -> PathBuf {
    let doc = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":DOC,"revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":LAYER,"name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":sequences.iter().map(|s| serde_json::to_value(s).unwrap()).collect::<Vec<_>>()
    });
    let token = CancellationToken::default();
    let snapshot = match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), Budget::default(), &token, &Any).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read-only fixture"),
    };
    let assets = hsk_studio_package::MemoryAssetSource::new();
    let mut bytes = Vec::new();
    hsk_studio_package::write_package_with(
        &snapshot,
        &assets as &dyn hsk_studio_package::AssetSource,
        &hsk_studio_package::Extras { quarantined: &[], previews: &[] },
        &hsk_studio_package::WriteOptions::default(),
        &hsk_studio_package::Limits::default(),
        &token,
        &mut bytes,
    )
    .unwrap();
    let path = dir.join("job-admission.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

fn status(v: &Viewer) -> Value {
    v.state_json()["status"].clone()
}

fn head(v: &Viewer) -> u64 {
    v.run().snapshot().unwrap().document().revision
}

/// A preview frame rendered at one revision and offered after an edit moved the head is recorded
/// `superseded` (counter, session job counter, status line naming both revisions) and not shown:
/// the displayed frame stays the head's; the next render shows the head and is not superseded.
#[test]
fn preview_frame_older_than_the_head_is_superseded_and_not_shown() {
    let scratch = Scratch::new("preview");
    let n = 16 * 8;
    let psd = common::layered_psd(16, 8, &[common::L { name: "red", rect: [0, 0, 8, 16], channels: vec![(0, vec![200; n]), (1, vec![30; n]), (2, vec![40; n])] }]);
    let path = scratch.0.join("preview.psd");
    std::fs::write(&path, psd).unwrap();
    let mut v = crate::support_context::viewer(Prefer::Cpu);
    v.open(&path).unwrap_or_else(|e| panic!("{e}"));
    v.render_if_dirty();
    let layer = v.layers().into_iter().find(|r| r.name == "red").expect("the layer").layer_id;
    let old = v.frame().cloned().expect("a frame of the opened revision");
    assert_eq!(old.revision, head(&v));
    let moved = v.rename_layer(&layer, "renamed").unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
    assert!(moved > old.revision, "the edit moved the head");
    v.render_if_dirty();
    assert_eq!(v.frame().map(|f| f.revision), Some(moved), "the head's frame is shown");
    assert_eq!(v.preview_superseded(), 0, "a current frame is not superseded");

    assert!(v.gate_preview(Ok(old.clone())).is_none(), "the old frame is not offered for display");
    assert_eq!(v.frame().map(|f| f.revision), Some(moved), "the displayed frame is still the head's");
    assert_eq!(v.preview_superseded(), 1);
    let gate = v.preview_gate_json();
    assert_eq!((gate["last"]["frame_revision"].as_u64(), gate["last"]["head_revision"].as_u64()), (Some(old.revision), Some(moved)), "{gate}");
    assert_eq!(v.job_counts().superseded, 1, "recorded through the session lifecycle");
    let log = status(&v).to_string();
    assert!(log.contains(&format!("frame of revision {} superseded by revision {moved}", old.revision)), "{log}");
}

#[cfg(feature = "audio-batch")]
mod audio_batch {
    use super::*;
    use hsk_studio_accord::{ActorContext, DomainId};
    use hsk_studio_harness_view::view::audio_batch::{AudioBatchJob, AudioBatchQueue, BatchOutcome};
    use hsk_studio_harness_view::view::audio_import::{ImportLimits, ImportOptions, MixFormat};
    use hsk_studio_harness_view::view::job_admission::JobAdmission;
    use hsk_studio_session::JobLifecycle;

    fn actor() -> ActorContext {
        ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap()
    }

    /// Float32 stereo WAV of `frames` frames at 48 kHz.
    fn wav(frames: usize) -> Vec<u8> {
        let mut data = Vec::new();
        for i in 0..frames {
            let v = ((i as f32) * 0.01).sin() * 0.5;
            data.extend_from_slice(&v.to_le_bytes());
            data.extend_from_slice(&(-v).to_le_bytes());
        }
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&3u16.to_le_bytes());
        fmt.extend_from_slice(&2u16.to_le_bytes());
        fmt.extend_from_slice(&48_000u32.to_le_bytes());
        fmt.extend_from_slice(&(48_000u32 * 8).to_le_bytes());
        fmt.extend_from_slice(&8u16.to_le_bytes());
        fmt.extend_from_slice(&32u16.to_le_bytes());
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((4 + 8 + fmt.len() + 8 + data.len()) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
        out.extend_from_slice(&fmt);
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    fn item(input: PathBuf, output: PathBuf) -> AudioBatchJob {
        AudioBatchJob {
            artifact: output.file_name().unwrap().to_string_lossy().into_owned(),
            input,
            output,
            overwrite: false,
            options: ImportOptions::default(),
            limits: ImportLimits::default(),
            chain: Vec::new(),
            keyframes: Vec::new(),
            format: MixFormat::Flac { bits: 16 },
        }
    }

    /// Each AudioBatchQueue item is admitted before it starts and settled once from its own
    /// receipt: a complete item `completed`, a failing one `failed` (its code), a canceled-before-
    /// start item refused `canceled` without asking the port; the port saw two admits and exactly
    /// two releases of the same tickets.
    #[test]
    fn audio_batch_items_settle_once_per_receipt() {
        let scratch = Scratch::new("batch");
        let input = scratch.0.join("tone.wav");
        std::fs::write(&input, wav(4800)).unwrap();
        let port = Arc::new(Recording::default());
        let lifecycle = JobLifecycle::default();
        let admission = JobAdmission::new(port.clone(), lifecycle.clone(), actor(), Some(DomainId::parse(DOC).unwrap())).with_revision(7);
        let mut queue = AudioBatchQueue::new(actor()).with_jobs(admission);
        let _ok = queue.submit(item(input, scratch.0.join("tone.flac")));
        let _missing = queue.submit(item(scratch.0.join("missing.wav"), scratch.0.join("missing.flac")));
        let canceled = queue.submit(item(scratch.0.join("never.wav"), scratch.0.join("never.flac")));
        canceled.cancel();
        let receipts = queue.run();
        assert_eq!(receipts.len(), 3);
        assert_eq!(receipts[0].outcome, BatchOutcome::Complete, "{}", receipts[0].to_json());
        assert_eq!((receipts[0].job["outcome"].as_str(), receipts[0].job["ticket"].as_u64(), receipts[0].job["bound_revision"].as_u64()), (Some("completed"), Some(1), Some(7)), "{}", receipts[0].job);
        assert!(matches!(receipts[1].outcome, BatchOutcome::Failed { .. }), "{}", receipts[1].to_json());
        assert_eq!((receipts[1].job["outcome"].as_str(), receipts[1].job["ticket"].as_u64()), (Some("failed"), Some(2)), "{}", receipts[1].job);
        assert_eq!(receipts[2].outcome, BatchOutcome::Canceled);
        assert_eq!(receipts[2].job["refused"], "canceled", "{}", receipts[2].job);
        assert_eq!((port.admitted(), port.released()), (vec![1, 2], vec![1, 2]), "two admits, each released exactly once");
        let c = lifecycle.counts();
        assert_eq!((c.admitted, c.completed, c.failed, c.refused_canceled, c.released, c.in_flight), (2, 1, 1, 1, 2, 0), "{c:?}");
        assert!(scratch.0.join("tone.flac").is_file());
    }
}

#[cfg(feature = "media")]
mod export_media {
    use super::*;
    use hsk_studio_accord::Ticks;
    use hsk_studio_harness_view::view::monitor::{FrameSource, SourceFrame};
    use hsk_studio_harness_view::view::shell::{Panel, ShellCommand};
    use hsk_studio_harness_view::view::timeline::{ExportSequenceRequest, TimelineCommand, widget};
    use hsk_studio_pulse::SourceExtents;
    use hsk_studio_reel as reel;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 25 fps ticks per frame; 48 kHz has 1920 samples per frame.
    const F: u64 = 10_160_640_000;
    const SPF: u64 = 1920;
    const RED: [f32; 3] = [0.8, 0.2, 0.1];
    const BLUE: [f32; 3] = [0.1, 0.4, 0.9];
    const PRESET: &str = "studio.dnxhr.hq.mov";

    fn cid(n: u32) -> String {
        format!("SCLP-019abcde-0000-7000-8000-{n:012x}")
    }
    fn media(name: &str) -> ClipSource {
        ClipSource::Media { artifact_manifest_id: name.into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() }, asset_id: None }
    }
    fn clip(n: u32, source: ClipSource, si: u64, so: u64, start: u64, group: Option<&str>) -> StudioClip {
        StudioClip {
            schema_id: ClipSchema::V1,
            clip_id: cid(n),
            source_ref: source,
            source_in: Ticks::new(si * F),
            source_out: Ticks::new(so * F),
            timeline_start: Ticks::new(start * F),
            duration: Ticks::new((so - si) * F),
            speed: Speed::UNIT,
            enabled: true,
            link_group: group.map(str::to_owned),
            markers: Vec::new(),
            transitions: ClipTransitions::default(),
        }
    }
    fn track(n: u32, kind: TrackKind, name: &str, clips: Vec<StudioClip>) -> StudioTrack {
        StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: format!("STRK-019abcde-0000-7000-8000-{n:012x}"),
            kind,
            index: 0,
            name: name.into(),
            enabled: true,
            locked: false,
            sync_locked: true,
            targeted: true,
            muted: false,
            soloed: false,
            clips,
            volume: 1.0,
            pan: 0.0,
        }
    }
    /// V1 clip 1 `a` (red) source [10, 30) at frames [0, 20) linked with A1 clip 3 `d`; V1 clip 2
    /// `b` (blue) at [30, 40). Work area [5, 25).
    fn main_sequence() -> StudioSequence {
        let mut s = StudioSequence {
            schema_id: SequenceSchema::V1,
            sequence_id: "seq-main".into(),
            name: "seq-main".into(),
            frame_rate: SequenceFrameRate { ticks_per_frame: F },
            duration: Ticks::new(0),
            tracks: vec![
                track(1, TrackKind::Video, "V1", vec![clip(1, media("a"), 10, 30, 0, Some("lg-1")), clip(2, media("b"), 0, 10, 30, None)]),
                track(2, TrackKind::Audio, "A1", vec![clip(3, media("d"), 0, 20, 0, Some("lg-1"))]),
            ],
            markers: Vec::new(),
            work_area: Some(WorkArea { start: Ticks::new(5 * F), end: Ticks::new(25 * F) }),
        };
        s.duration = s.content_end().unwrap();
        s
    }

    fn solid(rgb: [f32; 3]) -> Arc<SourceFrame> {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for _ in 0..16 {
            for c in rgb {
                colour.extend_from_slice(&c.to_le_bytes());
            }
            alpha.extend_from_slice(&1.0f32.to_le_bytes());
        }
        Arc::new(SourceFrame { width: 4, height: 4, colour_sha256: hsk_studio_prism::profile_hash(&colour), alpha_sha256: hsk_studio_prism::profile_hash(&alpha), colour, alpha })
    }
    /// Frames by media name (`a` red, `b` blue); counts every frame it is asked for.
    struct Solids(Arc<AtomicU64>);
    impl FrameSource for Solids {
        fn frame(&self, source: &ClipSource, _: Ticks) -> Option<Arc<SourceFrame>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let ClipSource::Media { artifact_manifest_id: id, .. } = source else { return None };
            match id.as_str() {
                "a" => Some(solid(RED)),
                "b" => Some(solid(BLUE)),
                _ => None,
            }
        }
    }
    #[derive(Clone, Copy)]
    struct Known;
    impl SourceExtents for Known {
        fn extent(&self, _: &ClipSource) -> Option<Ticks> {
            Some(Ticks::new(1000 * F))
        }
    }
    #[derive(Clone)]
    struct Tone(Arc<Vec<Vec<f32>>>);
    impl hsk_studio_pulse::AudioSources for Tone {
        fn audio(&self, s: &ClipSource) -> Option<&[Vec<f32>]> {
            matches!(s, ClipSource::Media { artifact_manifest_id: id, .. } if id == "d").then_some(self.0.as_slice())
        }
    }
    fn tone() -> Tone {
        let n = (40 * SPF) as usize;
        Tone(Arc::new(vec![(0..n).map(|i| ((i as f32) * 0.01).sin() * 0.5).collect(), (0..n).map(|i| ((i as f32) * 0.013).cos() * 0.25).collect()]))
    }

    /// The fixture open with the program monitor's counting frame source, the tone as the sound
    /// port, clip 1 nested, and `port` bound as the viewer's job port.
    fn viewer(label: &str, port: Arc<Recording>) -> (Scratch, Viewer, Arc<AtomicU64>) {
        let scratch = Scratch::new(label);
        let frames = Arc::new(AtomicU64::new(0));
        let mut v = crate::support_context::viewer(Prefer::Cpu);
        v.open(&package(&scratch.0, vec![main_sequence()])).unwrap_or_else(|e| panic!("{e}"));
        v.open_monitor("seq-main", 64, 32).unwrap();
        v.set_monitor_sources(Box::new(Solids(Arc::clone(&frames))), Box::new(Known));
        v.shell_command(ShellCommand::Panel(Panel::Timeline)).unwrap();
        v.set_export_audio(tone(), Arc::new(Known), 48_000).unwrap();
        v.timeline_command(TimelineCommand::Nest(vec![cid(1)])).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
        v.set_job_port(port);
        v.save_input = scratch.0.join("project.handshake").to_string_lossy().into_owned();
        frames.store(0, Ordering::SeqCst);
        (scratch, v, frames)
    }

    /// Opens the Export Media form for frames [10, 19] at 64x32 into `output` and starts it.
    fn start(v: &mut Viewer, output: &str) {
        v.timeline_command(TimelineCommand::ExportMediaOpen).unwrap();
        for (id, value) in [(widget::EXPORT_IN, "10"), (widget::EXPORT_OUT, "19"), (widget::EXPORT_WIDTH, "64"), (widget::EXPORT_HEIGHT, "32"), (widget::EXPORT_OUTPUT, output)] {
            v.set_timeline_export_field(id, value).unwrap_or_else(|e| panic!("{id}: {e}"));
        }
        v.timeline_command(TimelineCommand::ExportMediaRun).unwrap_or_else(|e| panic!("{e}: {}", status(v)));
    }

    /// A saturated port refuses the export typed `overloaded` (with its retry hint) before
    /// anything starts: no frame is ever asked for, no file appears, nothing is running and no
    /// ticket exists to release; the headless and the window path both refuse.
    #[test]
    fn export_with_a_saturated_port_is_refused_typed_before_anything_starts() {
        let port = Arc::new(Recording { refuse: Some(250), ..Recording::default() });
        let (s, mut v, frames) = viewer("overload", port.clone());
        let out = s.0.join("never.mov");
        let mut r = ExportSequenceRequest::new(PRESET, out.clone());
        r.width = 64;
        r.height = 32;
        let e = v.export_sequence("seq-main", r).expect_err("overloaded");
        assert_eq!(e.code, "overloaded", "{}", e.detail);
        assert!(e.detail.contains("250"), "the retry hint: {}", e.detail);
        v.timeline_command(TimelineCommand::ExportMediaOpen).unwrap();
        v.set_timeline_export_field(widget::EXPORT_OUTPUT, "never-window.mov").unwrap();
        let e = v.timeline_command(TimelineCommand::ExportMediaRun).expect_err("overloaded");
        assert!(e.contains("overloaded"), "{e}");
        assert!(v.state_json()["timeline"]["export"]["running"].is_null(), "nothing runs");
        assert_eq!(frames.load(Ordering::SeqCst), 0, "no frame was asked for: no worker, no encoder");
        assert!(!out.exists() && !s.0.join("never-window.mov").exists(), "no file");
        assert_eq!((port.calls(), port.released().len()), (2, 0), "two refused admissions, nothing to release");
        let c = v.job_counts();
        assert_eq!((c.refused_overloaded, c.admitted, c.in_flight), (2, 0, 0), "{c:?}");
    }

    /// Cancel on a running window export asks the worker to stop but keeps the ticket held until
    /// the worker's receipt arrives; the receipt (`canceled`) then settles the job `canceled`
    /// and releases the ticket exactly once.
    #[test]
    fn canceling_a_running_export_holds_the_ticket_until_the_receipt_then_settles_canceled_once() {
        let port = Arc::new(Recording::default());
        let (_s, mut v, _) = viewer("cancel", port.clone());
        start(&mut v, "canceled.mov");
        let running = v.state_json()["timeline"]["export"]["running"].clone();
        assert_eq!(running["job_tickets"], serde_json::json!([1]), "{running}");
        v.timeline_command(TimelineCommand::ExportMediaCancel).unwrap();
        // The worker waits on its first frame: no receipt yet, so nothing is released.
        assert!(v.state_json()["timeline"]["export"]["running"].is_object());
        assert_eq!((port.admitted(), port.released()), (vec![1], vec![]), "held until the receipt");
        assert_eq!(v.job_counts().in_flight, 1);
        let last = v.timeline_export_wait().expect("a result");
        assert_eq!(last["receipt"]["outcome"], "canceled", "{last}");
        assert_eq!((last["job"]["outcome"].as_str(), last["job"]["ticket"].as_u64()), (Some("canceled"), Some(1)), "{last}");
        assert_eq!(port.released(), vec![1], "released exactly once, after the receipt");
        let c = v.job_counts();
        assert_eq!((c.admitted, c.canceled, c.released, c.in_flight), (1, 1, 1, 0), "{c:?}");
    }

    /// The document head moves (Lift of the nested clip) while a window export runs: the export
    /// finishes from its bound snapshot (the frames are the nested clip's red, which the head no
    /// longer holds) and settles `completed_stale` with both revisions; the status log says so.
    #[test]
    fn export_whose_head_moved_finishes_completed_stale_from_the_bound_snapshot() {
        let port = Arc::new(Recording::default());
        let (s, mut v, _) = viewer("stale", port.clone());
        let bound = head(&v);
        start(&mut v, "stale.mov");
        let main = v.run().snapshot().unwrap().document().sequences.iter().find(|q| q.sequence_id == "seq-main").cloned().unwrap();
        let nested = main.tracks.iter().find(|t| t.kind == TrackKind::Video).unwrap().clips.iter().find(|c| matches!(c.source_ref, ClipSource::Sequence { .. })).unwrap().clip_id.clone();
        v.timeline_command(TimelineCommand::Select(Some(nested))).unwrap();
        v.timeline_command(TimelineCommand::Lift).unwrap_or_else(|e| panic!("{e}: {}", status(&v)));
        let moved = head(&v);
        assert!(moved > bound, "Lift moved the head");
        let last = v.timeline_export_wait().expect("a result");
        assert_eq!(last["receipt"]["outcome"], "complete", "{last}");
        assert_eq!(last["receipt"]["frames_encoded"].as_u64(), Some(10), "{last}");
        assert_eq!((last["job"]["outcome"].as_str(), last["job"]["bound_revision"].as_u64(), last["job"]["head_revision"].as_u64()), (Some("completed_stale"), Some(bound), Some(moved)), "{last}");
        assert_eq!(port.released(), vec![1]);
        assert_eq!(v.job_counts().completed_stale, 1);
        assert!(status(&v).to_string().contains("stale result, not current"), "{}", status(&v));
        // File frame 0 (timeline frame 10) is the bound snapshot's nested red clip.
        let bytes = std::fs::read(s.0.join("stale.mov")).unwrap();
        let video = read(&bytes, 10);
        assert_eq!(video.len(), 10);
        let (w, h) = (64usize, 32usize);
        let f0 = &video[0].1;
        let (yp, cb, cr) = (&f0[..w * h], &f0[w * h..w * h + w / 2 * h], &f0[w * h + w / 2 * h..]);
        let (x, y) = (32usize, 16usize);
        let got = rgb(yp[y * w + x], cb[y * (w / 2) + x / 2], cr[y * (w / 2) + x / 2]);
        assert!((0..3).all(|c| (got[c] - RED[c]).abs() <= 0.03), "the bound snapshot's red: {got:?}");
    }

    fn read(bytes: &[u8], count: u64) -> Vec<(i64, Vec<u8>)> {
        #[derive(Default)]
        struct Collect(Vec<(i64, Vec<u8>)>);
        impl reel::ChunkSink for Collect {
            fn accept(&mut self, c: reel::Chunk<'_>) -> Result<(), reel::ReelError> {
                self.0.push((c.pts, c.bytes.to_vec()));
                Ok(())
            }
        }
        let request = reel::SegmentRequest {
            codec: "isobmff".to_owned(),
            start_pts: Ticks::new(0),
            count,
            track: Some(1),
            output: reel::OutputMode::Decoded,
            prefer: reel::SourcePreference::Original,
            purpose: reel::Purpose::Export,
            grant: reel::Grant { content_id: reel::ContentId::parse("job-admission").unwrap(), revision: 1, expected_len: bytes.len() as u64 },
            expected_revision: 1,
        };
        let mut sink = Collect::default();
        reel::Registry::standard().decode_segment(&mut Cursor::new(bytes.to_vec()), None, &request, &reel::Limits::default(), &CancellationToken::default(), &mut sink).expect("reel reads the export");
        sink.0
    }
    /// BT.709 limited-range 8-bit Y'CbCr back to R'G'B'.
    fn rgb(y: u8, cb: u8, cr: u8) -> [f32; 3] {
        let y = (f32::from(y) - 16.0) / 219.0;
        let cb = (f32::from(cb) - 128.0) / 224.0;
        let cr = (f32::from(cr) - 128.0) / 224.0;
        let r = y + 1.5748 * cr;
        let b = y + 1.8556 * cb;
        let g = (y - 0.2126 * r - 0.0722 * b) / 0.7152;
        [r, g, b]
    }
}
