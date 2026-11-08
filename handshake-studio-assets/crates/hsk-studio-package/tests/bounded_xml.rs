//! SMB-PARSER-HOSTILE-1 item 2: the element-nesting scan run before every roxmltree parse of file
//! text (`src/bounded_xml.rs`). Table of shapes, including comments, CDATA, processing
//! instructions, declarations and attribute values that hold `<` or `>` (V4-08: escape, decoy,
//! whitespace). The differential against roxmltree's own tree lives in
//! `hsk-studio-interop-vector-svg/tests/hostile_xml_depth.rs`.
use hsk_studio_package::{XML_MAX_NESTING, xml_nesting_within};

fn nested(depth: usize) -> String {
    format!("{}{}", "<a>".repeat(depth), "</a>".repeat(depth))
}

#[test]
fn nesting_table() {
    // (text, cap, within)
    let cases: Vec<(String, usize, bool)> = vec![
        (String::new(), 0, true),
        ("plain text, no markup".into(), 0, true),
        // An empty element is one level (the parser enters it) that does not stay open.
        ("<a/>".into(), 0, false),
        ("<a/>".into(), 1, true),
        ("<a/><a/><a/>".into(), 1, true),
        ("<a></a>".into(), 0, false),
        ("<a></a>".into(), 1, true),
        (nested(3), 3, true),
        (nested(3), 2, false),
        // Siblings do not add up.
        ("<r><a></a><a></a><a/></r>".into(), 2, true),
        ("<r><a><b/></a></r>".into(), 2, false),
        ("<r><a></a><a></a><a/></r>".into(), 1, false),
        // Openers never closed count (the parser recurses before it finds the missing ends).
        ("<a><a><a>".into(), 2, false),
        // More closers than openers saturate at zero.
        ("</a></a></a><a><a></a></a>".into(), 2, true),
        ("</a></a></a><a><a></a></a>".into(), 1, false),
        // Comments, CDATA, processing instructions and declarations hide their brackets.
        ("<r><!-- <a><a><a> --></r>".into(), 1, true),
        ("<r><![CDATA[<a><a><a>]]></r>".into(), 1, true),
        ("<?xml version=\"1.0\"?><?pi <a><a>?><r/>".into(), 1, true),
        ("<!DOCTYPE r [ <!ENTITY e \"<a><a><a>\"> ]><r></r>".into(), 1, true),
        ("<!DOCTYPE r SYSTEM \"x>y\"><r></r>".into(), 1, true),
        // Attribute values may hold `>` and `/>` without ending or emptying the tag.
        ("<r a=\"x/>\"><b c='>'></b></r>".into(), 2, true),
        ("<r a=\"x/>\"><b c='>'></b></r>".into(), 1, false),
        ("<r a=\"/\"></r>".into(), 0, false),
        // Only `/` right before `>` is an empty-element tag; whitespace between counts a level.
        ("<r/ >".into(), 0, false),
        ("<r a=\"1\"/>".into(), 1, true),
        ("<r a=\"1\"/>".into(), 0, false),
        // Upper-case and spacing variants of the constructs.
        ("<R\n\tA = \"1\"\n></R >".into(), 1, true),
        ("<!-- unterminated <a><a><a>".into(), 0, true),
        ("<r><![CDATA[ unterminated <a><a>".into(), 1, true),
        ("<r a=\"unterminated <a><a><a>".into(), 0, true),
        // `<` that starts no known construct is measured as a start tag (conservative).
        ("<1><2>".into(), 1, false),
    ];
    for (text, cap, within) in &cases {
        assert_eq!(xml_nesting_within(text, *cap), *within, "{text:?} cap {cap}");
    }
}

#[test]
fn the_cap_is_inclusive_at_the_declared_number() {
    assert!(xml_nesting_within(&nested(XML_MAX_NESTING), XML_MAX_NESTING));
    assert!(!xml_nesting_within(&nested(XML_MAX_NESTING + 1), XML_MAX_NESTING));
    // Deep text is answered without recursion: 1,000,000 levels on the test thread.
    assert!(!xml_nesting_within(&"<g>".repeat(1_000_000), XML_MAX_NESTING));
}
