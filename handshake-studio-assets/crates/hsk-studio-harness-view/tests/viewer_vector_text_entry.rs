//! Lane SMB-R41-FOLLOWUPS-1, finding F-R41-5 (feature `viewer`): a document whose `text` entry
//! does not decode still draws its vector layers, and every live path / area text story reports
//! the decode error as the viewer's reason (`text_entry_unavailable: <decode error>`), not only
//! `text_entry_unavailable`. The corrupt entry is written through the harness session
//! (`Run::set_document_entry`) into a saved package that the viewer then reopens.
#![cfg(feature = "viewer")]

mod support_context;

#[path = "../../hsk-studio-harness/tests/common/mod.rs"]
mod common;

use common::{Scratch, solid_psd};
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::vector_text::TEXT_NOT_PROVIDED;
use hsk_studio_harness_core::harness::{Harness, References, Selection};
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::text;
use hsk_studio_harness_view::view::vector::{LiveTextKind, ShapeKind, VectorTool};

#[test]
fn corrupt_text_entry_names_its_decode_error() {
    let scratch = Scratch::new("vector-text-entry");
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(96, 64, "paper", [255, 255, 255])).unwrap();
    let mut v = crate::support_context::viewer(Prefer::Cpu);
    v.open(&source).unwrap();
    v.set_vector_tool(VectorTool { fill_on: false, stroke_on: false, stroke: [255, 255, 255], ..VectorTool::default() });
    let container = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [80.0, 54.0], false).unwrap();
    let story = v.bind_live_text(&container, "Area text", LiveTextKind::Area { inset_pt: 2.0 }).unwrap();
    assert!(v.vector_text_reports().unwrap()[0].unshaped.is_none(), "the story shapes before the entry is corrupted");
    let saved = scratch.0.join("live.handshake");
    v.save(&saved, false).unwrap();
    // Two decode errors of `text::TextEntry::decode`: bytes that are no JSON, and a foreign schema.
    let cases: [(&str, &[u8], &str); 2] = [("junk", b"{not json", "text_entry_json"), ("schema", br#"{"schema":"hsk.harness.text_layers@0"}"#, "text_entry_schema")];
    for (what, bytes, code) in cases {
        let harness = Harness::new(Selection { providers: vec![], references: References::TrustAll }).unwrap();
        let cancel = CancellationToken::default();
        let mut run = harness.run(crate::support_context::context(), &cancel);
        assert!(run.open_path(&saved), "{what}");
        run.set_document_entry(text::ENTRY, Some(bytes.to_vec())).unwrap();
        let corrupt = scratch.0.join(format!("corrupt-{what}.handshake"));
        assert!(run.save_to(&corrupt), "{what}");
        let mut w = crate::support_context::viewer(Prefer::Cpu);
        w.open(&corrupt).unwrap_or_else(|e| panic!("{what}: {e}"));
        let decode = w.text_entry().unwrap_err();
        assert!(decode.starts_with(code), "{what}: {decode}");
        // The layer still draws (typed report, no render error, no panic).
        w.render_if_dirty();
        assert!(w.render_error().is_none(), "{what}: {:?}", w.render_error());
        let reports = w.vector_text_reports().unwrap();
        assert_eq!(reports.len(), 1, "{what}: {reports:?}");
        assert_eq!(reports[0].report.text_id, story, "{what}");
        assert_eq!(reports[0].report.outcome, Err(TEXT_NOT_PROVIDED), "{what}");
        assert_eq!(reports[0].unshaped.as_deref(), Some(format!("text_entry_unavailable: {decode}").as_str()), "{what}");
        let live = w.state_json()["edit"]["vector"]["live_texts"][0].clone();
        assert_eq!((live["code"].as_str(), live["unshaped"].as_str()), (Some(TEXT_NOT_PROVIDED), Some(format!("text_entry_unavailable: {decode}").as_str())), "{what}: {live}");
    }
}
