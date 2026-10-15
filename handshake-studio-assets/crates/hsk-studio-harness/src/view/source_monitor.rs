//! Source monitor and source-to-program editing (lane SMB-PR-SOURCE-MONITOR; PR-19, STU-VID-023
//! "a SOURCE monitor showing a media item or a clip in isolation, and a PROGRAM monitor showing
//! the sequence at the playhead", STU-VID-024 three-point and four-point editing, STU-VID-025
//! source patching).
//!
//! [`SourceMonitor`] plays one media source in isolation through the same scheduler and render
//! path as the program monitor ([`ProgramMonitor`] over a one-clip sequence whose ticks ARE the
//! source ticks: frame `k` of the monitor is source tick `k * ticks_per_frame`), so play, pause,
//! frame-accurate scrub and step, loop, play in-to-out and the J/K/L shuttle behave exactly as on
//! the program monitor. Its in and out marks are whole source frames (out inclusive, as pulse's
//! `set_in_out`); a mark that is not set reads as the first or last frame of the source.
//!
//! [`plan_source_edit`] turns the marks into pulse's [`AssemblyEdit`]: source in = the in mark,
//! source out = the out mark when it is set; sequence in = the program monitor's in point when
//! set, else its playhead; sequence out = the program out point when set. Three points derive the
//! fourth (an unset source out with a program in/out span backtimes the source out from the span);
//! four points that disagree need the caller's explicit [`FourPointFit`] (`fit_required`). The
//! edit lands through pulse's `plan_assembly_edit` (Folio-validated prospective sequence) and the
//! viewer commits it as ONE undoable timeline step (`Run::submit_sequence`). Clip ids are minted
//! here (pulse mints none) and never collide with a clip id of the document.
//!
//! Keyboard (STU-VID transport and clip command tables, module 14): `J` / `K` / `L` shuttle,
//! `Shift+J` / `Shift+L` slow shuttle, `Space` play/pause, `Left` / `Right` step one frame
//! (`Shift` = five), `,` insert (`cmd.clip.insert`), `.` overwrite (`cmd.clip.overlay`); `I` / `O`
//! mark in / out are Studio keys (no row in the captured command table; Premiere's default
//! UNVERIFIED, D5). Premiere's source-monitor behaviour (mark defaults, backtiming, target
//! selection) is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]

use super::monitor::{
    AudioClock, DropPolicy, FrameSource, MonitorFail, PlaySpeed, ProgramMonitor, Transport,
    WallClock,
};
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{
    self as folio, ClipSchema, ClipSource, ClipTransitions, SequenceFrameRate, SequenceSchema,
    Speed, StudioClip, StudioSequence, StudioTrack, TrackKind, TrackSchema,
};
pub use hsk_studio_pulse::{AssemblyMode, FourPointFit};
use hsk_studio_pulse::{
    AssemblyEdit, AssemblyPoints, EditContext, PlaybackScheduler, SourceExtents, SourcePatch,
    plan_assembly_edit,
};
use serde_json::{Value, json};

/// Stable widget ids of the source monitor panel.
pub mod widget {
    pub const PANEL: &str = "source_monitor";
    pub const MARK_IN: &str = "source_monitor.mark_in";
    pub const MARK_OUT: &str = "source_monitor.mark_out";
    pub const CLEAR_MARKS: &str = "source_monitor.clear_marks";
    pub const INSERT: &str = "source_monitor.insert";
    pub const OVERWRITE: &str = "source_monitor.overwrite";
    /// Prefix of the transport ids (the program monitor's names with `monitor.` replaced).
    pub const TRANSPORT_PREFIX: &str = "source_monitor.";
}

/// Id of the one-clip sequence the source monitor plays (never committed).
pub const SOURCE_SEQUENCE_ID: &str = "source-monitor";
const SOURCE_TRACK_ID: &str = "STRK-019abcde-0000-7000-8000-00000000c001";
const SOURCE_CLIP_ID: &str = "SCLP-019abcde-0000-7000-8000-00000000c001";
/// Longest source the monitor opens, in frames (refused `limit_exceeded` above).
pub const MAX_SOURCE_FRAMES: u64 = 1 << 32;

fn fail(code: &'static str, detail: impl Into<String>) -> MonitorFail {
    MonitorFail {
        code,
        detail: detail.into(),
    }
}

/// The one-clip sequence of a source: source tick = sequence tick over `duration`.
fn source_sequence(source: &ClipSource, frame_rate: SequenceFrameRate, duration: Ticks) -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: SOURCE_SEQUENCE_ID.into(),
        name: "source".into(),
        frame_rate,
        duration,
        tracks: vec![StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: SOURCE_TRACK_ID.into(),
            kind: TrackKind::Video,
            index: 0,
            name: "source".into(),
            enabled: true,
            locked: false,
            sync_locked: false,
            targeted: true,
            muted: false,
            soloed: false,
            clips: vec![StudioClip {
                schema_id: ClipSchema::V1,
                clip_id: SOURCE_CLIP_ID.into(),
                source_ref: source.clone(),
                source_in: Ticks::new(0),
                source_out: duration,
                timeline_start: Ticks::new(0),
                duration,
                speed: Speed::UNIT,
                enabled: true,
                link_group: None,
                markers: Vec::new(),
                transitions: ClipTransitions::default(),
            }],
            volume: 1.0,
            pan: 0.0,
        }],
        markers: Vec::new(),
        work_area: None,
    }
}

/// A source monitor over one media source (module docs).
pub struct SourceMonitor {
    source: ClipSource,
    ticks_per_frame: u64,
    frames: u64,
    mark_in: Option<u64>,
    mark_out: Option<u64>,
    monitor: ProgramMonitor,
}

impl SourceMonitor {
    /// A paused source monitor at frame 0 of `source`, `extent` ticks long at `frame_rate`
    /// (whole frames only: a partial last frame is not shown). Refused typed: `source_frame_rate`
    /// (zero or not a canonical rate), `source_empty` (shorter than one frame), `limit_exceeded`
    /// (more than [`MAX_SOURCE_FRAMES`]), `monitor_size`.
    pub fn new(
        source: ClipSource,
        extent: Ticks,
        frame_rate: SequenceFrameRate,
        width: u32,
        height: u32,
    ) -> Result<Self, MonitorFail> {
        let tpf = frame_rate.ticks_per_frame;
        if tpf == 0 || frame_rate.accord().is_none() {
            return Err(fail("source_frame_rate", tpf.to_string()));
        }
        let frames = extent.value().checked_div(tpf).unwrap_or(0);
        if frames == 0 {
            return Err(fail("source_empty", extent.value().to_string()));
        }
        if frames > MAX_SOURCE_FRAMES {
            return Err(fail("limit_exceeded", format!("{frames} frames")));
        }
        let duration = frames
            .checked_mul(tpf)
            .ok_or_else(|| fail("overflow", "source duration"))?;
        let monitor = ProgramMonitor::new(
            source_sequence(&source, frame_rate, Ticks::new(duration)),
            width,
            height,
            DropPolicy::DropLate,
        )?;
        Ok(Self {
            source,
            ticks_per_frame: tpf,
            frames,
            mark_in: None,
            mark_out: None,
            monitor,
        })
    }

    pub fn source(&self) -> &ClipSource {
        &self.source
    }
    /// Whole frames of the source.
    pub fn frames(&self) -> u64 {
        self.frames
    }
    pub fn ticks_per_frame(&self) -> u64 {
        self.ticks_per_frame
    }
    /// The monitor (transport, shown frame, scheduler).
    pub fn monitor(&self) -> &ProgramMonitor {
        &self.monitor
    }
    /// Transport commands (play, pause, scrub, step, shuttle, loop, play in-to-out).
    pub fn monitor_mut(&mut self) -> &mut ProgramMonitor {
        &mut self.monitor
    }
    /// The frame under the playhead.
    pub fn playhead_frame(&self) -> u64 {
        let s = self.monitor.scheduler();
        s.frame_of(s.playhead())
    }
    /// `(in, out)` marks as set (out inclusive).
    pub fn marks(&self) -> (Option<u64>, Option<u64>) {
        (self.mark_in, self.mark_out)
    }

    fn check_frame(&self, frame: u64) -> Result<(), MonitorFail> {
        if frame >= self.frames {
            return Err(fail("outside_range", format!("frame {frame} of {}", self.frames)));
        }
        Ok(())
    }
    /// Keeps the scheduler's in/out span (play in-to-out, loop) equal to the marks.
    fn sync(&mut self) -> Result<(), MonitorFail> {
        if self.mark_in.is_none() && self.mark_out.is_none() {
            self.monitor.clear_in_out();
            return Ok(());
        }
        let last = self.frames.saturating_sub(1);
        self.monitor
            .set_in_out(self.mark_in.unwrap_or(0), self.mark_out.unwrap_or(last))
    }
    /// Sets the in mark (`in_after_out` when it lies after the out mark; `outside_range`).
    pub fn mark_in(&mut self, frame: u64) -> Result<(), MonitorFail> {
        self.check_frame(frame)?;
        if self.mark_out.is_some_and(|o| o < frame) {
            return Err(fail("in_after_out", format!("in {frame}")));
        }
        self.mark_in = Some(frame);
        self.sync()
    }
    /// Sets the out mark, inclusive (`out_before_in`; `outside_range`).
    pub fn mark_out(&mut self, frame: u64) -> Result<(), MonitorFail> {
        self.check_frame(frame)?;
        if self.mark_in.is_some_and(|i| frame < i) {
            return Err(fail("out_before_in", format!("out {frame}")));
        }
        self.mark_out = Some(frame);
        self.sync()
    }
    pub fn mark_in_at_playhead(&mut self) -> Result<(), MonitorFail> {
        self.mark_in(self.playhead_frame())
    }
    pub fn mark_out_at_playhead(&mut self) -> Result<(), MonitorFail> {
        self.mark_out(self.playhead_frame())
    }
    pub fn clear_marks(&mut self) {
        self.mark_in = None;
        self.mark_out = None;
        self.monitor.clear_in_out();
    }

    /// Source in tick (the in mark, else 0) and the explicit source out tick (exclusive end of the
    /// out frame), or `None` when no out mark is set.
    pub fn source_window(&self) -> Result<(Ticks, Option<Ticks>), MonitorFail> {
        let at = |frame: u64| {
            frame
                .checked_mul(self.ticks_per_frame)
                .map(Ticks::new)
                .ok_or_else(|| fail("overflow", "source tick"))
        };
        let out = match self.mark_out {
            Some(o) => Some(at(o.checked_add(1).ok_or_else(|| fail("overflow", "out"))?)?),
            None => None,
        };
        Ok((at(self.mark_in.unwrap_or(0))?, out))
    }
    /// The exclusive end of the source (whole frames).
    pub fn source_end(&self) -> Ticks {
        Ticks::new(self.frames.saturating_mul(self.ticks_per_frame))
    }

    /// Structured projection for models and tests.
    pub fn state_json(&self) -> Value {
        let mut state = self.monitor.state_json();
        if let Some(o) = state.as_object_mut() {
            o.insert("source".into(), serde_json::to_value(&self.source).unwrap_or(Value::Null));
            o.insert("frames".into(), json!(self.frames));
            o.insert("mark_in".into(), json!(self.mark_in));
            o.insert("mark_out".into(), json!(self.mark_out));
        }
        state
    }
}

/// One source-to-program edit (STU-VID-024/025).
#[derive(Clone, Debug, PartialEq)]
pub struct SourceEdit {
    pub mode: AssemblyMode,
    /// Destination video track ids (source patching); empty = the sequence's targeted video
    /// tracks, else its first video track.
    pub targets: Vec<String>,
    /// The explicit choice when four points disagree (STU-VID-024).
    pub fit: Option<FourPointFit>,
}

/// The source's own extent for pulse, every other source from the program's port.
struct WithSource<'a> {
    source: &'a ClipSource,
    end: Ticks,
    others: &'a dyn SourceExtents,
}
impl SourceExtents for WithSource<'_> {
    fn extent(&self, source: &ClipSource) -> Option<Ticks> {
        if source == self.source {
            Some(self.end)
        } else {
            self.others.extent(source)
        }
    }
}

/// Fresh `SCLP` ids: the document id's time fields, a fixed version/variant block and the first
/// unused counters (never a clip id of any sequence of the document, never repeated).
fn mint_clip_ids(doc: &folio::StudioDocument, count: usize) -> Vec<String> {
    let uuid = doc.document_id.split_once('-').map_or("", |(_, u)| u);
    let (high, low) = (uuid.get(0..8).unwrap_or("00000000"), uuid.get(9..13).unwrap_or("0000"));
    let used: std::collections::BTreeSet<&str> = doc
        .sequences
        .iter()
        .flat_map(|s| s.tracks.iter())
        .flat_map(|t| t.clips.iter())
        .map(|c| c.clip_id.as_str())
        .collect();
    let mut out = Vec::with_capacity(count);
    let mut n: u64 = 1;
    while out.len() < count {
        let id = format!("SCLP-{high}-{low}-7ed1-9000-{n:012x}");
        if !used.contains(id.as_str()) {
            out.push(id);
        }
        n = n.wrapping_add(1);
    }
    out
}

/// Plans `edit` of the source monitor's marked window onto sequence `sequence_id` at the program
/// scheduler's in point (else playhead): the Folio-validated sequence after the edit (module docs).
/// Refused typed: `unknown_sequence`, `no_target_track`, `unknown_track`, and pulse's codes
/// (`fit_required`, `sync_lock_blocked`, `track_locked`, `past_source_end`, `not_frame_aligned`,
/// `stale_revision`, ...).
#[allow(clippy::too_many_arguments)]
pub fn plan_source_edit(
    snapshot: &folio::Snapshot,
    sequence_id: &str,
    expected_revision: u64,
    source: &SourceMonitor,
    program: &PlaybackScheduler,
    edit: &SourceEdit,
    extents: &dyn SourceExtents,
    cancel: &CancellationToken,
) -> Result<StudioSequence, MonitorFail> {
    let doc = snapshot.document();
    let sequence = doc
        .sequences
        .iter()
        .find(|s| s.sequence_id == sequence_id)
        .ok_or_else(|| fail("unknown_sequence", sequence_id))?;
    if edit.targets.len() > hsk_studio_folio::MAX_TRACKS_PER_SEQUENCE {
        return Err(fail("limit_exceeded", "targets"));
    }
    let video = |id: &str| {
        sequence
            .tracks
            .iter()
            .any(|t| t.track_id == id && t.kind == TrackKind::Video)
    };
    let targets: Vec<String> = if edit.targets.is_empty() {
        let targeted: Vec<String> = sequence
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video && t.targeted)
            .map(|t| t.track_id.clone())
            .collect();
        if targeted.is_empty() {
            sequence
                .tracks
                .iter()
                .find(|t| t.kind == TrackKind::Video)
                .map(|t| vec![t.track_id.clone()])
                .ok_or_else(|| fail("no_target_track", sequence_id))?
        } else {
            targeted
        }
    } else {
        if let Some(bad) = edit.targets.iter().find(|t| !video(t)) {
            return Err(fail("unknown_track", bad.clone()));
        }
        edit.targets.clone()
    };
    let (source_in, mut source_out) = source.source_window()?;
    let (sequence_in, sequence_out) = match program.in_out() {
        Some(span) => (span.start(), Some(span.end())),
        None => (program.playhead(), None),
    };
    if source_out.is_none() && sequence_out.is_none() {
        source_out = Some(source.source_end());
    }
    // One new clip per target plus one fresh id per track for clips the edit cuts in two.
    let fresh = targets
        .len()
        .checked_add(sequence.tracks.len())
        .ok_or_else(|| fail("overflow", "ids"))?;
    let mut ids = mint_clip_ids(doc, fresh);
    let fresh_clip_ids = ids.split_off(targets.len());
    let patches = targets
        .into_iter()
        .zip(ids)
        .map(|(track_id, clip_id)| SourcePatch { track_id, clip_id })
        .collect();
    let assembly = AssemblyEdit {
        mode: edit.mode,
        source: source.source.clone(),
        speed: Speed::UNIT,
        points: AssemblyPoints {
            source_in: Some(source_in),
            source_out,
            sequence_in: Some(sequence_in),
            sequence_out,
        },
        fit: edit.fit,
        patches,
        link_group: None,
        fresh_clip_ids,
    };
    let extents = WithSource {
        source: &source.source,
        end: source.source_end(),
        others: extents,
    };
    let ctx = EditContext {
        budget: folio::Budget::default(),
        resolver: &crate::harness::TrustAll,
        extents: &extents,
        cancel,
    };
    let proposal = plan_assembly_edit(snapshot, sequence_id, expected_revision, &assembly, &ctx)
        .map_err(|e| fail(e.code(), sequence_id))?;
    proposal
        .snapshot()
        .document()
        .sequences
        .iter()
        .find(|s| s.sequence_id == sequence_id)
        .cloned()
        .ok_or_else(|| fail("unknown_sequence", sequence_id))
}

/// Which monitor a key or command addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonitorTarget {
    Source,
    Program,
}

/// Monitor keyboard commands (module docs for the key table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonitorKey {
    ShuttleLeft,
    ShuttleStop,
    ShuttleRight,
    SlowLeft,
    SlowRight,
    TogglePlay,
    StepBack(u8),
    StepForward(u8),
    MarkIn,
    MarkOut,
    Insert,
    Overwrite,
}

/// The command of a key press (`None` for keys the monitors do not bind).
pub fn key_command(key: egui::Key, shift: bool) -> Option<MonitorKey> {
    use egui::Key;
    Some(match (key, shift) {
        (Key::J, false) => MonitorKey::ShuttleLeft,
        (Key::J, true) => MonitorKey::SlowLeft,
        (Key::K, _) => MonitorKey::ShuttleStop,
        (Key::L, false) => MonitorKey::ShuttleRight,
        (Key::L, true) => MonitorKey::SlowRight,
        (Key::Space, _) => MonitorKey::TogglePlay,
        (Key::ArrowLeft, false) => MonitorKey::StepBack(1),
        (Key::ArrowLeft, true) => MonitorKey::StepBack(5),
        (Key::ArrowRight, false) => MonitorKey::StepForward(1),
        (Key::ArrowRight, true) => MonitorKey::StepForward(5),
        (Key::I, false) => MonitorKey::MarkIn,
        (Key::O, false) => MonitorKey::MarkOut,
        (Key::Comma, _) => MonitorKey::Insert,
        (Key::Period, _) => MonitorKey::Overwrite,
        _ => return None,
    })
}

/// Runs a transport key on one monitor (marks and edits are the viewer's).
fn transport_key(m: &mut ProgramMonitor, key: MonitorKey, clock: &dyn AudioClock) -> Result<(), MonitorFail> {
    match key {
        MonitorKey::ShuttleLeft => m.shuttle(true, clock).map(|_: PlaySpeed| ()),
        MonitorKey::ShuttleRight => m.shuttle(false, clock).map(|_| ()),
        MonitorKey::SlowLeft => m.shuttle_slow(true, clock).map(|_| ()),
        MonitorKey::SlowRight => m.shuttle_slow(false, clock).map(|_| ()),
        MonitorKey::ShuttleStop => m.shuttle_stop(clock),
        MonitorKey::TogglePlay => {
            if m.scheduler().transport() == Transport::Playing {
                m.pause(clock)
            } else {
                m.play(clock)
            }
        }
        MonitorKey::StepBack(n) => m.step(0i64.saturating_sub(i64::from(n)), clock).map(|_| ()),
        MonitorKey::StepForward(n) => m.step(i64::from(n), clock).map(|_| ()),
        MonitorKey::MarkIn | MonitorKey::MarkOut | MonitorKey::Insert | MonitorKey::Overwrite => {
            Err(fail("not_a_transport_key", format!("{key:?}")))
        }
    }
}

/// The viewer's source monitor: monitor, decoded-frame port, wall clock and texture.
pub(super) struct SourceState {
    pub(super) monitor: SourceMonitor,
    pub(super) sources: Option<Box<dyn FrameSource>>,
    pub(super) clock: WallClock,
    pub(super) texture: Option<egui::TextureHandle>,
    pub(super) shown: Option<u64>,
    pub(super) active: MonitorTarget,
}

impl super::Viewer {
    /// Opens the source monitor on `source` (`extent` ticks at `frame_rate`) at `width` x
    /// `height`; frames come from [`super::Viewer::set_source_monitor_sources`].
    pub fn open_source_monitor(
        &mut self,
        source: ClipSource,
        extent: Ticks,
        frame_rate: SequenceFrameRate,
        width: u32,
        height: u32,
    ) -> Result<(), MonitorFail> {
        let monitor = SourceMonitor::new(source, extent, frame_rate, width, height)?;
        self.source_monitor = Some(SourceState {
            monitor,
            sources: None,
            clock: WallClock::new(48_000),
            texture: None,
            shown: None,
            active: MonitorTarget::Source,
        });
        Ok(())
    }
    /// Decoded-frame port of the source monitor.
    pub fn set_source_monitor_sources(&mut self, sources: Box<dyn FrameSource>) {
        if let Some(s) = &mut self.source_monitor {
            s.sources = Some(sources);
        }
    }
    pub fn source_monitor(&self) -> Option<&SourceMonitor> {
        self.source_monitor.as_ref().map(|s| &s.monitor)
    }
    pub fn source_monitor_mut(&mut self) -> Option<&mut SourceMonitor> {
        self.source_monitor.as_mut().map(|s| &mut s.monitor)
    }
    /// The program monitor (transport commands on it).
    pub fn program_monitor_mut(&mut self) -> Option<&mut ProgramMonitor> {
        self.monitor.as_mut().map(|m| &mut m.monitor)
    }

    /// Insert or overwrite the source monitor's marked window into the program monitor's sequence
    /// (module docs) as one undoable step; the program monitor then shows the edited sequence with
    /// its playhead and in/out kept. Returns the new document revision.
    pub fn source_edit(&mut self, edit: &SourceEdit) -> Result<u64, MonitorFail> {
        let source = self
            .source_monitor
            .as_ref()
            .ok_or_else(|| fail("no_source_monitor", ""))?;
        let program = self
            .monitor
            .as_ref()
            .ok_or_else(|| fail("no_program_monitor", ""))?;
        let snapshot = self.run.snapshot().ok_or_else(|| fail("not_open", ""))?;
        let sequence_id = program.monitor.sequence().sequence_id.clone();
        let sequence = plan_source_edit(
            snapshot,
            &sequence_id,
            snapshot.document().revision,
            &source.monitor,
            program.monitor.scheduler(),
            edit,
            program.extents.as_ref(),
            self.cancel,
        )?;
        let op = match edit.mode {
            AssemblyMode::Insert => "source_insert",
            AssemblyMode::Overwrite => "source_overwrite",
        };
        if !self.run.submit_sequence(op, sequence.clone()) {
            let code = self
                .run
                .ops()
                .last()
                .and_then(|o| o.get("error").and_then(Value::as_str))
                .unwrap_or("submit_failed")
                .to_owned();
            return Err(fail("submit_failed", code));
        }
        let revision = self.run.snapshot().map_or(0, |s| s.document().revision);
        if let Some(m) = &mut self.monitor {
            m.monitor.replace_sequence(sequence, &m.clock)?;
        }
        Ok(revision)
    }

    /// Applies a monitor key to `target` (marks and insert/overwrite read the source monitor).
    pub fn monitor_key(&mut self, target: MonitorTarget, key: MonitorKey) -> Result<(), MonitorFail> {
        match key {
            MonitorKey::Insert | MonitorKey::Overwrite => {
                let mode = if key == MonitorKey::Insert { AssemblyMode::Insert } else { AssemblyMode::Overwrite };
                self.source_edit(&SourceEdit { mode, targets: Vec::new(), fit: None }).map(|_| ())
            }
            MonitorKey::MarkIn | MonitorKey::MarkOut => match target {
                MonitorTarget::Source => {
                    let s = self.source_monitor.as_mut().ok_or_else(|| fail("no_source_monitor", ""))?;
                    if key == MonitorKey::MarkIn { s.monitor.mark_in_at_playhead() } else { s.monitor.mark_out_at_playhead() }
                }
                MonitorTarget::Program => {
                    let m = self.monitor.as_mut().ok_or_else(|| fail("no_program_monitor", ""))?;
                    m.monitor.mark_at_playhead(key == MonitorKey::MarkIn)
                }
            },
            _ => match target {
                MonitorTarget::Source => {
                    let s = self.source_monitor.as_mut().ok_or_else(|| fail("no_source_monitor", ""))?;
                    transport_key(&mut s.monitor.monitor, key, &s.clock)
                }
                MonitorTarget::Program => {
                    let m = self.monitor.as_mut().ok_or_else(|| fail("no_program_monitor", ""))?;
                    transport_key(&mut m.monitor, key, &m.clock)
                }
            },
        }
    }

    /// Source monitor panel: frame, transport (ids prefixed `source_monitor.`), marks and the
    /// insert / overwrite buttons; monitor keys go to the active monitor while no text field has
    /// focus.
    pub(super) fn source_monitor_ui(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.source_monitor else { return };
        if let Some(sources) = &s.sources {
            let _ = s.monitor.monitor.refresh(&s.clock, sources.as_ref(), &hsk_studio_pulse::UnknownExtents, self.cancel);
        }
        let frame = s.monitor.monitor.shown().map(|f| f.frame);
        if frame != s.shown {
            s.shown = frame;
            s.texture = s.monitor.monitor.shown().map(|f| {
                let image = super::monitor::colour_image(&f.image);
                ctx.load_texture("hsk-studio-source-monitor", image, egui::TextureOptions::NEAREST)
            });
        }
        let mut request = None;
        let mut active = s.active;
        let drawn = egui::TopBottomPanel::bottom(super::widget::id(widget::PANEL))
            .show(ctx, |ui| {
                let mut drawn: Vec<String> = Vec::new();
                ui.horizontal(|ui| {
                    ui.label("Source");
                    if ui.selectable_label(active == MonitorTarget::Source, "keys").on_hover_text("Monitor keys drive the source monitor").clicked() {
                        active = MonitorTarget::Source;
                    }
                    if ui.selectable_label(active == MonitorTarget::Program, "program keys").clicked() {
                        active = MonitorTarget::Program;
                    }
                });
                if let Some(t) = &s.texture {
                    ui.image((t.id(), t.size_vec2()));
                }
                ui.push_id(widget::PANEL, |ui| {
                    drawn.extend(
                        s.monitor
                            .monitor
                            .ui(ui, &s.clock)
                            .into_iter()
                            .map(|n| format!("{}{}", widget::TRANSPORT_PREFIX, n.strip_prefix("monitor.").unwrap_or(n))),
                    );
                });
                ui.horizontal(|ui| {
                    if ui.button("Mark In").on_hover_text("I").clicked() {
                        let _ = s.monitor.mark_in_at_playhead();
                    }
                    drawn.push(widget::MARK_IN.into());
                    if ui.button("Mark Out").on_hover_text("O").clicked() {
                        let _ = s.monitor.mark_out_at_playhead();
                    }
                    drawn.push(widget::MARK_OUT.into());
                    if ui.button("Clear").clicked() {
                        s.monitor.clear_marks();
                    }
                    drawn.push(widget::CLEAR_MARKS.into());
                    if ui.button("Insert").on_hover_text(",").clicked() {
                        request = Some(MonitorKey::Insert);
                    }
                    drawn.push(widget::INSERT.into());
                    if ui.button("Overwrite").on_hover_text(".").clicked() {
                        request = Some(MonitorKey::Overwrite);
                    }
                    drawn.push(widget::OVERWRITE.into());
                    let (i, o) = s.monitor.marks();
                    ui.label(format!("in {} out {}", i.map_or("-".into(), |v| v.to_string()), o.map_or("-".into(), |v| v.to_string())));
                });
                drawn
            })
            .inner;
        s.active = active;
        let playing = s.monitor.monitor.scheduler().transport() == Transport::Playing;
        self.drawn.push(widget::PANEL.into());
        self.drawn.extend(drawn);
        if playing {
            ctx.request_repaint();
        }
        if ctx.memory(|m| m.focused().is_none()) {
            let keys: Vec<MonitorKey> = ctx.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Key { key, pressed: true, modifiers, .. } => key_command(*key, modifiers.shift),
                        _ => None,
                    })
                    .collect()
            });
            for key in keys {
                if let Err(e) = self.monitor_key(active, key) {
                    self.say(format!("monitor key {key:?}: {}: {}", e.code, e.detail));
                }
            }
        }
        if let Some(key) = request
            && let Err(e) = self.monitor_key(MonitorTarget::Source, key)
        {
            self.say(format!("source edit: {}: {}", e.code, e.detail));
        }
    }
}

#[cfg(feature = "media")]
impl super::Viewer {
    /// Opens the source monitor on a media file's bytes decoded by reel (feature `media`: VC-3 /
    /// DNxHR, ProRes, Y4M in process; lane SMB-PR-SOURCE-MONITOR): the source is content-addressed
    /// (`sha256:<hex>`), its length and frame rate come from the container's video track, and the
    /// monitor's frames come from pulse's frame cache over reel ([`super::media::MediaFrames`]).
    /// Refused typed: reel's codes (`import_unsupported`, `not_video`, `limit_exceeded`, ...),
    /// `source_frame_rate` for a rate outside the STU-VID-013 table. Returns the source, ready for
    /// [`super::Viewer::source_edit`].
    pub fn open_source_media(&mut self, bytes: Vec<u8>, width: u32, height: u32) -> Result<ClipSource, MonitorFail> {
        use super::media::{MediaFrames, ReelFrameDecoder};
        let cancel = self.cancel;
        let digest = hsk_studio_reel::full_digest(&mut std::io::Cursor::new(&bytes[..]), 1 << 20, cancel)
            .map_err(|e| fail("media_read", e.to_string()))?;
        let hex = hsk_studio_reel::hex(&digest);
        let source = ClipSource::Media {
            artifact_manifest_id: format!("sha256:{hex}"),
            content_digest: folio::ContentDigest { algorithm: "sha256".into(), digest: hex },
            asset_id: None,
        };
        let mut decoder = ReelFrameDecoder::new();
        decoder
            .register(&source, std::sync::Arc::from(bytes))
            .map_err(|e| fail(e.code, e.detail))?;
        let (units, ticks_per_unit, _, _) = decoder.layout(&source, cancel).map_err(|e| fail(e.code, e.detail))?;
        let extent = units.checked_mul(ticks_per_unit).ok_or_else(|| fail("overflow", "source extent"))?;
        self.open_source_monitor(source.clone(), Ticks::new(extent), SequenceFrameRate { ticks_per_frame: ticks_per_unit }, width, height)?;
        let frames = MediaFrames::new(decoder, hsk_studio_pulse::FrameCacheLimits::default(), 4)?;
        self.set_source_monitor_sources(Box::new(frames));
        Ok(source)
    }
}
