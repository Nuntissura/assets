//! WAV (RIFF/WAVE) into the whole-file audio import (lane SMB-AUDIO-IMPORT-2): `decode_audio_file`
//! takes a WAV file like every other audio file. The header is parsed here (format tags 1 PCM,
//! 3 IEEE float, 6 A-law, 7 mu-law, and WAVE_FORMAT_EXTENSIBLE 0xFFFE with its sub-format and channel
//! mask); the samples go through the AIFF reader's little-endian encodings. The `wav-pcm` segment
//! provider (`crate::wav`) is unchanged. RF64 / BW64 are typed refusals. Every chunk size, the channel
//! count and the rate are checked before use; a data chunk shorter than declared decodes the frames
//! present.
#![deny(clippy::arithmetic_side_effects)]
use super::aiff::{AiffEncoding, AiffInfo};
use super::{MAX_AUDIO_CHANNELS, MAX_SAMPLE_RATE};
use crate::error::{ReelError, check_cancel};
use crate::model::{Limits, ReadSeek};
use hsk_studio_accord::CancellationToken;
use std::io::SeekFrom;

/// Decoder id of WAV files decoded by `decode_audio_file`.
pub const WAV_FILE_DECODER_ID: &str = "wav-riff-pcm";
const MAX_CHUNKS: u32 = 256;
const CORRUPT: ReelError = ReelError::Corrupt { what: "wav-fmt" };

pub(crate) fn sniff(head: &[u8]) -> bool {
    head.len() >= 12 && matches!(&head[0..4], b"RIFF" | b"RF64" | b"BW64") && &head[8..12] == b"WAVE"
}

fn le16(b: &[u8], at: usize) -> Result<u16, ReelError> {
    b.get(at..at.saturating_add(2))
        .and_then(|s| <[u8; 2]>::try_from(s).ok())
        .map(u16::from_le_bytes)
        .ok_or(CORRUPT)
}

fn le32(b: &[u8], at: usize) -> Result<u32, ReelError> {
    b.get(at..at.saturating_add(4))
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
        .map(u32::from_le_bytes)
        .ok_or(CORRUPT)
}

fn read_exact(src: &mut dyn ReadSeek, buf: &mut [u8]) -> Result<(), ReelError> {
    src.read_exact(buf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            ReelError::Corrupt {
                what: "wav-truncated-header",
            }
        } else {
            ReelError::Io(e.kind())
        }
    })
}

/// Parses the header up to the start of `data`: the reader info and the channel mask (extensible only).
pub(crate) fn parse(
    src: &mut dyn ReadSeek,
    file_len: u64,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<(AiffInfo, Option<u32>), ReelError> {
    src.seek(SeekFrom::Start(0))?;
    let mut head = [0u8; 12];
    read_exact(src, &mut head)?;
    match (&head[0..4], &head[8..12]) {
        (b"RIFF", b"WAVE") => {}
        (b"RF64" | b"BW64", b"WAVE") => return Err(ReelError::Unsupported { what: "rf64-wav" }),
        _ => {
            return Err(ReelError::ImportUnsupported {
                detected: "not-wav".into(),
            });
        }
    }
    let mut at = 12u64;
    // (encoding, channels, rate, block_align, mask)
    let mut fmt: Option<(AiffEncoding, u16, u32, u16, Option<u32>)> = None;
    for _ in 0..MAX_CHUNKS {
        check_cancel(cancel)?;
        if at > limits.max_probe_bytes {
            return Err(ReelError::LimitExceeded {
                what: "probe-bytes",
            });
        }
        if at.saturating_add(8) > file_len {
            break;
        }
        src.seek(SeekFrom::Start(at))?;
        let mut ch = [0u8; 8];
        read_exact(src, &mut ch)?;
        let size = u64::from(le32(&ch, 4)?);
        let body = at.checked_add(8).ok_or(CORRUPT)?;
        match &ch[0..4] {
            b"fmt " => {
                if !(16..=64).contains(&size) {
                    return Err(ReelError::Corrupt {
                        what: "wav-fmt-size",
                    });
                }
                let mut f = [0u8; 64];
                let f = &mut f[..size as usize];
                read_exact(src, f)?;
                let mut tag = le16(f, 0)?;
                let channels = le16(f, 2)?;
                let rate = le32(f, 4)?;
                let block_align = le16(f, 12)?;
                let bits = le16(f, 14)?;
                let mut mask = None;
                if tag == 0xFFFE {
                    if size < 40 {
                        return Err(ReelError::Corrupt {
                            what: "wav-extensible-size",
                        });
                    }
                    mask = Some(le32(f, 20)?).filter(|m| *m != 0);
                    tag = le16(f, 24)?;
                }
                if channels == 0 || channels > MAX_AUDIO_CHANNELS {
                    return Err(ReelError::LimitExceeded {
                        what: "wav-channels",
                    });
                }
                if rate == 0 || rate > MAX_SAMPLE_RATE {
                    return Err(ReelError::LimitExceeded {
                        what: "wav-sample-rate",
                    });
                }
                let encoding = match (tag, bits) {
                    (1, 8) => AiffEncoding::U8,
                    (1, 16) => AiffEncoding::Int16Le,
                    (1, 24 | 32) => AiffEncoding::IntLe(bits),
                    (3, 32) => AiffEncoding::FloatLe32,
                    (3, 64) => AiffEncoding::FloatLe64,
                    (6, 8) => AiffEncoding::ALaw,
                    (7, 8) => AiffEncoding::ULaw,
                    (1 | 3 | 6 | 7, _) => {
                        return Err(ReelError::Unsupported {
                            what: "wav-bit-depth",
                        });
                    }
                    (other, _) => {
                        return Err(ReelError::CodecUnavailable {
                            codec: format!("wav-tag-0x{other:04x}"),
                        });
                    }
                };
                if Some(u32::from(block_align))
                    != u32::from(channels).checked_mul(u32::from(bits / 8))
                {
                    return Err(ReelError::Corrupt {
                        what: "wav-block-align",
                    });
                }
                fmt = Some((encoding, channels, rate, block_align, mask));
            }
            b"data" => {
                let (encoding, channels, sample_rate, block_align, mask) =
                    fmt.ok_or(ReelError::Corrupt {
                        what: "wav-data-before-fmt",
                    })?;
                let available = file_len.saturating_sub(body);
                // 0xFFFFFFFF: a streamed writer that never patched the size; take what is present.
                let declared = if size == u64::from(u32::MAX) {
                    available
                } else {
                    size
                };
                let frames = declared
                    .min(available)
                    .checked_div(u64::from(block_align))
                    .unwrap_or(0);
                return Ok((
                    AiffInfo {
                        channels,
                        frames,
                        sample_rate,
                        encoding,
                        data_offset: body,
                        data_available: available,
                    },
                    mask,
                ));
            }
            _ => {}
        }
        let padded = size.checked_add(size & 1).ok_or(CORRUPT)?;
        at = body.checked_add(padded).ok_or(ReelError::LimitExceeded {
            what: "wav-offset",
        })?;
    }
    Err(ReelError::Corrupt {
        what: "wav-no-data-chunk",
    })
}
