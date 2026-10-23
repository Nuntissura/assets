//! Processor state checkpoints (lane SMB-AUDITION-UI-2): a processor that supports checkpoints
//! writes its mutable signal state (filter memories, delay lines, envelopes, positions) with
//! [`StateWriter`] in `Processor::save_state` and reads it back with [`StateReader`] in
//! `Processor::restore_state`. Values are written as exact bit patterns (little-endian), so a
//! restored processor continues bit for bit. Parameters that automation sets are not part of the
//! state: the owner re-applies its automation lanes after a restore (set values are pure functions
//! of the lane value). A restore checks every length against the processor's own buffers and
//! refuses a mismatch with `LengthMismatch` before changing anything it cannot finish.

use crate::error::ScoreError;

/// Appends exact values.
#[derive(Default)]
pub struct StateWriter(Vec<u8>);

impl StateWriter {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn usize(&mut self, v: usize) -> &mut Self {
        self.u64(v as u64)
    }
    pub fn f64(&mut self, v: f64) -> &mut Self {
        self.u64(v.to_bits())
    }
    pub fn f64s(&mut self, v: &[f64]) -> &mut Self {
        self.usize(v.len());
        for x in v {
            self.f64(*x);
        }
        self
    }
    pub fn f32s(&mut self, v: &[f32]) -> &mut Self {
        self.usize(v.len());
        for x in v {
            self.0.extend_from_slice(&x.to_bits().to_le_bytes());
        }
        self
    }
    pub fn u32s(&mut self, v: &[u32]) -> &mut Self {
        self.usize(v.len());
        for x in v {
            self.0.extend_from_slice(&x.to_le_bytes());
        }
        self
    }
    pub fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.0)
    }
}

/// Reads values written by [`StateWriter`], bounds-checked.
pub struct StateReader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> StateReader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
    fn take<const N: usize>(&mut self) -> Result<[u8; N], ScoreError> {
        let end = self.at.checked_add(N).ok_or(ScoreError::LengthMismatch)?;
        let v = self.bytes.get(self.at..end).ok_or(ScoreError::LengthMismatch)?;
        self.at = end;
        <[u8; N]>::try_from(v).map_err(|_| ScoreError::LengthMismatch)
    }
    pub fn u64(&mut self) -> Result<u64, ScoreError> {
        self.take::<8>().map(u64::from_le_bytes)
    }
    pub fn usize(&mut self) -> Result<usize, ScoreError> {
        usize::try_from(self.u64()?).map_err(|_| ScoreError::LengthMismatch)
    }
    pub fn f64(&mut self) -> Result<f64, ScoreError> {
        self.u64().map(f64::from_bits)
    }
    /// Fills `into` exactly (the written length must equal `into.len()`).
    pub fn f64s(&mut self, into: &mut [f64]) -> Result<(), ScoreError> {
        if self.usize()? != into.len() {
            return Err(ScoreError::LengthMismatch);
        }
        for x in into.iter_mut() {
            *x = self.f64()?;
        }
        Ok(())
    }
    pub fn f32s(&mut self, into: &mut [f32]) -> Result<(), ScoreError> {
        if self.usize()? != into.len() {
            return Err(ScoreError::LengthMismatch);
        }
        for x in into.iter_mut() {
            *x = f32::from_bits(u32::from_le_bytes(self.take::<4>()?));
        }
        Ok(())
    }
    pub fn u32s(&mut self, into: &mut [u32]) -> Result<(), ScoreError> {
        if self.usize()? != into.len() {
            return Err(ScoreError::LengthMismatch);
        }
        for x in into.iter_mut() {
            *x = u32::from_le_bytes(self.take::<4>()?);
        }
        Ok(())
    }
    /// Every byte must have been read.
    pub fn finish(self) -> Result<(), ScoreError> {
        if self.at == self.bytes.len() { Ok(()) } else { Err(ScoreError::LengthMismatch) }
    }
}
