//! Lane SMB-TYPE-OT-FEATURES-1: STU-TYP-207 insertable catalogs (catalog half of MT-31005..31009;
//! composition behaviour belongs to layout). Expected code points are Unicode Standard values
//! (names checked with Python unicodedata, Unicode 16.0.0); expected glyph ids for Inter come from
//! fontTools 4.62.1 `getBestCmap` + `getGlyphID` on the hash-pinned fixture, not from Type.
#![allow(dead_code)]
include!("support/mod.rs");

use serde_json::json;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn labels(catalog: CharacterCatalog) -> Vec<&'static str> {
    catalog.members().map(|m| m.label).collect()
}
fn scalars(catalog: CharacterCatalog) -> Vec<u32> {
    catalog.members().map(|m| u32::from(m.scalar())).collect()
}
fn get(id: &str) -> &'static CatalogMember {
    catalog_member(id).unwrap_or_else(|| panic!("{id}"))
}
/// Shapes `text` with Inter and returns the glyph ids.
fn inter_glyphs(fonts: &Fonts, text: &str) -> Vec<u32> {
    let case = json!({
        "case": "character_catalogs", "text": text, "text_sha256": hex(&hash(text.as_bytes())),
        "text_utf8_hex": hex(text.as_bytes()), "font_index": 0, "script": "Latn", "language": "en",
    });
    let mut out = Vec::new();
    with_case(fonts, &case, None, |request, ports, _, _, _| {
        let result = prepared(request, ports);
        assert_eq!(result.request().text_utf8, text);
        for p in result.paragraphs() {
            for r in p.runs() {
                out.extend(r.glyphs().iter().map(|g| g.glyph_id));
            }
        }
    });
    out
}

#[test]
fn mt_31005_four_catalogs_with_stable_ids_and_assigned_stream_forms() {
    let names: Vec<_> = CharacterCatalog::ALL.iter().map(|c| c.label()).collect();
    assert_eq!(names, ["Symbols", "Markers", "Break characters", "White-space characters"]);
    let mut ids: Vec<_> = CATALOG.iter().map(|m| m.id).collect();
    let total = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), total, "ids are unique");
    for m in CATALOG {
        assert!(m.id.starts_with(&format!("{}.", m.catalog.id())), "{}", m.id);
        assert!(m.id.bytes().all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_'), "{}", m.id);
        assert_eq!(m.tooltip(), m.label);
        // SD-29: a member is a sentinel exactly when its stream character is in U+FDD0..U+FDDD.
        assert_eq!(m.is_sentinel(), ('\u{FDD0}'..='\u{FDDD}').contains(&m.scalar()), "{}", m.id);
    }
    let sentinels: Vec<_> = CATALOG.iter().filter(|m| m.is_sentinel()).map(|m| m.id).collect();
    assert_eq!(
        sentinels,
        [
            "markers.current_page_number", "markers.next_page_number", "markers.previous_page_number",
            "markers.section_marker", "markers.footnote_reference", "markers.endnote_reference",
            "markers.cross_reference", "markers.index", "breaks.column",
            "breaks.frame", "breaks.page", "breaks.odd_page", "breaks.even_page", "white_space.flush",
        ]
    );
    // Headless insertion: every member inserts its stream character; unknown ids and bad offsets
    // refuse and the borrowed text stays as it was.
    let text = "a\u{e9}b";
    assert_eq!(insert_catalog_member(text, 1, "symbols.em_dash").unwrap(), "a\u{2014}\u{e9}b");
    assert_eq!(insert_catalog_member(text, 2, "symbols.em_dash"), Err(CatalogError::InvalidOffset));
    assert_eq!(insert_catalog_member(text, 99, "symbols.em_dash"), Err(CatalogError::InvalidOffset));
    assert_eq!(insert_catalog_member(text, 0, "markers.index").unwrap(), "\u{FDD7}a\u{e9}b");
    assert_eq!(insert_catalog_member(text, 0, "symbols.nope"), Err(CatalogError::UnknownMember));
    assert_eq!(text, "a\u{e9}b");
    assert_eq!(CatalogError::ReservedSentinel.code(), "reserved_stream_sentinel");
}

#[test]
fn mt_31006_symbols_catalog_inserts_real_code_points() {
    bounded_case("mt_31006_symbols_catalog_inserts_real_code_points", || {
        assert_eq!(
            labels(CharacterCatalog::Symbols),
            [
                "em dash", "en dash", "discretionary hyphen", "non-breaking hyphen", "ellipsis", "bullet",
                "copyright", "registered", "trademark", "section", "paragraph mark",
                "single typographer quote (open)", "single typographer quote (close)",
                "double typographer quote (open)", "double typographer quote (close)", "degree",
                "currency sign: dollar", "currency sign: cent", "currency sign: pound",
                "currency sign: generic", "currency sign: yen", "currency sign: euro",
            ]
        );
        let want = [
            0x2014, 0x2013, 0x00AD, 0x2011, 0x2026, 0x2022, 0x00A9, 0x00AE, 0x2122, 0x00A7, 0x00B6, 0x2018,
            0x2019, 0x201C, 0x201D, 0x00B0, 0x0024, 0x00A2, 0x00A3, 0x00A4, 0x00A5, 0x20AC,
        ];
        assert_eq!(scalars(CharacterCatalog::Symbols), want);
        // Discretionary and non-breaking hyphens keep distinct break behaviour.
        assert!(get("symbols.discretionary_hyphen").flags.break_opportunity);
        assert!(!get("symbols.non_breaking_hyphen").flags.break_opportunity);
        // fontTools cmap glyph ids of Inter for each symbol (soft hyphen is default-ignorable and
        // shapes to no glyph; its byte stays in the text).
        let inter = [
            1366, 1364, 0, 2114, 1406, 1368, 1457, 1456, 1460, 1473, 1471, 1386, 1387, 1391, 1392, 1466,
            1263, 1264, 1266, 1357, 1265, 1270,
        ];
        let fonts = Fonts::load();
        for (member, gid) in CharacterCatalog::Symbols.members().zip(inter) {
            let text = insert_catalog_member("ab", 1, member.id).unwrap();
            let glyphs = inter_glyphs(&fonts, &text);
            let middle: Vec<u32> = glyphs[1..glyphs.len() - 1].to_vec();
            if gid == 0 {
                assert!(middle.is_empty(), "{}", member.id);
            } else {
                assert_eq!(middle, vec![gid], "{}", member.id);
            }
        }
    });
}

#[test]
fn mt_31007_markers_catalog_stores_assigned_stream_forms() {
    assert_eq!(
        labels(CharacterCatalog::Markers),
        [
            "current page number", "next page number", "previous page number", "section marker",
            "footnote reference", "endnote reference", "anchored-object marker",
            "cross-reference marker", "index marker",
        ]
    );
    // SD-29: eight sentinels U+FDD0..U+FDD7 in the spec's order around the anchored-object
    // marker, which is U+FFFC (a real character, not a noncharacter).
    let forms: Vec<_> = CharacterCatalog::Markers.members().map(|m| m.stream_form).collect();
    let sentinel = |cp: u32| StreamForm::Sentinel(char::from_u32(cp).unwrap());
    assert_eq!(
        forms,
        [
            sentinel(0xFDD0), sentinel(0xFDD1), sentinel(0xFDD2), sentinel(0xFDD3), sentinel(0xFDD4),
            sentinel(0xFDD5), StreamForm::Unicode('\u{FFFC}'), sentinel(0xFDD6), sentinel(0xFDD7),
        ]
    );
    for m in CharacterCatalog::Markers.members() {
        assert_eq!(m.flags, CompositionFlags::default());
        assert_eq!(insert_catalog_member("x", 0, m.id).unwrap(), format!("{}x", m.scalar()));
    }
}

#[test]
fn mt_31008_break_characters_keep_distinct_targets() {
    assert_eq!(
        labels(CharacterCatalog::BreakCharacters),
        [
            "forced line break", "column break", "frame break", "page break", "odd-page break",
            "even-page break", "paragraph return",
        ]
    );
    let targets: Vec<_> = CharacterCatalog::BreakCharacters.members().map(|m| m.break_target).collect();
    assert_eq!(
        targets,
        [
            Some(BreakTarget::Line), Some(BreakTarget::Column), Some(BreakTarget::Frame),
            Some(BreakTarget::Page), Some(BreakTarget::OddPage), Some(BreakTarget::EvenPage),
            Some(BreakTarget::Paragraph),
        ]
    );
    // LINE SEPARATOR and PARAGRAPH SEPARATOR, the conventions layout already reads (find/change
    // `\n` -> U+2028; TOC trims U+2029).
    assert_eq!(get("breaks.forced_line").scalar(), '\u{2028}');
    assert_eq!(get("breaks.paragraph_return").scalar(), '\u{2029}');
    assert!(CharacterCatalog::BreakCharacters.members().all(|m| m.flags.break_opportunity));
    assert!(CATALOG.iter().filter(|m| m.catalog != CharacterCatalog::BreakCharacters).all(|m| m.break_target.is_none()));
}

#[test]
fn mt_31009_white_space_members_and_composition_flags() {
    bounded_case("mt_31009_white_space_members_and_composition_flags", || {
        assert_eq!(
            labels(CharacterCatalog::WhiteSpaceCharacters),
            [
                "em space", "en space", "third space", "quarter space", "sixth space", "thin space",
                "hair space", "figure space", "punctuation space", "flush space",
                "non-breaking space (fixed width)", "non-breaking space (flexible width)",
            ]
        );
        assert_eq!(
            scalars(CharacterCatalog::WhiteSpaceCharacters),
            [
                0x2003, 0x2002, 0x2004, 0x2005, 0x2006, 0x2009, 0x200A, 0x2007, 0x2008, 0xFDDD, 0x202F,
                0x00A0,
            ]
        );
        let flush = get("white_space.flush").flags;
        let fixed = get("white_space.non_breaking_fixed").flags;
        let flexible = get("white_space.non_breaking_flexible").flags;
        assert!(flush.absorbs_final_line_slack && !flush.stretches);
        assert!(!fixed.stretches && !fixed.break_opportunity && !fixed.absorbs_final_line_slack);
        assert!(flexible.stretches && !flexible.break_opportunity && !flexible.absorbs_final_line_slack);
        assert!(CATALOG.iter().filter(|m| m.id != "white_space.flush").all(|m| !m.flags.absorbs_final_line_slack));
        assert!(CATALOG.iter().filter(|m| m.id != "white_space.non_breaking_flexible").all(|m| !m.flags.stretches));
        assert!(!get("white_space.figure").flags.break_opportunity, "UAX #14 GL");
        // fontTools cmap glyph ids of Inter for the real space characters.
        let inter = [1689, 1688, 1690, 1691, 1692, 1695, 1696, 1693, 1694, 1684, 1683];
        let fonts = Fonts::load();
        let real: Vec<_> = CharacterCatalog::WhiteSpaceCharacters.members().filter(|m| !m.is_sentinel()).collect();
        assert_eq!(real.len(), inter.len());
        for (member, gid) in real.into_iter().zip(inter) {
            let text = insert_catalog_member("ab", 1, member.id).unwrap();
            let glyphs = inter_glyphs(&fonts, &text);
            assert_eq!(glyphs.len(), 3, "{}", member.id);
            assert_eq!(glyphs[1], gid, "{}", member.id);
        }
    });
}
