//! egui drawing and input of the multitrack additions of the timeline panel (lane
//! SMB-AU-MULTITRACK-1; model, commands and manual in module `audio`): the Waveforms and Clip Gain
//! toolbar buttons, the waveform columns of audio clip blocks, the **A** lane button of audio and
//! submix track headers and the automation lane rows (envelope line, point handles; click adds,
//! drag moves, right click deletes). Every gesture becomes one [`AutomationCommand`] run after the
//! frame is drawn; handles and lane rows are AccessKit buttons with stable names.

use super::super::widget as vw;
use super::audio::{AudioLanes, AutomationCommand, CLIP_GAIN_STEP_DB, LaneView, MAX_DRAWN_POINTS, MAX_PEAK_COLUMNS_PER_FRAME, PointId, clip_peaks, value_at};
use super::{TimelineLayout, TimelineState, TimelineView, widget};
use hsk_studio_accord::Ticks;
use hsk_studio_folio::{StudioSequence, TrackKind};

const LANE_BG: egui::Color32 = egui::Color32::from_rgb(34, 38, 44);
const ENVELOPE: egui::Color32 = egui::Color32::from_rgb(240, 200, 60);
const STATIC: egui::Color32 = egui::Color32::from_rgb(150, 150, 160);
const POINT: egui::Color32 = egui::Color32::from_rgb(255, 230, 120);
const WAVE: egui::Color32 = egui::Color32::from_rgba_premultiplied(200, 235, 210, 170);
/// Lane padding above and below the value range (pixels).
pub const LANE_PAD: f32 = 4.0;
/// Point handle size (pixels).
pub const POINT_PX: f32 = 8.0;
/// Width of the lane button in a track header (pixels).
pub const LANE_BUTTON_PX: f32 = 14.0;

/// Screen y of `value` in lane row `rect` with value range `range`.
pub fn lane_y(rect: egui::Rect, range: (f64, f64), value: f64) -> f32 {
    let span = (range.1 - range.0).max(f64::MIN_POSITIVE);
    let k = ((value - range.0) / span).clamp(0.0, 1.0) as f32;
    rect.max.y - LANE_PAD - k * (rect.height() - 2.0 * LANE_PAD).max(1.0)
}

/// The value at screen y in lane row `rect` (clamped to `range`).
pub fn lane_value(rect: egui::Rect, range: (f64, f64), y: f32) -> f64 {
    let h = f64::from((rect.height() - 2.0 * LANE_PAD).max(1.0));
    let k = (f64::from(rect.max.y - LANE_PAD - y) / h).clamp(0.0, 1.0);
    range.0 + k * (range.1 - range.0)
}

/// The nearest whole frame to screen x, as ticks.
pub fn nearest_frame(x: f32, tpf: u64, view: &TimelineView, lanes_left: f32) -> Ticks {
    let frame = (view.scroll_frames + f64::from(x - lanes_left) / view.px_per_frame).round().max(0.0);
    Ticks::new((frame as u64).saturating_mul(tpf))
}

/// Toolbar: Waveforms on / off, Clip Gain -1 dB / +1 dB on the selected audio clip.
pub(in crate::view) fn toolbar(ui: &mut egui::Ui, seq: &StudioSequence, t: &TimelineState, drawn: &mut Vec<String>, cmds: &mut Vec<AutomationCommand>) {
    let r = ui.push_id(vw::id(widget::WAVEFORMS), |ui| ui.selectable_label(t.audio.waveforms, "Waveforms")).inner.on_hover_text("Waveform overviews in audio clips");
    drawn.push(widget::WAVEFORMS.into());
    if r.clicked() {
        cmds.push(AutomationCommand::ToggleWaveforms);
    }
    let audio_clip = t.selected().filter(|id| seq.tracks.iter().filter(|k| k.kind == TrackKind::Audio).any(|k| k.clips.iter().any(|c| c.clip_id == *id)));
    for (name, label, delta) in [(widget::CLIP_GAIN_DOWN, "Clip Gain -1 dB", -CLIP_GAIN_STEP_DB), (widget::CLIP_GAIN_UP, "Clip Gain +1 dB", CLIP_GAIN_STEP_DB)] {
        let r = ui.push_id(vw::id(name), |ui| ui.add_enabled(audio_clip.is_some(), egui::Button::new(label))).inner.on_hover_text("Adds the step to the selected audio clip's volume envelope (one undo step)");
        drawn.push(name.into());
        if r.clicked()
            && let Some(id) = audio_clip
        {
            cmds.push(AutomationCommand::ClipGain { clip_id: id.to_owned(), delta_db: delta });
        }
    }
}

/// Waveform columns of the visible audio clip blocks (one column per pixel, bounded per frame).
pub(in crate::view) fn clip_waves(audio: &mut AudioLanes, seq: &StudioSequence, l: &TimelineLayout) -> Vec<(String, Vec<(f32, f32)>)> {
    if !audio.waveforms || audio.rate().is_none() {
        return Vec::new();
    }
    let mut budget = MAX_PEAK_COLUMNS_PER_FRAME;
    let mut out = Vec::new();
    for b in l.clips.iter().filter(|b| b.kind == TrackKind::Audio) {
        let columns = (b.rect.width().floor() as usize).clamp(1, 4096);
        if columns > budget {
            break;
        }
        let Some(clip) = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.clip_id == b.clip_id) else { continue };
        let w = f64::from(b.x1 - b.x0);
        let (from, to) = if w > 0.0 { (f64::from(b.rect.min.x - b.x0) / w, f64::from(b.rect.max.x - b.x0) / w) } else { (0.0, 1.0) };
        if let Ok(cols) = clip_peaks(audio, clip, from, to, columns) {
            budget = budget.saturating_sub(columns);
            out.push((b.clip_id.clone(), cols));
        }
    }
    out
}

/// Paints one block's waveform columns (min / max around the block's middle line).
pub(in crate::view) fn paint_wave(painter: &egui::Painter, rect: egui::Rect, cols: &[(f32, f32)]) {
    let n = cols.len().max(1) as f32;
    let mid = rect.center().y;
    let half = (rect.height() / 2.0 - 2.0).max(1.0);
    for (i, (lo, hi)) in cols.iter().enumerate() {
        let x = rect.min.x + (i as f32 + 0.5) * rect.width() / n;
        let (a, b) = (mid - hi.clamp(-1.0, 1.0) * half, mid - lo.clamp(-1.0, 1.0) * half);
        painter.vline(x, a..=b.max(a + 0.5), egui::Stroke::new(1.0_f32, WAVE));
    }
}

/// The master row (lane SMB-AU-MULTITRACK-3): header "Master" with its **A** lane button and the
/// summary `text` (master volume, AUTO when a volume lane plays, effect count) in the row body.
pub(in crate::view) fn master_ui(ui: &mut egui::Ui, l: &TimelineLayout, t: &TimelineState, text: Option<&str>, drawn: &mut Vec<String>, cmds: &mut Vec<AutomationCommand>, pressed: &mut bool) {
    let (Some(row), Some(text)) = (l.master, text) else { return };
    let font = egui::FontId::proportional(11.0);
    let colour = ui.visuals().text_color();
    let header = egui::Rect::from_min_max(row.min, egui::pos2(l.lanes_left, row.max.y));
    let body = egui::Rect::from_min_max(egui::pos2(l.lanes_left, row.min.y), row.max);
    let r = ui.interact(body, vw::id(widget::MASTER), egui::Sense::hover());
    let summary = format!("Master {text}");
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &summary));
    drawn.push(widget::MASTER.into());
    let p = ui.painter_at(row);
    p.rect_filled(row, 0.0, LANE_BG);
    p.rect_filled(header, 0.0, ui.visuals().faint_bg_color);
    p.text(egui::pos2(header.min.x + 4.0, header.center().y), egui::Align2::LEFT_CENTER, "Master", font.clone(), colour);
    p.text(egui::pos2(body.min.x + 6.0, body.center().y), egui::Align2::LEFT_CENTER, text, font, if text.contains("AUTO") { ENVELOPE } else { colour });
    let name = widget::lane_toggle(super::MASTER_ROW);
    let rect = egui::Rect::from_min_size(egui::pos2(l.lanes_left - LANE_BUTTON_PX - 2.0, row.min.y + 2.0), egui::vec2(LANE_BUTTON_PX, LANE_BUTTON_PX));
    let b = ui.interact(rect, vw::id(&name), egui::Sense::click()).on_hover_text("Automation lane: click cycles master volume / master effect parameters / none");
    b.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "automation lane Master"));
    let shown = t.audio.lane(super::MASTER_ROW).is_some();
    p.rect_filled(rect, 2.0, if shown { ENVELOPE } else { ui.visuals().widgets.inactive.bg_fill });
    p.text(rect.center(), egui::Align2::CENTER_CENTER, "A", egui::FontId::proportional(10.0), if shown { egui::Color32::BLACK } else { colour });
    drawn.push(name);
    if b.clicked() {
        *pressed = true;
        cmds.push(AutomationCommand::CycleLane { track_id: super::MASTER_ROW.to_owned() });
    }
}

/// Header lane buttons and the lane rows.
#[allow(clippy::too_many_arguments)]
pub(in crate::view) fn lanes_ui(ui: &mut egui::Ui, l: &TimelineLayout, t: &TimelineState, views: &[LaneView], tpf: u64, drawn: &mut Vec<String>, cmds: &mut Vec<AutomationCommand>, pressed: &mut bool) {
    let font = egui::FontId::proportional(10.0);
    let text = ui.visuals().text_color();
    for row in l.tracks.iter().filter(|r| matches!(r.kind, TrackKind::Audio | TrackKind::Submix)) {
        let name = widget::lane_toggle(&row.track_id);
        let rect = egui::Rect::from_min_size(egui::pos2(l.lanes_left - LANE_BUTTON_PX - 2.0, row.rect.min.y + 2.0), egui::vec2(LANE_BUTTON_PX, LANE_BUTTON_PX));
        let r = ui.interact(rect, vw::id(&name), egui::Sense::click()).on_hover_text("Automation lane: click cycles clip volume / volume / pan / bus parameters / none");
        let row_name = row.name.clone();
        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("automation lane {row_name}")));
        let shown = t.audio.lane(&row.track_id).is_some();
        let p = ui.painter();
        p.rect_filled(rect, 2.0, if shown { ENVELOPE } else { ui.visuals().widgets.inactive.bg_fill });
        p.text(rect.center(), egui::Align2::CENTER_CENTER, "A", font.clone(), if shown { egui::Color32::BLACK } else { text });
        drawn.push(name);
        if r.clicked() {
            *pressed = true;
            cmds.push(AutomationCommand::CycleLane { track_id: row.track_id.clone() });
        }
    }
    for lane in &l.lanes {
        let Some(v) = views.iter().find(|v| v.track_id == lane.track_id) else { continue };
        let body = egui::Rect::from_min_max(egui::pos2(l.lanes_left, lane.rect.min.y), lane.rect.max);
        let header = egui::Rect::from_min_max(lane.rect.min, egui::pos2(l.lanes_left, lane.rect.max.y));
        let painter = ui.painter_at(lane.rect);
        painter.rect_filled(lane.rect, 0.0, LANE_BG);
        painter.text(egui::pos2(header.min.x + 3.0, header.min.y + 2.0), egui::Align2::LEFT_TOP, v.target.label(), font.clone(), text);
        let note = match (&v.error, v.target.editable()) {
            (Some(e), _) => e.chars().take(24).collect(),
            (None, false) => "(static)".to_owned(),
            (None, true) => format!("{:.0}..{:.0} {}", v.range.0, v.range.1, v.unit),
        };
        painter.text(egui::pos2(header.min.x + 3.0, header.max.y - 2.0), egui::Align2::LEFT_BOTTOM, note, font.clone(), text);
        let lanes = painter.with_clip_rect(body);
        let x_of = |at: Ticks| super::x_of(at, tpf, &t.view, l.lanes_left);
        let colour = if v.target.editable() { ENVELOPE } else { STATIC };
        // Visible tick range.
        let t0 = super::tick_at_x(body.min.x, tpf, &t.view, l.lanes_left);
        let t1 = super::tick_at_x(body.max.x, tpf, &t.view, l.lanes_left).value().saturating_add(tpf);
        let name = widget::lane(&v.track_id);
        let r = ui.interact(body, vw::id(&name), egui::Sense::click());
        let label = format!("{} lane {}", v.target.label(), v.track_id);
        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
        drawn.push(name);
        if r.clicked() {
            *pressed = true;
            if v.target.editable()
                && let Some(pos) = r.interact_pointer_pos()
            {
                let at = nearest_frame(pos.x, tpf, &t.view, l.lanes_left);
                cmds.push(AutomationCommand::AddPoint { track_id: v.track_id.clone(), lane: v.target.clone(), at, value: lane_value(lane.rect, v.range, pos.y) });
            }
        }
        let mut handles = 0usize;
        for span in &v.spans {
            if span.end.value() < t0.value() || span.start.value() > t1 {
                continue;
            }
            let pts: Vec<(u64, f64)> = span.points.iter().map(|p| (p.at.value(), p.value)).collect();
            let mut line = vec![egui::pos2(x_of(span.start), lane_y(lane.rect, v.range, value_at(&pts, span.start.value(), span.rest)))];
            for p in span.points.iter().filter(|p| p.at > span.start && p.at < span.end && p.at.value() >= t0.value() && p.at.value() <= t1).take(MAX_DRAWN_POINTS) {
                line.push(egui::pos2(x_of(p.at), lane_y(lane.rect, v.range, p.value)));
            }
            line.push(egui::pos2(x_of(span.end), lane_y(lane.rect, v.range, value_at(&pts, span.end.value(), span.rest))));
            lanes.add(egui::Shape::line(line, egui::Stroke::new(1.5_f32, colour)));
            if !v.target.editable() {
                continue;
            }
            for p in span.points.iter().filter(|p| p.at.value() >= t0.value() && p.at.value() <= t1) {
                if handles >= MAX_DRAWN_POINTS {
                    break;
                }
                handles = handles.saturating_add(1);
                let centre = egui::pos2(x_of(p.at), lane_y(lane.rect, v.range, p.value));
                let name = widget::lane_point(&v.track_id, p.id.clip_id.as_deref(), p.id.offset_ticks);
                let rect = egui::Rect::from_center_size(centre, egui::vec2(POINT_PX, POINT_PX));
                let h = ui.interact(rect, vw::id(&name), egui::Sense::click_and_drag());
                let frame = p.at.value().checked_div(tpf).unwrap_or(0);
                let text = format!("point {:.2} {} at frame {frame}", p.value, v.unit);
                h.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &text));
                drawn.push(name);
                let pointer = ui.ctx().input(|i| i.pointer.interact_pos().or(i.pointer.latest_pos()));
                let shown = if h.dragged() { pointer.unwrap_or(centre) } else { centre };
                lanes.rect_filled(egui::Rect::from_center_size(shown, egui::vec2(POINT_PX, POINT_PX)), 1.0, POINT);
                if h.clicked() || h.drag_started() || h.is_pointer_button_down_on() || h.secondary_clicked() {
                    *pressed = true;
                }
                let id = PointId { clip_id: p.id.clip_id.clone(), offset_ticks: p.id.offset_ticks };
                if h.drag_stopped()
                    && let Some(to) = pointer
                {
                    let at = nearest_frame(to.x, tpf, &t.view, l.lanes_left);
                    cmds.push(AutomationCommand::MovePoint { track_id: v.track_id.clone(), lane: v.target.clone(), point: id, at, value: lane_value(lane.rect, v.range, to.y) });
                } else if h.secondary_clicked() {
                    cmds.push(AutomationCommand::DeletePoint { track_id: v.track_id.clone(), lane: v.target.clone(), point: id });
                }
            }
        }
    }
}
