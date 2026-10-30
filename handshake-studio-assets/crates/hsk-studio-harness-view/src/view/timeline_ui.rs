//! egui drawing and input of the timeline panel (lane SMB-TIMELINE-UI-1; model and commands in
//! [`super::timeline`]). One bottom panel: a toolbar (snapping, add edit, lift, ripple delete,
//! markers, zoom, scroll), the ruler with timecode labels and markers, the work-area bar, the track
//! headers and the clip blocks. Clicks select, the block body drags to move, the block ends drag to
//! trim, the ruler clicks and drags to scrub; every finished gesture becomes one
//! [`TimelineCommand`] run after the frame is drawn (one undoable step per drag, never one per
//! frame dragged, [STU-VID-040]). Clip blocks and trim handles are AccessKit buttons labelled with
//! the clip name, so the panel is navigable without pixels. Lane SMB-PR-RATE-STRETCH-1 adds the
//! Rate Stretch tool (edge drags) and the Speed/Duration form row (`Ctrl+R`). Lane
//! SMB-AU-MULTITRACK-1 (feature `audio-batch`) adds waveforms in audio clips, the automation lane
//! rows and the clip gain buttons (module `timeline::audio_ui`). Lane SMB-PR-NEST-1 adds the Nest,
//! Make Subsequence, Open Nested and Back to Parent buttons, the nesting path label, and a
//! double-click on a nested sequence clip opens it (module `timeline::nest`).
#![deny(clippy::arithmetic_side_effects)]

use super::Viewer;
use super::timeline::{
    Drag, DragKind, EDGE_PX, EditMode, HEADER_PX, MAX_PX_PER_FRAME, MIN_PX_PER_FRAME, RULER_PX, SpeedFix, TimelineCommand, TimelineLayout, TimelineState, TrimFocus, TrimTool, WORK_AREA_PX,
    drag_command, drag_target, frames_of, key_command, layout_full, tick_at_x, ticks_per_frame, widget, x_of,
};
use super::widget as vw;
use hsk_studio_accord::Ticks;
use hsk_studio_folio::{StudioSequence, TrackKind};
use hsk_studio_pulse::{SnapTarget, TickRange, nearest_snap};

const VIDEO: egui::Color32 = egui::Color32::from_rgb(70, 110, 170);
const AUDIO: egui::Color32 = egui::Color32::from_rgb(70, 140, 95);
const OTHER: egui::Color32 = egui::Color32::from_rgb(120, 110, 150);
const PLAYHEAD: egui::Color32 = egui::Color32::from_rgb(230, 60, 60);
const MARKER: egui::Color32 = egui::Color32::from_rgb(90, 200, 90);
const WORK_AREA: egui::Color32 = egui::Color32::from_rgb(160, 160, 170);
const SNAP_LINE: egui::Color32 = egui::Color32::from_rgb(240, 220, 80);

fn colour(kind: TrackKind) -> egui::Color32 {
    match kind {
        TrackKind::Video => VIDEO,
        TrackKind::Audio => AUDIO,
        _ => OTHER,
    }
}

/// Everything one frame of the panel decided (applied after drawing).
#[derive(Default)]
struct Outcome {
    commands: Vec<TimelineCommand>,
    drawn: Vec<String>,
    /// A clip drag started on (clip id, kind, pointer x at the press).
    drag_start: Option<(String, DragKind, f32)>,
    /// The dragged block reported its drag stopped.
    drag_stopped: bool,
    pressed_inside: bool,
    panel: Option<egui::Rect>,
    /// Multitrack actions (lane SMB-AU-MULTITRACK-1).
    #[cfg(feature = "audio-batch")]
    audio: Vec<super::timeline::AutomationCommand>,
}

/// What the panel draws besides the sequence: automation lanes (lane SMB-AU-MULTITRACK-1).
#[derive(Default)]
struct Extra {
    lane_tracks: Vec<String>,
    /// The master row's header text (lane SMB-AU-MULTITRACK-3); `None`: no master row.
    master: Option<String>,
    #[cfg(feature = "audio-batch")]
    views: Vec<super::timeline::LaneView>,
}

impl Viewer {
    /// The timeline panel (bottom; above the program monitor).
    pub(super) fn timeline_ui(&mut self, ctx: &egui::Context) {
        if self.timeline.is_none() {
            return;
        }
        self.timeline_sync_monitor();
        self.timeline_refresh_cache();
        #[allow(unused_mut)]
        let mut extra = Extra::default();
        #[cfg(feature = "audio-batch")]
        {
            self.timeline_audio_follow();
            let lanes: Vec<(String, super::timeline::LaneTarget)> = self.timeline.as_ref().map(|t| t.audio.lanes.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
            extra.views = lanes.iter().filter_map(|(track, target)| self.timeline_lane(track, target)).collect();
            extra.lane_tracks = lanes.into_iter().map(|(k, _)| k).collect();
            extra.master = self.timeline_master_row().map(|m| {
                let auto = if m["volume_automated"] == true { " AUTO" } else { "" };
                format!("{:.1} dB{auto}  fx {}", m["volume_db"].as_f64().unwrap_or(0.0), m["effects"].as_u64().unwrap_or(0))
            });
        }
        let playhead = self.timeline_playhead();
        let playing = self.monitor.as_ref().is_some_and(|m| m.monitor.scheduler().transport() == super::monitor::Transport::Playing);
        let mut out = Outcome::default();
        {
            let id = self.timeline.as_ref().map(|t| t.sequence_id().to_owned()).unwrap_or_default();
            let sequence = self.run.snapshot().and_then(|s| s.document().sequences.iter().find(|q| q.sequence_id == id));
            let Some(t) = self.timeline.as_mut() else { return };
            let response = egui::TopBottomPanel::bottom(vw::id(widget::PANEL)).resizable(true).default_height(260.0).min_height(140.0).show(ctx, |ui| panel(ui, sequence, t, playhead, &extra, &mut out));
            out.panel = Some(response.response.rect);
        }
        self.drawn.push(widget::PANEL.into());
        self.drawn.append(&mut out.drawn);
        // Keys go to the panel after a press inside it; a press elsewhere hands them back.
        let pressed = ctx.input(|i| i.pointer.any_pressed().then(|| i.pointer.interact_pos()).flatten());
        if let (Some(pos), Some(rect), Some(t)) = (pressed, out.panel, self.timeline.as_mut()) {
            t.active = rect.contains(pos) || out.pressed_inside;
        }
        // Drag bookkeeping: start, live preview with snapping, commit on release.
        if let Some((clip_id, kind, origin_x)) = out.drag_start.take() {
            self.timeline_begin_drag(&clip_id, kind, origin_x);
        }
        let pointer = ctx.input(|i| (i.pointer.interact_pos().or(i.pointer.latest_pos()), i.pointer.primary_down()));
        if self.timeline.as_ref().is_some_and(|t| t.drag.is_some()) {
            if let Some(pos) = pointer.0 {
                self.timeline_update_drag(pos);
            }
            if out.drag_stopped || !pointer.1 {
                self.timeline_finish_drag();
            }
        }
        let active = self.timeline.as_ref().is_some_and(|t| t.active);
        if active && !ctx.wants_keyboard_input() {
            let keys: Vec<TimelineCommand> = ctx.input_mut(|i| {
                let mut found = Vec::new();
                i.events.retain(|e| match e {
                    egui::Event::Key { key, pressed: true, modifiers, .. } => match key_command(*key, *modifiers) {
                        Some(c) => {
                            found.push(c);
                            false
                        }
                        None => true,
                    },
                    _ => true,
                });
                found
            });
            out.commands.extend(keys);
        }
        for c in out.commands {
            let _ = self.timeline_command(c);
        }
        #[cfg(feature = "audio-batch")]
        for c in std::mem::take(&mut out.audio) {
            let _ = self.multitrack_command(c);
        }
        if playing || self.timeline.as_ref().is_some_and(|t| t.drag.is_some()) {
            ctx.request_repaint();
        }
    }

    fn timeline_begin_drag(&mut self, clip_id: &str, kind: DragKind, origin_x: f32) {
        let Some((track_id, track_kind, start, end)) = self.timeline_sequence().and_then(|s| {
            s.tracks.iter().find_map(|t| t.clips.iter().find(|c| c.clip_id == clip_id).and_then(|c| Some((t.track_id.clone(), t.kind, c.timeline_start, c.timeline_end()?))))
        }) else {
            return;
        };
        if let Some(t) = self.timeline.as_mut() {
            t.selected = Some(clip_id.to_owned());
            t.drag = Some(Drag { clip_id: clip_id.to_owned(), kind, origin_x, start, end, track_id, track_kind, preview: None });
        }
    }

    /// Live drag preview at pointer `pos` (snapping when on; a move may change to a track of the
    /// same kind under the pointer).
    pub(super) fn timeline_update_drag(&mut self, pos: egui::Pos2) {
        let Some(t) = self.timeline.as_ref() else { return };
        let Some(drag) = t.drag.clone() else { return };
        let tpf = self.timeline_sequence().map_or(1, ticks_per_frame);
        let dest = match (drag.kind, t.layout.as_ref().and_then(|l| l.track_at(pos.y))) {
            (DragKind::Move, Some(row)) if row.kind == drag.track_kind && !row.locked => row.track_id.clone(),
            _ => drag.track_id.clone(),
        };
        let targets = t.snapping.then(|| self.timeline_drag_targets(&drag.clip_id));
        let view = t.view;
        let target = drag_target(&drag, pos.x - drag.origin_x, &dest, &view, tpf, targets.as_deref());
        if let Some(d) = self.timeline.as_mut().and_then(|t| t.drag.as_mut()) {
            d.preview = Some(target);
        }
    }

    /// Ends the drag: the preview becomes one command (nothing when unchanged).
    pub(super) fn timeline_finish_drag(&mut self) {
        let Some(drag) = self.timeline.as_mut().and_then(|t| t.drag.take()) else { return };
        if let Some(c) = drag.preview.as_ref().and_then(|p| drag_command(&drag, p)) {
            let _ = self.timeline_command(c);
        }
    }
}

fn button(ui: &mut egui::Ui, out: &mut Outcome, name: &str, label: &str, hover: &str, enabled: bool, command: TimelineCommand) {
    let r = ui.push_id(vw::id(name), |ui| ui.add_enabled(enabled, egui::Button::new(label))).inner.on_hover_text(hover);
    out.drawn.push(name.to_owned());
    if r.clicked() {
        out.commands.push(command);
    }
}

fn panel(ui: &mut egui::Ui, sequence: Option<&StudioSequence>, t: &mut TimelineState, playhead: Option<Ticks>, extra: &Extra, out: &mut Outcome) {
    let Some(seq) = sequence else {
        ui.label(format!("Timeline: sequence {} is not in the open document", t.sequence_id()));
        t.layout = None;
        return;
    };
    let tpf = ticks_per_frame(seq);
    let selected = t.selected().map(str::to_owned);
    // One toolbar row (scrolls sideways when narrow): a wrapping row lets nested widget scopes
    // grow the row and push the lanes out of the panel.
    egui::ScrollArea::horizontal().id_salt(vw::id("timeline.toolbar")).show(ui, |ui| ui.horizontal(|ui| {
        ui.strong(format!("Timeline: {}", seq.name.chars().take(48).collect::<String>()));
        let snap = ui.push_id(vw::id(widget::SNAP), |ui| ui.selectable_label(t.snapping, "Snap")).inner.on_hover_text("Snapping to edit points, markers, work area and playhead (S)");
        out.drawn.push(widget::SNAP.into());
        if snap.clicked() {
            out.commands.push(TimelineCommand::ToggleSnap);
        }
        button(ui, out, widget::RAZOR, "Add Edit", "Split at the playhead: the selected clip, else the targeted tracks (Ctrl+K)", playhead.is_some(), TimelineCommand::RazorAtPlayhead);
        button(ui, out, widget::LIFT, "Lift", "Remove the selected clip and leave a gap (Delete)", selected.is_some(), TimelineCommand::Lift);
        button(ui, out, widget::RIPPLE_DELETE, "Ripple Delete", "Remove the selected clip and close the gap (Alt+Backspace)", selected.is_some(), TimelineCommand::RippleDelete);
        ui.add_space(12.0);
        // Nesting (lane SMB-PR-NEST-1).
        let nested_selected = selected.as_deref().is_some_and(|id| super::timeline::is_nested(seq, id));
        button(ui, out, widget::NEST, "Nest", "Replace the selected clip and its linked clips with a nested sequence (one undo step)", selected.is_some(), TimelineCommand::Nest(Vec::new()));
        button(ui, out, widget::MAKE_SUBSEQUENCE, "Make Subsequence", "New sequence from the selected clip, else the in-to-out range; this sequence is unchanged (Shift+U)", true, TimelineCommand::MakeSubsequence(Vec::new()));
        button(ui, out, widget::OPEN_NESTED, "Open Nested", "Open the selected nested sequence clip in the timeline (Ctrl+Alt+F, or double-click it)", nested_selected, TimelineCommand::OpenNested(None));
        let depth = t.nest_back().count();
        button(ui, out, widget::BACK_TO_PARENT, "Back to Parent", "Return to the sequence Open Nested came from", depth > 0, TimelineCommand::BackToParent);
        if depth > 0 {
            let path: Vec<&str> = t.nest_back().chain(std::iter::once(t.sequence_id())).collect();
            ui.label(path.join(" > "));
            out.drawn.push(widget::NEST_PATH.into());
        }
        ui.add_space(12.0);
        button(ui, out, widget::ADD_MARKER, "Add Marker", "Sequence marker at the playhead (M)", playhead.is_some(), TimelineCommand::AddMarker);
        button(ui, out, widget::PREVIOUS_MARKER, "Previous Marker", "Ctrl+Shift+M", playhead.is_some(), TimelineCommand::PreviousMarker);
        button(ui, out, widget::NEXT_MARKER, "Next Marker", "Shift+M", playhead.is_some(), TimelineCommand::NextMarker);
        ui.add_space(12.0);
        button(ui, out, widget::ZOOM_OUT, "Zoom Out", "-", t.view.px_per_frame > MIN_PX_PER_FRAME, TimelineCommand::ZoomOut);
        button(ui, out, widget::ZOOM_IN, "Zoom In", "=", t.view.px_per_frame < MAX_PX_PER_FRAME, TimelineCommand::ZoomIn);
        button(ui, out, widget::ZOOM_FIT, "Zoom to Fit", "\\", true, TimelineCommand::ZoomToFit);
        ui.add_space(12.0);
        // Trim mode of edge and body drags (lane SMB-TIMELINE-TRIM-1).
        for tool in TrimTool::ALL {
            let name = widget::tool(tool.key());
            let r = ui.push_id(vw::id(&name), |ui| ui.selectable_label(t.tool == tool, tool.label())).inner.on_hover_text(format!("{} tool: edge and body drags", tool.label()));
            out.drawn.push(name);
            if r.clicked() {
                out.commands.push(TimelineCommand::SetTool(tool));
            }
        }
        button(ui, out, widget::EXTEND_EDIT, "Extend Edit", "Roll the selected (else the nearest) edit to the playhead", playhead.is_some(), TimelineCommand::ExtendEditToPlayhead);
        let can_enter = t.edit_point().is_some() && t.session().is_none();
        button(ui, out, widget::TRIM_SESSION_ENTER, "Trim Session", "Trim the selected edit point with plus / minus, focus and ripple / roll", can_enter, TimelineCommand::TrimSessionEnter);
        #[cfg(feature = "audio-batch")]
        {
            ui.add_space(12.0);
            super::timeline::audio_ui::toolbar(ui, seq, t, &mut out.drawn, &mut out.audio);
        }
        let max = frames_of(seq.duration, tpf).max(1.0);
        let mut scroll = t.view.scroll_frames.min(max);
        let r = ui.push_id(vw::id(widget::SCROLL), |ui| ui.add(egui::Slider::new(&mut scroll, 0.0..=max).text("Scroll").show_value(false))).inner;
        out.drawn.push(widget::SCROLL.into());
        if r.changed() {
            t.view.scroll_frames = scroll.max(0.0);
        }
        let here = playhead.zip(seq.frame_rate.accord()).and_then(|(p, r)| hsk_studio_pulse::format_ticks(p, r, hsk_studio_pulse::TimeDisplay::matching(r), None).ok());
        ui.label(format!("Playhead {}", here.as_deref().unwrap_or("-")));
        out.drawn.push(widget::PLAYHEAD.into());
    }));
    if t.session().is_some() {
        session_row(ui, seq, t, tpf, out);
    }
    if t.speed_form().is_some() {
        speed_form_row(ui, seq, t, out);
    }
    let size = egui::vec2(ui.available_width().max(HEADER_PX + 40.0), ui.available_height().max(RULER_PX + WORK_AREA_PX + 30.0));
    let (area, bg) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let markers: &[(String, TickRange)] = t.cache.as_ref().map_or(&[], |c| &c.markers);
    let l = layout_full(seq, &t.view, area, markers, &extra.lane_tracks, extra.master.is_some());
    #[cfg(feature = "audio-batch")]
    let waves = super::timeline::audio_ui::clip_waves(&mut t.audio, seq, &l);
    #[cfg(not(feature = "audio-batch"))]
    let waves: Vec<(String, Vec<(f32, f32)>)> = Vec::new();
    out.drawn.extend(waves.iter().map(|(id, _)| widget::clip_waveform(id)));
    let targets: &[SnapTarget] = t.cache.as_ref().map_or(&[], |c| &c.targets);
    paint(ui, &l, t, playhead, tpf, &waves);
    out.drawn.extend([widget::RULER, widget::LANES].map(String::from));
    if l.work_area.is_some() {
        out.drawn.push(widget::WORK_AREA.into());
    }
    out.drawn.extend(l.tracks.iter().map(|r| widget::track(&r.track_id)));
    out.drawn.extend(l.markers.iter().map(|m| widget::marker(&m.marker_id)));
    // Ruler: click or drag scrubs (snapping to targets when on).
    if bg.is_pointer_button_down_on() || bg.clicked() || bg.dragged() {
        out.pressed_inside = true;
    }
    if let Some(pos) = bg.interact_pointer_pos().filter(|_| bg.clicked() || bg.dragged())
        && pos.y < l.ruler.max.y
        && pos.x >= l.lanes_left
    {
        let mut tick = tick_at_x(pos.x, tpf, &t.view, l.lanes_left);
        if t.snapping {
            let distance = Ticks::new((f64::from(super::timeline::SNAP_PX) / t.view.px_per_frame * tpf as f64) as u64);
            if let Some(s) = nearest_snap(targets, tick, distance) {
                tick = s.at;
            }
        }
        out.commands.push(TimelineCommand::Scrub(tick));
    } else if bg.clicked() {
        out.commands.push(TimelineCommand::Select(None));
    }
    // Mouse wheel over the lanes: scroll; with Ctrl, zoom.
    if bg.hovered() {
        let (delta, zoom) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers.command));
        let d = f64::from(delta.x + delta.y);
        if d != 0.0 {
            if zoom {
                t.view.px_per_frame = (t.view.px_per_frame * (1.0 + d / 400.0).clamp(0.5, 2.0)).clamp(MIN_PX_PER_FRAME, MAX_PX_PER_FRAME);
            } else {
                t.view.scroll_frames = (t.view.scroll_frames - d / t.view.px_per_frame).max(0.0);
            }
        }
    }
    // Clip blocks: body (select, move) then the trim handles on top.
    let dragged = t.drag().map(|d| d.clip_id.clone());
    for b in &l.clips {
        let name = widget::clip(&b.clip_id);
        let label = format!("{} [{}..{})", b.label, b.start.value().checked_div(tpf).unwrap_or(0), b.end.value().checked_div(tpf).unwrap_or(0));
        let body = ui.interact(b.rect, vw::id(&name), egui::Sense::click_and_drag());
        body.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
        out.drawn.push(name);
        let third = (b.rect.width() / 3.0).min(EDGE_PX);
        // A clip cut off by the view edge has no trim handle on that side.
        let (head_visible, tail_visible) = (b.x0 >= l.lanes_left - 0.5, b.x1 <= l.area.max.x + 0.5);
        let mut edges = Vec::new();
        for head in [true, false] {
            if (head && !head_visible) || (!head && !tail_visible) {
                continue;
            }
            let rect = if head {
                egui::Rect::from_min_max(b.rect.min, egui::pos2(b.rect.min.x + third, b.rect.max.y))
            } else {
                egui::Rect::from_min_max(egui::pos2(b.rect.max.x - third, b.rect.min.y), b.rect.max)
            };
            let ename = widget::clip_edge(&b.clip_id, head);
            let r = ui.interact(rect, vw::id(&ename), egui::Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
            let what = if head { "trim in" } else { "trim out" };
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("{what} {label}")));
            out.drawn.push(ename);
            edges.push((head, r));
        }
        // The trim tool decides what an edge or body gesture does (lane SMB-TIMELINE-TRIM-1); a
        // roll on a head is the roll of the previous abutting clip's tail.
        let previous = previous_abutting(seq, &b.clip_id);
        let mut gesture: Option<(String, DragKind, Option<EditMode>, &egui::Response)> = None;
        for (head, r) in &edges {
            if r.drag_started() || r.dragged() || r.drag_stopped() || r.clicked() {
                let own = b.clip_id.clone();
                gesture = Some(match (t.tool, *head, previous.clone()) {
                    (TrimTool::RateStretch, true, _) => (own, DragKind::RateStretchIn, None, r),
                    (TrimTool::RateStretch, false, _) => (own, DragKind::RateStretchOut, None, r),
                    (TrimTool::Ripple, true, _) => (own, DragKind::RippleIn, Some(EditMode::RippleIn), r),
                    (TrimTool::Ripple, false, _) => (own, DragKind::RippleOut, Some(EditMode::RippleOut), r),
                    (TrimTool::Roll, true, Some(p)) => (p, DragKind::Roll, Some(EditMode::Roll), r),
                    (TrimTool::Roll, false, _) => (own, DragKind::Roll, Some(EditMode::Roll), r),
                    (_, true, _) => (own, DragKind::TrimIn, Some(EditMode::TrimIn), r),
                    (_, false, _) => (own, DragKind::TrimOut, Some(EditMode::TrimOut), r),
                });
            }
        }
        let body_kind = match t.tool {
            TrimTool::Slip => DragKind::Slip,
            TrimTool::Slide => DragKind::Slide,
            _ => DragKind::Move,
        };
        let (target_clip, kind, mode, r) = gesture.unwrap_or((b.clip_id.clone(), body_kind, None, &body));
        if r.clicked() || r.drag_started() || r.is_pointer_button_down_on() {
            out.pressed_inside = true;
        }
        if r.clicked() {
            out.commands.push(TimelineCommand::Select(Some(b.clip_id.clone())));
            if let Some(mode) = mode {
                out.commands.push(TimelineCommand::EditPointAt { clip_id: target_clip.clone(), mode });
            }
        }
        // A double-click on a nested sequence clip opens it (lane SMB-PR-NEST-1).
        if body.double_clicked() && super::timeline::is_nested(seq, &b.clip_id) {
            out.commands.push(TimelineCommand::OpenNested(Some(b.clip_id.clone())));
        }
        let ours = dragged.as_deref() == Some(target_clip.as_str());
        if r.drag_started() {
            let x = r.ctx.input(|i| i.pointer.press_origin()).map_or(0.0, |p| p.x);
            out.drag_start = Some((target_clip, kind, x));
        }
        if r.drag_stopped() && ours {
            out.drag_stopped = true;
        }
    }
    #[cfg(feature = "audio-batch")]
    super::timeline::audio_ui::master_ui(ui, &l, t, extra.master.as_deref(), &mut out.drawn, &mut out.audio, &mut out.pressed_inside);
    #[cfg(feature = "audio-batch")]
    super::timeline::audio_ui::lanes_ui(ui, &l, t, &extra.views, tpf, &mut out.drawn, &mut out.audio, &mut out.pressed_inside);
    #[cfg(not(feature = "audio-batch"))]
    let _ = extra;
    t.layout = Some(l);
}

/// The clip abutting `clip_id`'s head on its track.
fn previous_abutting(seq: &StudioSequence, clip_id: &str) -> Option<String> {
    seq.tracks.iter().find_map(|t| {
        let i = t.clips.iter().position(|c| c.clip_id == clip_id)?;
        let p = t.clips.get(i.checked_sub(1)?)?;
        (p.timeline_end() == Some(t.clips[i].timeline_start)).then(|| p.clip_id.clone())
    })
}

/// The Speed/Duration form row (`cmd.clip.speed`, module `timeline::rate`): speed percent and
/// duration in frames (the field edited last is the fixed one), reverse, the two STU-VID-031
/// settings that have no clip storage shown disabled with the reason, Apply and Cancel.
fn speed_form_row(ui: &mut egui::Ui, seq: &StudioSequence, t: &mut TimelineState, out: &mut Outcome) {
    let Some(form) = t.speed_form_mut() else { return };
    let name = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.clip_id == form.clip_id).map_or_else(|| "(missing clip)".to_owned(), super::timeline::clip_label);
    out.drawn.push(widget::SPEED_FORM.into());
    egui::ScrollArea::horizontal().id_salt(vw::id(widget::SPEED_FORM)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.strong(format!("Speed/Duration: {name}"));
            ui.label("Speed %");
            let r = ui.push_id(vw::id(widget::SPEED_PERCENT), |ui| ui.add(egui::TextEdit::singleline(&mut form.percent).desired_width(64.0))).inner;
            out.drawn.push(widget::SPEED_PERCENT.into());
            if r.changed() {
                form.fixed = SpeedFix::Speed;
            }
            ui.label("Duration (frames)");
            let r = ui.push_id(vw::id(widget::SPEED_DURATION), |ui| ui.add(egui::TextEdit::singleline(&mut form.duration_frames).desired_width(72.0))).inner;
            out.drawn.push(widget::SPEED_DURATION.into());
            if r.changed() {
                form.fixed = SpeedFix::Duration;
            }
            let _ = ui.push_id(vw::id(widget::SPEED_REVERSE), |ui| ui.checkbox(&mut form.reverse, "Reverse Speed"));
            out.drawn.push(widget::SPEED_REVERSE.into());
            let why = "No clip field stores this (STU-VID-031 spec gap)";
            let mut off = false;
            let _ = ui.push_id(vw::id(widget::SPEED_PITCH), |ui| ui.add_enabled(false, egui::Checkbox::new(&mut off, "Maintain Audio Pitch")).on_disabled_hover_text(why));
            let _ = ui.push_id(vw::id(widget::SPEED_INTERPOLATION), |ui| ui.add_enabled(false, egui::Button::new("Interpolation: unavailable")).on_disabled_hover_text(why));
            out.drawn.extend([widget::SPEED_PITCH, widget::SPEED_INTERPOLATION].map(String::from));
            ui.label(match form.fixed {
                SpeedFix::Speed => "(duration follows)",
                SpeedFix::Duration => "(speed follows)",
            });
            button(ui, out, widget::SPEED_APPLY, "Apply", "Set the speed or duration as one undo step (Ctrl+R opens this form)", true, TimelineCommand::SpeedDurationApply);
            button(ui, out, widget::SPEED_CANCEL, "Cancel", "Close without a change", true, TimelineCommand::SpeedDurationCancel);
        });
    });
}

/// The trim session row ([STU-VID-026a]): outgoing / incoming tiles labelled with their source
/// ticks, plus / minus minor and major, focus, ripple / roll, composite preview, apply, cancel.
fn session_row(ui: &mut egui::Ui, seq: &StudioSequence, t: &TimelineState, tpf: u64, out: &mut Outcome) {
    let Some(s) = t.session() else { return };
    let name_of = |id: Option<&str>| {
        id.and_then(|id| seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.clip_id == id)).map_or_else(|| "(none)".to_owned(), super::timeline::clip_label)
    };
    let tick = |v: Option<u64>| v.map_or_else(|| "-".to_owned(), |v| format!("source tick {v} (frame {})", v.checked_div(tpf).unwrap_or(0)));
    let step = t.multi_frame_step;
    out.drawn.push(widget::TRIM_SESSION.into());
    egui::ScrollArea::horizontal().id_salt(vw::id(widget::TRIM_SESSION)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.strong(format!("Trim ({}, {})", s.trim_type(), s.focus.key()));
            let tile = |ui: &mut egui::Ui, name: &str, text: String| {
                let _ = ui.push_id(vw::id(name), |ui| ui.group(|ui| ui.label(text)));
            };
            tile(ui, widget::TRIM_OUTGOING, format!("Outgoing {} out @ {}", name_of(s.outgoing.as_deref()), tick(s.outgoing_source_out)));
            tile(ui, widget::TRIM_INCOMING, format!("Incoming {} in @ {}", name_of(s.incoming.as_deref()), tick(s.incoming_source_in)));
            out.drawn.extend([widget::TRIM_OUTGOING, widget::TRIM_INCOMING].map(String::from));
            button(ui, out, widget::TRIM_MINUS_MAJOR, &format!("-{step}"), "Trim backward by the multi-frame step", true, TimelineCommand::TrimNudge { forward: false, many: true });
            button(ui, out, widget::TRIM_MINUS_MINOR, "-1", "Trim backward one frame", true, TimelineCommand::TrimNudge { forward: false, many: false });
            button(ui, out, widget::TRIM_PLUS_MINOR, "+1", "Trim forward one frame", true, TimelineCommand::TrimNudge { forward: true, many: false });
            button(ui, out, widget::TRIM_PLUS_MAJOR, &format!("+{step}"), "Trim forward by the multi-frame step", true, TimelineCommand::TrimNudge { forward: true, many: true });
            ui.label(format!("Pending {} frames{}", s.pending_frames, s.limit.as_deref().map(|l| format!(" ({l})")).unwrap_or_default()));
            for (name, label, focus) in [(widget::TRIM_FOCUS_IN, "In", TrimFocus::In), (widget::TRIM_FOCUS_OUT, "Out", TrimFocus::Out), (widget::TRIM_FOCUS_BOTH, "In+Out", TrimFocus::Both)] {
                let r = ui.push_id(vw::id(name), |ui| ui.selectable_label(s.focus == focus, label)).inner;
                out.drawn.push(name.into());
                if r.clicked() {
                    out.commands.push(TimelineCommand::TrimSessionFocus(focus));
                }
            }
            let other = if s.trim_type() == "roll" { "Ripple" } else { "Roll" };
            button(ui, out, widget::TRIM_TYPE, &format!("Make {other}"), "Toggle the trim type between ripple and roll", true, TimelineCommand::TrimSessionToggleType);
            let r = ui.push_id(vw::id(widget::TRIM_COMPOSITE), |ui| ui.selectable_label(t.composite_preview, "Composite Preview")).inner;
            out.drawn.push(widget::TRIM_COMPOSITE.into());
            if r.clicked() {
                out.commands.push(TimelineCommand::ToggleCompositePreview);
            }
            button(ui, out, widget::TRIM_COMMIT, "Apply", "Commit the adjustment as one undo step", true, TimelineCommand::TrimSessionCommit);
            button(ui, out, widget::TRIM_ROLLBACK, "Cancel", "Abandon the session (nothing committed)", true, TimelineCommand::TrimSessionRollback);
        });
    });
}

fn paint(ui: &egui::Ui, l: &TimelineLayout, t: &TimelineState, playhead: Option<Ticks>, tpf: u64, waves: &[(String, Vec<(f32, f32)>)]) {
    let painter = ui.painter_at(l.area);
    let visuals = ui.visuals();
    let text = visuals.text_color();
    let font = egui::FontId::proportional(11.0);
    painter.rect_filled(l.area, 0.0, visuals.extreme_bg_color);
    painter.rect_filled(l.ruler, 0.0, visuals.faint_bg_color);
    for (x, label) in &l.ruler_labels {
        painter.vline(*x, l.ruler.min.y + 12.0..=l.ruler.max.y, egui::Stroke::new(1.0_f32, text));
        painter.text(egui::pos2(*x + 2.0, l.ruler.min.y), egui::Align2::LEFT_TOP, label, font.clone(), text);
    }
    if let Some((x0, x1)) = l.work_area {
        let y = l.ruler.max.y;
        let r = egui::Rect::from_min_max(egui::pos2(x0.max(l.lanes_left), y + 1.0), egui::pos2(x1.min(l.area.max.x).max(x0.max(l.lanes_left)), y + WORK_AREA_PX - 1.0));
        painter.rect_filled(r, 1.0, WORK_AREA);
    }
    let lanes = painter.with_clip_rect(egui::Rect::from_min_max(egui::pos2(l.lanes_left, l.area.min.y), l.area.max));
    for row in &l.tracks {
        painter.rect_stroke(row.rect, 0.0, egui::Stroke::new(1.0_f32, visuals.faint_bg_color), egui::StrokeKind::Inside);
        let header = egui::Rect::from_min_max(row.rect.min, egui::pos2(l.lanes_left, row.rect.max.y));
        painter.rect_filled(header, 0.0, visuals.faint_bg_color);
        let name = if row.locked { format!("{} (locked)", row.name) } else { row.name.clone() };
        painter.text(egui::pos2(header.min.x + 4.0, header.center().y), egui::Align2::LEFT_CENTER, name, font.clone(), text);
    }
    let selected = t.selected();
    let drag = t.drag();
    for b in &l.clips {
        let base = colour(b.kind);
        let fill = if b.enabled { base } else { base.gamma_multiply(0.4) };
        lanes.rect_filled(b.rect, 3.0, fill);
        let stroke = if selected == Some(b.clip_id.as_str()) { egui::Stroke::new(2.0_f32, egui::Color32::WHITE) } else { egui::Stroke::new(1.0_f32, egui::Color32::BLACK) };
        lanes.rect_stroke(b.rect, 3.0, stroke, egui::StrokeKind::Inside);
        if let Some((_, cols)) = waves.iter().find(|(id, _)| *id == b.clip_id) {
            paint_wave(&lanes, b.rect.shrink(1.0), cols);
        }
        if b.rect.width() > 24.0 {
            lanes.with_clip_rect(b.rect.shrink(2.0)).text(egui::pos2(b.rect.min.x + 4.0, b.rect.center().y), egui::Align2::LEFT_CENTER, &b.label, font.clone(), egui::Color32::WHITE);
        }
    }
    // The selected edit point: a bright bar on its edge (lane SMB-TIMELINE-TRIM-1).
    if let Some(e) = t.edit_point()
        && let Some(b) = l.clip(&e.clip_id)
    {
        let x = if e.mode.is_head() { b.x0 } else { b.x1 };
        lanes.vline(x, b.rect.y_range(), egui::Stroke::new(3.0_f32, SNAP_LINE));
    }
    if let Some(p) = drag.and_then(|d| d.preview.as_ref()) {
        let row = l.tracks.iter().find(|r| r.track_id == p.track_id);
        if let Some(row) = row {
            let ghost = egui::Rect::from_min_max(egui::pos2(x_of(p.start, tpf, &t.view, l.lanes_left), row.rect.min.y + 2.0), egui::pos2(x_of(p.end, tpf, &t.view, l.lanes_left), row.rect.max.y - 2.0));
            lanes.rect_stroke(ghost, 3.0, egui::Stroke::new(2.0_f32, egui::Color32::YELLOW), egui::StrokeKind::Inside);
        }
        if let Some(s) = &p.snapped {
            let x = x_of(s.at, tpf, &t.view, l.lanes_left);
            lanes.vline(x, l.area.y_range(), egui::Stroke::new(1.0_f32, SNAP_LINE));
        }
    }
    for m in &l.markers {
        let y = l.ruler.max.y;
        if m.x_end > m.x {
            lanes.rect_filled(egui::Rect::from_min_max(egui::pos2(m.x, y - 4.0), egui::pos2(m.x_end, y)), 0.0, MARKER.gamma_multiply(0.6));
        }
        let tri = vec![egui::pos2(m.x - 4.0, y - 8.0), egui::pos2(m.x + 4.0, y - 8.0), egui::pos2(m.x, y)];
        lanes.add(egui::Shape::convex_polygon(tri, MARKER, egui::Stroke::NONE));
    }
    if let Some(p) = playhead {
        let x = x_of(p, tpf, &t.view, l.lanes_left);
        lanes.vline(x, l.area.y_range(), egui::Stroke::new(1.5_f32, PLAYHEAD));
    }
    let hidden = l.tracks_hidden;
    if hidden > 0 || l.clips_truncated || l.unplaceable > 0 {
        let note = format!(
            "{}{}{}",
            if hidden > 0 { format!("{hidden} more tracks  ") } else { String::new() },
            if l.clips_truncated { "more clips than drawn  " } else { "" },
            if l.unplaceable > 0 { format!("{} clips out of range", l.unplaceable) } else { String::new() }
        );
        painter.text(egui::pos2(l.area.max.x - 4.0, l.area.max.y - 2.0), egui::Align2::RIGHT_BOTTOM, note, font, text);
    }
}

/// Waveform columns of an audio clip block (lane SMB-AU-MULTITRACK-1; empty without `audio-batch`).
fn paint_wave(painter: &egui::Painter, rect: egui::Rect, cols: &[(f32, f32)]) {
    #[cfg(feature = "audio-batch")]
    super::timeline::audio_ui::paint_wave(painter, rect, cols);
    #[cfg(not(feature = "audio-batch"))]
    let _ = (painter, rect, cols);
}
