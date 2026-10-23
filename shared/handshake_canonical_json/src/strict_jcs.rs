use serde::{
    de::{self, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use serde_json::Value;
use std::{collections::BTreeSet, fmt, sync::OnceLock};

/// Serialize trusted, already bounded typed data with RFC8785 binary64 numbers and UTF16 key ordering.
/// The caller bounds the value and its Serialize implementation, including output allocation.
pub fn jcs_bytes<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<Vec<u8>> {
    serde_json_canonicalizer::to_vec(&value)
}

/// Canonicalize one bounded JSON value, rejecting duplicate keys before they enter a map.
pub fn jcs_from_slice(input: &[u8], max_bytes: usize) -> serde_json::Result<Vec<u8>> {
    static OBJECT_KEYS_PRESERVED: OnceLock<bool> = OnceLock::new();
    if !OBJECT_KEYS_PRESERVED.get_or_init(|| {
        let probe =
            br#"[{"$serde_json::private::Number":"0"},{"$serde_json::private::RawValue":"0"}]"#;
        serde_json::from_slice::<Value>(probe)
            .ok()
            .and_then(|value| {
                value
                    .as_array()
                    .map(|values| values.iter().all(Value::is_object))
            })
            .unwrap_or(false)
    }) {
        return Err(<serde_json::Error as de::Error>::custom(
            "JCS raw admission requires serde_json to preserve object keys; incompatible serde_json features",
        ));
    }
    if max_bytes == 0 || input.len() > max_bytes {
        return Err(<serde_json::Error as de::Error>::custom(format!(
            "JCS input exceeds positive byte bound {max_bytes}"
        )));
    }
    let mut decoder = serde_json::Deserializer::from_slice(input);
    UniqueValue::deserialize(&mut decoder)?;
    decoder.end()?;
    let value: Value = serde_json::from_slice(input)?;
    jcs_bytes(&value)
}

struct UniqueValue;
impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        decoder.deserialize_any(UniqueVisitor)
    }
}
struct UniqueVisitor;
impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = UniqueValue;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a JSON value with unique object keys and finite numbers")
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue)
    }
    fn visit_bool<E: de::Error>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(UniqueValue)
    }
    fn visit_i64<E: de::Error>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(UniqueValue)
    }
    fn visit_u64<E: de::Error>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(UniqueValue)
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        if !value.is_finite() {
            return Err(E::custom("nonfinite JSON number"));
        }
        Ok(UniqueValue)
    }
    fn visit_str<E: de::Error>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(UniqueValue)
    }
    fn visit_string<E: de::Error>(self, _value: String) -> Result<Self::Value, E> {
        Ok(UniqueValue)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut input: A) -> Result<Self::Value, A::Error> {
        while input.next_element::<UniqueValue>()?.is_some() {}
        Ok(UniqueValue)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut input: A) -> Result<Self::Value, A::Error> {
        let mut keys = BTreeSet::new();
        while let Some(key) = input.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(de::Error::custom("duplicate JSON object key"));
            }
            input.next_value::<UniqueValue>()?;
        }
        Ok(UniqueValue)
    }
}
