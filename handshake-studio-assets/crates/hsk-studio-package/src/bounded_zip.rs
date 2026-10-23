//! Bounded NON-canonical ZIP reader shared by the Studio interop crates (D16, STU-IO-187).
//!
//! This is not the `.handshake` container reader (that stays STORE-only and canonical in `zip`).
//! It reads third-party archives (IDML, `.fig`): STORE and DEFLATE entries, ZIP64 only when the
//! caller opts in, archive comments allowed. It enforces: input, entry-count, per-entry and
//! aggregate inflated byte budgets and an expansion-ratio bound (all taken from the central
//! directory before any body is read); names with traversal, absolute, backslash, drive or control
//! characters or components, exact and case-folded duplicates are refused; encrypted entries and
//! methods other than 0 and 8 are refused; each local header must agree with the central
//! directory (encryption flags, method, CRC and sizes; a data descriptor may defer CRC and sizes
//! with all-zero local fields); entry data must lie before the central directory and must not
//! overlap; DEFLATE data must end exactly at the declared compressed size; the CRC-32 and the
//! declared size of every entry are verified. The reader never touches the file system.
#![deny(clippy::arithmetic_side_effects)]
use crate::hashing::crc32_of;
use hsk_studio_accord::CancellationToken;
use std::collections::BTreeSet;

const SIG_LOCAL: u32 = 0x0403_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_EOCD: u32 = 0x0605_4b50;
const SIG_EOCD64: u32 = 0x0606_4b50;
const SIG_LOCATOR64: u32 = 0x0706_4b50;
const EXTRA_ZIP64: u16 = 0x0001;
const EOCD_LEN: usize = 22;
const EOCD64_LEN: usize = 56;
const LOCATOR64_LEN: usize = 20;
/// Expansion ratio is only judged for DEFLATE entries above this inflated size.
const RATIO_FLOOR: u64 = 1 << 20;
/// Longest accepted entry name in bytes (the strictest of the readers this one replaced).
pub const MAX_ZIP_NAME_LEN: usize = 255;

/// Failure of [`ZipDirectory::parse`] / [`ZipDirectory::read_all`]; codes are fixed tokens, never
/// input-derived text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZipReadError {
    Canceled,
    /// Structurally invalid or unsupported archive; the detail is a fixed token.
    Malformed(&'static str),
    /// A declared bound was exceeded; the detail names the bound.
    BudgetExceeded(&'static str),
    UnsafeName,
    DuplicateName,
}

impl ZipReadError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Canceled => "canceled",
            Self::Malformed(detail) | Self::BudgetExceeded(detail) => detail,
            Self::UnsafeName => "unsafe_entry_name",
            Self::DuplicateName => "duplicate_entry",
        }
    }
}

type Res<T> = Result<T, ZipReadError>;

/// Declared bounds of one read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZipLimits {
    /// Input bytes.
    pub max_bytes: usize,
    pub max_entries: usize,
    /// Declared uncompressed bytes of one entry.
    pub max_entry_bytes: u64,
    /// Declared uncompressed bytes over all entries.
    pub max_inflated_bytes: u64,
    /// Expansion ratio, judged for DEFLATE entries above 1 MiB inflated.
    pub max_ratio: u64,
    /// Accept ZIP64 structures; when false they are the typed refusal `zip64_unsupported`.
    pub allow_zip64: bool,
}

/// One file entry of the central directory (directories are skipped).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZipEntry {
    pub name: String,
    /// 0 = STORE, 8 = DEFLATE (anything else is refused during parse).
    pub method: u16,
    pub crc32: u32,
    pub compressed_size: u64,
    pub size: u64,
    local: usize,
}

impl ZipEntry {
    /// Offset of the entry's local header in the input.
    pub fn local_offset(&self) -> usize {
        self.local
    }
}

/// A validated central directory over the exact buffer given to [`ZipDirectory::parse`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZipDirectory {
    entries: Vec<ZipEntry>,
    cd_off: usize,
    allow_zip64: bool,
}

fn check(cancel: &CancellationToken) -> Res<()> {
    cancel.check().map_err(|_| ZipReadError::Canceled)
}

fn malformed(detail: &'static str) -> ZipReadError {
    ZipReadError::Malformed(detail)
}

/// `bytes[at + delta .. at + delta + len]`, every sum checked.
fn take(bytes: &[u8], at: usize, delta: usize, len: usize) -> Res<&[u8]> {
    let start = at.checked_add(delta);
    let end = start.and_then(|s| s.checked_add(len));
    match (start, end) {
        (Some(s), Some(e)) => bytes.get(s..e),
        _ => None,
    }
    .ok_or(malformed("truncated"))
}
fn u16_at(bytes: &[u8], at: usize, delta: usize) -> Res<u16> {
    let s = take(bytes, at, delta, 2)?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}
fn u32_at(bytes: &[u8], at: usize, delta: usize) -> Res<u32> {
    let s = take(bytes, at, delta, 4)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn u64_at(bytes: &[u8], at: usize, delta: usize) -> Res<u64> {
    let s = take(bytes, at, delta, 8)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Ok(u64::from_le_bytes(a))
}

fn to_usize(v: u64, detail: &'static str) -> Res<usize> {
    usize::try_from(v).map_err(|_| malformed(detail))
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_ZIP_NAME_LEN
        && !name.starts_with('/')
        && !name.contains(['\\', ':'])
        && name.chars().all(|c| !c.is_control())
        && name
            .trim_end_matches('/')
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

/// End-of-central-directory record: the last position whose comment length reaches the input end.
fn find_eocd(bytes: &[u8]) -> Res<usize> {
    let last = bytes
        .len()
        .checked_sub(EOCD_LEN)
        .ok_or(malformed("too_short"))?;
    let lowest = last.saturating_sub(usize::from(u16::MAX));
    let mut pos = last;
    loop {
        if u32_at(bytes, pos, 0)? == SIG_EOCD {
            let comment = usize::from(u16_at(bytes, pos, 20)?);
            if pos.checked_add(EOCD_LEN).and_then(|p| p.checked_add(comment)) == Some(bytes.len())
            {
                return Ok(pos);
            }
        }
        if pos == lowest {
            return Err(malformed("eocd_missing"));
        }
        pos = pos.checked_sub(1).ok_or(malformed("eocd_missing"))?;
    }
}

struct End {
    n_total: usize,
    cd_off: usize,
    /// Start of the record that follows the central directory (EOCD or ZIP64 EOCD).
    region_end: usize,
}

fn parse_end(bytes: &[u8], limits: &ZipLimits) -> Res<End> {
    let bounds = malformed("central_directory_bounds");
    let eocd = find_eocd(bytes)?;
    if u16_at(bytes, eocd, 4)? != 0 || u16_at(bytes, eocd, 6)? != 0 {
        return Err(malformed("multi_disk"));
    }
    let (n_disk, n_total16) = (u16_at(bytes, eocd, 8)?, u16_at(bytes, eocd, 10)?);
    let (cd_size32, cd_off32) = (u32_at(bytes, eocd, 12)?, u32_at(bytes, eocd, 16)?);
    let needs64 = n_total16 == u16::MAX || cd_size32 == u32::MAX || cd_off32 == u32::MAX;
    let (n_total, cd_size, cd_off, region_end) = if needs64 {
        if !limits.allow_zip64 {
            return Err(malformed("zip64_unsupported"));
        }
        let locator = eocd
            .checked_sub(LOCATOR64_LEN)
            .ok_or(malformed("zip64_locator_missing"))?;
        if u32_at(bytes, locator, 0)? != SIG_LOCATOR64 {
            return Err(malformed("zip64_locator_missing"));
        }
        if u32_at(bytes, locator, 4)? != 0 || u32_at(bytes, locator, 16)? != 1 {
            return Err(malformed("zip64_locator_disks"));
        }
        let eocd64 = to_usize(u64_at(bytes, locator, 8)?, "eocd64_position")?;
        if eocd64.checked_add(EOCD64_LEN) != Some(locator) {
            return Err(malformed("eocd64_position"));
        }
        if u32_at(bytes, eocd64, 0)? != SIG_EOCD64
            || u64_at(bytes, eocd64, 4)? != (EOCD64_LEN.saturating_sub(12)) as u64
        {
            return Err(malformed("eocd64_record"));
        }
        if u32_at(bytes, eocd64, 16)? != 0 || u32_at(bytes, eocd64, 20)? != 0 {
            return Err(malformed("multi_disk"));
        }
        let (n64_disk, n64) = (u64_at(bytes, eocd64, 24)?, u64_at(bytes, eocd64, 32)?);
        let (size64, off64) = (u64_at(bytes, eocd64, 40)?, u64_at(bytes, eocd64, 48)?);
        if n64_disk != n64 {
            return Err(malformed("multi_disk"));
        }
        // Each 32/16-bit field is either its sentinel or must equal the 64-bit value.
        if (n_total16 != u16::MAX && u64::from(n_total16) != n64)
            || (cd_size32 != u32::MAX && u64::from(cd_size32) != size64)
            || (cd_off32 != u32::MAX && u64::from(cd_off32) != off64)
        {
            return Err(malformed("eocd_zip64_disagree"));
        }
        // An entry count above the bound is refused before it is narrowed or used as a loop count.
        let n_total = usize::try_from(n64)
            .ok()
            .filter(|n| *n <= limits.max_entries)
            .ok_or(ZipReadError::BudgetExceeded("entries"))?;
        (
            n_total,
            to_usize(size64, "central_directory_bounds")?,
            to_usize(off64, "central_directory_bounds")?,
            eocd64,
        )
    } else {
        if n_disk != n_total16 {
            return Err(malformed("multi_disk"));
        }
        (
            usize::from(n_total16),
            to_usize(u64::from(cd_size32), "central_directory_bounds")?,
            to_usize(u64::from(cd_off32), "central_directory_bounds")?,
            eocd,
        )
    };
    if cd_off.checked_add(cd_size) != Some(region_end) {
        return Err(bounds);
    }
    if n_total > limits.max_entries {
        return Err(ZipReadError::BudgetExceeded("entries"));
    }
    Ok(End {
        n_total,
        cd_off,
        region_end,
    })
}

/// Values of the ZIP64 extended-information field in `extra`, in the order the format defines:
/// uncompressed size, compressed size, local header offset (each present only when its 32-bit
/// field holds the sentinel). The extra block must tile exactly into `id,len,data` records.
struct Zip64Extra<'a>(&'a [u8]);

fn zip64_extra(extra: &[u8]) -> Res<Option<Zip64Extra<'_>>> {
    let (mut at, mut found) = (0usize, None);
    while at < extra.len() {
        let id = u16_at(extra, at, 0)?;
        let len = usize::from(u16_at(extra, at, 2)?);
        let data = take(extra, at, 4, len)?;
        if id == EXTRA_ZIP64 {
            if found.is_some() {
                return Err(malformed("duplicate_zip64_extra"));
            }
            found = Some(Zip64Extra(data));
        }
        at = at
            .checked_add(4)
            .and_then(|a| a.checked_add(len))
            .ok_or(malformed("truncated"))?;
    }
    Ok(found)
}

impl Zip64Extra<'_> {
    fn next(&mut self) -> Res<u64> {
        let Some((head, rest)) = self.0.split_first_chunk::<8>() else {
            return Err(malformed("zip64_extra_short"));
        };
        self.0 = rest;
        Ok(u64::from_le_bytes(*head))
    }
}

impl ZipDirectory {
    /// Validates the archive structure and central directory against `limits`; no entry body is
    /// read.
    pub fn parse(bytes: &[u8], limits: &ZipLimits, cancel: &CancellationToken) -> Res<Self> {
        check(cancel)?;
        if bytes.len() > limits.max_bytes {
            return Err(ZipReadError::BudgetExceeded("bytes"));
        }
        let End {
            n_total,
            cd_off,
            region_end,
        } = parse_end(bytes, limits)?;
        let mut entries: Vec<ZipEntry> = Vec::new();
        let (mut at, mut inflated) = (cd_off, 0u64);
        let mut seen = BTreeSet::new();
        for _ in 0..n_total {
            check(cancel)?;
            if u32_at(bytes, at, 0)? != SIG_CENTRAL {
                return Err(malformed("central_signature"));
            }
            let flags = u16_at(bytes, at, 8)?;
            if flags & (1 | 0x40 | 0x2000) != 0 {
                return Err(malformed("encrypted"));
            }
            let method = u16_at(bytes, at, 10)?;
            let (crc, comp32, uncomp32) = (
                u32_at(bytes, at, 16)?,
                u32_at(bytes, at, 20)?,
                u32_at(bytes, at, 24)?,
            );
            let (n, m, k) = (
                usize::from(u16_at(bytes, at, 28)?),
                usize::from(u16_at(bytes, at, 30)?),
                usize::from(u16_at(bytes, at, 32)?),
            );
            let local32 = u32_at(bytes, at, 42)?;
            let sentinel = comp32 == u32::MAX || uncomp32 == u32::MAX || local32 == u32::MAX;
            if sentinel && !limits.allow_zip64 {
                return Err(malformed("zip64_unsupported"));
            }
            if u16_at(bytes, at, 34)? != 0 {
                return Err(malformed("multi_disk"));
            }
            let name = std::str::from_utf8(take(bytes, at, 46, n)?)
                .map_err(|_| malformed("name_encoding"))?
                .to_owned();
            let (mut comp, mut uncomp, mut local) = (
                u64::from(comp32),
                u64::from(uncomp32),
                u64::from(local32),
            );
            if sentinel {
                let extra_at = 46usize.saturating_add(n);
                let extra = take(bytes, at, extra_at, m)?;
                let mut fields = zip64_extra(extra)?.ok_or(malformed("zip64_extra_missing"))?;
                if uncomp32 == u32::MAX {
                    uncomp = fields.next()?;
                }
                if comp32 == u32::MAX {
                    comp = fields.next()?;
                }
                if local32 == u32::MAX {
                    local = fields.next()?;
                }
            }
            // n, m and k are u16 values: the saturating sums cannot wrap, and the end is checked.
            let entry_len = 46usize
                .saturating_add(n)
                .saturating_add(m)
                .saturating_add(k);
            at = at
                .checked_add(entry_len)
                .filter(|end| *end <= region_end)
                .ok_or(malformed("central_directory_bounds"))?;
            if !safe_name(&name) {
                return Err(ZipReadError::UnsafeName);
            }
            if name.ends_with('/') {
                if comp != 0 || uncomp != 0 {
                    return Err(malformed("directory_with_data"));
                }
                continue;
            }
            if !seen.insert(name.to_lowercase()) {
                return Err(ZipReadError::DuplicateName);
            }
            if method != 0 && method != 8 {
                return Err(malformed("method_unsupported"));
            }
            if uncomp > limits.max_entry_bytes {
                return Err(ZipReadError::BudgetExceeded("entry_bytes"));
            }
            inflated = inflated.saturating_add(uncomp);
            if inflated > limits.max_inflated_bytes {
                return Err(ZipReadError::BudgetExceeded("inflated_bytes"));
            }
            if method == 0 && comp != uncomp {
                return Err(malformed("stored_size_mismatch"));
            }
            if method == 8 && uncomp > RATIO_FLOOR && uncomp > comp.saturating_mul(limits.max_ratio)
            {
                return Err(ZipReadError::BudgetExceeded("ratio"));
            }
            entries.push(ZipEntry {
                name,
                method,
                crc32: crc,
                compressed_size: comp,
                size: uncomp,
                local: to_usize(local, "entry_bounds")?,
            });
        }
        if at != region_end {
            return Err(malformed("central_directory_bounds"));
        }
        Ok(Self {
            entries,
            cd_off,
            allow_zip64: limits.allow_zip64,
        })
    }

    /// File entries in central-directory order.
    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    /// Reads every entry as `(name, bytes)`: local headers are cross-checked against the central
    /// directory, entry data is checked for overlap, DEFLATE is inflated within the declared size,
    /// and size and CRC-32 are verified. `bytes` must be the buffer given to `parse`.
    pub fn read_all(
        &self,
        bytes: &[u8],
        cancel: &CancellationToken,
    ) -> Res<Vec<(String, Vec<u8>)>> {
        let entry_bounds = || malformed("entry_bounds");
        let mut spans: Vec<(usize, usize)> = Vec::with_capacity(self.entries.len());
        let mut out = Vec::with_capacity(self.entries.len());
        for e in &self.entries {
            check(cancel)?;
            if u32_at(bytes, e.local, 0)? != SIG_LOCAL {
                return Err(malformed("local_signature"));
            }
            let (n, m) = (
                usize::from(u16_at(bytes, e.local, 26)?),
                usize::from(u16_at(bytes, e.local, 28)?),
            );
            if take(bytes, e.local, 30, n)? != e.name.as_bytes() {
                return Err(malformed("local_name_mismatch"));
            }
            // A reader that trusts the local header must see the same entry as the central
            // directory: same encryption flags, method, CRC and sizes. A data descriptor (flag
            // bit 3) may defer the CRC and sizes, in which case the local fields are all zero and
            // the central values rule.
            let flags = u16_at(bytes, e.local, 6)?;
            if flags & (1 | 0x40 | 0x2000) != 0 {
                return Err(malformed("encrypted"));
            }
            if u16_at(bytes, e.local, 8)? != e.method {
                return Err(malformed("local_method_mismatch"));
            }
            let (crc, comp32, uncomp32) = (
                u32_at(bytes, e.local, 14)?,
                u32_at(bytes, e.local, 18)?,
                u32_at(bytes, e.local, 22)?,
            );
            let deferred = flags & 8 != 0 && (crc, comp32, uncomp32) == (0, 0, 0);
            if !deferred {
                let (mut comp, mut uncomp) = (u64::from(comp32), u64::from(uncomp32));
                if self.allow_zip64 && (comp32 == u32::MAX || uncomp32 == u32::MAX) {
                    let extra = take(bytes, e.local, 30usize.saturating_add(n), m)?;
                    let mut fields = zip64_extra(extra)?.ok_or(malformed("zip64_extra_missing"))?;
                    if uncomp32 == u32::MAX {
                        uncomp = fields.next()?;
                    }
                    if comp32 == u32::MAX {
                        comp = fields.next()?;
                    }
                }
                if (crc, comp, uncomp) != (e.crc32, e.compressed_size, e.size) {
                    return Err(malformed("local_header_mismatch"));
                }
            }
            let comp_len = to_usize(e.compressed_size, "entry_bounds")?;
            let start = e
                .local
                .checked_add(30)
                .and_then(|s| s.checked_add(n))
                .and_then(|s| s.checked_add(m))
                .ok_or_else(entry_bounds)?;
            let end = start
                .checked_add(comp_len)
                .filter(|end| *end <= self.cd_off)
                .ok_or_else(entry_bounds)?;
            spans.push((e.local, end));
            let data = take(bytes, start, 0, comp_len)?;
            let content = if e.method == 0 {
                data.to_vec()
            } else {
                let cap = to_usize(e.size, "entry_bounds")?;
                inflate_exact(data, cap).map_err(|fail| match fail {
                    InflateFail::Trailing => malformed("trailing_compressed_bytes"),
                    InflateFail::OutputLimit | InflateFail::Invalid => {
                        malformed("inflate_failed_or_size_mismatch")
                    }
                })?
            };
            if u64::try_from(content.len()).ok() != Some(e.size) {
                return Err(malformed("size_mismatch"));
            }
            check(cancel)?;
            if crc32_of(&content) != e.crc32 {
                return Err(malformed("crc_mismatch"));
            }
            out.push((e.name.clone(), content));
        }
        spans.sort_unstable();
        if spans.windows(2).any(|w| w[0].1 > w[1].0) {
            return Err(malformed("overlapping_entries"));
        }
        Ok(out)
    }
}

/// Parses and reads every file entry of the archive as `(name, bytes)` in central-directory order.
pub fn read_zip(
    bytes: &[u8],
    limits: &ZipLimits,
    cancel: &CancellationToken,
) -> Res<Vec<(String, Vec<u8>)>> {
    ZipDirectory::parse(bytes, limits, cancel)?.read_all(bytes, cancel)
}

/// Why a raw DEFLATE stream was refused by [`inflate_exact`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InflateFail {
    /// More than `cap` bytes would be produced.
    OutputLimit,
    /// Not a valid DEFLATE stream (or truncated).
    Invalid,
    /// Valid stream, but input bytes remain after its final block.
    Trailing,
}

/// Raw DEFLATE (RFC 1951) of exactly `data` into at most `cap` bytes: the stream must end at the
/// end of `data`, because bytes after the final block carry no data and would let two readers
/// disagree about the entry. Uses miniz_oxide's own resumable inflater; the growth loop mirrors the
/// one in its `decompress_to_vec_with_limit`, plus the consumed-input check that function does not
/// make.
pub fn inflate_exact(data: &[u8], cap: usize) -> Result<Vec<u8>, InflateFail> {
    use miniz_oxide::inflate::TINFLStatus;
    use miniz_oxide::inflate::core::{
        DecompressorOxide, decompress, inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF,
    };
    let mut out = vec![0u8; data.len().saturating_mul(2).min(cap)];
    let mut state = Box::<DecompressorOxide>::default();
    let (mut in_pos, mut out_pos) = (0usize, 0usize);
    loop {
        let input = data.get(in_pos..).ok_or(InflateFail::Invalid)?;
        let (status, used, wrote) = decompress(
            &mut state,
            input,
            &mut out,
            out_pos,
            TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF,
        );
        in_pos = in_pos.checked_add(used).ok_or(InflateFail::Invalid)?;
        out_pos = out_pos.checked_add(wrote).ok_or(InflateFail::Invalid)?;
        match status {
            TINFLStatus::Done => {
                if in_pos != data.len() {
                    return Err(InflateFail::Trailing);
                }
                out.truncate(out_pos);
                return Ok(out);
            }
            TINFLStatus::HasMoreOutput => {
                let grown = out.len().saturating_mul(2).max(64).min(cap);
                if grown <= out.len() {
                    return Err(InflateFail::OutputLimit);
                }
                out.resize(grown, 0);
            }
            _ => return Err(InflateFail::Invalid),
        }
    }
}
