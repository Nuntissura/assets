//! Bounds-checked big-endian cursor. Every length is validated against the bytes that remain
//! before a slice is taken.

use crate::error::{PsdError, Result};

#[derive(Clone)]
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub(crate) fn rest(&self) -> &'a [u8] {
        &self.data[self.pos..]
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
        let out = &self.data[self.pos..self.pos + n];
        self.pos += n;
        out
    }

    pub(crate) fn array4(&mut self, what: &'static str) -> Result<[u8; 4]> {
        let b = self.bytes(4, what)?;
        Ok([b[0], b[1], b[2], b[3]])
    }

    pub(crate) fn u8(&mut self, what: &'static str) -> Result<u8> {
        Ok(self.bytes(1, what)?[0])
    }

    pub(crate) fn u16(&mut self, what: &'static str) -> Result<u16> {
        let b = self.bytes(2, what)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub(crate) fn i16(&mut self, what: &'static str) -> Result<i16> {
        let b = self.bytes(2, what)?;
        Ok(i16::from_be_bytes([b[0], b[1]]))
    }

    pub(crate) fn u32(&mut self, what: &'static str) -> Result<u32> {
        let b = self.bytes(4, what)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn i32(&mut self, what: &'static str) -> Result<i32> {
        let b = self.bytes(4, what)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn u64(&mut self, what: &'static str) -> Result<u64> {
        let b = self.bytes(8, what)?;
        Ok(u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    /// Reads a 4-byte (PSD) or 8-byte (PSB `wide`) length and converts it to `usize`.
    pub(crate) fn length(&mut self, wide: bool, what: &'static str) -> Result<usize> {
        let v = if wide { self.u64(what)? } else { u64::from(self.u32(what)?) };
        usize::try_from(v).map_err(|_| PsdError::Overflow)
    }
}
