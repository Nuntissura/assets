use cabinet::*;

fn fixture() -> Snapshot {
    let mut a = Entry::new("a", None, "A", EntryKind::Folder);
    a.order = 1;
    let mut b = Entry::new("b", None, "B", EntryKind::Folder);
    b.order = 2;
    Snapshot {
        context: Context {
            scope: "test-account-tree".into(),
            generation: 1,
            revision: 1,
        },
        hierarchy_complete: true,
        entries: vec![
            a,
            b,
            Entry::new("a1", Some("a".into()), "One", EntryKind::Leaf),
            Entry::new("a2", Some("a".into()), "Two", EntryKind::Leaf),
            Entry::new("b1", Some("b".into()), "One", EntryKind::Leaf),
            Entry::new("b2", Some("b".into()), "Two", EntryKind::Leaf),
        ],
    }
}
fn row_ids(c: &Cabinet) -> Vec<String> {
    c.rows().into_iter().map(|row| row.id).collect()
}

#[test]
fn global_default_changes_preserve_explicit_folder_overrides() {
    let mut c = Cabinet::new(fixture()).unwrap();
    c.set_folder_sort("a", Some(Sort::NameDescending)).unwrap();
    c.set_sort(Sort::NameAscending);
    assert_eq!(c.effective_sort(Some("a")), Sort::NameDescending);
    assert_eq!(c.effective_sort(Some("b")), Sort::NameAscending);
    assert_eq!(c.effective_sort(None), Sort::NameAscending);
    c.set_sort(Sort::ModifiedDescending);
    assert_eq!(c.effective_sort(Some("a")), Sort::NameDescending);
    assert_eq!(c.effective_sort(Some("b")), Sort::ModifiedDescending);
    c.set_folder_sort("a", None).unwrap();
    assert!(!c.settings().folder_sorts.contains_key("a"));
    assert_eq!(c.effective_sort(Some("a")), Sort::ModifiedDescending);
}

#[test]
fn each_folder_orders_its_own_children_independently() {
    let mut c = Cabinet::new(fixture()).unwrap();
    c.set_sort(Sort::NameAscending);
    c.set_folder_sort("a", Some(Sort::NameDescending)).unwrap();
    c.expand("a", true).unwrap();
    c.expand("b", true).unwrap();
    assert_eq!(row_ids(&c), vec!["a", "a2", "a1", "b", "b1", "b2"]);
    c.set_folder_sort("a", None).unwrap();
    assert_eq!(row_ids(&c), vec!["a", "a1", "a2", "b", "b1", "b2"]);
}

#[test]
fn sibling_drop_uses_destination_folder_effective_sort() {
    let mut c = Cabinet::new(fixture()).unwrap();
    c.set_sort(Sort::NameAscending);
    c.set_folder_sort("b", Some(Sort::Manual)).unwrap();
    assert!(
        c.validate_move(&["a1".into()], &Placement::Before("b1".into()))
            .is_ok(),
        "global sort must not block manual destination folder"
    );
    assert!(
        c.validate_move(&["b1".into()], &Placement::After("a1".into()))
            .is_err(),
        "source folder manual order must not permit sorted destination reorder"
    );
    assert!(c
        .validate_move(&["b1".into()], &Placement::Into(Some("a".into())))
        .is_ok());
}

#[test]
fn folder_override_rejects_leaf_or_missing_target_atomically() {
    let mut c = Cabinet::new(fixture()).unwrap();
    let before = c.settings().clone();
    assert!(c.set_folder_sort("a1", Some(Sort::NameDescending)).is_err());
    assert!(c
        .set_folder_sort("missing", Some(Sort::NameDescending))
        .is_err());
    assert_eq!(c.settings(), &before);
}

#[test]
fn acknowledged_preferences_survive_refresh_and_expire_on_context_change() {
    let mut c = Cabinet::new(fixture()).unwrap();
    c.set_sort(Sort::FileTypeDescending);
    c.set_folder_sort("a", Some(Sort::Manual)).unwrap();
    let before = c.settings().clone();
    let mut refreshed = fixture();
    refreshed.context.revision = 2;
    c.replace(refreshed).unwrap();
    assert_eq!(c.settings(), &before);
    let mut other = fixture();
    other.context.scope = "another-account".into();
    other.context.generation = 2;
    c.replace(other).unwrap();
    assert_eq!(c.settings(), &Settings::default());
}

#[cfg(feature = "serde")]
#[test]
fn persisted_settings_roundtrip_restarts_with_explicit_overrides() {
    let mut original = Cabinet::new(fixture()).unwrap();
    original.set_sort(Sort::FileSizeDescending);
    original
        .set_folder_sort("a", Some(Sort::NameDescending))
        .unwrap();
    let mut settings = original.settings().clone();
    settings.show_tags = false;
    settings.click_behavior = ClickBehavior::SelectDoubleOpen;
    settings.double_click_ms = 425;
    original.apply_settings(settings).unwrap();
    let encoded = serde_json::to_string(original.settings()).unwrap();
    let decoded: Settings = serde_json::from_str(&encoded).unwrap();
    let mut restarted = Cabinet::new(fixture()).unwrap();
    restarted.apply_settings(decoded).unwrap();
    assert_eq!(restarted.settings(), original.settings());
    assert_eq!(restarted.effective_sort(Some("a")), Sort::NameDescending);
    assert_eq!(
        restarted.effective_sort(Some("b")),
        Sort::FileSizeDescending
    );
    assert!(restarted.selected().is_empty());
    assert!(restarted.pending().is_none());
}

#[test]
fn size_sort_distinguishes_unknown_from_zero_without_reversing_unknown_last() {
    let mut s = fixture();
    let mut unknown = Entry::new("unknown", Some("a".into()), "Unknown", EntryKind::Leaf);
    unknown.size_bytes = None;
    s.entries.push(unknown);
    s.entries[2].size_bytes = Some(0);
    s.entries[3].size_bytes = Some(4096);
    let mut c = Cabinet::new(s).unwrap();
    c.expand("a", true).unwrap();
    c.set_sort(Sort::FileSizeAscending);
    assert_eq!(row_ids(&c), vec!["a", "a1", "a2", "unknown", "b"]);
    c.set_sort(Sort::FileSizeDescending);
    assert_eq!(row_ids(&c), vec!["a", "a2", "a1", "unknown", "b"]);
}

#[test]
fn type_sort_groups_case_insensitively_and_keeps_unknown_last() {
    let mut s = fixture();
    s.entries[2].file_type = "TXT".into();
    s.entries[3].file_type = "png".into();
    s.entries.push(Entry::new(
        "unknown",
        Some("a".into()),
        "Unknown",
        EntryKind::Leaf,
    ));
    let mut c = Cabinet::new(s).unwrap();
    c.expand("a", true).unwrap();
    c.set_sort(Sort::FileTypeAscending);
    assert_eq!(row_ids(&c), vec!["a", "a2", "a1", "unknown", "b"]);
    c.set_sort(Sort::FileTypeDescending);
    assert_eq!(row_ids(&c), vec!["a", "a1", "a2", "unknown", "b"]);
}

#[test]
fn capability_denial_blocks_new_operations_without_pending_or_canonical_mutation() {
    let ops = [
        Operation::Duplicate { id: "a1".into() },
        Operation::Download { id: "a1".into() },
        Operation::OpenParent { id: "a1".into() },
        Operation::CopyPath { id: "a1".into() },
        Operation::CopyLink { id: "a1".into() },
        Operation::Properties { id: "a1".into() },
        Operation::SetColor {
            id: "a1".into(),
            color: Some([0, 64, 255]),
        },
        Operation::SetTags {
            id: "a1".into(),
            tags: vec![],
        },
    ];
    for operation in ops {
        let mut s = fixture();
        let capabilities = &mut s.entries[2].capabilities;
        capabilities.duplicate = false;
        capabilities.download = false;
        capabilities.open_parent = false;
        capabilities.copy_reference = false;
        capabilities.properties = false;
        capabilities.color = false;
        capabilities.tag = false;
        let mut c = Cabinet::new(s.clone()).unwrap();
        assert!(c.request(operation).is_err());
        assert!(c.pending().is_none());
        assert_eq!(c.snapshot(), &s);
    }
}

#[test]
fn read_only_acknowledgments_never_fabricate_compensation_history() {
    for operation in [
        Operation::Download { id: "a1".into() },
        Operation::CopyPath { id: "a1".into() },
        Operation::CopyLink { id: "a1".into() },
        Operation::Properties { id: "a1".into() },
        Operation::OpenParent { id: "a1".into() },
    ] {
        let mut c = Cabinet::new(fixture()).unwrap();
        let req = c.request(operation).unwrap();
        c.complete(Completion {
            request_id: req.id,
            context: req.context,
            result: Ok(fixture()),
            undo_token: Some("host-stray-token".into()),
        })
        .unwrap();
        assert!(!c.can_undo());
        assert!(!c.can_redo());
    }
}

fn tag(id: &str, label: &str) -> Tag {
    Tag {
        id: id.into(),
        label: label.into(),
        color: [128, 64, 255],
    }
}

#[test]
fn invalid_tags_are_rejected_before_request_and_snapshot_admission() {
    for tags in [
        vec![tag("", "Good")],
        vec![tag("ok", "  ")],
        vec![tag("id\n", "Good")],
        vec![tag("ok", "Bad\tlabel")],
        vec![tag("same", "One"), tag("same", "Two")],
    ] {
        let mut c = Cabinet::new(fixture()).unwrap();
        let before = c.snapshot().clone();
        assert!(c
            .request(Operation::SetTags {
                id: "a1".into(),
                tags: tags.clone()
            })
            .is_err());
        assert!(c.pending().is_none());
        assert_eq!(c.snapshot(), &before);
        let mut invalid = fixture();
        invalid.context.revision = 2;
        invalid.entries[2].revision = 1;
        invalid.entries[2].tags = tags;
        assert!(c.replace(invalid).is_err());
        assert_eq!(c.snapshot(), &before);
    }
}

#[test]
fn distinct_tag_ids_can_share_labels_without_losing_identity() {
    let tags = vec![tag("id-a", "Shared label"), tag("id-b", "Shared label")];
    let mut c = Cabinet::new(fixture()).unwrap();
    let req = c
        .request(Operation::SetTags {
            id: "a1".into(),
            tags: tags.clone(),
        })
        .unwrap();
    assert_eq!(
        c.entry("a1").unwrap().tags,
        vec![],
        "tag intentions must not impersonate persisted state"
    );
    let mut canonical = fixture();
    canonical.context.revision = 2;
    canonical.entries[2].revision = 1;
    canonical.entries[2].tags = tags.clone();
    c.complete(Completion {
        request_id: req.id,
        context: req.context,
        result: Ok(canonical),
        undo_token: Some("undo-tags".into()),
    })
    .unwrap();
    assert_eq!(c.entry("a1").unwrap().tags, tags);
    assert!(c.can_undo());
}

#[test]
fn failed_tag_or_color_write_preserves_prior_canonical_value() {
    for operation in [
        Operation::SetTags {
            id: "a1".into(),
            tags: vec![tag("new", "New")],
        },
        Operation::SetColor {
            id: "a1".into(),
            color: Some([0, 0, 255]),
        },
    ] {
        let mut s = fixture();
        s.entries[2].tags = vec![tag("old", "Original")];
        s.entries[2].color = Some([255, 0, 0]);
        let mut c = Cabinet::new(s.clone()).unwrap();
        let req = c.request(operation).unwrap();
        c.complete(Completion {
            request_id: req.id,
            context: req.context,
            result: Err("host write failed".into()),
            undo_token: None,
        })
        .unwrap();
        assert_eq!(c.snapshot(), &s);
        assert!(!c.can_undo());
        assert!(c.error().is_some());
    }
}

#[test]
fn malformed_host_settings_never_partially_replace_acknowledged_preferences() {
    let mut c = Cabinet::new(fixture()).unwrap();
    c.set_folder_sort("a", Some(Sort::NameDescending)).unwrap();
    let before = c.settings().clone();
    let mut invalid_timer = before.clone();
    invalid_timer.double_click_ms = 0;
    let mut invalid_dimensions = before.clone();
    invalid_dimensions.peak.width = f32::NAN;
    let mut invalid_budget = before.clone();
    invalid_budget.peak.max_input_bytes = 0;
    let mut invalid_override = before.clone();
    invalid_override
        .folder_sorts
        .insert("a1".into(), Sort::Manual);
    for invalid in [
        invalid_timer,
        invalid_dimensions,
        invalid_budget,
        invalid_override,
    ] {
        assert!(c.apply_settings(invalid).is_err());
        assert_eq!(c.settings(), &before);
    }
}

fn preview_key(id: &str) -> peak::Key {
    peak::Key {
        scope: "account-a".into(),
        generation: 1,
        context_revision: 1,
        entry_id: id.into(),
        resource_id: "shared-resource".into(),
        entry_revision: 1,
    }
}
fn preview_request(p: &mut peak::Peak, key: peak::Key) -> peak::PreviewRequest {
    p.hover(key, "Preview entry", vec![], 0);
    assert!(
        p.tick(450).is_none(),
        "compact title-only phase must not request content"
    );
    p.tick(1650).expect("expanded phase requests content")
}

#[test]
fn late_preview_cannot_cross_occurrence_resource_scope_or_revision_fences() {
    let original = preview_key("occurrence-a");
    let mut variants = Vec::new();
    let mut occurrence = original.clone();
    occurrence.entry_id = "occurrence-b".into();
    variants.push(occurrence);
    let mut scope = original.clone();
    scope.scope = "account-b".into();
    variants.push(scope);
    let mut generation = original.clone();
    generation.generation = 2;
    variants.push(generation);
    let mut revision = original.clone();
    revision.context_revision = 2;
    variants.push(revision);
    let mut entry_revision = original.clone();
    entry_revision.entry_revision = 2;
    variants.push(entry_revision);
    let mut resource = original.clone();
    resource.resource_id = "another-resource".into();
    variants.push(resource);
    for next in variants {
        let mut p = peak::Peak::default();
        let old = preview_request(&mut p, original.clone());
        p.hover(next.clone(), "Next", vec![], 1651);
        assert!(!p.complete(peak::PreviewCompletion {
            request_id: old.id,
            key: old.key,
            content: peak::Content::Text {
                body: "private old contents".into(),
                truncated: false
            }
        }));
        assert_eq!(p.key(), Some(&next));
        assert!(p.content().is_none());
        let current = p.tick(4000).unwrap();
        assert!(p.complete(peak::PreviewCompletion {
            request_id: current.id,
            key: current.key,
            content: peak::Content::Text {
                body: "current".into(),
                truncated: false
            }
        }));
        assert!(matches!(p.content(), Some(peak::Content::Text { body, .. }) if body == "current"));
    }
}

#[test]
fn changed_preview_budget_cannot_relax_original_request_or_bypass_new_limit() {
    for shrink in [false, true] {
        let mut p = peak::Peak::default();
        p.settings.max_text_lines = if shrink { 3 } else { 1 };
        let request = preview_request(&mut p, preview_key("a"));
        p.settings.max_text_lines = if shrink { 1 } else { 3 };
        assert!(p.complete(peak::PreviewCompletion {
            request_id: request.id,
            key: request.key,
            content: peak::Content::Text {
                body: "one\ntwo".into(),
                truncated: false
            }
        }));
        assert!(
            matches!(p.content(), Some(peak::Content::Failed { .. })),
            "completion must honor the tighter of issued request and current settings"
        );
    }
}

#[test]
fn escape_dismissal_suppresses_stationary_hover_and_rejects_pending_result() {
    let mut p = peak::Peak::default();
    let key = preview_key("a");
    let old = preview_request(&mut p, key.clone());
    p.dismiss();
    p.hover(key.clone(), "Still hovered", vec![], 1700);
    assert!(p.tick(5000).is_none());
    assert_eq!(p.stage(), peak::Stage::Hidden);
    assert!(!p.complete(peak::PreviewCompletion {
        request_id: old.id,
        key: old.key,
        content: peak::Content::Text {
            body: "late".into(),
            truncated: false
        }
    }));
    p.leave(5001);
    p.hover(key, "Returned", vec![], 5002);
    assert!(
        p.tick(7000).is_some(),
        "a genuine leave and reenter permits a new preview"
    );
}

#[test]
fn retained_popup_survives_slow_frame_before_grace_expiry_tick() {
    let mut p = peak::Peak::default();
    preview_request(&mut p, preview_key("a"));
    p.leave(1700);
    p.retain();
    p.tick(2500);
    assert_eq!(
        p.stage(),
        peak::Stage::Expanded,
        "popup hover must be retained before elapsed grace is evaluated"
    );
    p.leave(2501);
    p.tick(3000);
    assert_eq!(p.stage(), peak::Stage::Hidden);
}

#[test]
fn disabling_peak_rejects_outstanding_content_and_erases_private_state() {
    let mut p = peak::Peak::default();
    let request = preview_request(&mut p, preview_key("a"));
    p.settings.enabled = false;
    assert!(!p.complete(peak::PreviewCompletion {
        request_id: request.id,
        key: request.key,
        content: peak::Content::Text {
            body: "secret".into(),
            truncated: false
        }
    }));
    p.tick(2000);
    assert_eq!(p.stage(), peak::Stage::Hidden);
    assert!(p.key().is_none());
    assert!(p.content().is_none());
    assert!(p.pending().is_none());
}

#[test]
fn repeated_dismissal_does_not_remove_stationary_hover_suppression() {
    let mut p = peak::Peak::default();
    let key = preview_key("a");
    preview_request(&mut p, key.clone());
    p.dismiss();
    p.dismiss();
    p.hover(key, "Same hovered entry", vec![], 2000);
    assert!(p.tick(5000).is_none());
    assert_eq!(
        p.stage(),
        peak::Stage::Hidden,
        "Escape dismissal should be idempotent until a real leave"
    );
}

#[test]
fn text_decoder_preserves_unicode_prefix_and_reports_budget_truncation() {
    let mut settings = peak::Settings::default();
    settings.max_input_bytes = 4;
    assert_eq!(
        peak::decode_text("a😀".as_bytes(), &settings, false),
        peak::Content::Text {
            body: "a".into(),
            truncated: true
        }
    );
    settings.max_input_bytes = 6;
    // UTF-16LE BOM, A, then an incomplete high surrogate caused by the prefix budget.
    assert_eq!(
        peak::decode_text(&[0xff, 0xfe, 65, 0, 0x3d, 0xd8, 0, 0xde], &settings, false),
        peak::Content::Text {
            body: "A".into(),
            truncated: true
        }
    );
}

#[cfg(feature = "egui")]
mod native_gestures {
    use super::*;
    use cabinet::egui_view::{CabinetView, ViewResponse};
    use egui::{Context as GuiContext, Event, PointerButton, Pos2, RawInput, Rect, Vec2};
    fn frame(
        ctx: &GuiContext,
        view: &mut CabinetView,
        c: &mut Cabinet,
        time: f64,
        events: Vec<Event>,
    ) -> ViewResponse {
        let mut out = None;
        let _ = ctx.run(
            RawInput {
                time: Some(time),
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(680.0, 600.0))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    out = Some(view.show(ui, c));
                });
            },
        );
        out.unwrap()
    }
    fn click(
        ctx: &GuiContext,
        view: &mut CabinetView,
        c: &mut Cabinet,
        time: f64,
        pos: Pos2,
    ) -> ViewResponse {
        frame(
            ctx,
            view,
            c,
            time,
            vec![
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
        );
        frame(
            ctx,
            view,
            c,
            time + 0.01,
            vec![Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        )
    }
    #[test]
    fn timed_single_click_opens_once_after_double_click_window() {
        let ctx = GuiContext::default();
        let mut view = CabinetView::default();
        let mut c = Cabinet::new(fixture()).unwrap();
        c.expand("a", true).unwrap();
        let layout = frame(&ctx, &mut view, &mut c, 0.0, vec![]);
        let clicked = click(&ctx, &mut view, &mut c, 0.02, layout.rows["a1"].center());
        assert!(
            clicked.requests.is_empty(),
            "first click must leave room for double rename"
        );
        let before_deadline = frame(&ctx, &mut view, &mut c, 0.15, vec![]);
        assert!(before_deadline.requests.is_empty());
        let opened = frame(&ctx, &mut view, &mut c, 0.5, vec![]);
        assert_eq!(opened.requests.len(), 1);
        assert_eq!(
            opened.requests[0].operation,
            Operation::Open { id: "a1".into() }
        );
        assert!(
            frame(&ctx, &mut view, &mut c, 0.9, vec![])
                .requests
                .is_empty(),
            "settled click may never be redispatched"
        );
    }
    #[test]
    fn timed_double_click_enters_rename_without_scheduled_open() {
        let ctx = GuiContext::default();
        let mut view = CabinetView::default();
        let mut c = Cabinet::new(fixture()).unwrap();
        c.expand("a", true).unwrap();
        let layout = frame(&ctx, &mut view, &mut c, 0.0, vec![]);
        let pos = layout.rows["a1"].center();
        assert!(click(&ctx, &mut view, &mut c, 0.02, pos)
            .requests
            .is_empty());
        assert!(click(&ctx, &mut view, &mut c, 0.12, pos)
            .requests
            .is_empty());
        let editing = frame(&ctx, &mut view, &mut c, 0.16, vec![]);
        assert!(
            editing.controls.contains_key("inline-edit"),
            "double click mounts a real input rather than opening the resource"
        );
        let settled = frame(&ctx, &mut view, &mut c, 0.6, vec![]);
        assert!(settled.requests.is_empty());
        assert!(c.pending().is_none());
    }

    #[test]
    fn ellipsis_download_targets_exact_row_without_open_or_canonical_mutation() {
        let ctx = GuiContext::default();
        let mut view = CabinetView::default();
        let mut c = Cabinet::new(fixture()).unwrap();
        c.expand("a", true).unwrap();
        let before = c.snapshot().clone();
        let layout = frame(&ctx, &mut view, &mut c, 0.0, vec![]);
        let menu_open = click(
            &ctx,
            &mut view,
            &mut c,
            0.02,
            layout.controls["options:a1"].center(),
        );
        assert!(menu_open.requests.is_empty());
        let menu = frame(&ctx, &mut view, &mut c, 0.07, vec![]);
        assert!(
            menu.controls.contains_key("menu-download:a1"),
            "real ellipsis menu exposes exact row Download action"
        );
        let downloaded = click(
            &ctx,
            &mut view,
            &mut c,
            0.1,
            menu.controls["menu-download:a1"].center(),
        );
        assert_eq!(downloaded.requests.len(), 1);
        assert_eq!(
            downloaded.requests[0].operation,
            Operation::Download { id: "a1".into() }
        );
        assert_eq!(c.snapshot(), &before);
        assert!(
            frame(&ctx, &mut view, &mut c, 0.6, vec![])
                .requests
                .is_empty(),
            "menu activation must not leave a scheduled row Open"
        );
    }

    #[test]
    fn denied_download_menu_does_not_dispatch() {
        let ctx = GuiContext::default();
        let mut view = CabinetView::default();
        let mut s = fixture();
        s.entries[2].capabilities.download = false;
        let mut c = Cabinet::new(s.clone()).unwrap();
        c.expand("a", true).unwrap();
        let layout = frame(&ctx, &mut view, &mut c, 0.0, vec![]);
        click(
            &ctx,
            &mut view,
            &mut c,
            0.02,
            layout.controls["options:a1"].center(),
        );
        let menu = frame(&ctx, &mut view, &mut c, 0.07, vec![]);
        let denied = click(
            &ctx,
            &mut view,
            &mut c,
            0.1,
            menu.controls["menu-download:a1"].center(),
        );
        assert!(denied.requests.is_empty());
        assert!(c.pending().is_none());
        assert_eq!(c.snapshot(), &s);
    }

    #[test]
    fn color_wheel_and_hex_require_explicit_apply_to_exact_row() {
        fn open_color(ctx: &GuiContext, view: &mut CabinetView, c: &mut Cabinet) -> ViewResponse {
            c.expand("a", true).unwrap();
            let layout = frame(ctx, view, c, 0.0, vec![]);
            click(ctx, view, c, 0.02, layout.controls["options:a1"].center());
            let menu = frame(ctx, view, c, 0.07, vec![]);
            assert!(
                click(ctx, view, c, 0.1, menu.controls["menu-color:a1"].center())
                    .requests
                    .is_empty()
            );
            frame(ctx, view, c, 0.15, vec![])
        }
        fn replace_hex(
            ctx: &GuiContext,
            view: &mut CabinetView,
            c: &mut Cabinet,
            time: f64,
            value: &str,
        ) -> ViewResponse {
            let command = egui::Modifiers {
                ctrl: true,
                command: true,
                ..Default::default()
            };
            frame(
                ctx,
                view,
                c,
                time,
                vec![
                    Event::Key {
                        key: egui::Key::A,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: command,
                    },
                    Event::Text(value.into()),
                    Event::Key {
                        key: egui::Key::A,
                        physical_key: None,
                        pressed: false,
                        repeat: false,
                        modifiers: command,
                    },
                ],
            )
        }
        let ctx = GuiContext::default();
        let mut view = CabinetView::default();
        let mut c = Cabinet::new(fixture()).unwrap();
        let before = c.snapshot().clone();
        let dialog = open_color(&ctx, &mut view, &mut c);
        let wheel = dialog.controls["color-wheel"];
        let picked = click(
            &ctx,
            &mut view,
            &mut c,
            0.2,
            wheel.center() + Vec2::new(50.0, 0.0),
        );
        assert!(picked.requests.is_empty());
        assert_eq!(
            c.snapshot(),
            &before,
            "draft wheel movement must not mutate canonical state"
        );
        assert!(c.pending().is_none());
        let settled = frame(&ctx, &mut view, &mut c, 0.25, vec![]);
        let applied = click(
            &ctx,
            &mut view,
            &mut c,
            0.3,
            settled.controls["apply-color"].center(),
        );
        assert_eq!(
            applied.requests.len(),
            1,
            "Apply emits exactly one host command"
        );
        match &applied.requests[0].operation {
            Operation::SetColor {
                id,
                color: Some(rgb),
            } => {
                assert_eq!(id, "a1");
                assert_ne!(
                    *rgb,
                    [110, 150, 230],
                    "real wheel input changes the draft color"
                );
            }
            other => panic!("unexpected wheel command: {other:?}"),
        }
        assert_eq!(
            c.snapshot(),
            &before,
            "even Apply waits for host acknowledgement"
        );

        // A fresh host instance isolates the hex path from the first outstanding write.
        let ctx = GuiContext::default();
        let mut view = CabinetView::default();
        let mut c = Cabinet::new(fixture()).unwrap();
        let dialog = open_color(&ctx, &mut view, &mut c);
        click(
            &ctx,
            &mut view,
            &mut c,
            0.2,
            dialog.controls["color-hex"].center(),
        );
        let invalid = replace_hex(&ctx, &mut view, &mut c, 0.25, "#GG0000");
        assert!(invalid.requests.is_empty());
        let denied = click(
            &ctx,
            &mut view,
            &mut c,
            0.3,
            invalid.controls["apply-color"].center(),
        );
        assert!(denied.requests.is_empty(), "invalid hex disables Apply");
        assert!(c.pending().is_none());
        assert_eq!(c.snapshot(), &before);
        let dialog = frame(&ctx, &mut view, &mut c, 0.35, vec![]);
        click(
            &ctx,
            &mut view,
            &mut c,
            0.4,
            dialog.controls["color-hex"].center(),
        );
        let valid = replace_hex(&ctx, &mut view, &mut c, 0.45, "#ABCDEF");
        assert!(valid.requests.is_empty());
        let applied = click(
            &ctx,
            &mut view,
            &mut c,
            0.5,
            valid.controls["apply-color"].center(),
        );
        assert_eq!(applied.requests.len(), 1);
        assert_eq!(
            applied.requests[0].operation,
            Operation::SetColor {
                id: "a1".into(),
                color: Some([0xAB, 0xCD, 0xEF])
            }
        );
        assert_eq!(c.snapshot(), &before);
    }

    #[test]
    fn timed_preview_expansion_applies_configured_window_geometry() {
        let ctx = GuiContext::default();
        let mut view = peak::egui_view::PeakView::default();
        let mut p = peak::Peak::default();
        p.hover(preview_key("a"), "Short", vec![], 0);
        p.tick(450);
        let render = |view: &mut peak::egui_view::PeakView, p: &mut peak::Peak, time: f64| {
            let mut rect = None;
            let _ = ctx.run(
                RawInput {
                    time: Some(time),
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0))),
                    ..Default::default()
                },
                |ctx| {
                    rect = view.show(
                        ctx,
                        p,
                        Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(80.0, 24.0)),
                        (time * 1000.0) as u64,
                    );
                },
            );
            rect.expect("preview window is visible")
        };
        render(&mut view, &mut p, 0.45);
        let compact = render(&mut view, &mut p, 0.5);
        p.tick(1700);
        assert_eq!(p.stage(), peak::Stage::Expanded);
        render(&mut view, &mut p, 1.7);
        let expanded = render(&mut view, &mut p, 1.75);
        assert!(expanded.width() > compact.width() + 50.0, "compact-to-expanded transition must resize the actual persistent egui window, not only change default_size after initialization");
    }
}
