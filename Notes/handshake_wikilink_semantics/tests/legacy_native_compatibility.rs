use handshake_wikilink_semantics::parser::{self as canonical, legacy_native, WikilinkKind};

#[test]
fn first_match_native_api_keeps_surrounding_text_and_multiple_tokens() {
    for text in ["prefix [[wp:X]] suffix", "[[wp:X]] [[note:Y]]"] {
        let parsed = legacy_native::parse_wikilink(text).unwrap();
        assert_eq!(parsed, canonical::parse_wikilink("[[wp:X]]").unwrap());
        assert_eq!(parsed.ref_value, "X");
        assert!(parsed.resolved);
    }
}

#[test]
fn unicode_prefix_remains_unknown_and_mixed_extraction_keeps_every_token() {
    let unknown = legacy_native::parse_wikilink("[[noté:X]]").unwrap();
    assert_eq!(unknown.raw_prefix, "noté");
    assert_eq!(unknown.kind, WikilinkKind::Unknown("noté".into()));
    assert!(!unknown.resolved);
    assert_eq!(unknown.ref_value, "X");
    let link = unknown.to_hs_link();
    assert_eq!(link.ref_kind, "unknown");
    assert!(!link.resolved);
    assert_eq!(link.target_kind, None);
    assert_eq!(link.target_block_id, None);
    assert_eq!(link.tag_ref_kind, None);
    let tokens = legacy_native::extract_wikilinks("start [[noté:X]] then [[wp:WP-1|Name]] end");
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0], unknown);
    assert_eq!(
        tokens[1],
        canonical::parse_wikilink("[[wp:WP-1|Name]]").unwrap()
    );
    assert!(legacy_native::wikilink_regex().is_match("[[noté:X]]"));
    assert!(legacy_native::parse_wikilink("[[é:X]]").is_none());
}

#[test]
fn full_code_locus_and_alias_tokens_preserve_values_and_metadata() {
    for (token, kind, value, label) in [
        (
            "[[code:src/main.rs#Thing|Thing alias]]",
            "code",
            "src/main.rs#Thing",
            "Thing alias",
        ),
        (
            "[[locus:artifact-1|Artwork]]",
            "locus",
            "artifact-1",
            "Artwork",
        ),
        ("[[note:note-id|#Raw 全文]]", "note", "note-id", "#Raw 全文"),
    ] {
        let parsed = legacy_native::parse_wikilink(token).unwrap();
        assert_eq!(parsed, canonical::parse_wikilink(token).unwrap());
        let link = parsed.to_hs_link();
        assert_eq!(link.ref_kind, kind);
        assert_eq!(link.ref_value, value);
        assert_eq!(link.label, label);
        assert!(link.resolved);
        assert_eq!(link.provenance, None);
    }
}

#[test]
fn canonical_ascii_whole_token_defaults_and_regex_remain_strict() {
    for text in [
        "prefix [[wp:X]] suffix",
        "[[wp:X]] [[note:Y]]",
        "[[noté:X]]",
    ] {
        assert!(canonical::parse_wikilink(text).is_none());
    }
    assert!(!canonical::wikilink_regex().is_match("[[noté:X]]"));
    let tokens = canonical::extract_wikilinks("[[noté:X]] [[wp:X]]");
    assert_eq!(tokens, vec![canonical::parse_wikilink("[[wp:X]]").unwrap()]);
    assert_eq!(
        legacy_native::open_wikilink_query("text [[wp:"),
        canonical::open_wikilink_query("text [[wp:")
    );
}
