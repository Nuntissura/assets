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
//! stable name ([`widget`]), and `state_json()["timeline"]` projects the panel.
#![deny(clippy::arithmetic_side_effects)]

use super::Viewer;
use hsk_studio_accord::Ticks;
use hsk_studio_folio::{self as folio, ClipSource, LabelColor, StudioClip, StudioMarker, StudioSequence, TrackKind};
use hsk_studio_pulse::{
    AssemblyEdit, AssemblyMode, AssemblyPoints, EditContext, LinkedRazor, MarkerScope, RecordEdit, SnapTarget, SourceExtents, SourcePatch, TickRange, TimeDisplay, TimelineEdit,
    UnknownExtents, adjacent_edit, format_ticks, marker_spans, nearest_snap, next_marker, plan_assembly_edit, plan_record_edit, plan_timeline_edit, prev_marker, snap_targets,
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
}

/// Track header column width (pixels).
pub const HEADER_PX: f32 = 64.0;
/// Ruler band height (pixels).
pub const RULER_PX: f32 = 22.0;
/// Work-area bar height under the ruler (pixels).
pub const WORK_AREA_PX: f32 = 6.0;
/// Height of one track row (pixels).
pub const TRACK_PX: f32 = 30.0;
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
    text.chars().take(MAX_LABEL_CHARS).collect()
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
    let tpf = ticks_per_frame(sequence);
    let lanes_left = area.left() + HEADER_PX;
    let lanes_right = area.right().max(lanes_left);
    let ruler = egui::Rect::from_min_max(egui::pos2(lanes_left, area.top()), egui::pos2(lanes_right, area.top() + RULER_PX));
    let mut out = TimelineLayout {
        area,
        lanes_left,
        ruler,
        tracks: Vec::new(),
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
    }
    out
}

/// What a drag changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragKind {
    Move,
    TrimIn,
    TrimOut,
}

impl DragKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::TrimIn => "trim_in",
            Self::TrimOut => "trim_out",
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
        DragKind::Move => {
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
        DragKind::TrimIn => {
            let limit = drag.end.value().saturating_sub(tpf);
            start = Ticks::new(shifted(drag.start, delta).value().min(limit));
            if let Some(t) = snap(start).filter(|t| t.at.value() <= limit) {
                start = t.at;
                snapped = Some(t);
            }
        }
        DragKind::TrimOut => {
            let limit = drag.start.value().saturating_add(tpf);
            end = Ticks::new(shifted(drag.end, delta).value().max(limit));
            if let Some(t) = snap(end).filter(|t| t.at.value() >= limit) {
                end = t.at;
                snapped = Some(t);
            }
        }
    }
    DragTarget { start, end, track_id: dest_track.to_owned(), snapped }
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
            _ => return None,
        })
    }
}

/// Keyboard map of the timeline panel (active after a click in the panel or opening it):
/// Ctrl+K razor, Delete / Backspace lift, Alt+Backspace and Shift+Delete ripple delete, S snapping,
/// M add marker, Shift+M next marker, Ctrl+Shift+M previous marker, Up / Down previous / next edit,
/// `=` / `-` zoom, `\` zoom to fit. Bindings from the module 14 tables: Ctrl+K, Alt+Backspace, S,
/// Shift+M, Ctrl+Shift+M, Up, Down; the rest are harness bindings.
pub fn key_command(key: egui::Key, m: egui::Modifiers) -> Option<TimelineCommand> {
    use egui::Key;
    Some(match key {
        Key::K if m.command && !m.shift => TimelineCommand::RazorAtPlayhead,
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
            TimelineCommand::Select(_) | TimelineCommand::ToggleSnap | TimelineCommand::ZoomIn | TimelineCommand::ZoomOut | TimelineCommand::ScrollTo(_) => Ok(()),
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
        let snapshot = self.run.snapshot().ok_or("not_open")?;
        let unknown = UnknownExtents;
        let extents: &dyn SourceExtents = match self.monitor.as_ref() {
            Some(m) => m.extents.as_ref(),
            None => &unknown,
        };
        let ctx = EditContext { budget: folio::Budget::default(), resolver: &crate::harness::TrustAll, extents, cancel: self.cancel };
        let planned = plan(snapshot, &sequence_id, &ctx)?;
        if !self.run.submit_sequence(op, planned.sequence.clone()) {
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
        let notes = if planned.notes.is_empty() { String::new() } else { format!("; {}", planned.notes.join("; ")) };
        self.say(format!("timeline: {op} (revision {}){notes}", revision.unwrap_or(0)));
        Ok(())
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
        json!({
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
            "drawn": l.map(|l| json!({"tracks": l.tracks.len(), "tracks_hidden": l.tracks_hidden, "clips": l.clips.len(), "clips_truncated": l.clips_truncated, "unplaceable": l.unplaceable, "markers": l.markers.len()})),
            "cache_error": t.cache.as_ref().and_then(|c| c.error),
        })
    }
}
