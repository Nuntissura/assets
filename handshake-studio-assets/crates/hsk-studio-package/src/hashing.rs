//! Cancellable CRC-32 (ZIP mandated) and SHA-256 (content identity) over bounded chunks.
use crate::error::PackageError;
use hsk_studio_accord::CancellationToken;
use sha2::{Digest, Sha256};

const CHUNK: usize = 1 << 20;
const HEX: &[u8; 16] = b"0123456789abcdef";

pub(crate) fn check(cancel: &CancellationToken) -> Result<(), PackageError> {
    cancel.check().map_err(|_| PackageError::Canceled)
}

pub(crate) fn crc32(data: &[u8], cancel: &CancellationToken) -> Result<u32, PackageError> {
    let mut hasher = crc32fast::Hasher::new();
    for chunk in data.chunks(CHUNK) {
        check(cancel)?;
        hasher.update(chunk);
    }
    Ok(hasher.finalize())
}

pub(crate) fn sha256_hex(data: &[u8], cancel: &CancellationToken) -> Result<String, PackageError> {
    let mut hasher = Sha256::new();
    for chunk in data.chunks(CHUNK) {
        check(cancel)?;
        hasher.update(chunk);
    }
    Ok(hex(hasher.finalize().as_slice()))
}

/// Lowercase hex SHA-256 of `data` (the package content identity).
pub fn sha256_hex_of(data: &[u8]) -> String {
    hex(Sha256::digest(data).as_slice())
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(HEX[usize::from(b >> 4)]));
        out.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
    out
}

pub(crate) fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}
