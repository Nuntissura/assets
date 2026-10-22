//! Bounded schema-1 wire codec. This crate performs no filesystem or engine operations.
//! Hosts use one writer, check the save stamp before publication, and atomically replace
//! a complete blob or retain recoverable prior/new blobs. Decode/restore failures retain
//! the last valid Core. Atomic host storage and interrupted-save recovery are unproven here.
#![forbid(unsafe_code)]

use kumihimo_core::{Core, PersistedLayout, SaveStamp, SAVED_SCHEMA};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use std::{collections::BTreeSet, fmt, io};

pub const DEFAULT_MAX_BYTES: usize = 8 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodecOptions {
    pub max_bytes: usize,
}
impl Default for CodecOptions {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    ByteLimit { limit: usize, attempted: usize },
    Json(String),
    UnsupportedSchema { found: u32, supported: u32 },
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
fn json(e: serde_json::Error) -> Error {
    Error::Json(e.to_string())
}
fn schema(d: &PersistedLayout) -> Result<(), Error> {
    if d.schema != SAVED_SCHEMA {
        Err(Error::UnsupportedSchema {
            found: d.schema,
            supported: SAVED_SCHEMA,
        })
    } else {
        Ok(())
    }
}

/// Complete-input decode with strict unknown fields and duplicate-member rejection,
/// including inside arbitrary nested semantic leaves before maps can normalize input.
pub fn decode(bytes: &[u8]) -> Result<PersistedLayout, Error> {
    decode_with_options(bytes, CodecOptions::default())
}
pub fn decode_with_options(bytes: &[u8], options: CodecOptions) -> Result<PersistedLayout, Error> {
    if bytes.len() > options.max_bytes {
        return Err(Error::ByteLimit {
            limit: options.max_bytes,
            attempted: bytes.len(),
        });
    }
    let mut check = serde_json::Deserializer::from_slice(bytes);
    Unique.deserialize(&mut check).map_err(json)?;
    check.end().map_err(json)?;
    let mut parser = serde_json::Deserializer::from_slice(bytes);
    let layout = <PersistedLayout as serde::Deserialize>::deserialize(&mut parser).map_err(json)?;
    parser.end().map_err(json)?;
    schema(&layout)?;
    Ok(layout)
}
pub fn encode(layout: &PersistedLayout) -> Result<Vec<u8>, Error> {
    encode_with_options(layout, CodecOptions::default())
}
pub fn encode_with_options(
    layout: &PersistedLayout,
    options: CodecOptions,
) -> Result<Vec<u8>, Error> {
    schema(layout)?;
    let mut writer = Bounded {
        bytes: vec![],
        limit: options.max_bytes,
        attempted: None,
    };
    let result = serde_json::to_writer(&mut writer, layout);
    if let Some(attempted) = writer.attempted {
        return Err(Error::ByteLimit {
            limit: writer.limit,
            attempted,
        });
    }
    result.map_err(json)?;
    Ok(writer.bytes)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedBytes {
    stamp: SaveStamp,
    bytes: Vec<u8>,
}
impl SavedBytes {
    pub fn stamp(&self) -> &SaveStamp {
        &self.stamp
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn is_current(&self, core: &Core) -> bool {
        core.is_save_current(&self.stamp)
    }
}
pub fn encode_core(core: &Core, options: CodecOptions) -> Result<SavedBytes, Error> {
    let data = core.save_data();
    let bytes = encode_with_options(data.layout(), options)?;
    Ok(SavedBytes {
        stamp: data.stamp().clone(),
        bytes,
    })
}
struct Bounded {
    bytes: Vec<u8>,
    limit: usize,
    attempted: Option<usize>,
}
impl io::Write for Bounded {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let attempted = self
            .bytes
            .len()
            .checked_add(buf.len())
            .unwrap_or(usize::MAX);
        if attempted > self.limit {
            self.attempted = Some(attempted);
            return Err(io::Error::other("JSON output byte limit"));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
// serde_json's ordinary recursion guard stays enabled on both passes. No Value tree
// is allocated; object member names alone are retained for that object's lifetime.
struct Unique;
impl<'de> DeserializeSeed<'de> for Unique {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Unique {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("JSON with unique object members")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<(), A::Error> {
        let mut names = BTreeSet::new();
        while let Some(name) = a.next_key::<String>()? {
            if !names.insert(name) {
                return Err(serde::de::Error::custom("duplicate JSON member"));
            }
            a.next_value_seed(Unique)?;
        }
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<(), A::Error> {
        while a.next_element_seed(Unique)?.is_some() {}
        Ok(())
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_string<E: serde::de::Error>(self, _: String) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }
}
