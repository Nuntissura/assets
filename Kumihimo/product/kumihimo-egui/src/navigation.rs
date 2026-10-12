//! WP011 navigation is the authority for range, gestures, observations and requests.
use crate::input::{InputState, PointerPhase, PointerSource};
use crate::*;
use kumihimo_content::{
    GestureSettlement, NavigationKey, NavigationMode, Observation, PointerKind, RailGeometry, Scope,
};
use kumihimo_core::Scalar;

#[derive(Clone, Debug)]
pub(crate) struct NavigationCapture {
    scope: Scope,
    gesture: u64,
    pointer: u64,
    source: PointerSource,
    generation: u64,
    epoch: u64,
}
pub fn draw(
    ctx: &egui::Context,
    controller: &mut Controller,
    projection: &workspace::Projection,
    surface: SurfaceBinding,
    host: &mut impl DesktopHost,
    state: &mut InputState,
    report: &mut FrameReport,
) {
    let now = controller.inspect().now;
    let mut events = crate::input::pointer_events(ctx, surface.epoch);
    events.extend(state.injected.iter().copied());
    if let Some(capture) = state.navigation_capture.clone() {
        let reason = if capture.epoch != surface.epoch
            || capture.generation != controller.snapshot().generation
        {
            Some(GestureSettlement::Invalidated)
        } else if ctx.input(|i| {
            !i.focused
                || i.events
                    .iter()
                    .any(|e| matches!(e, egui::Event::WindowFocused(false)))
        }) {
            Some(GestureSettlement::FocusLost)
        } else if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            Some(GestureSettlement::Escape)
        } else if !events.iter().any(|e| {
            e.pointer == capture.pointer
                && e.source == capture.source
                && e.phase == PointerPhase::Release
        }) && ctx.input(|i| {
            i.events
                .iter()
                .any(|e| matches!(e, egui::Event::PointerGone))
        }) {
            Some(GestureSettlement::CaptureLost)
        } else {
            None
        };
        if let Some(reason) = reason {
            settle(controller, state, capture, reason, now, report);
        }
    }
    let initial = controller.inspect();
    for slot in report.slots.clone() {
        let Some(mount) = initial
            .mounts
            .values()
            .find(|m| m.active && m.scope.target.view() == Some(slot.view))
        else {
            continue;
        };
        let scope = mount.scope.clone();
        let Some(navigation) = controller.inspect().navigation.get(&scope.mount).cloned() else {
            continue;
        };
        let Some(frame) = projection.frames.iter().find(|f| f.frame == slot.frame) else {
            continue;
        };
        // A fixed gutter is excluded from the provider slot; expansion cannot reflow it.
        let gutter = match crate::content_and_navigation_bounds(controller, frame, &host.policy()) {
            Ok((_, Some(gutter))) => to_egui(gutter),
            Ok(_) => continue,
            Err(error) => {
                report.reject(error);
                continue;
            }
        };
        let document = to_egui(slot.bounds);
        let length = document.height().max(0.0) as f64;
        let rail = RailGeometry {
            origin: Scalar::new(document.top() as f64).unwrap(),
            length: Scalar::new(length).unwrap(),
            map_origin: Scalar::new(document.top() as f64).unwrap(),
            map_length: Scalar::new(length).unwrap(),
            document_rect: slot.bounds,
        };
        if navigation.rail.as_ref() != Some(&rail) {
            let Some(revision) = navigation
                .rail_revision
                .checked_add(1)
                .filter(|n| *n < u64::MAX)
            else {
                report.reject(AdapterError::Budget("navigation rail revision"));
                continue;
            };
            if let Err(error) = controller.update_navigation_rail(&scope, revision, rail) {
                report.reject(error);
                continue;
            }
        }
        let mut nav = match controller.navigation_projection(&scope) {
            Ok(n) => n,
            Err(e) => {
                report.reject(e);
                continue;
            }
        };
        let x = gutter.right() - nav.hit_width.get() as f32 / 2.0;
        let hit_rect = if nav.effective == Some(NavigationMode::MinimapAlways) || nav.held_viewport
        {
            gutter
        } else {
            egui::Rect::from_center_size(
                egui::pos2(x, document.center().y),
                egui::vec2(nav.hit_width.get() as f32, document.height()),
            )
        }
        .intersect(to_egui(frame.clip));
        let id = egui::Id::new((surface.viewport, "navigation", slot.view.raw()));
        let mut ui = egui::Ui::new(
            ctx.clone(),
            id.with("ui"),
            egui::UiBuilder::new()
                .layer_id(frame.layer())
                .max_rect(hit_rect),
        );
        ui.set_clip_rect(to_egui(frame.clip));
        let response = ui.interact(hit_rect, id, egui::Sense::click_and_drag());
        response.widget_info(|| {
            egui::WidgetInfo::slider(
                nav.disabled_reason.is_none(),
                nav.thumb.as_ref().map_or(0.0, |t| t.progress.get()),
                nav.accessible_name.clone(),
            )
        });
        ctx.accesskit_node_builder(response.id, |node| {
            node.set_role(egui::accesskit::Role::ScrollBar);
            node.set_min_numeric_value(0.0);
            if let Some(model) = &navigation.model {
                if let Ok(range) = model.range() {
                    node.set_max_numeric_value(range.get());
                }
            }
            if let Observation::Known(actual) = &nav.observation {
                node.set_numeric_value(actual.offset.get());
            }
            node.add_action(egui::accesskit::Action::Increment);
            node.add_action(egui::accesskit::Action::Decrement);
            node.add_action(egui::accesskit::Action::SetValue);
        });
        crate::chrome::record(
            report,
            &response,
            format!("view/{}/navigation", slot.view.raw()),
            nav.accessible_name.clone(),
            Some(slot.view),
        );
        let hovered = response.hovered();
        if let Err(e) = controller.navigation_reveal(&scope, hovered, response.has_focus(), false) {
            report.reject(e);
        }
        let current = controller
            .inspect()
            .navigation
            .get(&scope.mount)
            .cloned()
            .unwrap_or(navigation.clone());
        // Wheel ownership follows the same clipped rail hit target as pointer gestures.
        if state.active_capture().is_none()
            && state.navigation_capture.is_none()
            && ctx.input(|i| i.pointer.hover_pos()).is_some_and(|point| {
                hit_rect.contains(point)
                    && crate::input::pick_frame(projection, point) == Some(slot.frame)
            })
        {
            let wheels = ctx.input(|i| {
                i.events
                    .iter()
                    .filter_map(|event| {
                        if let egui::Event::MouseWheel { unit, delta, .. } = event {
                            Some((*unit, *delta))
                        } else {
                            None
                        }
                    })
                    .take(16)
                    .collect::<Vec<_>>()
            });
            for (unit, delta) in wheels {
                state.consumed_pointer = true;
                if !delta.y.is_finite() {
                    report.reject(AdapterError::Invalid("navigation wheel delta"));
                    continue;
                }
                let live = controller.inspect().navigation.get(&scope.mount).cloned();
                if let Some(live) = live {
                    if let (Some(model), Observation::Known(actual)) =
                        (&live.model, &live.observation)
                    {
                        if let Ok(range) = model.range() {
                            let scale = match unit {
                                egui::MouseWheelUnit::Point => 1.0,
                                egui::MouseWheelUnit::Line => 16.0,
                                egui::MouseWheelUnit::Page => model.viewport.get(),
                            };
                            let value = (actual.offset.get() - delta.y as f64 * scale)
                                .clamp(0.0, range.get());
                            let request = kumihimo_content::NavigationRequest {
                                model_revision: model.revision,
                                settings_revision: live.settings_revision,
                                rail_revision: live.rail_revision,
                                gesture: None,
                                action: kumihimo_content::NavigationAction::AbsoluteScroll(
                                    Scalar::new(value).unwrap(),
                                ),
                            };
                            let permission = host.navigation_permission(controller, &scope);
                            if let Err(error) = controller.request(
                                scope.mount,
                                kumihimo_content::RequestKind::Navigation(request),
                                permission,
                                now,
                            ) {
                                report.reject(error);
                            }
                            if let Err(error) = host.navigation_pending(controller) {
                                report.reject(error);
                            }
                        }
                    } else {
                        report.reject(AdapterError::Invalid(
                            "navigation wheel observation unavailable",
                        ));
                    }
                }
            }
        }
        for event in &events {
            if event.pointer == 0
                || event.pointer == u64::MAX
                || !event.position.x.is_finite()
                || !event.position.y.is_finite()
            {
                report.reject(AdapterError::Invalid(
                    "navigation pointer identity/position",
                ));
                continue;
            }
            if event.epoch != surface.epoch {
                continue;
            }
            let owned = state.navigation_capture.as_ref().is_some_and(|c| {
                c.scope == scope && c.pointer == event.pointer && c.source == event.source
            });
            if owned {
                state
                    .consumed_pointers
                    .insert((event.pointer, event.source));
                state.consumed_pointer = true;
                let capture = state.navigation_capture.clone().unwrap();
                match event.phase {
                    PointerPhase::Move => {
                        let permission = host.navigation_permission(controller, &scope);
                        if let Ok(position) = Scalar::new(event.position.y as f64) {
                            if let Err(e) = controller.navigation_move(
                                &scope,
                                capture.gesture,
                                capture.pointer,
                                position,
                                permission,
                                now,
                            ) {
                                report.reject(e);
                                settle(
                                    controller,
                                    state,
                                    capture,
                                    GestureSettlement::Invalidated,
                                    now,
                                    report,
                                );
                            }
                        }
                    }
                    PointerPhase::Release => settle(
                        controller,
                        state,
                        capture,
                        GestureSettlement::Release,
                        now,
                        report,
                    ),
                    PointerPhase::Cancel => settle(
                        controller,
                        state,
                        capture,
                        GestureSettlement::Cancel,
                        now,
                        report,
                    ),
                    _ => {}
                }
            } else if event.phase == PointerPhase::Press
                && state.navigation_capture.is_none()
                && state.active_capture().is_none()
                && hit_rect.contains(event.position)
                && crate::input::pick_frame(projection, event.position) == Some(slot.frame)
            {
                let thumb = if nav.effective == Some(NavigationMode::MinimapAlways) {
                    &nav.viewport_rectangle
                } else {
                    &nav.thumb
                };
                if nav.disabled_reason.is_some() {
                    report.reject(format!("navigation unavailable: {:?}", nav.disabled_reason));
                    continue;
                }
                if thumb.as_ref().is_some_and(|t| {
                    event.position.y as f64 >= t.start.get()
                        && event.position.y as f64 <= t.start.get() + t.length.get()
                }) {
                    let Some(gesture) = current
                        .last_gesture
                        .checked_add(1)
                        .filter(|v| *v < u64::MAX)
                    else {
                        report.reject(AdapterError::Budget("navigation gesture identity"));
                        continue;
                    };
                    let kind = match event.source {
                        PointerSource::Mouse => PointerKind::Mouse,
                        PointerSource::Touch => PointerKind::Touch,
                        PointerSource::HostPen => PointerKind::Pen,
                    };
                    match controller.navigation_press(
                        &scope,
                        gesture,
                        event.pointer,
                        kind,
                        Scalar::new(event.position.y as f64).unwrap(),
                    ) {
                        Ok(()) => {
                            state.navigation_capture = Some(NavigationCapture {
                                scope: scope.clone(),
                                gesture,
                                pointer: event.pointer,
                                source: event.source,
                                generation: controller.snapshot().generation,
                                epoch: surface.epoch,
                            });
                            state
                                .consumed_pointers
                                .insert((event.pointer, event.source));
                            state.consumed_pointer = true;
                            response.request_focus();
                        }
                        Err(e) => report.reject(e),
                    }
                } else {
                    let key = if (event.position.y as f64)
                        < thumb.as_ref().map_or(0.0, |t| t.start.get())
                    {
                        NavigationKey::PageBackward
                    } else {
                        NavigationKey::PageForward
                    };
                    send_key(controller, host, &scope, key, now, report);
                    state
                        .consumed_pointers
                        .insert((event.pointer, event.source));
                    state.consumed_pointer = true;
                    response.request_focus();
                }
            }
        }
        if response.has_focus() {
            for (key, navigation_key) in [
                (egui::Key::ArrowUp, NavigationKey::LineBackward),
                (egui::Key::ArrowDown, NavigationKey::LineForward),
                (egui::Key::PageUp, NavigationKey::PageBackward),
                (egui::Key::PageDown, NavigationKey::PageForward),
                (egui::Key::Home, NavigationKey::Home),
                (egui::Key::End, NavigationKey::End),
            ] {
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key)) {
                    send_key(controller, host, &scope, navigation_key, now, report);
                }
            }
        }
        for (action, key) in [
            (
                egui::accesskit::Action::Increment,
                NavigationKey::LineForward,
            ),
            (
                egui::accesskit::Action::Decrement,
                NavigationKey::LineBackward,
            ),
        ] {
            let count = ctx.input(|i| i.num_accesskit_action_requests(response.id, action));
            for _ in 0..count.min(16) {
                send_key(controller, host, &scope, key, now, report);
            }
        }
        let values: Vec<_> = ctx.input(|i| {
            i.accesskit_action_requests(response.id, egui::accesskit::Action::SetValue)
                .filter_map(|r| {
                    if let Some(egui::accesskit::ActionData::NumericValue(v)) = r.data {
                        Some(v)
                    } else {
                        None
                    }
                })
                .take(16)
                .collect()
        });
        for value in values {
            if !value.is_finite() {
                report.reject(AdapterError::Invalid("accessible navigation value"));
                continue;
            }
            if let Some(model) = &current.model {
                if let Ok(range) = model.range() {
                    let request = kumihimo_content::NavigationRequest {
                        model_revision: model.revision,
                        settings_revision: current.settings_revision,
                        rail_revision: current.rail_revision,
                        gesture: None,
                        action: kumihimo_content::NavigationAction::AbsoluteScroll(
                            Scalar::new(value.clamp(0.0, range.get())).unwrap(),
                        ),
                    };
                    let permission = host.navigation_permission(controller, &scope);
                    if let Err(e) = controller.request(
                        scope.mount,
                        kumihimo_content::RequestKind::Navigation(request),
                        permission,
                        now,
                    ) {
                        report.reject(e);
                    }
                }
            }
        }
        if let Err(e) = host.navigation_pending(controller) {
            report.reject(e);
        }
        nav = match controller.navigation_projection(&scope) {
            Ok(n) => n,
            Err(e) => {
                report.reject(e);
                continue;
            }
        };
        let painter = ui.painter();
        if nav.rail_visible {
            let rail_rect = egui::Rect::from_center_size(
                hit_rect.center(),
                egui::vec2(nav.rail_width.get() as f32, hit_rect.height()),
            );
            painter.rect_filled(rail_rect, 2.0, egui::Color32::from_gray(35));
            if let Some(thumb) = &nav.thumb {
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(
                            x - nav.thumb_width.get() as f32 / 2.0,
                            thumb.start.get() as f32,
                        ),
                        egui::vec2(nav.thumb_width.get() as f32, thumb.length.get() as f32),
                    ),
                    3.0,
                    egui::Color32::LIGHT_GRAY,
                );
            }
        }
        if nav.minimap_visible {
            let map_right = if nav.rail_visible {
                (gutter.right() - nav.hit_width.get() as f32 - 2.0).max(gutter.left())
            } else {
                gutter.right()
            };
            let map_rect = egui::Rect::from_min_max(
                egui::pos2(gutter.left(), document.top()),
                egui::pos2(map_right, document.bottom()),
            );
            painter.rect_filled(map_rect, 0.0, egui::Color32::from_gray(25));
            if let (Some(model), Some(overview)) = (&current.model, &current.overview) {
                let extent = model.extent.get();
                if extent > 0.0 {
                    for marker in overview.markers.iter().take(host.policy().max_regions) {
                        let top = document.top()
                            + (marker.start.get() / extent) as f32 * document.height();
                        let bottom =
                            document.top() + (marker.end.get() / extent) as f32 * document.height();
                        let marker_rect = egui::Rect::from_min_max(
                            egui::pos2(map_rect.left() + 2.0, top),
                            egui::pos2(map_rect.right() - 2.0, bottom.max(top + 2.0)),
                        );
                        painter.rect_filled(
                            marker_rect,
                            0.0,
                            egui::Color32::from_rgb(76, 124, 168),
                        );
                        let response =
                            ui.interact(marker_rect, id.with(&marker.id), egui::Sense::click());
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                marker.jump_target.is_some(),
                                marker.label.clone(),
                            )
                        });
                        crate::chrome::record(
                            report,
                            &response,
                            format!("view/{}/marker/{}", slot.view.raw(), marker.id),
                            marker.label.clone(),
                            Some(slot.view),
                        );
                        if response.clicked() && marker.jump_target.is_some() {
                            let permission = host.navigation_permission(controller, &scope);
                            if let Err(e) =
                                controller.navigation_jump(&scope, &marker.id, permission, now)
                            {
                                report.reject(e);
                            }
                        }
                    }
                }
            }
            if let Some(viewport) = &nav.viewport_rectangle {
                let viewport = egui::Rect::from_min_size(
                    egui::pos2(map_rect.left(), viewport.start.get() as f32),
                    egui::vec2(map_rect.width(), viewport.length.get() as f32),
                );
                painter.rect_stroke(
                    viewport,
                    0.0,
                    egui::Stroke::new(2.0, egui::Color32::WHITE),
                    egui::StrokeKind::Inside,
                );
            }
        }
        if let Some(reason) = nav.disabled_reason {
            painter.text(
                hit_rect.center_top(),
                egui::Align2::RIGHT_TOP,
                format!("Unknown / {reason:?}"),
                egui::FontId::proportional(10.0),
                egui::Color32::LIGHT_RED,
            );
        }
    }
    if let Err(e) = host.navigation_pending(controller) {
        report.reject(e);
    }
}
fn send_key(
    controller: &mut Controller,
    host: &mut impl DesktopHost,
    scope: &Scope,
    key: NavigationKey,
    now: u64,
    report: &mut FrameReport,
) {
    let permission = host.navigation_permission(controller, scope);
    if let Err(e) = controller.navigation_key(scope, key, permission, now) {
        report.reject(e);
    }
}
fn settle(
    controller: &mut Controller,
    state: &mut InputState,
    capture: NavigationCapture,
    reason: GestureSettlement,
    now: u64,
    report: &mut FrameReport,
) {
    if let Err(e) = controller.navigation_settle(
        &capture.scope,
        capture.gesture,
        capture.pointer,
        reason,
        now,
    ) {
        report.reject(e);
    }
    state.navigation_capture = None;
    state.last_settlement = Some(match reason {
        GestureSettlement::Release => "navigation release",
        GestureSettlement::Cancel => "navigation cancelled",
        GestureSettlement::Escape => "navigation Escape",
        GestureSettlement::CaptureLost => "navigation capture lost",
        GestureSettlement::FocusLost => "navigation focus lost",
        GestureSettlement::Invalidated => "navigation invalidated",
    });
}
