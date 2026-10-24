use serde::{
    de::{self, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use serde_json::{Map, Number, Value};
use std::{fmt, sync::OnceLock};

/// Serialize trusted, already bounded typed data with RFC8785 binary64 numbers and UTF16 key ordering.
/// The caller bounds the value and its Serialize implementation, including output allocation.
pub fn jcs_bytes<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<Vec<u8>> {
    serde_json_canonicalizer::to_vec(&value)
}

/// Canonicalize one bounded JSON value, rejecting duplicate keys before they enter a map.
pub fn jcs_from_slice(input: &[u8], max_bytes: usize) -> serde_json::Result<Vec<u8>> {
    static NUMBER_KEYS_PRESERVED: OnceLock<bool> = OnceLock::new();
    if !NUMBER_KEYS_PRESERVED.get_or_init(|| {
        let probe = br#"{"$serde_json::private::Number":"0"}"#;
        serde_json::from_slice::<Value>(probe)
            .ok()
            .map(|value| value.is_object())
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
    let value = UniqueValue::deserialize(&mut decoder)?;
    decoder.end()?;
    jcs_bytes(&value.0)
}

struct UniqueValue(Value);
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
        Ok(UniqueValue(Value::Null))
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Bool(value)))
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(value.into())))
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(value.into())))
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        if !value.is_finite() {
            return Err(E::custom("nonfinite JSON number"));
        }
        Number::from_f64(value)
            .map(|number| UniqueValue(Value::Number(number)))
            .ok_or_else(|| E::custom("nonfinite JSON number"))
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(value.into())))
    }
    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(value)))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut input: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = input.next_element::<UniqueValue>()? {
            values.push(value.0);
        }
        Ok(UniqueValue(Value::Array(values)))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut input: A) -> Result<Self::Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = input.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom("duplicate JSON object key"));
            }
            values.insert(key, input.next_value::<UniqueValue>()?.0);
        }
        Ok(UniqueValue(Value::Object(values)))
    }
}
