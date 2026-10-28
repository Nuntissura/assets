use handpick::{parse_query, QueryError, QueryPurpose};

#[test]
fn query_common_filters_quotes_and_unicode_are_retained() {
    let query = parse_query("TYPE:notes tag:\"#Project Ideas\" in:body is:todo \"Élan vital\" 猫").unwrap();
    assert_eq!(query.result_type.as_deref(), Some("notes"));
    assert_eq!(query.tag.as_deref(), Some("project ideas"));
    assert_eq!(query.area.as_deref(), Some("body"));
    assert_eq!(query.task.as_deref(), Some("todo"));
    assert_eq!(query.words, ["élan vital", "猫"]);
    assert!(!query.incomplete);
    assert_eq!(query.purpose, QueryPurpose::Navigation);
}

#[test]
fn query_purposes_are_explicit_and_never_execute() {
    assert_eq!(parse_query("write an idea").unwrap().purpose, QueryPurpose::SearchWrite);
    assert_eq!(parse_query("> open settings").unwrap().purpose, QueryPurpose::Commands);
    for (text, purpose) in [("type:command open", QueryPurpose::Commands), ("type:settings font", QueryPurpose::Settings),
        ("type:file image", QueryPurpose::Navigation), ("type:folder plans", QueryPurpose::Navigation)] {
        assert_eq!(parse_query(text).unwrap().purpose, purpose);
    }
}

#[test]
fn query_unsupported_and_incomplete_lookups_remain_honest() {
    let query = parse_query("type:spaceship in: is:perhaps tag:\"unfinished name").unwrap();
    assert!(query.incomplete);
    assert_eq!(query.unsupported, ["type:spaceship", "is:perhaps"]);
    assert!(query.words.contains(&"type:spaceship".into()));
    assert_eq!(query.purpose, QueryPurpose::Navigation);
    let quoted_prose = parse_query("\"unfinished writing").unwrap();
    assert!(quoted_prose.incomplete);
    assert_eq!(quoted_prose.purpose, QueryPurpose::SearchWrite);
    assert_eq!(parse_query("type:").unwrap().purpose, QueryPurpose::Navigation);
}

#[test]
fn query_transport_and_resource_bounds_are_explicit() {
    let query = parse_query("type:file hello").unwrap();
    let json = serde_json::to_value(&query).unwrap();
    assert_eq!(json["type"], "file");
    assert_eq!(json["purpose"], "navigation");
    assert_eq!(serde_json::from_value::<handpick::ParsedQuery>(json).unwrap(), query);
    assert_eq!(parse_query(&"x".repeat(65537)), Err(QueryError::TextLimit));
    assert_eq!(parse_query(&"x ".repeat(1025)), Err(QueryError::TokenLimit));
}

#[test]
fn quoted_query_escapes_preserve_literal_operator_and_unicode_meanings() {
    let escaped_name = "quote\" slash\\ and\tline\n";
    let quoted = serde_json::to_string(escaped_name).unwrap();
    let query = parse_query(&format!("tag:{quoted} type:note in:body is:todo")).unwrap();
    assert_eq!(query.tag.as_deref(), Some(escaped_name)); assert!(!query.incomplete);
    assert_eq!(query.result_type.as_deref(), Some("note"));
    assert_eq!(query.area.as_deref(), Some("body")); assert_eq!(query.task.as_deref(), Some("todo"));
    let literal = parse_query(&serde_json::to_string("type:note \"quoted\" \\path").unwrap()).unwrap();
    assert_eq!(literal.words, ["type:note \"quoted\" \\path"]);
    assert_eq!(literal.result_type, None); assert!(!literal.incomplete);
    let unicode = parse_query(r#"tag:"\u0065\u0301\uD83D\uDE42""#).unwrap();
    assert_eq!(unicode.tag.as_deref(), Some("é🙂")); assert!(!unicode.incomplete);
    let slash = parse_query(r#"tag:"a\/b""#).unwrap();
    assert_eq!(slash.tag.as_deref(), Some("a/b")); assert!(!slash.incomplete);
    // Existing optional # syntax remains supported; an escaped # is a literal name character.
    assert_eq!(parse_query("tag:#Project").unwrap().tag.as_deref(), Some("project"));
    assert_eq!(parse_query("tag:\"#Project Ideas\"").unwrap().tag.as_deref(), Some("project ideas"));
    assert_eq!(parse_query(r#"tag:"\u0023Project""#).unwrap().tag.as_deref(), Some("#project"));
    // An unquoted backslash remains an ordinary literal, not an escape sequence.
    assert_eq!(parse_query("tag:a\\b").unwrap().tag.as_deref(), Some("a\\b"));
}

#[test]
fn partial_and_malformed_quoted_escapes_never_become_complete_filter_identity() {
    for input in [r#"tag:"a\q""#, r#"tag:"\uD800""#, r#"tag:"\uDC00""#, r#"tag:"\u00zz""#] {
        let parsed = parse_query(input).unwrap();
        assert!(parsed.incomplete, "{input}"); assert_eq!(parsed.tag, None, "{input}");
        assert_eq!(parsed.unsupported, [input]); assert_eq!(parsed.purpose, QueryPurpose::Navigation);
    }
    for input in ["tag:\"unfinished name", "tag:\"escaped\\\" quote", "tag:\"trailing\\", "\"unfinished phrase"] {
        let parsed = parse_query(input).unwrap(); assert!(parsed.incomplete, "{input}");
    }
    assert_eq!(parse_query("\"unfinished phrase").unwrap().words, ["unfinished phrase"]);
    assert_eq!(parse_query("tag:\"unfinished name").unwrap().tag.as_deref(), Some("unfinished name"));
    assert_eq!(parse_query("type:spaceship in: is:perhaps").unwrap().unsupported, ["type:spaceship", "is:perhaps"]);
}
