//! Stable identifiers, schema versions, compatibility ranges, and bounded values.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

macro_rules! stable_id {
    ($name:ident, $prefix:literal) => {
        #[doc = concat!("Stable `", $prefix, "` identifier.")]
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates an ID after enforcing the stable ASCII grammar and length bound.
            ///
            /// # Errors
            ///
            /// Returns [`IdError`] when the prefix, length, or character grammar is invalid.
            pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
                let value = value.into();
                validate_id(&value, $prefix)?;
                Ok(Self(value))
            }

            /// Returns the canonical wire representation.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

stable_id!(ItemId, "item_");
stable_id!(RepresentationId, "repr_");
stable_id!(TrackId, "track_");
stable_id!(AssetId, "asset_");
stable_id!(DerivedOutputId, "output_");
stable_id!(NodeId, "node_");
stable_id!(EdgeId, "edge_");
stable_id!(ContinuationId, "continuation_");
stable_id!(JobId, "job_");
stable_id!(RequestId, "request_");
stable_id!(ProducerId, "producer_");
stable_id!(TransactionId, "transaction_");
stable_id!(CapabilityId, "capability_");

const MAX_ID_LEN: usize = 128;

/// Stable identifier validation failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdError {
    /// The identifier is empty or lacks the required prefix.
    InvalidPrefix { required: &'static str },
    /// The identifier exceeds the fixed wire limit.
    TooLong { actual: usize, maximum: usize },
    /// A character falls outside lowercase ASCII, digits, `_`, `-`, and `.`.
    InvalidCharacter { index: usize },
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPrefix { required } => write!(f, "identifier must start with {required}"),
            Self::TooLong { actual, maximum } => {
                write!(f, "identifier length {actual} exceeds {maximum}")
            }
            Self::InvalidCharacter { index } => {
                write!(f, "identifier has invalid character at byte {index}")
            }
        }
    }
}

impl std::error::Error for IdError {}

fn validate_id(value: &str, prefix: &'static str) -> Result<(), IdError> {
    if !value.starts_with(prefix) || value.len() == prefix.len() {
        return Err(IdError::InvalidPrefix { required: prefix });
    }
    if value.len() > MAX_ID_LEN {
        return Err(IdError::TooLong {
            actual: value.len(),
            maximum: MAX_ID_LEN,
        });
    }
    if let Some((index, _)) = value.bytes().enumerate().find(|(_, byte)| {
        !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && !matches!(byte, b'_' | b'-' | b'.')
    }) {
        return Err(IdError::InvalidCharacter { index });
    }
    Ok(())
}

/// Semantic wire version. Major changes are incompatible; minor changes are additive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaVersion {
    pub major: u16,
    pub minor: u16,
}

/// Inclusive compatibility range supported by a reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityRange {
    pub major: u16,
    pub minimum_minor: u16,
    pub maximum_minor: u16,
}

impl CompatibilityRange {
    /// Checks a schema version without silently accepting incompatible majors.
    ///
    /// # Errors
    ///
    /// Returns [`CompatibilityError`] when the version is outside this range.
    pub fn check(self, version: SchemaVersion) -> Result<(), CompatibilityError> {
        if version.major != self.major {
            return Err(CompatibilityError::IncompatibleMajor {
                received: version.major,
                supported: self.major,
            });
        }
        if version.minor < self.minimum_minor || version.minor > self.maximum_minor {
            return Err(CompatibilityError::UnsupportedMinor {
                received: version.minor,
                minimum: self.minimum_minor,
                maximum: self.maximum_minor,
            });
        }
        Ok(())
    }
}

/// Typed schema-negotiation failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompatibilityError {
    IncompatibleMajor {
        received: u16,
        supported: u16,
    },
    UnsupportedMinor {
        received: u16,
        minimum: u16,
        maximum: u16,
    },
}

impl fmt::Display for CompatibilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncompatibleMajor {
                received,
                supported,
            } => write!(f, "major {received} is incompatible with {supported}"),
            Self::UnsupportedMinor {
                received,
                minimum,
                maximum,
            } => write!(f, "minor {received} is outside {minimum}..={maximum}"),
        }
    }
}

impl std::error::Error for CompatibilityError {}

/// Explicit three-state metadata value; missing is never conflated with null.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum TriState<T> {
    Unknown,
    NotApplicable,
    Present(T),
}

/// Bounded namespaced JSON extension map.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ExtensionMap(BTreeMap<String, serde_json::Value>);

impl<'de> Deserialize<'de> for ExtensionMap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let values = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
        Self::new(values, ExtensionLimits::default()).map_err(serde::de::Error::custom)
    }
}

/// Extension validation limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtensionLimits {
    pub maximum_entries: usize,
    pub maximum_key_bytes: usize,
    pub maximum_value_bytes: usize,
    pub maximum_total_bytes: usize,
}

impl Default for ExtensionLimits {
    fn default() -> Self {
        Self {
            maximum_entries: 64,
            maximum_key_bytes: 128,
            maximum_value_bytes: 16 * 1024,
            maximum_total_bytes: 64 * 1024,
        }
    }
}

/// Bounded extension-map failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtensionError {
    TooManyEntries {
        actual: usize,
        maximum: usize,
    },
    InvalidNamespace {
        key: String,
    },
    KeyTooLong {
        key: String,
        maximum: usize,
    },
    ValueTooLarge {
        key: String,
        actual: usize,
        maximum: usize,
    },
    TotalTooLarge {
        actual: usize,
        maximum: usize,
    },
    Serialization {
        key: String,
    },
}

impl fmt::Display for ExtensionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid extension map: {self:?}")
    }
}
impl std::error::Error for ExtensionError {}

impl ExtensionMap {
    /// Builds and validates an extension map.
    ///
    /// # Errors
    ///
    /// Returns [`ExtensionError`] when namespace grammar or a byte/count bound is violated.
    pub fn new(
        values: BTreeMap<String, serde_json::Value>,
        limits: ExtensionLimits,
    ) -> Result<Self, ExtensionError> {
        let result = Self(values);
        result.validate(limits)?;
        Ok(result)
    }

    /// Returns the deterministic key-ordered values.
    #[must_use]
    pub fn values(&self) -> &BTreeMap<String, serde_json::Value> {
        &self.0
    }

    /// Enforces namespace grammar and serialized byte budgets.
    ///
    /// # Errors
    ///
    /// Returns [`ExtensionError`] for the first invalid key or exceeded bound.
    pub fn validate(&self, limits: ExtensionLimits) -> Result<(), ExtensionError> {
        if self.0.len() > limits.maximum_entries {
            return Err(ExtensionError::TooManyEntries {
                actual: self.0.len(),
                maximum: limits.maximum_entries,
            });
        }
        let encoded_map =
            serde_json::to_vec(&self.0).map_err(|_| ExtensionError::Serialization {
                key: "<extension-map>".to_owned(),
            })?;
        if encoded_map.len() > limits.maximum_total_bytes {
            return Err(ExtensionError::TotalTooLarge {
                actual: encoded_map.len(),
                maximum: limits.maximum_total_bytes,
            });
        }
        for (key, value) in &self.0 {
            if key.len() > limits.maximum_key_bytes {
                return Err(ExtensionError::KeyTooLong {
                    key: key.clone(),
                    maximum: limits.maximum_key_bytes,
                });
            }
            if !valid_namespace(key) {
                return Err(ExtensionError::InvalidNamespace { key: key.clone() });
            }
            let encoded = serde_json::to_vec(value)
                .map_err(|_| ExtensionError::Serialization { key: key.clone() })?;
            if encoded.len() > limits.maximum_value_bytes {
                return Err(ExtensionError::ValueTooLarge {
                    key: key.clone(),
                    actual: encoded.len(),
                    maximum: limits.maximum_value_bytes,
                });
            }
        }
        Ok(())
    }
}

fn valid_namespace(key: &str) -> bool {
    let mut segments = key.split('.');
    let Some(namespace) = segments.next() else {
        return false;
    };
    let Some(name) = segments.next() else {
        return false;
    };
    !namespace.is_empty()
        && !name.is_empty()
        && segments.all(|segment| !segment.is_empty())
        && key.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_ids_reject_wrong_prefix_and_uppercase() {
        assert!(matches!(
            ItemId::new("node_a"),
            Err(IdError::InvalidPrefix { .. })
        ));
        assert!(matches!(
            ItemId::new("item_A"),
            Err(IdError::InvalidCharacter { .. })
        ));
        assert!(ItemId::new("item_42.good").is_ok());
    }

    #[test]
    fn compatibility_is_bounded() {
        let range = CompatibilityRange {
            major: 1,
            minimum_minor: 0,
            maximum_minor: 2,
        };
        assert!(range.check(SchemaVersion { major: 1, minor: 2 }).is_ok());
        assert!(matches!(
            range.check(SchemaVersion { major: 2, minor: 0 }),
            Err(CompatibilityError::IncompatibleMajor { .. })
        ));
    }

    #[test]
    fn extensions_require_namespace_and_budget() {
        let mut values = BTreeMap::new();
        values.insert("plain".to_owned(), serde_json::Value::Null);
        assert!(matches!(
            ExtensionMap::new(values, ExtensionLimits::default()),
            Err(ExtensionError::InvalidNamespace { .. })
        ));
    }

    #[test]
    fn extension_deserialization_cannot_bypass_validation() {
        let invalid = serde_json::from_str::<ExtensionMap>(r#"{"plain":null}"#);
        assert!(invalid.is_err());
        let empty_segment = serde_json::from_str::<ExtensionMap>(r#"{"plugin..value":null}"#);
        assert!(empty_segment.is_err());
        let trailing_segment = serde_json::from_str::<ExtensionMap>(r#"{"plugin.value.":null}"#);
        assert!(trailing_segment.is_err());
        let valid = serde_json::from_str::<ExtensionMap>(r#"{"plugin.value":null}"#);
        assert!(valid.is_ok());
        let nested = serde_json::from_str::<ExtensionMap>(r#"{"plugin.group.value":null}"#);
        assert!(nested.is_ok());
    }

    #[test]
    fn tri_state_rejects_unknown_wire_fields() {
        assert!(
            serde_json::from_str::<TriState<String>>(r#"{"state":"unknown","smuggled":true}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<TriState<String>>(
                r#"{"state":"present","value":"visible","smuggled":true}"#
            )
            .is_err()
        );
    }

    #[test]
    fn extension_total_budget_counts_exact_json_encoding() {
        let mut values = BTreeMap::new();
        values.insert("plugin.value".to_owned(), serde_json::Value::Null);
        let encoded_len = serde_json::to_vec(&values).unwrap().len();
        let old_partial_count =
            "plugin.value".len() + serde_json::to_vec(&serde_json::Value::Null).unwrap().len();
        assert!(old_partial_count < encoded_len);
        assert!(matches!(
            ExtensionMap::new(
                values.clone(),
                ExtensionLimits {
                    maximum_total_bytes: old_partial_count,
                    ..ExtensionLimits::default()
                }
            ),
            Err(ExtensionError::TotalTooLarge { actual, maximum })
                if actual == encoded_len && maximum == old_partial_count
        ));
        assert!(
            ExtensionMap::new(
                values,
                ExtensionLimits {
                    maximum_total_bytes: encoded_len,
                    ..ExtensionLimits::default()
                }
            )
            .is_ok()
        );
    }
}
