//! Lane SMB-LAYOUT-UI-2 slice 2: thread references, in/out ports and the story editor's line list
//! (STU-LAY-122, STU-LAY-026), headless (fixture in `layout_ui_fixture`: 21 glyphs fit a 130 pt
//! line, so frame 1 holds bytes 0..40 on baselines 10 and 25 and 40..49 is overset).
//! InDesign port/story-editor behaviour UNVERIFIED (D5).
#![cfg(feature = "layout-ui")]
mod layout_ui_fixture;
use hsk_studio_harness::layout_ui::{LayoutEdit, LayoutEditor, Port, threads::colour, widget};
use layout_ui_fixture::{TEXT, doc, id, rect_colours, rects_of, ui_frames};

/// (frame, depth, start, end, overset) of a story-editor line.
type Row = (Option<usize>, Option<f64>, u64, u64, bool);

#[test]
fn thread_references_ports_and_story_lines_follow_the_composition() {
    let mut ed = LayoutEditor::new(doc());
    let t = ed.thread_info(&id("SLYR", 1)).unwrap();
    assert_eq!((t.in_port, t.out_port, t.overflows, t.text_frame_index, t.source), (Port::Empty, Port::Overset, true, 0, Some((0, 40))));
    assert_eq!((t.start_text_frame.as_str(), t.end_text_frame.as_str()), (id("SLYR", 1).as_str(), id("SLYR", 1).as_str()));
    // Story editor: two composed lines with frame, column and depth, then the overset row.
    let lines = ed.story_lines(&id("STXT", 1)).unwrap();
    let rows: Vec<Row> = lines.iter().map(|l| (l.frame, l.depth_pt, l.start, l.end, l.overset)).collect();
    assert_eq!(rows, vec![(Some(0), Some(10.0), 0, 20), (Some(0), Some(25.0), 20, 40), (None, None, 40, 49)].into_iter().map(|(f, d, s, e)| (f, d, s, e, f.is_none())).collect::<Vec<_>>());
    assert_eq!(lines[2].text, &TEXT[40..]);

    // Thread into a frame on page 2: frame 1's out port is threaded, the story fits, frame 2 is index 1.
    ed.apply(LayoutEdit::DrawFrame { page_id: "page-2".into(), x: 10.0, y: 10.0, w: 60.0, h: 30.0 }).unwrap();
    let f2 = ed.selected.clone().unwrap();
    ed.apply(LayoutEdit::Thread { from: id("SLYR", 1), to: f2.clone() }).unwrap();
    let a = ed.thread_info(&id("SLYR", 1)).unwrap();
    let b = ed.thread_info(&f2).unwrap();
    assert_eq!((a.out_port, a.overflows, a.next_text_frame.as_deref()), (Port::Threaded, false, Some(f2.as_str())));
    assert_eq!((b.in_port, b.out_port, b.text_frame_index, b.source), (Port::Threaded, Port::Empty, 1, Some((40, 49))));
    assert_eq!((b.start_text_frame.clone(), b.end_text_frame.clone(), b.parent_story.clone()), (id("SLYR", 1), f2.clone(), id("STXT", 1)));
    assert!(ed.story_lines(&id("STXT", 1)).unwrap().iter().all(|l| !l.overset));
    assert_eq!(ed.state_json()["frames"][1]["thread"]["text_frame_index"], 1);
    // Unknown frames are typed.
    assert_eq!(ed.thread_info("nope").unwrap_err().code(), "unknown_frame");
}

#[test]
fn one_ui_frame_draws_ports_overset_marker_and_thread_ids() {
    let mut ed = LayoutEditor::new(doc());
    ed.selected = Some(id("SLYR", 1));
    let (drawn, shapes) = ui_frames(&mut ed);
    for w in [widget::SHOW_THREADS, widget::THREAD_INFO, widget::STORY_LINES, widget::STORY_OVERSET] {
        assert!(drawn.contains(&w), "{w} drawn");
    }
    // One frame: an empty in port and the red overset out port.
    assert_eq!(rects_of(&shapes, colour::PORT_OVERSET, true), 1, "{:?}", rect_colours(&shapes));
    assert_eq!(rects_of(&shapes, colour::PORT_EMPTY, true), 1);
    assert_eq!(ed.state_json()["frames"][0]["thread"]["out_port"], "overset");
}
