#![cfg(feature = "drops")]
//! Memory-only sample construction; no native window, transfer or input effects.
use std::collections::BTreeSet;
use kumihimo_egui::demo;

#[test]
fn file_drop_sample_has_two_distinct_editable_destinations() {
    let demo = demo::file_drop_sample().expect("manual file-drop sample construction");
    let snapshot = demo.controller.snapshot();
    let editable: Vec<_> = snapshot.views.values()
        .filter(|view| demo.host.document_text(view.resource).is_some())
        .collect();
    let frames: BTreeSet<_> = editable.iter().map(|view| view.frame).collect();
    let resources: BTreeSet<_> = editable.iter().map(|view| view.resource).collect();
    assert_eq!(frames.len(), 2);
    assert_eq!(resources.len(), 4);
    assert!(editable.iter().all(|view| view.closed.is_none()));
    assert!(snapshot.floats.is_empty());
    for frame in frames {
        let selected = snapshot.frames[&frame].selected_view.expect("visible document tab");
        assert!(demo.host.document_text(snapshot.views[&selected].resource).is_some());
    }
}

#[test]
fn default_demo_retains_its_document_and_canvas_providers() {
    let demo = demo::new().expect("default sample construction");
    let snapshot = demo.controller.snapshot();
    assert!(demo.host.document_text(snapshot.views[&demo.text_view].resource).is_some());
    assert!(demo.host.document_text(snapshot.views[&demo.canvas_view].resource).is_none());
}

#[test]
fn file_drop_destinations_and_document_state_survive_caller_theme_changes() {
    use kumihimo_egui::{chrome::ThemeTokens, DesktopAdapter, SurfaceBinding, to_egui};
    let mut demo = demo::file_drop_sample().unwrap();
    let mut adapter = DesktopAdapter::default();
    let ctx = egui::Context::default();
    let binding = SurfaceBinding {
        root: demo.root,
        viewport: egui::ViewportId::ROOT,
        epoch: 1,
        bounds: kumihimo_core::Rect::new(0.0, 0.0, 960.0, 640.0).unwrap(),
        pixels_per_point: 1.0,
    };
    let render = |demo: &mut demo::Demo, adapter: &mut DesktopAdapter| {
        ctx.run(egui::RawInput {
            screen_rect: Some(to_egui(binding.bounds)),
            ..Default::default()
        }, |ctx| {
            adapter.show(ctx, &mut demo.controller, binding, &mut demo.host).unwrap();
        })
    };
    for _ in 0..3 {
        render(&mut demo, &mut adapter);
    }
    let snapshot = demo.controller.snapshot().clone();
    let documents = snapshot.resources.keys().filter_map(|resource| {
        demo.host.document_text(*resource).map(|text| (*resource, text.to_owned()))
    }).collect::<Vec<_>>();
    for visuals in [egui::Visuals::dark(), egui::Visuals::light()] {
        let mut tokens = ThemeTokens::from_visuals(&visuals);
        tokens.divider_stroke = egui::Color32::from_rgb(29, 181, 211);
        ctx.set_visuals(visuals);
        demo.host.policy.theme_tokens = Some(tokens);
        let output = render(&mut demo, &mut adapter);
        assert!(!output.shapes.is_empty());
        assert_eq!(demo.controller.snapshot(), &snapshot);
        for (resource, text) in &documents {
            assert_eq!(demo.host.document_text(*resource), Some(text.as_str()));
        }
        assert!(adapter.input.active_capture().is_none());
    }
}
