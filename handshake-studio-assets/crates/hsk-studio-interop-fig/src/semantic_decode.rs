//! Editable semantic decode: BLOCKED. Parser success, self-render and original-byte preservation are
//! not editable-parity proof (STU-IO-186); the decoder stays blocked until Figma-produced local files
//! with original semantic/render snapshots exist. SVG, PNG, tokens and API output never substitute.
use crate::container::FigContainer;
use crate::{FigError, Result};

/// Editable status of every import. A constant `NotProven` until original-produced proof exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditableStatus {
    NotProven,
}

impl EditableStatus {
    pub const fn label(self) -> &'static str {
        "NOT_PROVEN"
    }
}

/// Never produces a document: always `EditableDecoderBlocked`.
pub fn decode_semantic(_container: &FigContainer) -> Result<std::convert::Infallible> {
    Err(FigError::EditableDecoderBlocked {
        reason: "NOT_PROVEN: needs original-produced semantic/render proof",
    })
}
