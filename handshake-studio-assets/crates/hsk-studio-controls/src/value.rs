//! Typed control values and the single error vocabulary of the control mechanic.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A typed control value. Every route (pointer, typed text, keyboard, model, undo) produces
/// and consumes this one type; the descriptor is the only owner of legality.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Value {
    Number(f64),
    Bool(bool),
    /// Id of one `ChoiceOption` of an enum/select control.
    Choice(String),
    /// sRGB, straight (unmultiplied) alpha.
    Color([u8; 4]),
    Text(String),
}

impl Value {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Value::Number(_) => "number",
            Value::Bool(_) => "bool",
            Value::Choice(_) => "choice",
            Value::Color(_) => "color",
            Value::Text(_) => "text",
        }
    }
}

/// Why a descriptor, value or registry operation was refused. Hard-bound violations name the
/// control, the offending value and both bounds (STU-FX-104); nothing is ever silently clamped.
#[derive(Clone, Debug, PartialEq)]
pub enum ControlError {
    InvalidDescriptor { control_id: String, reason: String },
    DuplicateControl { control_id: String },
    UnknownControl { control_id: String },
    TypeMismatch { control_id: String, expected: &'static str, got: &'static str },
    InvalidText { control_id: String, reason: &'static str },
    NonFinite { control_id: String },
    OutOfHardBounds { control_id: String, value: f64, hard_min: Option<f64>, hard_max: Option<f64> },
    UnknownChoice { control_id: String, choice: String },
    TextTooLong { control_id: String, max_chars: usize, got_chars: usize },
    Stale { control_id: String, expected: Value, current: Value },
}

impl ControlError {
    /// Stable snake_case code (values are not part of the code, so telemetry can stay redacted).
    pub fn code(&self) -> &'static str {
        match self {
            ControlError::InvalidDescriptor { .. } => "invalid_descriptor",
            ControlError::DuplicateControl { .. } => "duplicate_control",
            ControlError::UnknownControl { .. } => "unknown_control",
            ControlError::TypeMismatch { .. } => "type_mismatch",
            ControlError::InvalidText { .. } => "invalid_text",
            ControlError::NonFinite { .. } => "non_finite",
            ControlError::OutOfHardBounds { .. } => "out_of_hard_bounds",
            ControlError::UnknownChoice { .. } => "unknown_choice",
            ControlError::TextTooLong { .. } => "text_too_long",
            ControlError::Stale { .. } => "stale",
        }
    }
}

fn bound(b: &Option<f64>) -> String {
    b.map_or_else(|| "unbounded".to_string(), |v| v.to_string())
}

impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ControlError::InvalidDescriptor { control_id, reason } => {
                write!(f, "invalid descriptor `{control_id}`: {reason}")
            }
            ControlError::DuplicateControl { control_id } => write!(f, "duplicate control `{control_id}`"),
            ControlError::UnknownControl { control_id } => write!(f, "unknown control `{control_id}`"),
            ControlError::TypeMismatch { control_id, expected, got } => {
                write!(f, "control `{control_id}` expects {expected}, got {got}")
            }
            ControlError::InvalidText { control_id, reason } => {
                write!(f, "control `{control_id}`: invalid text ({reason})")
            }
            ControlError::NonFinite { control_id } => write!(f, "control `{control_id}`: value is not finite"),
            ControlError::OutOfHardBounds { control_id, value, hard_min, hard_max } => write!(
                f,
                "control `{control_id}`: {value} is outside the hard range [{}, {}]",
                bound(hard_min),
                bound(hard_max)
            ),
            ControlError::UnknownChoice { control_id, choice } => {
                write!(f, "control `{control_id}`: unknown choice `{choice}`")
            }
            ControlError::TextTooLong { control_id, max_chars, got_chars } => {
                write!(f, "control `{control_id}`: text has {got_chars} chars, limit {max_chars}")
            }
            ControlError::Stale { control_id, .. } => write!(f, "control `{control_id}`: stale change"),
        }
    }
}

impl std::error::Error for ControlError {}
