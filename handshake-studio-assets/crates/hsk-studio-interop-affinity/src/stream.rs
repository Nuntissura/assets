//! Bounded stream extraction: `#Fil` tag, stored / zlib / zstd payload, predictor inverse, CRC-32.
//! Behaviour reference: afread `ArchivePrivate::Extract` and its `undo_predict*` helpers (MIT,
//! Copyright (c) 2020-2021 Vladimir Mamonov), re-expressed with size, ratio and output caps.

use crate::container::{Codec, Cur, Entry, FIXED_HEADER_END, KaContainer, TAG_FIL, predictor_class};
use crate::error::{AffinityError, Result};
use hsk_studio_accord::CancellationToken;

impl KaContainer<'_> {
    /// Decodes one directory record: bounds, `#Fil`, declared size and ratio caps, inflate (never past
    /// the declared size), predictor inverse, size and CRC-32 checks.
    pub fn read_entry(&self, entry: &Entry, cancel: &CancellationToken) -> Result<Vec<u8>> {
        cancel.check().map_err(|_| AffinityError::Canceled)?;
        let limits = &self.limits;
        if entry.size > limits.max_stream_bytes {
            return Err(AffinityError::StreamTooLarge);
        }
        let codec = entry.codec();
        let stored_len = match codec {
            Codec::Stored => entry.size,
            Codec::Zlib | Codec::Zstd => entry.stored_size,
            Codec::Unknown(_) => return Err(AffinityError::CompressionUnsupported(entry.compression)),
        };
        if entry.size > stored_len.max(1).saturating_mul(limits.max_ratio) {
            return Err(AffinityError::StreamRatio);
        }
        if entry.offset < FIXED_HEADER_END {
            return Err(AffinityError::StreamOutOfBounds);
        }
        let mut c = Cur::at(self.bytes, entry.offset).ok_or(AffinityError::StreamOutOfBounds)?;
        if c.take(4).ok_or(AffinityError::StreamOutOfBounds)? != TAG_FIL {
            return Err(AffinityError::StreamTagInvalid);
        }
        let len = usize::try_from(stored_len).map_err(|_| AffinityError::StreamOutOfBounds)?;
        let payload = c.take(len).ok_or(AffinityError::StreamOutOfBounds)?;
        let size = usize::try_from(entry.size).map_err(|_| AffinityError::StreamTooLarge)?;
        let mut out = match codec {
            Codec::Stored => payload.to_vec(),
            Codec::Zlib => miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(payload, size).map_err(|e| {
                if e.status == miniz_oxide::inflate::TINFLStatus::HasMoreOutput {
                    AffinityError::InflatedTooLarge
                } else {
                    AffinityError::ZlibInvalid
                }
            })?,
            Codec::Zstd => zstd(payload, size, cancel)?,
            Codec::Unknown(_) => return Err(AffinityError::CompressionUnsupported(entry.compression)),
        };
        if out.len() != size {
            return Err(AffinityError::SizeMismatch);
        }
        undo_predictor(entry.compression, &mut out);
        if crc32fast::hash(&out) != entry.crc32 {
            return Err(AffinityError::CrcMismatch);
        }
        Ok(out)
    }

    /// [`KaContainer::read_entry`] for the head stream named `name`.
    pub fn read_stream(&self, name: &str, cancel: &CancellationToken) -> Result<Vec<u8>> {
        let s = self.stream(name)?;
        self.read_entry(&s.entry, cancel)
    }
}

/// Every zstd frame of `data` (afread streams them back to back), never more than `cap` bytes out.
fn zstd(data: &[u8], cap: usize, cancel: &CancellationToken) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut source = data;
    let mut out: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    while !source.is_empty() {
        let mut decoder = ruzstd::decoding::StreamingDecoder::new(&mut source).map_err(|_| AffinityError::ZstdInvalid)?;
        loop {
            cancel.check().map_err(|_| AffinityError::Canceled)?;
            let n = decoder.read(&mut buf).map_err(|_| AffinityError::ZstdInvalid)?;
            if n == 0 {
                break;
            }
            if out.len().saturating_add(n) > cap {
                return Err(AffinityError::InflatedTooLarge);
            }
            out.extend_from_slice(buf.get(..n).ok_or(AffinityError::ZstdInvalid)?);
        }
    }
    Ok(out)
}

/// Predictor inverse (afread `Extract`): class = bits 6-7, interleave flag = bit 5 (only with class 2).
/// Class 1: byte running sum. Class 2 without the flag: little-endian u16 running sum. Class 2 with the
/// flag: byte running sum, then a 64 KiB stream is de-interleaved from two 32 KiB halves. Class 3: no
/// inverse (afread applies none; the CRC check decides).
fn undo_predictor(compression: u8, data: &mut [u8]) {
    let class = predictor_class(compression);
    let flag = class >= 2 && compression & 0x20 != 0;
    match (class, flag) {
        (1, _) => byte_sum(data),
        (2, false) => {
            let mut acc: u16 = 0;
            for pair in data.chunks_exact_mut(2) {
                acc = acc.wrapping_add(u16::from_le_bytes([pair[0], pair[1]]));
                pair.copy_from_slice(&acc.to_le_bytes());
            }
        }
        (2, true) => {
            byte_sum(data);
            if data.len() == 0x10000 {
                let src = data.to_vec();
                let (lo, hi) = src.split_at(0x8000);
                for (i, quad) in data.chunks_exact_mut(4).enumerate() {
                    let j = i.wrapping_mul(2);
                    let (Some(a), Some(b), Some(c), Some(d)) = (hi.get(j), lo.get(j), hi.get(j.wrapping_add(1)), lo.get(j.wrapping_add(1)))
                    else {
                        return;
                    };
                    quad.copy_from_slice(&[*a, *b, *c, *d]);
                }
            }
        }
        _ => {}
    }
}

fn byte_sum(data: &mut [u8]) {
    let mut acc: u8 = 0;
    for v in data {
        acc = acc.wrapping_add(*v);
        *v = acc;
    }
}
