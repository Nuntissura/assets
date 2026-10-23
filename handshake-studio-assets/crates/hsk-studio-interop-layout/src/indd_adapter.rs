//! InDesign binary (`.indd`) detection by header and the explicit native-editability blocker
//! (STU-IO-186): an IDML package, a preview or a PDF never satisfies INDD import, and a missing
//! decoder is a blocking scope gap, never a silent exclusion. Nothing past the 29-byte header is
//! read or reverse engineered here; the original bytes are preserved verbatim.
//!
//! Header layout (public format registry PRONOM fmt/548, fmt/1191, fmt/1192): the 16-byte GUID
//! `0606EDF5-D81D-46E5-BD31-EFE7FE74B71D`, an 8-byte type string (`DOCUMENT`, `BOOKBOOK`,
//! `LIBRARY4`), one endianness byte (1 little, 2 big) and a 4-byte version in that byte order.
//! UNVERIFIED on an InDesign-saved file on this machine (no .indd fixture exists here).
use crate::error::{IdmlError, PreservedBlob, Result};
use crate::model::Limits;
use hsk_studio_accord::CancellationToken;
use std::convert::Infallible;

const GUID: [u8; 16] = [
    0x06, 0x06, 0xED, 0xF5, 0xD8, 0x1D, 0x46, 0xE5, 0xBD, 0x31, 0xEF, 0xE7, 0xFE, 0x74, 0xB7, 0x1D,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndKind {
    /// Not an InDesign database file (including IDML packages, which are ZIP).
    NotIndd,
    /// Document or template header. `little_endian` / `version` are `None` when the header is
    /// shorter than 29 bytes or the endianness byte is neither 1 nor 2.
    Document {
        little_endian: Option<bool>,
        version: Option<u32>,
    },
    Book,
    Library,
    /// The InDesign GUID without a known type string.
    Unknown,
}

/// Header sniff only; nothing is decoded.
pub fn identify_indd(bytes: &[u8]) -> IndKind {
    if bytes.len() < 24 || bytes[..16] != GUID {
        return IndKind::NotIndd;
    }
    match &bytes[16..24] {
        b"DOCUMENT" => {
            let little_endian = match bytes.get(24) {
                Some(1) => Some(true),
                Some(2) => Some(false),
                _ => None,
            };
            let version = bytes.get(25..29).and_then(|v| {
                let v = [v[0], v[1], v[2], v[3]];
                little_endian.map(|le| {
                    if le {
                        u32::from_le_bytes(v)
                    } else {
                        u32::from_be_bytes(v)
                    }
                })
            });
            IndKind::Document {
                little_endian,
                version,
            }
        }
        b"BOOKBOOK" => IndKind::Book,
        b"LIBRARY4" => IndKind::Library,
        _ => IndKind::Unknown,
    }
}

/// Always refuses with [`IdmlError::NativeEditabilityBlocked`] for an InDesign binary file (the
/// original bytes travel in the error as a quarantined preservation blob) and with
/// [`IdmlError::Unsupported`] for anything else. There is no INDD decoder in this crate: the `Ok`
/// type is uninhabited, so "INDD readable" cannot be returned.
pub fn import_indd(
    bytes: &[u8],
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<Infallible> {
    cancel.check().map_err(|_| IdmlError::Canceled)?;
    if bytes.len() > limits.max_bytes {
        return Err(IdmlError::BudgetExceeded("bytes"));
    }
    let kind = match identify_indd(bytes) {
        IndKind::NotIndd => return Err(IdmlError::Unsupported("not_an_indd_file")),
        IndKind::Document { .. } => "indd_document",
        IndKind::Book => "indd_book",
        IndKind::Library => "indd_library",
        IndKind::Unknown => "indd_unknown",
    };
    Err(IdmlError::NativeEditabilityBlocked {
        reason: "native_indd_decoder_missing_idml_does_not_satisfy_indd",
        preserved: PreservedBlob {
            kind,
            bytes: bytes.to_vec(),
        },
    })
}
