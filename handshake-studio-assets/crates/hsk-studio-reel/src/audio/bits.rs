//! MSB-first bit reader and writer for the audio codecs (FLAC, MPEG audio side info). Every read is
//! checked: reading past the slice is `None`, never a panic and never a read outside the slice.
#![deny(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

pub(crate) struct Bits<'a> {
    data: &'a [u8],
    /// Next bit, counted from the most significant bit of `data[0]`.
    pos: usize,
    end: usize,
}

impl<'a> Bits<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            end: data.len().saturating_mul(8),
        }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.end.saturating_sub(self.pos)
    }

    /// Bytes consumed so far, rounding a partial byte up.
    pub(crate) fn bytes_consumed(&self) -> usize {
        self.pos.div_ceil(8)
    }

    pub(crate) fn align(&mut self) {
        self.pos = self.bytes_consumed().saturating_mul(8).min(self.end);
    }

    /// Read `n` bits (0..=32) as an unsigned value.
    pub(crate) fn read(&mut self, n: u32) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        if n > 32 || (n as usize) > self.remaining() {
            return None;
        }
        let mut value: u64 = 0;
        let mut left = n;
        while left > 0 {
            let byte = u64::from(*self.data.get(self.pos >> 3)?);
            let used = (self.pos & 7) as u32;
            let avail = 8u32.checked_sub(used)?;
            let take = avail.min(left);
            let shift = avail.checked_sub(take)?;
            let bits = (byte >> shift) & ((1u64 << take).checked_sub(1)?);
            value = (value << take) | bits;
            left = left.checked_sub(take)?;
            self.pos = self.pos.checked_add(take as usize)?;
        }
        u32::try_from(value).ok()
    }

    pub(crate) fn bit(&mut self) -> Option<bool> {
        self.read(1).map(|b| b != 0)
    }

    /// Read `n` bits (1..=32) as a two's complement signed value.
    pub(crate) fn read_signed(&mut self, n: u32) -> Option<i32> {
        if n == 0 {
            return Some(0);
        }
        let raw = self.read(n)?;
        let shift = 32u32.checked_sub(n)?;
        Some(((raw << shift) as i32) >> shift)
    }

    /// Unary code: count of 0 bits before the next 1 bit (the 1 is consumed). `None` past `limit` zeros
    /// or past the end of the slice.
    pub(crate) fn unary(&mut self, limit: u32) -> Option<u32> {
        let mut count: u32 = 0;
        loop {
            let byte_index = self.pos >> 3;
            let byte = *self.data.get(byte_index)?;
            let used = (self.pos & 7) as u32;
            // Bits of this byte not yet read, left-aligned.
            let rest = byte << used;
            let avail = 8u32.checked_sub(used)?;
            let bits_left_in_slice = self.remaining();
            if rest != 0 {
                let zeros = rest.leading_zeros();
                if zeros >= avail {
                    return None;
                }
                let step = zeros.checked_add(1)? as usize;
                if step > bits_left_in_slice {
                    return None;
                }
                self.pos = self.pos.checked_add(step)?;
                count = count.checked_add(zeros)?;
                return if count > limit { None } else { Some(count) };
            }
            if (avail as usize) > bits_left_in_slice {
                return None;
            }
            self.pos = self.pos.checked_add(avail as usize)?;
            count = count.checked_add(avail)?;
            if count > limit {
                return None;
            }
        }
    }
}

/// MSB-first bit writer (FLAC encoder).
#[derive(Default)]
pub(crate) struct BitWriter {
    bytes: Vec<u8>,
    acc: u64,
    nbits: u32,
}

impl BitWriter {
    pub(crate) fn with_capacity(bytes: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(bytes),
            acc: 0,
            nbits: 0,
        }
    }

    /// Write the low `n` bits (0..=32) of `value`.
    pub(crate) fn write(&mut self, n: u32, value: u32) {
        if n == 0 {
            return;
        }
        let n = n.min(32);
        let mask = if n == 32 {
            u64::from(u32::MAX)
        } else {
            (1u64 << n).wrapping_sub(1)
        };
        self.acc = (self.acc << n) | (u64::from(value) & mask);
        self.nbits = self.nbits.wrapping_add(n);
        while self.nbits >= 8 {
            self.nbits = self.nbits.wrapping_sub(8);
            self.bytes.push((self.acc >> self.nbits) as u8);
        }
        // Keep only the bits not yet flushed.
        self.acc &= (1u64 << self.nbits).wrapping_sub(1);
    }

    pub(crate) fn write_signed(&mut self, n: u32, value: i32) {
        self.write(n, value as u32);
    }

    pub(crate) fn write_unary(&mut self, zeros: u32) {
        let mut left = zeros;
        while left >= 32 {
            self.write(32, 0);
            left = left.wrapping_sub(32);
        }
        self.write(left.wrapping_add(1), 1);
    }

    pub(crate) fn align(&mut self) {
        if self.nbits > 0 {
            let pad = 8u32.wrapping_sub(self.nbits);
            self.write(pad, 0);
        }
    }

    pub(crate) fn bit_len(&self) -> u64 {
        (self.bytes.len() as u64)
            .saturating_mul(8)
            .saturating_add(u64::from(self.nbits))
    }

    /// The bits written after the last complete byte (low `bit_len() % 8` bits).
    pub(crate) fn pending_bits(&self) -> u32 {
        self.acc as u32
    }

    /// Bytes written so far (only complete bytes; call `align` first).
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn into_bytes(mut self) -> Vec<u8> {
        self.align();
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_round_trips_writer() {
        let mut w = BitWriter::default();
        w.write(3, 5);
        w.write_signed(7, -3);
        w.write_unary(40);
        w.write(32, 0xDEAD_BEEF);
        let bytes = w.into_bytes();
        let mut r = Bits::new(&bytes);
        assert_eq!(r.read(3), Some(5));
        assert_eq!(r.read_signed(7), Some(-3));
        assert_eq!(r.unary(64), Some(40));
        assert_eq!(r.read(32), Some(0xDEAD_BEEF));
        assert!(r.read(32).is_none(), "past the end is None, never a panic");
    }
}
