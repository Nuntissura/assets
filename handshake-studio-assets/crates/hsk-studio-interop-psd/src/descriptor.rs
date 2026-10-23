//! Bounded reader for the Photoshop descriptor structure (descriptor version 16) that carries
//! `CgEd` (brightness/contrast) and `lfx2` / `lmfx` (layer effects). The structure follows the
//! public Adobe Photoshop File Formats Specification, "Descriptor structure": a Unicode class name,
//! a class id, an item count, and per item a key and a type tag with its value.
//!
//! Hostile bytes: every declared count and length is checked against the bytes that remain before
//! anything is allocated; depth, items per object, total items, key length and string length are
//! bounded; an unknown KEY is just an item (callers ignore what they do not map), an unknown TYPE
//! tag cannot be skipped and refuses the whole descriptor with a fixed reason code. The result
//! owns copies only of what it keeps: scalar values, short keys and bounded strings.
#![deny(clippy::arithmetic_side_effects)]

use crate::limits::Limits;
use crate::reader::Reader;

/// Nesting limit of objects and lists.
pub(crate) const MAX_DEPTH: usize = 8;
/// Items in one object or one list.
pub(crate) const MAX_ITEMS_PER_CONTAINER: usize = 512;
/// Items in the whole descriptor, counted before each is read (the aggregate bound).
pub(crate) const MAX_TOTAL_ITEMS: usize = 8192;
/// Bytes of a key or class id.
pub(crate) const MAX_KEY_BYTES: usize = 128;

/// One descriptor value. Length-delimited payloads this crate does not interpret (`alis`, class
/// references) are skipped and reported as [`Value::Raw`]; `tdta` bytes are kept as
/// [`Value::Data`]; the block bytes stay in the document.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Object(Object),
    List(Vec<Value>),
    Double(f64),
    /// Unit float: 4-byte unit tag (`#Prc`, `#Pxl`, `#Ang`, ...) and the value.
    Unit([u8; 4], f64),
    Text(String),
    Enum { type_id: String, value: String },
    Integer(i32),
    Large(i64),
    Bool(bool),
    /// A `tdta` raw-data item's bytes (lane SMB-PS-ADJUST-STYLES-2: the colour-lookup table text);
    /// bounded by the descriptor's own data (the length is checked against it first).
    Data(Vec<u8>),
    Raw,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Object {
    pub class_id: String,
    pub items: Vec<(String, Value)>,
}

impl Object {
    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        self.items.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

impl Value {
    /// Any numeric scalar (double, unit float, long, large integer) with its raw value. A `doub` or
    /// `UntF` item holds any 64-bit pattern, NaN and the infinities included, so this is private: every
    /// consumer goes through [`Value::finite_number`] (R7 review B, class "descriptor number reaches Folio
    /// validation unchecked"; test `non_finite_descriptor_numbers_are_typed_losses_not_import_failures`).
    fn raw_number(&self) -> Option<f64> {
        match self {
            Self::Double(v) | Self::Unit(_, v) => Some(*v),
            Self::Integer(v) => Some(f64::from(*v)),
            Self::Large(v) => Some(*v as f64),
            _ => None,
        }
    }

    /// A numeric scalar as `f64`, only when it is finite; NaN and the infinities give `None`.
    pub(crate) fn finite_number(&self) -> Option<f64> {
        self.raw_number().filter(|n| n.is_finite())
    }

    /// Whether the item is a numeric scalar of any value (NaN and the infinities included): lets a
    /// caller tell an item that is present but not a usable number from an absent or non-numeric one.
    pub(crate) fn is_number(&self) -> bool {
        self.raw_number().is_some()
    }

    pub(crate) fn boolean(&self) -> Option<bool> {
        match self {
            Self::Bool(v) => Some(*v),
            _ => None,
        }
    }

    pub(crate) fn object(&self) -> Option<&Object> {
        match self {
            Self::Object(o) => Some(o),
            _ => None,
        }
    }

    pub(crate) fn list(&self) -> Option<&[Value]> {
        match self {
            Self::List(l) => Some(l),
            _ => None,
        }
    }

    /// The enumerated value id of an `enum` item (for example `Nrml` of `BlnM.Nrml`).
    pub(crate) fn enum_value(&self) -> Option<&str> {
        match self {
            Self::Enum { value, .. } => Some(value),
            _ => None,
        }
    }

    pub(crate) fn unit(&self) -> Option<[u8; 4]> {
        match self {
            Self::Unit(unit, _) => Some(*unit),
            _ => None,
        }
    }
}

/// Fixed reason codes of a refused descriptor.
pub(crate) type Reason = &'static str;

struct Cursor<'a, 'l> {
    r: Reader<'a>,
    limits: &'l Limits,
    items_left: usize,
}

impl Cursor<'_, '_> {
    fn charge(&mut self) -> Result<(), Reason> {
        self.items_left = self.items_left.checked_sub(1).ok_or("descriptor_item_limit")?;
        Ok(())
    }

    fn u32(&mut self) -> Result<u32, Reason> {
        self.r.u32("descriptor").map_err(|_| "descriptor_truncated")
    }

    fn array4(&mut self) -> Result<[u8; 4], Reason> {
        self.r.array4("descriptor").map_err(|_| "descriptor_truncated")
    }

    /// Unicode string: u32 UTF-16 unit count, then that many big-endian units.
    fn text(&mut self) -> Result<String, Reason> {
        let units = usize::try_from(self.u32()?).map_err(|_| "descriptor_string_limit")?;
        if units > self.limits.max_name_units {
            return Err("descriptor_string_limit");
        }
        let byte_len = units.checked_mul(2).ok_or("descriptor_string_limit")?;
        let raw = self.r.bytes(byte_len, "descriptor").map_err(|_| "descriptor_truncated")?;
        let decoded = char::decode_utf16(raw.chunks_exact(2).map(|p| u16::from_be_bytes([p[0], p[1]])))
            .map(|c| c.unwrap_or('\u{fffd}'))
            .collect::<String>();
        Ok(decoded.trim_end_matches('\0').to_owned())
    }

    /// Key or class id: u32 length (0 means a 4-byte id), then the bytes.
    fn key(&mut self) -> Result<String, Reason> {
        let len = usize::try_from(self.u32()?).map_err(|_| "descriptor_key_limit")?;
        let len = if len == 0 { 4 } else { len };
        if len > MAX_KEY_BYTES {
            return Err("descriptor_key_limit");
        }
        let raw = self.r.bytes(len, "descriptor").map_err(|_| "descriptor_truncated")?;
        Ok(raw.iter().map(|&b| char::from(b)).collect())
    }

    fn f64(&mut self) -> Result<f64, Reason> {
        let raw = self.r.u64("descriptor").map_err(|_| "descriptor_truncated")?;
        Ok(f64::from_bits(raw))
    }

    fn object(&mut self, depth: usize) -> Result<Object, Reason> {
        if depth > MAX_DEPTH {
            return Err("descriptor_depth");
        }
        let _class_name = self.text()?;
        let class_id = self.key()?;
        let count = usize::try_from(self.u32()?).map_err(|_| "descriptor_item_limit")?;
        // Each item needs at least a 4-byte key length, a key and a 4-byte type tag.
        if count > MAX_ITEMS_PER_CONTAINER || count > self.r.remaining() {
            return Err("descriptor_item_limit");
        }
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            self.charge()?;
            let key = self.key()?;
            let tag = self.array4()?;
            items.push((key, self.value(tag, depth)?));
        }
        Ok(Object { class_id, items })
    }

    fn value(&mut self, tag: [u8; 4], depth: usize) -> Result<Value, Reason> {
        match &tag {
            b"Objc" | b"GlbO" => Ok(Value::Object(self.object(depth.saturating_add(1))?)),
            b"VlLs" => {
                if depth >= MAX_DEPTH {
                    return Err("descriptor_depth");
                }
                let count = usize::try_from(self.u32()?).map_err(|_| "descriptor_item_limit")?;
                // Each list item has at least a 4-byte type tag.
                if count > MAX_ITEMS_PER_CONTAINER || count > self.r.remaining() {
                    return Err("descriptor_item_limit");
                }
                let mut list = Vec::with_capacity(count);
                for _ in 0..count {
                    self.charge()?;
                    let tag = self.array4()?;
                    list.push(self.value(tag, depth.saturating_add(1))?);
                }
                Ok(Value::List(list))
            }
            b"doub" => Ok(Value::Double(self.f64()?)),
            b"UntF" => {
                let unit = self.array4()?;
                Ok(Value::Unit(unit, self.f64()?))
            }
            b"TEXT" => Ok(Value::Text(self.text()?)),
            b"enum" => {
                let type_id = self.key()?;
                Ok(Value::Enum { type_id, value: self.key()? })
            }
            b"long" => Ok(Value::Integer(i32::from_be_bytes(self.u32()?.to_be_bytes()))),
            b"comp" => Ok(Value::Large(i64::from_be_bytes(
                self.r.u64("descriptor").map_err(|_| "descriptor_truncated")?.to_be_bytes(),
            ))),
            b"bool" => Ok(Value::Bool(self.r.u8("descriptor").map_err(|_| "descriptor_truncated")? != 0)),
            b"type" | b"GlbC" => {
                let _name = self.text()?;
                let _class = self.key()?;
                Ok(Value::Raw)
            }
            b"tdta" => {
                let len = usize::try_from(self.u32()?).map_err(|_| "descriptor_truncated")?;
                let bytes = self.r.bytes(len, "descriptor").map_err(|_| "descriptor_truncated")?;
                Ok(Value::Data(bytes.to_vec()))
            }
            b"alis" => {
                let len = usize::try_from(self.u32()?).map_err(|_| "descriptor_truncated")?;
                self.r.bytes(len, "descriptor").map_err(|_| "descriptor_truncated")?;
                Ok(Value::Raw)
            }
            // Object array (lane SMB-PS-ADJUST-STYLES-2; layout read from the psd-tools corpus file
            // `placedLayer.psd`, a custom envelope warp's `meshPoints`): a u32 element count, then one
            // object whose items are per-key `UnFl` columns. Kept as that object.
            b"ObAr" => {
                let count = usize::try_from(self.u32()?).map_err(|_| "descriptor_item_limit")?;
                if count > MAX_ITEMS_PER_CONTAINER {
                    return Err("descriptor_item_limit");
                }
                Ok(Value::Object(self.object(depth.saturating_add(1))?))
            }
            // Unit-float list: 4-byte unit, u32 count, `count` doubles; the count is bounded and checked
            // against the remaining bytes before anything is allocated. Kept as a list of unit floats.
            b"UnFl" => {
                let unit = self.array4()?;
                let count = usize::try_from(self.u32()?).map_err(|_| "descriptor_item_limit")?;
                let bytes = count.checked_mul(8).ok_or("descriptor_item_limit")?;
                if count > MAX_ITEMS_PER_CONTAINER || bytes > self.r.remaining() {
                    return Err("descriptor_item_limit");
                }
                let mut list = Vec::with_capacity(count);
                for _ in 0..count {
                    self.charge()?;
                    list.push(Value::Unit(unit, self.f64()?));
                }
                Ok(Value::List(list))
            }
            _ => Err("descriptor_type_unsupported"),
        }
    }
}

/// [`parse_versioned`] that also returns the bytes after the descriptor (a linked-file item's
/// payload follows its open descriptor; lane SMB-PS-ADJUST-STYLES-2).
pub(crate) fn parse_versioned_rest<'a>(
    data: &'a [u8],
    versions: &[u32],
    limits: &Limits,
) -> Result<(Object, &'a [u8]), Reason> {
    let mut cursor = Cursor { r: Reader::new(data), limits, items_left: MAX_TOTAL_ITEMS };
    for expected in versions {
        if cursor.u32()? != *expected {
            return Err("descriptor_version_unsupported");
        }
    }
    let object = cursor.object(0)?;
    Ok((object, cursor.r.rest()))
}

/// Parses `data` as: `versions.len()` leading u32 version words (each must equal the matching
/// entry), then one descriptor. Bytes after the descriptor are ignored (they stay in the block).
pub(crate) fn parse_versioned(data: &[u8], versions: &[u32], limits: &Limits) -> Result<Object, Reason> {
    let mut cursor = Cursor { r: Reader::new(data), limits, items_left: MAX_TOTAL_ITEMS };
    for expected in versions {
        if cursor.u32()? != *expected {
            return Err("descriptor_version_unsupported");
        }
    }
    cursor.object(0)
}
