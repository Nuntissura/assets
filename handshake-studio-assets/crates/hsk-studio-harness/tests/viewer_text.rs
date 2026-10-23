//! SMB-HARNESS-UI-3 slice 4 (feature `viewer`): the text tool creates a Folio text layer through
//! chronicle (undoable), keeps its story and type styles in the document, shapes it with Type and
//! draws it through composite's text lowering; it survives save and reopen.
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd};
use hsk_studio_folio::Payload;
use hsk_studio_harness::view::edit::BrushSettings;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::Viewer;

fn frame(v: &mut Viewer) -> Vec<u8> {
    v.render_if_dirty();
    v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json())).rgba8.clone()
}

/// Pixels clearly darker than the white paper inside `rows` x `cols`.
fn ink(rgba: &[u8], width: usize, rows: std::ops::Range<usize>, cols: std::ops::Range<usize>) -> usize {
    let mut n = 0;
    for y in rows {
        for x in cols.clone() {
            if rgba[(y * width + x) * 4] < 128 {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn text_layer_is_created_drawn_undone_and_saved() {
    let scratch = Scratch::new("text");
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(96, 48, "paper", [255, 255, 255])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    let blank = frame(&mut v);
    assert_eq!(ink(&blank, 96, 0..48, 0..96), 0);

    v.set_brush(BrushSettings { rgb8: [0, 0, 0], ..BrushSettings::default() });
    v.text_size_pt = 24.0;
    let id = v.place_text(4.0, 34.0, "Hi").unwrap_or_else(|e| panic!("{e}"));
    let layer = v.run().snapshot().unwrap().document().layers.iter().find(|l| l.layer_id == id).unwrap().clone();
    assert_eq!(layer.kind, "text");
    assert!(matches!(&layer.payload, Payload::Text { story_id } if story_id.starts_with("STXT-")));
    assert_eq!(v.layers()[0].layer_id, id, "on top");
    let entry = v.text_entry().unwrap();
    assert_eq!(entry.layers.len(), 1);
    assert_eq!(entry.styles.styles.len(), 2, "paragraph + character style");
    let drawn = frame(&mut v);
    let inked = ink(&drawn, 96, 0..48, 0..96);
    assert!(inked > 40, "glyph ink: {inked} pixels");
    assert_eq!(ink(&drawn, 96, 0..48, 48..96), 0, "nothing right of the short word");
    assert_eq!(ink(&drawn, 96, 38..48, 0..96), 0, "nothing below the baseline (no descenders in Hi)");

    // Undo removes the layer (white again), redo brings it back with the same pixels.
    v.undo().unwrap();
    assert_eq!(frame(&mut v), blank);
    v.redo().unwrap();
    assert_eq!(frame(&mut v), drawn);

    // A text layer groups like any layer (image-typed source) and renders the same inside a group.
    let group = v.group_layers(std::slice::from_ref(&id), "Group").unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(frame(&mut v), drawn, "same pixels inside a normal isolated group");
    v.ungroup(&group).unwrap();

    // Save and reopen: the story, styles and drawing come back.
    let out = scratch.0.join("text.handshake");
    v.save(&out, false).unwrap();
    let mut again = Viewer::new(Prefer::Cpu);
    again.open(&out).unwrap();
    assert_eq!(again.text_entry().unwrap(), v.text_entry().unwrap());
    assert_eq!(frame(&mut again), drawn);

    // Typed refusals change nothing.
    let revision = v.run().snapshot().unwrap().document().revision;
    assert!(v.place_text(1.0, 1.0, "").is_err());
    assert!(v.place_text(1.0, 1.0, "two\nlines").is_err());
    assert!(v.place_text_with_font(1.0, 1.0, "x", "NoSuchFont").unwrap_err().starts_with("text_font_unavailable"));
    assert!(v.place_text(f64::NAN, 1.0, "x").is_err());
    assert_eq!(v.run().snapshot().unwrap().document().revision, revision);
}
