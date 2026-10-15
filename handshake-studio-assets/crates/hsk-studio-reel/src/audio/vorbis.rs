//! Vorbis I decode through `lewton` 0.10.2 (MIT OR Apache-2.0, default features off: no `ogg`, no
//! async). Reel owns everything around it: the identification header is checked against the audio
//! bounds before lewton sees it, the setup header's codebooks are walked by a bounded pre-scan (Vorbis I
//! specification section 3.2.1) that refuses entry and lookup-table sizes past `SETUP_BUDGET` before
//! lewton allocates them, the comment header is never parsed (lewton allocates its declared lengths
//! unchecked), and every lewton call runs behind `packet::guarded`.
#![deny(clippy::arithmetic_side_effects)]
use super::packet::{PacketDecoder, guarded, interleave};
use super::{MAX_AUDIO_CHANNELS, MAX_SAMPLE_RATE};
use crate::error::ReelError;
use lewton::audio::{PreviousWindowRight, read_audio_packet_generic};
use lewton::header::{IdentHeader, SetupHeader, read_header_ident, read_header_setup};

/// Vorbis decoder id (receipts carry it as `decoder_id`).
pub const VORBIS_DECODER_ID: &str = "vorbis-lewton-0.10.2";
pub const VORBIS_DECODER_VERSION: &str = "1";

/// Largest header packet accepted (identification, comment or setup).
pub(crate) const MAX_HEADER_PACKET: usize = 1 << 20;
/// Bound on the codebook sizes a setup header may declare, summed over its codebooks: entries, and
/// lookup values of lookup types 1 and 2. Real encoders stay far below (libvorbis codebooks hold at most
/// a few thousand entries).
const SETUP_BUDGET: u64 = 1 << 20;

/// Stream parameters from the identification header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VorbisInfo {
    pub channels: u8,
    pub sample_rate: u32,
}

const CORRUPT_IDENT: ReelError = ReelError::Corrupt {
    what: "vorbis-ident-header",
};

/// Check the identification header (packet type 1) against the audio bounds.
pub(crate) fn parse_ident(packet: &[u8]) -> Result<VorbisInfo, ReelError> {
    if packet.len() < 30 || packet.first() != Some(&1) || packet.get(1..7) != Some(b"vorbis") {
        return Err(CORRUPT_IDENT);
    }
    let le32 = |at: usize| {
        packet
            .get(at..at.saturating_add(4))
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map(u32::from_le_bytes)
            .ok_or(CORRUPT_IDENT)
    };
    if le32(7)? != 0 {
        return Err(ReelError::Unsupported {
            what: "vorbis-version",
        });
    }
    let channels = packet.get(11).copied().ok_or(CORRUPT_IDENT)?;
    let sample_rate = le32(12)?;
    if channels == 0 || sample_rate == 0 {
        return Err(CORRUPT_IDENT);
    }
    if u16::from(channels) > MAX_AUDIO_CHANNELS {
        return Err(ReelError::LimitExceeded {
            what: "vorbis-channels",
        });
    }
    if sample_rate > MAX_SAMPLE_RATE {
        return Err(ReelError::LimitExceeded {
            what: "vorbis-sample-rate",
        });
    }
    Ok(VorbisInfo {
        channels,
        sample_rate,
    })
}

/// Vorbis bit packing: LSB-first within each byte (Vorbis I section 2.1.4).
struct LsbBits<'a> {
    data: &'a [u8],
    pos: u64,
}

impl LsbBits<'_> {
    fn read(&mut self, n: u32) -> Result<u64, ReelError> {
        let mut value = 0u64;
        for i in 0..n {
            let byte = usize::try_from(self.pos >> 3)
                .ok()
                .and_then(|b| self.data.get(b))
                .copied()
                .ok_or(ReelError::Corrupt {
                    what: "vorbis-setup-truncated",
                })?;
            let bit = (byte >> (self.pos & 7)) & 1;
            value |= u64::from(bit) << i;
            self.pos = self.pos.saturating_add(1);
        }
        Ok(value)
    }
}

fn ilog(x: u64) -> u32 {
    64u32.saturating_sub(x.leading_zeros())
}

/// Greatest r with r^dims <= entries (Vorbis I section 9.2.3).
fn lookup1_values(entries: u64, dims: u64) -> Result<u64, ReelError> {
    if dims == 0 {
        return Err(ReelError::Corrupt {
            what: "vorbis-codebook-dimensions",
        });
    }
    let pow = |r: u64| -> Option<u64> {
        let mut acc: u64 = 1;
        for _ in 0..dims {
            acc = acc.checked_mul(r)?;
            if acc > entries {
                return None;
            }
        }
        Some(acc)
    };
    let mut r = (entries as f64).powf(1.0 / dims as f64).floor() as u64;
    while r > 0 && pow(r).is_none() {
        r = r.saturating_sub(1);
    }
    while pow(r.saturating_add(1)).is_some() {
        r = r.saturating_add(1);
    }
    Ok(r)
}

/// Walk the codebooks of a setup header (packet type 5) and refuse sizes past `SETUP_BUDGET`.
pub(crate) fn prescan_setup(packet: &[u8]) -> Result<(), ReelError> {
    if packet.len() > MAX_HEADER_PACKET {
        return Err(ReelError::LimitExceeded {
            what: "vorbis-header-size",
        });
    }
    if packet.first() != Some(&5) || packet.get(1..7) != Some(b"vorbis") {
        return Err(ReelError::Corrupt {
            what: "vorbis-setup-header",
        });
    }
    let mut b = LsbBits {
        data: packet,
        pos: 56,
    };
    let over = || ReelError::LimitExceeded {
        what: "vorbis-setup-budget",
    };
    let count = b.read(8)?.saturating_add(1);
    let mut budget: u64 = 0;
    for _ in 0..count {
        if b.read(24)? != 0x56_4342 {
            return Err(ReelError::Corrupt {
                what: "vorbis-codebook-sync",
            });
        }
        let dims = b.read(16)?;
        let entries = b.read(24)?;
        budget = budget
            .checked_add(entries)
            .filter(|v| *v <= SETUP_BUDGET)
            .ok_or_else(over)?;
        if b.read(1)? == 0 {
            let sparse = b.read(1)? == 1;
            for _ in 0..entries {
                if !sparse || b.read(1)? == 1 {
                    b.read(5)?;
                }
            }
        } else {
            b.read(5)?;
            let mut current: u64 = 0;
            while current < entries {
                let number = b.read(ilog(entries.saturating_sub(current)))?;
                current = current.saturating_add(number);
                if current > entries {
                    return Err(ReelError::Corrupt {
                        what: "vorbis-codebook-lengths",
                    });
                }
                if number == 0 {
                    // A zero run cannot make progress; the length counter would overflow first.
                    return Err(ReelError::Corrupt {
                        what: "vorbis-codebook-lengths",
                    });
                }
            }
        }
        match b.read(4)? {
            0 => {}
            kind @ (1 | 2) => {
                b.read(32)?;
                b.read(32)?;
                let value_bits = b.read(4)?.saturating_add(1);
                b.read(1)?;
                let values = if kind == 1 {
                    lookup1_values(entries, dims)?
                } else {
                    entries.checked_mul(dims).ok_or_else(over)?
                };
                budget = budget
                    .checked_add(values)
                    .filter(|v| *v <= SETUP_BUDGET)
                    .ok_or_else(over)?;
                let bits = values.checked_mul(value_bits).ok_or_else(over)?;
                if bits > (packet.len() as u64).saturating_mul(8) {
                    return Err(ReelError::Corrupt {
                        what: "vorbis-setup-truncated",
                    });
                }
                b.pos = b.pos.saturating_add(bits);
            }
            _ => {
                return Err(ReelError::Corrupt {
                    what: "vorbis-lookup-type",
                });
            }
        }
    }
    Ok(())
}

pub(crate) struct VorbisDecoder {
    ident: IdentHeader,
    setup: SetupHeader,
    pwr: PreviousWindowRight,
    poisoned: bool,
    pub(crate) info: VorbisInfo,
}

impl VorbisDecoder {
    /// From the identification and setup header packets (the comment header is not needed).
    pub(crate) fn new(ident_packet: &[u8], setup_packet: &[u8]) -> Result<Self, ReelError> {
        if ident_packet.len() > MAX_HEADER_PACKET {
            return Err(ReelError::LimitExceeded {
                what: "vorbis-header-size",
            });
        }
        let info = parse_ident(ident_packet)?;
        prescan_setup(setup_packet)?;
        let mut poisoned = false;
        let ident = guarded(&mut poisoned, || {
            read_header_ident(ident_packet).map_err(|_| CORRUPT_IDENT)
        })?;
        let setup = guarded(&mut poisoned, || {
            read_header_setup(
                setup_packet,
                ident.audio_channels,
                (ident.blocksize_0, ident.blocksize_1),
            )
            .map_err(|_| ReelError::Corrupt {
                what: "vorbis-setup-header",
            })
        })?;
        Ok(Self {
            ident,
            setup,
            pwr: PreviousWindowRight::new(),
            poisoned,
            info,
        })
    }
}

impl PacketDecoder for VorbisDecoder {
    fn decode(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize, ReelError> {
        if packet.is_empty() {
            // A zero-length audio packet carries no audio (Vorbis I section 4.3.1 lets decoders drop it).
            return Ok(0);
        }
        let (ident, setup, pwr) = (&self.ident, &self.setup, &mut self.pwr);
        let planar: Vec<Vec<f32>> = guarded(&mut self.poisoned, || {
            read_audio_packet_generic(ident, setup, packet, pwr).map_err(|_| ReelError::Corrupt {
                what: "vorbis-audio-packet",
            })
        })?;
        if planar.len() != usize::from(self.info.channels) {
            return Err(ReelError::Corrupt {
                what: "vorbis-channel-count",
            });
        }
        interleave(&planar, out)
    }
}

/// Split Xiph-laced CodecPrivate (Matroska A_VORBIS): count-1, then the lengths of all but the last
/// packet in Xiph lacing.
#[allow(dead_code)] // Matroska A_VORBIS CodecPrivate (container hook slice)
pub(crate) fn split_xiph_lacing(private: &[u8]) -> Result<Vec<&[u8]>, ReelError> {
    let bad = ReelError::Corrupt {
        what: "xiph-lacing",
    };
    let (&count, mut rest) = private.split_first().ok_or(bad.clone())?;
    let count = usize::from(count).saturating_add(1);
    let mut sizes = Vec::new();
    for _ in 1..count {
        let mut size = 0usize;
        loop {
            let (&b, tail) = rest.split_first().ok_or(bad.clone())?;
            rest = tail;
            size = size.checked_add(usize::from(b)).ok_or(bad.clone())?;
            if b < 255 {
                break;
            }
        }
        sizes.push(size);
    }
    let mut out = Vec::new();
    for size in sizes {
        let (head, tail) = rest.split_at_checked(size).ok_or(bad.clone())?;
        out.push(head);
        rest = tail;
    }
    out.push(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup1_values_is_the_largest_root() {
        assert_eq!(lookup1_values(81, 4).unwrap(), 3);
        assert_eq!(lookup1_values(80, 4).unwrap(), 2);
        assert_eq!(lookup1_values(1 << 24, 1).unwrap(), 1 << 24);
        assert_eq!(lookup1_values(1 << 24, 65_535).unwrap(), 1);
    }
}
