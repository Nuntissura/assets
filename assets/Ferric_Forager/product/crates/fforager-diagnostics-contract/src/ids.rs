use crate::{ContractError, MAX_ID_BYTES, MAX_TEXT_BYTES};
use serde::{Deserialize, Serialize};

fn validate_identifier(value: &str, field: &'static str) -> Result<(), ContractError> {
    if value.is_empty() {
        return Err(ContractError::Empty { field });
    }
    if value.len() > MAX_ID_BYTES {
        return Err(ContractError::LimitExceeded {
            kind: crate::LimitKind::Identifier,
            limit: MAX_ID_BYTES,
            actual: value.len(),
        });
    }
    if !value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
    }) {
        return Err(ContractError::InvalidIdentifier { field });
    }
    Ok(())
}

macro_rules! identifier {
    ($name:ident, $field:literal) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Creates a bounded, portable wire identifier.
            ///
            /// # Errors
            /// Returns [`ContractError`] when empty, oversized, or outside the identifier alphabet.
            pub fn new(value: impl Into<String>) -> Result<Self, ContractError> {
                let value = value.into();
                validate_identifier(&value, $field)?;
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = ContractError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

identifier!(ProducerInstanceId, "producer_instance");
identifier!(BootSessionId, "boot_session");
identifier!(ChannelId, "channel");
identifier!(CapabilityId, "capability_id");
identifier!(ArtifactId, "artifact_id");
identifier!(BuildId, "build_id");
identifier!(ProcessStartId, "process_start_id");
identifier!(EventId, "event_id");
identifier!(ReasonCode, "reason_code");
identifier!(RequestId, "request_id");
identifier!(WorkId, "work_id");

/// UTF-8 text validated against the contract-wide text bound.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BoundedText(String);

impl BoundedText {
    /// Creates non-empty text within [`crate::MAX_TEXT_BYTES`].
    ///
    /// # Errors
    /// Returns [`ContractError`] for empty or oversized text.
    pub fn new(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ContractError::Empty { field: "text" });
        }
        if value.len() > MAX_TEXT_BYTES {
            return Err(ContractError::LimitExceeded {
                kind: crate::LimitKind::Text,
                limit: MAX_TEXT_BYTES,
                actual: value.len(),
            });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for BoundedText {
    type Error = ContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<BoundedText> for String {
    fn from(value: BoundedText) -> Self {
        value.0
    }
}
