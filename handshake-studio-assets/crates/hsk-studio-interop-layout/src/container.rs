//! Bounded ZIP reader and deterministic ZIP writer for IDML packages (STU-IO-187): bounded entry
//! count, entry size, total inflated size and expansion ratio; exact and case-folded duplicate-name
//! rejection; traversal/absolute/backslash/drive names refused; `mimetype` first and stored; local
//! headers cross-checked against the central directory; overlapping entry data refused; CRC-32
//! verified. No ZIP64, encryption or methods other than STORE (0) and DEFLATE (8): those are typed
//! refusals, never partial reads. The reader never touches the file system.
use crate::{IdmlError, Limits, Result};
use hsk_studio_accord::CancellationToken;
use hsk_studio_package::{ZipDirectory, ZipLimits, ZipReadError, crc32_of};
use miniz_oxide::deflate::compress_to_vec;
use std::collections::BTreeSet;

/// Value of the first, stored `mimetype` entry of every IDML package.
pub const IDML_MIMETYPE: &str = "application/vnd.adobe.indesign-idml-package";
const MIMETYPE_NAME: &str = "mimetype";
const SIG_LOCAL: u32 = 0x0403_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_EOCD: u32 = 0x0605_4b50;
const MAX_NAME: usize = 512;

fn zip_err(e: ZipReadError) -> IdmlError {
    match e {
        ZipReadError::Canceled => IdmlError::Canceled,
        ZipReadError::Malformed(code) => IdmlError::Zip(code),
        ZipReadError::BudgetExceeded(what) => IdmlError::BudgetExceeded(what),
        ZipReadError::UnsafeName => IdmlError::UnsafeEntryName,
        ZipReadError::DuplicateName => IdmlError::DuplicateEntry,
    }
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && !name.starts_with('/')
        && !name.contains(['\\', ':'])
        && name.chars().all(|c| !c.is_control())
        && name
            .trim_end_matches('/')
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

/// Reads every file entry as `(name, bytes)` in central-directory order (bounded reader owned by
/// `hsk-studio-package`; ZIP64 stays refused for IDML).
pub(crate) fn read(
    bytes: &[u8],
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<Vec<(String, Vec<u8>)>> {
    let zl = ZipLimits {
        max_bytes: limits.max_bytes,
        max_entries: limits.max_entries,
        max_entry_bytes: limits.max_entry_bytes,
        max_inflated_bytes: limits.max_inflated_bytes,
        max_ratio: limits.max_ratio,
        allow_zip64: false,
    };
    let dir = ZipDirectory::parse(bytes, &zl, cancel).map_err(zip_err)?;
    let entries = dir.entries();
    match entries.iter().position(|e| e.name == MIMETYPE_NAME) {
        None => return Err(IdmlError::Mimetype("missing")),
        Some(0) if entries[0].local_offset() == 0 => {}
        Some(_) => return Err(IdmlError::Mimetype("not_first")),
    }
    if entries[0].method != 0 {
        return Err(IdmlError::Mimetype("compressed"));
    }
    let out = dir.read_all(bytes, cancel).map_err(zip_err)?;
    if out[0].1 != IDML_MIMETYPE.as_bytes() {
        return Err(IdmlError::Mimetype("wrong_value"));
    }
    Ok(out)
}

fn put16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Deterministic ZIP: the caller's order, fixed 1980-01-01 timestamps, no extra fields or comments,
/// `mimetype` stored, other entries DEFLATE level 6 when that is smaller (or all STORE when
/// `compress` is false). The same inputs always yield the same bytes for the pinned miniz_oxide.
pub(crate) fn write(parts: &[(&str, &[u8])], compress: bool) -> Result<Vec<u8>> {
    if parts.len() >= usize::from(u16::MAX) {
        return Err(IdmlError::Zip("zip64_unsupported"));
    }
    let mut seen = BTreeSet::new();
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for (name, data) in parts {
        if !safe_name(name) || name.ends_with('/') {
            return Err(IdmlError::UnsafeEntryName);
        }
        if !seen.insert(name.to_lowercase()) {
            return Err(IdmlError::DuplicateEntry);
        }
        let deflated = (compress && *name != MIMETYPE_NAME)
            .then(|| compress_to_vec(data, 6))
            .filter(|d| d.len() < data.len());
        let (method, body): (u16, &[u8]) = match &deflated {
            Some(d) => (8, d.as_slice()),
            None => (0, *data),
        };
        let too_big = IdmlError::Zip("zip64_unsupported");
        let (comp, uncomp, local) = (
            u32::try_from(body.len()).map_err(|_| too_big.clone())?,
            u32::try_from(data.len()).map_err(|_| too_big.clone())?,
            u32::try_from(out.len()).map_err(|_| too_big)?,
        );
        let crc = crc32_of(data);
        put32(&mut out, SIG_LOCAL);
        for v in [20, 0, method, 0, 0x0021] {
            put16(&mut out, v);
        }
        for v in [crc, comp, uncomp] {
            put32(&mut out, v);
        }
        put16(&mut out, name.len() as u16);
        put16(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(body);

        put32(&mut central, SIG_CENTRAL);
        for v in [20, 20, 0, method, 0, 0x0021] {
            put16(&mut central, v);
        }
        for v in [crc, comp, uncomp] {
            put32(&mut central, v);
        }
        for v in [name.len() as u16, 0, 0, 0, 0] {
            put16(&mut central, v);
        }
        put32(&mut central, 0);
        put32(&mut central, local);
        central.extend_from_slice(name.as_bytes());
    }
    let cd_off = u32::try_from(out.len()).map_err(|_| IdmlError::Zip("zip64_unsupported"))?;
    let cd_size = u32::try_from(central.len()).map_err(|_| IdmlError::Zip("zip64_unsupported"))?;
    out.extend_from_slice(&central);
    put32(&mut out, SIG_EOCD);
    for v in [0, 0, parts.len() as u16, parts.len() as u16] {
        put16(&mut out, v);
    }
    put32(&mut out, cd_size);
    put32(&mut out, cd_off);
    put16(&mut out, 0);
    Ok(out)
}
