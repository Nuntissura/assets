use crate::{canonical_json_bytes, sha256_hex};
use serde_json::{json, Value};

#[test]
fn sorts_object_keys_recursively_without_reordering_arrays() {
    let value = json!({
        "z": [{"z": 2, "a": 1}, 3, 1],
        "a": {"y": false, "b": null},
    });

    assert_eq!(
        canonical_json_bytes(&value),
        br#"{"a":{"b":null,"y":false},"z":[{"a":1,"z":2},3,1]}"#
    );
}

#[test]
fn serializes_empty_containers_and_scalar_literals() {
    assert_eq!(
        canonical_json_bytes(&json!([null, true, false, [], {}])),
        b"[null,true,false,[],{}]"
    );
}

#[test]
fn escapes_only_quote_backslash_newline_carriage_return_and_tab() {
    let value = Value::String("\"\\\n\r\t/".into());

    assert_eq!(canonical_json_bytes(&value), br#""\"\\\n\r\t/""#);
}

#[test]
fn preserves_literal_control_characters_outside_the_escape_set() {
    let value = Value::String("\0\u{0001}\u{0008}\u{000c}\u{001f}".into());

    assert_eq!(
        canonical_json_bytes(&value),
        b"\"\x00\x01\x08\x0c\x1f\""
    );
}

#[test]
fn preserves_unicode_without_normalizing_or_ascii_escaping() {
    let value = Value::String("é e\u{0301} Ω 😀 \u{2028}\u{2029}".into());

    assert_eq!(
        canonical_json_bytes(&value),
        "\"é e\u{0301} Ω 😀 \u{2028}\u{2029}\"".as_bytes()
    );
}

#[test]
fn sorts_unicode_keys_and_escapes_keys_like_string_values() {
    let value = json!({"Ω": 4, "é": 3, "a": 2, "\n": 1});

    assert_eq!(
        canonical_json_bytes(&value),
        "{\"\\n\":1,\"a\":2,\"é\":3,\"Ω\":4}".as_bytes()
    );
}

#[test]
fn preserves_integer_limits_and_float_number_serialization() {
    let value = json!([i64::MIN, u64::MAX, 0, 1.0, 1.5, -0.0]);

    assert_eq!(
        canonical_json_bytes(&value),
        b"[-9223372036854775808,18446744073709551615,0,1.0,1.5,-0.0]"
    );
}

#[test]
fn hashes_exact_input_bytes_as_lowercase_sha256_hex() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
}
