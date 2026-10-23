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
