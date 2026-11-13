//! Lane SMB-TYPE-CHARACTER-ATTRIBUTES-1 (feature `viewer`; STU-TYP-155..161, 133, 170, 192, 209, 210,
//! 221, 222): character attributes, stroke styles and language records through the typed `layout.*`
//! rows by id (`Viewer::run_command`, the JSONL loop's surface): each mutating row is one named undo
//! step carrying the caller's attribution, a refusal changes nothing, the records survive save and
//! reopen, the read reports mixed values with their extents, and the PDF export shapes with them
//! (tracking widens lines; the case mapping draws capitals over unchanged stored text; script and
//! language reach Type; what it does not draw is reported per field). The empty-frame form keeps
//! its paragraph style through save and reopen (lead decision (a)).
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
fn undo_labels(v: &Viewer) -> Vec<String> {
    serde_json::from_value(v.run().history()["undo"].clone()).unwrap()
}
/// A refusal changes nothing: same document entry, same history.
fn refused(v: &mut Viewer, id: &str, p: Value) -> String {
    let before = (v.run().document_entry("layout_records").map(<[u8]>::to_vec), v.run().history());
    let code = match cmd(v, id, p) {
        Ok(r) => panic!("{id} should refuse: {r}"),
        Err((code, _)) => code,
    };
    assert_eq!((v.run().document_entry("layout_records").map(<[u8]>::to_vec), v.run().history()), before, "{id} refused ({code}) and changed nothing");
    code
}

/// A 595 x 842 pt layout document with one 300 pt wide frame holding `text`; its story id.
fn layout_with(text: &str) -> (Viewer, String) {
    let mut v = Viewer::new(Prefer::Cpu);
    v.new_document(&NewDocument::new(DocumentProfile::Layout, Some((595.0, 842.0))), None).unwrap();
    ok(&mut v, "layout.draw_frame", json!({"page_id": "page-1", "x": 40, "y": 40, "w": 300, "h": 600}));
    let story = v.layout_editor().unwrap().state_json()["frames"][0]["story_id"].as_str().expect("the frame's story").to_owned();
    if !text.is_empty() {
        ok(&mut v, "layout.replace_text", json!({"story": story, "start": 0, "end": 0, "text": text}));
    }
    (v, story)
}

fn read(v: &mut Viewer, story: &str, start: u64, end: u64) -> Value {
    ok(v, "layout.character_attributes", json!({"story": story, "start": start, "end": end}))
}

/// The page-1 text a reader extracts: a glyph inside an ActualText span gives that span's text once, every
/// other glyph its /ToUnicode text; whitespace dropped (spaces may or may not be drawn as glyphs).
fn extracted(pdf: &Path) -> String {
    let bytes = std::fs::read(pdf).unwrap();
    let doc = read_vector_document(&bytes, 1 << 26, &CancellationToken::default()).unwrap();
    let mut out = String::new();
    let mut span: Option<String> = None;
    for i in &doc.pages[0].items {
        let VectorItem::Draw { glyph: Some(g), .. } = i else { continue };
        match &g.actual_text {
            Some(t) if span.as_ref() == Some(t) => {}
            Some(t) => {
                out.push_str(t);
                span = Some(t.clone());
            }
            None => {
                out.push_str(g.unicode.as_deref().unwrap_or("\u{fffd}"));
                span = None;
            }
        }
    }
    out.chars().filter(|c| !c.is_whitespace()).collect()
}

/// (glyph id, baseline y) of every glyph drawn on page 1, in drawing order.
fn glyphs(pdf: &Path) -> Vec<(u32, f64)> {
    let bytes = std::fs::read(pdf).unwrap();
    let doc = read_vector_document(&bytes, 1 << 26, &CancellationToken::default()).unwrap();
    doc.pages[0]
        .items
        .iter()
        .filter_map(|i| match i {
            VectorItem::Draw { glyph: Some(g), .. } => Some((g.glyph_id, g.matrix[5])),
            _ => None,
        })
        .collect()
}

fn export(v: &mut Viewer, pdf: &Path) -> Value {
    ok(v, "layout.export_pdf", json!({"path": pdf.to_str().unwrap(), "overwrite": true}))
}

fn host_loss(r: &Value, code: &str) -> Vec<(String, u64)> {
    r["host_loss"].as_array().unwrap().iter().filter(|l| l["code"] == code).map(|l| (l["element"].as_str().unwrap().to_owned(), l["count"].as_u64().unwrap())).collect()
}

#[test]
fn character_attributes_are_typed_undoable_steps_that_survive_save_and_reopen() {
    let s = Scratch::new("char-attrs");
    let (mut v, story) = layout_with("Hello world");
    let steps = undo_labels(&v).len();
    ok(&mut v, "layout.define_language", json!({"language": {"name": "tr_TR", "primary_language_name": "Turkish", "icu_locale_name": "tr_TR",
        "single_quotes": {"open": "\u{2018}", "close": "\u{2019}"}}}));
    ok(&mut v, "layout.define_stroke_style", json!({"stroke_style": {"name": "hairline", "pattern": {"pattern_kind": "dash", "segments": [2.0, 1.0]}, "end_cap": "butt", "end_join": "miter"}}));
    ok(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 6, "end": 11,
        "attributes": {"tracking": 50, "baseline_shift": 3.0, "underline": {"type": "hairline", "skip_ink": true}, "opentype_features": {"liga": 0}}}));
    let made = ok(&mut v, "layout.style.new_style", json!({"scope": "character", "name": "Strong"}));
    assert_eq!(made["undo_name"], "layout.style.new_style", "{made}");
    let style = v.layout_editor().unwrap().doc().styles.styles.iter().find(|s| s.name == "Strong").unwrap().style_id.clone();
    ok(&mut v, "layout.style.set_character_attributes", json!({"style": style, "attributes": {"size": 18.0, "capitalization": "all_caps"}}));
    ok(&mut v, "layout.apply_character_style", json!({"story": story, "start": 0, "end": 5, "style": style}));
    let last = ok(&mut v, "layout.set_language", json!({"story": story, "start": 0, "end": 5, "language": "tr_TR"}));
    assert_eq!((last["undo_name"].as_str(), last["undo_steps"].as_u64()), (Some("layout.set_language"), Some(steps as u64 + 7)), "{last}");
    let labels = undo_labels(&v);
    assert_eq!(
        labels[steps..].to_vec(),
        ["layout.define_language", "layout.define_stroke_style", "layout.set_character_attributes", "layout.style.new_style",
            "layout.style.set_character_attributes", "layout.apply_character_style", "layout.set_language"],
        "one named step each"
    );
    let attribution = v.run().history()["undo_steps"].as_array().unwrap().last().cloned().unwrap();
    assert!(attribution["actor"].is_object(), "the caller's attribution: {attribution}");

    // STU-TYP-222: one value where the range agrees, `mixed` with extents where it does not.
    let r = read(&mut v, &story, 0, 11);
    // The read reports what the export draws: an unset field reads its default, marked `defaulted` (review F3).
    assert_eq!(r["fields"]["tracking"], json!({"mixed": true, "values": [{"value": 0.0, "extents": [[0, 5], [5, 6]]}, {"value": 50.0, "extents": [[6, 11]]}]}), "{r}");
    assert_eq!(r["defaulted"]["tracking"], json!({"mixed": true, "values": [{"value": true, "extents": [[0, 5], [5, 6]]}, {"value": false, "extents": [[6, 11]]}]}), "{r}");
    assert_eq!(r["fields"]["size"]["mixed"], true, "{r}");
    assert_eq!(r["applied_language"]["values"][0], json!({"value": "tr_TR", "extents": [[0, 5]]}), "{r}");
    assert_eq!(r["legacy_booleans"]["ligatures"]["values"][1]["value"], false, "liga 0 reads as ligatures off: {r}");
    assert_eq!(read(&mut v, &story, 6, 11)["fields"]["underline"], json!({"type": "hairline", "skip_ink": true}));
    assert_eq!(r["parameters"].as_array().unwrap().len(), 26, "STU-TYP-221 rows");
    assert_eq!(r["enumerations"]["digits_type"].as_array().unwrap().len(), 20);
    assert!(r["stroke_styles"].as_array().unwrap().iter().any(|n| n == "hairline"), "{r}");

    // Typed refusals change nothing.
    assert_eq!(refused(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 0, "end": 2, "attributes": {"underline": {"type": "nowhere"}}})), "unknown_stroke_style");
    assert_eq!(refused(&mut v, "layout.set_language", json!({"story": story, "start": 0, "end": 2, "language": "xx_YY"})), "unknown_language");
    assert_eq!(refused(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 0, "end": 2, "clear": ["not_a_field"]})), "unknown_character_attribute");
    assert_eq!(refused(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 0, "end": 2, "attributes": {"tracking": 1.5}})), "folio_invalid_field");
    assert_eq!(refused(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 0, "end": 2, "attributes": {"small_cap_size": 0.5}})), "folio_invalid_field");
    assert_eq!(refused(&mut v, "layout.delete_stroke_style", json!({"name": "hairline"})), "stroke_style_in_use");
    for bad in [json!({"story": story, "start": 4, "end": 4}), json!({"story": story, "start": 0, "end": 99}), json!({"story": story, "start": 0, "end": 2, "attributes": {"size": "big"}})] {
        refused(&mut v, "layout.set_character_attributes", bad);
    }
    // Clearing the underline frees the stroke style.
    ok(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 6, "end": 11, "clear": ["underline", "opentype_features/liga"]}));
    assert_eq!(read(&mut v, &story, 6, 11)["fields"]["underline"], Value::Null);

    let before = read(&mut v, &story, 0, 11);
    let package = s.0.join("chars.handshake");
    v.save(&package, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&package).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(read(&mut w, &story, 0, 11), before, "save and reopen keep every record");
    // Undo after reopen is not claimed (open clears history); undo in the live session restores.
    ok(&mut v, "layout.undo", json!({}));
    assert_eq!(read(&mut v, &story, 6, 11)["fields"]["underline"], json!({"type": "hairline", "skip_ink": true}));
}

/// Lead decision (a): the empty-frame form is Folio's own; the paragraph style applied to an empty
/// frame survives save and reopen, and text typed into it splits into styled paragraphs.
#[test]
fn an_empty_frame_keeps_its_paragraph_style_through_save_and_reopen() {
    let s = Scratch::new("empty-frame-style");
    let (mut v, story) = layout_with("");
    ok(&mut v, "layout.style.new_style", json!({"scope": "paragraph", "name": "Heading"}));
    let style = v.layout_editor().unwrap().doc().styles.styles.iter().find(|s| s.name == "Heading").unwrap().style_id.clone();
    ok(&mut v, "layout.apply_paragraph_style", json!({"story": story, "paragraph": 0, "style": style}));
    let package = s.0.join("empty.handshake");
    v.save(&package, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&package).unwrap_or_else(|e| panic!("{e}"));
    ok(&mut w, "layout.preflight", json!({}));
    let kept = |w: &mut Viewer| w.layout_editor().unwrap().doc().stories.iter().find(|s| s.story_id == story).map(|s| s.paragraphs.iter().map(|p| (p.start, p.end, p.style_ref.clone())).collect::<Vec<_>>());
    assert_eq!(kept(&mut w), Some(vec![(0, 0, Some(style.clone()))]));
    ok(&mut w, "layout.replace_text", json!({"story": story, "start": 0, "end": 0, "text": "one\ntwo"}));
    assert_eq!(kept(&mut w), Some(vec![(0, 4, Some(style.clone())), (4, 7, Some(style))]));
}

#[test]
fn the_pdf_shapes_with_the_attributes_and_reports_what_it_does_not_draw() {
    let s = Scratch::new("char-pdf");
    // Hello 0..5, HELLO 6..11, world 12..17, of 18..20, type 21..25.
    let text = "Hello HELLO world of type";
    let (mut v, story) = layout_with(text);
    let plain = export(&mut v, &s.0.join("plain.pdf"));
    assert_eq!(plain["lines"], 1, "{plain}");
    assert_eq!(plain["itemization"], json!([{"script": "Latn", "language": "und", "spans": 1}]), "no stated language is und: {plain}");

    // Tracking reaches composition: one em per glyph wraps the 300 pt line.
    ok(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 0, "end": text.len(), "attributes": {"tracking": 1000}}));
    let tracked = export(&mut v, &s.0.join("tracked.pdf"));
    assert!(tracked["lines"].as_u64().unwrap() > 1, "{tracked}");
    ok(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 0, "end": text.len(), "clear": ["tracking"]}));

    // STU-TYP-158: all caps draws the capital glyphs while the stored (and searchable) text keeps its case;
    // baseline shift moves its glyphs up.
    ok(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 0, "end": 5, "attributes": {"capitalization": "all_caps", "horizontal_scale": 80.0, "hyperlink": {"target_kind": "url", "url": "https://example.com"}}}));
    ok(&mut v, "layout.set_character_attributes", json!({"story": story, "start": 12, "end": 17, "attributes": {"baseline_shift": 3.0, "underline": {"type": "solid"}}}));
    ok(&mut v, "layout.define_language", json!({"language": {"name": "de_DE", "primary_language_name": "German", "icu_locale_name": "de_DE"}}));
    ok(&mut v, "layout.set_language", json!({"story": story, "start": 18, "end": 20, "language": "de_DE"}));
    let pdf = s.0.join("styled.pdf");
    let r = export(&mut v, &pdf);
    assert_eq!(r["lines"], 1, "{r}");
    // Review F2 + lead ruling: the case-mapped span extracts as its source text through ActualText, and
    // unrelated text (HELLO, typed in capitals) extracts exactly as typed.
    assert_eq!(extracted(&pdf), "HelloHELLOworldoftype");
    assert_eq!(extracted(&s.0.join("plain.pdf")), "HelloHELLOworldoftype");
    // Glyph ids in drawing order (the PDF text layer maps a glyph id to one character, so the capital
    // glyphs are compared by id, not read back as text). In the plain export the second H opens HELLO.
    let (before, after) = (glyphs(&s.0.join("plain.pdf")), glyphs(&pdf));
    let k = before.iter().skip(1).position(|g| g.0 == before[0].0).map(|i| i + 1).expect("HELLO's H");
    let ids = |g: &[(u32, f64)], r: std::ops::Range<usize>| g[r].iter().map(|x| x.0).collect::<Vec<_>>();
    assert_ne!(ids(&before, 0..5), ids(&before, k..k + 5), "Hello and HELLO draw different glyphs");
    assert_eq!(ids(&after, 0..5), ids(&before, k..k + 5), "Hello in all caps draws HELLO's glyphs");
    assert_eq!(ids(&after, k..k + 5), ids(&before, k..k + 5));
    // `w` follows HELLO and the same separator again (k - 5 glyphs: a drawn space or none): index 2k.
    let w = 2 * k;
    assert_eq!(after[w].0, before[w].0, "the w of world");
    assert!((after[w].1 - after[0].1 - 3.0).abs() < 1e-6 && (before[w].1 - before[0].1).abs() < 1e-6, "baseline shift 3 pt up: {before:?} {after:?}");
    let stored = v.layout_editor().unwrap().doc().stories.iter().find(|s| s.story_id == story).and_then(|s| s.inline_text().map(str::to_owned));
    assert_eq!(stored.as_deref(), Some(text), "the stored characters are unchanged");
    let missing = host_loss(&r, "character_attribute_not_rendered");
    for field in ["horizontal_scale", "hyperlink", "underline"] {
        assert!(missing.iter().any(|(f, n)| f == field && *n > 0), "{field}: {r}");
    }
    let languages: Vec<&str> = r["itemization"].as_array().unwrap().iter().map(|e| e["language"].as_str().unwrap()).collect();
    assert!(languages.contains(&"de-DE") && languages.contains(&"und"), "{r}");

    // IDML export names what it does not write.
    let idml = ok(&mut v, "layout.export_idml", json!({"path": s.0.join("styled.idml").to_str().unwrap(), "overwrite": true}));
    let codes: Vec<&str> = idml["loss"].as_array().unwrap().iter().map(|l| l["code"].as_str().unwrap()).collect();
    assert!(codes.contains(&"character_overrides_not_exported") && codes.contains(&"languages_not_exported"), "{idml}");
}
