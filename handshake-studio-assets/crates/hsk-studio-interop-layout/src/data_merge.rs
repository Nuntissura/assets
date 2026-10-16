//! Data merge (lane SMB-ID-PREPRESS; STU-LAY-063, STU-LAY-163). A data source (delimited text or a
//! JSON array of objects) is bound to placeholders in story text and merged per record.
//!
//! - Sources: [`parse_delimited`] (RFC 4180 quoting: `"` quotes, `""` escapes, delimiters and line
//!   breaks inside quotes; CRLF / LF / CR row ends; a UTF-8 BOM is skipped) and [`parse_json_records`]
//!   (an array of objects; string, number, boolean and null values). The first row names the fields.
//!   A field named `@name` is an image field (path), `#name` a QR-code field (the three captured
//!   placeholder kinds stay distinct; the `@`/`#` header convention is InDesign's, UNVERIFIED D5).
//!   Bytes that are not UTF-8 or hold NUL are `data_file_not_text`; no header or no rows is
//!   `data_source_empty`; rows, fields and bytes are bounded before allocation grows past them.
//! - Placeholders are `<<field>>` in story text ([`find_placeholders`]); a placeholder naming no
//!   source field is `placeholder_field_missing`, a story with none is `missing_placeholders`.
//! - [`merge_story`] replaces every placeholder of one record back to front through layout's
//!   `replace_story_text` (paragraph and character partitions follow the edit); with
//!   `remove_blank_lines`, a paragraph left empty only because its fields were empty is removed.
//! - Record selection: all, one record, or a range string `1-3,7` ([`select_records`]); a number
//!   outside 1..=records or a malformed range is `invalid_record_range`.
//! - [`arrange_records`]: multiple records per page: the record frame repeated over the target
//!   page's margin box, rows first or columns first, with row/column spacing; the cell count per
//!   page and each record's page and position (spread coordinates of the page) are returned.
//!
//! Not implemented (gaps): image and QR placement into frames (fields are typed and returned),
//! content-fitting options, direct-to-PDF merge, spreadsheet sources. InDesign parity UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_folio::StudioTextStory;
use hsk_studio_layout::{LayoutError, replace_story_text};

/// Most records one source holds.
pub const MAX_MERGE_RECORDS: usize = 1_000_000;
/// Most fields per record.
pub const MAX_MERGE_FIELDS: usize = 1024;
/// Largest source, bytes.
pub const MAX_MERGE_SOURCE_BYTES: usize = 64 << 20;

/// Typed refusals (the captured data-merge errors plus bounds).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeError {
    DataFileNotText,
    DataSourceEmpty,
    UnsupportedFormat(&'static str),
    PlaceholderFieldMissing(String),
    MissingPlaceholders,
    InvalidRecordRange(String),
    /// A row has more cells than the header names.
    RowTooWide(usize),
    BudgetExceeded(&'static str),
    Layout(LayoutError),
}
impl MergeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::DataFileNotText => "data_file_not_text",
            Self::DataSourceEmpty => "data_source_empty",
            Self::UnsupportedFormat(_) => "data_source_unsupported_format",
            Self::PlaceholderFieldMissing(_) => "placeholder_field_missing",
            Self::MissingPlaceholders => "missing_placeholders",
            Self::InvalidRecordRange(_) => "invalid_record_range",
            Self::RowTooWide(_) => "data_row_too_wide",
            Self::BudgetExceeded(_) => "budget_exceeded",
            Self::Layout(e) => e.code(),
        }
    }
}
impl From<LayoutError> for MergeError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}

/// The three captured placeholder kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Image,
    QrCode,
}

/// A source field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeField {
    /// Name without the `@` / `#` marker.
    pub name: String,
    pub kind: FieldKind,
}

/// A parsed data source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataSource {
    pub fields: Vec<MergeField>,
    /// One value per field (missing trailing cells are empty).
    pub records: Vec<Vec<String>>,
}
impl DataSource {
    /// Index of a field by name.
    pub fn field(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.name == name)
    }
}

fn text_of(bytes: &[u8]) -> Result<&str, MergeError> {
    if bytes.len() > MAX_MERGE_SOURCE_BYTES {
        return Err(MergeError::BudgetExceeded("source_bytes"));
    }
    let s = std::str::from_utf8(bytes).map_err(|_| MergeError::DataFileNotText)?;
    if s.contains('\0') {
        return Err(MergeError::DataFileNotText);
    }
    Ok(s.strip_prefix('\u{FEFF}').unwrap_or(s))
}

fn field_of(raw: &str) -> MergeField {
    let raw = raw.trim();
    match raw.chars().next() {
        Some('@') => MergeField { name: raw[1..].to_owned(), kind: FieldKind::Image },
        Some('#') => MergeField { name: raw[1..].to_owned(), kind: FieldKind::QrCode },
        _ => MergeField { name: raw.to_owned(), kind: FieldKind::Text },
    }
}

fn source(header: Vec<String>, rows: Vec<Vec<String>>) -> Result<DataSource, MergeError> {
    if header.is_empty() || header.iter().all(|h| h.trim().is_empty()) || rows.is_empty() {
        return Err(MergeError::DataSourceEmpty);
    }
    let fields: Vec<MergeField> = header.iter().map(|h| field_of(h)).collect();
    let mut records = Vec::with_capacity(rows.len());
    for (i, mut r) in rows.into_iter().enumerate() {
        if r.len() > fields.len() {
            return Err(MergeError::RowTooWide(i.saturating_add(1)));
        }
        r.resize(fields.len(), String::new());
        records.push(r);
    }
    Ok(DataSource { fields, records })
}

/// Parses delimited text (module docs). `delimiter` is `,` or `\t` typically.
pub fn parse_delimited(bytes: &[u8], delimiter: char) -> Result<DataSource, MergeError> {
    if delimiter == '"' || delimiter == '\n' || delimiter == '\r' {
        return Err(MergeError::UnsupportedFormat("delimiter"));
    }
    let text = text_of(bytes)?;
    let mut rows: Vec<Vec<String>> = Vec::new();
    let (mut row, mut cell) = (Vec::new(), String::new());
    let (mut quoted, mut was_quoted) = (false, false);
    let mut chars = text.chars().peekable();
    let mut end_row = |row: &mut Vec<String>, cell: &mut String, rows: &mut Vec<Vec<String>>| -> Result<(), MergeError> {
        row.push(std::mem::take(cell));
        if row.len() > MAX_MERGE_FIELDS {
            return Err(MergeError::BudgetExceeded("fields"));
        }
        let r = std::mem::take(row);
        if !(r.len() == 1 && r[0].is_empty()) {
            if rows.len() > MAX_MERGE_RECORDS {
                return Err(MergeError::BudgetExceeded("records"));
            }
            rows.push(r);
        }
        Ok(())
    };
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    cell.push('"');
                } else {
                    quoted = false;
                }
            } else {
                cell.push(c);
            }
            continue;
        }
        match c {
            '"' if cell.is_empty() && !was_quoted => {
                quoted = true;
                was_quoted = true;
            }
            c if c == delimiter => {
                row.push(std::mem::take(&mut cell));
                was_quoted = false;
                if row.len() > MAX_MERGE_FIELDS {
                    return Err(MergeError::BudgetExceeded("fields"));
                }
            }
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                end_row(&mut row, &mut cell, &mut rows)?;
                was_quoted = false;
            }
            c => cell.push(c),
        }
    }
    if quoted {
        return Err(MergeError::UnsupportedFormat("unterminated_quote"));
    }
    if !cell.is_empty() || !row.is_empty() {
        end_row(&mut row, &mut cell, &mut rows)?;
    }
    if rows.is_empty() {
        return Err(MergeError::DataSourceEmpty);
    }
    let header = rows.remove(0);
    source(header, rows)
}

/// Parses a JSON array of objects; fields are the keys in order of first appearance.
pub fn parse_json_records(bytes: &[u8]) -> Result<DataSource, MergeError> {
    let text = text_of(bytes)?;
    let v: serde_json::Value = serde_json::from_str(text).map_err(|_| MergeError::UnsupportedFormat("json"))?;
    let arr = v.as_array().ok_or(MergeError::UnsupportedFormat("json_not_array"))?;
    if arr.len() > MAX_MERGE_RECORDS {
        return Err(MergeError::BudgetExceeded("records"));
    }
    let mut header: Vec<String> = Vec::new();
    for o in arr {
        let o = o.as_object().ok_or(MergeError::UnsupportedFormat("json_record_not_object"))?;
        for k in o.keys() {
            if !header.contains(k) {
                if header.len() >= MAX_MERGE_FIELDS {
                    return Err(MergeError::BudgetExceeded("fields"));
                }
                header.push(k.clone());
            }
        }
    }
    let rows = arr
        .iter()
        .filter_map(|o| o.as_object())
        .map(|o| {
            header
                .iter()
                .map(|k| match o.get(k) {
                    None | Some(serde_json::Value::Null) => String::new(),
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(serde_json::Value::Bool(b)) => b.to_string(),
                    Some(serde_json::Value::Number(n)) => n.to_string(),
                    Some(other) => other.to_string(),
                })
                .collect()
        })
        .collect();
    source(header, rows)
}

/// A placeholder in story text: bytes and field name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placeholder {
    pub start: u64,
    pub end: u64,
    pub field: String,
}

/// Every `<<field>>` placeholder of `text` (non-nested, name without `<`, `>` or line breaks).
pub fn find_placeholders(text: &str) -> Vec<Placeholder> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(i) = text.get(at..).and_then(|t| t.find("<<")) {
        let Some(open) = at.checked_add(i) else { break };
        let Some(body) = open.checked_add(2) else { break };
        let Some(rest) = text.get(body..) else { break };
        match rest.find(">>") {
            Some(j) if j > 0 && !rest[..j].contains(['<', '>', '\n', '\u{2028}']) => {
                let end = body.saturating_add(j).saturating_add(2);
                out.push(Placeholder { start: open as u64, end: end as u64, field: rest[..j].trim().to_owned() });
                at = end;
            }
            _ => at = body,
        }
    }
    out
}

/// Checks every placeholder against the source (typed errors of STU-LAY-063).
pub fn check_placeholders(text: &str, src: &DataSource) -> Result<Vec<Placeholder>, MergeError> {
    let ph = find_placeholders(text);
    if ph.is_empty() {
        return Err(MergeError::MissingPlaceholders);
    }
    if let Some(p) = ph.iter().find(|p| src.field(&p.field).is_none()) {
        return Err(MergeError::PlaceholderFieldMissing(p.field.clone()));
    }
    Ok(ph)
}

/// One merged record: the story and its text, and the image / QR values the record carries.
#[derive(Clone, Debug, PartialEq)]
pub struct MergedRecord {
    pub record: usize,
    pub story: StudioTextStory,
    pub text: String,
    /// (field, kind, value) of placeholders that are image or QR fields.
    pub graphic_fields: Vec<(String, FieldKind, String)>,
}

/// Merges record `index` (0-based) into `story` (inline content; `text` is its text).
pub fn merge_story(story: &StudioTextStory, text: &str, src: &DataSource, index: usize, remove_blank_lines: bool) -> Result<MergedRecord, MergeError> {
    let ph = check_placeholders(text, src)?;
    let rec = src.records.get(index).ok_or_else(|| MergeError::InvalidRecordRange(index.saturating_add(1).to_string()))?;
    let mut graphic_fields = Vec::new();
    // Paragraphs whose only content is placeholders that all merge empty (before any edit).
    let mut blank: Vec<(u64, u64)> = Vec::new();
    if remove_blank_lines {
        for p in &story.paragraphs {
            let inside: Vec<&Placeholder> = ph.iter().filter(|x| x.start >= p.start && x.end <= p.end).collect();
            if inside.is_empty() {
                continue;
            }
            let (a, b) = (usize::try_from(p.start).map_err(|_| LayoutError::InvalidInput)?, usize::try_from(p.end).map_err(|_| LayoutError::InvalidInput)?);
            let mut rest = text.get(a..b).ok_or(LayoutError::InvalidInput)?.to_owned();
            for x in inside.iter().rev() {
                let rel = |v: u64| v.checked_sub(p.start).and_then(|d| usize::try_from(d).ok()).ok_or(LayoutError::InvalidInput);
                let (s, e) = (rel(x.start)?, rel(x.end)?);
                rest.replace_range(s..e, "");
            }
            let all_empty = inside.iter().all(|x| src.field(&x.field).is_some_and(|f| rec[f].is_empty()));
            if all_empty && rest.trim().is_empty() {
                blank.push((p.start, p.end));
            }
        }
    }
    let mut cur = story.clone();
    let mut cur_text = text.to_owned();
    // Edits back to front: placeholders, and whole blank paragraphs.
    let mut edits: Vec<(u64, u64, String)> = Vec::new();
    for x in &ph {
        if blank.iter().any(|(a, b)| x.start >= *a && x.end <= *b) {
            continue;
        }
        let f = src.field(&x.field).ok_or_else(|| MergeError::PlaceholderFieldMissing(x.field.clone()))?;
        let kind = src.fields[f].kind;
        if kind != FieldKind::Text {
            graphic_fields.push((x.field.clone(), kind, rec[f].clone()));
        }
        edits.push((x.start, x.end, if kind == FieldKind::Text { rec[f].clone() } else { String::new() }));
    }
    for (a, b) in &blank {
        edits.push((*a, *b, String::new()));
    }
    edits.sort_by_key(|e| e.0);
    for (a, b, t) in edits.iter().rev() {
        cur = replace_story_text(&cur, None, *a, *b, t)?;
        let (s, e) = (usize::try_from(*a).map_err(|_| LayoutError::InvalidInput)?, usize::try_from(*b).map_err(|_| LayoutError::InvalidInput)?);
        cur_text.replace_range(s..e, t);
    }
    Ok(MergedRecord { record: index, story: cur, text: cur_text, graphic_fields })
}

/// Record selection (STU-LAY-163 `record_selection`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordSelection {
    All,
    /// 1-based.
    One(usize),
    /// `1-3,7` (1-based, inclusive).
    Range(String),
}

/// 0-based record indexes of a selection over `count` records, in order, without duplicates.
pub fn select_records(sel: &RecordSelection, count: usize) -> Result<Vec<usize>, MergeError> {
    let bad = |s: &str| MergeError::InvalidRecordRange(s.to_owned());
    let check = |n: usize, s: &str| if (1..=count).contains(&n) { Ok(n.saturating_sub(1)) } else { Err(bad(s)) };
    match sel {
        RecordSelection::All => Ok((0..count).collect()),
        RecordSelection::One(n) => Ok(vec![check(*n, &n.to_string())?]),
        RecordSelection::Range(r) => {
            let mut out: Vec<usize> = Vec::new();
            for part in r.split(',') {
                let part = part.trim();
                let (a, b) = match part.split_once('-') {
                    Some((a, b)) => (a.trim(), b.trim()),
                    None => (part, part),
                };
                let (a, b): (usize, usize) = (a.parse().map_err(|_| bad(part))?, b.parse().map_err(|_| bad(part))?);
                let (a, b) = (check(a, part)?, check(b, part)?);
                if a > b {
                    return Err(bad(part));
                }
                for i in a..=b {
                    if !out.contains(&i) {
                        out.push(i);
                    }
                }
            }
            if out.is_empty() {
                return Err(bad(r));
            }
            Ok(out)
        }
    }
}

/// Arrangement order of multiple records per page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrangeBy {
    RowsFirst,
    ColumnsFirst,
}

/// Where a record lands: output page (0-based) and the record frame's top-left on that page (pt).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecordCell {
    pub page: usize,
    pub x_pt: f64,
    pub y_pt: f64,
}

/// Multiple records per page: cells of `frame_w x frame_h` in the margin box `[left, top, right,
/// bottom]` of a `page_w x page_h` page with spacing; `records` cells are placed in order.
#[allow(clippy::too_many_arguments)]
pub fn arrange_records(
    records: usize,
    page_w: f64,
    page_h: f64,
    margins: [f64; 4],
    frame_w: f64,
    frame_h: f64,
    spacing: [f64; 2],
    by: ArrangeBy,
) -> Result<(usize, Vec<RecordCell>), MergeError> {
    let [l, t, r, b] = margins;
    let [col_gap, row_gap] = spacing;
    let (w, h) = (page_w - l - r, page_h - t - b);
    let ok = [page_w, page_h, l, t, r, b, frame_w, frame_h, col_gap, row_gap].iter().all(|v| v.is_finite() && *v >= 0.0);
    if !ok || frame_w <= 0.0 || frame_h <= 0.0 || w < frame_w || h < frame_h {
        return Err(LayoutError::InvalidGeometry.into());
    }
    let cols = ((w + col_gap) / (frame_w + col_gap)).floor().min(1e6) as usize;
    let rows = ((h + row_gap) / (frame_h + row_gap)).floor().min(1e6) as usize;
    let per_page = cols.checked_mul(rows).filter(|n| *n > 0).ok_or(MergeError::BudgetExceeded("cells"))?;
    if records > MAX_MERGE_RECORDS {
        return Err(MergeError::BudgetExceeded("records"));
    }
    let cells = (0..records)
        .map(|i| {
            let div = |a: usize, b: usize| (a.checked_div(b).unwrap_or(0), a.checked_rem(b).unwrap_or(0));
            let (page, k) = div(i, per_page);
            let (row, col) = match by {
                ArrangeBy::RowsFirst => div(k, cols),
                ArrangeBy::ColumnsFirst => {
                    let (c, r) = div(k, rows);
                    (r, c)
                }
            };
            RecordCell { page, x_pt: l + col as f64 * (frame_w + col_gap), y_pt: t + row as f64 * (frame_h + row_gap) }
        })
        .collect();
    Ok((per_page, cells))
}
