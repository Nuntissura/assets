//! Kiwi (evanw/kiwi) binary schema reader and schema-driven, bounded message walker used for the
//! .fig census. Wire facts (verified against evanw/kiwi js/binary.ts and js/bb.ts, 2026-10-11):
//! schema = varuint definition count, then per definition {NUL-terminated name, kind byte
//! (0 ENUM, 1 STRUCT, 2 MESSAGE), varuint field count, per field {name, zigzag varint type, flag byte
//! (bit 0 = array), varuint value}}; a negative type `t` is native index `!t` into
//! [bool, byte, int, uint, float, string, int64, uint64], otherwise a definition index. Message =
//! repeated (varuint field value, payload) terminated by 0; struct = fields in order; enum = varuint.
//!
//! Nothing is guessed: an unknown field id, a truncated value or a bound hit is a typed error, and a
//! census is produced only when the entire message decodes (test: unsupported_version_duplicate_key_bomb_cancel
//! asserts no census for an unknown field id and a truncated message).
#![deny(clippy::arithmetic_side_effects)]
use crate::container::{CANVAS_PART, FigContainer, Limits};
use crate::import::{
    Census, DefinitionSummary, FieldSummary, LossEntry, LossKind, SchemaSummary, SourceId,
};
use crate::{FigError, Result};
use hsk_studio_accord::CancellationToken;
use hsk_studio_package::{InflateFail, inflate_exact};
use std::collections::BTreeMap;

pub(crate) type R<T> = std::result::Result<T, &'static str>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Enum,
    Struct,
    Message,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Native {
    Bool,
    Byte,
    Int,
    Uint,
    Float,
    String,
    Int64,
    Uint64,
}
const NATIVES: [(Native, &str); 8] = [
    (Native::Bool, "bool"),
    (Native::Byte, "byte"),
    (Native::Int, "int"),
    (Native::Uint, "uint"),
    (Native::Float, "float"),
    (Native::String, "string"),
    (Native::Int64, "int64"),
    (Native::Uint64, "uint64"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ty {
    Native(Native),
    Def(usize),
}

#[derive(Debug)]
pub(crate) struct Field {
    pub(crate) name: std::sync::Arc<str>,
    pub(crate) ty: Ty,
    pub(crate) is_array: bool,
    pub(crate) value: u32,
}

#[derive(Debug)]
pub(crate) struct Def {
    pub(crate) name: String,
    pub(crate) kind: Kind,
    pub(crate) fields: Vec<Field>,
}

#[derive(Debug)]
pub(crate) struct Schema {
    pub(crate) defs: Vec<Def>,
}

pub(crate) struct Reader<'a> {
    pub(crate) data: &'a [u8],
    pub(crate) at: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }
    pub(crate) fn byte(&mut self) -> R<u8> {
        let b = *self.data.get(self.at).ok_or("truncated")?;
        self.at = self.at.checked_add(1).ok_or("truncated")?;
        Ok(b)
    }
    pub(crate) fn skip(&mut self, n: usize) -> R<()> {
        let end = self.at.checked_add(n).filter(|e| *e <= self.data.len());
        self.at = end.ok_or("truncated")?;
        Ok(())
    }
    /// 32-bit varuint: little-endian 7-bit groups, at most 5 bytes.
    pub(crate) fn var_uint(&mut self) -> R<u32> {
        let (mut value, mut shift) = (0u32, 0u32);
        loop {
            let b = self.byte()?;
            value |= u32::from(b & 127) << shift;
            if b & 128 == 0 {
                return Ok(value);
            }
            shift = shift.saturating_add(7);
            if shift >= 35 {
                return Err("varint_too_long");
            }
        }
    }
    pub(crate) fn var_int(&mut self) -> R<i32> {
        let u = self.var_uint()?;
        Ok(((u >> 1) as i32) ^ ((u & 1) as i32).wrapping_neg())
    }
    /// 64-bit varuint/varint is only skipped: 7-bit groups for 8 bytes, a full 9th byte.
    fn skip_var64(&mut self) -> R<()> {
        for _ in 0..8 {
            if self.byte()? & 128 == 0 {
                return Ok(());
            }
        }
        self.byte().map(|_| ())
    }
    /// varfloat: a lone zero byte is 0.0, otherwise 4 bytes.
    fn skip_var_float(&mut self) -> R<()> {
        if self.byte()? == 0 {
            Ok(())
        } else {
            self.skip(3)
        }
    }
    /// NUL-terminated string, not validated (message payload strings are only skipped).
    fn skip_string(&mut self) -> R<()> {
        let rest = &self.data[self.at..];
        let n = rest.iter().position(|b| *b == 0).ok_or("truncated")?;
        self.at = n
            .checked_add(1)
            .and_then(|k| self.at.checked_add(k))
            .ok_or("truncated")?;
        Ok(())
    }
    /// NUL-terminated UTF-8 schema name, at most 1 KiB.
    fn name(&mut self) -> R<String> {
        let rest = &self.data[self.at..];
        let n = rest.iter().position(|b| *b == 0).ok_or("truncated")?;
        if n > 1024 {
            return Err("name_too_long");
        }
        let s = std::str::from_utf8(&rest[..n])
            .map_err(|_| "name_encoding")?
            .to_owned();
        self.at = n
            .checked_add(1)
            .and_then(|k| self.at.checked_add(k))
            .ok_or("truncated")?;
        Ok(s)
    }
}

/// Parses a Kiwi binary schema. Bytes after the last definition are ignored (as the reference does).
pub(crate) fn decode_schema(bytes: &[u8], limits: &Limits) -> Result<Schema> {
    let mut r = Reader::new(bytes);
    let bad = FigError::SchemaInvalid;
    let count = r.var_uint().map_err(bad)? as usize;
    if count > limits.max_definitions {
        return Err(FigError::BudgetExceeded("definitions"));
    }
    // (name, kind, [(name, raw type, is_array, value)])
    type Raw = (String, Kind, Vec<(String, i32, bool, u32)>);
    let mut raw: Vec<Raw> = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let name = r.name().map_err(bad)?;
        let kind = match r.byte().map_err(bad)? {
            0 => Kind::Enum,
            1 => Kind::Struct,
            2 => Kind::Message,
            _ => return Err(bad("kind_invalid")),
        };
        let fields = r.var_uint().map_err(bad)? as usize;
        if fields > limits.max_fields {
            return Err(FigError::BudgetExceeded("fields"));
        }
        let mut list = Vec::with_capacity(fields.min(256));
        for _ in 0..fields {
            let fname = r.name().map_err(bad)?;
            let ty = r.var_int().map_err(bad)?;
            let is_array = r.byte().map_err(bad)? & 1 != 0;
            let value = r.var_uint().map_err(bad)?;
            list.push((fname, ty, is_array, value));
        }
        raw.push((name, kind, list));
    }
    let mut defs: Vec<Def> = Vec::with_capacity(raw.len());
    for (name, kind, fields) in &raw {
        if defs.iter().any(|d: &Def| d.name == *name) {
            return Err(bad("duplicate_definition"));
        }
        let mut out = Vec::with_capacity(fields.len());
        for (fname, ty, is_array, value) in fields {
            let ty = if *kind == Kind::Enum {
                Ty::Native(Native::Uint)
            } else if *ty < 0 {
                let idx = usize::try_from(!*ty).map_err(|_| bad("type_out_of_range"))?;
                Ty::Native(NATIVES.get(idx).ok_or(bad("type_out_of_range"))?.0)
            } else {
                let idx = *ty as usize;
                if idx >= raw.len() {
                    return Err(bad("type_out_of_range"));
                }
                Ty::Def(idx)
            };
            if *kind == Kind::Message && *value == 0 {
                return Err(bad("field_id_zero"));
            }
            if out.iter().any(|f: &Field| f.value == *value) {
                return Err(bad("duplicate_field_id"));
            }
            out.push(Field {
                name: std::sync::Arc::from(fname.as_str()),
                ty,
                is_array: *is_array,
                value: *value,
            });
        }
        defs.push(Def {
            name: name.clone(),
            kind: *kind,
            fields: out,
        });
    }
    Ok(Schema { defs })
}

impl Schema {
    pub(crate) fn summary(&self, chunk_index: usize, chunk_sha256: [u8; 32]) -> SchemaSummary {
        let type_name = |ty: Ty| match ty {
            Ty::Native(n) => NATIVES
                .iter()
                .find(|(k, _)| *k == n)
                .map_or("", |(_, s)| s)
                .to_owned(),
            Ty::Def(i) => self.defs[i].name.clone(),
        };
        SchemaSummary {
            definitions: self
                .defs
                .iter()
                .map(|d| DefinitionSummary {
                    name: d.name.clone(),
                    kind: match d.kind {
                        Kind::Enum => "enum",
                        Kind::Struct => "struct",
                        Kind::Message => "message",
                    },
                    fields: d
                        .fields
                        .iter()
                        .map(|f| FieldSummary {
                            name: f.name.to_string(),
                            id: f.value,
                            type_name: if d.kind == Kind::Enum {
                                String::new()
                            } else {
                                type_name(f.ty)
                            },
                            is_array: f.is_array,
                        })
                        .collect(),
                })
                .collect(),
            chunk_index,
            chunk_sha256,
        }
    }

    pub(crate) fn def(&self, name: &str, kind: Kind) -> Option<(usize, &Def)> {
        self.defs
            .iter()
            .enumerate()
            .find(|(_, d)| d.name == name && d.kind == kind)
    }
}

/// Work counter: bounds zero-width element loops and polls cancellation.
struct Work<'a> {
    values: u64,
    max: u64,
    cancel: &'a CancellationToken,
}

impl Work<'_> {
    fn tick(&mut self) -> Result<()> {
        self.values = self.values.saturating_add(1);
        if self.values > self.max {
            return Err(FigError::BudgetExceeded("values"));
        }
        if self.values.is_multiple_of(4096) {
            self.cancel.check().map_err(|_| FigError::Canceled)?;
        }
        Ok(())
    }
}

struct Walker<'a> {
    schema: &'a Schema,
    r: Reader<'a>,
    work: Work<'a>,
    max_depth: usize,
}

impl Walker<'_> {
    fn bad<T>(what: &'static str) -> Result<T> {
        Err(FigError::MessageInvalid(what))
    }

    fn skip_native(&mut self, n: Native) -> R<()> {
        match n {
            Native::Bool | Native::Byte => self.r.skip(1),
            Native::Int | Native::Uint => self.r.var_uint().map(|_| ()),
            Native::Float => self.r.skip_var_float(),
            Native::String => self.r.skip_string(),
            Native::Int64 | Native::Uint64 => self.r.skip_var64(),
        }
    }

    fn skip_value(&mut self, ty: Ty, depth: usize) -> Result<()> {
        if depth > self.max_depth {
            return Self::bad("depth");
        }
        self.work.tick()?;
        match ty {
            Ty::Native(n) => self.skip_native(n).map_err(FigError::MessageInvalid),
            Ty::Def(i) => {
                let d = &self.schema.defs[i];
                match d.kind {
                    Kind::Enum => self
                        .r
                        .var_uint()
                        .map(|_| ())
                        .map_err(FigError::MessageInvalid),
                    Kind::Struct => {
                        for f in &d.fields {
                            self.skip_field(f, depth.saturating_add(1))?;
                        }
                        Ok(())
                    }
                    Kind::Message => {
                        while let Some(f) = self.next_field(d)? {
                            self.skip_field(f, depth.saturating_add(1))?;
                        }
                        Ok(())
                    }
                }
            }
        }
    }

    fn skip_field(&mut self, f: &Field, depth: usize) -> Result<()> {
        if !f.is_array {
            return self.skip_value(f.ty, depth);
        }
        let count = self.r.var_uint().map_err(FigError::MessageInvalid)? as usize;
        if f.ty == Ty::Native(Native::Byte) {
            return self.r.skip(count).map_err(FigError::MessageInvalid);
        }
        for _ in 0..count {
            self.skip_value(f.ty, depth)?;
        }
        Ok(())
    }

    /// Next field of a message, `None` at the terminating 0; an unknown id cannot be skipped.
    fn next_field<'d>(&mut self, d: &'d Def) -> Result<Option<&'d Field>> {
        let id = self.r.var_uint().map_err(FigError::MessageInvalid)?;
        if id == 0 {
            return Ok(None);
        }
        match d.fields.iter().find(|f| f.value == id) {
            Some(f) => Ok(Some(f)),
            None => Self::bad("unknown_field_id"),
        }
    }
}

pub(crate) fn find_field<'d>(d: &'d Def, name: &str) -> Option<&'d Field> {
    d.fields.iter().find(|f| &*f.name == name)
}

/// Walks the whole message once and counts `nodeChanges` elements by their `type` enum name; ids are
/// the `guid` struct's `sessionID`/`localID` verbatim. Root ids are the nodes whose type name is
/// `DOCUMENT`. Field names are verified against three real Figma files and the installed editor schema: when
/// the schema lacks them the result is a typed `MessageInvalid`.
pub(crate) fn census(
    schema: &Schema,
    message: &[u8],
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<Census> {
    let Some((_, root)) = schema.def("Message", Kind::Message) else {
        return Walker::bad("root_message_missing");
    };
    let Some(changes) = find_field(root, "nodeChanges") else {
        return Walker::bad("node_changes_field_missing");
    };
    let Ty::Def(node_idx) = changes.ty else {
        return Walker::bad("node_changes_type");
    };
    let node = &schema.defs[node_idx];
    if !changes.is_array || node.kind != Kind::Message {
        return Walker::bad("node_changes_type");
    }
    let (Some(guid_f), Some(type_f)) = (find_field(node, "guid"), find_field(node, "type")) else {
        return Walker::bad("node_fields_missing");
    };
    let (Ty::Def(guid_idx), Ty::Def(type_idx)) = (guid_f.ty, type_f.ty) else {
        return Walker::bad("node_fields_type");
    };
    let (guid, ty) = (&schema.defs[guid_idx], &schema.defs[type_idx]);
    if guid_f.is_array || type_f.is_array || guid.kind != Kind::Struct || ty.kind != Kind::Enum {
        return Walker::bad("node_fields_type");
    }
    let uint_field = |name: &str| {
        find_field(guid, name)
            .filter(|f| f.ty == Ty::Native(Native::Uint) && !f.is_array)
            .map(|f| f.value)
    };
    let (Some(session_key), Some(local_key)) = (uint_field("sessionID"), uint_field("localID"))
    else {
        return Walker::bad("guid_fields");
    };

    let mut w = Walker {
        schema,
        r: Reader::new(message),
        work: Work {
            values: 0,
            max: limits.max_values,
            cancel,
        },
        max_depth: limits.max_depth,
    };
    let file_version_f =
        find_field(root, "fileVersion").filter(|f| f.ty == Ty::Native(Native::Uint) && !f.is_array);
    let mut file_version = None;
    let mut field_histogram = BTreeMap::<String, u32>::new();
    let (mut histogram, mut node_count) = (BTreeMap::<String, u32>::new(), 0u32);
    let mut roots = Vec::new();
    while let Some(field) = w.next_field(root)? {
        if file_version_f.is_some_and(|f| std::ptr::eq(field, f)) {
            file_version = Some(w.r.var_uint().map_err(FigError::MessageInvalid)?);
            continue;
        }
        if !std::ptr::eq(field, changes) {
            w.skip_field(field, 1)?;
            continue;
        }
        let count = w.r.var_uint().map_err(FigError::MessageInvalid)?;
        for _ in 0..count {
            w.work.tick()?;
            node_count = node_count
                .checked_add(1)
                .filter(|n| *n <= limits.max_nodes)
                .ok_or(FigError::BudgetExceeded("nodes"))?;
            let (mut id, mut type_name) = (None::<SourceId>, None::<String>);
            while let Some(f) = w.next_field(node)? {
                let seen = field_histogram.entry(f.name.to_string()).or_insert(0u32);
                *seen = seen.saturating_add(1);
                if std::ptr::eq(f, guid_f) {
                    let (mut session, mut local) = (None, None);
                    for sf in &guid.fields {
                        if sf.value == session_key {
                            session = Some(w.r.var_uint().map_err(FigError::MessageInvalid)?);
                        } else if sf.value == local_key {
                            local = Some(w.r.var_uint().map_err(FigError::MessageInvalid)?);
                        } else {
                            w.skip_field(sf, 2)?;
                        }
                    }
                    if let (Some(session), Some(local)) = (session, local) {
                        id = Some(SourceId { session, local });
                    }
                } else if std::ptr::eq(f, type_f) {
                    let v = w.r.var_uint().map_err(FigError::MessageInvalid)?;
                    type_name =
                        Some(ty.fields.iter().find(|e| e.value == v).map_or_else(
                            || "(unknown_enum_value)".to_owned(),
                            |e| e.name.to_string(),
                        ));
                } else {
                    w.skip_field(f, 2)?;
                }
            }
            let name = type_name.unwrap_or_else(|| "(untyped)".to_owned());
            if name == "DOCUMENT"
                && let Some(id) = id
            {
                roots.push(id);
            }
            let slot = histogram.entry(name).or_insert(0u32);
            *slot = slot.saturating_add(1);
        }
    }
    if w.r.at != message.len() {
        return Walker::bad("trailing_bytes");
    }
    Ok(Census {
        node_type_histogram: histogram,
        node_count,
        root_ids: roots,
        file_version,
        node_field_histogram: field_histogram,
    })
}

/// Result of the canvas decode stage: a schema summary and census when the whole chain decoded,
/// and the loss entries for chunks (and any soft decode failure).
pub(crate) struct Decoded {
    pub schema: Option<SchemaSummary>,
    pub census: Option<Census>,
    pub loss: Vec<LossEntry>,
}

/// Soft (typed, non-fatal) decode failure code, `None` for cancellation and budget failures.
fn soft(e: &FigError) -> Option<&'static str> {
    match e {
        FigError::SchemaInvalid(w) | FigError::MessageInvalid(w) => Some(w),
        FigError::DecoderUnavailable { stage } => Some(stage),
        _ => None,
    }
}

fn inflate_raw(
    data: &[u8],
    cap: usize,
    limit_name: &'static str,
    soft_err: FigError,
    trailing_err: FigError,
) -> Result<Vec<u8>> {
    inflate_exact(data, cap).map_err(|fail| match fail {
        InflateFail::OutputLimit => FigError::BudgetExceeded(limit_name),
        InflateFail::Invalid => soft_err,
        InflateFail::Trailing => trailing_err,
    })
}

const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

fn zstd_decode(data: &[u8], cap: usize, cancel: &CancellationToken) -> Result<Vec<u8>> {
    use std::io::Read;
    // Concatenated frames are valid zstd (RFC 8878 s3.1) but only the first frame is decoded here:
    // input left after it would make the census partial, so it is a typed refusal.
    let mut source = data;
    let mut out = Vec::new();
    {
        let mut decoder = ruzstd::decoding::StreamingDecoder::new(&mut source)
            .map_err(|_| FigError::MessageInvalid("zstd_header_invalid"))?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            cancel.check().map_err(|_| FigError::Canceled)?;
            let n = decoder
                .read(&mut buf)
                .map_err(|_| FigError::MessageInvalid("zstd_decode_failed"))?;
            if n == 0 {
                break;
            }
            if out.len().saturating_add(n) > cap {
                return Err(FigError::BudgetExceeded("message_bytes"));
            }
            out.extend_from_slice(&buf[..n]);
        }
    }
    if !source.is_empty() {
        return Err(FigError::MessageInvalid("zstd_trailing_bytes"));
    }
    Ok(out)
}

fn chunk_loss(index: usize, code: &'static str) -> LossEntry {
    LossEntry {
        part: format!("{CANVAS_PART}#chunk{index}"),
        kind: LossKind::PreservedOpaque,
        code,
    }
}

/// Parses the Kiwi schema (the first of chunks 0 and 1 that inflates as a raw DEFLATE stream; a parse
/// failure or trailing bytes in that chunk is final; the public notes disagree on numbering, so the
/// index found is recorded) and, when it decodes, walks
/// the next chunk as the message (zstd by magic, else raw DEFLATE) for the census. Soft failures
/// (invalid schema/message, unrecognised compression) become loss entries with no census; bound
/// hits and cancellation are hard errors.
pub(crate) fn decode_canvas(
    container: &FigContainer,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<Decoded> {
    let mut loss = Vec::new();
    let mut last_soft = "schema_chunk_not_found";
    let mut found = None;
    for index in 0..container.chunks.len().min(2) {
        cancel.check().map_err(|_| FigError::Canceled)?;
        let data = container.chunk_bytes(index).unwrap_or_default();
        let attempt = inflate_raw(
            data,
            limits.max_schema_bytes,
            "schema_bytes",
            FigError::SchemaInvalid("schema_inflate_failed"),
            FigError::SchemaInvalid("schema_chunk_trailing_bytes"),
        )
        .and_then(|raw| decode_schema(&raw, limits));
        match attempt {
            Ok(schema) => {
                found = Some((index, schema));
                break;
            }
            Err(e) => {
                last_soft = soft(&e).ok_or(e)?;
                // A chunk that inflates is the schema candidate and its failure is final (no second
                // candidate, so there is no ambiguity about which chunk was the schema).
                if last_soft != "schema_inflate_failed" {
                    break;
                }
            }
        }
    }
    let Some((schema_index, schema)) = found else {
        for c in &container.chunks {
            loss.push(chunk_loss(c.index, "chunk_not_interpreted"));
        }
        loss.push(LossEntry {
            part: CANVAS_PART.to_owned(),
            kind: LossKind::Unsupported,
            code: last_soft,
        });
        return Ok(Decoded {
            schema: None,
            census: None,
            loss,
        });
    };
    let summary = schema.summary(schema_index, container.chunks[schema_index].sha256);
    let message_index = schema_index.saturating_add(1);
    for c in &container.chunks {
        let code = if c.index == schema_index {
            "schema_chunk_summarized"
        } else if c.index == message_index {
            "message_chunk_census_only"
        } else {
            "chunk_not_interpreted"
        };
        loss.push(chunk_loss(c.index, code));
    }
    let decoded = match container.chunk_bytes(message_index) {
        None => Err(FigError::MessageInvalid("message_chunk_missing")),
        Some(data) if data.starts_with(&ZSTD_MAGIC) => {
            zstd_decode(data, limits.max_message_bytes, cancel)
        }
        Some(data) => inflate_raw(
            data,
            limits.max_message_bytes,
            "message_bytes",
            FigError::DecoderUnavailable {
                stage: "message_compression_unrecognized",
            },
            FigError::MessageInvalid("message_chunk_trailing_bytes"),
        ),
    }
    .and_then(|message| census(&schema, &message, limits, cancel));
    let census = match decoded {
        Ok(c) => Some(c),
        Err(e) => {
            loss.push(LossEntry {
                part: CANVAS_PART.to_owned(),
                kind: LossKind::Unsupported,
                code: soft(&e).ok_or(e)?,
            });
            None
        }
    };
    Ok(Decoded {
        schema: Some(summary),
        census,
        loss,
    })
}

/// Schema and inflated message of an admitted container, for the value-tree decoder: the same chunk
/// rules as [`decode_canvas`], but every failure is a typed error (no soft loss, no partial tree).
pub(crate) fn schema_and_message(
    container: &FigContainer,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<(Schema, Vec<u8>)> {
    let mut found = None;
    for index in 0..container.chunks.len().min(2) {
        cancel.check().map_err(|_| FigError::Canceled)?;
        let data = container.chunk_bytes(index).unwrap_or_default();
        match inflate_raw(
            data,
            limits.max_schema_bytes,
            "schema_bytes",
            FigError::SchemaInvalid("schema_inflate_failed"),
            FigError::SchemaInvalid("schema_chunk_trailing_bytes"),
        ) {
            Ok(raw) => {
                found = Some((index, decode_schema(&raw, limits)?));
                break;
            }
            Err(FigError::SchemaInvalid("schema_inflate_failed")) => {}
            Err(e) => return Err(e),
        }
    }
    let Some((schema_index, schema)) = found else {
        return Err(FigError::SchemaInvalid("schema_chunk_not_found"));
    };
    let message = match container.chunk_bytes(schema_index.saturating_add(1)) {
        None => return Err(FigError::MessageInvalid("message_chunk_missing")),
        Some(data) if data.starts_with(&ZSTD_MAGIC) => {
            zstd_decode(data, limits.max_message_bytes, cancel)?
        }
        Some(data) => inflate_raw(
            data,
            limits.max_message_bytes,
            "message_bytes",
            FigError::DecoderUnavailable {
                stage: "message_compression_unrecognized",
            },
            FigError::MessageInvalid("message_chunk_trailing_bytes"),
        )?,
    };
    Ok((schema, message))
}
