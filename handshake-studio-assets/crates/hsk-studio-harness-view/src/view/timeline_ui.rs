//! egui drawing and input of the timeline panel (lane SMB-TIMELINE-UI-1; model and commands in
//! [`super::timeline`]). One bottom panel: a toolbar (snapping, add edit, lift, ripple delete,
//! markers, zoom, scroll), the ruler with timecode labels and markers, the work-area bar, the track
//! headers and the clip blocks. Clicks select, the block body drags to move, the block ends drag to
//! trim, the ruler clicks and drags to scrub; every finished gesture becomes one
//! [`TimelineCommand`] run after the frame is drawn (one undoable step per drag, never one per
//! frame dragged, [STU-VID-040]). Clip blocks and trim handles are AccessKit buttons labelled with
//! the clip name, so the panel is navigable without pixels.
#![deny(clippy::arithmetic_side_effects)]

use super::Viewer;
use super::timeline::{
    Drag, DragKind, EDGE_PX, HEADER_PX, MAX_PX_PER_FRAME, MIN_PX_PER_FRAME, RULER_PX, TimelineCommand, TimelineLayout, TimelineState, WORK_AREA_PX, drag_command, drag_target, frames_of,
    key_command, layout, tick_at_x, ticks_per_frame, widget, x_of,
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
}

impl Viewer {
    /// The timeline panel (bottom; above the program monitor).
    pub(super) fn timeline_ui(&mut self, ctx: &egui::Context) {
        if self.timeline.is_none() {
            return;
        }
        self.timeline_sync_monitor();
        self.timeline_refresh_cache();
        let playhead = self.timeline_playhead();
        let playing = self.monitor.as_ref().is_some_and(|m| m.monitor.scheduler().transport() == super::monitor::Transport::Playing);
        let mut out = Outcome::default();
        {
            let id = self.timeline.as_ref().map(|t| t.sequence_id().to_owned()).unwrap_or_default();
            let sequence = self.run.snapshot().and_then(|s| s.document().sequences.iter().find(|q| q.sequence_id == id));
            let Some(t) = self.timeline.as_mut() else { return };
            let response = egui::TopBottomPanel::bottom(vw::id(widget::PANEL)).resizable(true).default_height(230.0).min_height(120.0).show(ctx, |ui| panel(ui, sequence, t, playhead, &mut out));
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

fn panel(ui: &mut egui::Ui, sequence: Option<&StudioSequence>, t: &mut TimelineState, playhead: Option<Ticks>, out: &mut Outcome) {
    let Some(seq) = sequence else {
        ui.label(format!("Timeline: sequence {} is not in the open document", t.sequence_id()));
        t.layout = None;
        return;
    };
    let tpf = ticks_per_frame(seq);
    let selected = t.selected().map(str::to_owned);
    ui.horizontal_wrapped(|ui| {
        ui.strong(format!("Timeline: {}", seq.name.chars().take(48).collect::<String>()));
        let snap = ui.push_id(vw::id(widget::SNAP), |ui| ui.selectable_label(t.snapping, "Snap")).inner.on_hover_text("Snapping to edit points, markers, work area and playhead (S)");
        out.drawn.push(widget::SNAP.into());
        if snap.clicked() {
            out.commands.push(TimelineCommand::ToggleSnap);
        }
        button(ui, out, widget::RAZOR, "Add Edit", "Split at the playhead: the selected clip, else the targeted tracks (Ctrl+K)", playhead.is_some(), TimelineCommand::RazorAtPlayhead);
        button(ui, out, widget::LIFT, "Lift", "Remove the selected clip and leave a gap (Delete)", selected.is_some(), TimelineCommand::Lift);
        button(ui, out, widget::RIPPLE_DELETE, "Ripple Delete", "Remove the selected clip and close the gap (Alt+Backspace)", selected.is_some(), TimelineCommand::RippleDelete);
        ui.separator();
        button(ui, out, widget::ADD_MARKER, "Add Marker", "Sequence marker at the playhead (M)", playhead.is_some(), TimelineCommand::AddMarker);
        button(ui, out, widget::PREVIOUS_MARKER, "Previous Marker", "Ctrl+Shift+M", playhead.is_some(), TimelineCommand::PreviousMarker);
        button(ui, out, widget::NEXT_MARKER, "Next Marker", "Shift+M", playhead.is_some(), TimelineCommand::NextMarker);
        ui.separator();
        button(ui, out, widget::ZOOM_OUT, "Zoom Out", "-", t.view.px_per_frame > MIN_PX_PER_FRAME, TimelineCommand::ZoomOut);
        button(ui, out, widget::ZOOM_IN, "Zoom In", "=", t.view.px_per_frame < MAX_PX_PER_FRAME, TimelineCommand::ZoomIn);
        button(ui, out, widget::ZOOM_FIT, "Zoom to Fit", "\\", true, TimelineCommand::ZoomToFit);
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
    });
    let size = egui::vec2(ui.available_width().max(HEADER_PX + 40.0), ui.available_height().max(RULER_PX + WORK_AREA_PX + 30.0));
    let (area, bg) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let markers: &[(String, TickRange)] = t.cache.as_ref().map_or(&[], |c| &c.markers);
    let l = layout(seq, &t.view, area, markers);
    let targets: &[SnapTarget] = t.cache.as_ref().map_or(&[], |c| &c.targets);
    paint(ui, &l, t, playhead, tpf);
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
        let mut gesture: Option<(DragKind, &egui::Response)> = None;
        for (head, r) in &edges {
            if r.drag_started() || r.dragged() || r.drag_stopped() || r.clicked() {
                gesture = Some((if *head { DragKind::TrimIn } else { DragKind::TrimOut }, r));
            }
        }
        let (kind, r) = gesture.unwrap_or((DragKind::Move, &body));
        if r.clicked() || r.drag_started() || r.is_pointer_button_down_on() {
            out.pressed_inside = true;
        }
        if r.clicked() {
            out.commands.push(TimelineCommand::Select(Some(b.clip_id.clone())));
        }
        if r.drag_started() {
            let x = r.ctx.input(|i| i.pointer.press_origin()).map_or(0.0, |p| p.x);
            out.drag_start = Some((b.clip_id.clone(), kind, x));
        }
        if r.drag_stopped() && dragged.as_deref() == Some(b.clip_id.as_str()) {
            out.drag_stopped = true;
        }
    }
    t.layout = Some(l);
}

fn paint(ui: &egui::Ui, l: &TimelineLayout, t: &TimelineState, playhead: Option<Ticks>, tpf: u64) {
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
        if b.rect.width() > 24.0 {
            lanes.with_clip_rect(b.rect.shrink(2.0)).text(egui::pos2(b.rect.min.x + 4.0, b.rect.center().y), egui::Align2::LEFT_CENTER, &b.label, font.clone(), egui::Color32::WHITE);
        }
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
