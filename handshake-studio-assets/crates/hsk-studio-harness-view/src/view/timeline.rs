//! Timeline panel of the viewer (lane SMB-TIMELINE-UI-1; WORKSPACE > Panels > Timeline, F10): the
//! clip timeline of one document sequence ([STU-VID-022] "the clip timeline", [STU-SHL-091] panel
//! `timeline`). It shows a time ruler in the sequence's timecode, the tracks (caption and video
//! rows above, audio and submix rows below) with every clip as a named block at its timeline in and
//! out, the sequence markers and clip markers, the work-area bar, and the playhead.
//!
//! One clock: the playhead IS the program monitor's ([`super::monitor::ProgramMonitor`]) pulse
//! scheduler position (`playhead()` when paused, `master(clock)` while playing); opening the
//! timeline opens the program monitor on the same sequence when it is not open, scrubbing the ruler
//! scrubs that scheduler, and every committed edit replaces the monitor's sequence. Nothing here
//! keeps a second clock or a second copy of the sequence: the canonical sequence is always read
//! from the run's snapshot.
//!
//! Edits go through pulse's existing planners only and commit as ONE undoable step each through
//! `Run::submit_sequence` (the session history: EDIT > Undo / Ctrl+Z undoes them, [STU-VID-040]):
//! trim in / trim out = `TimelineEdit::TrimIn` / `TrimOut` (no ripple); delete = `LiftClip`; ripple
//! delete = `ExtractClip` ([STU-VID-028]); razor at the playhead = `Razor` per clip under the
//! playhead ([STU-VID-027]); move = `LiftClip` then a `plan_assembly_edit` OVERWRITE of the same
//! source window at the destination keeping the clip id (the clip's markers and enabled flag are
//! carried over; a linked clip is refused `linked_move_unsupported`; dragging onto another track of
//! the same kind moves it there). Markers: `RecordEdit::AddMarker` at the playhead frame,
//! `next_marker` / `prev_marker` jumps. Snapping ([STU-VID-035], parity row PR-12) uses pulse's
//! `snap_targets` / `nearest_snap`: dragged edges and the scrubbed playhead are attracted to edit
//! points, markers, work-area bounds and the sequence start within [`SNAP_PX`] screen pixels.
//! Drag-to-overwrite as the default move and the snap distance are Studio choices; Premiere Pro's
//! exact behaviour is UNVERIFIED (D5).
//!
//! Bounded drawing (V4-01/02): at most [`MAX_DRAWN_TRACKS`] rows, [`MAX_DRAWN_CLIPS`] blocks,
//! [`MAX_SCANNED_CLIPS`] clips scanned per row, [`MAX_DRAWN_MARKERS`] markers and
//! [`MAX_RULER_LABELS`] ruler labels per frame, whatever the sequence holds; clips whose end
//! overflows are counted, never drawn; a zero-length clip is drawn one pixel wide. Model
//! navigation: [`Viewer::timeline_command`] runs every panel action headlessly, every widget has a
//! stable name ([`widget`]), and `state_json()["timeline"]` projects the panel. Trims live in
//! module `trim` / `trim_session`; the Rate Stretch tool, Speed/Duration and slip at speed in
//! module `rate` (its rustdoc is the manual). Lane SMB-AU-MULTITRACK-1 (feature `audio-batch`)
//! adds waveform overviews in audio clips, automation lane rows under audio and submix tracks
//! ([`LaneRow`], [`LANE_PX`]) and clip gain, in module `audio` (its rustdoc is the manual).
//! Lane SMB-AU-MULTITRACK-2: clip volume envelopes of the audio routing entry follow every edit
//! committed here (trim, slip, razor, lift, ripple delete, move, rate) in the same history step
//! (`Run::submit_sequence_and_entry`), module `envelopes`.
#![deny(clippy::arithmetic_side_effects)]

#[cfg(feature = "audio-batch")]
mod audio;
#[cfg(feature = "audio-batch")]
pub(super) mod audio_ui;
pub(super) mod envelopes;
mod rate;
mod trim;
mod trim_session;

pub use rate::{SpeedFix, SpeedForm};

#[cfg(feature = "audio-batch")]
pub use audio::{
    AudioLanes, AutomationCommand, CLIP_GAIN_STEP_DB, LanePoint, LaneSpan, LaneTarget, LaneView, MAX_DRAWN_POINTS, MAX_PEAK_COLUMNS_PER_FRAME, PointId, sample_of, value_at,
};
#[cfg(feature = "audio-batch")]
pub use audio_ui::{LANE_BUTTON_PX, LANE_PAD, POINT_PX, lane_value, lane_y, nearest_frame};

pub use trim::{DEFAULT_MULTI_FRAME_STEP, EditMode, EditPoint, MAX_MULTI_FRAME_STEP, MAX_PLANNER_CALLS, TrimKind, TrimOutcome, TrimTool};
pub use trim_session::{TrimFocus, TrimSession};

use super::Viewer;
use hsk_studio_accord::Ticks;
use hsk_studio_folio::{self as folio, ClipSource, LabelColor, StudioClip, StudioMarker, StudioSequence, TrackKind};
use hsk_studio_pulse::{
    AssemblyEdit, AssemblyMode, AssemblyPoints, CMD_CLIP_SPEED, CMD_TOOL_RATE_STRETCH, EditContext, LinkedRazor, MarkerScope, RateEdge, RecordEdit, SnapTarget, SourceExtents, SourcePatch,
    SpeedDurationTarget, TickRange, TimeDisplay, TimelineEdit, UnknownExtents, adjacent_edit, format_ticks, marker_spans, nearest_snap, next_marker, plan_assembly_edit, plan_record_edit,
    plan_timeline_edit, prev_marker, snap_targets, speed_percent_label,
};
use serde_json::{Value, json};

/// Stable widget names of the timeline panel.
pub mod widget {
    pub const PANEL: &str = "timeline";
    pub const RULER: &str = "timeline.ruler";
    pub const LANES: &str = "timeline.lanes";
    pub const PLAYHEAD: &str = "timeline.playhead";
    pub const WORK_AREA: &str = "timeline.work_area";
    pub const SNAP: &str = "timeline.snap";
    pub const RAZOR: &str = "timeline.razor";
    pub const LIFT: &str = "timeline.lift";
    pub const RIPPLE_DELETE: &str = "timeline.ripple_delete";
    pub const ADD_MARKER: &str = "timeline.marker.add";
    pub const PREVIOUS_MARKER: &str = "timeline.marker.previous";
    pub const NEXT_MARKER: &str = "timeline.marker.next";
    pub const ZOOM_OUT: &str = "timeline.zoom_out";
    pub const ZOOM_IN: &str = "timeline.zoom_in";
    pub const ZOOM_FIT: &str = "timeline.zoom_fit";
    pub const SCROLL: &str = "timeline.scroll";
    /// One clip block: `timeline.clip.<clip id>`.
    pub fn clip(clip_id: &str) -> String {
        format!("timeline.clip.{clip_id}")
    }
    /// The trim handle of a clip's head (`.in`) or tail (`.out`).
    pub fn clip_edge(clip_id: &str, head: bool) -> String {
        format!("timeline.clip.{clip_id}.{}", if head { "in" } else { "out" })
    }
    /// One track header: `timeline.track.<track id>`.
    pub fn track(track_id: &str) -> String {
        format!("timeline.track.{track_id}")
    }
    /// One marker on the ruler: `timeline.marker.<marker id>`.
    pub fn marker(marker_id: &str) -> String {
        format!("timeline.marker.{marker_id}")
    }
    /// One trim-mode selector of the toolbar: `timeline.tool.<tool key>` (lane SMB-TIMELINE-TRIM-1).
    pub fn tool(key: &str) -> String {
        format!("timeline.tool.{key}")
    }
    pub const EXTEND_EDIT: &str = "timeline.extend_edit";
    pub const TRIM_SESSION_ENTER: &str = "timeline.trim_session.enter";
    pub const TRIM_SESSION: &str = "timeline.trim_session";
    pub const TRIM_OUTGOING: &str = "timeline.trim_session.outgoing";
    pub const TRIM_INCOMING: &str = "timeline.trim_session.incoming";
    pub const TRIM_MINUS_MAJOR: &str = "timeline.trim_session.minus_major";
    pub const TRIM_MINUS_MINOR: &str = "timeline.trim_session.minus_minor";
    pub const TRIM_PLUS_MINOR: &str = "timeline.trim_session.plus_minor";
    pub const TRIM_PLUS_MAJOR: &str = "timeline.trim_session.plus_major";
    pub const TRIM_FOCUS_IN: &str = "timeline.trim_session.focus_in";
    pub const TRIM_FOCUS_OUT: &str = "timeline.trim_session.focus_out";
    pub const TRIM_FOCUS_BOTH: &str = "timeline.trim_session.focus_both";
    pub const TRIM_TYPE: &str = "timeline.trim_session.type";
    pub const TRIM_COMPOSITE: &str = "timeline.trim_session.composite_preview";
    pub const TRIM_COMMIT: &str = "timeline.trim_session.commit";
    pub const TRIM_ROLLBACK: &str = "timeline.trim_session.rollback";
    /// Speed/Duration form (lane SMB-PR-RATE-STRETCH-1; module `rate`).
    pub const SPEED_FORM: &str = "timeline.speed_form";
    pub const SPEED_PERCENT: &str = "timeline.speed_form.percent";
    pub const SPEED_DURATION: &str = "timeline.speed_form.duration";
    pub const SPEED_REVERSE: &str = "timeline.speed_form.reverse";
    pub const SPEED_PITCH: &str = "timeline.speed_form.maintain_audio_pitch";
    pub const SPEED_INTERPOLATION: &str = "timeline.speed_form.interpolation";
    pub const SPEED_APPLY: &str = "timeline.speed_form.apply";
    pub const SPEED_CANCEL: &str = "timeline.speed_form.cancel";
    /// Multitrack audio (lane SMB-AU-MULTITRACK-1; module `audio`).
    pub const WAVEFORMS: &str = "timeline.waveforms";
    pub const CLIP_GAIN_DOWN: &str = "timeline.clip_gain.down";
    pub const CLIP_GAIN_UP: &str = "timeline.clip_gain.up";
    /// The waveform overview drawn in an audio clip block.
    pub fn clip_waveform(clip_id: &str) -> String {
        format!("timeline.clip.{clip_id}.waveform")
    }
    /// The automation lane button of a track header.
    pub fn lane_toggle(track_id: &str) -> String {
        format!("timeline.track.{track_id}.lane")
    }
    /// The automation lane row of a track.
    pub fn lane(track_id: &str) -> String {
        format!("timeline.lane.{track_id}")
    }
    /// One point of a lane: `timeline.lane.<track>.point.<clip id or "seq">.<offset ticks>`.
    pub fn lane_point(track_id: &str, clip_id: Option<&str>, offset_ticks: u64) -> String {
        format!("timeline.lane.{track_id}.point.{}.{offset_ticks}", clip_id.unwrap_or("seq"))
    }
}

/// Track header column width (pixels).
pub const HEADER_PX: f32 = 64.0;
/// Ruler band height (pixels).
pub const RULER_PX: f32 = 22.0;
/// Work-area bar height under the ruler (pixels).
pub const WORK_AREA_PX: f32 = 6.0;
/// Height of one track row (pixels).
pub const TRACK_PX: f32 = 30.0;
/// Height of one automation lane row under its track (pixels; lane SMB-AU-MULTITRACK-1).
pub const LANE_PX: f32 = 44.0;
/// Width of a clip's trim handle at each end (pixels; narrower clips split into thirds).
pub const EDGE_PX: f32 = 6.0;
/// Snap distance in screen pixels, turned into ticks at the current zoom.
pub const SNAP_PX: f32 = 8.0;
pub const MIN_PX_PER_FRAME: f64 = 1.0 / 4096.0;
pub const MAX_PX_PER_FRAME: f64 = 64.0;
pub const DEFAULT_PX_PER_FRAME: f64 = 4.0;
/// Drawing bounds per frame (module docs).
pub const MAX_DRAWN_TRACKS: usize = 64;
pub const MAX_DRAWN_CLIPS: usize = 2048;
pub const MAX_SCANNED_CLIPS: usize = 16_384;
pub const MAX_DRAWN_MARKERS: usize = 1024;
pub const MAX_RULER_LABELS: usize = 64;
/// Smallest spacing of two ruler labels (pixels).
const RULER_LABEL_PX: f64 = 90.0;
/// Longest clip label kept (characters).
const MAX_LABEL_CHARS: usize = 64;

/// Horizontal view: zoom in pixels per frame and the first visible frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimelineView {
    pub px_per_frame: f64,
    pub scroll_frames: f64,
}

impl Default for TimelineView {
    fn default() -> Self {
        Self { px_per_frame: DEFAULT_PX_PER_FRAME, scroll_frames: 0.0 }
    }
}

/// Ticks per frame of `sequence` (a hostile 0 reads as 1 so geometry never divides by zero).
pub fn ticks_per_frame(sequence: &StudioSequence) -> u64 {
    sequence.frame_rate.ticks_per_frame.max(1)
}

/// Frames (fractional) of `t`.
pub fn frames_of(t: Ticks, tpf: u64) -> f64 {
    t.value() as f64 / tpf.max(1) as f64
}

/// Screen x of tick `t` with the lanes starting at `lanes_left`.
pub fn x_of(t: Ticks, tpf: u64, view: &TimelineView, lanes_left: f32) -> f32 {
    lanes_left + ((frames_of(t, tpf) - view.scroll_frames) * view.px_per_frame) as f32
}

/// The whole frame under screen x (never negative), as ticks.
pub fn tick_at_x(x: f32, tpf: u64, view: &TimelineView, lanes_left: f32) -> Ticks {
    let frame = (view.scroll_frames + f64::from(x - lanes_left) / view.px_per_frame).floor().max(0.0);
    Ticks::new((frame as u64).saturating_mul(tpf))
}

/// Draw order of the tracks: caption and video rows top-down by descending index, then audio and
/// submix rows by ascending index (the video/audio split of an edit timeline).
pub fn track_order(sequence: &StudioSequence) -> Vec<usize> {
    let rank = |k: TrackKind| match k {
        TrackKind::Caption => 0u8,
        TrackKind::Video => 1,
        TrackKind::Audio => 2,
        TrackKind::Submix => 3,
    };
    let mut order: Vec<usize> = (0..sequence.tracks.len()).collect();
    order.sort_by_key(|&i| {
        let t = &sequence.tracks[i];
        let above = matches!(t.kind, TrackKind::Caption | TrackKind::Video);
        (rank(t.kind), if above { u32::MAX.saturating_sub(t.index) } else { t.index })
    });
    order
}

/// The display name of a clip: its media manifest id, nested sequence id or primitive object id.
pub fn clip_label(clip: &StudioClip) -> String {
    let text = match &clip.source_ref {
        ClipSource::Media { artifact_manifest_id, .. } => artifact_manifest_id.clone(),
        ClipSource::Sequence { sequence_id } => format!("sequence {sequence_id}"),
        ClipSource::Primitive { object_id, .. } => object_id.clone(),
    };
    let name: String = text.chars().take(MAX_LABEL_CHARS).collect();
    // A clip not at 100 % shows its speed (lane SMB-PR-RATE-STRETCH-1).
    if clip.speed == folio::Speed::UNIT { name } else { format!("{name} ({}%)", speed_percent_label(clip.speed)) }
}

/// One drawn track row (header plus lane).
#[derive(Clone, Debug, PartialEq)]
pub struct TrackRow {
    pub track_id: String,
    pub name: String,
    pub kind: TrackKind,
    pub locked: bool,
    pub rect: egui::Rect,
}

/// One drawn automation lane row (under the row of `track_id`; lanes start at `lanes_left`).
#[derive(Clone, Debug, PartialEq)]
pub struct LaneRow {
    pub track_id: String,
    pub rect: egui::Rect,
}

/// One drawn clip block. `x0` / `x1` are the unclipped screen positions of its in and out; `rect`
/// is the block clipped to the lanes (at least one pixel wide).
#[derive(Clone, Debug, PartialEq)]
pub struct ClipBlock {
    pub clip_id: String,
    pub track_id: String,
    pub kind: TrackKind,
    pub label: String,
    pub start: Ticks,
    pub end: Ticks,
    pub enabled: bool,
    pub x0: f32,
    pub x1: f32,
    pub rect: egui::Rect,
}

/// One drawn marker (span markers have `x_end > x`).
#[derive(Clone, Debug, PartialEq)]
pub struct MarkerMark {
    pub marker_id: String,
    pub at: Ticks,
    pub x: f32,
    pub x_end: f32,
}

/// The geometry of one timeline frame (pure; [`layout`]).
#[derive(Clone, Debug, PartialEq)]
pub struct TimelineLayout {
    pub area: egui::Rect,
    /// Screen x where the time lanes start (right of the track headers).
    pub lanes_left: f32,
    pub ruler: egui::Rect,
    pub tracks: Vec<TrackRow>,
    /// Automation lane rows (lane SMB-AU-MULTITRACK-1), in draw order.
    pub lanes: Vec<LaneRow>,
    pub clips: Vec<ClipBlock>,
    pub markers: Vec<MarkerMark>,
    /// `(x, label)` per ruler label.
    pub ruler_labels: Vec<(f32, String)>,
    pub work_area: Option<(f32, f32)>,
    /// Rows not drawn (panel height or [`MAX_DRAWN_TRACKS`]).
    pub tracks_hidden: usize,
    /// A drawing bound cut the clip blocks short.
    pub clips_truncated: bool,
    /// Clips whose end overflows u64 ticks (not drawn).
    pub unplaceable: usize,
}

impl TimelineLayout {
    pub fn clip(&self, clip_id: &str) -> Option<&ClipBlock> {
        self.clips.iter().find(|c| c.clip_id == clip_id)
    }
    /// The drawn row under screen y.
    pub fn track_at(&self, y: f32) -> Option<&TrackRow> {
        self.tracks.iter().find(|t| t.rect.min.y <= y && y < t.rect.max.y)
    }
    /// The drawn automation lane row of `track_id`.
    pub fn lane(&self, track_id: &str) -> Option<&LaneRow> {
        self.lanes.iter().find(|l| l.track_id == track_id)
    }
}

/// Ruler label step in frames: the smallest of 1, 2, 5, 10, 25, 50, 100, 250, 500, ... at least
/// [`RULER_LABEL_PX`] apart at `px_per_frame`.
fn ruler_step(px_per_frame: f64) -> u64 {
    let fits = |s: u64| s as f64 * px_per_frame >= RULER_LABEL_PX;
    if let Some(s) = [1u64, 2, 5].into_iter().find(|s| fits(*s)) {
        return s;
    }
    let mut decade: u64 = 10;
    for _ in 0..20 {
        // decade, 2.5 x decade, 5 x decade (decade is a multiple of 10, so 2.5 x is whole).
        for s in [decade, (decade / 2).saturating_mul(5), decade.saturating_mul(5)] {
            if fits(s) {
                return s;
            }
        }
        decade = decade.saturating_mul(10);
    }
    u64::MAX
}

/// Lays out `sequence` in `area` (module docs; bounded). `markers` are the sequence's visible marker
/// spans sorted by start ([`hsk_studio_pulse::marker_spans`]).
pub fn layout(sequence: &StudioSequence, view: &TimelineView, area: egui::Rect, markers: &[(String, TickRange)]) -> TimelineLayout {
    layout_with_lanes(sequence, view, area, markers, &[])
}

/// [`layout`] with one automation lane row of [`LANE_PX`] under each track named in `lane_tracks`
/// (when it fits in `area`).
pub fn layout_with_lanes(sequence: &StudioSequence, view: &TimelineView, area: egui::Rect, markers: &[(String, TickRange)], lane_tracks: &[String]) -> TimelineLayout {
    let tpf = ticks_per_frame(sequence);
    let lanes_left = area.left() + HEADER_PX;
    let lanes_right = area.right().max(lanes_left);
    let ruler = egui::Rect::from_min_max(egui::pos2(lanes_left, area.top()), egui::pos2(lanes_right, area.top() + RULER_PX));
    let mut out = TimelineLayout {
        area,
        lanes_left,
        ruler,
        tracks: Vec::new(),
        lanes: Vec::new(),
        clips: Vec::new(),
        markers: Vec::new(),
        ruler_labels: Vec::new(),
        work_area: None,
        tracks_hidden: 0,
        clips_truncated: false,
        unplaceable: 0,
    };
    let width = f64::from(lanes_right - lanes_left);
    let f0 = view.scroll_frames.max(0.0);
    let f1 = f0 + width / view.px_per_frame;
    let t0 = Ticks::new((f0 * tpf as f64) as u64);
    let t1 = Ticks::new((f1 * tpf as f64).ceil() as u64);
    // Ruler labels.
    let rate = sequence.frame_rate.accord();
    let step = ruler_step(view.px_per_frame);
    let mut frame = (f0 as u64).div_ceil(step).saturating_mul(step);
    while (frame as f64) <= f1 && out.ruler_labels.len() < MAX_RULER_LABELS {
        let t = Ticks::new(frame.saturating_mul(tpf));
        let text = match rate {
            Some(r) => format_ticks(t, r, TimeDisplay::matching(r), None).unwrap_or_else(|_| frame.to_string()),
            None => frame.to_string(),
        };
        out.ruler_labels.push((x_of(t, tpf, view, lanes_left), text));
        let Some(next) = frame.checked_add(step) else { break };
        frame = next;
    }
    if let Some(a) = &sequence.work_area {
        out.work_area = Some((x_of(a.start, tpf, view, lanes_left), x_of(a.end, tpf, view, lanes_left)));
    }
    // Markers (sorted by start; spans reaching into the view from the left are found by scanning
    // back at most the drawing bound).
    let first = markers.partition_point(|(_, s)| s.start() < t0);
    let back = first.saturating_sub(MAX_DRAWN_MARKERS);
    for (id, span) in markers.get(back..).unwrap_or(&[]) {
        if span.start() > t1 || out.markers.len() >= MAX_DRAWN_MARKERS {
            break;
        }
        if span.end() < t0 {
            continue;
        }
        out.markers.push(MarkerMark { marker_id: id.clone(), at: span.start(), x: x_of(span.start(), tpf, view, lanes_left), x_end: x_of(span.end(), tpf, view, lanes_left) });
    }
    // Tracks and clips.
    let mut y = area.top() + RULER_PX + WORK_AREA_PX;
    let order = track_order(sequence);
    for (n, &i) in order.iter().enumerate() {
        if n >= MAX_DRAWN_TRACKS || y + TRACK_PX > area.bottom() {
            out.tracks_hidden = order.len().saturating_sub(n);
            break;
        }
        let track = &sequence.tracks[i];
        let row = egui::Rect::from_min_max(egui::pos2(area.left(), y), egui::pos2(lanes_right, y + TRACK_PX));
        out.tracks.push(TrackRow { track_id: track.track_id.clone(), name: track.name.chars().take(MAX_LABEL_CHARS).collect(), kind: track.kind, locked: track.locked, rect: row });
        let clips = &track.clips;
        // Valid sequences are ordered and non-overlapping, so ends are ordered too.
        let start = clips.partition_point(|c| c.timeline_start.value().saturating_add(c.duration.value()) < t0.value());
        let mut scanned = 0usize;
        for clip in clips.get(start..).unwrap_or(&[]) {
            scanned = scanned.saturating_add(1);
            if scanned > MAX_SCANNED_CLIPS || out.clips.len() >= MAX_DRAWN_CLIPS {
                out.clips_truncated = true;
                break;
            }
            if clip.timeline_start > t1 {
                break;
            }
            let Some(end) = clip.timeline_end() else {
                out.unplaceable = out.unplaceable.saturating_add(1);
                continue;
            };
            if end < t0 {
                continue;
            }
            let (x0, x1) = (x_of(clip.timeline_start, tpf, view, lanes_left), x_of(end, tpf, view, lanes_left));
            let left = x0.max(lanes_left);
            let right = x1.min(lanes_right).max(left + 1.0);
            let rect = egui::Rect::from_min_max(egui::pos2(left, y + 2.0), egui::pos2(right, y + TRACK_PX - 2.0));
            out.clips.push(ClipBlock {
                clip_id: clip.clip_id.clone(),
                track_id: track.track_id.clone(),
                kind: track.kind,
                label: clip_label(clip),
                start: clip.timeline_start,
                end,
                enabled: clip.enabled,
                x0,
                x1,
                rect,
            });
        }
        y += TRACK_PX;
        if lane_tracks.contains(&track.track_id) && y + LANE_PX <= area.bottom() {
            out.lanes.push(LaneRow { track_id: track.track_id.clone(), rect: egui::Rect::from_min_max(egui::pos2(area.left(), y), egui::pos2(lanes_right, y + LANE_PX)) });
            y += LANE_PX;
        }
    }
    out
}

/// What a drag changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragKind {
    Move,
    TrimIn,
    TrimOut,
    /// Ripple tool on a clip head / tail (lane SMB-TIMELINE-TRIM-1).
    RippleIn,
    RippleOut,
    /// Roll tool on the edit at the dragged clip's tail (a head edit drags as the previous
    /// abutting clip's tail).
    Roll,
    /// Slip / slide tool on the clip body.
    Slip,
    Slide,
    /// Rate Stretch tool on a clip head / tail (lane SMB-PR-RATE-STRETCH-1).
    RateStretchIn,
    RateStretchOut,
}

impl DragKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::TrimIn => "trim_in",
            Self::TrimOut => "trim_out",
            Self::RippleIn => "ripple_in",
            Self::RippleOut => "ripple_out",
            Self::Roll => "roll",
            Self::Slip => "slip",
            Self::Slide => "slide",
            Self::RateStretchIn => "rate_stretch_in",
            Self::RateStretchOut => "rate_stretch_out",
        }
    }
}

/// Where a drag would put the clip now.
#[derive(Clone, Debug, PartialEq)]
pub struct DragTarget {
    pub start: Ticks,
    pub end: Ticks,
    pub track_id: String,
    pub snapped: Option<SnapTarget>,
    /// Signed whole frames the drag moves its edge, its clip or (slip) its source window.
    pub delta_frames: i64,
}

/// A clip drag in progress (pointer x at the press, the clip as it was).
#[derive(Clone, Debug, PartialEq)]
pub struct Drag {
    pub clip_id: String,
    pub kind: DragKind,
    pub origin_x: f32,
    pub start: Ticks,
    pub end: Ticks,
    pub track_id: String,
    pub track_kind: TrackKind,
    pub preview: Option<DragTarget>,
}

fn shifted(t: Ticks, delta: i128) -> Ticks {
    let v = i128::from(t.value()).saturating_add(delta).clamp(0, i128::from(u64::MAX));
    Ticks::new(u64::try_from(v).unwrap_or(u64::MAX))
}

/// The drag result for a pointer moved `dx` pixels: whole frames at the current zoom, then (with
/// `targets`) the moving edge snapped to the nearest target within [`SNAP_PX`]. Moves keep the
/// duration and never pass tick 0; trims keep at least one frame.
pub fn drag_target(drag: &Drag, dx: f32, dest_track: &str, view: &TimelineView, tpf: u64, targets: Option<&[SnapTarget]>) -> DragTarget {
    let frames = (f64::from(dx) / view.px_per_frame).round().clamp(-1e15, 1e15) as i64;
    let delta = i128::from(frames).saturating_mul(i128::from(tpf));
    let distance = Ticks::new((f64::from(SNAP_PX) / view.px_per_frame * tpf as f64) as u64);
    let snap = |at: Ticks| targets.and_then(|t| nearest_snap(t, at, distance)).cloned();
    let duration = drag.end.value().saturating_sub(drag.start.value());
    let (mut start, mut end, mut snapped) = (drag.start, drag.end, None);
    match drag.kind {
        DragKind::Slip => {}
        DragKind::Move | DragKind::Slide => {
            start = shifted(drag.start, delta);
            end = Ticks::new(start.value().saturating_add(duration));
            let head = snap(start).map(|t| (t.at.value().abs_diff(start.value()), true, t));
            let tail = snap(end).map(|t| (t.at.value().abs_diff(end.value()), false, t));
            let best = match (head, tail) {
                (Some(h), Some(t)) => Some(if t.0 < h.0 { t } else { h }),
                (h, t) => h.or(t),
            };
            if let Some((_, is_head, t)) = best {
                let new_start = if is_head { t.at.value() } else { t.at.value().saturating_sub(duration) };
                if is_head || t.at.value() >= duration {
                    start = Ticks::new(new_start);
                    end = Ticks::new(new_start.saturating_add(duration));
                    snapped = Some(t);
                }
            }
        }
        DragKind::TrimIn | DragKind::RippleIn | DragKind::RateStretchIn => {
            let limit = drag.end.value().saturating_sub(tpf);
            start = Ticks::new(shifted(drag.start, delta).value().min(limit));
            if let Some(t) = snap(start).filter(|t| t.at.value() <= limit) {
                start = t.at;
                snapped = Some(t);
            }
        }
        DragKind::TrimOut | DragKind::RippleOut | DragKind::Roll | DragKind::RateStretchOut => {
            let limit = drag.start.value().saturating_add(tpf);
            end = Ticks::new(shifted(drag.end, delta).value().max(limit));
            if let Some(t) = snap(end).filter(|t| t.at.value() >= limit) {
                end = t.at;
                snapped = Some(t);
            }
        }
    }
    let moved = |now: Ticks, was: Ticks| {
        let d = i128::from(now.value()).saturating_sub(i128::from(was.value())).checked_div(i128::from(tpf.max(1))).unwrap_or(0);
        i64::try_from(d).unwrap_or(if d < 0 { i64::MIN } else { i64::MAX })
    };
    let delta_frames = match drag.kind {
        DragKind::Slip => frames,
        DragKind::TrimOut | DragKind::RippleOut | DragKind::Roll | DragKind::RateStretchOut => moved(end, drag.end),
        DragKind::Move | DragKind::Slide | DragKind::TrimIn | DragKind::RippleIn | DragKind::RateStretchIn => moved(start, drag.start),
    };
    DragTarget { start, end, track_id: dest_track.to_owned(), snapped, delta_frames }
}

/// The edit a finished drag commits (`None`: nothing changed).
pub fn drag_command(drag: &Drag, target: &DragTarget) -> Option<TimelineCommand> {
    match drag.kind {
        DragKind::Move => (target.start != drag.start || target.track_id != drag.track_id).then(|| TimelineCommand::MoveClip {
            clip_id: drag.clip_id.clone(),
            start: target.start,
            track_id: (target.track_id != drag.track_id).then(|| target.track_id.clone()),
        }),
        DragKind::TrimIn => (target.start != drag.start).then(|| TimelineCommand::TrimIn { clip_id: drag.clip_id.clone(), head: target.start }),
        DragKind::TrimOut => (target.end != drag.end).then(|| TimelineCommand::TrimOut { clip_id: drag.clip_id.clone(), tail: target.end }),
        DragKind::RateStretchIn | DragKind::RateStretchOut => (target.delta_frames != 0).then(|| TimelineCommand::RateStretch {
            clip_id: drag.clip_id.clone(),
            edge: if drag.kind == DragKind::RateStretchIn { RateEdge::Head } else { RateEdge::Tail },
            delta_frames: target.delta_frames,
        }),
        DragKind::RippleIn | DragKind::RippleOut | DragKind::Roll | DragKind::Slip | DragKind::Slide => {
            let (clip_id, delta_frames) = (drag.clip_id.clone(), target.delta_frames);
            (delta_frames != 0).then_some(match drag.kind {
                DragKind::RippleIn => TimelineCommand::RippleTrimIn { clip_id, delta_frames },
                DragKind::RippleOut => TimelineCommand::RippleTrimOut { clip_id, delta_frames },
                DragKind::Roll => TimelineCommand::Roll { clip_id, delta_frames },
                DragKind::Slip => TimelineCommand::Slip { clip_id, delta_frames },
                _ => TimelineCommand::Slide { clip_id, delta_frames },
            })
        }
    }
}

/// Every timeline panel action (buttons, keys, drags, ruler); [`Viewer::timeline_command`] runs one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimelineCommand {
    Select(Option<String>),
    /// Moves the clip to start at `start` (overwrite at the destination), on `track_id` when given.
    MoveClip { clip_id: String, start: Ticks, track_id: Option<String> },
    /// Moves the clip's head to timeline tick `head` (no ripple).
    TrimIn { clip_id: String, head: Ticks },
    /// Moves the clip's tail to timeline tick `tail` (no ripple).
    TrimOut { clip_id: String, tail: Ticks },
    /// Splits the selected clip at the playhead, else every clip under the playhead on the
    /// targeted tracks (all tracks when none is targeted).
    RazorAtPlayhead,
    /// Removes the selected clip and leaves a gap.
    Lift,
    /// Removes the selected clip and closes the gap.
    RippleDelete,
    /// Adds a sequence marker at the playhead frame.
    AddMarker,
    NextMarker,
    PreviousMarker,
    NextEdit,
    PreviousEdit,
    ToggleSnap,
    ZoomIn,
    ZoomOut,
    ZoomToFit,
    /// Sets the first visible frame.
    ScrollTo(u64),
    /// Moves the playhead (the program monitor's scheduler) to `tick`.
    Scrub(Ticks),
    // ---- trims, nudges and the trim session (lane SMB-TIMELINE-TRIM-1; module `trim`) ----
    /// Ripple trim of the clip's head by `delta_frames` (the clip keeps its start; later content on
    /// its track and on every sync-locked track shifts, [STU-VID-026]). Every trim below commits
    /// the achieved delta ([`TrimOutcome`], `state_json()["timeline"]["last_trim"]`).
    RippleTrimIn { clip_id: String, delta_frames: i64 },
    /// Ripple trim of the clip's tail by `delta_frames`.
    RippleTrimOut { clip_id: String, delta_frames: i64 },
    /// Rolls the edit between the clip (outgoing) and the clip abutting its tail by `delta_frames`.
    Roll { clip_id: String, delta_frames: i64 },
    /// Moves the clip's source window by `delta_frames` (timeline start and duration fixed).
    Slip { clip_id: String, delta_frames: i64 },
    /// Moves the clip by `delta_frames` between its two abutting neighbours.
    Slide { clip_id: String, delta_frames: i64 },
    /// Selects the edit nearest `nearest_to` on the targeted tracks in `mode` ([STU-VID-026b]).
    SelectEdit { mode: EditMode, nearest_to: Ticks },
    /// Selects the edit point of `clip_id` in `mode` (an edge click in the panel).
    EditPointAt { clip_id: String, mode: EditMode },
    /// Trim forward / backward by one frame or by the multi-frame step: the open trim session's
    /// plus / minus, else the selected edit point trimmed in its mode.
    TrimNudge { forward: bool, many: bool },
    /// Slips the selected clip one frame or the multi-frame step ([STU-VID-035a]).
    SlipNudge { right: bool, many: bool },
    /// Slides the selected clip one frame or the multi-frame step.
    SlideNudge { right: bool, many: bool },
    /// Moves the selected clip one frame or the multi-frame step (overwrite at the destination).
    ClipNudge { right: bool, many: bool },
    /// Rolls the selected roll edit, else the nearest roll edit on the targeted tracks, to the
    /// playhead.
    ExtendEditToPlayhead,
    /// The one multi-frame step shared by every "many" variant (1..=[`MAX_MULTI_FRAME_STEP`]).
    SetMultiFrameStep(u32),
    /// The trim mode of edge and body drags in the panel.
    SetTool(TrimTool),
    /// Enters a trim session on the selected edit point ([STU-VID-026a]).
    TrimSessionEnter,
    TrimSessionFocus(TrimFocus),
    /// Toggles the session's trim type between ripple and roll.
    TrimSessionToggleType,
    /// Commits the session's adjustment as ONE history step and closes it.
    TrimSessionCommit,
    /// Abandons the session (nothing was committed while it was open).
    TrimSessionRollback,
    /// Composite preview during trim on / off (a display preference).
    ToggleCompositePreview,
    // ---- rate stretch and Speed/Duration (lane SMB-PR-RATE-STRETCH-1; module `rate`) ----
    /// Moves the clip's `edge` by `delta_frames` changing its speed, never its source window
    /// ([STU-VID-026] "Rate stretch"); commits the achieved delta.
    RateStretch { clip_id: String, edge: RateEdge, delta_frames: i64 },
    /// Sets the clip's speed or duration (the other is derived) and direction; the head stays.
    SpeedDuration { clip_id: String, target: SpeedDurationTarget, reverse: bool },
    /// Opens the Speed/Duration form on the selected clip (`Ctrl+R`).
    SpeedDurationOpen,
    /// Applies the open form ([`SpeedForm::command`]) and closes it.
    SpeedDurationApply,
    /// Closes the form without a change.
    SpeedDurationCancel,
}

impl TimelineCommand {
    /// Stable harness command key.
    pub fn key(&self) -> &'static str {
        match self {
            Self::Select(_) => "select",
            Self::MoveClip { .. } => "move_clip",
            Self::TrimIn { .. } => "trim_in",
            Self::TrimOut { .. } => "trim_out",
            Self::RazorAtPlayhead => "razor",
            Self::Lift => "lift",
            Self::RippleDelete => "ripple_delete",
            Self::AddMarker => "marker_add",
            Self::NextMarker => "marker_next",
            Self::PreviousMarker => "marker_previous",
            Self::NextEdit => "edit_next",
            Self::PreviousEdit => "edit_previous",
            Self::ToggleSnap => "snap",
            Self::ZoomIn => "zoom_in",
            Self::ZoomOut => "zoom_out",
            Self::ZoomToFit => "zoom_fit",
            Self::ScrollTo(_) => "scroll",
            Self::Scrub(_) => "scrub",
            Self::RippleTrimIn { .. } => "ripple_trim_in",
            Self::RippleTrimOut { .. } => "ripple_trim_out",
            Self::Roll { .. } => "roll",
            Self::Slip { .. } => "slip",
            Self::Slide { .. } => "slide",
            Self::SelectEdit { .. } => "edit_select",
            Self::EditPointAt { .. } => "edit_point",
            Self::TrimNudge { .. } => "trim_nudge",
            Self::SlipNudge { .. } => "slip_nudge",
            Self::SlideNudge { .. } => "slide_nudge",
            Self::ClipNudge { .. } => "clip_nudge",
            Self::ExtendEditToPlayhead => "extend_edit",
            Self::SetMultiFrameStep(_) => "multi_frame_step",
            Self::SetTool(_) => "tool",
            Self::TrimSessionEnter => "trim_session_enter",
            Self::TrimSessionFocus(_) => "trim_session_focus",
            Self::TrimSessionToggleType => "trim_session_type",
            Self::TrimSessionCommit => "trim_session_commit",
            Self::TrimSessionRollback => "trim_session_rollback",
            Self::ToggleCompositePreview => "composite_preview",
            Self::RateStretch { .. } => "rate_stretch",
            Self::SpeedDuration { .. } => "speed_duration",
            Self::SpeedDurationOpen => "speed_duration_open",
            Self::SpeedDurationApply => "speed_duration_apply",
            Self::SpeedDurationCancel => "speed_duration_cancel",
        }
    }
    /// The recovered command id of the module 14 command tables ([STU-VID-041]) this action runs,
    /// where the tables name one.
    pub fn spec_command(&self) -> Option<&'static str> {
        Some(match self {
            Self::RazorAtPlayhead => "cmd.sequence.razorateditline",
            Self::RippleDelete => "cmd.timeline.ripple.delete",
            Self::ToggleSnap => "cmd.sequence.snap",
            Self::NextMarker => "cmd.marker.gotomarker.next",
            Self::PreviousMarker => "cmd.marker.gotomarker.previous",
            Self::NextEdit => "cmd.tlnav.next.edit",
            Self::PreviousEdit => "cmd.tlnav.prev.edit",
            Self::SelectEdit { mode, .. } => mode.spec_command(),
            Self::TrimNudge { forward, many } => match (forward, many) {
                (true, false) => "cmd.trim.plus.minor",
                (true, true) => "cmd.trim.plus.major",
                (false, false) => "cmd.trim.minus.minor",
                (false, true) => "cmd.trim.minus.major",
            },
            Self::SlipNudge { right, many } => match (right, many) {
                (false, false) => "cmd.timeline.slip.left.one",
                (false, true) => "cmd.timeline.slip.left.several",
                (true, false) => "cmd.timeline.slip.right.one",
                (true, true) => "cmd.timeline.slip.right.several",
            },
            Self::SlideNudge { right, many } => match (right, many) {
                (false, false) => "cmd.timeline.slide.left.one",
                (false, true) => "cmd.timeline.slide.left.several",
                (true, false) => "cmd.timeline.slide.right.one",
                (true, true) => "cmd.timeline.slide.right.several",
            },
            Self::TrimSessionFocus(f) => f.spec_command(),
            Self::ToggleCompositePreview => "cmd.timeline.composite.preview.during.trim",
            Self::SetTool(TrimTool::RateStretch) => CMD_TOOL_RATE_STRETCH,
            Self::SpeedDurationOpen | Self::SpeedDuration { .. } | Self::SpeedDurationApply => CMD_CLIP_SPEED,
            _ => return None,
        })
    }
}

/// Keyboard map of the timeline panel (active after a click in the panel or opening it):
/// Ctrl+K razor, Delete / Backspace lift, Alt+Backspace and Shift+Delete ripple delete, S snapping,
/// M add marker, Shift+M next marker, Ctrl+Shift+M previous marker, Up / Down previous / next edit,
/// `=` / `-` zoom, `\` zoom to fit. Bindings from the module 14 tables: Ctrl+K, Alt+Backspace, S,
/// Shift+M, Ctrl+Shift+M, Up, Down; the rest are harness bindings. Lane SMB-TIMELINE-TRIM-1 adds
/// the [STU-VID-041] slip and slide nudges (Ctrl+Alt+Left / Right slip one frame, with Shift the
/// multi-frame step; Alt+, / Alt+. slide one frame, with Shift the step) and the harness clip nudge
/// Alt+Left / Right (with Shift the step; Premiere's binding UNVERIFIED). Lane SMB-PR-RATE-STRETCH-1
/// adds `R` (Rate Stretch tool, `cmd.tools.05ratestretch`) and `Ctrl+R` (Speed/Duration form,
/// `cmd.clip.speed`).
pub fn key_command(key: egui::Key, m: egui::Modifiers) -> Option<TimelineCommand> {
    use egui::Key;
    Some(match key {
        Key::ArrowLeft | Key::ArrowRight if m.command && m.alt => TimelineCommand::SlipNudge { right: key == Key::ArrowRight, many: m.shift },
        Key::Comma | Key::Period if m.alt && !m.command => TimelineCommand::SlideNudge { right: key == Key::Period, many: m.shift },
        Key::ArrowLeft | Key::ArrowRight if m.alt && !m.command => TimelineCommand::ClipNudge { right: key == Key::ArrowRight, many: m.shift },
        Key::K if m.command && !m.shift => TimelineCommand::RazorAtPlayhead,
        Key::R if m.command && !m.shift && !m.alt => TimelineCommand::SpeedDurationOpen,
        Key::R if m.is_none() => TimelineCommand::SetTool(TrimTool::RateStretch),
        Key::Backspace if m.alt => TimelineCommand::RippleDelete,
        Key::Delete if m.shift => TimelineCommand::RippleDelete,
        Key::Delete | Key::Backspace if m.is_none() => TimelineCommand::Lift,
        Key::S if m.is_none() => TimelineCommand::ToggleSnap,
        Key::M if m.command && m.shift => TimelineCommand::PreviousMarker,
        Key::M if m.shift && !m.command => TimelineCommand::NextMarker,
        Key::M if m.is_none() => TimelineCommand::AddMarker,
        Key::ArrowUp if m.is_none() => TimelineCommand::PreviousEdit,
        Key::ArrowDown if m.is_none() => TimelineCommand::NextEdit,
        Key::Equals | Key::Plus if !m.command => TimelineCommand::ZoomIn,
        Key::Minus if !m.command => TimelineCommand::ZoomOut,
        Key::Backslash if m.is_none() => TimelineCommand::ZoomToFit,
        _ => return None,
    })
}

/// Snap targets and marker spans of one sequence revision (rebuilt when the revision changes).
pub(super) struct TimelineCache {
    pub(super) revision: u64,
    pub(super) targets: Vec<SnapTarget>,
    pub(super) markers: Vec<(String, TickRange)>,
    pub(super) error: Option<&'static str>,
}

/// Timeline panel state (the sequence itself is never cached; see module docs).
pub struct TimelineState {
    pub(super) sequence_id: String,
    pub view: TimelineView,
    pub snapping: bool,
    pub(super) selected: Option<String>,
    pub(super) drag: Option<Drag>,
    /// Keys go to the panel (set by a click in the panel or by opening it).
    pub active: bool,
    pub(super) layout: Option<TimelineLayout>,
    pub(super) synced_revision: Option<u64>,
    pub(super) cache: Option<TimelineCache>,
    /// Trim mode of panel drags (lane SMB-TIMELINE-TRIM-1).
    pub tool: TrimTool,
    /// The one multi-frame step of every "many" nudge ([STU-VID-035a]).
    pub multi_frame_step: u32,
    pub(super) edit_point: Option<EditPoint>,
    pub(super) last_trim: Option<TrimOutcome>,
    pub(super) session: Option<TrimSession>,
    pub composite_preview: bool,
    /// The open Speed/Duration form (lane SMB-PR-RATE-STRETCH-1).
    pub(super) speed_form: Option<SpeedForm>,
    /// Waveforms, automation lanes and the bound source port (lane SMB-AU-MULTITRACK-1).
    #[cfg(feature = "audio-batch")]
    pub(super) audio: AudioLanes,
}

impl TimelineState {
    pub fn sequence_id(&self) -> &str {
        &self.sequence_id
    }
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }
    pub fn drag(&self) -> Option<&Drag> {
        self.drag.as_ref()
    }
    /// The geometry of the last drawn frame.
    pub fn layout(&self) -> Option<&TimelineLayout> {
        self.layout.as_ref()
    }
    /// The selected edit point ([STU-VID-026b]).
    pub fn edit_point(&self) -> Option<&EditPoint> {
        self.edit_point.as_ref()
    }
    /// What the last trim requested, achieved or refused.
    pub fn last_trim(&self) -> Option<&TrimOutcome> {
        self.last_trim.as_ref()
    }
    /// The open trim session.
    pub fn session(&self) -> Option<&TrimSession> {
        self.session.as_ref()
    }
    /// The open Speed/Duration form.
    pub fn speed_form(&self) -> Option<&SpeedForm> {
        self.speed_form.as_ref()
    }
    /// The open Speed/Duration form for editing its fields (what typing in the form does).
    pub fn speed_form_mut(&mut self) -> Option<&mut SpeedForm> {
        self.speed_form.as_mut()
    }
    /// Waveforms, automation lanes and the bound source port (lane SMB-AU-MULTITRACK-1).
    #[cfg(feature = "audio-batch")]
    pub fn audio(&self) -> &AudioLanes {
        &self.audio
    }
}

/// Fresh `SCLP` ids for clips the timeline splits: the document id's time fields, a fixed
/// version/variant block (`7e1e-9000`, distinct from the source monitor's) and the first unused
/// counters (never a clip id of the document).
fn mint_clip_ids(doc: &folio::StudioDocument, count: usize) -> Vec<String> {
    let uuid = doc.document_id.split_once('-').map_or("", |(_, u)| u);
    let hex = |s: Option<&str>, n: usize| s.filter(|v| v.len() == n && v.bytes().all(|b| b.is_ascii_hexdigit())).map_or_else(|| "0".repeat(n), str::to_owned);
    let (high, low) = (hex(uuid.get(0..8), 8), hex(uuid.get(9..13), 4));
    let used: std::collections::BTreeSet<&str> = doc.sequences.iter().flat_map(|s| s.tracks.iter()).flat_map(|t| t.clips.iter()).map(|c| c.clip_id.as_str()).collect();
    let mut out = Vec::with_capacity(count);
    let mut n: u64 = 1;
    while out.len() < count {
        let id = format!("SCLP-{high}-{low}-7e1e-9000-{n:012x}");
        if !used.contains(id.as_str()) {
            out.push(id);
        }
        n = n.wrapping_add(1);
    }
    out
}

/// A key (`prefix-<n>`) unused by `taken`.
fn mint_key(prefix: &str, taken: &std::collections::BTreeSet<&str>) -> String {
    let mut n: u64 = 1;
    loop {
        let id = format!("{prefix}-{n}");
        if !taken.contains(id.as_str()) {
            return id;
        }
        n = n.wrapping_add(1);
    }
}

fn all_marker_ids(doc: &folio::StudioDocument) -> std::collections::BTreeSet<&str> {
    doc.sequences
        .iter()
        .flat_map(|s| s.markers.iter().chain(s.tracks.iter().flat_map(|t| t.clips.iter()).flat_map(|c| c.markers.iter())))
        .map(|m| m.marker_id.as_str())
        .collect()
}

fn find_clip<'a>(sequence: &'a StudioSequence, clip_id: &str) -> Option<(&'a folio::StudioTrack, &'a StudioClip)> {
    sequence.tracks.iter().find_map(|t| t.clips.iter().find(|c| c.clip_id == clip_id).map(|c| (t, c)))
}

/// Source length port for a move: the source of the moved clip is at least as long as the window
/// it already plays (its `source_out`), so re-placing that same window is admitted when the
/// viewer's own extents do not know the source; everything else defers to them.
struct AtLeast<'a> {
    inner: &'a dyn SourceExtents,
    source: &'a ClipSource,
    end: Ticks,
}

impl SourceExtents for AtLeast<'_> {
    fn extent(&self, source: &ClipSource) -> Option<Ticks> {
        self.inner.extent(source).or_else(|| (source == self.source).then_some(self.end))
    }
}

fn sequence_in(snapshot: &folio::Snapshot, sequence_id: &str) -> Result<StudioSequence, String> {
    snapshot.document().sequences.iter().find(|s| s.sequence_id == sequence_id).cloned().ok_or_else(|| "unknown_sequence".to_owned())
}

/// A planned edit: the edited sequence and what the plan reported.
struct Planned {
    sequence: StudioSequence,
    notes: Vec<String>,
}

fn noted(p: &hsk_studio_pulse::EditProposal, notes: &mut Vec<String>) {
    if !p.dropped_transitions().is_empty() {
        notes.push(format!("dropped transitions {:?}", p.dropped_transitions()));
    }
    if !p.removed_markers().is_empty() {
        notes.push(format!("removed markers {:?}", p.removed_markers()));
    }
}

fn plan_move(snapshot: &folio::Snapshot, sequence_id: &str, ctx: &EditContext, clip_id: &str, start: Ticks, track_id: Option<&str>) -> Result<Planned, String> {
    let revision = snapshot.document().revision;
    let sequence = sequence_in(snapshot, sequence_id)?;
    let (track, clip) = find_clip(&sequence, clip_id).ok_or("unknown_clip")?;
    if clip.link_group.is_some() {
        return Err("linked_move_unsupported".into());
    }
    let dest = match track_id {
        Some(id) => sequence.tracks.iter().find(|t| t.track_id == id).ok_or("unknown_track")?,
        None => track,
    };
    if dest.kind != track.kind {
        return Err("track_kind_mismatch".into());
    }
    if start == clip.timeline_start && dest.track_id == track.track_id {
        return Err("no_change".into());
    }
    let lift = plan_timeline_edit(snapshot, sequence_id, revision, &TimelineEdit::LiftClip { clip_id: clip_id.to_owned() }, ctx).map_err(|e| e.code().to_owned())?;
    let fresh_clip_ids = mint_clip_ids(lift.snapshot().document(), 2).into_iter().filter(|id| id != clip_id).collect();
    let assembly = AssemblyEdit {
        mode: AssemblyMode::Overwrite,
        source: clip.source_ref.clone(),
        speed: clip.speed,
        points: AssemblyPoints { source_in: Some(clip.source_in), source_out: Some(clip.source_out), sequence_in: Some(start), sequence_out: None },
        fit: None,
        patches: vec![SourcePatch { track_id: dest.track_id.clone(), clip_id: clip_id.to_owned() }],
        link_group: None,
        fresh_clip_ids,
    };
    let extents = AtLeast { inner: ctx.extents, source: &clip.source_ref, end: clip.source_out };
    let ctx2 = EditContext { budget: ctx.budget, resolver: ctx.resolver, extents: &extents, cancel: ctx.cancel };
    let next = revision.checked_add(1).ok_or("overflow")?;
    let placed = plan_assembly_edit(lift.snapshot(), sequence_id, next, &assembly, &ctx2).map_err(|e| e.code().to_owned())?;
    let mut notes = Vec::new();
    noted(&lift, &mut notes);
    noted(&placed, &mut notes);
    let mut out = sequence_in(placed.snapshot(), sequence_id)?;
    // The clip's own record fields the assembly does not set travel with it.
    if let Some(c) = out.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.clip_id == clip_id) {
        c.markers.clone_from(&clip.markers);
        c.enabled = clip.enabled;
    }
    if !clip.markers.is_empty() {
        notes.retain(|n| !n.starts_with("removed markers"));
    }
    Ok(Planned { sequence: out, notes })
}

fn plan_razor(snapshot: &folio::Snapshot, sequence_id: &str, ctx: &EditContext, at: Ticks, selected: Option<&str>) -> Result<Planned, String> {
    let original = sequence_in(snapshot, sequence_id)?;
    let inside = |c: &StudioClip| c.timeline_start < at && c.timeline_end().is_some_and(|e| at < e);
    let any_targeted = original.tracks.iter().any(|t| t.targeted);
    let candidates: Vec<String> = match selected.and_then(|id| find_clip(&original, id)).filter(|(_, c)| inside(c)) {
        Some((_, c)) => vec![c.clip_id.clone()],
        None => original
            .tracks
            .iter()
            .filter(|t| !t.locked && (t.targeted || !any_targeted))
            .flat_map(|t| t.clips.iter())
            .filter(|c| inside(c))
            .map(|c| c.clip_id.clone())
            .collect(),
    };
    if candidates.is_empty() {
        return Err("nothing_under_playhead".into());
    }
    let mut last: Option<hsk_studio_pulse::EditProposal> = None;
    let mut notes = Vec::new();
    for clip_id in candidates {
        let next = {
            let current = last.as_ref().map_or(snapshot, |p| p.snapshot());
            let seq = sequence_in(current, sequence_id)?;
            // A linked member may already be cut by an earlier razor of its group.
            let Some((_, clip)) = find_clip(&seq, &clip_id).filter(|(_, c)| inside(c)) else { continue };
            let doc = current.document();
            let linked = match &clip.link_group {
                Some(group) => {
                    let members: Vec<&str> = seq.tracks.iter().flat_map(|t| t.clips.iter()).filter(|c| c.link_group.as_deref() == Some(group) && c.clip_id != clip_id).map(|c| c.clip_id.as_str()).collect();
                    let ids = mint_clip_ids(doc, members.len().saturating_add(1));
                    let groups: std::collections::BTreeSet<&str> = doc.sequences.iter().flat_map(|s| s.tracks.iter()).flat_map(|t| t.clips.iter()).filter_map(|c| c.link_group.as_deref()).collect();
                    Some((ids[0].clone(), LinkedRazor { right_link_group: mint_key("link-tl", &groups), right_clip_ids: members.iter().zip(ids.iter().skip(1)).map(|(m, i)| ((*m).to_owned(), i.clone())).collect() }))
                }
                None => None,
            };
            let (new_right, linked) = match linked {
                Some((id, l)) => (id, Some(l)),
                None => (mint_clip_ids(doc, 1).remove(0), None),
            };
            let edit = TimelineEdit::Razor { clip_id: clip_id.clone(), at, new_right_clip_id: new_right, linked };
            plan_timeline_edit(current, sequence_id, doc.revision, &edit, ctx).map_err(|e| e.code().to_owned())?
        };
        noted(&next, &mut notes);
        last = Some(next);
    }
    let last = last.ok_or("nothing_under_playhead")?;
    Ok(Planned { sequence: sequence_in(last.snapshot(), sequence_id)?, notes })
}

fn plan_one(snapshot: &folio::Snapshot, sequence_id: &str, ctx: &EditContext, edit: &TimelineEdit) -> Result<Planned, String> {
    let p = plan_timeline_edit(snapshot, sequence_id, snapshot.document().revision, edit, ctx).map_err(|e| e.code().to_owned())?;
    let mut notes = Vec::new();
    noted(&p, &mut notes);
    Ok(Planned { sequence: sequence_in(p.snapshot(), sequence_id)?, notes })
}

impl Viewer {
    /// Opens the timeline panel on the program monitor's sequence, or on the document's first
    /// sequence (opening the program monitor on it: one clock, module docs).
    pub fn open_timeline(&mut self) -> Result<(), String> {
        let doc = self.run.snapshot().map(|s| s.document()).ok_or("nothing is open")?;
        let on_monitor = self.monitor.as_ref().map(|m| m.monitor.sequence().sequence_id.clone()).filter(|id| doc.sequences.iter().any(|s| &s.sequence_id == id));
        let sequence_id = match on_monitor {
            Some(id) => id,
            None => {
                let id = doc.sequences.first().map(|s| s.sequence_id.clone()).ok_or("the open document has no sequence")?;
                self.open_monitor(&id, super::shell::MONITOR_WIDTH, super::shell::MONITOR_HEIGHT).map_err(|e| format!("program monitor: {} {}", e.code, e.detail))?;
                id
            }
        };
        let revision = self.run.snapshot().map(|s| s.document().revision);
        self.timeline = Some(TimelineState {
            sequence_id: sequence_id.clone(),
            view: TimelineView::default(),
            snapping: true,
            selected: None,
            drag: None,
            active: true,
            layout: None,
            synced_revision: revision,
            cache: None,
            tool: TrimTool::Selection,
            multi_frame_step: DEFAULT_MULTI_FRAME_STEP,
            edit_point: None,
            last_trim: None,
            session: None,
            composite_preview: false,
            speed_form: None,
            #[cfg(feature = "audio-batch")]
            audio: AudioLanes::default(),
        });
        self.say(format!("timeline: sequence {sequence_id} (playhead = program monitor)"));
        Ok(())
    }

    /// The open timeline panel.
    pub fn timeline(&self) -> Option<&TimelineState> {
        self.timeline.as_ref()
    }
    pub fn timeline_mut(&mut self) -> Option<&mut TimelineState> {
        self.timeline.as_mut()
    }

    /// The canonical sequence the timeline shows (from the run's snapshot).
    pub fn timeline_sequence(&self) -> Option<&StudioSequence> {
        let id = &self.timeline.as_ref()?.sequence_id;
        self.run.snapshot()?.document().sequences.iter().find(|s| &s.sequence_id == id)
    }

    /// The playhead: the program monitor scheduler's position (its master clock while playing).
    pub fn timeline_playhead(&self) -> Option<Ticks> {
        let m = self.monitor.as_ref()?;
        let s = m.monitor.scheduler();
        if s.transport() == super::monitor::Transport::Playing {
            s.master(&m.clock).ok().or(Some(s.playhead()))
        } else {
            Some(s.playhead())
        }
    }

    /// Replaces the program monitor's sequence when the document's changed (an edit, undo or
    /// redo); one comparison per document revision.
    pub(super) fn timeline_sync_monitor(&mut self) {
        let revision = self.run.snapshot().map(|s| s.document().revision);
        let Some(t) = self.timeline.as_mut() else { return };
        if t.synced_revision == revision {
            return;
        }
        t.synced_revision = revision;
        let id = t.sequence_id.clone();
        let Some(seq) = self.run.snapshot().and_then(|s| s.document().sequences.iter().find(|q| q.sequence_id == id)).cloned() else { return };
        if let Some(m) = self.monitor.as_mut()
            && m.monitor.sequence().sequence_id == id
            && *m.monitor.sequence() != seq
            && let Err(e) = m.monitor.replace_sequence(seq, &m.clock)
        {
            self.say(format!("timeline: program monitor kept the previous sequence ({} {})", e.code, e.detail));
        }
    }

    /// Rebuilds the snap targets and marker spans when the document revision changed.
    pub(super) fn timeline_refresh_cache(&mut self) {
        let revision = self.run.snapshot().map_or(0, |s| s.document().revision);
        let cancel = self.cancel;
        let Some(t) = self.timeline.as_ref() else { return };
        if t.cache.as_ref().is_some_and(|c| c.revision == revision) {
            return;
        }
        let id = t.sequence_id.clone();
        let cache = match self.run.snapshot().and_then(|s| s.document().sequences.iter().find(|q| q.sequence_id == id)) {
            Some(seq) => {
                let targets = snap_targets(seq, None, &[], cancel);
                let markers = marker_spans(seq, cancel);
                let error = targets.as_ref().err().or(markers.as_ref().err()).map(|e| e.code());
                TimelineCache { revision, targets: targets.unwrap_or_default(), markers: markers.unwrap_or_default(), error }
            }
            None => TimelineCache { revision, targets: Vec::new(), markers: Vec::new(), error: Some("unknown_sequence") },
        };
        if let Some(t) = self.timeline.as_mut() {
            t.cache = Some(cache);
        }
    }

    /// Snap targets for a drag of `exclude` (its link group too) plus the playhead.
    pub(super) fn timeline_drag_targets(&self, exclude: &str) -> Vec<SnapTarget> {
        let Some(seq) = self.timeline_sequence() else { return Vec::new() };
        let group = find_clip(seq, exclude).and_then(|(_, c)| c.link_group.clone());
        let members: Vec<&str> = seq
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter(|c| c.clip_id == exclude || (group.is_some() && c.link_group == group))
            .map(|c| c.clip_id.as_str())
            .collect();
        snap_targets(seq, self.timeline_playhead(), &members, self.cancel).unwrap_or_default()
    }

    /// Runs one timeline action (a button, key, drag or ruler click does exactly this). Refusals
    /// are typed codes, also written to the status log.
    pub fn timeline_command(&mut self, command: TimelineCommand) -> Result<(), String> {
        let result = self.timeline_command_inner(&command);
        if let Err(e) = &result {
            self.say(format!("timeline {}: {e}", command.key()));
        }
        result
    }

    fn timeline_command_inner(&mut self, command: &TimelineCommand) -> Result<(), String> {
        // An undo or redo since the last frame changed the sequence: the clock follows it first.
        self.timeline_sync_monitor();
        if self.timeline.is_none() {
            return Err("no_timeline".into());
        }
        if let Some(result) = self.timeline_trim_command(command) {
            return result;
        }
        if let Some(result) = self.timeline_rate_command(command) {
            return result;
        }
        let t = self.timeline.as_mut().ok_or("no_timeline")?;
        match command {
            TimelineCommand::Select(id) => {
                t.selected.clone_from(id);
                return Ok(());
            }
            TimelineCommand::ToggleSnap => {
                t.snapping = !t.snapping;
                let on = t.snapping;
                self.say(format!("timeline: snapping {}", if on { "on" } else { "off" }));
                return Ok(());
            }
            TimelineCommand::ZoomIn | TimelineCommand::ZoomOut => {
                let factor = if *command == TimelineCommand::ZoomIn { 2.0 } else { 0.5 };
                let width = t.layout.as_ref().map_or(800.0, |l| f64::from(l.area.width() - HEADER_PX).max(1.0));
                let center = t.view.scroll_frames + width / 2.0 / t.view.px_per_frame;
                t.view.px_per_frame = (t.view.px_per_frame * factor).clamp(MIN_PX_PER_FRAME, MAX_PX_PER_FRAME);
                t.view.scroll_frames = (center - width / 2.0 / t.view.px_per_frame).max(0.0);
                return Ok(());
            }
            TimelineCommand::ScrollTo(frame) => {
                t.view.scroll_frames = *frame as f64;
                return Ok(());
            }
            _ => {}
        }
        let tpf = self.timeline_sequence().map(ticks_per_frame).ok_or("unknown_sequence")?;
        let playhead = self.timeline_playhead();
        match command {
            TimelineCommand::ZoomToFit => {
                let seq_frames = self.timeline_sequence().map_or(0.0, |s| frames_of(s.duration, tpf)).max(1.0);
                let t = self.timeline.as_mut().ok_or("no_timeline")?;
                let width = t.layout.as_ref().map_or(800.0, |l| f64::from(l.area.width() - HEADER_PX).max(1.0));
                t.view.px_per_frame = (width / seq_frames).clamp(MIN_PX_PER_FRAME, MAX_PX_PER_FRAME);
                t.view.scroll_frames = 0.0;
                Ok(())
            }
            TimelineCommand::Scrub(tick) => self.timeline_scrub(*tick),
            TimelineCommand::NextMarker | TimelineCommand::PreviousMarker => {
                let here = playhead.ok_or("no_program_monitor")?;
                let seq = self.timeline_sequence().ok_or("unknown_sequence")?;
                let found = if *command == TimelineCommand::NextMarker { next_marker(seq, here, self.cancel) } else { prev_marker(seq, here, self.cancel) };
                let to = found.map_err(|e| e.code().to_owned())?.ok_or(if *command == TimelineCommand::NextMarker { "no_next_marker" } else { "no_previous_marker" })?;
                self.timeline_scrub(to)
            }
            TimelineCommand::NextEdit | TimelineCommand::PreviousEdit => {
                let here = playhead.ok_or("no_program_monitor")?;
                let seq = self.timeline_sequence().ok_or("unknown_sequence")?;
                let targets = snap_targets(seq, None, &[], self.cancel).map_err(|e| e.code().to_owned())?;
                let forward = *command == TimelineCommand::NextEdit;
                let to = adjacent_edit(&targets, here, forward).ok_or(if forward { "no_next_edit" } else { "no_previous_edit" })?;
                self.timeline_scrub(to)
            }
            TimelineCommand::AddMarker => {
                let here = playhead.ok_or("no_program_monitor")?;
                let frame_start = Ticks::new(here.value().checked_div(tpf).unwrap_or(0).saturating_mul(tpf));
                self.timeline_commit("timeline_marker_add", |snap, seq_id, ctx| {
                    let doc = snap.document();
                    let taken = all_marker_ids(doc);
                    let n = doc.sequences.iter().find(|s| s.sequence_id == seq_id).map_or(0, |s| s.markers.len()).saturating_add(1);
                    let marker = StudioMarker { marker_id: mint_key("mk-tl", &taken), name: format!("Marker {n}"), comment: String::new(), color: LabelColor::Green, start: frame_start, duration: Ticks::new(0), role: None };
                    let p = plan_record_edit(snap, seq_id, doc.revision, &RecordEdit::AddMarker { scope: MarkerScope::Sequence, marker }, ctx).map_err(|e| e.code().to_owned())?;
                    Ok(Planned { sequence: sequence_in(p.snapshot(), seq_id)?, notes: Vec::new() })
                })
            }
            TimelineCommand::RazorAtPlayhead => {
                let here = playhead.ok_or("no_program_monitor")?;
                let selected = self.timeline.as_ref().and_then(|t| t.selected.clone());
                self.timeline_commit("timeline_razor", |snap, seq_id, ctx| plan_razor(snap, seq_id, ctx, here, selected.as_deref()))
            }
            TimelineCommand::Lift | TimelineCommand::RippleDelete => {
                let clip_id = self.timeline.as_ref().and_then(|t| t.selected.clone()).ok_or("no_selection")?;
                let (op, edit) = if *command == TimelineCommand::Lift {
                    ("timeline_lift", TimelineEdit::LiftClip { clip_id })
                } else {
                    ("timeline_ripple_delete", TimelineEdit::ExtractClip { clip_id })
                };
                self.timeline_commit(op, |snap, seq_id, ctx| plan_one(snap, seq_id, ctx, &edit))?;
                if let Some(t) = self.timeline.as_mut() {
                    t.selected = None;
                }
                Ok(())
            }
            TimelineCommand::TrimIn { clip_id, head } => {
                let edit = TimelineEdit::TrimIn { clip_id: clip_id.clone(), new_head: *head, ripple: false };
                self.timeline_commit("timeline_trim_in", |snap, seq_id, ctx| plan_one(snap, seq_id, ctx, &edit))
            }
            TimelineCommand::TrimOut { clip_id, tail } => {
                let start = self.timeline_sequence().and_then(|s| find_clip(s, clip_id)).map(|(_, c)| c.timeline_start).ok_or("unknown_clip")?;
                let duration = tail.checked_sub(start).ok().filter(|d| d.value() > 0).ok_or("out_before_in")?;
                let edit = TimelineEdit::TrimOut { clip_id: clip_id.clone(), new_duration: duration, ripple: false };
                self.timeline_commit("timeline_trim_out", |snap, seq_id, ctx| plan_one(snap, seq_id, ctx, &edit))
            }
            TimelineCommand::MoveClip { clip_id, start, track_id } => self.timeline_commit("timeline_move", |snap, seq_id, ctx| plan_move(snap, seq_id, ctx, clip_id, *start, track_id.as_deref())),
            // Select / snap / zoom / scroll returned above; the trim family runs in module `trim`.
            _ => Ok(()),
        }
    }

    fn timeline_scrub(&mut self, tick: Ticks) -> Result<(), String> {
        let m = self.monitor.as_mut().ok_or("no_program_monitor")?;
        m.monitor.scrub(tick, &m.clock).map_err(|e| format!("{} {}", e.code, e.detail))
    }

    /// Plans an edit on the current snapshot and commits the edited sequence as ONE undoable step
    /// (`Run::submit_sequence`); the program monitor then plays the edited sequence.
    fn timeline_commit(&mut self, op: &str, plan: impl FnOnce(&folio::Snapshot, &str, &EditContext) -> Result<Planned, String>) -> Result<(), String> {
        let sequence_id = self.timeline.as_ref().map(|t| t.sequence_id.clone()).ok_or("no_timeline")?;
        let mut planned = self.timeline_plan(plan)?;
        // Clip envelopes follow the edit in the same history step (module `envelopes`).
        let entry = self.envelopes_for(&planned.sequence, &mut planned.notes)?;
        let submitted = match entry {
            Some(bytes) => self.run.submit_sequence_and_entry(op, planned.sequence.clone(), envelopes::ROUTING_ENTRY, bytes),
            None => self.run.submit_sequence(op, planned.sequence.clone()),
        };
        if !submitted {
            let code = self.run.ops().last().and_then(|o| o.get("error").and_then(Value::as_str)).unwrap_or("submit_failed").to_owned();
            return Err(format!("submit_failed {code}"));
        }
        let revision = self.run.snapshot().map(|s| s.document().revision);
        if let Some(m) = self.monitor.as_mut()
            && m.monitor.sequence().sequence_id == sequence_id
            && let Err(e) = m.monitor.replace_sequence(planned.sequence, &m.clock)
        {
            self.say(format!("timeline: program monitor kept the previous sequence ({} {})", e.code, e.detail));
        }
        if let Some(t) = self.timeline.as_mut() {
            t.synced_revision = revision;
        }
        // The mixer plays the edited sequence (lane SMB-AU-MULTITRACK-1).
        #[cfg(feature = "audio-batch")]
        self.timeline_audio_follow();
        let notes = if planned.notes.is_empty() { String::new() } else { format!("; {}", planned.notes.join("; ")) };
        self.say(format!("timeline: {op} (revision {}){notes}", revision.unwrap_or(0)));
        Ok(())
    }

    /// Plans on the current snapshot with the program monitor's source extents (unknown without a
    /// monitor); commits nothing.
    fn timeline_plan<T>(&self, plan: impl FnOnce(&folio::Snapshot, &str, &EditContext) -> Result<T, String>) -> Result<T, String> {
        let sequence_id = self.timeline.as_ref().map(|t| t.sequence_id.clone()).ok_or("no_timeline")?;
        let snapshot = self.run.snapshot().ok_or("not_open")?;
        let unknown = UnknownExtents;
        let extents: &dyn SourceExtents = match self.monitor.as_ref() {
            Some(m) => m.extents.as_ref(),
            None => &unknown,
        };
        let ctx = EditContext { budget: folio::Budget::default(), resolver: &crate::harness::TrustAll, extents, cancel: self.cancel };
        plan(snapshot, &sequence_id, &ctx)
    }

    /// `state_json()["timeline"]`: the panel projection (`null` when closed).
    pub(super) fn timeline_json(&self) -> Value {
        let Some(t) = self.timeline.as_ref() else { return Value::Null };
        let seq = self.timeline_sequence();
        let playhead = self.timeline_playhead();
        let tpf = seq.map_or(1, ticks_per_frame);
        let timecode = match (playhead, seq.and_then(|s| s.frame_rate.accord())) {
            (Some(p), Some(r)) => format_ticks(p, r, TimeDisplay::matching(r), None).ok(),
            _ => None,
        };
        let l = t.layout.as_ref();
        #[allow(unused_mut)]
        let mut out = json!({
            "sequence_id": t.sequence_id,
            "sequence_found": seq.is_some(),
            "tracks": seq.map_or(0, |s| s.tracks.len()),
            "clips": seq.map_or(0, |s| s.tracks.iter().map(|t| t.clips.len()).sum::<usize>()),
            "markers": t.cache.as_ref().map_or(0, |c| c.markers.len()),
            "work_area": seq.and_then(|s| s.work_area.as_ref()).map(|a| json!([a.start.value(), a.end.value()])),
            "playhead_ticks": playhead.map(Ticks::value),
            "playhead_frame": playhead.map(|p| p.value().checked_div(tpf).unwrap_or(0)),
            "playhead_timecode": timecode,
            "snapping": t.snapping,
            "selected": t.selected,
            "active": t.active,
            "px_per_frame": t.view.px_per_frame,
            "scroll_frames": t.view.scroll_frames,
            "drag": t.drag.as_ref().map(|d| json!({"clip_id": d.clip_id, "kind": d.kind.key(), "preview": d.preview.as_ref().map(|p| json!({"start": p.start.value(), "end": p.end.value(), "track_id": p.track_id, "snapped": p.snapped.as_ref().map(|s| s.kind.key())}))})),
            "drawn": l.map(|l| json!({"tracks": l.tracks.len(), "tracks_hidden": l.tracks_hidden, "clips": l.clips.len(), "clips_truncated": l.clips_truncated, "unplaceable": l.unplaceable, "markers": l.markers.len(), "lanes": l.lanes.len()})),
            "cache_error": t.cache.as_ref().and_then(|c| c.error),
            "tool": t.tool.key(),
            "multi_frame_step": t.multi_frame_step,
            "edit_point": t.edit_point.as_ref().map(|e| json!({"clip_id": e.clip_id, "mode": e.mode.key()})),
            "last_trim": t.last_trim.as_ref().map(TrimOutcome::json),
            "trim_session": t.session.as_ref().map(TrimSession::json),
            "composite_preview": t.composite_preview,
            "speed_form": t.speed_form.as_ref().map(SpeedForm::json),
        });
        #[cfg(feature = "audio-batch")]
        {
            out["audio"] = self.timeline_audio_json();
        }
        out
    }
}
