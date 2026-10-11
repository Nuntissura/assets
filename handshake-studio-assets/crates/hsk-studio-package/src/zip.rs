//! In-house STORE-only ZIP/ZIP64 codec for the `.handshake` container (STU-IO-001/STU-IO-187).
//!
//! Canonical-form reader: one interpretation per archive. The reader rejects anything the writer
//! never emits, so parser differentials (prepended/trailing/concatenated data, gaps between
//! entries, overlapping entries, central-directory vs local-header disagreement, data
//! descriptors, archive/entry comments, encryption, compression) cannot hide content.
use crate::{
    error::PackageError,
    hashing::{check, crc32},
    limits::Limits,
};
use hsk_studio_accord::CancellationToken;
use std::{
    collections::{BTreeSet, HashSet},
    io::Write,
};

const SIG_LOCAL: u32 = 0x0403_4b50;
const SIG_CD: u32 = 0x0201_4b50;
const SIG_EOCD: u32 = 0x0605_4b50;
const SIG_EOCD64: u32 = 0x0606_4b50;
const SIG_LOCATOR64: u32 = 0x0706_4b50;
const SENT16: u16 = 0xFFFF;
const SENT32: u32 = 0xFFFF_FFFF;
const FLAG_UTF8: u16 = 0x0800;
/// 1980-01-01, 00:00:00: fixed so containers are deterministic.
const DOS_DATE: u16 = 0x0021;
const EXTRA_ZIP64: u16 = 0x0001;
const LOCAL_FIXED: usize = 30;
const CD_FIXED: usize = 46;
const EOCD_LEN: usize = 22;
const EOCD64_LEN: usize = 56;
const LOCATOR64_LEN: usize = 20;

/// Whether the writer emits ZIP64 structures only when required, or always.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Zip64Policy {
    #[default]
    Auto,
    Always,
}

/// Validated archive entry. `data` slices the exact buffer given to [`preflight`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryMeta {
    pub name: String,
    pub size: u64,
    pub crc32: u32,
    pub header_offset: u64,
    data_start: usize,
}

impl EntryMeta {
    /// Entry bytes; `bytes` must be the buffer that was preflighted (otherwise empty).
    pub fn data<'a>(&self, bytes: &'a [u8]) -> &'a [u8] {
        let end = self.data_start.saturating_add(self.size as usize);
        bytes.get(self.data_start..end).unwrap_or(&[])
    }
}

fn malformed(detail: &'static str) -> PackageError {
    PackageError::Malformed(detail)
}

// ---------------------------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------------------------

/// Entry names are restricted to ASCII `[A-Za-z0-9._-]` components joined by `/`: this removes
/// Unicode normalisation and case-fold ambiguity, traversal, absolute/device/drive forms and
/// reserved Windows names at the source.
pub(crate) fn validate_name(raw: &[u8], max_len: u16) -> Result<String, PackageError> {
    if raw.is_empty() {
        return Err(PackageError::UnsafeName);
    }
    if raw.len() > usize::from(max_len) {
        return Err(PackageError::NameTooLong);
    }
    if raw[0] == b'/' || raw[0] == b'\\' || raw.get(1) == Some(&b':') {
        return Err(PackageError::AbsoluteName);
    }
    if raw
        .split(|b| matches!(*b, b'/' | b'\\'))
        .any(|component| component == b"..")
    {
        return Err(PackageError::TraversalName);
    }
    let allowed = |b: &u8| b.is_ascii_alphanumeric() || matches!(*b, b'.' | b'_' | b'-' | b'/');
    if !raw.iter().all(allowed) {
        return Err(PackageError::UnsafeName);
    }
    let name = std::str::from_utf8(raw).map_err(|_| PackageError::UnsafeName)?;
    for component in name.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.ends_with('.')
            || is_reserved_device(component.split('.').next().unwrap_or(""))
        {
            return Err(PackageError::UnsafeName);
        }
    }
    Ok(name.to_owned())
}

fn is_reserved_device(stem: &str) -> bool {
    let upper = stem.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let bytes = upper.as_bytes();
    bytes.len() == 4
        && (upper.starts_with("COM") || upper.starts_with("LPT"))
        && matches!(bytes[3], b'1'..=b'9')
}

/// Duplicate, case-fold duplicate and file-vs-directory conflicts across a complete name set.
pub(crate) fn validate_name_set<'a>(
    names: impl IntoIterator<Item = &'a str>,
) -> Result<(), PackageError> {
    let mut exact = BTreeSet::new();
    let mut folded = HashSet::new();
    for name in names {
        if !exact.insert(name) {
            return Err(PackageError::DuplicateName);
        }
        if !folded.insert(name.to_ascii_lowercase()) {
            return Err(PackageError::CaseFoldDuplicate);
        }
    }
    for name in &exact {
        for (index, _) in name.match_indices('/') {
            if exact.contains(&name[..index]) {
                return Err(PackageError::PathConflict);
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------------------------

struct Record {
    name: String,
    crc: u32,
    size: u64,
    offset: u64,
}

pub(crate) struct WriteStats {
    pub entries: u32,
    pub bytes_written: u64,
    pub zip64: bool,
}

pub(crate) struct ZipWriter<'a, W: Write> {
    out: &'a mut W,
    pos: u64,
    records: Vec<Record>,
    limits: Limits,
    policy: Zip64Policy,
    total: u64,
}

fn put_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn put_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

impl<'a, W: Write> ZipWriter<'a, W> {
    pub(crate) fn new(out: &'a mut W, limits: Limits, policy: Zip64Policy) -> Self {
        Self {
            out,
            pos: 0,
            records: Vec::new(),
            limits,
            policy,
            total: 0,
        }
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), PackageError> {
        self.out
            .write_all(bytes)
            .map_err(|e| PackageError::Io(e.kind()))?;
        self.pos += bytes.len() as u64;
        Ok(())
    }

    fn always(&self) -> bool {
        self.policy == Zip64Policy::Always
    }

    pub(crate) fn add(
        &mut self,
        name: &str,
        data: &[u8],
        cancel: &CancellationToken,
    ) -> Result<(), PackageError> {
        check(cancel)?;
        if self.records.len() >= self.limits.max_entries as usize {
            return Err(PackageError::TooManyEntries);
        }
        validate_name(name.as_bytes(), self.limits.max_name_len)?;
        let size = data.len() as u64;
        if size > self.limits.max_entry_bytes {
            return Err(PackageError::EntryTooLarge);
        }
        self.total = self
            .total
            .checked_add(size)
            .filter(|t| *t <= self.limits.max_total_bytes)
            .ok_or(PackageError::TotalTooLarge)?;
        let crc = crc32(data, cancel)?;
        let zip64 = self.always() || size >= u64::from(SENT32);
        let offset = self.pos;
        let mut header = Vec::with_capacity(LOCAL_FIXED + name.len() + 20);
        put_u32(&mut header, SIG_LOCAL);
        put_u16(&mut header, if zip64 { 45 } else { 20 });
        put_u16(&mut header, FLAG_UTF8);
        put_u16(&mut header, 0); // STORE
        put_u16(&mut header, 0); // time
        put_u16(&mut header, DOS_DATE);
        put_u32(&mut header, crc);
        if zip64 {
            put_u32(&mut header, SENT32);
            put_u32(&mut header, SENT32);
        } else {
            put_u32(&mut header, size as u32);
            put_u32(&mut header, size as u32);
        }
        put_u16(&mut header, name.len() as u16);
        put_u16(&mut header, if zip64 { 20 } else { 0 });
        header.extend_from_slice(name.as_bytes());
        if zip64 {
            put_u16(&mut header, EXTRA_ZIP64);
            put_u16(&mut header, 16);
            put_u64(&mut header, size); // uncompressed
            put_u64(&mut header, size); // compressed
        }
        self.put(&header)?;
        self.put(data)?;
        self.records.push(Record {
            name: name.to_owned(),
            crc,
            size,
            offset,
        });
        Ok(())
    }

    pub(crate) fn finish(mut self, cancel: &CancellationToken) -> Result<WriteStats, PackageError> {
        let always = self.always();
        let cd_offset = self.pos;
        let mut cd = Vec::new();
        for rec in &self.records {
            check(cancel)?;
            let z_sizes = always || rec.size >= u64::from(SENT32);
            let z_offset = always || rec.offset >= u64::from(SENT32);
            let mut extra = Vec::new();
            if z_sizes {
                put_u64(&mut extra, rec.size); // uncompressed
                put_u64(&mut extra, rec.size); // compressed
            }
            if z_offset {
                put_u64(&mut extra, rec.offset);
            }
            let need = if z_sizes || z_offset { 45 } else { 20 };
            put_u32(&mut cd, SIG_CD);
            put_u16(&mut cd, 45); // version made by (MS-DOS, 4.5)
            put_u16(&mut cd, need);
            put_u16(&mut cd, FLAG_UTF8);
            put_u16(&mut cd, 0); // STORE
            put_u16(&mut cd, 0); // time
            put_u16(&mut cd, DOS_DATE);
            put_u32(&mut cd, rec.crc);
            let size32 = if z_sizes { SENT32 } else { rec.size as u32 };
            put_u32(&mut cd, size32);
            put_u32(&mut cd, size32);
            put_u16(&mut cd, rec.name.len() as u16);
            put_u16(
                &mut cd,
                if extra.is_empty() {
                    0
                } else {
                    (extra.len() + 4) as u16
                },
            );
            put_u16(&mut cd, 0); // comment
            put_u16(&mut cd, 0); // disk
            put_u16(&mut cd, 0); // internal attributes
            put_u32(&mut cd, 0); // external attributes
            put_u32(&mut cd, if z_offset { SENT32 } else { rec.offset as u32 });
            cd.extend_from_slice(rec.name.as_bytes());
            if !extra.is_empty() {
                put_u16(&mut cd, EXTRA_ZIP64);
                put_u16(&mut cd, extra.len() as u16);
                cd.extend_from_slice(&extra);
            }
        }
        self.put(&cd)?;
        let cd_size = cd.len() as u64;
        let count = self.records.len() as u64;
        let zip64 = always
            || count >= u64::from(SENT16)
            || cd_size >= u64::from(SENT32)
            || cd_offset >= u64::from(SENT32);
        let mut tail = Vec::with_capacity(EOCD64_LEN + LOCATOR64_LEN + EOCD_LEN);
        if zip64 {
            let eocd64_offset = self.pos;
            put_u32(&mut tail, SIG_EOCD64);
            put_u64(&mut tail, (EOCD64_LEN - 12) as u64);
            put_u16(&mut tail, 45);
            put_u16(&mut tail, 45);
            put_u32(&mut tail, 0);
            put_u32(&mut tail, 0);
            put_u64(&mut tail, count);
            put_u64(&mut tail, count);
            put_u64(&mut tail, cd_size);
            put_u64(&mut tail, cd_offset);
            put_u32(&mut tail, SIG_LOCATOR64);
            put_u32(&mut tail, 0);
            put_u64(&mut tail, eocd64_offset);
            put_u32(&mut tail, 1);
        }
        put_u32(&mut tail, SIG_EOCD);
        put_u16(&mut tail, 0);
        put_u16(&mut tail, 0);
        let count16 = if always || count >= u64::from(SENT16) {
            SENT16
        } else {
            count as u16
        };
        put_u16(&mut tail, count16);
        put_u16(&mut tail, count16);
        put_u32(
            &mut tail,
            if always || cd_size >= u64::from(SENT32) {
                SENT32
            } else {
                cd_size as u32
            },
        );
        put_u32(
            &mut tail,
            if always || cd_offset >= u64::from(SENT32) {
                SENT32
            } else {
                cd_offset as u32
            },
        );
        put_u16(&mut tail, 0); // archive comment length
        self.put(&tail)?;
        self.out
            .flush()
            .map_err(|e| PackageError::Io(e.kind()))?;
        Ok(WriteStats {
            entries: self.records.len() as u32,
            bytes_written: self.pos,
            zip64,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------------------------

struct Rd<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Rd<'a> {
    fn at(bytes: &'a [u8], pos: usize) -> Self {
        Self { bytes, pos }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], PackageError> {
        let end = self.pos.checked_add(n).ok_or(malformed("truncated"))?;
        let slice = self.bytes.get(self.pos..end).ok_or(malformed("truncated"))?;
        self.pos = end;
        Ok(slice)
    }
    fn u16(&mut self) -> Result<u16, PackageError> {
        let s = self.take(2)?;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Result<u32, PackageError> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn u64(&mut self) -> Result<u64, PackageError> {
        let s = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Ok(u64::from_le_bytes(a))
    }
}

/// Finds the (single) ZIP64 extra field; the extra block must tile exactly into TLV records.
fn zip64_extra(extra: &[u8]) -> Result<&[u8], PackageError> {
    let mut rd = Rd::at(extra, 0);
    let mut found: Option<&[u8]> = None;
    while rd.pos < extra.len() {
        let id = rd.u16()?;
        let size = usize::from(rd.u16()?);
        let data = rd.take(size)?;
        if id == EXTRA_ZIP64 {
            if found.is_some() {
                return Err(malformed("duplicate_zip64_extra"));
            }
            found = Some(data);
        }
    }
    Ok(found.unwrap_or(&[]))
}

struct Zip64Fields<'a>(&'a [u8]);

impl Zip64Fields<'_> {
    fn next(&mut self) -> Result<u64, PackageError> {
        if self.0.len() < 8 {
            return Err(malformed("zip64_extra_short"));
        }
        let (head, rest) = self.0.split_at(8);
        self.0 = rest;
        let mut a = [0u8; 8];
        a.copy_from_slice(head);
        Ok(u64::from_le_bytes(a))
    }
    fn finish(self) -> Result<(), PackageError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(malformed("zip64_extra_excess"))
        }
    }
}

struct CdEntry<'a> {
    name: String,
    name_raw: &'a [u8],
    flags: u16,
    crc: u32,
    size: u64,
    offset: usize,
}

struct EndRecord {
    entries: u64,
    cd_offset: usize,
    cd_size: usize,
}

fn to_usize(v: u64) -> Result<usize, PackageError> {
    usize::try_from(v).map_err(|_| malformed("offset_overflow"))
}

fn parse_end(bytes: &[u8], limits: &Limits) -> Result<EndRecord, PackageError> {
    if bytes.len() < EOCD_LEN {
        return Err(malformed("too_short"));
    }
    let eocd_pos = bytes.len() - EOCD_LEN;
    let mut rd = Rd::at(bytes, eocd_pos);
    if rd.u32()? != SIG_EOCD {
        // Includes trailing data and archive comments: the EOCD must be the final record.
        return Err(malformed("eocd_not_final"));
    }
    let disk = rd.u16()?;
    let cd_disk = rd.u16()?;
    let n_disk = rd.u16()?;
    let n_total = rd.u16()?;
    let cd_size32 = rd.u32()?;
    let cd_offset32 = rd.u32()?;
    let comment = rd.u16()?;
    if disk != 0 || cd_disk != 0 || n_disk != n_total || comment != 0 {
        return Err(malformed("eocd_fields"));
    }
    let locator_pos = eocd_pos.checked_sub(LOCATOR64_LEN);
    let has_locator = match locator_pos {
        Some(p) => Rd::at(bytes, p).u32()? == SIG_LOCATOR64,
        None => false,
    };
    let (entries, cd_size, cd_offset, region_end) = if has_locator {
        let locator_pos = locator_pos.ok_or(malformed("locator"))?;
        let mut rd = Rd::at(bytes, locator_pos + 4);
        if rd.u32()? != 0 {
            return Err(malformed("locator_disk"));
        }
        let eocd64_offset = to_usize(rd.u64()?)?;
        if rd.u32()? != 1 {
            return Err(malformed("locator_disks"));
        }
        if eocd64_offset.checked_add(EOCD64_LEN) != Some(locator_pos) {
            return Err(malformed("eocd64_position"));
        }
        let mut rd = Rd::at(bytes, eocd64_offset);
        if rd.u32()? != SIG_EOCD64 || rd.u64()? != (EOCD64_LEN - 12) as u64 {
            return Err(malformed("eocd64_record"));
        }
        let _made = rd.u16()?;
        let _need = rd.u16()?;
        if rd.u32()? != 0 || rd.u32()? != 0 {
            return Err(malformed("eocd64_disks"));
        }
        let n64_disk = rd.u64()?;
        let n64 = rd.u64()?;
        let size64 = rd.u64()?;
        let offset64 = rd.u64()?;
        if n64_disk != n64 {
            return Err(malformed("eocd64_entries"));
        }
        // Each 32/16-bit field is either its sentinel or must equal the 64-bit value.
        if (n_total != SENT16 && u64::from(n_total) != n64)
            || (cd_size32 != SENT32 && u64::from(cd_size32) != size64)
            || (cd_offset32 != SENT32 && u64::from(cd_offset32) != offset64)
        {
            return Err(malformed("eocd_zip64_disagree"));
        }
        (n64, to_usize(size64)?, to_usize(offset64)?, eocd64_offset)
    } else {
        if n_total == SENT16 || cd_size32 == SENT32 || cd_offset32 == SENT32 {
            return Err(malformed("zip64_required"));
        }
        (
            u64::from(n_total),
            cd_size32 as usize,
            cd_offset32 as usize,
            eocd_pos,
        )
    };
    if entries > u64::from(limits.max_entries) {
        return Err(PackageError::TooManyEntries);
    }
    if cd_offset.checked_add(cd_size) != Some(region_end) {
        return Err(malformed("central_directory_range"));
    }
    if cd_size < (entries as usize) * CD_FIXED {
        return Err(malformed("central_directory_size"));
    }
    Ok(EndRecord {
        entries,
        cd_offset,
        cd_size,
    })
}

/// Validates a complete STORE-only archive against `limits` and returns its entries.
///
/// Rejects (before reading any entry body): over-limit counts/sizes, unsafe, duplicate or
/// case-fold-duplicate names, non-STORE methods, flags/encryption/descriptors/comments, ZIP64
/// inconsistencies, overlapping/gapped/prepended/trailing layouts and central-directory vs
/// local-header disagreement; then verifies every CRC-32.
pub fn preflight(
    bytes: &[u8],
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<Vec<EntryMeta>, PackageError> {
    check(cancel)?;
    let end = parse_end(bytes, limits)?;
    let cd = bytes
        .get(end.cd_offset..end.cd_offset + end.cd_size)
        .ok_or(malformed("central_directory_range"))?;
    let mut rd = Rd::at(cd, 0);
    let mut entries: Vec<CdEntry<'_>> = Vec::with_capacity(end.entries as usize);
    let mut total: u64 = 0;
    for _ in 0..end.entries {
        check(cancel)?;
        if rd.u32()? != SIG_CD {
            return Err(malformed("central_directory_signature"));
        }
        let _made = rd.u16()?;
        let need = rd.u16()?;
        let flags = rd.u16()?;
        let method = rd.u16()?;
        let _time = rd.u16()?;
        let _date = rd.u16()?;
        let crc = rd.u32()?;
        let csize32 = rd.u32()?;
        let usize32 = rd.u32()?;
        let name_len = usize::from(rd.u16()?);
        let extra_len = usize::from(rd.u16()?);
        let comment_len = rd.u16()?;
        let disk_start = rd.u16()?;
        let _internal = rd.u16()?;
        let _external = rd.u32()?;
        let offset32 = rd.u32()?;
        if need > 45 || (flags & !FLAG_UTF8) != 0 {
            return Err(PackageError::UnsupportedFeature);
        }
        if method != 0 {
            return Err(PackageError::UnsupportedCompression);
        }
        if comment_len != 0 || disk_start != 0 {
            return Err(malformed("entry_comment_or_disk"));
        }
        if name_len > usize::from(limits.max_name_len) {
            return Err(PackageError::NameTooLong);
        }
        let name_raw = rd.take(name_len)?;
        let extra = rd.take(extra_len)?;
        let name = validate_name(name_raw, limits.max_name_len)?;
        let mut fields = Zip64Fields(zip64_extra(extra)?);
        let size = if usize32 == SENT32 {
            fields.next()?
        } else {
            u64::from(usize32)
        };
        let csize = if csize32 == SENT32 {
            fields.next()?
        } else {
            u64::from(csize32)
        };
        let offset = if offset32 == SENT32 {
            fields.next()?
        } else {
            u64::from(offset32)
        };
        fields.finish()?;
        if csize != size {
            return Err(malformed("stored_size_mismatch"));
        }
        if size > limits.max_entry_bytes {
            return Err(PackageError::EntryTooLarge);
        }
        total = total
            .checked_add(size)
            .filter(|t| *t <= limits.max_total_bytes)
            .ok_or(PackageError::TotalTooLarge)?;
        entries.push(CdEntry {
            name,
            name_raw,
            flags,
            crc,
            size,
            offset: to_usize(offset)?,
        });
    }
    if rd.pos != cd.len() {
        return Err(malformed("central_directory_excess"));
    }
    validate_name_set(entries.iter().map(|e| e.name.as_str()))?;

    // Layout: entries tile [0, cd_offset) exactly, in header-offset order.
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by_key(|&i| entries[i].offset);
    let mut expected = 0usize;
    let mut metas: Vec<Option<EntryMeta>> = vec![None; entries.len()];
    for &i in &order {
        check(cancel)?;
        let e = &entries[i];
        if e.offset != expected {
            return Err(malformed("layout_gap_or_overlap"));
        }
        let mut rd = Rd::at(bytes, e.offset);
        if rd.u32()? != SIG_LOCAL {
            return Err(malformed("local_signature"));
        }
        let _need = rd.u16()?;
        let flags = rd.u16()?;
        let method = rd.u16()?;
        let _time = rd.u16()?;
        let _date = rd.u16()?;
        let crc = rd.u32()?;
        let csize32 = rd.u32()?;
        let usize32 = rd.u32()?;
        let name_len = usize::from(rd.u16()?);
        let extra_len = usize::from(rd.u16()?);
        let name = rd.take(name_len)?;
        let extra = rd.take(extra_len)?;
        if flags != e.flags || method != 0 || crc != e.crc || name != e.name_raw {
            return Err(malformed("local_header_disagrees"));
        }
        let mut fields = Zip64Fields(zip64_extra(extra)?);
        let (lsize, lcsize) = match (usize32 == SENT32, csize32 == SENT32) {
            (true, true) => (fields.next()?, fields.next()?),
            (false, false) => (u64::from(usize32), u64::from(csize32)),
            _ => return Err(malformed("local_zip64_partial")),
        };
        fields.finish()?;
        if lsize != e.size || lcsize != e.size {
            return Err(malformed("local_header_disagrees"));
        }
        let data_start = rd.pos;
        let data_end = data_start
            .checked_add(to_usize(e.size)?)
            .ok_or(malformed("offset_overflow"))?;
        if data_end > end.cd_offset {
            return Err(malformed("entry_overruns_central_directory"));
        }
        expected = data_end;
        metas[i] = Some(EntryMeta {
            name: e.name.clone(),
            size: e.size,
            crc32: e.crc,
            header_offset: e.offset as u64,
            data_start,
        });
    }
    if expected != end.cd_offset {
        return Err(malformed("layout_trailing_data"));
    }
    let metas: Vec<EntryMeta> = metas.into_iter().flatten().collect();
    for meta in &metas {
        if crc32(meta.data(bytes), cancel)? != meta.crc32 {
            return Err(PackageError::CrcMismatch);
        }
    }
    Ok(metas)
}
