//! Lane SMB-ID-PREPRESS slice 3: the find/change command family (STU-LAY-027, STU-LAY-129). Byte
//! offsets are counted by hand from the literal story texts. InDesign parity UNVERIFIED (D5).
mod support_layout;
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use support_layout::*;

fn no_font(_: Option<&str>) -> Option<String> {
    None
}

fn ctx<'a>(doc: &'a StudioDocument, stories: &'a [StudioTextStory], font_of: &'a dyn Fn(Option<&str>) -> Option<String>) -> FindContext<'a> {
    FindContext { document: doc, stories, font_of, include_hidden_layers: false }
}

fn spans(h: &[FindChangeHit]) -> Vec<(u64, u64)> {
    h.iter().map(|h| (h.start, h.end)).collect()
}

fn doc_two_frames(hide_second: bool) -> StudioDocument {
    let mut b = frame_layer(2, 2, spec(1, 10.0, 10.0, 100.0, 100.0), None, None);
    b.visible = !hide_second;
    document(vec![frame_layer(1, 1, spec(0, 10.0, 10.0, 100.0, 100.0), None, None), b, item_layer(3, on_spread())], [PageSide::RightHand, PageSide::LeftHand])
}

#[test]
fn text_mode_metacharacters_scope_change_and_undo() {
    // "Cat cat\tcatalog 42\ncat" : bytes C0 a1 t2 ' '3 c4 a5 t6 \t7 c8 ... catalog 8..15, ' '15, 42 16..18, \n18, cat 19..22.
    let text = "Cat cat\tcatalog 42\ncat";
    let (st, _) = story(1, text, &[(0, 18), (19, 22)]);
    let (st2, _) = story(2, "the cat", &[(0, 7)]);
    let stories = [st.clone(), st2];
    let doc = doc_two_frames(false);
    let c = ctx(&doc, &stories, &no_font);
    let mode = |find: &str, cs: bool, ww: bool| FindChangeMode::Text { find: find.into(), case_sensitive: cs, whole_word: ww };
    let s1 = FindScope::Story(id("STXT", 1));
    // Case-insensitive finds "Cat" too; whole word skips "catalog".
    assert_eq!(spans(&find_change_text(&c, &mode("cat", false, true), &s1, None).unwrap()), vec![(0, 3), (4, 7), (19, 22)]);
    assert_eq!(spans(&find_change_text(&c, &mode("cat", true, false), &s1, None).unwrap()), vec![(4, 7), (8, 11), (19, 22)]);
    // ^t tab, ^9 digit, ^p paragraph end; regex syntax in the literal stays literal.
    assert_eq!(spans(&find_change_text(&c, &mode("t^tc", true, false), &s1, None).unwrap()), vec![(6, 9)]);
    assert_eq!(spans(&find_change_text(&c, &mode("^9^9^p", true, false), &s1, None).unwrap()), vec![(16, 19)]);
    assert!(find_change_text(&c, &mode("a.c", true, false), &s1, None).unwrap().is_empty());
    assert_eq!(find_change_text(&c, &mode("^q", true, false), &s1, None).unwrap_err().code(), "find_metacharacter_invalid");
    // Scopes: to end of story from byte 5, a selection, the whole document (both stories).
    assert_eq!(spans(&find_change_text(&c, &mode("cat", true, true), &FindScope::ToEndOfStory(id("STXT", 1), 5), None).unwrap()), vec![(19, 22)]);
    assert_eq!(spans(&find_change_text(&c, &mode("cat", true, false), &FindScope::Selection(id("STXT", 1), 4, 12), None).unwrap()), vec![(4, 7), (8, 11)]);
    let all = find_change_text(&c, &mode("cat", true, true), &FindScope::Document, None).unwrap();
    assert_eq!(all.iter().map(|h| (h.story_id == id("STXT", 2), h.start)).collect::<Vec<_>>(), vec![(false, 4), (false, 19), (true, 4)]);
    // A hidden frame's story is skipped unless hidden layers are included.
    let hidden = doc_two_frames(true);
    let mut ch = ctx(&hidden, &stories, &no_font);
    assert_eq!(find_change_text(&ch, &mode("cat", true, true), &FindScope::Document, None).unwrap().len(), 2);
    ch.include_hidden_layers = true;
    assert_eq!(find_change_text(&ch, &mode("cat", true, true), &FindScope::Document, None).unwrap().len(), 3);
    // Change with ^t in the replacement; apply back to front; one snapshot undo.
    let hits = find_change_text(&c, &mode("cat", true, true), &s1, Some("dog^t")).unwrap();
    let (changed, undo) = apply_text_changes(&stories, &hits).unwrap();
    assert_eq!(text_of(&changed[0]), "Cat dog\t\tcatalog 42\ndog\t");
    assert_eq!(changed[1], stories[1]);
    assert_eq!(changed[0].paragraphs.iter().map(|p| (p.start, p.end)).collect::<Vec<_>>(), vec![(0, 20), (20, 24)]);
    assert_eq!(undo.stories_before, stories.to_vec());
    // Selecting a non-text layer is its own error, not "no matches".
    let e = find_change_text(&c, &mode("cat", true, false), &FindScope::SelectedObject(id("SLYR", 3)), None).unwrap_err();
    assert_eq!(e.code(), "object_contains_no_text");
    assert_eq!(spans(&find_change_text(&c, &mode("cat", true, false), &FindScope::SelectedObject(id("SLYR", 2)), None).unwrap()), vec![(4, 7)]);
}

#[test]
fn grep_mode_captures_and_hostile_patterns() {
    let text = "2026-10-16 and 1999-01-02";
    let (st, _) = story(1, text, &[(0, 25)]);
    let stories = [st];
    let doc = doc_two_frames(false);
    let c = ctx(&doc, &stories, &no_font);
    let s1 = FindScope::Story(id("STXT", 1));
    let grep = |p: &str| FindChangeMode::Grep { find: p.into(), case_sensitive: true };
    let hits = find_change_text(&c, &grep(r"(\d{4})-(\d\d)-(?P<d>\d\d)"), &s1, Some("${d}.$2.$1")).unwrap();
    assert_eq!(spans(&hits), vec![(0, 10), (15, 25)]);
    let (changed, _) = apply_text_changes(&stories, &hits).unwrap();
    assert_eq!(text_of(&changed[0]), "16.10.2026 and 02.01.1999");
    // Look-around and back-references: unsupported (typed), invalid syntax: invalid.
    assert_eq!(find_change_text(&c, &grep(r"(?<=a)b"), &s1, None).unwrap_err().code(), "grep_pattern_unsupported");
    assert_eq!(find_change_text(&c, &grep(r"(a)\1"), &s1, None).unwrap_err().code(), "grep_pattern_unsupported");
    assert_eq!(find_change_text(&c, &grep(r"(unclosed"), &s1, None).unwrap_err().code(), "grep_pattern_invalid");
    // Huge GREP: a program over the size limit, a query over the byte cap, an empty query.
    assert_eq!(find_change_text(&c, &grep(r"(?:\w{1000}){1000}"), &s1, None).unwrap_err().code(), "grep_pattern_unsupported");
    assert_eq!(find_change_text(&c, &grep(&"a".repeat(MAX_FIND_CHANGE_QUERY_BYTES + 1)), &s1, None).unwrap_err().code(), "find_query_too_long");
    assert_eq!(find_change_text(&c, &grep(""), &s1, None).unwrap_err().code(), "find_query_empty");
    // Empty matches are skipped and do not loop.
    assert!(find_change_text(&c, &grep("x*"), &s1, None).unwrap().is_empty());
    // Match budget: one more hit than the cap is refused, not truncated.
    let big = "a".repeat(MAX_FIND_CHANGE_MATCHES + 1);
    let (bs, _) = story(1, &big, &[(0, big.len() as u64)]);
    let bstories = [bs];
    let cb = ctx(&doc, &bstories, &no_font);
    assert_eq!(find_change_text(&cb, &grep("a"), &s1, None).unwrap_err().code(), "find_budget_exceeded");
}

#[test]
fn glyph_and_transliterate_modes() {
    // "AxA" in style "serif", then "A" in style "sans": glyph search for A in font Serif hits 0 and 2.
    let (mut st, _) = story(1, "AxAA", &[(0, 4)]);
    st.character_ranges = vec![
        StoryRange { start: 0, end: 3, style_ref: Some("serif".into()), applied_language: None },
        StoryRange { start: 3, end: 4, style_ref: Some("sans".into()), applied_language: None },
    ];
    let stories = [st];
    let doc = doc_two_frames(false);
    let fonts = |s: Option<&str>| match s {
        Some("serif") => Some("Serif".to_string()),
        Some("sans") => Some("Sans".to_string()),
        _ => None,
    };
    let c = ctx(&doc, &stories, &fonts);
    let s1 = FindScope::Story(id("STXT", 1));
    let g = find_change_text(&c, &FindChangeMode::Glyph { font: "Serif".into(), character: 'A' }, &s1, Some("B")).unwrap();
    assert_eq!(spans(&g), vec![(0, 1), (2, 3)]);
    assert_eq!(text_of(&apply_text_changes(&stories, &g).unwrap().0[0]), "BxBA");
    // Transliterate: full-width ABC1 (3 bytes each) and an ideographic space -> half width.
    let (fw, _) = story(1, "\u{FF21}\u{FF22}\u{3000}\u{FF11}z", &[(0, 13)]);
    let fws = [fw];
    let c = ctx(&doc, &fws, &no_font);
    let t = find_change_text(&c, &FindChangeMode::Transliterate { from: CharType::FullWidthRoman, to: CharType::HalfWidthRoman }, &s1, None).unwrap();
    assert_eq!(spans(&t), vec![(0, 3), (3, 6), (6, 9), (9, 12)]);
    assert_eq!(text_of(&apply_text_changes(&fws, &t).unwrap().0[0]), "AB 1z");
    // Hiragana -> katakana.
    let (hk, _) = story(1, "\u{3072}\u{3089}\u{304C}\u{306A}", &[(0, 12)]);
    let hks = [hk];
    let c = ctx(&doc, &hks, &no_font);
    let t = find_change_text(&c, &FindChangeMode::Transliterate { from: CharType::Hiragana, to: CharType::Katakana }, &s1, None).unwrap();
    assert_eq!(text_of(&apply_text_changes(&hks, &t).unwrap().0[0]), "\u{30D2}\u{30E9}\u{30AC}\u{30CA}");
    let e = find_change_text(&c, &FindChangeMode::Transliterate { from: CharType::Hiragana, to: CharType::HalfWidthKatakana }, &s1, None).unwrap_err();
    assert_eq!(e.code(), "transliteration_unsupported");
}

#[test]
fn object_and_colour_modes() {
    let mut doc = doc_two_frames(false);
    doc.layers[1].text_frame.as_mut().unwrap().text_column_count = 2;
    let mut flow = records();
    let mut style = StudioObjectStyle::new("OS1", "Two up");
    style.categories.enable_text_frame_general_options = true;
    style.vertical_justification = Some(VerticalJustification::Bottom);
    flow.object_styles.push(style);
    // Find two-column text frames; then every text-kind layer.
    let two = find_objects(&doc, &flow, &ObjectCriteria { text_column_count: Some(2), ..Default::default() });
    assert_eq!(two, vec![id("SLYR", 2)]);
    let text = find_objects(&doc, &flow, &ObjectCriteria { kind: Some("text".into()), ..Default::default() });
    assert_eq!(text, vec![id("SLYR", 1), id("SLYR", 2)]);
    // Change: apply the object style and set one column; the object style is then findable.
    let (d2, f2) = change_objects(&doc, &flow, &two, &ObjectChange { object_style: Some("OS1".into()), text_column_count: Some(1), ..Default::default() }).unwrap();
    let f = d2.layers[1].text_frame.as_ref().unwrap();
    assert_eq!((f.text_column_count, f.vertical_justification), (1, VerticalJustification::Bottom));
    assert_eq!(find_objects(&d2, &f2, &ObjectCriteria { applied_object_style: Some("OS1".into()), ..Default::default() }), vec![id("SLYR", 2)]);
    assert_eq!(doc.layers[1].text_frame.as_ref().unwrap().text_column_count, 2, "inputs are the undo snapshot");
    // A frame attribute on a layer without a text frame is refused.
    assert!(change_objects(&doc, &flow, &[id("SLYR", 3)], &ObjectChange { text_column_count: Some(1), ..Default::default() }).is_err());
    // find.colour: swatch usages, replaced document-wide with per-use tints kept.
    let swatch = |n: &str| StudioSwatch {
        schema_id: SwatchSchema::V1,
        swatch_id: n.into(),
        name: n.into(),
        swatch_kind: StudioSwatchKind::Process,
        color_model: Some(StudioColorModel::Cmyk),
        components: vec![0.0, 0.0, 0.0, 1.0],
        profile_ref: None,
        spot: None,
        base_swatch_ref: None,
        tint_value: None,
        gradient: None,
        group_ref: None,
    };
    let mut vr = StudioVectorRecords::new(id("SDOC", 1));
    vr.swatches = vec![swatch("SW-A"), swatch("SW-B")];
    let paint = |style: &str, row: u32, sw: &str, tint: Option<f64>| StudioSwatchPaint { style_id: style.into(), row, swatch_id: sw.into(), tint };
    vr.swatch_paints = vec![paint("P1", 0, "SW-A", None), paint("P1", 1, "SW-B", None), paint("P2", 0, "SW-A", Some(40.0))];
    assert_eq!(find_colour(&vr, "SW-A"), vec![("P1".to_string(), 0), ("P2".to_string(), 0)]);
    let (vr2, n) = change_colour(&vr, "SW-A", "SW-B").unwrap();
    assert_eq!(n, 2);
    assert!(find_colour(&vr2, "SW-A").is_empty());
    assert_eq!(vr2.swatch_paints[2], paint("P2", 0, "SW-B", Some(40.0)));
    assert!(change_colour(&vr, "SW-A", "SW-missing").is_err());
}

#[test]
fn saved_queries_and_their_typed_errors() {
    let mut q = SavedQueries::default();
    let query = |mode: &str, locked: bool| SavedQuery { mode: mode.into(), find: r"\d+".into(), change: None, locked };
    q.save("Digits", query("find.grep", false)).unwrap();
    assert_eq!(q.load("Digits").unwrap().mode, "find.grep");
    q.save("Shipped", query("find.text", true)).unwrap();
    assert_eq!(q.save("Shipped", query("find.text", false)).unwrap_err().code(), "query_file_locked");
    assert_eq!(q.delete("Shipped").unwrap_err().code(), "query_file_locked");
    for bad in ["", "  ", "a/b", "a:b", "x\u{0}y"] {
        assert_eq!(q.save(bad, query("find.text", false)).unwrap_err().code(), "invalid_query_name", "{bad:?}");
    }
    assert_eq!(q.save(&"n".repeat(256), query("find.text", false)).unwrap_err().code(), "invalid_query_name");
    assert_eq!(q.save("Weird", query("find.sound", false)).unwrap_err().code(), "invalid_search_mode");
    assert_eq!(q.load("Nope").unwrap_err().code(), "query_file_not_found");
    assert_eq!(q.delete("Nope").unwrap_err().code(), "query_file_not_found");
    // A stored query whose mode is not a search mode loads as invalid_search_mode.
    q.queries.insert("Legacy".into(), query("find.unknown", false));
    assert_eq!(q.load("Legacy").unwrap_err().code(), "invalid_search_mode");
    q.delete("Digits").unwrap();
    assert!(q.load("Digits").is_err());
    // The cap on saved queries.
    let mut many = SavedQueries::default();
    for i in 0..MAX_SAVED_QUERIES {
        many.save(&format!("q{i}"), query("find.text", false)).unwrap();
    }
    assert_eq!(many.save("one more", query("find.text", false)).unwrap_err().code(), "too_many_queries");
}
