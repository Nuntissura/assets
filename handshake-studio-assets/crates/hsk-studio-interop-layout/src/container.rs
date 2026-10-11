//! Bounded ZIP reader and deterministic ZIP writer for IDML packages (STU-IO-187): bounded entry
//! count, entry size, total inflated size and expansion ratio; exact and case-folded duplicate-name
//! rejection; traversal/absolute/backslash/drive names refused; `mimetype` first and stored; local
//! headers cross-checked against the central directory; overlapping entry data refused; CRC-32
//! verified. No ZIP64, encryption or methods other than STORE (0) and DEFLATE (8): those are typed
//! refusals, never partial reads. The reader never touches the file system.
use crate::{IdmlError, Limits, Result};
use hsk_studio_accord::CancellationToken;
use miniz_oxide::{deflate::compress_to_vec, inflate::decompress_to_vec_with_limit};
use std::collections::BTreeSet;

/// Value of the first, stored `mimetype` entry of every IDML package.
pub const IDML_MIMETYPE: &str = "application/vnd.adobe.indesign-idml-package";
const MIMETYPE_NAME: &str = "mimetype";
const SIG_LOCAL: u32 = 0x0403_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_EOCD: u32 = 0x0605_4b50;
/// Expansion ratio is only judged for entries above this inflated size.
const RATIO_FLOOR: u64 = 1 << 20;
const MAX_NAME: usize = 512;

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}
static CRC_TABLE: [u32; 256] = crc_table();

pub(crate) fn crc32(data: &[u8]) -> u32 {
    !data.iter().fold(!0u32, |c, b| {
        CRC_TABLE[usize::from((c as u8) ^ b)] ^ (c >> 8)
    })
}

fn take(bytes: &[u8], at: usize, len: usize) -> Result<&[u8]> {
    at.checked_add(len)
        .and_then(|end| bytes.get(at..end))
        .ok_or(IdmlError::Zip("truncated"))
}
fn le16(bytes: &[u8], at: usize) -> Result<u16> {
    let s = take(bytes, at, 2)?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}
fn le32(bytes: &[u8], at: usize) -> Result<u32> {
    let s = take(bytes, at, 4)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
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

struct Central {
    name: String,
    method: u16,
    crc: u32,
    comp: u64,
    uncomp: u64,
    local: usize,
}

fn find_eocd(bytes: &[u8]) -> Result<usize> {
    let lowest = bytes.len().saturating_sub(22 + usize::from(u16::MAX));
    let mut pos = bytes.len() - 22;
    loop {
        if le32(bytes, pos)? == SIG_EOCD {
            let comment = usize::from(le16(bytes, pos + 20)?);
            if pos + 22 + comment == bytes.len() {
                return Ok(pos);
            }
        }
        if pos == lowest {
            return Err(IdmlError::Zip("eocd_missing"));
        }
        pos -= 1;
    }
}

/// Reads every file entry as `(name, bytes)` in central-directory order.
pub(crate) fn read(
    bytes: &[u8],
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<Vec<(String, Vec<u8>)>> {
    cancel.check().map_err(|_| IdmlError::Canceled)?;
    if bytes.len() > limits.max_bytes {
        return Err(IdmlError::BudgetExceeded("bytes"));
    }
    if bytes.len() < 22 {
        return Err(IdmlError::Zip("too_short"));
    }
    let eocd = find_eocd(bytes)?;
    if le16(bytes, eocd + 4)? != 0 || le16(bytes, eocd + 6)? != 0 {
        return Err(IdmlError::Zip("multi_disk"));
    }
    let (n_disk, n_total) = (le16(bytes, eocd + 8)?, le16(bytes, eocd + 10)?);
    let (cd_size, cd_off) = (le32(bytes, eocd + 12)?, le32(bytes, eocd + 16)?);
    if n_total == u16::MAX || cd_size == u32::MAX || cd_off == u32::MAX {
        return Err(IdmlError::Zip("zip64_unsupported"));
    }
    if n_disk != n_total {
        return Err(IdmlError::Zip("multi_disk"));
    }
    let (cd_size, cd_off) = (cd_size as usize, cd_off as usize);
    if cd_off.checked_add(cd_size) != Some(eocd) {
        return Err(IdmlError::Zip("central_directory_bounds"));
    }
    if usize::from(n_total) > limits.max_entries {
        return Err(IdmlError::BudgetExceeded("entries"));
    }

    let mut entries: Vec<Central> = Vec::new();
    let (mut at, mut inflated) = (cd_off, 0u64);
    let mut seen = BTreeSet::new();
    for _ in 0..n_total {
        cancel.check().map_err(|_| IdmlError::Canceled)?;
        if le32(bytes, at)? != SIG_CENTRAL {
            return Err(IdmlError::Zip("central_signature"));
        }
        let flags = le16(bytes, at + 8)?;
        if flags & (1 | 0x40 | 0x2000) != 0 {
            return Err(IdmlError::Zip("encrypted"));
        }
        let method = le16(bytes, at + 10)?;
        let (crc, comp, uncomp) = (
            le32(bytes, at + 16)?,
            le32(bytes, at + 20)?,
            le32(bytes, at + 24)?,
        );
        let (n, m, k) = (
            usize::from(le16(bytes, at + 28)?),
            usize::from(le16(bytes, at + 30)?),
            usize::from(le16(bytes, at + 32)?),
        );
        let local = le32(bytes, at + 42)?;
        if comp == u32::MAX || uncomp == u32::MAX || local == u32::MAX {
            return Err(IdmlError::Zip("zip64_unsupported"));
        }
        if le16(bytes, at + 34)? != 0 {
            return Err(IdmlError::Zip("multi_disk"));
        }
        let name = std::str::from_utf8(take(bytes, at + 46, n)?)
            .map_err(|_| IdmlError::Zip("name_encoding"))?
            .to_owned();
        at = at
            .checked_add(46 + n + m + k)
            .filter(|end| *end <= eocd)
            .ok_or(IdmlError::Zip("central_directory_bounds"))?;
        if !safe_name(&name) {
            return Err(IdmlError::UnsafeEntryName);
        }
        if name.ends_with('/') {
            if comp != 0 || uncomp != 0 {
                return Err(IdmlError::Zip("directory_with_data"));
            }
            continue;
        }
        if !seen.insert(name.to_lowercase()) {
            return Err(IdmlError::DuplicateEntry);
        }
        if method != 0 && method != 8 {
            return Err(IdmlError::Zip("method_unsupported"));
        }
        let (comp, uncomp) = (u64::from(comp), u64::from(uncomp));
        if uncomp > limits.max_entry_bytes {
            return Err(IdmlError::BudgetExceeded("entry_bytes"));
        }
        inflated = inflated.saturating_add(uncomp);
        if inflated > limits.max_inflated_bytes {
            return Err(IdmlError::BudgetExceeded("inflated_bytes"));
        }
        if method == 0 && comp != uncomp {
            return Err(IdmlError::Zip("stored_size_mismatch"));
        }
        if method == 8 && uncomp > RATIO_FLOOR && uncomp > comp.saturating_mul(limits.max_ratio) {
            return Err(IdmlError::BudgetExceeded("ratio"));
        }
        entries.push(Central {
            name,
            method,
            crc,
            comp,
            uncomp,
            local: local as usize,
        });
    }
    if at != eocd {
        return Err(IdmlError::Zip("central_directory_bounds"));
    }

    match entries.iter().position(|e| e.name == MIMETYPE_NAME) {
        None => return Err(IdmlError::Mimetype("missing")),
        Some(0) if entries[0].local == 0 => {}
        Some(_) => return Err(IdmlError::Mimetype("not_first")),
    }
    if entries[0].method != 0 {
        return Err(IdmlError::Mimetype("compressed"));
    }

    let mut spans: Vec<(usize, usize)> = Vec::with_capacity(entries.len());
    let mut out = Vec::with_capacity(entries.len());
    for e in &entries {
        cancel.check().map_err(|_| IdmlError::Canceled)?;
        if le32(bytes, e.local)? != SIG_LOCAL {
            return Err(IdmlError::Zip("local_signature"));
        }
        let (n, m) = (
            usize::from(le16(bytes, e.local + 26)?),
            usize::from(le16(bytes, e.local + 28)?),
        );
        if take(bytes, e.local + 30, n)? != e.name.as_bytes() {
            return Err(IdmlError::Zip("local_name_mismatch"));
        }
        let start = e.local + 30 + n + m;
        let end = start
            .checked_add(e.comp as usize)
            .filter(|end| *end <= cd_off)
            .ok_or(IdmlError::Zip("entry_bounds"))?;
        spans.push((e.local, end));
        let data = take(bytes, start, e.comp as usize)?;
        let content = if e.method == 0 {
            data.to_vec()
        } else {
            decompress_to_vec_with_limit(data, e.uncomp as usize)
                .map_err(|_| IdmlError::Zip("inflate_failed_or_size_mismatch"))?
        };
        if content.len() as u64 != e.uncomp {
            return Err(IdmlError::Zip("size_mismatch"));
        }
        if crc32(&content) != e.crc {
            return Err(IdmlError::Zip("crc_mismatch"));
        }
        out.push((e.name.clone(), content));
    }
    spans.sort_unstable();
    if spans.windows(2).any(|w| w[0].1 > w[1].0) {
        return Err(IdmlError::Zip("overlapping_entries"));
    }
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
        let crc = crc32(data);
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
