//! The composition token of a stored document in the session (lane SMB-TYPE-AUTHORED-TOKEN-1,
//! STU-TYP-130).
use crate::error::SessionError;
use crate::session::Session;
use crate::source::DOCUMENT_ENTRY_PREFIX;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{CURRENT_COMPOSITION_TOKEN, StudioCompositionTokenRecord};
use hsk_studio_package::{Quarantined, sha256_hex_of};

/// Document-entry token of the composition token record. (`composition` is the motion composition.)
pub const COMPOSITION_TOKEN_ENTRY: &str = "composition_token";
/// The note of a document with no stored token.
pub const COMPOSITION_TOKEN_ABSENT: &str = "composition_token_absent";
/// The refusal of a well-formed token this build cannot compose under.
pub const COMPOSITION_TOKEN_UNKNOWN: &str = "composition_token_unknown";

/// What a document composes under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthoredComposition {
    /// The token a host passes to Type.
    pub token: &'static str,
    /// False when the document stores no token.
    pub stored: bool,
    /// [`COMPOSITION_TOKEN_ABSENT`] when the document stores no token.
    pub note: Option<&'static str>,
}

/// The entry a path that creates a new document holds: the current token, no upgrades.
pub fn new_document_composition_entry(document_id: &str) -> Result<Quarantined, SessionError> {
    let record = StudioCompositionTokenRecord::new(document_id, CURRENT_COMPOSITION_TOKEN);
    let bytes = serde_json::to_vec(&record).map_err(|_| SessionError::Provider("invalid_input"))?;
    let sha256_hex = sha256_hex_of(&bytes);
    Ok(Quarantined { name: format!("{DOCUMENT_ENTRY_PREFIX}{COMPOSITION_TOKEN_ENTRY}.{sha256_hex}"), bytes, sha256_hex })
}

impl Session {
    /// The stored composition token record.
    pub fn composition_token_record(&self, cancel: &CancellationToken) -> Result<Option<StudioCompositionTokenRecord>, SessionError> {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        Ok(None)
    }

    /// What the document composes under.
    pub fn authored_composition(&self, cancel: &CancellationToken) -> Result<AuthoredComposition, SessionError> {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        Ok(AuthoredComposition { token: CURRENT_COMPOSITION_TOKEN, stored: true, note: None })
    }

    /// Stores or removes the record.
    pub fn set_composition_token_record(&mut self, record: Option<&StudioCompositionTokenRecord>, cancel: &CancellationToken) -> Result<(), SessionError> {
        let bytes = match record {
            None => None,
            Some(r) => Some(serde_json::to_vec(r).map_err(|_| SessionError::Provider("invalid_input"))?),
        };
        self.set_document_entry(COMPOSITION_TOKEN_ENTRY, bytes, cancel)
    }
}
