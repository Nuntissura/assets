//! Export Media: a document sequence to a media file (lane SMB-PR-SEQ-EXPORT-UI-1, feature
//! `media`; [STU-VID-030] nested clips are clips, [STU-CMP-110] / [STU-QUIET-005] the export runs
//! headless and in the background). The first window export surface of the viewer: compositions
//! stay API-only (`timeline_export::composition`).
//!
//! # Manual (no-context operator or model)
//!
//! - **Headless** ([`Viewer::export_sequence`]): `sequence_id` plus an [`ExportSequenceRequest`]
//!   (preset id from reel's shipped table through `timeline_export::preset_recipe`, in/out ticks,
//!   raster, bit depth, chroma, output path, overwrite, artifact label). In/out default to the
//!   sequence's work area, else the whole sequence (rounded up to a whole frame). A relative output
//!   path resolves against the save root: the folder of the save path field, else of the opened
//!   file. Returns `{sequence_id, preset_id, in_tick, out_tick, frames, audio_rate, width, height,
//!   output, receipt}` where `receipt` is reel's receipt JSON (`frames_encoded`, `audio_frames`,
//!   `output_sha256`, `outcome`, ...). A failed or canceled export still returns its receipt
//!   (`outcome` `failed` / `canceled`, the code and detail inside); a refusal before anything
//!   renders is an `Err` with a typed code.
//! - **Frames** come from the program monitor's decoded-frame port ([`Viewer::set_monitor_sources`];
//!   [`Viewer::bind_program_media`] binds media files through reel's decoders) and its source
//!   extents; the composite is SMB-REEL-ENCODE's ([`super::super::timeline_export`]). No port:
//!   `frame_decoder_missing` (never black frames, V4-03). That port is not `Send`, so the job's
//!   frame source is a channel proxy: the worker asks, the thread owning the port decodes (the
//!   calling thread headless, the UI thread in the window, at most [`FRAMES_PER_POLL`] per drawn
//!   frame); compositing, conversion, encoding and muxing run on the worker.
//! - **Sound** comes from the port given to [`Viewer::set_export_audio`] (the same `AudioSources`
//!   kind the mixer plays; the mixer keeps its own port inside its stream builder). The root mix is
//!   the stream the mixer plays (R44 F-R44-3, `timeline_export` module docs "root mix"): with the
//!   document's stored routing (clip gain and pan envelopes, track and master automation, bus
//!   routes, sends, bus and master effects) or a submix track, the export renders pulse's
//!   `TimelineBusStream` built as `Viewer::bus_stream` builds it, so the file holds what the mixer
//!   plays (`tests/viewer_timeline_export_bus.rs`); without them the plain timeline mix. An export is
//!   never silent in place of sound: without a port, a sequence whose enabled audio tracks (nested
//!   ones included) hold an enabled media clip is refused `audio_sources_missing` naming the clip;
//!   with a port that lacks a clip's sound, pulse refuses the mix (`mixdown`, or `nest_unresolved`
//!   for a nested clip). Only a sequence with no such clip exports silence, because nothing in it
//!   sounds (every preset carries PCM).
//! - **Nested clips** resolve through SMB-PR-NEST-2's `timeline_export_job_with` with the document's
//!   sequences and audio routing: an inner sound the port lacks is `nest_unresolved` naming the
//!   nested sequence and its clips; an inner frame the port cannot give fails the export
//!   (`export-frame-unavailable` in the receipt).
//! - **Window**: timeline toolbar `Export Media...` (`timeline.export_media`,
//!   [`TimelineCommand::ExportMediaOpen`]; Premiere's command id is UNVERIFIED (D5), no spec
//!   command id is claimed) opens the form row `timeline.export_media.form`: preset picker
//!   (`.preset`, the shipped presets this build can encode), In / Out frames (`.in`, `.out`, the out
//!   frame inclusive like the monitor's out point; prefilled from the program monitor's in/out,
//!   else the work area, else the whole sequence), Width / Height (`.width`, `.height`), Output
//!   (`.output`), Overwrite (`.overwrite`), Export (`.export`, [`TimelineCommand::ExportMediaRun`])
//!   and Cancel (`.cancel`, [`TimelineCommand::ExportMediaCancel`]: closes the form, or cancels the
//!   running export). [`Viewer::set_timeline_export_field`] sets a field by id (what typing does).
//!   Export returns at once; the job runs on its own worker thread and never takes focus. Progress
//!   is `state_json()["timeline"]["export"]["running"]` (`state`, `done`, `total`) and the toolbar
//!   label; the result is `["last"]` and a status log line. Frames are served while the timeline
//!   panel is drawn: hiding the panel pauses the export (declared gap: the monitor's port cannot
//!   move to the worker without a `Send` port in `view/mod.rs`). The start line says so; while the
//!   worker waits over [`PAUSE_AFTER`] for a frame, `["running"]["paused"]` is `true` (with
//!   `paused_ms`) and the toolbar label reads `Export paused`; the first drawn frame after the
//!   pause logs `export media: resumed after N ms paused`. Closing the document ends the export
//!   (the worker's next frame is `frame_unavailable`).
//! - **Log**: every result and refusal is a status log line `export media: ...` (the viewer's
//!   operation log for exports; nothing is committed to the session).
//!
//! Refusals: `not_open`, `unknown_sequence`, `frame_decoder_missing`, `unknown_preset`,
//! `codec_unavailable`, `export_range`, `audio_sources_missing`, `nest_unresolved`, `mixdown`,
//! `routing_unreadable`, `export_output_root`, `monitor_size`, `export_worker`, `export_running`,
//! and the form's `export_field_<field>`.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::super::monitor::{FrameSource, MonitorFail, SourceFrame};
use super::super::timeline_export::{NestedDocument, TimelineExport, export_queue, preset_recipe, timeline_export_job_with};
use super::{TimelineCommand, widget};
use hsk_studio_accord::{TICKS_PER_SECOND, Ticks};
use hsk_studio_folio::{ClipSource, StudioSequence, TrackKind};
use hsk_studio_pulse::{AudioSources, SourceExtents};
use hsk_studio_reel::{ChromaFormat, Container, ExportHandle, ExportJob, ExportQueue, ExportReceipt, VideoCodecChoice};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

/// Decoded frames the UI thread serves to a running export per drawn frame.
pub const FRAMES_PER_POLL: usize = 8;
/// Default raster of a request (the form's prefill).
pub const DEFAULT_WIDTH: u32 = 1920;
pub const DEFAULT_HEIGHT: u32 = 1080;
/// Sample rate of the mix when no sound port is bound.
pub const DEFAULT_AUDIO_RATE: u32 = 48_000;
/// A frame request unanswered this long marks a window export `paused` (the timeline panel, which
/// serves frames, is hidden or the UI thread is stalled).
pub const PAUSE_AFTER: std::time::Duration = std::time::Duration::from_millis(500);

fn fail(code: &'static str, detail: impl Into<String>) -> MonitorFail {
    MonitorFail { code, detail: detail.into() }
}

/// What to export from a sequence (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct ExportSequenceRequest {
    pub preset_id: String,
    /// First exported tick and the end tick (exclusive); both `None`: the work area, else the whole
    /// sequence. One given alone takes the other from that default.
    pub in_tick: Option<u64>,
    pub out_tick: Option<u64>,
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    /// `None`: 4:2:2 for VC-3, 4:2:0 for the other codecs.
    pub chroma: Option<ChromaFormat>,
    /// Absolute, or relative to the save root.
    pub output: PathBuf,
    pub overwrite: bool,
    /// Artifact label of the receipt (`None`: the sequence name).
    pub artifact: Option<String>,
}

impl ExportSequenceRequest {
    /// Preset `preset_id` to `output`: default range, 1920x1080, 8-bit, the codec's chroma.
    pub fn new(preset_id: &str, output: PathBuf) -> Self {
        Self { preset_id: preset_id.to_owned(), in_tick: None, out_tick: None, width: DEFAULT_WIDTH, height: DEFAULT_HEIGHT, bit_depth: 8, chroma: None, output, overwrite: false, artifact: None }
    }
}

/// The export's sound port (module docs).
#[derive(Clone)]
pub(in crate::view) struct AudioPort {
    sources: Arc<dyn AudioSources + Send + Sync>,
    extents: Arc<dyn SourceExtents + Send + Sync>,
    rate: u32,
}

#[derive(Clone)]
struct SharedAudio(Arc<dyn AudioSources + Send + Sync>);
impl AudioSources for SharedAudio {
    fn audio(&self, source: &ClipSource) -> Option<&[Vec<f32>]> {
        self.0.audio(source)
    }
}
/// No sound port and no audio clip: nothing sounds.
struct Silent;
impl AudioSources for Silent {
    fn audio(&self, _: &ClipSource) -> Option<&[Vec<f32>]> {
        None
    }
}

/// Source lengths read once from a port, owned by the job.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExtentTable(Vec<(ClipSource, Ticks)>);

impl ExtentTable {
    /// The extent of every distinct clip source of `sequences` that `extents` knows.
    pub fn of(sequences: &[StudioSequence], extents: &dyn SourceExtents) -> Self {
        let mut out: Vec<(ClipSource, Ticks)> = Vec::new();
        for c in sequences.iter().flat_map(|s| s.tracks.iter()).flat_map(|t| t.clips.iter()) {
            if out.iter().any(|(k, _)| *k == c.source_ref) {
                continue;
            }
            if let Some(t) = extents.extent(&c.source_ref) {
                out.push((c.source_ref.clone(), t));
            }
        }
        Self(out)
    }
}

impl SourceExtents for ExtentTable {
    fn extent(&self, source: &ClipSource) -> Option<Ticks> {
        self.0.iter().find(|(k, _)| k == source).map(|(_, t)| *t)
    }
}

type FrameReply = Result<Arc<SourceFrame>, MonitorFail>;

/// One frame the worker asks the port owner for.
pub(in crate::view) struct FrameRequest {
    source: ClipSource,
    tick: Ticks,
    reply: mpsc::SyncSender<FrameReply>,
}

/// Since when the worker waits for an unanswered frame (`None`: not waiting).
type Waiting = Arc<std::sync::Mutex<Option<std::time::Instant>>>;

/// The job's frame source: forwards every request to the thread owning the monitor's port.
struct FramePort(mpsc::Sender<FrameRequest>, Waiting);

fn set_waiting(w: &Waiting, at: Option<std::time::Instant>) {
    if let Ok(mut g) = w.lock() {
        *g = at;
    }
}
/// How long the worker has waited for a frame.
fn waited(w: &Waiting) -> Option<std::time::Duration> {
    w.lock().ok().and_then(|g| g.map(|t| t.elapsed()))
}

fn port_closed() -> MonitorFail {
    fail("frame_unavailable", "the export's frame port closed")
}

impl FrameSource for FramePort {
    fn frame(&self, source: &ClipSource, source_tick: Ticks) -> Option<Arc<SourceFrame>> {
        self.frame_result(source, source_tick).ok()
    }
    fn frame_result(&self, source: &ClipSource, source_tick: Ticks) -> Result<Arc<SourceFrame>, MonitorFail> {
        let (reply, answer) = mpsc::sync_channel(1);
        set_waiting(&self.1, Some(std::time::Instant::now()));
        let answer = self.0.send(FrameRequest { source: source.clone(), tick: source_tick, reply }).map_err(|_| port_closed()).and_then(|()| answer.recv().map_err(|_| port_closed()));
        set_waiting(&self.1, None);
        answer?
    }
}

/// Answers one request from `port` (`None`: the decoder went away).
fn serve(request: FrameRequest, port: Option<&dyn FrameSource>) {
    let answer = match port {
        Some(p) => p.frame_result(&request.source, request.tick),
        None => Err(fail("frame_decoder_missing", "the program monitor's frame decoder went away")),
    };
    let _ = request.reply.send(answer);
}

/// A job ready to submit, with its frame request channel and its description.
struct Prepared {
    queue: ExportQueue,
    job: ExportJob,
    requests: mpsc::Receiver<FrameRequest>,
    waiting: Waiting,
    meta: Value,
}

/// A running window export.
pub(in crate::view) struct Running {
    requests: mpsc::Receiver<FrameRequest>,
    worker: std::thread::JoinHandle<Vec<ExportReceipt>>,
    handle: ExportHandle,
    waiting: Waiting,
    meta: Value,
}

impl Running {
    /// How long the worker has waited for a frame, when that is over [`PAUSE_AFTER`].
    fn paused(&self) -> Option<std::time::Duration> {
        waited(&self.waiting).filter(|d| *d >= PAUSE_AFTER)
    }
}

/// The Export Media form (strings as typed; module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct ExportForm {
    pub sequence_id: String,
    pub preset_id: String,
    /// The shipped presets this build can encode: (preset id, name).
    pub presets: Vec<(String, String)>,
    pub in_frame: String,
    /// Last exported frame (inclusive).
    pub out_frame: String,
    pub width: String,
    pub height: String,
    pub output: String,
    pub overwrite: bool,
    /// The last typed refusal of Export.
    pub error: Option<String>,
}

impl ExportForm {
    pub fn json(&self) -> Value {
        json!({
            "sequence_id": self.sequence_id,
            "preset_id": self.preset_id,
            "presets": self.presets.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
            "in_frame": self.in_frame,
            "out_frame": self.out_frame,
            "width": self.width,
            "height": self.height,
            "output": self.output,
            "overwrite": self.overwrite,
            "error": self.error,
        })
    }

    /// The request Export runs (`ticks_per_frame` of the sequence).
    pub fn request(&self, ticks_per_frame: u64) -> Result<ExportSequenceRequest, String> {
        let number = |text: &str, what: &str| text.trim().parse::<u64>().map_err(|_| format!("export_field_{what}: `{text}` is not a whole number"));
        let first = number(&self.in_frame, "in")?;
        let last = number(&self.out_frame, "out")?;
        if last < first {
            return Err(format!("export_field_out: out frame {last} is before in frame {first}"));
        }
        let in_tick = first.checked_mul(ticks_per_frame).ok_or("export_field_in: overflow")?;
        let out_tick = last.checked_add(1).and_then(|n| n.checked_mul(ticks_per_frame)).ok_or("export_field_out: overflow")?;
        let size = |text: &str, what: &str| -> Result<u32, String> {
            text.trim().parse::<u32>().ok().filter(|v| *v > 0).ok_or_else(|| format!("export_field_{what}: `{text}` is not a positive whole number"))
        };
        let output = self.output.trim();
        if output.is_empty() {
            return Err("export_field_output: empty".into());
        }
        let mut r = ExportSequenceRequest::new(&self.preset_id, PathBuf::from(output));
        r.in_tick = Some(in_tick);
        r.out_tick = Some(out_tick);
        r.width = size(&self.width, "width")?;
        r.height = size(&self.height, "height")?;
        r.overwrite = self.overwrite;
        Ok(r)
    }
}

/// Export state of the timeline panel (inside `TimelineState`).
#[derive(Default)]
pub struct ExportMediaState {
    pub(in crate::view) form: Option<ExportForm>,
    pub(in crate::view) running: Option<Running>,
    last: Option<Value>,
    audio: Option<AudioPort>,
}

impl ExportMediaState {
    /// The open form.
    pub fn form(&self) -> Option<&ExportForm> {
        self.form.as_ref()
    }
    /// Whether an export runs.
    pub fn running(&self) -> bool {
        self.running.is_some()
    }
    /// The last finished export (its result JSON).
    pub fn last(&self) -> Option<&Value> {
        self.last.as_ref()
    }
    /// `state_json()["timeline"]["export"]`.
    pub fn json(&self) -> Value {
        json!({
            "form": self.form.as_ref().map(ExportForm::json),
            "running": self.running.as_ref().map(|r| {
                let p = r.handle.progress();
                let mut m = r.meta.clone();
                m["state"] = json!(p.state.as_str());
                m["done"] = json!(p.done);
                m["total"] = json!(p.total);
                let paused = r.paused();
                m["paused"] = json!(paused.is_some());
                m["paused_ms"] = json!(paused.map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)));
                m
            }),
            "last": self.last,
            "audio_port": self.audio.as_ref().map(|a| json!({"rate": a.rate})),
        })
    }
    /// Toolbar progress label of a running export.
    pub(in crate::view) fn progress_label(&self) -> Option<String> {
        self.running.as_ref().map(|r| {
            let p = r.handle.progress();
            format!("Export {} {}/{}", if r.paused().is_some() { "paused" } else { p.state.as_str() }, p.done, p.total)
        })
    }
}

/// The shipped presets this build can encode (`preset_recipe` accepts them).
pub fn offered_presets() -> Vec<(String, String)> {
    let (_, os) = export_queue(None);
    hsk_studio_reel::presets::EXPORT_PRESETS.iter().filter(|p| preset_recipe(p.preset_id, os).is_ok()).map(|p| (p.preset_id.to_owned(), p.name.to_owned())).collect()
}

fn extension(container: Container) -> &'static str {
    match container {
        Container::Mov => "mov",
        Container::Mp4 => "mp4",
    }
}

/// The first enabled media clip on an enabled audio or submix track of `root` or of a sequence it
/// nests (each sequence visited once).
fn first_audio_clip(document: &[StudioSequence], root: &StudioSequence) -> Option<String> {
    let mut todo = vec![root];
    let mut seen: Vec<&str> = vec![root.sequence_id.as_str()];
    while let Some(s) = todo.pop() {
        for t in s.tracks.iter().filter(|t| t.enabled) {
            for c in t.clips.iter().filter(|c| c.enabled) {
                match &c.source_ref {
                    ClipSource::Sequence { sequence_id } => {
                        if !seen.contains(&sequence_id.as_str())
                            && let Some(n) = document.iter().find(|q| &q.sequence_id == sequence_id)
                        {
                            seen.push(n.sequence_id.as_str());
                            todo.push(n);
                        }
                    }
                    _ if matches!(t.kind, TrackKind::Audio | TrackKind::Submix) => return Some(c.clip_id.clone()),
                    _ => {}
                }
            }
        }
    }
    None
}

/// The default range: the work area, else `[0, duration)` rounded up to a whole frame.
fn default_range(sequence: &StudioSequence) -> (u64, u64) {
    if let Some(a) = &sequence.work_area {
        return (a.start.value(), a.end.value());
    }
    let tpf = sequence.frame_rate.ticks_per_frame.max(1);
    let frames = sequence.duration.value().div_ceil(tpf);
    (0, frames.saturating_mul(tpf))
}

fn result_json(meta: &Value, receipt: &ExportReceipt) -> Value {
    let mut out = meta.clone();
    out["receipt"] = serde_json::from_str(&receipt.to_json()).unwrap_or(Value::Null);
    out
}

fn outcome(result: &Value) -> String {
    let r = &result["receipt"];
    match (r["outcome"].as_str(), r["code"].as_str()) {
        (Some(o), Some(c)) => format!("{o} {c} {}", r["detail"].as_str().unwrap_or("")),
        (Some(o), None) => format!("{o}: {} frames, {} audio frames, sha256 {}", r["frames_encoded"], r["audio_frames"], r["output_sha256"].as_str().unwrap_or("-")),
        _ => "no receipt".into(),
    }
}

impl Viewer {
    /// The export's sound port (module docs): `sources` at `rate`, source lengths from `extents`.
    /// Opens the timeline panel when it is not open (the port lives with the panel's export state).
    pub fn set_export_audio<S: AudioSources + Send + Sync + 'static>(&mut self, sources: S, extents: Arc<dyn SourceExtents + Send + Sync>, rate: u32) -> Result<(), String> {
        if rate == 0 {
            return Err("audio_rate: zero".into());
        }
        if self.timeline.is_none() {
            self.open_timeline()?;
        }
        let t = self.timeline.as_mut().ok_or("no_timeline")?;
        t.export.audio = Some(AudioPort { sources: Arc::new(sources), extents, rate });
        self.say(format!("export media: sound port bound at {rate} Hz"));
        Ok(())
    }

    /// Binds media files to the program monitor through reel's decoders (the CLI's `--media`;
    /// module docs): each file is a clip source `sha256:<hex>` (as the source monitor names it),
    /// its length the container's video track; the program monitor opens on `sequence_id` at
    /// `width` x `height` when it shows another sequence or none. Returns the sources.
    pub fn bind_program_media(&mut self, sequence_id: &str, files: Vec<Vec<u8>>, width: u32, height: u32) -> Result<Vec<ClipSource>, MonitorFail> {
        use super::super::media::{MediaFrames, ReelFrameDecoder};
        let cancel = self.cancel;
        let mut decoder = ReelFrameDecoder::new();
        let mut sources = Vec::with_capacity(files.len());
        let mut extents = Vec::with_capacity(files.len());
        for bytes in files {
            let digest = hsk_studio_reel::full_digest(&mut std::io::Cursor::new(&bytes[..]), 1 << 20, cancel).map_err(|e| fail("media_read", e.to_string()))?;
            let hex = hsk_studio_reel::hex(&digest);
            let source = ClipSource::Media {
                artifact_manifest_id: format!("sha256:{hex}"),
                content_digest: hsk_studio_folio::ContentDigest { algorithm: "sha256".into(), digest: hex },
                asset_id: None,
            };
            decoder.register(&source, Arc::from(bytes)).map_err(|e| fail(e.code, e.detail))?;
            let (units, ticks_per_unit, _, _) = decoder.layout(&source, cancel).map_err(|e| fail(e.code, e.detail))?;
            let extent = units.checked_mul(ticks_per_unit).ok_or_else(|| fail("overflow", "source extent"))?;
            extents.push((source.clone(), Ticks::new(extent)));
            sources.push(source);
        }
        let on_it = self.monitor.as_ref().is_some_and(|m| m.monitor.sequence().sequence_id == sequence_id);
        if !on_it {
            self.open_monitor(sequence_id, width, height)?;
        }
        let frames = MediaFrames::new(decoder, hsk_studio_pulse::FrameCacheLimits::default(), 4)?;
        self.set_monitor_sources(Box::new(frames), Box::new(ExtentTable(extents)));
        self.say(format!("export media: {} media file(s) bound to the program monitor", sources.len()));
        Ok(sources)
    }

    /// Exports `sequence_id` (module docs): the job runs on reel's queue on a worker thread while
    /// this thread serves decoded frames; returns when the receipt is in.
    pub fn export_sequence(&mut self, sequence_id: &str, request: ExportSequenceRequest) -> Result<Value, MonitorFail> {
        let result = self.export_sequence_inner(sequence_id, &request);
        match &result {
            Ok(v) => self.say(format!("export media: {sequence_id} [{}, {}) {}: {}", v["in_tick"], v["out_tick"], request.preset_id, outcome(v))),
            Err(e) => self.say(format!("export media: {sequence_id} refused {} {}", e.code, e.detail)),
        }
        result
    }

    fn export_sequence_inner(&mut self, sequence_id: &str, request: &ExportSequenceRequest) -> Result<Value, MonitorFail> {
        let Prepared { mut queue, job, requests, meta, .. } = self.export_prepare(sequence_id, request)?;
        let _handle = queue.submit(job);
        let worker = std::thread::Builder::new().name("hsk-export-media".into()).spawn(move || queue.run()).map_err(|e| fail("export_worker", e.to_string()))?;
        // The job owns the only sender: the loop ends when the queue has run it.
        let port = self.monitor.as_ref().and_then(|m| m.sources.as_deref());
        while let Ok(r) = requests.recv() {
            serve(r, port);
        }
        let receipt = worker.join().map_err(|_| fail("export_worker", "the export worker panicked"))?.into_iter().next().ok_or_else(|| fail("export_worker", "no receipt"))?;
        Ok(result_json(&meta, &receipt))
    }

    /// Builds the job of `request` (every refusal before anything renders).
    fn export_prepare(&self, sequence_id: &str, request: &ExportSequenceRequest) -> Result<Prepared, MonitorFail> {
        let doc = self.run.snapshot().map(|s| s.document()).ok_or_else(|| fail("not_open", "nothing is open"))?;
        let sequence = doc.sequences.iter().find(|s| s.sequence_id == sequence_id).cloned().ok_or_else(|| fail("unknown_sequence", sequence_id))?;
        let monitor = self.monitor.as_ref().filter(|m| m.sources.is_some()).ok_or_else(|| fail("frame_decoder_missing", "no frame decoder is bound to the program monitor"))?;
        let video_extents = ExtentTable::of(&doc.sequences, monitor.extents.as_ref());
        let (queue, os) = export_queue(None);
        let recipe = preset_recipe(&request.preset_id, os)?;
        let (din, dout) = default_range(&sequence);
        let (in_tick, out_tick) = (request.in_tick.unwrap_or(din), request.out_tick.unwrap_or(dout));
        let chroma = request.chroma.unwrap_or(match recipe.video {
            Some(VideoCodecChoice::Vc3(_)) => ChromaFormat::Yuv422,
            _ => ChromaFormat::Yuv420,
        });
        let port = self.timeline.as_ref().and_then(|t| t.export.audio.clone());
        let (audio, audio_extents, rate) = match port {
            Some(p) => (SharedAudio(p.sources), ExtentTable::of(&doc.sequences, p.extents.as_ref()), p.rate),
            None => {
                if let Some(clip) = first_audio_clip(&doc.sequences, &sequence) {
                    return Err(fail("audio_sources_missing", format!("{clip}: no sound port is bound (Viewer::set_export_audio)")));
                }
                (SharedAudio(Arc::new(Silent)), ExtentTable::default(), DEFAULT_AUDIO_RATE)
            }
        };
        #[cfg(feature = "audio-batch")]
        let routing = self.audio_routing().map_err(|e| fail("routing_unreadable", e))?.unwrap_or_default();
        #[cfg(not(feature = "audio-batch"))]
        let routing = hsk_studio_folio::StudioAudioRoutingSet::default();
        let output = self.export_output(&request.output)?;
        let artifact = request.artifact.clone().unwrap_or_else(|| sequence.name.clone()).replace(['/', '\\'], "_");
        let tpf = sequence.frame_rate.ticks_per_frame;
        let meta = json!({
            "sequence_id": sequence_id,
            "preset_id": request.preset_id,
            "in_tick": in_tick,
            "out_tick": out_tick,
            "frames": out_tick.saturating_sub(in_tick).checked_div(tpf).unwrap_or(0),
            "audio_rate": rate,
            "audio_frames": u128::from(out_tick.saturating_sub(in_tick)).saturating_mul(u128::from(rate)).checked_div(u128::from(TICKS_PER_SECOND)).and_then(|n| u64::try_from(n).ok()),
            "width": request.width,
            "height": request.height,
            "bit_depth": request.bit_depth,
            "container": extension(recipe.container),
            "output": output.to_string_lossy(),
        });
        let (sender, requests) = mpsc::channel();
        let waiting: Waiting = Arc::default();
        let export = TimelineExport {
            sequence,
            in_tick,
            out_tick,
            recipe,
            width: request.width,
            height: request.height,
            bit_depth: request.bit_depth,
            chroma,
            video_sources: FramePort(sender, Arc::clone(&waiting)),
            video_extents,
            audio: Some((audio, rate)),
            audio_extents,
            output,
            overwrite: request.overwrite,
            artifact,
        };
        let job = timeline_export_job_with(export, NestedDocument::new(doc.sequences.clone(), routing), self.cancel)?;
        Ok(Prepared { queue, job, requests, waiting, meta })
    }

    /// `output` as given when absolute, else joined to the save root (module docs).
    fn export_output(&self, output: &Path) -> Result<PathBuf, MonitorFail> {
        if output.is_absolute() {
            return Ok(output.to_path_buf());
        }
        let save = Path::new(self.save_input.trim());
        let root = if self.save_input.trim().is_empty() { None } else { save.parent() }.or_else(|| self.source.as_deref().and_then(Path::parent));
        root.filter(|r| !r.as_os_str().is_empty())
            .map(|r| r.join(output))
            .ok_or_else(|| fail("export_output_root", format!("{}: relative, and no save path or opened file names a folder", output.display())))
    }

    /// The Export Media commands (module docs); `None` for every other command.
    pub(in crate::view) fn timeline_export_command(&mut self, command: &TimelineCommand) -> Option<Result<(), String>> {
        Some(match command {
            TimelineCommand::ExportMediaOpen => self.export_form_open(),
            TimelineCommand::ExportMediaRun => self.export_form_run(),
            TimelineCommand::ExportMediaCancel => self.export_form_cancel(),
            _ => return None,
        })
    }

    fn export_form_open(&mut self) -> Result<(), String> {
        let t = self.timeline.as_ref().ok_or("no_timeline")?;
        if t.export.running.is_some() {
            return Err("export_running".into());
        }
        let sequence = self.timeline_sequence().cloned().ok_or("unknown_sequence")?;
        let tpf = sequence.frame_rate.ticks_per_frame.max(1);
        // The program monitor's in/out when it shows this sequence, else the work area, else all.
        let marks = self.monitor.as_ref().filter(|m| m.monitor.sequence().sequence_id == sequence.sequence_id).and_then(|m| m.monitor.scheduler().in_out()).map(|r| (r.start().value(), r.end().value()));
        let (a, b) = marks.unwrap_or_else(|| default_range(&sequence));
        let first = a.checked_div(tpf).unwrap_or(0);
        let last = b.div_ceil(tpf).saturating_sub(1).max(first);
        let presets = offered_presets();
        let preset_id = presets.iter().find(|(id, _)| id == "studio.dnxhr.hq.mov").or(presets.first()).map(|(id, _)| id.clone()).ok_or("no_export_preset")?;
        let container = hsk_studio_reel::presets::export_preset(&preset_id).map_or(Container::Mov, |p| p.container);
        let form = ExportForm {
            sequence_id: sequence.sequence_id.clone(),
            preset_id,
            presets,
            in_frame: first.to_string(),
            out_frame: last.to_string(),
            width: DEFAULT_WIDTH.to_string(),
            height: DEFAULT_HEIGHT.to_string(),
            output: format!("{}.{}", sequence.name.replace(['/', '\\', ':'], "_"), extension(container)),
            overwrite: false,
            error: None,
        };
        if let Some(t) = self.timeline.as_mut() {
            t.export.form = Some(form);
        }
        Ok(())
    }

    /// Sets a form field by its widget id (what typing or picking does).
    pub fn set_timeline_export_field(&mut self, id: &str, value: &str) -> Result<(), String> {
        let form = self.timeline.as_mut().and_then(|t| t.export.form.as_mut()).ok_or("no_export_form")?;
        match id {
            widget::EXPORT_PRESET => {
                if !form.presets.iter().any(|(p, _)| p == value) {
                    return Err(format!("unknown_preset: {value}"));
                }
                value.clone_into(&mut form.preset_id);
            }
            widget::EXPORT_IN => value.clone_into(&mut form.in_frame),
            widget::EXPORT_OUT => value.clone_into(&mut form.out_frame),
            widget::EXPORT_WIDTH => value.clone_into(&mut form.width),
            widget::EXPORT_HEIGHT => value.clone_into(&mut form.height),
            widget::EXPORT_OUTPUT => value.clone_into(&mut form.output),
            widget::EXPORT_OVERWRITE => {
                form.overwrite = match value {
                    "true" => true,
                    "false" => false,
                    other => return Err(format!("export_field_overwrite: `{other}` is not true or false")),
                }
            }
            other => return Err(format!("unknown_field: {other}")),
        }
        form.error = None;
        Ok(())
    }

    fn export_form_run(&mut self) -> Result<(), String> {
        let t = self.timeline.as_ref().ok_or("no_timeline")?;
        if t.export.running.is_some() {
            return Err("export_running".into());
        }
        let form = t.export.form.clone().ok_or("no_export_form")?;
        let started = self.export_form_start(&form);
        match started {
            Ok(()) => Ok(()),
            Err(e) => {
                if let Some(f) = self.timeline.as_mut().and_then(|t| t.export.form.as_mut()) {
                    f.error = Some(e.clone());
                }
                self.say(format!("export media: {} refused {e}", form.sequence_id));
                Err(e)
            }
        }
    }

    fn export_form_start(&mut self, form: &ExportForm) -> Result<(), String> {
        let tpf = self.run.snapshot().and_then(|s| s.document().sequences.iter().find(|q| q.sequence_id == form.sequence_id)).map(|s| s.frame_rate.ticks_per_frame).ok_or("unknown_sequence")?;
        let request = form.request(tpf)?;
        let Prepared { mut queue, job, requests, waiting, meta } = self.export_prepare(&form.sequence_id, &request).map_err(|e| format!("{} {}", e.code, e.detail))?;
        let handle = queue.submit(job);
        let worker = std::thread::Builder::new().name("hsk-export-media".into()).spawn(move || queue.run()).map_err(|e| format!("export_worker {e}"))?;
        self.say(format!(
            "export media: {} [{}, {}) {} started -> {} (frames decode while the timeline panel is shown; hiding it pauses the export)",
            form.sequence_id,
            meta["in_tick"],
            meta["out_tick"],
            form.preset_id,
            meta["output"].as_str().unwrap_or("")
        ));
        if let Some(t) = self.timeline.as_mut() {
            t.export.form = None;
            t.export.running = Some(Running { requests, worker, handle, waiting, meta });
        }
        Ok(())
    }

    fn export_form_cancel(&mut self) -> Result<(), String> {
        let t = self.timeline.as_mut().ok_or("no_timeline")?;
        if let Some(r) = &t.export.running {
            r.handle.cancel();
            self.say("export media: cancel requested".into());
            return Ok(());
        }
        match t.export.form.take() {
            Some(_) => Ok(()),
            None => Err("no_export_form".into()),
        }
    }

    /// Serves up to [`FRAMES_PER_POLL`] frame requests of the running export and collects its
    /// result when the worker is done; returns whether an export still runs. The timeline panel
    /// calls it every drawn frame.
    pub(in crate::view) fn timeline_export_poll(&mut self) -> bool {
        let Some(r) = self.timeline.as_ref().and_then(|t| t.export.running.as_ref()) else { return false };
        // The panel was hidden (or the UI stalled) while the worker waited: say it was paused.
        if let Some(d) = r.paused() {
            self.say(format!("export media: resumed after {} ms paused (frames decode on the UI thread while the timeline panel is shown)", d.as_millis()));
        }
        let Some(r) = self.timeline.as_ref().and_then(|t| t.export.running.as_ref()) else { return false };
        let port = self.monitor.as_ref().and_then(|m| m.sources.as_deref());
        for _ in 0..FRAMES_PER_POLL {
            match r.requests.try_recv() {
                Ok(request) => serve(request, port),
                Err(_) => break,
            }
        }
        if !r.worker.is_finished() {
            return true;
        }
        let Some(r) = self.timeline.as_mut().and_then(|t| t.export.running.take()) else { return false };
        let sequence_id = r.meta["sequence_id"].as_str().unwrap_or("").to_owned();
        let result = match r.worker.join() {
            Ok(receipts) => match receipts.first() {
                Some(receipt) => result_json(&r.meta, receipt),
                None => json!({"error": "export_worker", "detail": "no receipt"}),
            },
            Err(_) => json!({"error": "export_worker", "detail": "the export worker panicked"}),
        };
        let line = if result.get("receipt").is_some() { outcome(&result) } else { format!("{} {}", result["error"], result["detail"]) };
        self.say(format!("export media: {sequence_id} [{}, {}) {}: {line}", r.meta["in_tick"], r.meta["out_tick"], r.meta["preset_id"].as_str().unwrap_or("")));
        if let Some(t) = self.timeline.as_mut() {
            t.export.last = Some(result);
        }
        false
    }

    /// Serves the running window export until it ends (headless hosts; the window polls instead)
    /// and returns its result.
    pub fn timeline_export_wait(&mut self) -> Option<Value> {
        while self.timeline.as_ref().is_some_and(|t| t.export.running.is_some()) {
            let pending = self.timeline.as_ref().and_then(|t| t.export.running.as_ref()).map(|r| r.requests.recv_timeout(std::time::Duration::from_millis(5)));
            match pending {
                Some(Ok(request)) => serve(request, self.monitor.as_ref().and_then(|m| m.sources.as_deref())),
                _ => {
                    self.timeline_export_poll();
                }
            }
        }
        self.timeline.as_ref().and_then(|t| t.export.last.clone())
    }
}
