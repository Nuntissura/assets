#![deny(clippy::arithmetic_side_effects)]
//! INTERNAL bounded read-only decoder of untrusted SQLite file bytes: header, table b-tree
//! pages, cell/record format, overflow chains, schema table, freelist check. Nothing here is
//! `pub` outside the crate: no engine, no query API, no cache, no write path. Format source:
//! https://www.sqlite.org/fileformat2.html (own implementation, no copied code).
use crate::error::{LimitKind, LrcatError, Result, be16, be32, check_cancel};
use crate::source::{ByteSource, Limits};
use hsk_studio_accord::CancellationToken;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet};

const MAGIC: &[u8; 16] = b"SQLite format 3\0";
const HEADER_BYTES: usize = 100;
pub(crate) const MAX_SCHEMA_SQL: usize = 64 * 1024;
/// Accounted bookkeeping bytes per retained collection entry (hash/vector slack).
pub(crate) const ENTRY_BYTES: u64 = 24;

/// Accounted bytes of one retained entry of type `T` (its inline size plus slack); heap text the
/// entry owns is charged separately, by length.
pub(crate) const fn entry_bytes<T>() -> u64 {
    (std::mem::size_of::<T>() as u64).saturating_add(ENTRY_BYTES)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Null,
    Int(i64),
    Real(f64),
    Text(Vec<u8>),
    Blob(Vec<u8>),
}

impl Value {
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(v) => Some(*v),
            Self::Real(f) if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e18 => {
                Some(*f as i64)
            }
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Int(v) => Some(*v as f64),
            Self::Real(f) if f.is_finite() => Some(*f),
            _ => None,
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
    /// Raw bytes of a text or blob value.
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Text(b) | Self::Blob(b) => Some(b),
            _ => None,
        }
    }
    /// Strict text decode in the database encoding; `None` for non-text or invalid text.
    pub fn text(&self, encoding: TextEncoding) -> Option<String> {
        let Self::Text(bytes) = self else {
            return None;
        };
        decode_text(bytes, encoding)
    }
}

pub(crate) fn decode_text(bytes: &[u8], encoding: TextEncoding) -> Option<String> {
    match encoding {
        TextEncoding::Utf8 => String::from_utf8(bytes.to_vec()).ok(),
        TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
            if !bytes.len().is_multiple_of(2) {
                return None;
            }
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| {
                    if encoding == TextEncoding::Utf16Le {
                        u16::from_le_bytes([c[0], c[1]])
                    } else {
                        u16::from_be_bytes([c[0], c[1]])
                    }
                })
                .collect();
            String::from_utf16(&units).ok()
        }
    }
}

/// SQLite varint: 1-9 bytes, the ninth byte contributes all eight bits.
pub(crate) fn varint(buf: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for i in 0..8 {
        let b = *buf.get(i)?;
        value = (value << 7) | u64::from(b & 0x7f);
        if b & 0x80 == 0 {
            return Some((value, i.saturating_add(1)));
        }
    }
    let b = *buf.get(8)?;
    Some(((value << 8) | u64::from(b), 9))
}

fn serial_size(serial: u64) -> Result<u64> {
    Ok(match serial {
        0 | 8 | 9 => 0,
        1 => 1,
        2 => 2,
        3 => 3,
        4 => 4,
        5 => 6,
        6 | 7 => 8,
        10 | 11 => return Err(LrcatError::Corrupt("serial_type")),
        // 12 and above: even = blob of (n - 12) / 2 bytes, odd = text of (n - 13) / 2 bytes.
        n if n % 2 == 0 => n.saturating_sub(12) / 2,
        n => n.saturating_sub(13) / 2,
    })
}

fn read_value(serial: u64, bytes: &[u8]) -> Value {
    match serial {
        0 => Value::Null,
        8 => Value::Int(0),
        9 => Value::Int(1),
        1..=6 => {
            let mut v: i64 = if bytes[0] & 0x80 != 0 { -1 } else { 0 };
            for b in bytes {
                v = (v << 8) | i64::from(*b);
            }
            Value::Int(v)
        }
        7 => {
            let mut raw = [0u8; 8];
            raw.copy_from_slice(bytes);
            Value::Real(f64::from_bits(u64::from_be_bytes(raw)))
        }
        n if n % 2 == 0 => Value::Blob(bytes.to_vec()),
        _ => Value::Text(bytes.to_vec()),
    }
}

/// Decodes only the wanted record columns (`None` or a column past the record end is NULL).
pub(crate) fn decode_record(
    payload: &[u8],
    max_columns: u32,
    want: &[Option<usize>],
) -> Result<Vec<Value>> {
    let corrupt = LrcatError::Corrupt("record_header");
    let (header_size, n) = varint(payload).ok_or(corrupt.clone())?;
    let header_size = usize::try_from(header_size).map_err(|_| corrupt.clone())?;
    if header_size < n || header_size > payload.len() {
        return Err(corrupt);
    }
    let mut pos = n;
    let mut serials: Vec<u64> = Vec::new();
    while pos < header_size {
        let (serial, k) = varint(&payload[pos..header_size]).ok_or(LrcatError::Corrupt("record_header"))?;
        pos = pos.saturating_add(k);
        if serials.len() >= max_columns as usize {
            return Err(LrcatError::Limit(LimitKind::Columns));
        }
        serials.push(serial);
    }
    let mut offsets = Vec::with_capacity(serials.len());
    let mut sizes = Vec::with_capacity(serials.len());
    let mut offset = header_size as u64;
    for serial in &serials {
        let size = serial_size(*serial)?;
        offsets.push(offset);
        sizes.push(size);
        offset = offset
            .checked_add(size)
            .ok_or(LrcatError::Corrupt("record_truncated"))?;
    }
    if offset > payload.len() as u64 {
        return Err(LrcatError::Corrupt("record_truncated"));
    }
    let truncated = LrcatError::Corrupt("record_truncated");
    let mut out = Vec::with_capacity(want.len());
    for w in want {
        let Some((i, serial)) = w.and_then(|i| serials.get(i).map(|s| (i, *s))) else {
            out.push(Value::Null);
            continue;
        };
        let start = usize::try_from(offsets[i]).map_err(|_| truncated.clone())?;
        let len = usize::try_from(sizes[i]).map_err(|_| truncated.clone())?;
        let end = start.checked_add(len).ok_or(truncated.clone())?;
        let bytes = payload.get(start..end).ok_or(truncated.clone())?;
        out.push(read_value(serial, bytes));
    }
    Ok(out)
}

/// Local payload bytes of a table-leaf cell with `p` payload bytes and usable size `u`
/// (`u >= 480` is validated at open; the saturating forms keep the formulas total anyway).
fn local_size(p: u64, u: usize) -> usize {
    let x = u.saturating_sub(35) as u64;
    if p <= x {
        return usize::try_from(p).unwrap_or(usize::MAX);
    }
    let m = (u.saturating_sub(12).saturating_mul(32) / 255).saturating_sub(23) as u64;
    let span = (u as u64).saturating_sub(4);
    let k = m.saturating_add(p.saturating_sub(m).checked_rem(span).unwrap_or(0));
    usize::try_from(if k <= x { k } else { m }).unwrap_or(usize::MAX)
}

/// Validates the SQLite magic and returns the page size from the first 100 bytes of the main file.
pub(crate) fn peek_page_size(main: &dyn ByteSource, limits: &Limits) -> Result<u32> {
    let len = main.len();
    if len > limits.max_main_bytes {
        return Err(LrcatError::Limit(LimitKind::MainBytes));
    }
    if len < HEADER_BYTES as u64 {
        return Err(LrcatError::NotSqlite);
    }
    let mut head = [0u8; HEADER_BYTES];
    main.read_at(0, &mut head)?;
    if &head[..16] != MAGIC {
        return Err(if looks_random(&head) {
            LrcatError::Encrypted
        } else {
            LrcatError::NotSqlite
        });
    }
    let raw = be16(&head[16..]);
    let page_size = if raw == 1 { 65_536 } else { u32::from(raw) };
    if !(512..=65_536).contains(&page_size) || !page_size.is_power_of_two() {
        return Err(LrcatError::Corrupt("page_size"));
    }
    Ok(page_size)
}

/// HEURISTIC: a header with no magic and near-uniform byte values (many distinct values in 100
/// bytes) is most likely ciphertext. Text, zero-filled or structured files have far fewer.
fn looks_random(head: &[u8]) -> bool {
    let mut seen = [false; 256];
    for b in head {
        seen[usize::from(*b)] = true;
    }
    seen.iter().filter(|s| **s).count() >= 64
}

/// Granted WAL bytes, committed page -> data-offset overlay, database size after the last commit.
pub(crate) type WalView<'a> = (&'a dyn ByteSource, &'a BTreeMap<u32, u64>, Option<u32>);

pub(crate) struct Db<'a> {
    main: &'a dyn ByteSource,
    overlay: Option<(&'a dyn ByteSource, &'a BTreeMap<u32, u64>)>,
    limits: &'a Limits,
    cancel: &'a CancellationToken,
    page_size: u32,
    usable: u32,
    page_count: u32,
    pub encoding: TextEncoding,
    freelist_trunk: u32,
    freelist_count: u32,
    page_reads: Cell<u64>,
    cells: Cell<u64>,
    /// Overflow pages already consumed by ANY chain of this extraction. A page belongs to exactly
    /// one cell's chain in a valid file; a second claim (another row, another table, a loop) is a
    /// shared page and is refused, so no chain can be read once per row.
    overflow_claimed: RefCell<HashSet<u32>>,
    /// Extraction-wide accounted retained memory (see `charge`).
    retained: Cell<u64>,
}

/// One table-leaf cell: the local payload bytes are copied, the rest is fetched on demand.
pub(crate) struct TableCell {
    pub rowid: i64,
    payload_len: u64,
    local: Vec<u8>,
    overflow: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FreelistReport {
    pub declared: u32,
    pub counted: u64,
    pub anomaly: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TableDef {
    pub name: String,
    pub root: u32,
    /// Lowercase column names in declaration order (empty when `columns_known` is false).
    pub columns: Vec<String>,
    pub columns_known: bool,
    /// Index of the `INTEGER PRIMARY KEY` rowid alias column, stored as NULL in records.
    pub rowid_alias: Option<usize>,
    pub without_rowid: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Schema {
    pub tables: Vec<TableDef>,
    pub other_objects: u64,
}

impl<'a> Db<'a> {
    pub fn open(
        main: &'a dyn ByteSource,
        overlay: Option<WalView<'a>>,
        limits: &'a Limits,
        cancel: &'a CancellationToken,
    ) -> Result<Self> {
        let page_size = peek_page_size(main, limits)?;
        let mut db = Db {
            main,
            overlay: overlay.map(|(s, m, _)| (s, m)),
            limits,
            cancel,
            page_size,
            usable: page_size,
            page_count: u32::MAX,
            encoding: TextEncoding::Utf8,
            freelist_trunk: 0,
            freelist_count: 0,
            page_reads: Cell::new(0),
            cells: Cell::new(0),
            overflow_claimed: RefCell::new(HashSet::new()),
            retained: Cell::new(0),
        };
        let page1 = db.read_page(1)?;
        if &page1[..16] != MAGIC {
            return Err(LrcatError::Corrupt("header_magic"));
        }
        let raw = be16(&page1[16..]);
        let size = if raw == 1 { 65_536 } else { u32::from(raw) };
        if size != page_size {
            return Err(LrcatError::Corrupt("page_size_changed"));
        }
        let (write_version, read_version) = (page1[18], page1[19]);
        if write_version == 0 || read_version == 0 {
            return Err(LrcatError::Corrupt("file_format_version"));
        }
        if write_version > 2 || read_version > 2 {
            return Err(LrcatError::Unsupported("file_format_version"));
        }
        let usable = page_size
            .checked_sub(u32::from(page1[20]))
            .filter(|u| *u >= 480)
            .ok_or(LrcatError::Corrupt("usable_size"))?;
        if page1[21] != 64 || page1[22] != 32 || page1[23] != 32 {
            return Err(LrcatError::Corrupt("payload_fractions"));
        }
        db.encoding = match be32(&page1[56..]) {
            0 | 1 => TextEncoding::Utf8,
            2 => TextEncoding::Utf16Le,
            3 => TextEncoding::Utf16Be,
            _ => return Err(LrcatError::Corrupt("text_encoding")),
        };
        let wal_pages = overlay.and_then(|(_, _, pages)| pages);
        let (counter, in_header, valid_for) =
            (be32(&page1[24..]), be32(&page1[28..]), be32(&page1[92..]));
        let pages = if let Some(pages) = wal_pages {
            pages
        } else if in_header != 0 && counter == valid_for {
            in_header
        } else {
            u32::try_from(main.len().checked_div(u64::from(page_size)).unwrap_or(0))
                .map_err(|_| LrcatError::Limit(LimitKind::Pages))?
        };
        if pages == 0 {
            return Err(LrcatError::Corrupt("page_count"));
        }
        if pages > limits.max_pages {
            return Err(LrcatError::Limit(LimitKind::Pages));
        }
        if wal_pages.is_none()
            && u64::from(pages).saturating_mul(u64::from(page_size)) > main.len()
        {
            return Err(LrcatError::Corrupt("main_truncated"));
        }
        db.usable = usable;
        db.page_count = pages;
        db.freelist_trunk = be32(&page1[32..]);
        db.freelist_count = be32(&page1[36..]);
        Ok(db)
    }

    pub fn read_page(&self, no: u32) -> Result<Vec<u8>> {
        check_cancel(self.cancel)?;
        if no == 0 || no > self.page_count {
            return Err(LrcatError::Corrupt("page_number"));
        }
        let reads = self.page_reads.get().saturating_add(1);
        if reads > self.limits.max_page_reads {
            return Err(LrcatError::Limit(LimitKind::PageReads));
        }
        self.page_reads.set(reads);
        let mut buf = vec![0u8; self.page_size as usize];
        if let Some((wal, frames)) = self.overlay
            && let Some(offset) = frames.get(&no)
        {
            wal.read_at(*offset, &mut buf)?;
            return Ok(buf);
        }
        let offset = u64::from(no.saturating_sub(1)).saturating_mul(u64::from(self.page_size));
        if offset.saturating_add(u64::from(self.page_size)) > self.main.len() {
            return Err(LrcatError::Corrupt("page_missing"));
        }
        self.main.read_at(offset, &mut buf)?;
        Ok(buf)
    }

    /// Charges `bytes` of memory the extraction is about to retain (schema entries, lookup-table
    /// entries, proposal entries, every kept or cloned string) against
    /// `Limits::max_retained_text_bytes`. Call BEFORE the allocation it guards; the total is
    /// extraction-wide, so a value stored once but referenced by many rows is charged per copy.
    pub fn charge(&self, bytes: u64) -> Result<()> {
        let total = self.retained.get().saturating_add(bytes);
        if total > self.limits.max_retained_text_bytes {
            return Err(LrcatError::Limit(LimitKind::RetainedText));
        }
        self.retained.set(total);
        Ok(())
    }

    /// Accounted retained bytes charged so far (see `charge`).
    pub fn retained(&self) -> u64 {
        self.retained.get()
    }

    fn add_cells(&self, n: u64) -> Result<()> {
        let total = self.cells.get().saturating_add(n);
        if total > self.limits.max_cells {
            return Err(LrcatError::Limit(LimitKind::Cells));
        }
        self.cells.set(total);
        Ok(())
    }

    /// Iterative table b-tree walk (interior 0x05 / leaf 0x0d) in rowid order with a depth cap and
    /// a visited-page set. Returns the number of leaf rows.
    pub fn walk_table(
        &self,
        root: u32,
        visit: &mut dyn FnMut(TableCell) -> Result<()>,
    ) -> Result<u64> {
        let usable = self.usable as usize;
        let mut stack = vec![(root, 1u32)];
        let mut seen: HashSet<u32> = HashSet::new();
        let mut rows = 0u64;
        while let Some((no, depth)) = stack.pop() {
            if depth > self.limits.max_btree_depth {
                return Err(LrcatError::Limit(LimitKind::Depth));
            }
            if !seen.insert(no) {
                return Err(LrcatError::Cycle("btree_cycle"));
            }
            let page = self.read_page(no)?;
            // Page 1 carries the 100-byte database header before its b-tree header.
            let head = &page[if no == 1 { HEADER_BYTES } else { 0 }..];
            let array_base = if no == 1 { HEADER_BYTES } else { 0 };
            let (header_len, interior) = match head[0] {
                0x0d => (8usize, false),
                0x05 => (12usize, true),
                _ => return Err(LrcatError::Corrupt("btree_page_type")),
            };
            let ncells = usize::from(be16(&head[3..]));
            let array_start = array_base.saturating_add(header_len);
            let array_end = ncells.saturating_mul(2).saturating_add(array_start);
            if array_end > usable {
                return Err(LrcatError::Corrupt("cell_pointer_array"));
            }
            self.add_cells(ncells as u64)?;
            // `array_end <= usable <= page.len()` was checked, so every pointer read is in range.
            let pointer = |i: usize| {
                let at = i.saturating_mul(2).saturating_add(array_start);
                usize::from(be16(&page[at..]))
            };
            if interior {
                let mut children = Vec::with_capacity(ncells.saturating_add(1));
                for i in 0..ncells {
                    let p = pointer(i);
                    if p < array_end || p.saturating_add(5) > usable {
                        return Err(LrcatError::Corrupt("cell_bounds"));
                    }
                    children.push(be32(&page[p..]));
                }
                children.push(be32(&head[8..]));
                for child in children.into_iter().rev() {
                    if child == 0 || child > self.page_count {
                        return Err(LrcatError::Corrupt("child_page"));
                    }
                    stack.push((child, depth.saturating_add(1)));
                }
                continue;
            }
            let bounds = LrcatError::Corrupt("cell_bounds");
            for i in 0..ncells {
                let p = pointer(i);
                if p < array_end || p >= usable {
                    return Err(bounds);
                }
                let (payload_len, n1) = page
                    .get(p..usable)
                    .and_then(varint)
                    .ok_or(bounds.clone())?;
                let (rowid, n2) = page
                    .get(p.saturating_add(n1)..usable)
                    .and_then(varint)
                    .ok_or(bounds.clone())?;
                if payload_len > self.limits.max_payload {
                    return Err(LrcatError::Limit(LimitKind::Payload));
                }
                let local = local_size(payload_len, usable);
                let overrun = LrcatError::Corrupt("cell_overrun");
                let start = p.saturating_add(n1).saturating_add(n2);
                let end = start.checked_add(local).ok_or(overrun.clone())?;
                let spills = payload_len > usable.saturating_sub(35) as u64;
                if end.saturating_add(if spills { 4 } else { 0 }) > usable {
                    return Err(overrun);
                }
                let overflow = if spills { be32(&page[end..]) } else { 0 };
                if spills && (overflow == 0 || overflow > self.page_count) {
                    return Err(LrcatError::Corrupt("overflow_pointer"));
                }
                rows = rows.saturating_add(1);
                if rows > self.limits.max_rows {
                    return Err(LrcatError::Limit(LimitKind::Rows));
                }
                visit(TableCell {
                    rowid: rowid as i64,
                    payload_len,
                    local: page.get(start..end).ok_or(overrun)?.to_vec(),
                    overflow,
                })?;
            }
        }
        Ok(rows)
    }

    /// Assembles the full record payload, following the overflow chain with a page cap, cycle
    /// detection and an extraction-wide claim on every overflow page: a page already consumed by
    /// this chain is `overflow_cycle`, by another cell's chain `overflow_shared`. The declared
    /// size was already checked against `max_payload`.
    pub fn payload(&self, cell: &TableCell) -> Result<Vec<u8>> {
        let total = usize::try_from(cell.payload_len)
            .map_err(|_| LrcatError::Limit(LimitKind::Payload))?;
        if total <= cell.local.len() {
            return Ok(cell.local.clone());
        }
        if cell.overflow == 0 {
            return Err(LrcatError::Corrupt("overflow_missing"));
        }
        // Every size is checked BEFORE the buffer is allocated: the chain this payload needs must
        // fit the configured cap, and the up-front reservation never exceeds what the database
        // itself could supply (the loop below grows only with bytes really read).
        let per_page = (self.usable as usize).saturating_sub(4).max(1);
        let needed = total.saturating_sub(cell.local.len()).div_ceil(per_page);
        if needed > self.limits.max_overflow_pages as usize {
            return Err(LrcatError::Limit(LimitKind::OverflowPages));
        }
        let supply = (self.page_count as usize)
            .saturating_mul(per_page)
            .saturating_add(cell.local.len());
        let mut out = Vec::with_capacity(total.min(supply));
        out.extend_from_slice(&cell.local);
        let mut chain: Vec<u32> = Vec::new();
        let mut next = cell.overflow;
        while out.len() < total {
            if next == 0 {
                return Err(LrcatError::Corrupt("overflow_chain_short"));
            }
            if !self.overflow_claimed.borrow_mut().insert(next) {
                return Err(LrcatError::Cycle(if chain.contains(&next) {
                    "overflow_cycle"
                } else {
                    "overflow_shared"
                }));
            }
            chain.push(next);
            let page = self.read_page(next)?;
            next = be32(&page[0..]);
            let take = per_page.min(total.saturating_sub(out.len()));
            let chunk = page
                .get(4..take.saturating_add(4))
                .ok_or(LrcatError::Corrupt("overflow_page"))?;
            out.extend_from_slice(chunk);
        }
        if next != 0 {
            return Err(LrcatError::Corrupt("overflow_chain_long"));
        }
        Ok(out)
    }

    /// Reads `sqlite_master` (root page 1) and parses `CREATE TABLE` column names.
    pub fn read_schema(&self) -> Result<Schema> {
        let want = [Some(0), Some(1), Some(3), Some(4)];
        let mut schema = Schema::default();
        let enc = self.encoding;
        self.walk_table(1, &mut |cell| {
            let payload = self.payload(&cell)?;
            let v = decode_record(&payload, self.limits.max_columns, &want)?;
            let name = v[1].text(enc);
            if v[0].text(enc).as_deref() != Some("table") || name.is_none() {
                schema.other_objects = schema.other_objects.saturating_add(1);
                return Ok(());
            }
            let root = v[2]
                .as_i64()
                .and_then(|r| u32::try_from(r).ok())
                .unwrap_or(0);
            let parsed = v[3]
                .text(enc)
                .filter(|s| s.len() <= MAX_SCHEMA_SQL)
                .and_then(|s| parse_create_table(&s, self.limits.max_columns).ok());
            let def = match parsed {
                Some(p) => TableDef {
                    name: name.unwrap_or_default(),
                    root,
                    columns: p.columns,
                    columns_known: true,
                    rowid_alias: p.rowid_alias,
                    without_rowid: p.without_rowid,
                },
                None => TableDef {
                    name: name.unwrap_or_default(),
                    root,
                    columns: Vec::new(),
                    columns_known: false,
                    rowid_alias: None,
                    without_rowid: false,
                },
            };
            // Names are only bounded by the record payload: charge every retained schema entry.
            let columns = def
                .columns
                .iter()
                .fold(0u64, |a, c| a.saturating_add(ENTRY_BYTES).saturating_add(c.len() as u64));
            self.charge(
                entry_bytes::<TableDef>()
                    .saturating_add(def.name.len() as u64)
                    .saturating_add(columns),
            )?;
            schema.tables.push(def);
            Ok(())
        })?;
        Ok(schema)
    }

    /// Walks the freelist trunk chain (never used for reading). Structural anomalies are a typed
    /// report, not an error; budgets and cancellation still propagate.
    pub fn freelist_report(&self) -> Result<FreelistReport> {
        let mut report = FreelistReport {
            declared: self.freelist_count,
            ..FreelistReport::default()
        };
        let mut trunk = self.freelist_trunk;
        if trunk == 0 && self.freelist_count == 0 {
            return Ok(report);
        }
        let mut seen: HashSet<u32> = HashSet::new();
        let per_trunk_max = (self.usable as usize / 4).saturating_sub(2);
        while trunk != 0 {
            if trunk > self.page_count || !seen.insert(trunk) {
                report.anomaly = Some("freelist_trunk_invalid");
                return Ok(report);
            }
            let page = match self.read_page(trunk) {
                Ok(p) => p,
                Err(LrcatError::Corrupt(_)) => {
                    report.anomaly = Some("freelist_trunk_unreadable");
                    return Ok(report);
                }
                Err(e) => return Err(e),
            };
            let leaves = be32(&page[4..]) as usize;
            if leaves > per_trunk_max {
                report.anomaly = Some("freelist_leaf_count");
                return Ok(report);
            }
            report.counted = report.counted.saturating_add(1);
            for i in 0..leaves {
                // `leaves <= per_trunk_max`, so the 4-byte entry lies inside the page.
                let at = i.saturating_mul(4).saturating_add(8);
                let leaf = be32(&page[at..]);
                if leaf == 0 || leaf > self.page_count || !seen.insert(leaf) {
                    report.anomaly = Some("freelist_leaf_invalid");
                    return Ok(report);
                }
                report.counted = report.counted.saturating_add(1);
            }
            trunk = be32(&page[0..]);
        }
        if report.counted != u64::from(self.freelist_count) {
            report.anomaly = Some("freelist_count_mismatch");
        }
        Ok(report)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ParsedTable {
    pub columns: Vec<String>,
    pub rowid_alias: Option<usize>,
    pub without_rowid: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Tok {
    Word(String),
    Quoted(String),
    Str,
    Punct(char),
}

/// Cursor step over untrusted text: saturating, and every use is guarded by a `get`/length test.
fn adv(i: usize, n: usize) -> usize {
    i.saturating_add(n)
}

fn tokenize(sql: &str) -> Option<Vec<Tok>> {
    let c: Vec<char> = sql.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while let Some(&ch) = c.get(i) {
        if ch.is_whitespace() {
            i = adv(i, 1);
        } else if ch == '-' && c.get(adv(i, 1)) == Some(&'-') {
            while c.get(i).is_some_and(|x| *x != '\n') {
                i = adv(i, 1);
            }
        } else if ch == '/' && c.get(adv(i, 1)) == Some(&'*') {
            i = adv(i, 2);
            while !(c.get(i) == Some(&'*') && c.get(adv(i, 1)) == Some(&'/')) {
                if i >= c.len() {
                    return None;
                }
                i = adv(i, 1);
            }
            i = adv(i, 2);
        } else if ch == '"' || ch == '`' || ch == '\'' {
            let mut text = String::new();
            i = adv(i, 1);
            loop {
                let x = *c.get(i)?;
                if x == ch {
                    if c.get(adv(i, 1)) == Some(&ch) {
                        text.push(ch);
                        i = adv(i, 2);
                        continue;
                    }
                    i = adv(i, 1);
                    break;
                }
                text.push(x);
                i = adv(i, 1);
            }
            toks.push(if ch == '\'' {
                Tok::Str
            } else {
                Tok::Quoted(text.to_lowercase())
            });
        } else if ch == '[' {
            let mut text = String::new();
            i = adv(i, 1);
            loop {
                let x = *c.get(i)?;
                if x == ']' {
                    break;
                }
                text.push(x);
                i = adv(i, 1);
            }
            i = adv(i, 1);
            toks.push(Tok::Quoted(text.to_lowercase()));
        } else if ch.is_alphanumeric() || ch == '_' || ch == '$' {
            let mut text = String::new();
            while let Some(&x) = c.get(i) {
                if !(x.is_alphanumeric() || x == '_' || x == '$') {
                    break;
                }
                text.push(x);
                i = adv(i, 1);
            }
            toks.push(Tok::Word(text.to_lowercase()));
        } else {
            toks.push(Tok::Punct(ch));
            i = adv(i, 1);
        }
    }
    Some(toks)
}

fn is_word(t: &Tok, w: &str) -> bool {
    matches!(t, Tok::Word(x) if x == w)
}

/// Tiny bounded `CREATE TABLE` reader: column names, the `INTEGER PRIMARY KEY` rowid alias and
/// `WITHOUT ROWID`. Everything else (types, defaults, constraints) is skipped by depth counting.
pub(crate) fn parse_create_table(
    sql: &str,
    max_columns: u32,
) -> std::result::Result<ParsedTable, &'static str> {
    let toks = tokenize(sql).ok_or("schema_tokenize")?;
    let mut i = 0;
    if !toks.get(i).is_some_and(|t| is_word(t, "create")) {
        return Err("schema_not_create");
    }
    i = adv(i, 1);
    if toks
        .get(i)
        .is_some_and(|t| is_word(t, "temp") || is_word(t, "temporary"))
    {
        i = adv(i, 1);
    }
    if !toks.get(i).is_some_and(|t| is_word(t, "table")) {
        return Err("schema_not_table");
    }
    while i < toks.len() && toks[i] != Tok::Punct('(') {
        if is_word(&toks[i], "as") {
            return Err("schema_create_as");
        }
        i = adv(i, 1);
    }
    i = adv(i, 1); // past '('
    let mut columns: Vec<String> = Vec::new();
    let mut rowid_alias = None;
    loop {
        let start = i;
        let mut depth = 0u32;
        while i < toks.len() {
            match &toks[i] {
                Tok::Punct('(') => depth = depth.saturating_add(1),
                Tok::Punct(')') if depth == 0 => break,
                Tok::Punct(')') => depth = depth.saturating_sub(1),
                Tok::Punct(',') if depth == 0 => break,
                _ => {}
            }
            i = adv(i, 1);
        }
        if i >= toks.len() {
            return Err("schema_unbalanced");
        }
        let def = &toks[start..i];
        let is_constraint = matches!(
            def.first(),
            Some(Tok::Word(w)) if matches!(w.as_str(), "constraint" | "primary" | "unique" | "check" | "foreign")
        );
        if !is_constraint {
            let name = match def.first() {
                Some(Tok::Word(w)) | Some(Tok::Quoted(w)) => w.clone(),
                _ => return Err("schema_column_name"),
            };
            if columns.len() >= max_columns as usize {
                return Err("schema_columns");
            }
            let primary_key = def
                .windows(2)
                .any(|w| is_word(&w[0], "primary") && is_word(&w[1], "key"));
            if primary_key && def.get(1).is_some_and(|t| is_word(t, "integer")) {
                rowid_alias = Some(columns.len());
            }
            columns.push(name);
        }
        let closing = toks[i] == Tok::Punct(')');
        i = adv(i, 1);
        if closing {
            break;
        }
    }
    let without_rowid = toks[i..]
        .windows(2)
        .any(|w| is_word(&w[0], "without") && is_word(&w[1], "rowid"));
    Ok(ParsedTable {
        columns,
        rowid_alias: if without_rowid { None } else { rowid_alias },
        without_rowid,
    })
}
