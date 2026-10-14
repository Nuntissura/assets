//! Generic, schema-driven Kiwi value decoder: turns a message into a typed value tree using the
//! file's OWN embedded schema (chunk 0), never a hard-coded one. Wire rules follow evanw/kiwi
//! `js/bb.ts` (checked 2026-10-14): varuint = 7-bit groups, at most 5 bytes; varint = zigzag varuint;
//! float = 0x00 for 0.0, otherwise 4 bytes whose u32 is rotated left by 23 bits back into IEEE-754
//! order; uint64 = up to 8 7-bit groups and a full 9th byte; string = NUL-terminated UTF-8;
//! `byte[]` = varuint length + raw bytes; arrays = varuint count + elements; struct = every field in
//! order; message = (field id, value)* terminated by 0.
//!
//! Bounds (V4-01/V4-02, checked BEFORE allocating): nesting depth (`Limits::max_depth`), array
//! element count (`max_array_len`, and never more elements than input bytes left unless the element
//! type is zero-width), string bytes (`max_string_bytes`), and the aggregate number of decoded values
//! (`max_tree_values`). Every failure is a typed error and no partial tree is returned (tests:
//! fig_nodes::kiwi_tree_hostile_bounds).
#![deny(clippy::arithmetic_side_effects)]
use crate::container::Limits;
use crate::import::FigImport;
use crate::kiwi::{Kind, Native, Reader, Schema, Ty, decode_schema, schema_and_message};
use crate::{FigError, Result};
use hsk_studio_accord::CancellationToken;
use std::sync::Arc;

/// One decoded Kiwi value. Struct fields appear in schema order, message fields in wire order.
#[derive(Clone, Debug, PartialEq)]
pub enum KiwiValue {
    Bool(bool),
    Byte(u8),
    Int(i32),
    Uint(u32),
    Float(f32),
    String(String),
    Int64(i64),
    Uint64(u64),
    /// Enum value with its member name from the file's schema (`None`: value not in the enum).
    Enum { value: u32, name: Option<Arc<str>> },
    /// `byte[]`.
    Bytes(Vec<u8>),
    Array(Vec<KiwiValue>),
    Struct(Vec<KiwiField>),
    Message(Vec<KiwiField>),
}

/// A named field of a struct or message value (`id` is the schema field value).
#[derive(Clone, Debug, PartialEq)]
pub struct KiwiField {
    pub name: Arc<str>,
    pub id: u32,
    pub value: KiwiValue,
}

impl KiwiValue {
    /// Fields of a struct or message; empty for every other value.
    pub fn fields(&self) -> &[KiwiField] {
        match self {
            Self::Struct(f) | Self::Message(f) => f,
            _ => &[],
        }
    }
    /// The named field of a struct or message.
    pub fn field(&self, name: &str) -> Option<&KiwiValue> {
        self.fields()
            .iter()
            .find(|f| &*f.name == name)
            .map(|f| &f.value)
    }
    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Self::Float(v) => Some(*v),
            _ => None,
        }
    }
    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Self::Uint(v) => Some(*v),
            _ => None,
        }
    }
    pub fn as_i32(&self) -> Option<i32> {
        match self {
            Self::Int(v) => Some(*v),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(v) => Some(*v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(v) => Some(v),
            _ => None,
        }
    }
    /// Enum member name (`None` for a non-enum or a value outside the enum).
    pub fn as_enum_name(&self) -> Option<&str> {
        match self {
            Self::Enum { name, .. } => name.as_deref(),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[KiwiValue]> {
        match self {
            Self::Array(v) => Some(v),
            _ => None,
        }
    }
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(v) => Some(v),
            _ => None,
        }
    }
}

struct Decoder<'a> {
    schema: &'a Schema,
    r: Reader<'a>,
    /// Definition index -> the type encodes in zero bytes (a struct of zero-width fields).
    zero_width: Vec<bool>,
    values: u64,
    limits: &'a Limits,
    cancel: &'a CancellationToken,
}

fn bad<T>(what: &'static str) -> Result<T> {
    Err(FigError::MessageInvalid(what))
}

fn zero_width_table(schema: &Schema) -> Vec<bool> {
    // Fixed point from "nothing is zero-width": a cyclic struct stays non-zero-width (conservative;
    // decoding it hits the depth bound).
    let mut zw = vec![false; schema.defs.len()];
    loop {
        let mut changed = false;
        for (i, d) in schema.defs.iter().enumerate() {
            if zw[i] || d.kind != Kind::Struct {
                continue;
            }
            let all = d.fields.iter().all(|f| {
                !f.is_array
                    && match f.ty {
                        Ty::Native(_) => false,
                        Ty::Def(j) => zw.get(j).copied().unwrap_or(false),
                    }
            });
            if all {
                zw[i] = true;
                changed = true;
            }
        }
        if !changed {
            return zw;
        }
    }
}

impl Decoder<'_> {
    fn tick(&mut self, n: u64) -> Result<()> {
        self.values = self.values.saturating_add(n);
        if self.values > self.limits.max_tree_values {
            return Err(FigError::BudgetExceeded("tree_values"));
        }
        if self.values & 4095 == 0 || n > 1 {
            self.cancel.check().map_err(|_| FigError::Canceled)?;
        }
        Ok(())
    }

    fn remaining(&self) -> usize {
        self.r.data.len().saturating_sub(self.r.at)
    }

    fn string(&mut self) -> Result<String> {
        let rest = self.r.data.get(self.r.at..).unwrap_or_default();
        let window = &rest[..rest.len().min(self.limits.max_string_bytes.saturating_add(1))];
        let Some(n) = window.iter().position(|b| *b == 0) else {
            return if rest.len() > self.limits.max_string_bytes {
                Err(FigError::BudgetExceeded("string_bytes"))
            } else {
                bad("truncated")
            };
        };
        let s = std::str::from_utf8(&rest[..n])
            .map_err(|_| FigError::MessageInvalid("string_encoding"))?
            .to_owned();
        self.r.skip(n.saturating_add(1)).map_err(FigError::MessageInvalid)?;
        Ok(s)
    }

    fn float(&mut self) -> Result<f32> {
        let first = self.r.byte().map_err(FigError::MessageInvalid)?;
        if first == 0 {
            return Ok(0.0);
        }
        let mut b = [first, 0, 0, 0];
        for slot in b.iter_mut().skip(1) {
            *slot = self.r.byte().map_err(FigError::MessageInvalid)?;
        }
        Ok(f32::from_bits(u32::from_le_bytes(b).rotate_left(23)))
    }

    fn uint64(&mut self) -> Result<u64> {
        let (mut value, mut shift) = (0u64, 0u32);
        loop {
            let b = self.r.byte().map_err(FigError::MessageInvalid)?;
            if b & 128 != 0 && shift < 56 {
                value |= u64::from(b & 127) << shift;
                shift = shift.saturating_add(7);
            } else {
                value |= u64::from(b) << shift;
                return Ok(value);
            }
        }
    }

    fn native(&mut self, n: Native) -> Result<KiwiValue> {
        let m = FigError::MessageInvalid;
        Ok(match n {
            Native::Bool => match self.r.byte().map_err(m)? {
                0 => KiwiValue::Bool(false),
                1 => KiwiValue::Bool(true),
                _ => return bad("bool_invalid"),
            },
            Native::Byte => KiwiValue::Byte(self.r.byte().map_err(m)?),
            Native::Int => KiwiValue::Int(self.r.var_int().map_err(m)?),
            Native::Uint => KiwiValue::Uint(self.r.var_uint().map_err(m)?),
            Native::Float => KiwiValue::Float(self.float()?),
            Native::String => KiwiValue::String(self.string()?),
            Native::Uint64 => KiwiValue::Uint64(self.uint64()?),
            Native::Int64 => {
                let u = self.uint64()?;
                let half = (u >> 1) as i64;
                KiwiValue::Int64(if u & 1 == 1 { !half } else { half })
            }
        })
    }

    fn value(&mut self, ty: Ty, depth: usize) -> Result<KiwiValue> {
        if depth > self.limits.max_depth {
            return bad("depth");
        }
        self.tick(1)?;
        let i = match ty {
            Ty::Native(n) => return self.native(n),
            Ty::Def(i) => i,
        };
        let schema = self.schema;
        let d = &schema.defs[i];
        let next = depth.saturating_add(1);
        match d.kind {
            Kind::Enum => {
                let v = self.r.var_uint().map_err(FigError::MessageInvalid)?;
                let name = d.fields.iter().find(|f| f.value == v).map(|f| f.name.clone());
                Ok(KiwiValue::Enum { value: v, name })
            }
            Kind::Struct => {
                let mut out = Vec::with_capacity(d.fields.len());
                for f in &d.fields {
                    let value = self.field(f.ty, f.is_array, next)?;
                    out.push(KiwiField {
                        name: f.name.clone(),
                        id: f.value,
                        value,
                    });
                }
                Ok(KiwiValue::Struct(out))
            }
            Kind::Message => {
                let mut out: Vec<KiwiField> = Vec::new();
                loop {
                    let id = self.r.var_uint().map_err(FigError::MessageInvalid)?;
                    if id == 0 {
                        return Ok(KiwiValue::Message(out));
                    }
                    let Some(f) = d.fields.iter().find(|f| f.value == id) else {
                        return bad("unknown_field_id");
                    };
                    if out.iter().any(|seen| seen.id == id) {
                        return bad("duplicate_field_on_wire");
                    }
                    let value = self.field(f.ty, f.is_array, next)?;
                    out.push(KiwiField {
                        name: f.name.clone(),
                        id,
                        value,
                    });
                }
            }
        }
    }

    fn field(&mut self, ty: Ty, is_array: bool, depth: usize) -> Result<KiwiValue> {
        if !is_array {
            return self.value(ty, depth);
        }
        let count = self.r.var_uint().map_err(FigError::MessageInvalid)?;
        if count > self.limits.max_array_len {
            return Err(FigError::BudgetExceeded("array_len"));
        }
        let count = count as usize;
        if ty == Ty::Native(Native::Byte) {
            if count > self.remaining() {
                return bad("truncated");
            }
            let start = self.r.at;
            self.r.skip(count).map_err(FigError::MessageInvalid)?;
            self.tick(1)?;
            return Ok(KiwiValue::Bytes(self.r.data[start..self.r.at].to_vec()));
        }
        let zero_width = matches!(ty, Ty::Def(j) if self.zero_width.get(j).copied().unwrap_or(false));
        if !zero_width && count > self.remaining() {
            // Every non-zero-width element needs at least one input byte.
            return bad("array_len_exceeds_input");
        }
        // Aggregate budget before allocating the element vector.
        if self.values.saturating_add(count as u64) > self.limits.max_tree_values {
            return Err(FigError::BudgetExceeded("tree_values"));
        }
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(self.value(ty, depth)?);
        }
        Ok(KiwiValue::Array(out))
    }
}

/// Decodes `message` as the definition named `root` (a message) of `schema`. The whole input must
/// be consumed (`trailing_bytes` otherwise).
pub(crate) fn decode_tree(
    schema: &Schema,
    message: &[u8],
    root: &str,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<KiwiValue> {
    cancel.check().map_err(|_| FigError::Canceled)?;
    let Some((idx, _)) = schema.def(root, Kind::Message) else {
        return bad("root_message_missing");
    };
    let mut d = Decoder {
        schema,
        r: Reader::new(message),
        zero_width: zero_width_table(schema),
        values: 0,
        limits,
        cancel,
    };
    let v = d.value(Ty::Def(idx), 0)?;
    if d.r.at != message.len() {
        return bad("trailing_bytes");
    }
    Ok(v)
}

/// Decodes raw (already inflated) Kiwi schema bytes and a message into a value tree rooted at the
/// message definition named `root`. Bounded by `limits` as described in the module docs.
pub fn decode_kiwi(
    schema_bytes: &[u8],
    message: &[u8],
    root: &str,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<KiwiValue> {
    if schema_bytes.len() > limits.max_schema_bytes {
        return Err(FigError::BudgetExceeded("schema_bytes"));
    }
    if message.len() > limits.max_message_bytes {
        return Err(FigError::BudgetExceeded("message_bytes"));
    }
    let schema = decode_schema(schema_bytes, limits)?;
    decode_tree(&schema, message, root, limits, cancel)
}

/// Value tree of an imported (version-admitted) file's `Message`, decoded against the schema the
/// file itself carries.
pub fn decode_message_tree(
    import: &FigImport,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<KiwiValue> {
    let (schema, message) = schema_and_message(&import.container, limits, cancel)?;
    decode_tree(&schema, &message, "Message", limits, cancel)
}
