mod fixture;
#[path = "../reference_host/adapter.rs"]
mod owner_adapter;
use egui::accesskit::Role;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, TouchDeviceId, TouchId, TouchPhase, Vec2};
use egui_kittest::{
    kittest::{NodeT as _, Queryable as _},
    Harness,
};
use fixture::*;
use hsscrap::interaction::Phase;
use hsscrap::ports::{ItemOwner, OwnerCommand};
use hsscrap::*;
use hsscrap_gui::{Inspector, ViewAction, ViewResponse};
use std::{collections::BTreeSet, path::PathBuf};

struct Host {
    inspector: Inspector,
    actions: Vec<ViewAction>,
    latest: ViewResponse,
}

fn harness(width: f32, height: f32) -> Harness<'static, Host> {
    Harness::builder()
        .with_size(Vec2::new(width, height))
        .build_state(
            |ctx, state: &mut Host| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.weak("SYNTHETIC GUI PROOF · reusable Rust inspector");
                    state.latest = state.inspector.show(ui);
                });
                for action in &state.latest.actions {
                    match action {
                        ViewAction::Save => {
                            let mut command = context(80);
                            let inspection = state.inspector.session.inspection().unwrap();
                            command.scope = inspection.scope.clone();
                            command.bounds = inspection.bounds.clone();
                            command.expected_revisions = inspection.owner_fences.clone();
                            state
                                .inspector
                                .session
                                .begin_save(command, &BTreeSet::new())
                                .unwrap();
                        }
                        ViewAction::ChooseTag { tag, target } => {
                            let mut value = assignment(90, "later", target.clone());
                            value.tag_ref = tag.clone();
                            state.inspector.session.stage_tag(value).unwrap();
                        }
                        _ => {}
                    }
                }
                state.actions.extend(state.latest.actions.clone());
            },
            Host {
                inspector: Inspector::new("proof", inspection()).unwrap(),
                actions: vec![],
                latest: ViewResponse::default(),
            },
        )
}

fn edit(harness: &mut Harness<'_, Host>) {
    harness.get_by_label("Edit details").click();
    harness.run();
}

fn change_title(harness: &mut Harness<'_, Host>, text: &str) {
    harness
        .get_by_role_and_label(Role::TextInput, "Title")
        .click();
    harness.run();
    harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
    harness.run();
    harness
        .get_by_role_and_label(Role::TextInput, "Title")
        .type_text(text);
    harness.run();
}

fn touch(harness: &mut Harness<'_, Host>, pos: Pos2) {
    harness.input_mut().events.extend([
        Event::Touch {
            device_id: TouchDeviceId(1),
            id: TouchId(1),
            phase: TouchPhase::Start,
            pos,
            force: None,
        },
        Event::PointerMoved(pos),
        Event::PointerButton {
            pos,
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
            pos,
            force: None,
        },
        Event::PointerButton {
            pos,
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
fn keyboard_edit_escape_restores_focus_and_cancel_sends_no_owner_command() {
    let mut harness = harness(800.0, 1000.0);
    harness.run();
    harness.key_press(Key::Tab);
    harness.run();
    assert!(harness.get_by_label("Edit details").is_focused());
    harness.key_press(Key::Enter);
    harness.run();
    assert_eq!(harness.state().inspector.session.phase(), &Phase::Editing);
    assert!(harness.get_by_label("Cancel edits").is_focused());
    change_title(&mut harness, "Keyboard draft");
    assert!(harness.state().inspector.session.dirty());
    harness.key_press(Key::Escape);
    harness.run();
    assert!(harness.get_by_label("Edit details").is_focused());
    assert_eq!(harness.state().inspector.session.phase(), &Phase::Inspect);
    assert!(harness.state().inspector.session.pending().is_none());
    assert!(harness.state().actions.is_empty());
    harness.key_press(Key::Escape);
    harness.run();
    assert_eq!(harness.state().actions, vec![ViewAction::Close]);
}

#[test]
fn keyboard_navigation_skips_read_only_owner_and_dispatches_save() {
    let mut harness = harness(390.0, 1000.0);
    harness.run();
    edit(&mut harness);
    let mut found = false;
    for _ in 0..24 {
        harness.key_press(Key::Tab);
        harness.run();
        assert!(!harness
            .get_by_label("Remove Reference from Folder")
            .is_focused());
        if harness
            .get_by_label("Remove Design from Project")
            .is_focused()
        {
            found = true;
            break;
        }
    }
    assert!(found, "project removal must be reachable by keyboard");
    harness.key_press(Key::Space);
    harness.run();
    assert!(harness
        .state()
        .inspector
        .session
        .draft()
        .unwrap()
        .removals
        .contains(&AssignmentId(uuid::Uuid::from_u128(22))));
    let mut found = false;
    for _ in 0..24 {
        harness.key_press(Key::Tab);
        harness.run();
        if harness.get_by_label("Save changes").is_focused() {
            found = true;
            break;
        }
    }
    assert!(found, "save must be reachable by keyboard");
    harness.key_press(Key::Enter);
    harness.run();
    assert_eq!(harness.state().actions, vec![ViewAction::Save]);
}

#[test]
fn touch_stages_owner_context_removal_and_save_without_changing_inspection() {
    let mut harness = harness(390.0, 1100.0);
    harness.run();
    let edit_pos = harness.state().latest.controls["edit"].center();
    touch(&mut harness, edit_pos);
    assert_eq!(harness.state().inspector.session.phase(), &Phase::Editing);
    harness
        .get_by_label("Remove Design from Project")
        .scroll_to_me();
    harness.run();
    let position = harness
        .get_by_label("Remove Design from Project")
        .rect()
        .center();
    touch(&mut harness, position);
    let save_pos = harness.state().latest.controls["save"].center();
    touch(&mut harness, save_pos);
    let request = harness.state().inspector.session.pending().unwrap();
    assert_eq!(
        request.removal_targets,
        vec![(AssignmentId(uuid::Uuid::from_u128(22)), project())]
    );
    assert_eq!(
        harness
            .state()
            .inspector
            .session
            .inspection()
            .unwrap()
            .assignments,
        inspection().assignments
    );
    assert_eq!(harness.state().actions, vec![ViewAction::Save]);
}

#[test]
fn canonical_tag_picker_stages_addition_and_cancel_discards_it() {
    let mut harness = harness(800.0, 1200.0);
    harness.run();
    edit(&mut harness);
    harness.get_by_label("Choose canonical tag").scroll_to_me();
    harness.run();
    harness.get_by_label("Choose canonical tag").click();
    harness.run();
    harness.get_by_label("Read later · later").click();
    harness.run();
    harness.get_by_label("Add tag to draft").click();
    harness.run();
    assert_eq!(
        harness.state().inspector.session.draft().unwrap().additions[0].tag_ref,
        tag("later")
    );
    assert_eq!(
        harness.state().inspector.session.draft().unwrap().additions[0].target,
        TagTarget::Item(item_id())
    );
    harness.get_by_label("Cancel edits").click();
    harness.run();
    assert!(harness.state().inspector.session.draft().is_none());
    assert!(harness.state().inspector.session.pending().is_none());
    assert!(!harness.state().actions.contains(&ViewAction::Save));
}

#[test]
fn catalog_reload_clears_removed_selection_and_disables_stale_tag_dispatch() {
    let mut harness = harness(800.0, 1200.0);
    harness.run();
    edit(&mut harness);
    harness.get_by_label("Choose canonical tag").scroll_to_me();
    harness.run();
    harness.get_by_label("Choose canonical tag").click();
    harness.run();
    harness.get_by_label("Read later · later").click();
    harness.run();
    assert!(!harness
        .get_by_label("Add tag to draft")
        .accesskit_node()
        .is_disabled());
    let mut refreshed = inspection();
    refreshed
        .tag_choices
        .retain(|choice| choice.tag != tag("later"));
    harness
        .state_mut()
        .inspector
        .session
        .reload(refreshed)
        .unwrap();
    harness.run();
    edit(&mut harness);
    harness.get_by_label("Choose canonical tag").scroll_to_me();
    harness.run();
    assert!(harness
        .get_by_label("Add tag to draft")
        .accesskit_node()
        .is_disabled());
    harness.get_by_label("Add tag to draft").click();
    harness.run();
    assert!(harness.state().actions.is_empty());
    assert!(harness
        .state()
        .inspector
        .session
        .draft()
        .unwrap()
        .additions
        .is_empty());
    assert!(harness.state().inspector.session.pending().is_none());
}

#[test]
fn escape_closes_tag_popup_before_canceling_the_edit() {
    let mut harness = harness(800.0, 1200.0);
    harness.run();
    edit(&mut harness);
    harness.get_by_label("Choose canonical tag").scroll_to_me();
    harness.run();
    harness.get_by_label("Choose canonical tag").click();
    harness.run();
    assert!(harness.query_by_label("Read later · later").is_some());
    harness.key_press(Key::Escape);
    harness.run();
    assert!(harness.query_by_label("Read later · later").is_none());
    assert_eq!(harness.state().inspector.session.phase(), &Phase::Editing);
    harness.key_press(Key::Escape);
    harness.run();
    assert_eq!(harness.state().inspector.session.phase(), &Phase::Inspect);
}

#[test]
fn widget_save_uses_existing_item_owner_and_rereads_canonical_file() {
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("hsscrap-gui-owner-{}-{suffix}", std::process::id()));
    let path = directory.join("owner.json");
    let mut owner = owner_adapter::ReferenceHost::new(
        path.clone(),
        owner_adapter::Limits {
            max_bytes: 1_000_000,
            max_items: 100,
            max_tags: 100,
            max_operations: 100,
        },
        at(),
    )
    .unwrap();
    let fixture = inspection();
    owner
        .initialize_with_observations(
            fixture.scope.clone(),
            BTreeSet::from([context(81).actor]),
            vec![fixture.item.clone()],
            fixture.captures.clone(),
            vec![fixture.selected_observation.clone().unwrap()],
            BTreeSet::new(),
        )
        .unwrap();
    let canonical = owner.inspect(&context(81), item_id()).unwrap();
    let mut initial = fixture;
    initial.item = canonical.item;
    initial.captures = canonical.captures;
    initial.selected_observation = canonical
        .observations
        .into_iter()
        .find(|value| Some(value.observation_id) == initial.item.selected_observation_id);
    initial.assignments.clear();
    initial.tag_choices.clear();
    initial.memberships.clear();
    initial.targets.truncate(1);
    initial.item_owner = owner_adapter::item_ref(item_id());
    initial.owner_fences = vec![
        ExpectedRevision {
            owner: initial.item_owner.clone(),
            revision: initial.item.revision,
        },
        ExpectedRevision {
            owner: owner_adapter::catalogue_ref(&initial.scope),
            revision: Revision(1),
        },
    ];
    let mut harness = harness(800.0, 1000.0);
    harness.state_mut().inspector = Inspector::new("owner-proof", initial.clone()).unwrap();
    harness.run();
    edit(&mut harness);
    change_title(&mut harness, "Saved through the canonical item owner");
    harness.get_by_label("Save changes").click();
    harness.run();
    let request = harness.state().inspector.session.pending().unwrap().clone();
    let OwnerCommand::ItemPatch { item, patch } = request.plan.commands[0].clone() else {
        panic!("expected item owner patch")
    };
    let outcome = owner
        .apply_patch(request.plan.context.clone(), item, patch)
        .unwrap();
    let reread: owner_adapter::State =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        reread.items[&item].title_override,
        Override::Set("Saved through the canonical item owner".into())
    );
    assert_eq!(
        reread
            .observations
            .values()
            .next()
            .unwrap()
            .title
            .as_deref(),
        Some("Source title: a field guide to collections")
    );
    let canonical = owner.inspect(&request.plan.context, item).unwrap();
    let mut refreshed = initial;
    refreshed.item = canonical.item;
    refreshed.owner_fences[0].revision = refreshed.item.revision;
    harness
        .state_mut()
        .inspector
        .session
        .complete(request.ticket, Ok(outcome));
    harness.run();
    assert_eq!(
        harness.state().inspector.session.phase(),
        &Phase::ReloadRequired
    );
    harness
        .state_mut()
        .inspector
        .session
        .reload(refreshed)
        .unwrap();
    harness.run();
    assert!(harness
        .query_by_label("Saved through the canonical item owner")
        .is_some());
    assert_eq!(harness.state().inspector.session.phase(), &Phase::Inspect);
    for file in [
        path,
        directory.join("owner.lock"),
        directory.join("owner.next"),
    ] {
        if file.exists() {
            std::fs::remove_file(file).unwrap();
        }
    }
    std::fs::remove_dir(directory).unwrap();
}

#[test]
fn owner_conflict_and_uncertainty_have_distinct_recovery_actions() {
    let mut harness = harness(800.0, 1000.0);
    harness.run();
    edit(&mut harness);
    change_title(&mut harness, "Conflict draft");
    harness.get_by_label("Save changes").click();
    harness.run();
    let ticket = harness.state().inspector.session.pending().unwrap().ticket;
    harness.state_mut().inspector.session.complete(
        ticket,
        Err(Error::UnknownOutcome {
            operation_id: ticket.operation_id,
        }),
    );
    harness.run();
    harness.get_by_label("Check save status").click();
    harness.run();
    assert!(harness
        .state()
        .actions
        .contains(&ViewAction::Recover(ticket.operation_id)));
    assert_eq!(
        harness.state().inspector.session.pending().unwrap().ticket,
        ticket
    );
    harness.state_mut().inspector.session.complete(
        ticket,
        Err(Error::Conflict {
            field: "owner_revision".into(),
        }),
    );
    harness.run();
    harness.get_by_label("Reload current data").click();
    harness.run();
    assert!(harness.state().actions.contains(&ViewAction::Reload));
    assert_eq!(
        harness.state().inspector.session.draft().unwrap().title,
        Override::Set("Conflict draft".into())
    );
}

#[test]
fn revocation_removes_private_text_and_all_edit_controls() {
    let mut harness = harness(390.0, 844.0);
    harness.run();
    edit(&mut harness);
    harness.state_mut().inspector.session.revoke();
    harness.run();
    assert!(harness.query_by_label("Edit details").is_none());
    assert!(harness
        .query_by_label("A field guide worth keeping")
        .is_none());
    assert!(harness
        .query_by_label("Reading collection · Project")
        .is_none());
    assert!(harness.query_by_label("Choose canonical tag").is_none());
    harness.get_by_label("Close details").click();
    harness.run();
    assert!(harness.state().actions.contains(&ViewAction::Close));
}

#[test]
fn provenance_preserves_unknown_capture_time_and_project_saved_time() {
    let mut harness = harness(390.0, 1000.0);
    harness.run();
    harness.get_by_label("Link and capture provenance").click();
    harness.run();
    assert!(harness
        .query_by_label("https://example.org/design-notes")
        .is_some());
    assert!(harness.query_by_label("Captured at").is_some());
    assert!(harness
        .query_by_label("Reading collection · saved 2026-10-22T12:00:00Z")
        .is_some());
    assert_eq!(
        harness
            .state()
            .inspector
            .session
            .inspection()
            .unwrap()
            .captures[0]
            .captured_at,
        None
    );
    assert_eq!(
        harness
            .state()
            .inspector
            .session
            .inspection()
            .unwrap()
            .metadata()
            .unwrap()
            .published_at,
        None
    );
    assert!(harness.state().actions.is_empty());
}

fn capture(harness: &mut Harness<'_, Host>, directory: &std::path::Path, name: &str) {
    harness.remove_cursor();
    harness.run();
    let image = harness.render().expect("actual egui renderer required");
    image.save(directory.join(format!("{name}.png"))).unwrap();
    let labels: &[&str] = match name {
        "inspector-wide-light" | "inspector-narrow-dark" => &[
            "User override",
            "Unknown origin",
            "Observed 2026-10-23T08:00:00Z · Example publisher · Available",
        ],
        "inspector-provenance-wide-light" => &[
            "Created here 2026-10-23T08:00:00Z · LegacyMigration",
            "Membership Active · created here 2026-10-23T08:00:00Z",
        ],
        "inspector-edit-wide-light" | "inspector-edit-narrow-dark" => &[
            "Source: Source title: a field guide to collections",
            "Source: Publisher description retained in the immutable observation.",
        ],
        "inspector-error-narrow-dark" => &[
            "The owner could not find this record. Reload current data.",
            "Source: Source title: a field guide to collections",
            "Source: Publisher description retained in the immutable observation.",
        ],
        "inspector-conflict-wide-light" => &[
            "The saved data changed (item_revision). Your draft is still here.",
            "Source: Source title: a field guide to collections",
            "Source: Publisher description retained in the immutable observation.",
        ],
        _ => &[],
    };
    let background: [u8; 3] = image.get_pixel(0, 0).0[..3].try_into().unwrap();
    for label in labels {
        let rect = harness.get_by_label(label).rect();
        let mut core_count = 0;
        let mut strongest = (1.0_f64, background);
        for y in
            (rect.top().floor().max(0.0) as u32)..(rect.bottom().ceil() as u32).min(image.height())
        {
            for x in (rect.left().floor().max(0.0) as u32)
                ..(rect.right().ceil() as u32).min(image.width())
            {
                let foreground: [u8; 3] = image.get_pixel(x, y).0[..3].try_into().unwrap();
                let ratio = contrast_ratio(foreground, background);
                if ratio >= 4.5 {
                    core_count += 1;
                }
                if ratio > strongest.0 {
                    strongest = (ratio, foreground);
                }
            }
        }
        assert!(
            core_count >= (label.chars().filter(|value| !value.is_whitespace()).count() / 2).max(3),
            "{name}: {label:?} has insufficient rendered glyph cores: {core_count}, peak {}",
            strongest.0
        );
        println!(
            "rendered_contrast {}",
            serde_json::json!({"image":name,"label":label,"background":background,
            "glyph_core":strongest.1,"contrast":strongest.0,"core_pixels":core_count,
            "resolved_body":harness.ctx.style().visuals.text_color().to_array()})
        );
    }
}

fn contrast_ratio(foreground: [u8; 3], background: [u8; 3]) -> f64 {
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
    let a = luminance(foreground);
    let b = luminance(background);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

#[test]
fn render_actual_inspector_wide_narrow_and_failure_states() {
    let directory = std::env::var_os("HSSCRAP_GUI_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("hsscrap-gui-proof"));
    std::fs::create_dir_all(&directory).unwrap();
    let mut wide = harness(960.0, 1080.0);
    wide.ctx.set_visuals(egui::Visuals::light());
    wide.run();
    capture(&mut wide, &directory, "inspector-wide-light");
    wide.get_by_label("Link and capture provenance").click();
    wide.run();
    capture(&mut wide, &directory, "inspector-provenance-wide-light");
    wide.get_by_label("Link and capture provenance").click();
    wide.run();
    edit(&mut wide);
    change_title(&mut wide, "A useful local draft");
    capture(&mut wide, &directory, "inspector-edit-wide-light");
    wide.get_by_label("Save changes").click();
    wide.run();
    let ticket = wide.state().inspector.session.pending().unwrap().ticket;
    wide.state_mut().inspector.session.complete(
        ticket,
        Err(Error::Conflict {
            field: "item_revision".into(),
        }),
    );
    wide.run();
    capture(&mut wide, &directory, "inspector-conflict-wide-light");
    let mut narrow = harness(390.0, 844.0);
    narrow.ctx.set_visuals(egui::Visuals::dark());
    narrow.run();
    capture(&mut narrow, &directory, "inspector-narrow-dark");
    narrow.get_by_label("Link and capture provenance").click();
    narrow.run();
    capture(&mut narrow, &directory, "inspector-provenance-narrow-dark");
    narrow.get_by_label("Link and capture provenance").click();
    narrow.run();
    for rect in narrow.state().latest.controls.values() {
        assert!(rect.right() <= 390.0 && rect.left() >= 0.0);
    }
    edit(&mut narrow);
    change_title(&mut narrow, "Narrow draft");
    for (name, rect) in &narrow.state().latest.controls {
        assert!(
            rect.right() <= 390.0 && rect.left() >= 0.0,
            "{name}: {rect:?}"
        );
    }
    capture(&mut narrow, &directory, "inspector-edit-narrow-dark");
    let save = narrow.state().latest.controls["save"];
    assert!(save.bottom() <= 844.0 && save.height() >= 40.0);
    narrow.get_by_label("Save changes").click();
    narrow.run();
    let ticket = narrow.state().inspector.session.pending().unwrap().ticket;
    narrow
        .state_mut()
        .inspector
        .session
        .complete(ticket, Err(Error::NotFound));
    narrow.run();
    capture(&mut narrow, &directory, "inspector-error-narrow-dark");
    narrow.state_mut().inspector.session.revoke();
    narrow.run();
    capture(&mut narrow, &directory, "inspector-revoked-narrow-dark");
    println!("Actual GUI images: {}", directory.display());
}
