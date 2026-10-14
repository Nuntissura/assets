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

mod native_gestures {
    use super::*;
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
    use cabinet_egui::{CabinetView, ViewResponse};
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
        let mut view = cabinet_peak_egui::PeakView::default();
        let mut p = peak::Peak::default();
        p.hover(preview_key("a"), "Short", vec![], 0);
        p.tick(450);
        let render = |view: &mut cabinet_peak_egui::PeakView, p: &mut peak::Peak, time: f64| {
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
