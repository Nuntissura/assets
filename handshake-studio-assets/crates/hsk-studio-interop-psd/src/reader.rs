//! Bounds-checked cursor, big-endian by default (PSD/PSB). A little-endian cursor
//! ([`Reader::with_order`]) reads the byte-swapped layer-and-mask structures that TIFF
//! ImageSourceData blocks (`MIB8`, `46B8`) use: numbers are little-endian and the four-byte tags
//! (signatures, keys, blend keys) are stored reversed, so [`Reader::array4`] returns them reversed back
//! into their canonical spelling. Every length is validated against the bytes that remain before a
//! slice is taken.
#![deny(clippy::arithmetic_side_effects)]

use crate::error::{PsdError, Result};

#[derive(Clone)]
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    little: bool,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0, little: false }
    }

    /// `little` selects the byte-swapped (TIFF ImageSourceData) layout.
    pub(crate) fn with_order(data: &'a [u8], little: bool) -> Self {
        Self { data, pos: 0, little }
    }

    pub(crate) fn is_little(&self) -> bool {
        self.little
    }

    pub(crate) fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub(crate) fn rest(&self) -> &'a [u8] {
        self.data.get(self.pos..).unwrap_or_default()
    }

    pub(crate) fn peek(&self, n: usize) -> Option<&'a [u8]> {
        self.data.get(self.pos..self.pos.checked_add(n)?)
    }

    pub(crate) fn bytes(&mut self, n: usize, what: &'static str) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(PsdError::Overflow)?;
        let out = self.data.get(self.pos..end).ok_or(PsdError::Truncated(what))?;
        self.pos = end;
        Ok(out)
    }

    pub(crate) fn skip_up_to(&mut self, n: usize) -> &'a [u8] {
        let n = n.min(self.remaining());
        let end = self.pos.saturating_add(n).min(self.data.len());
        let out = self.data.get(self.pos..end).unwrap_or_default();
        self.pos = end;
        out
    }

    pub(crate) fn array4(&mut self, what: &'static str) -> Result<[u8; 4]> {
        let b = self.bytes(4, what)?;
        let tag = [b[0], b[1], b[2], b[3]];
        Ok(if self.little { [tag[3], tag[2], tag[1], tag[0]] } else { tag })
    }

    pub(crate) fn u8(&mut self, what: &'static str) -> Result<u8> {
        Ok(self.bytes(1, what)?[0])
    }

    pub(crate) fn u16(&mut self, what: &'static str) -> Result<u16> {
        let b = self.bytes(2, what)?;
        let a = [b[0], b[1]];
        Ok(if self.little { u16::from_le_bytes(a) } else { u16::from_be_bytes(a) })
    }

    pub(crate) fn i16(&mut self, what: &'static str) -> Result<i16> {
        let b = self.bytes(2, what)?;
        let a = [b[0], b[1]];
        Ok(if self.little { i16::from_le_bytes(a) } else { i16::from_be_bytes(a) })
    }

    pub(crate) fn u32(&mut self, what: &'static str) -> Result<u32> {
        let b = self.bytes(4, what)?;
        let a = [b[0], b[1], b[2], b[3]];
        Ok(if self.little { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) })
    }

    pub(crate) fn i32(&mut self, what: &'static str) -> Result<i32> {
        let b = self.bytes(4, what)?;
        let a = [b[0], b[1], b[2], b[3]];
        Ok(if self.little { i32::from_le_bytes(a) } else { i32::from_be_bytes(a) })
    }

    pub(crate) fn u64(&mut self, what: &'static str) -> Result<u64> {
        let b = self.bytes(8, what)?;
        let a = [b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]];
        Ok(if self.little { u64::from_le_bytes(a) } else { u64::from_be_bytes(a) })
    }

    /// Reads a 4-byte (PSD) or 8-byte (PSB `wide`) length and converts it to `usize`.
    pub(crate) fn length(&mut self, wide: bool, what: &'static str) -> Result<usize> {
        let v = if wide { self.u64(what)? } else { u64::from(self.u32(what)?) };
        usize::try_from(v).map_err(|_| PsdError::Overflow)
    }
}
