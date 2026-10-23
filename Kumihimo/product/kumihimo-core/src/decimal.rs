//! Exact u64 wire values, independent of JSON numeric precision.
use serde::{Deserialize, Deserializer, Serializer};
pub(crate) fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.to_string())
}
pub(crate) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(serde::de::Error::custom(
            "expected canonical decimal u64 string",
        ));
    }
    value.parse().map_err(serde::de::Error::custom)
}
