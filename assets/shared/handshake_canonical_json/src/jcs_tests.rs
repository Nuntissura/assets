use crate::{jcs_bytes, jcs_from_slice};
use serde::{ser::SerializeMap, Serialize, Serializer};
use serde_json::json;

#[test]
fn rfc_8785_section_3_2_2_example_matches_exact_utf8_bytes() {
    let input = br#"{
        "numbers":[333333333.33333329,1E30,4.50,2e-3,0.000000000000000000000000001],
        "string":"\u20ac$\u000F\u000aA'\u0042\u0022\u005c\\\"\/",
        "literals":[null,true,false]
    }"#;
    let expected = r#"{"literals":[null,true,false],"numbers":[333333333.3333333,1e+30,4.5,0.002,1e-27],"string":"€$\u000f\nA'B\"\\\\\"/"}"#;
    assert_eq!(
        jcs_from_slice(input, input.len()).unwrap(),
        expected.as_bytes()
    );
}

#[test]
fn rfc_property_order_uses_utf16_and_preserves_array_order() {
    let value = json!({"€":1,"\r":1,"דּ":1,"1":1,"😀":1,"\u{0080}":1,"ö":1});
    let expected = "{\"\\r\":1,\"1\":1,\"\u{0080}\":1,\"ö\":1,\"€\":1,\"😀\":1,\"דּ\":1}";
    assert_eq!(jcs_bytes(&value).unwrap(), expected.as_bytes());
    assert_eq!(
        jcs_bytes(&json!([{"z":2,"a":1},3,1])).unwrap(),
        br#"[{"a":1,"z":2},3,1]"#
    );
}

const NUMBER_CASES: [(u64, &str); 24] = [
    (0x0000000000000000, "0"),
    (0x8000000000000000, "0"),
    (0x0000000000000001, "5e-324"),
    (0x8000000000000001, "-5e-324"),
    (0x7fefffffffffffff, "1.7976931348623157e+308"),
    (0xffefffffffffffff, "-1.7976931348623157e+308"),
    (0x4340000000000000, "9007199254740992"),
    (0xc340000000000000, "-9007199254740992"),
    (0x4430000000000000, "295147905179352830000"),
    (0x44b52d02c7e14af5, "9.999999999999997e+22"),
    (0x44b52d02c7e14af6, "1e+23"),
    (0x44b52d02c7e14af7, "1.0000000000000001e+23"),
    (0x444b1ae4d6e2ef4e, "999999999999999700000"),
    (0x444b1ae4d6e2ef4f, "999999999999999900000"),
    (0x444b1ae4d6e2ef50, "1e+21"),
    (0x3eb0c6f7a0b5ed8c, "9.999999999999997e-7"),
    (0x3eb0c6f7a0b5ed8d, "0.000001"),
    (0x41b3de4355555553, "333333333.3333332"),
    (0x41b3de4355555554, "333333333.33333325"),
    (0x41b3de4355555555, "333333333.3333333"),
    (0x41b3de4355555556, "333333333.3333334"),
    (0x41b3de4355555557, "333333333.33333343"),
    (0xbecbf647612f3696, "-0.0000033333333333333333"),
    (0x43143ff3c1cb0959, "1424953923781206.2"),
];

#[test]
fn rfc_appendix_b_binary64_number_vectors_report_every_mismatch() {
    let mut failures = vec![];
    for (bits, expected) in NUMBER_CASES {
        match jcs_bytes(&f64::from_bits(bits)) {
            Ok(actual) if actual == expected.as_bytes() => {}
            actual => failures.push(format!(
                "{bits:016x}: expected {expected}, observed {actual:?}"
            )),
        }
        match jcs_from_slice(expected.as_bytes(), 128) {
            Ok(actual) if actual == expected.as_bytes() => {}
            actual => failures.push(format!("raw {expected}: observed {actual:?}")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn serde_private_token_object_keys_remain_json_objects() {
    for input in [
        br#"{"$serde_json::private::Number":"1.0"}"#.as_slice(),
        br#"{"$serde_json::private::RawValue":"0"}"#.as_slice(),
    ] {
        assert_eq!(jcs_from_slice(input, 128).unwrap(), input);
    }
}

#[test]
fn raw_parser_feature_behavior_is_explicit() {
    let probe = br#"[{"$serde_json::private::Number":"0"},{"$serde_json::private::RawValue":"0"}]"#;
    let preserved = serde_json::from_slice::<serde_json::Value>(probe)
        .ok()
        .and_then(|value| {
            value
                .as_array()
                .map(|values| values.iter().all(serde_json::Value::is_object))
        })
        .unwrap_or(false);
    if preserved {
        assert_eq!(jcs_from_slice(b"1.0", 3).unwrap(), b"1");
    } else {
        for input in [b"1.0".as_slice(), b"{}", probe] {
            assert!(jcs_from_slice(input, 1024)
                .unwrap_err()
                .to_string()
                .contains("incompatible serde_json features"));
        }
    }
    assert_eq!(
        jcs_bytes(&serde_json::from_str::<serde_json::Value>("1.0").unwrap()).unwrap(),
        b"1"
    );
    assert_eq!(
        jcs_bytes(&serde_json::from_str::<serde_json::Value>("1e30").unwrap()).unwrap(),
        b"1e+30"
    );
    let mut failures = vec![];
    for (bits, expected) in NUMBER_CASES {
        let value: serde_json::Value = serde_json::from_str(expected).unwrap();
        for result in [jcs_bytes(&f64::from_bits(bits)), jcs_bytes(&value)] {
            match result {
                Ok(actual) if actual == expected.as_bytes() => {}
                actual => failures.push(format!(
                    "{bits:016x}: expected {expected}, observed {actual:?}"
                )),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn strings_escape_json_controls_without_unicode_normalization() {
    let value = "\0\u{0001}\u{0008}\u{000c}\u{001f}\"\\\n\r\t/";
    assert_eq!(
        jcs_bytes(&value).unwrap(),
        br#""\u0000\u0001\b\f\u001f\"\\\n\r\t/""#
    );
    assert_eq!(
        jcs_bytes("é e\u{0301} 😀").unwrap(),
        "\"é e\u{0301} 😀\"".as_bytes()
    );
    assert_eq!(
        jcs_from_slice(br#""\ud83d\ude00""#, 64).unwrap(),
        "\"😀\"".as_bytes()
    );
}

#[test]
fn duplicate_object_keys_reject_before_raw_values_or_typed_maps_can_lose_them() {
    let inputs: &[&[u8]] = &[
        br#"{"a":1,"a":2}"#,
        br#"{"a":1,"\u0061":2}"#,
        br#"{"outer":[{"x":1,"x":2}]}"#,
    ];
    let accepted: Vec<_> = inputs
        .iter()
        .filter(|input| jcs_from_slice(input, 1024).is_ok())
        .collect();
    assert!(
        accepted.is_empty(),
        "accepted duplicate-key inputs: {accepted:?}"
    );
    struct DuplicateMap;
    impl Serialize for DuplicateMap {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut map = serializer.serialize_map(Some(2))?;
            map.serialize_entry("a", &1)?;
            map.serialize_entry("a", &2)?;
            map.end()
        }
    }
    assert!(jcs_bytes(&DuplicateMap).is_err());
}

#[test]
fn malformed_nonfinite_out_of_range_and_trailing_json_reject_with_bounded_admission() {
    let invalid: &[&[u8]] = &[
        br#""\ud800""#,
        br#""\udfff""#,
        b"\"\xff\"",
        b"\"\0\"",
        b"NaN",
        b"Infinity",
        b"1e400",
        b"{} []",
    ];
    let accepted: Vec<_> = invalid
        .iter()
        .filter(|input| jcs_from_slice(input, 1024).is_ok())
        .collect();
    assert!(accepted.is_empty(), "accepted invalid inputs: {accepted:?}");
    let accepted: Vec<_> = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY]
        .into_iter()
        .filter(|value| jcs_bytes(value).is_ok())
        .collect();
    assert!(
        accepted.is_empty(),
        "accepted nonfinite floats: {accepted:?}"
    );
    assert!(jcs_from_slice(b"null", 0).is_err());
    assert!(jcs_from_slice(b"null", 3).is_err());
    assert_eq!(jcs_from_slice(b"null", 4).unwrap(), b"null");
    let deep = format!("{}0{}", "[".repeat(129), "]".repeat(129));
    assert!(jcs_from_slice(deep.as_bytes(), deep.len()).is_err());
    assert_eq!(
        jcs_bytes(&u64::MAX.to_string()).unwrap(),
        br#""18446744073709551615""#
    );
}
