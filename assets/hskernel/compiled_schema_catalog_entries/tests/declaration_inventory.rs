use compiled_schema_catalog_entries::{
    compiled_schema_catalog_entries, declarative_function_name, insert_catalog_identity,
    production_catalog_entries,
};
use std::collections::BTreeSet;

const ZERO: (usize, usize, usize, usize, usize, usize, usize, usize) = (0, 0, 0, 0, 0, 0, 0, 0);

// Actual declaration from model_registry_schema.surql, SHA256
// 5ee10b0475ff45d9e370b0b5fa376a3b8b8161dbbc5ee48f727adc2f5c9489d6.
const ACTUAL_EVENT: &str = r#"DEFINE EVENT OVERWRITE engine_profile_registry_transition_guard
ON TABLE engine_profile_registry
WHEN $event IN ['CREATE', 'UPDATE']
THEN {
    IF !fn::wp012_engine_profile_row($after, true) {
        THROW 'ENGINE_PROFILE_TRANSITION_DENIED';
    };
    LET $payload = $after.receipt_id.payload;
    IF $after.authenticated_session_id != record::id($auth.id)
        OR $after.receipt_id.authority_session_id != $auth.id {
        THROW 'ENGINE_PROFILE_TRANSITION_DENIED';
    };
    IF $after.data != $payload.after {
        THROW 'ENGINE_PROFILE_TRANSITION_DENIED';
    };
    IF $event = 'CREATE' {
        IF $payload.before != NULL OR $after.data.registry_generation != 1 {
            THROW 'ENGINE_PROFILE_TRANSITION_DENIED';
        };
    } ELSE {
        IF $before.id != $after.id
            OR $before.owner_account_id != $after.owner_account_id
            OR $before.actor_principal_id != $after.actor_principal_id
            OR $before.access_space_id != $after.access_space_id
            OR $before.workspace_id != $after.workspace_id
            OR $before.receipt_id = $after.receipt_id
            OR $payload.before = NULL
            OR $before.data != $payload.before
            OR !type::is_int($before.data.registry_generation)
            OR !type::is_int($after.data.registry_generation)
            OR $after.data.registry_generation != $before.data.registry_generation + 1 {
            THROW 'ENGINE_PROFILE_TRANSITION_DENIED';
        };
    };
};"#;

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn strict_inventory_preserves_all_eight_counts_and_sorted_identities() {
    let schema = r#"
        DEFINE TABLE OVERWRITE items SCHEMAFULL;
        DEFINE TABLE OVERWRITE summary TYPE NORMAL AS SELECT * FROM items;
        DEFINE FIELD OVERWRITE value ON TABLE items TYPE string;
        DEFINE INDEX OVERWRITE by_value ON TABLE items FIELDS value;
        DEFINE EVENT OVERWRITE changed ON TABLE items WHEN true THEN {};
        DEFINE SEQUENCE OVERWRITE primary;
        DEFINE SEQUENCE IF NOT EXISTS backup;
        DEFINE ACCESS OVERWRITE login ON DATABASE TYPE RECORD;
        DEFINE ACCESS IF NOT EXISTS alternate ON DATABASE TYPE RECORD;
        DEFINE FUNCTION OVERWRITE fn::valid($value: string) { RETURN $value; };
    "#;
    let actual = compiled_schema_catalog_entries(schema, &["items", "summary"], (2, 1, 1, 1, 1, 2, 2, 1)).unwrap();
    assert_eq!(actual, strings(&[
        "access:alternate", "access:login", "event:items:changed", "field:items:value",
        "function:fn::valid", "index:items:by_value", "sequence:backup", "sequence:primary",
        "table:items", "table:summary", "view:summary",
    ]));
}

#[test]
fn original_helpers_preserve_borrowed_names_and_duplicate_errors() {
    assert_eq!(declarative_function_name("fn::name($x: string)"), Ok("fn::name"));
    assert_eq!(declarative_function_name("fn::missing"), Err("function declaration lacks argument opener: fn::missing".into()));
    assert_eq!(declarative_function_name("other()"), Err("invalid declarative function name: other()".into()));
    assert_eq!(declarative_function_name("fn::()"), Err("invalid declarative function name: fn::()".into()));
    let mut entries = BTreeSet::new();
    insert_catalog_identity(&mut entries, "table:items".into()).unwrap();
    assert_eq!(insert_catalog_identity(&mut entries, "table:items".into()), Err("duplicate declarative schema identity: table:items".into()));
    assert_eq!(entries.into_iter().collect::<Vec<_>>(), strings(&["table:items"]));
}

#[test]
fn strict_table_and_count_failures_retain_error_semantics() {
    let schema = "DEFINE TABLE OVERWRITE items;";
    assert_eq!(compiled_schema_catalog_entries(schema, &["different"], (1, 0, 0, 0, 0, 0, 0, 0)),
        Err("declarative table inventory differs from TABLE_NAMES: parsed=1; expected=1".into()));
    assert_eq!(compiled_schema_catalog_entries(schema, &["items"], (1, 1, 0, 0, 0, 0, 0, 0)),
        Err("declarative schema catalog counts differ: observed=(1, 0, 0, 0, 0, 0, 0, 0); expected=(1, 1, 0, 0, 0, 0, 0, 0)".into()));
}

#[test]
fn strict_duplicate_identities_reject_in_every_supported_category() {
    for (declaration, identity) in [
        ("DEFINE TABLE OVERWRITE t;", "table:t"),
        ("DEFINE TABLE OVERWRITE v TYPE NORMAL AS SELECT * FROM t;", "view:v"),
        ("DEFINE FIELD OVERWRITE f ON TABLE t TYPE string;", "field:t:f"),
        ("DEFINE INDEX OVERWRITE i ON TABLE t FIELDS f;", "index:t:i"),
        ("DEFINE EVENT OVERWRITE e ON TABLE t WHEN true THEN {};", "event:t:e"),
        ("DEFINE SEQUENCE IF NOT EXISTS s;", "sequence:s"),
        ("DEFINE ACCESS OVERWRITE a ON DATABASE TYPE RECORD;", "access:a"),
        ("DEFINE FUNCTION OVERWRITE fn::f() {};", "function:fn::f"),
    ] {
        let schema = format!("{declaration}\n{declaration}");
        assert_eq!(compiled_schema_catalog_entries(&schema, &[], ZERO),
            Err(format!("duplicate declarative schema identity: {identity}")));
    }
}

#[test]
fn strict_access_and_function_rejection_is_preserved() {
    for declaration in [
        "DEFINE ACCESS a ON DATABASE TYPE RECORD;",
        "DEFINE ACCESS OVERWRITE a ON NAMESPACE;",
        "DEFINE FUNCTION fn::f() {};",
        "DEFINE FUNCTION IF NOT EXISTS fn::f() {};",
    ] {
        assert_eq!(compiled_schema_catalog_entries(declaration, &[], ZERO),
            Err(format!("unsupported declarative authority definition: {declaration}")));
    }
    assert_eq!(compiled_schema_catalog_entries("DEFINE FUNCTION OVERWRITE fn::missing;", &[], ZERO),
        Err("function declaration lacks argument opener: fn::missing".into()));
}

#[test]
fn strict_mode_does_not_acquire_overlay_modifier_or_extra_kind_policies() {
    let schema = "DEFINE TABLE plain; DEFINE FIELD OVERWRITE f ON plain TYPE string; DEFINE ANALYZER a TOKENIZERS blank; DEFINE PARAM $p VALUE 1;";
    assert_eq!(compiled_schema_catalog_entries(schema, &[], ZERO).unwrap(), Vec::<String>::new());
    assert_eq!(production_catalog_entries(&[], &[("extra", schema)]).unwrap(),
        strings(&["analyzer:a", "field:plain:f", "param:$p", "table:plain"]));
}

#[test]
fn actual_multiline_event_and_one_line_header_match_in_both_public_modes() {
    let one_line = ACTUAL_EVENT.replace("transition_guard\nON TABLE", "transition_guard ON TABLE");
    assert_ne!(ACTUAL_EVENT, one_line);
    let identity = strings(&["event:engine_profile_registry:engine_profile_registry_transition_guard"]);
    for source in [ACTUAL_EVENT, one_line.as_str()] {
        assert_eq!(compiled_schema_catalog_entries(source, &[], (0, 0, 0, 1, 0, 0, 0, 0)).unwrap(), identity);
        assert_eq!(production_catalog_entries(&[], &[("actual", source)]).unwrap(), identity);
    }
}

#[test]
fn multiline_header_comments_work_for_fields_indexes_and_functions() {
    let schema = r#"
        DEFINE /* one /* nested */ comment */ TABLE OVERWRITE items;
        DEFINE FIELD OVERWRITE value
        -- A header continuation, not a new statement.
        ON /* target */ TABLE items TYPE string;
        DEFINE INDEX OVERWRITE by_value
        ON TABLE items FIELDS value;
        DEFINE FUNCTION OVERWRITE fn::identity
        /* argument boundary */ ($value: string) { RETURN $value; };
        DEFINE SEQUENCE IF
        NOT EXISTS sequence;
    "#;
    let strict = compiled_schema_catalog_entries(schema, &["items"], (1, 1, 1, 0, 0, 1, 0, 1)).unwrap();
    assert_eq!(strict, strings(&["field:items:value", "function:fn::identity", "index:items:by_value", "sequence:sequence", "table:items"]));
    assert_eq!(production_catalog_entries(&[], &[("same", schema)]).unwrap(), strict);
}

#[test]
fn overlay_preserves_all_kinds_modifiers_optional_table_and_deduplication() {
    let base = strings(&["table:base", "field:base:seed"]);
    let first = r#"
        DEFINE TABLE base;
        DEFINE FIELD seed ON base TYPE string;
        DEFINE INDEX OVERWRITE by_seed ON TABLE base FIELDS seed;
        DEFINE EVENT IF NOT EXISTS changed ON base WHEN true THEN {};
        DEFINE FUNCTION fn::f() {};
        DEFINE SEQUENCE s;
        DEFINE ACCESS a ON DATABASE TYPE RECORD;
        DEFINE ANALYZER tokenizer TOKENIZERS blank;
        DEFINE PARAM $limit VALUE 10;
    "#;
    let second = "DEFINE TABLE OVERWRITE base; DEFINE FIELD IF NOT EXISTS seed ON TABLE base TYPE int; DEFINE FUNCTION IF NOT EXISTS fn::f() {};";
    assert_eq!(production_catalog_entries(&base, &[("first", first), ("second", second)]).unwrap(),
        strings(&["access:a", "analyzer:tokenizer", "event:base:changed", "field:base:seed", "function:fn::f", "index:base:by_seed", "param:$limit", "sequence:s", "table:base"]));
    assert_eq!(base, strings(&["table:base", "field:base:seed"]));
}

#[test]
fn overlay_uses_caller_order_for_failure_and_does_not_sort_source_labels() {
    let first = "DEFINE UNKNOWN one;";
    let second = "DEFINE OTHER two;";
    assert_eq!(production_catalog_entries(&[], &[("z", first), ("a", second)]),
        Err(format!("unsupported production catalog declaration: {first}")));
    assert_eq!(production_catalog_entries(&[], &[("a", second), ("z", first)]),
        Err(format!("unsupported production catalog declaration: {second}")));
    assert_eq!(compiled_schema_catalog_entries(first, &[], ZERO).unwrap(), Vec::<String>::new());
}

#[test]
fn quoted_text_comments_and_nested_bodies_do_not_invent_declarations_or_views() {
    let schema = r#"
        -- DEFINE TABLE OVERWRITE comment_decoy;
        /* DEFINE TABLE OVERWRITE block_decoy; /* nested */ */
        LET $text = 'ignored; DEFINE TABLE OVERWRITE string_decoy;';
        DEFINE TABLE OVERWRITE visible COMMENT ' TYPE NORMAL AS DEFINE TABLE OVERWRITE ghost;';
        DEFINE FUNCTION OVERWRITE fn::guard() {
            LET $quoted = "escaped \"; DEFINE TABLE OVERWRITE ghost;";
            LET $object = { text: '}; DEFINE TABLE OVERWRITE ghost;' };
            IF true { DEFINE TABLE OVERWRITE nested_decoy; };
            RETURN /["';{}]/;
        };
        // DEFINE EVENT OVERWRITE false_event ON TABLE visible;
    "#;
    let expected = strings(&["function:fn::guard", "table:visible"]);
    assert_eq!(compiled_schema_catalog_entries(schema, &["visible"], (1, 0, 0, 0, 0, 0, 0, 1)).unwrap(), expected);
    assert_eq!(production_catalog_entries(&[], &[("same", schema)]).unwrap(), expected);
}

#[test]
fn quoted_identifiers_keep_exact_spelling_and_do_not_split_on_semicolons() {
    let schema = "DEFINE TABLE OVERWRITE `a; DEFINE TABLE ghost`; DEFINE FIELD OVERWRITE ⟨value name⟩ ON TABLE `a; DEFINE TABLE ghost` TYPE string;";
    let expected = strings(&["field:`a; DEFINE TABLE ghost`:⟨value name⟩", "table:`a; DEFINE TABLE ghost`"]);
    assert_eq!(compiled_schema_catalog_entries(schema, &["`a; DEFINE TABLE ghost`"], (1, 1, 0, 0, 0, 0, 0, 0)).unwrap(), expected);
    assert_eq!(production_catalog_entries(&[], &[("quoted", schema)]).unwrap(), expected);
}

#[test]
fn truncated_headers_and_invalid_modifiers_cannot_invent_target_identities() {
    for schema in [
        "DEFINE", "DEFINE EVENT OVERWRITE broken", "DEFINE FIELD f ON TABLE;",
        "DEFINE INDEX i ON", "DEFINE TABLE IF EXISTS invalid;",
        "DEFINE EVENT OVERWRITE broken\nDEFINE TABLE OVERWRITE next;",
        "DEFINE FUNCTION OVERWRITE fn::missing;",
    ] {
        assert!(production_catalog_entries(&[], &[("invalid", schema)]).is_err(), "{schema}");
    }
    for schema in ["DEFINE EVENT OVERWRITE broken", "DEFINE FIELD OVERWRITE f ON TABLE;", "DEFINE INDEX OVERWRITE i ON"] {
        assert!(compiled_schema_catalog_entries(schema, &[], ZERO).is_err(), "{schema}");
    }
}

#[test]
fn unterminated_lexical_regions_and_mismatched_delimiters_are_errors() {
    for source in [
        "/* missing", "LET $value = 'missing", "DEFINE TABLE OVERWRITE `missing",
        "DEFINE FUNCTION OVERWRITE fn::bad() {", "RETURN /[unterminated/;",
        "DEFINE FUNCTION OVERWRITE fn::bad() { RETURN ]; };",
    ] {
        assert!(compiled_schema_catalog_entries(source, &[], ZERO).is_err(), "{source}");
        assert!(production_catalog_entries(&[], &[("invalid", source)]).is_err(), "{source}");
    }
}

#[test]
fn overlay_identity_union_does_not_inherit_strict_duplicate_rejection() {
    let base = strings(&["table:existing", "table:existing"]);
    let schema = "DEFINE TABLE existing; DEFINE TABLE OVERWRITE existing; DEFINE TABLE IF NOT EXISTS existing;";
    assert_eq!(production_catalog_entries(&base, &[("repeat", schema), ("repeat", schema)]).unwrap(), strings(&["table:existing"]));
    assert_eq!(production_catalog_entries(&strings(&["table:z", "table:a"]), &[]).unwrap(), strings(&["table:a", "table:z"]));
}

#[test]
fn declaration_errors_preserve_encounter_order_before_later_lexical_errors() {
    let overlay = "DEFINE UNKNOWN first; LET $later = 'unterminated";
    assert_eq!(production_catalog_entries(&[], &[("same", overlay)]),
        Err("unsupported production catalog declaration: DEFINE UNKNOWN first;".into()));
    let strict = "DEFINE TABLE OVERWRITE t; DEFINE TABLE OVERWRITE t; /* unterminated";
    assert_eq!(compiled_schema_catalog_entries(strict, &["t"], (1, 0, 0, 0, 0, 0, 0, 0)),
        Err("duplicate declarative schema identity: table:t".into()));
}

#[test]
fn bracket_field_idioms_keep_complete_identity_in_both_inventory_policies() {
    for field in ["foo.*[*]...", "foo[0].bar[*]", "foo[WHERE active = true].bar"] {
        let schema = format!("DEFINE FIELD OVERWRITE {field} ON TABLE bar TYPE string;");
        let expected = strings(&[&format!("field:bar:{field}")]);
        assert_eq!(compiled_schema_catalog_entries(&schema, &[], (0, 1, 0, 0, 0, 0, 0, 0)).unwrap(), expected);
        assert_eq!(production_catalog_entries(&[], &[("bracket-field", &schema)]).unwrap(), expected);
    }
    let schema = "DEFINE FIELD IF NOT EXISTS foo.*[*]...\nON bar TYPE string;";
    assert_eq!(production_catalog_entries(&[], &[("optional-table", schema)]).unwrap(),
        strings(&["field:bar:foo.*[*]..."]));
}

#[test]
fn invalid_function_errors_keep_actual_signature_in_both_inventory_policies() {
    for signature in ["bad()", "bad($value:", "fn::()"] {
        let schema = if signature == "bad($value:" {
            "DEFINE FUNCTION OVERWRITE bad($value: string) {};".to_owned()
        } else {
            format!("DEFINE FUNCTION OVERWRITE {signature} {{}};")
        };
        let expected = Err(format!("invalid declarative function name: {signature}"));
        assert_eq!(compiled_schema_catalog_entries(&schema, &[], ZERO), expected);
        assert_eq!(production_catalog_entries(&[], &[("invalid-function", &schema)]), expected);
    }
}

#[test]
fn truncated_field_cannot_absorb_a_following_unquoted_define() {
    let schema = "DEFINE FIELD OVERWRITE broken\nDEFINE TABLE OVERWRITE next ON TABLE t;";
    let expected = Err(format!("invalid catalog declaration: {schema}"));
    assert_eq!(compiled_schema_catalog_entries(schema, &[], (0, 1, 0, 0, 0, 0, 0, 0)), expected);
    assert_eq!(production_catalog_entries(&[], &[("truncated-field", schema)]), expected);

    let quoted = "DEFINE FIELD OVERWRITE `DEFINE` ON TABLE t TYPE string;";
    let identities = strings(&["field:t:`DEFINE`"]);
    assert_eq!(compiled_schema_catalog_entries(quoted, &[], (0, 1, 0, 0, 0, 0, 0, 0)).unwrap(), identities);
    assert_eq!(production_catalog_entries(&[], &[("quoted-field", quoted)]).unwrap(), identities);
}
