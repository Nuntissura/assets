mod fixture;
use egui::{
    Color32, Event, Key, Modifiers, PointerButton, Pos2, Rect, TouchDeviceId, TouchId, TouchPhase,
    Vec2,
};
use egui_kittest::{kittest::Queryable as _, Harness};
use fixture::*;
use hsscrap::*;
use rss_contracts::AcquisitionState;
use rss_core::query::*;
use rss_gui::{Action, MediaPainter, Response, WallView};
use std::{collections::BTreeSet, path::PathBuf};

#[derive(Default)]
struct SyntheticMedia {
    ready: BTreeSet<(ItemId, MemberId)>,
}
impl MediaPainter for SyntheticMedia {
    fn preview(&mut self, ui: &mut egui::Ui, rect: Rect, member: &MemberRef) -> bool {
        if !self.ready.contains(&(member.item_id, member.member_id)) {
            return false;
        }
        let alternate =
            member.item_id.to_string().ends_with('2') || member.item_id.to_string().ends_with('4');
        let (background, foreground) = if alternate {
            (
                Color32::from_rgb(35, 72, 66),
                Color32::from_rgb(196, 220, 174),
            )
        } else {
            (
                Color32::from_rgb(197, 102, 65),
                Color32::from_rgb(254, 220, 155),
            )
        };
        ui.painter().rect_filled(rect, 10.0, background);
        ui.painter().circle_filled(
            rect.center_top() + Vec2::new(-rect.width() * 0.18, rect.height() * 0.33),
            rect.width() * 0.15,
            foreground,
        );
        let points = vec![
            rect.left_bottom(),
            rect.left_center() + Vec2::new(0.0, 25.0),
            rect.center() + Vec2::new(40.0, -40.0),
            rect.right_center(),
            rect.right_bottom(),
        ];
        ui.painter().add(egui::Shape::convex_polygon(
            points,
            foreground.gamma_multiply(0.7),
            egui::Stroke::NONE,
        ));
        true
    }
    fn player(&mut self, ui: &mut egui::Ui, rect: Rect, member: &MemberRef) -> bool {
        self.preview(ui, rect, member);
        ui.painter().text(
            rect.center() + Vec2::new(0.0, 58.0),
            egui::Align2::CENTER_CENTER,
            "Synthetic host player",
            egui::FontId::proportional(17.0),
            Color32::WHITE,
        );
        true
    }
}

struct Host {
    query: QuerySession,
    wall: WallState,
    db: FixtureDb,
    view: WallView,
    manager: ManagerModel,
    media: SyntheticMedia,
    latest: Response,
    actions: Vec<Action>,
    dark: bool,
}
impl Host {
    fn select_context(&mut self) {
        let membership = ProjectMembership {
            membership_id: id(80_000),
            item_id: id(1),
            project_ref: ProjectRef(host("reading-project")),
            scope: ScopeRef("project-scope".into()),
            saved_at: Instant::parse("2026-10-22T12:00:00Z").unwrap(),
            created_here_at: at(),
            revision: Revision(1),
            lifecycle: Lifecycle::Active,
            removed_at: None,
            common: Extensions::default(),
        };
        let mut spec = query(PageSize::Items25);
        spec.context = ContextFilter::Project {
            project: membership.project_ref.clone(),
            scope: membership.scope.clone(),
            folder: None,
        };
        self.query.reset(spec).unwrap();
        self.db.canonical.truncate(1);
        self.db.canonical[0].selected_context = Some(SelectedContext {
            membership,
            folder: None,
        });
        let ticket = self.query.load().unwrap();
        self.query
            .complete(&ticket, Ok(self.db.page(&ticket)))
            .unwrap();
        self.wall.reconcile(&self.query);
        install(
            &self.query,
            &mut self.wall,
            vec![card(1, MediaKind::Image, 1)],
        );
        self.media.ready.clear();
    }
    fn new() -> Self {
        let (query, mut wall, db) = loaded(6);
        let cards: Vec<_> = (1..=6)
            .map(|number| {
                let mut value = match number {
                    2 | 6 => card(number, MediaKind::Video, 1),
                    3 => card(number, MediaKind::Image, 3),
                    4 => card(number, MediaKind::Link, 0),
                    _ => card(number, MediaKind::Image, 1),
                };
                if number == 5 {
                    value.members[0].availability.state = AvailabilityState::Unavailable;
                }
                value
            })
            .collect();
        install(&query, &mut wall, cards);
        let (library, selection) = library();
        let manager = ManagerModel::from_library(
            &library,
            &selection,
            &BTreeSet::from([host("source")]),
            &Default::default(),
            &bounds(),
        )
        .unwrap();
        Self {
            query,
            wall,
            db,
            view: WallView::new("fixture"),
            manager,
            media: SyntheticMedia::default(),
            latest: Response::default(),
            actions: vec![],
            dark: false,
        }
    }
    fn handle(&mut self) {
        for action in self.latest.actions.clone() {
            match &action {
                Action::Query(ticket) => {
                    self.query
                        .complete(ticket, Ok(self.db.page(ticket)))
                        .unwrap();
                }
                Action::Wall(WallIntent::ResolveMedia(ticket)) => {
                    self.wall.complete_media(ticket, MediaState::Ready).unwrap();
                    self.media
                        .ready
                        .insert((ticket.member.item_id, ticket.member.member_id));
                }
                Action::Wall(WallIntent::InvalidateMedia { item_id }) => {
                    self.media.ready.retain(|(item, _)| item != item_id);
                }
                Action::Wall(WallIntent::AccessRevoked) => self.query.revoke(),
                Action::Wall(WallIntent::Inspect(ticket)) => {
                    let mut inspection = inspection(self.wall.card(ticket.item_id).unwrap());
                    if let Some(context) = &ticket.context {
                        inspection.memberships.push(context.membership.clone());
                        inspection.targets.push(hsscrap::interaction::TargetAccess {
                            target: TagTarget::Project(context.membership.project_ref.clone()),
                            scope: context.membership.scope.clone(),
                            label: "Selected reading project".into(),
                            writable: false,
                        });
                        inspection.assignments.push(TagAssignment {
                            assignment_id: id(90_002),
                            tag_ref: TagRef(host("reading")),
                            target: TagTarget::Project(context.membership.project_ref.clone()),
                            scope: context.membership.scope.clone(),
                            origin: TagOrigin::User,
                            created_at: at(),
                            removed_at: None,
                            revision: Revision(1),
                            common: Extensions::default(),
                        });
                        inspection
                            .tag_choices
                            .push(hsscrap::interaction::TagChoice {
                                tag: TagRef(host("reading")),
                                label: "reading".into(),
                            });
                    }
                    self.view
                        .complete_inspection(&self.wall, ticket, inspection)
                        .unwrap();
                }
                _ => {}
            }
            self.actions.push(action);
        }
    }
}
fn harness(width: f32, height: f32) -> Harness<'static, Host> {
    Harness::builder()
        .with_size(Vec2::new(width, height))
        .build_state(
            |context, host: &mut Host| {
                context.set_visuals(if host.dark {
                    egui::Visuals::dark()
                } else {
                    egui::Visuals::light()
                });
                egui::CentralPanel::default().show(context, |ui| {
                    ui.label("SYNTHETIC RUST WALL PROOF · no live provider or host database");
                    host.latest = host.view.show(
                        ui,
                        &mut host.query,
                        &mut host.wall,
                        &host.manager,
                        &mut host.media,
                    );
                });
                let changed = !host.latest.actions.is_empty();
                host.handle();
                if changed {
                    context.request_repaint();
                }
            },
            Host::new(),
        )
}
fn info(item: u64) -> String {
    format!("Inspect item metadata and tags {}", id::<ItemId>(item))
}
fn touch(harness: &mut Harness<'_, Host>, position: Pos2) {
    harness.input_mut().events.extend([
        Event::Touch {
            device_id: TouchDeviceId(1),
            id: TouchId(1),
            phase: TouchPhase::Start,
            pos: position,
            force: None,
        },
        Event::PointerMoved(position),
        Event::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        },
    ]);
    harness.step();
    assert!(harness.ctx.input(|input| input.any_touches()));
    harness.input_mut().events.extend([
        Event::Touch {
            device_id: TouchDeviceId(1),
            id: TouchId(1),
            phase: TouchPhase::End,
            pos: position,
            force: None,
        },
        Event::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        },
        Event::PointerGone,
    ]);
    harness.step();
    harness.run();
}

#[test]
fn metadata_stays_off_cards_and_keyboard_escape_returns_to_exact_info_trigger() {
    let mut harness = harness(1100.0, 1000.0);
    harness.run();
    assert!(harness.query_by_label("Item details").is_none());
    harness.get_by_label(&info(1)).focus();
    harness.run();
    harness.key_press(Key::Enter);
    harness.run();
    assert!(harness.state_mut().view.inspector().is_some());
    assert!(harness.get_by_label("Close feed item details").is_focused());
    harness.key_press(Key::Escape);
    harness.run();
    assert!(harness.state_mut().view.inspector().is_none());
    assert!(harness.get_by_label(&info(1)).is_focused());
}

#[test]
fn raw_touch_opens_inspector_and_gallery_change_keeps_member_identity() {
    let mut harness = harness(390.0, 1000.0);
    harness.run();
    let rect = harness.state().latest.controls[&format!("info-{}", id::<ItemId>(1))];
    touch(&mut harness, rect.center());
    assert!(harness.state_mut().view.inspector().is_some());
    harness.get_by_label("Close feed item details").click();
    harness.run();
    assert!(harness.state_mut().view.inspector().is_none());
    // Gallery controls are exercised in the actual wide viewport where item3 is visible.
    let mut wide = self::harness(1100.0, 1000.0);
    wide.run();
    let rect = wide.state().latest.controls[&format!("next-{}", id::<ItemId>(3))];
    touch(&mut wide, rect.center());
    assert_eq!(
        wide.state().wall.selected_member(id(3)).unwrap().member_id,
        id(301)
    );
}

#[test]
fn rendering_video_does_not_play_and_activating_second_stops_first() {
    let mut harness = harness(1440.0, 1200.0);
    harness.run();
    assert!(harness.state().wall.player().is_none());
    let rect = harness.state().latest.controls[&format!("play-{}", id::<ItemId>(2))];
    touch(&mut harness, rect.center());
    assert_eq!(harness.state().wall.player().unwrap().item_id, id(2));
    let rect = harness.state().latest.controls[&format!("play-{}", id::<ItemId>(6))];
    touch(&mut harness, rect.center());
    assert_eq!(harness.state().wall.player().unwrap().item_id, id(6));
    assert!(harness.state().actions.iter().any(|action| matches!(action, Action::Wall(WallIntent::StopPlayback(member)) if member.item_id == id(2))));
}

#[test]
fn manager_changes_are_intents_and_canonical_checkbox_remains_until_confirmation() {
    let mut harness = harness(390.0, 1100.0);
    harness.run();
    harness.get_by_label("Manage sources").click();
    harness.run();
    harness.get_by_label("Select source Field notes").click();
    harness.run();
    assert!(harness.state().actions.iter().any(|action| matches!(
        action,
        Action::SetSourceSelected {
            selected: false,
            ..
        }
    )));
    assert!(harness.state().manager.sources[0].selected);
    harness.state_mut().manager.sources[0].state = AcquisitionState::Reconnect;
    harness.run();
    assert!(harness.query_by_label("Reconnect needed").is_some());
}

#[test]
fn narrow_overlay_edit_controls_fit_and_escape_cancels_before_closing() {
    let mut harness = harness(390.0, 1000.0);
    harness.run();
    harness.get_by_label(&info(1)).click();
    harness.run();
    harness.get_by_label("Edit details").click();
    harness.run();
    for key in [
        "details-title",
        "details-title-origin",
        "details-description-origin",
        "details-save",
        "details-cancel",
    ] {
        let rect = harness.state().latest.controls[key];
        assert!(
            rect.left() >= 0.0 && rect.right() <= 390.0,
            "{key}: {rect:?}"
        );
    }
    assert_eq!(
        harness
            .state_mut()
            .view
            .inspector()
            .unwrap()
            .session
            .phase(),
        &hsscrap::interaction::Phase::Editing
    );
    assert!(harness
        .state_mut()
        .view
        .inspector()
        .unwrap()
        .session
        .draft()
        .is_some());
    harness.key_press(Key::Escape);
    harness.run();
    assert!(harness.state_mut().view.inspector().is_some());
    assert_eq!(
        harness
            .state_mut()
            .view
            .inspector()
            .unwrap()
            .session
            .phase(),
        &hsscrap::interaction::Phase::Inspect
    );
    harness.key_press(Key::Escape);
    harness.run();
    assert!(harness.state_mut().view.inspector().is_none());
}

#[test]
fn current_denial_closes_details_and_hides_the_manager_and_private_cards() {
    let mut harness = harness(1100.0, 1000.0);
    harness.run();
    harness.get_by_label("Manage sources").click();
    harness.run();
    harness.get_by_label(&info(1)).click();
    harness.run();
    let ticket = harness.state_mut().query.refresh().unwrap();
    harness
        .state_mut()
        .query
        .complete(&ticket, Err(rss_contracts::Error::Denied))
        .unwrap_err();
    harness.run();
    assert!(harness.state_mut().view.inspector().is_none());
    assert!(harness.state().wall.card(id(1)).is_none());
    assert!(harness.state().media.ready.is_empty());
    assert!(harness
        .query_by_label("Select source Field notes")
        .is_none());
    assert!(harness.query_by_label(&info(1)).is_none());
}

#[test]
fn offscreen_cards_do_not_resolve_previews_and_modal_keyboard_stays_inside() {
    let mut harness = harness(390.0, 1000.0);
    harness.run();
    assert!(!harness.state().actions.iter().any(|action| matches!(action,Action::Wall(WallIntent::ResolveMedia(ticket)) if ticket.member.item_id == id(6))));
    harness.get_by_label(&info(1)).click();
    harness.run();
    for _ in 0..20 {
        harness.key_press(Key::Tab);
        harness.run();
        assert!(!harness.get_by_label("Manage sources").is_focused());
        assert!(!harness.get_by_label(&info(1)).is_focused());
    }
}

fn proof_path(name: &str) -> PathBuf {
    let directory = std::env::var_os("RSS_GUI_PROOF_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../.local/rss-gui/proof")
        });
    std::fs::create_dir_all(&directory).unwrap();
    directory.join(format!("{name}.png"))
}

fn save(harness: &mut Harness<'_, Host>, name: &str) {
    harness.run();
    harness
        .render()
        .expect("actual egui WGPU rendering")
        .save(proof_path(name))
        .unwrap();
}

#[test]
fn render_standalone_wall_and_narrow_state_surfaces() {
    for (width, dark, name) in [
        (1440.0, false, "wall-desktop-light"),
        (390.0, true, "wall-mobile-dark"),
    ] {
        let mut harness = harness(width, 1000.0);
        harness.state_mut().dark = dark;
        save(&mut harness, name);
        for (key, rect) in &harness.state().latest.controls {
            assert!(
                rect.left() >= 0.0 && rect.right() <= width + 1.0,
                "{key}: {rect:?}"
            );
        }
        harness.get_by_label(&info(1)).click();
        save(&mut harness, &format!("{name}-details"));
        harness.get_by_label("Edit details").click();
        save(&mut harness, &format!("{name}-edit"));
    }
    for (state, name) in [
        (0, "wall-empty"),
        (1, "wall-loading"),
        (2, "wall-partial"),
        (3, "wall-reconnect"),
        (4, "wall-failed"),
    ] {
        let mut harness = harness(390.0, 1000.0);
        let spec = query(PageSize::Items25);
        harness.state_mut().query.reset(spec).unwrap();
        let ticket = harness.state_mut().query.load().unwrap();
        if state != 1 {
            let mut page = harness.state_mut().db.page(&ticket);
            match state {
                0 => {
                    page.rows.clear();
                    page.next = None;
                }
                2 => {
                    page.health = PageHealth::Partial {
                        reason: "One source is temporarily unavailable".into(),
                    }
                }
                3 => {
                    page.rows.clear();
                    page.next = None;
                    page.health = PageHealth::Reconnect;
                }
                _ => {
                    page.rows.clear();
                    page.next = None;
                    page.health = PageHealth::Failed {
                        reason: "Synthetic network failure".into(),
                    };
                }
            }
            harness
                .state_mut()
                .query
                .complete(&ticket, Ok(page))
                .unwrap();
        }
        save(&mut harness, name);
    }
    let mut manager = harness(390.0, 1000.0);
    manager.run();
    manager.get_by_label("Manage sources").click();
    save(&mut manager, "wall-manager-mobile");
}

#[test]
fn selected_project_date_and_tags_render_from_the_same_inspection() {
    let mut selected = harness(390.0, 1000.0);
    selected.state_mut().select_context();
    selected.run();
    selected.get_by_label(&info(1)).click();
    selected.run();
    selected.get_by_label("Link and capture provenance").click();
    selected.run();
    assert!(selected
        .query_by_label("Selected reading project · saved 2026-10-22T12:00:00Z")
        .is_some());
    assert!(selected.query_by_label("reading").is_some());
    selected.get_by_label("reading").scroll_to_me();
    selected.run();
    for label in [
        "Selected reading project · saved 2026-10-22T12:00:00Z",
        "reading",
    ] {
        let rect = selected.get_by_label(label).rect();
        assert!(rect.top() >= 152.0 && rect.bottom() <= 968.0);
    }
    save(&mut selected, "wall-selected-context-details");
    let inspector = selected.state_mut().view.inspector().unwrap();
    assert_eq!(inspector.session.inspection().unwrap().memberships.len(), 1);
    assert_eq!(
        inspector.session.inspection().unwrap().memberships[0].project_ref,
        ProjectRef(host("reading-project"))
    );
}

#[test]
fn modal_information_has_readable_glyph_cores_in_both_host_themes() {
    fn luminance(rgb: [u8; 3]) -> f64 {
        let linear = rgb.map(|value| {
            let value = f64::from(value) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        });
        0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2]
    }
    for (width, dark, name) in [
        (1440.0, false, "wall-desktop-light-details"),
        (390.0, true, "wall-mobile-dark-details"),
    ] {
        let mut harness = harness(width, 1000.0);
        harness.state_mut().dark = dark;
        harness.run();
        harness.get_by_label(&info(1)).click();
        harness.run();
        let image = harness.render().expect("actual egui WGPU rendering");
        image.save(proof_path(name)).unwrap();
        for label in [
            "User override",
            "Field note 1: a place for useful ideas",
            "Each assignment keeps its item or context origin.",
        ] {
            let rect = harness.get_by_label(label).rect();
            let mut pixels = std::collections::BTreeMap::<[u8; 3], usize>::new();
            for y in (rect.top().floor().max(0.0) as u32)
                ..(rect.bottom().ceil() as u32).min(image.height())
            {
                for x in (rect.left().floor().max(0.0) as u32)
                    ..(rect.right().ceil() as u32).min(image.width())
                {
                    *pixels
                        .entry(image.get_pixel(x, y).0[..3].try_into().unwrap())
                        .or_default() += 1;
                }
            }
            let background = *pixels.iter().max_by_key(|(_, count)| *count).unwrap().0;
            let background_luminance = luminance(background);
            let mut core_pixels = 0;
            let mut peak = 1.0_f64;
            for (pixel, count) in pixels {
                let foreground = luminance(pixel);
                let ratio = (foreground.max(background_luminance) + 0.05)
                    / (foreground.min(background_luminance) + 0.05);
                peak = peak.max(ratio);
                if ratio >= 4.5 {
                    core_pixels += count;
                }
            }
            println!("rendered_contrast {name} label={label:?} background={background:?} peak={peak} core_pixels={core_pixels}");
            assert!(
                core_pixels
                    >= (label.chars().filter(|value| !value.is_whitespace()).count() / 2).max(3),
                "{name}: {label:?} has insufficient actual glyph cores"
            );
        }
        harness.get_by_label("Edit details").click();
        save(&mut harness, &name.replace("-details", "-edit"));
    }
}
