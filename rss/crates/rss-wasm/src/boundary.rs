use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

pub const MAX_INPUT: usize = 1024 * 1024;
pub const MAX_OUTPUT: usize = 16 * MAX_INPUT;
pub const MAX_QUERY: usize = 64 * 1024;
pub const MAX_RETAINED: usize = 2 * MAX_INPUT;
pub const MAX_HANDLES: usize = 32;

pub fn decode<T: DeserializeOwned>(input: &str) -> Result<T, String> {
    handshake_canonical_json::jcs_from_slice(input.as_bytes(), MAX_INPUT)
        .map_err(|_| "invalid_json".to_owned())?;
    let value: Value = serde_json::from_str(input).map_err(|_| "invalid_input".to_owned())?;
    fn safe(value: &Value) -> bool {
        match value {
            Value::Number(number) => {
                number
                    .as_i64()
                    .is_none_or(|n| n.unsigned_abs() <= 9_007_199_254_740_991)
                    && number.as_u64().is_none_or(|n| n <= 9_007_199_254_740_991)
                    && number
                        .as_f64()
                        .is_none_or(|n| n.fract() != 0.0 || n.abs() <= 9_007_199_254_740_991_f64)
            }
            Value::Array(values) => values.iter().all(safe),
            Value::Object(values) => values.values().all(safe),
            _ => true,
        }
    }
    if !safe(&value) {
        return Err("numeric_range".into());
    }
    serde_json::from_value(value).map_err(|_| "invalid_input".into())
}

pub fn bound(value: &impl Serialize, limit: usize) -> Result<(), String> {
    struct Count {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes = self
                .bytes
                .checked_add(bytes.len())
                .filter(|n| *n <= self.limit)
                .ok_or_else(|| std::io::Error::other("encoded bounds"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Count { bytes: 0, limit }, value).map_err(|_| "bounds".into())
}

pub fn encode(value: &impl Serialize) -> Result<String, String> {
    bound(value, MAX_OUTPUT)?;
    serde_json::to_string(value).map_err(|_| "invalid_output".into())
}

pub fn version(version: u32) -> Result<(), String> {
    if version == 1 {
        Ok(())
    } else {
        Err("unsupported_version".into())
    }
}

pub fn bounds(bounds: &rss_contracts::Bounds) -> Result<(), String> {
    bounds.validate().map_err(|_| "bounds")?;
    if bounds.max_operation_bytes > MAX_INPUT
        || bounds.max_collection_items > 1024
        || bounds.max_page_items > 100
        || bounds.max_url_bytes > MAX_QUERY
        || bounds.max_text_bytes > MAX_QUERY
    {
        return Err("bounds".into());
    }
    Ok(())
}
