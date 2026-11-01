//! Lane SMB-TYPE-SENTINELS-REGISTRY-1, item 1: STU-TYP-207 stream sentinels (v02.228, SD-29).
//!
//! The expected code points below are the spec's "Stored as" column, typed in from the Master
//! Spec v02.229 table, not read from this crate. Normalisation facts (noncharacters and U+FFFC are
//! unchanged by NFC/NFD/NFKC/NFKD, U+2001 EM QUAD becomes U+2003) were produced with Python
//! unicodedata (Unicode 16.0.0) and are pinned as data, because the crate ships no normaliser
//! (`Normalization::Preserve` only). Vendor behaviour for sentinels is UNVERIFIED (D5).
#![allow(dead_code)]
include!("support/mod.rs");

use serde_json::json;

/// Spec table: (member id in this crate, member label, catalog, stored as).
const TABLE: [(&str, &str, &str, u32); 15] = [
    ("markers.current_page_number", "current page number", "Markers", 0xFDD0),
    ("markers.next_page_number", "next page number", "Markers", 0xFDD1),
    ("markers.previous_page_number", "previous page number", "Markers", 0xFDD2),
    ("markers.section_marker", "section marker", "Markers", 0xFDD3),
    ("markers.footnote_reference", "footnote reference", "Markers", 0xFDD4),
    ("markers.endnote_reference", "endnote reference", "Markers", 0xFDD5),
    ("markers.anchored_object", "anchored-object marker", "Markers", 0xFFFC),
    ("markers.cross_reference", "cross-reference marker", "Markers", 0xFDD6),
    ("markers.index", "index marker", "Markers", 0xFDD7),
    ("breaks.column", "column break", "Break characters", 0xFDD8),
    ("breaks.frame", "frame break", "Break characters", 0xFDD9),
    ("breaks.page", "page break", "Break characters", 0xFDDA),
    ("breaks.odd_page", "odd-page break", "Break characters", 0xFDDB),
    ("breaks.even_page", "even-page break", "Break characters", 0xFDDC),
    ("white_space.flush", "flush space", "White-space characters", 0xFDDD),
];
const BREAK_SENTINELS: [u32; 5] = [0xFDD8, 0xFDD9, 0xFDDA, 0xFDDB, 0xFDDC];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn get(id: &str) -> &'static CatalogMember {
    catalog_member(id).unwrap_or_else(|| panic!("{id}"))
}
fn ch(cp: u32) -> char {
    char::from_u32(cp).unwrap()
}

#[test]
fn mt_31005_each_sentinel_member_inserts_its_assigned_code_point() {
    for (id, label, catalog, cp) in TABLE {
        let member = get(id);
        assert_eq!(member.label, label, "{id} keeps the spec's member name");
        assert_eq!(member.catalog.label(), catalog, "{id} stays in its catalog");
        let inserted = insert_catalog_member("ab", 1, id).unwrap_or_else(|e| panic!("{id}: {e:?}"));
        assert_eq!(inserted, format!("a{}b", ch(cp)), "{id} is stored as U+{cp:04X}");
    }
    // No row is added or silently mapped onto another: the 15 stream characters are distinct.
    let mut stored: Vec<u32> = TABLE.iter().map(|row| row.3).collect();
    stored.sort_unstable();
    stored.dedup();
    assert_eq!(stored.len(), 15);
    // Members that already had a Unicode character keep it (SD-29: stored as that scalar).
    for (id, cp) in [("breaks.forced_line", 0x2028), ("breaks.paragraph_return", 0x2029)] {
        assert_eq!(insert_catalog_member("", 0, id).unwrap(), ch(cp).to_string(), "{id}");
    }
}

/// Python unicodedata 16.0.0: NFC/NFD/NFKC/NFKD of each of these code points is the code point
/// itself (no decomposition, no composition, not a compatibility character).
const NORMALISATION_STABLE: [u32; 14] = [
    0xFDD0, 0xFDD1, 0xFDD2, 0xFDD3, 0xFDD4, 0xFDD5, 0xFDD6, 0xFDD7, 0xFDD8, 0xFDD9, 0xFDDA, 0xFDDB,
    0xFDDC, 0xFDDD,
];

#[test]
fn mt_31005_sentinels_and_object_replacement_survive_all_normalisation_forms() {
    // U+FFFC is a real character with no decomposition; the fourteen others are noncharacters
    // that no normalisation form changes (oracle above).
    let mut covered = Vec::new();
    for (id, _, _, cp) in TABLE {
        let inserted = insert_catalog_member("", 0, id).unwrap();
        let stream: Vec<u32> = inserted.chars().map(u32::from).collect();
        assert_eq!(stream, [cp], "{id} is exactly one stream character");
        let stable = cp == 0xFFFC || NORMALISATION_STABLE.contains(&cp);
        assert!(stable, "{id}: U+{cp:04X} is normalisation-stable per the oracle");
        covered.push(cp);
    }
    covered.sort_unstable();
    let mut expected: Vec<u32> = NORMALISATION_STABLE.to_vec();
    expected.push(0xFFFC);
    expected.sort_unstable();
    assert_eq!(covered, expected);
    // Flush space is a sentinel and not U+2001 EM QUAD: the oracle maps U+2001 to U+2003 under
    // every form (NFKC/NFKD to U+0020), so U+2001 would not survive; flush space must differ
    // from it and from the em space that U+2001 turns into.
    let flush = insert_catalog_member("", 0, "white_space.flush").unwrap();
    assert_ne!(flush, "\u{2001}");
    assert_ne!(flush, "\u{2003}");
    assert_eq!(insert_catalog_member("", 0, "white_space.em").unwrap(), "\u{2003}");
}

fn text_with(sentinel: u32) -> String {
    format!("ab{}cd", ch(sentinel))
}

fn paragraph_case(text: &str, ranges: serde_json::Value) -> Value {
    json!({
        "case": "stream_sentinels", "text": text, "text_sha256": hex(&hash(text.as_bytes())),
        "text_utf8_hex": hex(text.as_bytes()), "font_index": 0, "script": "Latn", "language": "en",
        "paragraph_ranges": ranges,
    })
}

#[test]
fn mt_31008_each_break_sentinel_terminates_its_paragraph() {
    bounded_case("mt_31008_each_break_sentinel_terminates_its_paragraph", || {
        let fonts = Fonts::load();
        for cp in BREAK_SENTINELS {
            let text = text_with(cp);
            // "ab" + 3-byte sentinel ends the first paragraph at byte 5; "cd" is the second.
            assert_eq!(text.len(), 7);
            let case = paragraph_case(&text, json!([[0, 5], [5, 7]]));
            with_case(&fonts, &case, None, |request, ports, _, _, _| {
                let result = prepared(request, ports);
                assert_eq!(result.paragraphs().len(), 2, "U+{cp:04X} splits two paragraphs");
                let separators: Vec<_> = result
                    .coverage()
                    .iter()
                    .filter(|c| c.kind == CoverageKind::ParagraphSeparator)
                    .map(|c| (c.source.start, c.source.end))
                    .collect();
                assert_eq!(separators, [(2, 5)], "U+{cp:04X} is the terminator, outside shaping");
            });
            // A caller partition that keeps the break inside one paragraph is refused: the
            // sentinel ends its paragraph exactly as U+2029 does.
            let whole = paragraph_case(&text, json!([[0, 7]]));
            with_case(&fonts, &whole, None, |request, ports, _, _, _| {
                let refused = TextEngine.shaped_runs(request, ports).err().expect("refused");
                assert_eq!(refused.error, Error::InvalidBidi, "U+{cp:04X}");
            });
        }
        // The reference terminator U+2029 behaves the same way (control for the harness).
        let control = "ab\u{2029}cd";
        let case = paragraph_case(control, json!([[0, 5], [5, 7]]));
        with_case(&fonts, &case, None, |request, ports, _, _, _| {
            assert_eq!(prepared(request, ports).paragraphs().len(), 2);
        });
    });
}
