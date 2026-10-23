//! Read-only WAL snapshot: committed-prefix overlay of the granted `-wal` bytes plus the stable
//! source fingerprint. The WAL is never replayed into the main file; the overlay only maps page
//! numbers to byte offsets inside the granted WAL (format: sqlite.org/fileformat2.html).
use crate::error::{LimitKind, LrcatError, Result, be32, check_cancel};
use crate::source::{ByteSource, Grant, Limits, WalGrant};
use hsk_studio_accord::CancellationToken;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(crate) const WAL_HEADER_BYTES: u64 = 32;
pub(crate) const WAL_FRAME_HEADER_BYTES: u64 = 24;
const WAL_MAGIC_LE: u32 = 0x377f_0682;
const WAL_MAGIC_BE: u32 = 0x377f_0683;
const WAL_VERSION: u32 = 3_007_000;
const HASH_CHUNK: usize = 64 * 1024;

/// Identity of the granted bytes. Equal fingerprints before and after a read prove the handles
/// were stable for that read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    pub main_len: u64,
    /// Length of the whole granted WAL (0 when none).
    pub wal_len: u64,
    /// Bytes of the WAL up to and including the last commit frame (0 when none).
    pub committed_wal_len: u64,
    pub sha256_main: [u8; 32],
    /// SHA-256 of the whole granted WAL; all zero when no WAL is granted.
    pub sha256_wal: [u8; 32],
}

impl Fingerprint {
    pub fn main_sha256_hex(&self) -> String {
        hex(&self.sha256_main)
    }
    pub fn wal_sha256_hex(&self) -> String {
        hex(&self.sha256_wal)
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 15)] as char);
    }
    out
}

/// Whole-byte identity taken before and after decoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SourceDigest {
    pub main_len: u64,
    pub wal_len: u64,
    pub sha256_main: [u8; 32],
    pub sha256_wal: [u8; 32],
}

fn hash_source(src: &dyn ByteSource, cancel: &CancellationToken) -> Result<[u8; 32]> {
    let len = src.len();
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; HASH_CHUNK];
    let mut offset = 0u64;
    while offset < len {
        check_cancel(cancel)?;
        let n = (len - offset).min(HASH_CHUNK as u64) as usize;
        src.read_at(offset, &mut buf[..n])?;
        hasher.update(&buf[..n]);
        offset += n as u64;
    }
    Ok(hasher.finalize().into())
}

pub(crate) fn digest(grant: &Grant, cancel: &CancellationToken) -> Result<SourceDigest> {
    let sha256_main = hash_source(grant.main.as_ref(), cancel)?;
    let (wal_len, sha256_wal) = match &grant.wal {
        WalGrant::Granted(src) => (src.len(), hash_source(src.as_ref(), cancel)?),
        WalGrant::None | WalGrant::PresentButNotGranted => (0, [0u8; 32]),
    };
    Ok(SourceDigest {
        main_len: grant.main.len(),
        wal_len,
        sha256_main,
        sha256_wal,
    })
}

/// Committed view of a granted WAL.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WalOverlay {
    /// Page number -> byte offset of that page's data inside the granted WAL.
    pub frames: BTreeMap<u32, u64>,
    /// Database size in pages after the last commit frame.
    pub db_pages: Option<u32>,
    pub committed_frames: u64,
    /// Valid frames after the last commit frame (an uncommitted transaction): ignored.
    pub ignored_tail_frames: u64,
    pub committed_len: u64,
    /// Bytes after the last valid frame (partial or invalid frames): ignored.
    pub invalid_tail_bytes: u64,
}

/// SQLite WAL checksum over 8-byte word pairs, seeded by the previous cumulative values.
fn checksum(data: &[u8], big_endian: bool, mut s0: u32, mut s1: u32) -> (u32, u32) {
    for pair in data.chunks_exact(8) {
        let (a, b) = if big_endian {
            (
                u32::from_be_bytes([pair[0], pair[1], pair[2], pair[3]]),
                u32::from_be_bytes([pair[4], pair[5], pair[6], pair[7]]),
            )
        } else {
            (
                u32::from_le_bytes([pair[0], pair[1], pair[2], pair[3]]),
                u32::from_le_bytes([pair[4], pair[5], pair[6], pair[7]]),
            )
        };
        s0 = s0.wrapping_add(a).wrapping_add(s1);
        s1 = s1.wrapping_add(b).wrapping_add(s0);
    }
    (s0, s1)
}

/// Accepts only frames up to the last valid commit frame; stops at the first salt or checksum
/// mismatch. A WAL header that does not validate fails closed (the main file could be stale).
pub(crate) fn scan_wal(
    wal: &dyn ByteSource,
    main_page_size: u32,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<WalOverlay> {
    let len = wal.len();
    if len > limits.max_wal_bytes {
        return Err(LrcatError::Limit(LimitKind::WalBytes));
    }
    let mut overlay = WalOverlay::default();
    if len == 0 {
        return Ok(overlay);
    }
    if len < WAL_HEADER_BYTES {
        return Err(LrcatError::Corrupt("wal_header_truncated"));
    }
    let mut header = [0u8; WAL_HEADER_BYTES as usize];
    wal.read_at(0, &mut header)?;
    let big_endian = match be32(&header[0..]) {
        WAL_MAGIC_LE => false,
        WAL_MAGIC_BE => true,
        _ => return Err(LrcatError::Corrupt("wal_magic")),
    };
    if be32(&header[4..]) != WAL_VERSION {
        return Err(LrcatError::Unsupported("wal_version"));
    }
    if be32(&header[8..]) != main_page_size {
        return Err(LrcatError::Corrupt("wal_page_size"));
    }
    let (salt1, salt2) = (be32(&header[16..]), be32(&header[20..]));
    let (c0, c1) = checksum(&header[..24], big_endian, 0, 0);
    if c0 != be32(&header[24..]) || c1 != be32(&header[28..]) {
        return Err(LrcatError::Corrupt("wal_header_checksum"));
    }

    let frame_len = WAL_FRAME_HEADER_BYTES + u64::from(main_page_size);
    let frame_count = (len - WAL_HEADER_BYTES) / frame_len;
    let mut buf = vec![0u8; frame_len as usize];
    let (mut s0, mut s1) = (c0, c1);
    let mut pending: Vec<(u32, u64)> = Vec::new();
    let mut valid_frames = 0u64;
    for index in 0..frame_count {
        if index % 64 == 0 {
            check_cancel(cancel)?;
        }
        let offset = WAL_HEADER_BYTES + index * frame_len;
        wal.read_at(offset, &mut buf)?;
        let page_no = be32(&buf[0..]);
        let commit_size = be32(&buf[4..]);
        if page_no == 0 || be32(&buf[8..]) != salt1 || be32(&buf[12..]) != salt2 {
            break;
        }
        let (n0, n1) = checksum(&buf[0..8], big_endian, s0, s1);
        let (n0, n1) = checksum(&buf[WAL_FRAME_HEADER_BYTES as usize..], big_endian, n0, n1);
        if n0 != be32(&buf[16..]) || n1 != be32(&buf[20..]) {
            break;
        }
        (s0, s1) = (n0, n1);
        valid_frames += 1;
        pending.push((page_no, offset + WAL_FRAME_HEADER_BYTES));
        if commit_size != 0 {
            for (page, data_offset) in pending.drain(..) {
                overlay.frames.insert(page, data_offset);
            }
            overlay.db_pages = Some(commit_size);
            overlay.committed_frames = index + 1;
        }
    }
    overlay.ignored_tail_frames = valid_frames - overlay.committed_frames;
    overlay.committed_len = if overlay.committed_frames == 0 {
        0
    } else {
        WAL_HEADER_BYTES + overlay.committed_frames * frame_len
    };
    overlay.invalid_tail_bytes = len - (WAL_HEADER_BYTES + valid_frames * frame_len);
    Ok(overlay)
}
