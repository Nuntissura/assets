//! Lane SMB-TYPE-CHARACTER-ATTRIBUTES-1, review FIX_FIRST of 83cb89b8 (research/review-smb-type-character-
//! attributes-1-83cb89b8.json) and the lead rulings on it (feature `viewer`): every test drives the typed
//! `layout.*` rows by id and reads the result back independently (press's vector reader, the stored
//! layout records, the read row).
#![cfg(feature = "viewer")]
mod common;
use common::Scratch;
use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness_core::document_new::{DocumentProfile, NewDocument};
use hsk_studio_press::{VectorItem, read_vector_document};
use serde_json::{Map, Value, json};
use std::path::Path;

fn params(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("parameters must be an object: {other}"),
    }
}
fn cmd(v: &mut Viewer, id: &str, p: Value) -> Result<Value, (String, Value)> {
    v.run_command(id, &params(p)).map_err(|r| (r.code, r.detail))
}
fn ok(v: &mut Viewer, id: &str, p: Value) -> Value {
    cmd(v, id, p).unwrap_or_else(|(code, detail)| panic!("{id}: {code} {detail}"))
}
fn refused(v: &mut Viewer, id: &str, p: Value) -> String {
    match cmd(v, id, p) {
        Ok(r) => panic!("{id} should refuse: {r}"),
        Err((code, _)) => code,
    }
}
fn undo_labels(v: &Viewer) -> Vec<String> {
    serde_json::from_value(v.run().history()["undo"].clone()).unwrap()
}
fn records(v: &Viewer) -> Value {
    v.run().document_entry("layout_records").map(|b| serde_json::from_slice(b).unwrap()).unwrap_or(Value::Null)
}
fn overrides_of(v: &Viewer, story: &str) -> usize {
    records(v)["styles"]["character_overrides"].as_array().map_or(0, |a| a.iter().filter(|o| o["text_id"] == story).count())
}

/// A layout document of `pages` pages (595 x 842 pt) with one 300 pt wide frame on page 1 holding `text`.
fn layout_with(pages: usize, text: &str) -> (Viewer, String) {
    let mut v = Viewer::new(Prefer::Cpu);
    v.new_document(&NewDocument::new(DocumentProfile::Layout, Some((595.0, 842.0))), None).unwrap();
    for _ in 1..pages {
        v.add_page(Some("page-1"), false, None, None).unwrap();
    }
    ok(&mut v, "layout.draw_frame", json!({"page_id": "page-1", "x": 40, "y": 40, "w": 300, "h": 600}));
    let story = v.layout_editor().unwrap().state_json()["frames"][0]["story_id"].as_str().expect("the frame's story").to_owned();
    if !text.is_empty() {
        ok(&mut v, "layout.replace_text", json!({"story": story, "start": 0, "end": 0, "text": text}));
    }
    (v, story)
}
fn new_style(v: &mut Viewer, scope: &str, name: &str) -> String {
    ok(v, "layout.style.new_style", json!({"scope": scope, "name": name}));
    v.layout_editor().unwrap().doc().styles.styles.iter().find(|s| s.name == name).unwrap().style_id.clone()
}
fn read(v: &mut Viewer, story: &str, start: u64, end: u64) -> Value {
    ok(v, "layout.character_attributes", json!({"story": story, "start": start, "end": end}))
}
fn export(v: &mut Viewer, pdf: &Path) -> Value {
    ok(v, "layout.export_pdf", json!({"path": pdf.to_str().unwrap(), "overwrite": true}))
}
/// Exports the layout to `pdf` and reads its drawn glyphs back.
fn drawn(v: &mut Viewer, pdf: &Path) -> Vec<G> {
    export(v, pdf);
    glyphs(pdf)
}
fn host_loss(r: &Value, code: &str) -> Vec<(String, u64)> {
    r["host_loss"].as_array().unwrap().iter().filter(|l| l["code"] == code).map(|l| (l["element"].as_str().unwrap().to_owned(), l["count"].as_u64().unwrap())).collect()
}
/// One drawn glyph: id, x, baseline y, size (pt).
#[derive(Clone, Copy, Debug)]
struct G {
    id: u32,
    x: f64,
    y: f64,
    size: f64,
}
fn glyphs(pdf: &Path) -> Vec<G> {
    let bytes = std::fs::read(pdf).unwrap();
    let doc = read_vector_document(&bytes, 1 << 26, &CancellationToken::default()).unwrap();
    doc.pages[0]
        .items
        .iter()
        .filter_map(|i| match i {
            VectorItem::Draw { glyph: Some(g), .. } => Some(G { id: g.glyph_id, x: g.matrix[4], y: g.matrix[5], size: g.matrix[3] * 1000.0 }),
            _ => None,
        })
        .collect()
}

/// F1 + Q1: a successful command never leaves the export refused. Attributes Type does not read never cut a
/// span, spans with equal shaping inputs merge, a span carries only its effective features, and a paragraph
/// past Type's request limits is shaped in chunks at span edges (Type's limits are not raised).
#[test]
fn many_overrides_and_feature_maps_still_export() {
    let s = Scratch::new("review-f1");
    // 16 underlined words in one paragraph.
    let text: String = (0..16).map(|i| format!("w{i:02}")).collect::<Vec<_>>().join(" ");
    let (mut v, story) = layout_with(1, &text);
    for i in 0..16u64 {
        ok(&mut v, "layout.set_character_attributes", json!({"story": story, "start": i * 4, "end": i * 4 + 3, "attributes": {"underline": {"type": "solid"}}}));
    }
    let r = export(&mut v, &s.0.join("underlined.pdf"));
    assert!(r["lines"].as_u64().unwrap() > 0, "{r}");
    assert_eq!(host_loss(&r, "character_attribute_not_rendered"), [("underline".to_owned(), 16)], "{r}");

    // 7 links under a paragraph style with 5 feature tags.
    let text = "one two three four five six seven";
    let (mut v, story) = layout_with(1, text);
    let body = new_style(&mut v, "paragraph", "Body");
    ok(&mut v, "layout.style.set_character_attributes", json!({"style": body, "attributes": {"opentype_features": {"onum": 1, "pnum": 1, "liga": 1, "calt": 1, "kern": 1}}}));
    ok(&mut v, "layout.apply_paragraph_style", json!({"story": story, "paragraph": 0, "style": body}));
    let mut at = 0u64;
    for word in text.split(' ') {
        let end = at + word.len() as u64;
        ok(&mut v, "layout.set_character_attributes", json!({"story": story, "start": at, "end": end, "attributes": {"hyperlink": {"target_kind": "url", "url": format!("https://example.com/{word}")}}}));
        at = end + 1;
    }
    let r = export(&mut v, &s.0.join("links.pdf"));
    assert_eq!(host_loss(&r, "character_attribute_not_rendered"), [("hyperlink".to_owned(), 7)], "{r}");

    // One paragraph at Type's style limit plus one: 33 letters, each with its own tracking.
    let letters: String = ('a'..='z').chain('A'..='G').collect();
    assert_eq!(letters.len(), 33);
    let (mut v, story) = layout_with(1, &letters);
    for i in 0..33u64 {
        ok(&mut v, "layout.set_character_attributes", json!({"story": story, "start": i, "end": i + 1, "attributes": {"tracking": i * 10}}));
    }
    let r = export(&mut v, &s.0.join("chunked.pdf"));
    assert_eq!(glyphs(&s.0.join("chunked.pdf")).len(), 33, "every letter is drawn: {r}");
    // One record past the feature limit: 65 tags on one span; the export keeps 64 and reports the rest.
    let tags: Map<String, Value> = (1..=20).map(|i| format!("ss{i:02}")).chain((1..=45).map(|i| format!("cv{i:02}"))).map(|t| (t, json!(1))).collect();
    assert_eq!(tags.len(), 65);
    ok(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 0, "end": 33, "clear": ["tracking"], "attributes": {"opentype_features": tags}}));
    let r = export(&mut v, &s.0.join("features.pdf"));
    assert_eq!(host_loss(&r, "opentype_features_over_shaping_limit"), [("opentype_features".to_owned(), 1)], "{r}");
}

/// F3 + Q2: one storage per attribute; STU-TYP-201 order (local override, character style, paragraph style).
/// A character style's size set through edit_style beats a paragraph style's record size, in the read and in
/// the PDF; edit_style leading reaches the line pitch.
#[test]
fn a_character_style_size_beats_a_paragraph_style_size_in_either_storage() {
    let s = Scratch::new("review-f3");
    let (mut v, story) = layout_with(1, "Big word\nNext line");
    let body = new_style(&mut v, "paragraph", "Body");
    ok(&mut v, "layout.style.set_character_attributes", json!({"style": body, "attributes": {"size": 10.0}}));
    ok(&mut v, "layout.apply_paragraph_style", json!({"story": story, "paragraph": 0, "style": body}));
    ok(&mut v, "layout.apply_paragraph_style", json!({"story": story, "paragraph": 1, "style": body}));
    let big = new_style(&mut v, "character", "Big");
    ok(&mut v, "layout.style.edit_style", json!({"style": big, "patch": {"font_size_pt": 18}}));
    ok(&mut v, "layout.apply_character_style", json!({"story": story, "start": 0, "end": 3, "style": big}));
    assert_eq!(read(&mut v, &story, 0, 3)["fields"]["size"], 18.0);
    assert_eq!(read(&mut v, &story, 4, 8)["fields"]["size"], 10.0);
    let g = drawn(&mut v, &s.0.join("sizes.pdf"));
    assert!((g[0].size - 18.0).abs() < 1e-6 && (g[g.len() - 1].size - 10.0).abs() < 1e-6, "{g:?}");
    // One storage: the character style's size is its record's.
    let styles = records(&v)["styles"].clone();
    let legacy = styles["styles"].as_array().unwrap().iter().find(|s| s["style_id"] == big.as_str()).unwrap()["font_size"].clone();
    assert!(legacy.is_null(), "edit_style writes the record, not a second storage: {styles}");
    ok(&mut v, "layout.style.edit_style", json!({"style": body, "patch": {"leading_pt": 30}}));
    let g = drawn(&mut v, &s.0.join("leading.pdf"));
    let pitch = (g[0].y - g[g.len() - 1].y).abs();
    assert!((pitch - 30.0).abs() < 1e-6, "line pitch {pitch}: {g:?}");
}

/// F4: a feature the font lacks, small caps and OpenType positions are reported as UnavailableInert receipts in
/// the export result and its host loss, never dropped silently; case mapping that cannot map a character is
/// reported too.
#[test]
fn features_the_font_lacks_are_reported_not_dropped() {
    let s = Scratch::new("review-f4");
    // Small 0..5, caps 6..10, zz 11..13, 1st 14..17, Stra\u{df}e 18..25.
    let (mut v, story) = layout_with(1, "Small caps zz 1st Stra\u{df}e");
    ok(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 0, "end": 5, "attributes": {"capitalization": "small_caps"}}));
    ok(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 11, "end": 13, "attributes": {"opentype_features": {"zz99": 1}}}));
    ok(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 15, "end": 17, "attributes": {"position": "ot_superscript"}}));
    ok(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 18, "end": 25, "attributes": {"capitalization": "all_caps"}}));
    let r = export(&mut v, &s.0.join("receipts.pdf"));
    let receipts = r["feature_receipts"].as_array().unwrap_or_else(|| panic!("feature receipts: {r}"));
    let state = |tag: &str| receipts.iter().find(|x| x["tag"] == tag).map(|x| x["state"].as_str().unwrap().to_owned());
    assert_eq!(state("zz99").as_deref(), Some("unavailable_inert"), "{r}");
    assert!(host_loss(&r, "opentype_feature_unavailable").iter().any(|(t, n)| t == "zz99" && *n >= 1), "{r}");
    let missing = host_loss(&r, "character_attribute_not_rendered");
    for (tag, field) in [("smcp", "capitalization"), ("sups", "position")] {
        let st = state(tag).unwrap_or_else(|| panic!("{tag} has a receipt: {r}"));
        assert!(st == "applied" || missing.iter().any(|(f, _)| f == field), "{tag} {st}: {r}");
    }
    assert_eq!(host_loss(&r, "case_mapping_incomplete"), [("capitalization".to_owned(), 1)], "{r}");
}

/// F5: tracking is added once per cluster, after its last spacing glyph; a combining mark stays on its base.
#[test]
fn tracking_is_added_once_per_cluster() {
    let s = Scratch::new("review-f5");
    let (mut v, story) = layout_with(1, "x\u{301}y");
    let plain = drawn(&mut v, &s.0.join("plain.pdf"));
    assert_eq!(plain.len(), 3, "x, the mark and y: {plain:?}");
    ok(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 0, "end": 4, "attributes": {"tracking": 500}}));
    let tracked = drawn(&mut v, &s.0.join("tracked.pdf"));
    assert!(((tracked[1].x - tracked[0].x) - (plain[1].x - plain[0].x)).abs() < 1e-6, "the mark stays on its base: {plain:?} {tracked:?}");
    assert!(((tracked[2].x - tracked[0].x) - (plain[2].x - plain[0].x) - 6.0).abs() < 1e-6, "one 6 pt step per cluster: {plain:?} {tracked:?}");
}

/// F6 + Q8: clear_overrides clears typographic formatting (language included) and keeps the hyperlink.
#[test]
fn clear_overrides_keeps_hyperlinks() {
    let s = Scratch::new("review-f6");
    let (mut v, story) = layout_with(1, "Linked words");
    ok(&mut v, "layout.define_language", json!({"language": {"name": "de_DE", "primary_language_name": "German", "icu_locale_name": "de_DE"}}));
    ok(&mut v, "layout.set_language", json!({"story": story, "start": 0, "end": 6, "language": "de_DE"}));
    ok(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 0, "end": 6, "attributes": {"tracking": 50, "hyperlink": {"target_kind": "url", "url": "https://example.com"}}}));
    ok(&mut v, "layout.style.clear_overrides", json!({"story": story, "paragraph": 0}));
    let check = |v: &mut Viewer| {
        let r = read(v, &story, 0, 6);
        assert_eq!(r["fields"]["hyperlink"], json!({"target_kind": "url", "url": "https://example.com"}), "{r}");
        assert_eq!((r["fields"]["tracking"].clone(), r["defaulted"]["tracking"].clone()), (json!(0.0), json!(true)), "{r}");
        assert_eq!(r["applied_language"], Value::Null, "{r}");
    };
    check(&mut v);
    let package = s.0.join("cleared.handshake");
    v.save(&package, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&package).unwrap_or_else(|e| panic!("{e}"));
    check(&mut w);
}

/// F7: a language record's name is a range key (the applied_language rule), checked when it is defined.
#[test]
fn language_names_are_range_keys() {
    let s = Scratch::new("review-f7");
    let (mut v, story) = layout_with(1, "Hello");
    assert_eq!(refused(&mut v, "layout.define_language", json!({"language": {"name": "English: USA", "primary_language_name": "English", "icu_locale_name": "en_US"}})), "folio_invalid_field");
    ok(&mut v, "layout.define_language", json!({"language": {"name": "en_US", "primary_language_name": "English", "icu_locale_name": "en_US"}}));
    ok(&mut v, "layout.set_language", json!({"story": story, "start": 0, "end": 5, "language": "en_US"}));
    let package = s.0.join("lang.handshake");
    v.save(&package, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&package).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(read(&mut w, &story, 0, 5)["applied_language"], "en_US");
}

/// F11 + Q5: CR LF is one paragraph terminator (normalised to LF at the replace_text input).
#[test]
fn crlf_is_one_paragraph_terminator() {
    let (mut v, story) = layout_with(1, "");
    ok(&mut v, "layout.replace_text", json!({"story": story, "start": 0, "end": 0, "text": "one\r\ntwo"}));
    let st = v.layout_editor().unwrap().doc().stories.iter().find(|s| s.story_id == story).cloned().unwrap();
    assert_eq!((st.inline_text(), st.paragraphs.iter().map(|p| (p.start, p.end)).collect::<Vec<_>>()), (Some("one\ntwo"), vec![(0, 4), (4, 7)]));
}

/// Q7: text inserted at byte 0 of a non-empty story lies in a character range (the ranges tile the stream), so
/// the commit, save and reopen go through.
#[test]
fn text_inserted_at_byte_zero_lies_in_a_character_range() {
    let s = Scratch::new("review-q7");
    let (mut v, story) = layout_with(1, "world");
    ok(&mut v, "layout.replace_text", json!({"story": story, "start": 0, "end": 0, "text": "hello "}));
    let st = v.layout_editor().unwrap().doc().stories.iter().find(|s| s.story_id == story).cloned().unwrap();
    assert_eq!(st.character_ranges.iter().map(|r| (r.start, r.end)).collect::<Vec<_>>(), [(0, 11)]);
    let package = s.0.join("zero.handshake");
    v.save(&package, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&package).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(read(&mut w, &story, 0, 11)["segments"].as_array().unwrap().len(), 1);
}

/// F13 (STU-TYP-221, 109, 170, 112, 111): the parameter rows carry the three steps and the domain, UNKNOWN is
/// written as UNKNOWN; a binary feature is a boolean; one decoration field clears alone; a read at an
/// insertion point gives the attributes there.
#[test]
fn introspection_rows_binary_features_subfield_clear_and_insertion_point_read() {
    let (mut v, story) = layout_with(1, "Hello world");
    let r = read(&mut v, &story, 0, 5);
    let row = |k: &str| r["parameters"].as_array().unwrap().iter().find(|p| p["key"] == k).cloned().unwrap();
    let size = row("size");
    assert_eq!((size["hard_min"].clone(), size["exclusive_min"].clone(), size["step"].clone()), (json!("UNKNOWN"), json!(0.0), json!("UNKNOWN")), "{size}");
    assert!(size["selected_step"].is_number() && size["selected_coarse_step"].is_number() && size["selected_fine_step"].is_number(), "{size}");
    assert_eq!(row("tracking")["precision"], 0);
    ok(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 0, "end": 5, "attributes": {"opentype_features": {"liga": true, "salt": 2}, "underline": {"type": "solid", "tint": 40.0}}}));
    let back = read(&mut v, &story, 0, 5);
    assert_eq!(back["fields"]["opentype_features"], json!({"liga": true, "salt": 2}), "{back}");
    ok(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 0, "end": 5, "clear": ["underline/tint"]}));
    assert_eq!(read(&mut v, &story, 0, 5)["fields"]["underline"], json!({"type": "solid"}));
    let point = read(&mut v, &story, 3, 3);
    assert_eq!(point["segments"].as_array().unwrap().len(), 1, "{point}");
    assert_eq!(point["fields"]["underline"], json!({"type": "solid"}), "{point}");
}

/// F14: a node hyperlink names a document node; a page delete that removes the node removes the link in the
/// same step, and undo restores both.
#[test]
fn node_links_name_document_nodes() {
    let (mut v, story) = layout_with(2, "Go to page two");
    assert_eq!(refused(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 0, "end": 2, "attributes": {"hyperlink": {"target_kind": "node", "node_id": "nothing"}}})), "unknown_link_target");
    let spread = v.run().snapshot().unwrap().document().page_spreads.iter().find(|s| s.pages.iter().any(|p| p.page_id == "page-2")).unwrap().page_spread_id.clone();
    ok(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 0, "end": 2, "attributes": {"hyperlink": {"target_kind": "node", "node_id": spread}}}));
    let steps = undo_labels(&v).len();
    v.delete_page("page-2").unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(undo_labels(&v).len(), steps + 1);
    assert_eq!(read(&mut v, &story, 0, 2)["fields"]["hyperlink"], Value::Null, "the link went with its node");
    v.undo().unwrap();
    assert_eq!(read(&mut v, &story, 0, 2)["fields"]["hyperlink"]["node_id"], spread.as_str());
}

/// Lead ruling on SMB-LAYOUT-DELETE-CASCADE-1: deleting a frame or a page with overridden text leaves no stale
/// override, in the same undo step; undo restores it.
#[test]
fn deleting_a_frame_or_page_drops_its_overrides_in_one_step() {
    let (mut v, story) = layout_with(2, "Some styled text");
    ok(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 0, "end": 4, "attributes": {"tracking": 25}}));
    let frame = v.layout_editor().unwrap().state_json()["frames"][0]["frame_id"].as_str().unwrap().to_owned();
    assert_eq!(overrides_of(&v, &story), 1);
    let steps = undo_labels(&v).len();
    v.delete_layer(&frame).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!((undo_labels(&v).len(), overrides_of(&v, &story)), (steps + 1, 0), "the story and its overrides go in one step");
    v.undo().unwrap();
    assert_eq!(overrides_of(&v, &story), 1, "undo restores the override");
    v.delete_page("page-1").unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(overrides_of(&v, &story), 0, "a page delete takes the frame, its story and its overrides");
    v.undo().unwrap();
    assert_eq!(overrides_of(&v, &story), 1);
}

/// F15 (d): the success paths of the registry rows: a stroke style deletes, undo and redo; define replaces.
#[test]
fn registry_rows_replace_delete_undo_and_redo() {
    let (mut v, story) = layout_with(1, "Text");
    ok(&mut v, "layout.define_stroke_style", json!({"stroke_style": {"name": "hairline", "pattern": {"pattern_kind": "solid"}, "end_cap": "butt", "end_join": "miter"}}));
    ok(&mut v, "layout.define_stroke_style", json!({"stroke_style": {"name": "hairline", "pattern": {"pattern_kind": "dash", "segments": [2.0, 2.0]}, "end_cap": "round", "end_join": "round"}}));
    let strokes = records(&v)["styles"]["stroke_styles"].clone();
    assert_eq!((strokes.as_array().unwrap().len(), strokes[0]["end_cap"].as_str()), (1, Some("round")), "define replaces: {strokes}");
    ok(&mut v, "layout.define_language", json!({"language": {"name": "de_DE", "primary_language_name": "German", "icu_locale_name": "de_DE"}}));
    ok(&mut v, "layout.define_language", json!({"language": {"name": "de_DE", "primary_language_name": "Deutsch", "icu_locale_name": "de_DE"}}));
    let langs = records(&v)["styles"]["languages"].clone();
    assert_eq!((langs.as_array().unwrap().len(), langs[0]["primary_language_name"].as_str()), (1, Some("Deutsch")), "{langs}");
    let deleted = ok(&mut v, "layout.delete_stroke_style", json!({"name": "hairline"}));
    assert_eq!(deleted["undo_name"], "layout.delete_stroke_style");
    assert!(records(&v)["styles"]["stroke_styles"].is_null());
    ok(&mut v, "layout.undo", json!({}));
    assert_eq!(records(&v)["styles"]["stroke_styles"].as_array().map(Vec::len), Some(1));
    ok(&mut v, "layout.redo", json!({}));
    assert!(records(&v)["styles"]["stroke_styles"].is_null());
    let _ = story;
}
