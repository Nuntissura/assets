//! Indexed colour table and the colour-table file codec (STU-COL-119; MT-31175, MT-31177).
//!
//! [`ColorTable`]: an ordered list of 1..=256 RGB entries with an optional `transparency_index`
//! below the entry count, editable entry by entry. Shipped tables (STU-COL-119 lists four):
//! `black_white` (2 entries: black, white) and `grayscale` (256 entries, entry `i` = `(i, i, i)`)
//! are built here; `mac_os` and `windows` are refused `color_table_not_shipped` because their
//! entries are not in the spec (spec gap). The black/white order is a Studio reading
//! (UNVERIFIED, D5).
//!
//! Colour-table file layout, from Adobe's Photoshop File Formats Specification ("Color Table"):
//! the file is 768 or 772 bytes; 256 RGB colours of three bytes each (red, green, blue), index 0
//! first; a 772-byte file adds two bytes for the number of colours to use and two bytes for the
//! transparency colour index. The section gives no byte order; the specification states that
//! Photoshop load files store multi-byte values big-endian, which this codec follows. A
//! transparency field of `0xFFFF` decodes as "no transparency" (Studio reading, UNVERIFIED, D5);
//! any other value must be below the count.
//!
//! Hostile input (V4-01/02): the length is checked first (exactly 768 or 772, before any
//! allocation), the count must be 1..=256 and the transparency index below the count.
//! `decode_act(encode_act(t)) == t` for every table (test
//! `mt_31177_color_table_encode_decode_is_identity`).
#![deny(clippy::arithmetic_side_effects)]

use super::ModeError;

/// Most entries of a colour table (8-bit palette, STU-COL-120).
pub const MAX_TABLE_ENTRIES: usize = 256;
/// The RGB block of a colour-table file.
pub const ACT_BLOCK_BYTES: usize = 768;
/// A colour-table file with the count and transparency trailer.
pub const ACT_TRAILER_BYTES: usize = 772;
/// The trailer's "no transparency" value (Studio reading).
pub const ACT_NO_TRANSPARENCY: u16 = 0xFFFF;

/// STU-COL-119 indexed colour table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColorTable {
    entries: Vec<[u8; 3]>,
    transparency_index: Option<u8>,
}

impl ColorTable {
    /// A table of `entries` (1..=256) and an optional transparency index below the count.
    pub fn new(entries: &[[u8; 3]], transparency_index: Option<u8>) -> Result<Self, ModeError> {
        if entries.is_empty() {
            return Err(ModeError::ColorTableEmpty);
        }
        if entries.len() > MAX_TABLE_ENTRIES {
            return Err(ModeError::ColorTableOver256);
        }
        if transparency_index.is_some_and(|t| usize::from(t) >= entries.len()) {
            return Err(ModeError::TransparencyIndexOutOfRange);
        }
        Ok(Self { entries: entries.to_vec(), transparency_index })
    }
    pub fn entries(&self) -> &[[u8; 3]] {
        &self.entries
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    /// Never true: a table holds at least one entry.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn transparency_index(&self) -> Option<u8> {
        self.transparency_index
    }
    /// Replace entry `index`.
    pub fn set_entry(&mut self, index: usize, rgb: [u8; 3]) -> Result<(), ModeError> {
        let slot = self.entries.get_mut(index).ok_or(ModeError::ColorTableIndexOutOfRange)?;
        *slot = rgb;
        Ok(())
    }
    /// Set or clear the transparency index (below the count).
    pub fn set_transparency_index(&mut self, index: Option<u8>) -> Result<(), ModeError> {
        if index.is_some_and(|t| usize::from(t) >= self.entries.len()) {
            return Err(ModeError::TransparencyIndexOutOfRange);
        }
        self.transparency_index = index;
        Ok(())
    }
    /// The colour-table file: 768 bytes for a full 256-entry table without transparency,
    /// otherwise 772 bytes with the big-endian count and transparency index (`0xFFFF` = none).
    /// Unused entries are zero.
    pub fn encode_act(&self) -> Vec<u8> {
        let trailer = self.entries.len() != MAX_TABLE_ENTRIES || self.transparency_index.is_some();
        let mut out = vec![0_u8; if trailer { ACT_TRAILER_BYTES } else { ACT_BLOCK_BYTES }];
        for (chunk, rgb) in out.chunks_exact_mut(3).zip(&self.entries) {
            chunk.copy_from_slice(rgb);
        }
        if trailer {
            // len is 1..=256 by construction.
            let count = u16::try_from(self.entries.len()).unwrap_or(256);
            let t = self.transparency_index.map_or(ACT_NO_TRANSPARENCY, u16::from);
            out[ACT_BLOCK_BYTES..ACT_BLOCK_BYTES.wrapping_add(2)].copy_from_slice(&count.to_be_bytes());
            out[ACT_BLOCK_BYTES.wrapping_add(2)..ACT_TRAILER_BYTES].copy_from_slice(&t.to_be_bytes());
        }
        out
    }
    /// Decode a colour-table file (module docs). Refusals precede allocation.
    pub fn decode_act(bytes: &[u8]) -> Result<Self, ModeError> {
        let (count, transparency) = match bytes.len() {
            ACT_BLOCK_BYTES => (MAX_TABLE_ENTRIES, None),
            ACT_TRAILER_BYTES => {
                let count = usize::from(u16::from_be_bytes([bytes[768], bytes[769]]));
                if count == 0 || count > MAX_TABLE_ENTRIES {
                    return Err(ModeError::ColorTableFileCount);
                }
                let t = u16::from_be_bytes([bytes[770], bytes[771]]);
                let t = if t == ACT_NO_TRANSPARENCY {
                    None
                } else if usize::from(t) < count {
                    Some(u8::try_from(t).map_err(|_| ModeError::TransparencyIndexOutOfRange)?)
                } else {
                    return Err(ModeError::TransparencyIndexOutOfRange);
                };
                (count, t)
            }
            _ => return Err(ModeError::ColorTableFileLength),
        };
        let entries: Vec<[u8; 3]> = bytes[..ACT_BLOCK_BYTES].chunks_exact(3).take(count).map(|c| [c[0], c[1], c[2]]).collect();
        Self::new(&entries, transparency)
    }
}

/// STU-COL-119 shipped colour tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShippedTable {
    BlackWhite,
    Grayscale,
    MacOs,
    Windows,
}

impl ShippedTable {
    pub const ALL: &'static [Self] = &[Self::BlackWhite, Self::Grayscale, Self::MacOs, Self::Windows];
    pub const fn token(self) -> &'static str {
        match self {
            Self::BlackWhite => "black_white",
            Self::Grayscale => "grayscale",
            Self::MacOs => "mac_os",
            Self::Windows => "windows",
        }
    }
    /// The declared entry count (all four are declared by the spec).
    pub const fn declared_entries(self) -> usize {
        match self {
            Self::BlackWhite => 2,
            Self::Grayscale | Self::MacOs | Self::Windows => 256,
        }
    }
}

/// A shipped table; `mac_os` and `windows` are `color_table_not_shipped` (spec gap).
pub fn shipped_table(table: ShippedTable) -> Result<ColorTable, ModeError> {
    match table {
        ShippedTable::BlackWhite => ColorTable::new(&[[0, 0, 0], [255, 255, 255]], None),
        ShippedTable::Grayscale => {
            let mut entries = [[0_u8; 3]; MAX_TABLE_ENTRIES];
            for (e, v) in entries.iter_mut().zip(0_u8..=255) {
                *e = [v, v, v];
            }
            ColorTable::new(&entries, None)
        }
        ShippedTable::MacOs | ShippedTable::Windows => Err(ModeError::ColorTableNotShipped),
    }
}
