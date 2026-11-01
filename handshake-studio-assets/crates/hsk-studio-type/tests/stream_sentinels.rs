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

#[test]
fn mt_31005_stream_form_helpers_agree_with_the_catalog_table() {
    // The range helpers are checked against the member table, not the other way round.
    for member in CATALOG {
        let c = member.scalar();
        assert_eq!(
            is_break_sentinel(c),
            member.catalog == CharacterCatalog::BreakCharacters && member.is_sentinel(),
            "{}",
            member.id
        );
        assert_eq!(is_sentinel_block(c), member.is_sentinel(), "{}", member.id);
        assert!(!is_reserved_sentinel(c), "{} uses no reserved code point", member.id);
        assert_eq!(
            catalog_member_for_scalar(c).map(|m| m.id),
            Some(member.id),
            "no two members share a stream character"
        );
    }
    let breaks: Vec<u32> = (0xFDD0..=0xFDEF).filter(|cp| is_break_sentinel(ch(*cp))).collect();
    assert_eq!(breaks, BREAK_SENTINELS);
    // Block edges: 32 noncharacters, 14 assigned, 18 reserved.
    assert_eq!((0xFDCF..=0xFDF0).filter(|cp| is_sentinel_block(ch(*cp))).count(), 32);
    assert_eq!((0xFDCF..=0xFDF0).filter(|cp| is_reserved_sentinel(ch(*cp))).count(), 18);
    assert!(!is_sentinel_block('\u{FDCF}') && !is_sentinel_block('\u{FDF0}'));
    assert!(
        is_reserved_sentinel('\u{FDDE}') && is_reserved_sentinel('\u{FDEF}') && !is_reserved_sentinel('\u{FDDD}')
    );
    // Private-use code points are not used, and U+2001 is not a member.
    assert!(CATALOG.iter().all(|m| !('\u{E000}'..='\u{F8FF}').contains(&m.scalar())));
    assert!(catalog_member_for_scalar('\u{2001}').is_none());
    assert_eq!(catalog_member_for_scalar('\u{FFFC}').map(|m| m.id), Some("markers.anchored_object"));
}

#[test]
fn mt_31005_reserved_code_points_refuse_on_literal_insertion() {
    for cp in 0xFDDE..=0xFDEF_u32 {
        let literal = format!("x{}y", ch(cp));
        assert_eq!(insert_literal_text("ab", 1, &literal), Err(CatalogError::ReservedSentinel), "U+{cp:04X}");
    }
    assert_eq!(CatalogError::ReservedSentinel.code(), "reserved_stream_sentinel");
    // The refusal leaves the borrowed text untouched; the neighbouring code points insert.
    let text = String::from("ab");
    assert!(insert_literal_text(&text, 1, "\u{FDDE}").is_err());
    assert_eq!(text, "ab");
    assert_eq!(insert_literal_text("ab", 1, "\u{FDF0}\u{FDDD}").unwrap(), "a\u{FDF0}\u{FDDD}b");
    assert_eq!(insert_literal_text("ab", 1, "plain").unwrap(), "aplainb");
    // Offsets are still checked (inside a multi-byte character, past the end).
    assert_eq!(insert_literal_text("\u{e9}", 1, "x"), Err(CatalogError::InvalidOffset));
    assert_eq!(insert_literal_text("ab", 3, "x"), Err(CatalogError::InvalidOffset));
    // No member id inserts a reserved code point.
    for member in CATALOG {
        assert!(!insert_catalog_member("", 0, member.id).unwrap().chars().any(is_reserved_sentinel));
    }
}

#[test]
fn mt_31005_foreign_noncharacters_are_replaced_and_the_loss_is_reported() {
    // Every code point of U+FDD0..U+FDEF arriving in foreign text is replaced, never read as a
    // sentinel, and each loss is reported with its byte offset.
    for cp in 0xFDD0..=0xFDEF_u32 {
        let foreign = format!("a{}b", ch(cp));
        let imported = import_foreign_text(&foreign);
        assert_eq!(imported.text, "a\u{FFFD}b", "U+{cp:04X}");
        assert!(imported.is_lossy());
        assert_eq!(imported.replaced_count, 1);
        assert_eq!(imported.replaced, [ReplacedSentinel { offset: 1, code_point: ch(cp) }]);
        assert!(!imported.text.chars().any(is_sentinel_block));
    }
    // Text without a sentinel-block code point imports unchanged and lossless, including the
    // real characters and the noncharacters SD-29 does not cover.
    let clean = "plain \u{FFFC}\u{2028}\u{2029}\u{FFFE}\u{FFFF}\u{10FFFF} \u{FDCF}\u{FDF0}\u{E000}";
    let imported = import_foreign_text(clean);
    assert_eq!(imported.text, clean);
    assert!(!imported.is_lossy() && imported.replaced.is_empty());
    // Studio's own sentinel is a foreign sentinel as soon as it is re-imported as foreign text.
    let own = insert_catalog_member("ab", 1, "breaks.page").unwrap();
    let imported = import_foreign_text(&own);
    assert_eq!(imported.text, "a\u{FFFD}b");
    assert_eq!(imported.replaced, [ReplacedSentinel { offset: 1, code_point: '\u{FDDA}' }]);
    // Hostile volume: the count stays exact, the listing is bounded, nothing is dropped from text.
    let total = MAX_REPORTED_IMPORT_LOSSES + 904;
    let many = "\u{FDD0}".repeat(total);
    let imported = import_foreign_text(&many);
    assert_eq!(imported.replaced_count, total);
    assert_eq!(imported.replaced.len(), MAX_REPORTED_IMPORT_LOSSES);
    assert_eq!(imported.replaced.last().unwrap().offset, (MAX_REPORTED_IMPORT_LOSSES - 1) * 3);
    assert_eq!(imported.text.chars().count(), total);
    assert!(imported.text.chars().all(|c| c == '\u{FFFD}'));
}

#[test]
fn mt_31005_marker_sentinels_are_never_shaped_as_a_guessed_glyph() {
    bounded_case("mt_31005_marker_sentinels_are_never_shaped_as_a_guessed_glyph", || {
        let fonts = Fonts::load();
        // A marker or flush sentinel carries no text and no font maps it: the shaper refuses the
        // span (MissingGlyph) instead of drawing .notdef; layout resolves markers before shaping.
        for member in CATALOG.iter().filter(|m| m.is_sentinel() && !is_break_sentinel(m.scalar())) {
            let text = format!("ab{}cd", member.scalar());
            let case = paragraph_case(&text, json!([[0, 7]]));
            with_case(&fonts, &case, None, |request, ports, _, _, _| {
                let refused = TextEngine.shaped_runs(request, ports).err().expect("refused");
                assert_eq!(refused.error, Error::MissingGlyph, "{}", member.id);
            });
        }
    });
}

struct Frames(Vec<Vec<u8>>);
impl obs::SinkPort for Frames {
    fn try_send(&mut self, _: obs::DeliveryClass, bytes: &[u8]) -> Result<(), obs::DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}

#[test]
fn mt_31005_catalog_outcomes_reach_observe_as_fixed_codes_only() {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let cancel = CancellationToken::default();
    let mut sink = Frames(Vec::new());
    let observer = |correlation: u64| {
        obs::Observe::new(correlation, 3, id("STXT"), actor.clone(), obs::Budget::new(0, 0).unwrap())
    };
    let receipt = report_catalog_outcome(&mut observer(1), &mut sink, 1, 3, Ok(()), &cancel).unwrap();
    assert_eq!(receipt.outcome, obs::Outcome::Success);
    let refusal = insert_literal_text("ab", 1, "\u{FDDE}").unwrap_err();
    let receipt = report_catalog_outcome(&mut observer(2), &mut sink, 2, 3, Err(&refusal), &cancel).unwrap();
    assert_eq!(receipt.outcome, obs::Outcome::Failure(obs::FailureCode::Validation));
    for error in [CatalogError::UnknownMember, CatalogError::InvalidOffset, CatalogError::ReservedSentinel] {
        assert_eq!(catalog_failure_code(&error), obs::FailureCode::Validation);
    }
    let lossless = import_foreign_text("plain");
    let receipt = report_import_outcome(&mut observer(3), &mut sink, 3, 3, &lossless, &cancel).unwrap();
    assert_eq!(receipt.outcome, obs::Outcome::Success);
    let lossy = import_foreign_text("a\u{FDD0}b");
    let receipt = report_import_outcome(&mut observer(4), &mut sink, 4, 3, &lossy, &cancel).unwrap();
    assert_eq!(receipt.outcome, obs::Outcome::Failure(obs::FailureCode::Loss));
    assert_eq!(sink.0.len(), 4);
    for frame in &sink.0 {
        for needle in [&b"plain"[..], "\u{FDD0}".as_bytes(), "\u{FFFD}".as_bytes(), b"ab"] {
            assert!(!frame.windows(needle.len()).any(|w| w == needle), "no text in Observe frames");
        }
    }
}

#[test]
fn descriptor_names_the_assigned_stream_forms() {
    let d: Value = serde_json::from_str(descriptor()).unwrap();
    let catalogs = d["features"]["catalogs"].as_str().unwrap();
    assert!(catalogs.contains("U+FDD0..U+FDDD") && catalogs.contains("U+FFFC") && catalogs.contains("U+FDDE..U+FDEF"));
    assert!(!catalogs.contains("Pending") && !catalogs.contains("pending"), "{catalogs}");
}
