//! MSB-first bit reader over a bounded byte slice (VC-3 Figure 28 big-endian mapping; ProRes RDD 36
//! clause 5 bit order). Every read is checked: reading past the end is `None`, never a panic and never a
//! read of bytes outside the slice the caller bounded it to.
#![deny(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Next bit to read, counted from the most significant bit of `data[0]`.
    pos: u64,
    /// `data.len() * 8`.
    end: u64,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            end: (data.len() as u64).saturating_mul(8),
        }
    }

    /// Bits not yet read.
    pub(crate) fn remaining(&self) -> u64 {
        self.end.saturating_sub(self.pos)
    }

    fn bit_at(&self, at: u64) -> u32 {
        let byte = usize::try_from(at >> 3)
            .ok()
            .and_then(|i| self.data.get(i))
            .copied()
            .unwrap_or(0);
        let shift = 7u32.saturating_sub((at & 7) as u32);
        u32::from(byte.wrapping_shr(shift) & 1)
    }

    /// The next 16 bits without consuming them; bits past the end read as 0 (the caller checks the
    /// length it actually consumes against `remaining`).
    pub(crate) fn peek16(&self) -> u16 {
        let mut value = 0u16;
        for k in 0..16u64 {
            let bit = self.bit_at(self.pos.saturating_add(k)) as u16;
            value = (value << 1) | bit;
        }
        value
    }

    /// Consume `n` bits; `None` when fewer remain.
    pub(crate) fn skip(&mut self, n: u32) -> Option<()> {
        let next = self.pos.checked_add(u64::from(n))?;
        if next > self.end {
            return None;
        }
        self.pos = next;
        Some(())
    }

    /// Read `n <= 32` bits as an unsigned value; `None` when fewer remain or `n > 32`.
    pub(crate) fn read(&mut self, n: u32) -> Option<u32> {
        if n > 32 || u64::from(n) > self.remaining() {
            return None;
        }
        let mut value = 0u64;
        for k in 0..u64::from(n) {
            value = (value << 1) | u64::from(self.bit_at(self.pos.saturating_add(k)));
        }
        self.pos = self.pos.saturating_add(u64::from(n));
        u32::try_from(value).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_msb_first_and_refuses_past_the_end() {
        let mut r = BitReader::new(&[0b1010_0000, 0xFF]);
        assert_eq!(r.peek16(), 0b1010_0000_1111_1111);
        assert_eq!(r.read(3), Some(0b101));
        assert_eq!(r.read(13), Some(0b0_0000_1111_1111));
        assert_eq!(r.read(1), None);
        assert_eq!(r.peek16(), 0);
        assert_eq!(r.skip(1), None);
        let mut r = BitReader::new(&[0xAB, 0xCD, 0xEF, 0x01, 0x23]);
        assert_eq!(r.read(32), Some(0xABCD_EF01));
        assert_eq!(r.read(33), None);
        assert_eq!(r.remaining(), 8);
    }
}
