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
