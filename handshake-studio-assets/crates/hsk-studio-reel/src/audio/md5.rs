//! MD5 (RFC 1321) for the FLAC STREAMINFO signature of the unencoded audio (RFC 9639 section 8.2). Used
//! as a checksum of decoded samples, never for security.
#![deny(clippy::arithmetic_side_effects)]

const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// K[i] = floor(abs(sin(i + 1)) * 2^32) (RFC 1321 section 3.4), computed once.
fn k_table() -> [u32; 64] {
    let mut k = [0u32; 64];
    for (i, slot) in k.iter_mut().enumerate() {
        let x = ((i as f64) + 1.0).sin().abs() * 4_294_967_296.0;
        *slot = x as u32;
    }
    k
}

/// Streaming MD5 (public for hosts that write a FLAC STREAMINFO signature after streaming the samples).
pub struct Md5 {
    state: [u32; 4],
    buffer: [u8; 64],
    buffered: usize,
    length: u64,
    k: [u32; 64],
}

impl Default for Md5 {
    fn default() -> Self {
        Self {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            buffer: [0; 64],
            buffered: 0,
            length: 0,
            k: k_table(),
        }
    }
}

impl Md5 {
    pub fn update(&mut self, mut data: &[u8]) {
        self.length = self
            .length
            .wrapping_add((data.len() as u64).wrapping_mul(8));
        while !data.is_empty() {
            let take = (64usize.wrapping_sub(self.buffered)).min(data.len());
            let (head, tail) = data.split_at(take);
            if let Some(dst) = self
                .buffer
                .get_mut(self.buffered..self.buffered.wrapping_add(take))
            {
                dst.copy_from_slice(head);
            }
            self.buffered = self.buffered.wrapping_add(take);
            data = tail;
            if self.buffered == 64 {
                let block = self.buffer;
                self.block(&block);
                self.buffered = 0;
            }
        }
    }

    fn block(&mut self, block: &[u8; 64]) {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            let at = i.wrapping_mul(4);
            let b = [
                block.get(at).copied().unwrap_or(0),
                block.get(at.wrapping_add(1)).copied().unwrap_or(0),
                block.get(at.wrapping_add(2)).copied().unwrap_or(0),
                block.get(at.wrapping_add(3)).copied().unwrap_or(0),
            ];
            *word = u32::from_le_bytes(b);
        }
        let [mut a, mut b, mut c, mut d] = self.state;
        for i in 0..64usize {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (i.wrapping_mul(5).wrapping_add(1)) % 16),
                2 => (b ^ c ^ d, (i.wrapping_mul(3).wrapping_add(5)) % 16),
                _ => (c ^ (b | !d), (i.wrapping_mul(7)) % 16),
            };
            let sum = a
                .wrapping_add(f)
                .wrapping_add(self.k.get(i).copied().unwrap_or(0))
                .wrapping_add(m.get(g).copied().unwrap_or(0));
            let rotated = sum.rotate_left(S.get(i).copied().unwrap_or(0));
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(rotated);
        }
        self.state = [
            self.state[0].wrapping_add(a),
            self.state[1].wrapping_add(b),
            self.state[2].wrapping_add(c),
            self.state[3].wrapping_add(d),
        ];
    }

    pub fn finish(mut self) -> [u8; 16] {
        let bits = self.length;
        let mut pad = vec![0x80u8];
        let used = self.buffered.wrapping_add(1) % 64;
        let zeros = if used <= 56 {
            56usize.wrapping_sub(used)
        } else {
            120usize.wrapping_sub(used)
        };
        pad.resize(zeros.wrapping_add(1), 0);
        let length_before = self.length;
        self.update(&pad);
        self.update(&bits.to_le_bytes());
        self.length = length_before;
        let mut out = [0u8; 16];
        for (i, word) in self.state.iter().enumerate() {
            if let Some(dst) = out.get_mut(i.wrapping_mul(4)..i.wrapping_mul(4).wrapping_add(4)) {
                dst.copy_from_slice(&word.to_le_bytes());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::Md5;

    fn hex(d: [u8; 16]) -> String {
        d.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// RFC 1321 appendix A.5 test suite (three of its vectors).
    #[test]
    fn rfc1321_vectors() {
        let digest = |s: &str| {
            let mut m = Md5::default();
            m.update(s.as_bytes());
            hex(m.finish())
        };
        assert_eq!(digest(""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(digest("abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            digest(
                "12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            ),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }
}
