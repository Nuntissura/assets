//! Uncompressed PCM audio in MOV/MP4 (`twos`, `sowt`, `in24`, `in32`, `fl32`, `fl64`, `raw `, `lpcm`).
//!
//! A regular PCM track (constant `stsz`, one `stts` run, no `ctts`/`stss`) is NEVER expanded per audio
//! frame: it is kept as chunk runs (`stsc` + `stco`/`co64`), so hours of 48 kHz audio index in O(chunks).
//! Decoded output is always interleaved little-endian (`PcmFormat::output_name`), delivered in bounded
//! slices that may span chunk runs. Timing is exact: ticks per audio frame must divide the tick rate.
use super::*;
use crate::time::exact_ticks_per_unit;
use hsk_studio_accord::{Rounding, Timescale};

/// Stored sample layout of a PCM sample entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmFormat {
    pub bits: u16,
    pub float: bool,
    pub big_endian: bool,
    pub signed: bool,
    pub channels: u16,
    pub rate: u32,
}

impl PcmFormat {
    /// Name of the delivered (little-endian, interleaved) sample format.
    pub fn output_name(&self) -> &'static str {
        match (self.float, self.bits, self.signed) {
            (true, 32, _) => "pcm_f32le",
            (true, _, _) => "pcm_f64le",
            (false, 8, true) => "pcm_s8",
            (false, 8, false) => "pcm_u8",
            (false, 16, _) => "pcm_s16le",
            (false, 24, _) => "pcm_s24le",
            _ => "pcm_s32le",
        }
    }
    pub fn block_bytes(&self) -> usize {
        usize::from(self.channels) * usize::from(self.bits / 8)
    }
}

/// Interpret a sample entry as PCM. `Ok(None)`: not a PCM FourCC (compressed or unknown).
/// `Err`: a PCM FourCC whose layout this reader cannot honour (never guessed).
pub fn pcm_format(
    codec: [u8; 4],
    audio: Option<&AudioFormat>,
) -> Result<Option<PcmFormat>, ReelError> {
    let (bits, float, big_endian, signed) = match &codec {
        b"raw " => (8, false, false, false),
        b"twos" => (audio.map_or(0, |a| a.sample_size), false, true, true),
        b"sowt" => (audio.map_or(0, |a| a.sample_size), false, false, true),
        b"in24" => (24, false, true, true),
        b"in32" => (32, false, true, true),
        b"fl32" => (32, true, true, true),
        b"fl64" => (64, true, true, true),
        b"lpcm" => {
            let a = audio.ok_or(ReelError::Unsupported {
                what: "lpcm-without-sound-description",
            })?;
            let l = a.lpcm.ok_or(ReelError::Unsupported {
                what: "lpcm-needs-sound-description-v2",
            })?;
            if l.flags & 0x20 != 0 {
                return Err(ReelError::Unsupported {
                    what: "lpcm-non-interleaved",
                });
            }
            let bits = u16::try_from(l.bits_per_channel).unwrap_or(0);
            let expected = u32::from(a.channels) * u32::from(bits / 8);
            if bits % 8 != 0 || l.flags & 0x8 == 0 || l.bytes_per_packet != expected {
                return Err(ReelError::Unsupported {
                    what: "lpcm-not-packed-interleaved",
                });
            }
            (
                bits,
                l.flags & 0x1 != 0,
                l.flags & 0x2 != 0,
                l.flags & 0x4 != 0,
            )
        }
        _ => return Ok(None),
    };
    let a = audio.ok_or(ReelError::Unsupported {
        what: "pcm-without-sound-description",
    })?;
    let valid_bits = if float {
        matches!(bits, 32 | 64)
    } else if signed {
        matches!(bits, 8 | 16 | 24 | 32)
    } else {
        bits == 8
    };
    if !valid_bits {
        return Err(ReelError::Unsupported {
            what: "pcm-bit-depth",
        });
    }
    if a.channels == 0 || a.channels > 64 {
        return Err(ReelError::Unsupported {
            what: "pcm-channel-count",
        });
    }
    if a.sample_rate == 0 {
        return Err(ReelError::Unsupported {
            what: "pcm-sample-rate",
        });
    }
    Ok(Some(PcmFormat {
        bits,
        float,
        big_endian,
        signed,
        channels: a.channels,
        rate: a.sample_rate,
    }))
}

/// The presented frame range of a PCM stream (after the edit list).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmWindow {
    pub presentation_start: i64,
    /// Media frame index of the first presented frame.
    pub first_frame: u64,
    pub frames: u64,
    pub ticks_per_frame: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    first_frame: u64,
    frames: u64,
    offset: u64,
}

/// Compact index of a regular PCM track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcmStream {
    pub format: PcmFormat,
    pub total_frames: u64,
    runs: Vec<Run>,
    /// `Err` carries the determinate reason this edit list / rate cannot be honoured.
    pub window: Result<PcmWindow, ReelError>,
}

impl PcmStream {
    pub(super) fn approx_bytes(&self) -> usize {
        self.runs.len() * std::mem::size_of::<Run>()
    }
    pub fn chunk_runs(&self) -> usize {
        self.runs.len()
    }
}

pub(super) struct CompactInput<'a> {
    pub codec: [u8; 4],
    pub audio: Option<AudioFormat>,
    pub fixed_size: u32,
    pub samples: usize,
    pub stts: &'a [(u32, u32)],
    pub stsc: &'a [(u32, u32)],
    pub chunk_offsets: &'a [u64],
    pub media_timescale: u32,
}

/// `Ok(None)`: not PCM, or the table is irregular (the caller expands normally and the decode path
/// reports `pcm-irregular-sample-table`).
pub(super) fn try_compact(
    input: CompactInput<'_>,
    limits: &Limits,
    budget: &mut Budget,
) -> Result<Option<PcmStream>, ReelError> {
    let Ok(Some(format)) = pcm_format(input.codec, input.audio.as_ref()) else {
        return Ok(None);
    };
    let block = format.block_bytes() as u32;
    if input.stts.len() != 1 || block == 0 || !input.fixed_size.is_multiple_of(block) {
        return Ok(None);
    }
    let delta = input.stts[0].1;
    let frames_per_sample = u64::from(input.fixed_size / block);
    // One table sample covers `frames_per_sample` frames: duration * rate == frames * timescale.
    if delta == 0
        || u128::from(delta) * u128::from(format.rate)
            != u128::from(frames_per_sample) * u128::from(input.media_timescale)
    {
        return Ok(None);
    }
    let n = input.samples as u64;
    let (mut runs, mut consumed, mut media_frame, mut stsc_i) = (Vec::new(), 0u64, 0u64, 0usize);
    for (chunk, base) in input.chunk_offsets.iter().enumerate() {
        if consumed == n {
            break;
        }
        while stsc_i + 1 < input.stsc.len() && (input.stsc[stsc_i + 1].0 as usize) <= chunk + 1 {
            stsc_i += 1;
        }
        let per_chunk = u64::from(input.stsc.get(stsc_i).map_or(0, |e| e.1));
        if per_chunk == 0 {
            continue;
        }
        if consumed + per_chunk > n {
            return Err(ReelError::Corrupt {
                what: "stsc-exceeds-stsz",
            });
        }
        if runs.len() >= limits.max_samples.min(budget.samples_left) {
            return Err(ReelError::LimitExceeded {
                what: "chunk-count",
            });
        }
        let frames = per_chunk * frames_per_sample;
        runs.push(Run {
            first_frame: media_frame,
            frames,
            offset: *base,
        });
        media_frame += frames;
        consumed += per_chunk;
    }
    if consumed != n {
        return Err(ReelError::Corrupt { what: "stsc-short" });
    }
    // Chunk runs are what a compact PCM track holds in memory: they, not frames, spend the budget.
    budget.samples_left -= runs.len();
    Ok(Some(PcmStream {
        format,
        total_frames: media_frame,
        runs,
        window: Err(ReelError::Unsupported {
            what: "pcm-window-unset",
        }),
    }))
}

/// Resolve the edit list into the presented frame window (one non-empty edit; empty edits shift it).
pub(super) fn apply_window(track: &mut TrackIndex) {
    let Some(stream) = &track.pcm else { return };
    let (rate, total) = (stream.format.rate, stream.total_frames);
    let window = (|| -> Result<PcmWindow, ReelError> {
        let tps = exact_ticks_per_unit(rate).ok_or(ReelError::Unsupported {
            what: "sample-rate-not-tick-exact",
        })?;
        let mut real = track.edits.iter().filter(|e| e.media_time.is_some());
        let (Some(edit), None) = (real.next(), real.next()) else {
            return Err(ReelError::Unsupported {
                what: "pcm-edit-list-shape",
            });
        };
        let media_time = edit.media_time.unwrap_or(0);
        let frame = Timescale::new(track.media_timescale)?.rescale(
            media_time,
            Timescale::new(rate)?,
            Rounding::Floor,
        )?;
        if !frame.exact {
            return Err(ReelError::Unsupported {
                what: "pcm-edit-not-frame-aligned",
            });
        }
        let first_frame = u64::try_from(frame.value).map_err(|_| ReelError::Corrupt {
            what: "elst-media-time",
        })?;
        let available = total.saturating_sub(first_frame);
        let frames = if edit.duration == i64::MAX {
            available
        } else {
            let duration = u64::try_from(edit.duration).unwrap_or(0);
            if !duration.is_multiple_of(tps) {
                return Err(ReelError::Unsupported {
                    what: "pcm-edit-not-frame-aligned",
                });
            }
            (duration / tps).min(available)
        };
        Ok(PcmWindow {
            presentation_start: edit.presentation_start,
            first_frame,
            frames,
            ticks_per_frame: tps,
        })
    })();
    if let Some(stream) = &mut track.pcm {
        stream.window = window;
    }
}

fn tick_span(frames: u64, tps: u64) -> Result<i64, ReelError> {
    frames
        .checked_mul(tps)
        .and_then(|t| i64::try_from(t).ok())
        .ok_or(ReelError::LimitExceeded { what: "tick-range" })
}

impl IsobmffProvider {
    /// Frame-range delivery of a PCM track: exact requested vs actual accounting, bounded slices,
    /// little-endian output.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn deliver_pcm(
        &self,
        src: &mut dyn ReadSeek,
        track: &TrackIndex,
        stream: &PcmStream,
        req: &SegmentRequest,
        limits: &Limits,
        cancel: &CancellationToken,
        sink: &mut dyn ChunkSink,
    ) -> Result<SegmentReceipt, ReelError> {
        let window = stream.window.clone()?;
        let format = stream.format;
        let tps = window.ticks_per_frame;
        let block = format.block_bytes();
        let end_tick =
            i128::from(window.presentation_start) + i128::from(window.frames) * i128::from(tps);
        let out_of_range = || ReelError::OutOfRange {
            requested: req.start_pts,
            first: window.presentation_start,
            end: i64::try_from(end_tick).unwrap_or(i64::MAX),
        };
        let rel = i128::from(req.start_pts.value()) - i128::from(window.presentation_start);
        if rel < 0 {
            return Err(out_of_range());
        }
        let start = u64::try_from(rel / i128::from(tps)).map_err(|_| out_of_range())?;
        let aligned = rel % i128::from(tps) == 0;
        let want_end = start
            .checked_add(req.count)
            .filter(|end| *end <= window.frames)
            .ok_or_else(out_of_range)?;
        let chunk_frames = (limits.max_chunk_bytes / block) as u64;
        if chunk_frames == 0 {
            return Err(ReelError::LimitExceeded {
                what: "chunk-smaller-than-sample",
            });
        }
        let swap = format.big_endian && format.bits > 8;
        let sample_bytes = usize::from(format.bits / 8);
        let mut buf = vec![0u8; (chunk_frames.min(req.count) as usize) * block];
        let (mut cursor, mut chunks, mut payload) = (start, 0u32, 0u64);
        let mut run_i = stream
            .runs
            .partition_point(|r| r.first_frame + r.frames <= window.first_frame + start);
        while cursor < want_end {
            check_cancel(cancel)?;
            let media_frame = window.first_frame + cursor;
            while stream
                .runs
                .get(run_i)
                .is_some_and(|r| r.first_frame + r.frames <= media_frame)
            {
                run_i += 1;
            }
            let run = stream.runs.get(run_i).ok_or(ReelError::Corrupt {
                what: "pcm-frame-outside-chunks",
            })?;
            let n = (want_end - cursor)
                .min(run.first_frame + run.frames - media_frame)
                .min(chunk_frames);
            let offset = (media_frame - run.first_frame)
                .checked_mul(block as u64)
                .and_then(|b| b.checked_add(run.offset))
                .ok_or(ReelError::LimitExceeded {
                    what: "byte-offset",
                })?;
            let bytes = n as usize * block;
            src.seek(SeekFrom::Start(offset))?;
            read_exact_or_truncated(src, &mut buf[..bytes], cursor - start)?;
            if swap {
                buf[..bytes]
                    .chunks_exact_mut(sample_bytes)
                    .for_each(|sample| sample.reverse());
            }
            sink.accept(Chunk {
                kind: ChunkKind::AudioSamples,
                pts: window
                    .presentation_start
                    .checked_add(tick_span(cursor, tps)?)
                    .ok_or(ReelError::LimitExceeded { what: "tick-range" })?,
                duration: Ticks::new(n * tps),
                index: cursor,
                units: n,
                sync: true,
                preroll: false,
                trim_start: 0,
                bytes: &buf[..bytes],
            })?;
            cursor += n;
            chunks += 1;
            payload += bytes as u64;
        }
        let actual_start = to_ticks(window.presentation_start + tick_span(start, tps)?)?;
        let actual_end = to_ticks(window.presentation_start + tick_span(want_end, tps)?)?;
        Ok(SegmentReceipt {
            requested: Span {
                start_pts: req.start_pts,
                end_pts: Ticks::new(
                    req.start_pts
                        .value()
                        .checked_add(tick_span(req.count, tps)? as u64)
                        .ok_or(ReelError::LimitExceeded { what: "tick-range" })?,
                ),
                units: req.count,
            },
            actual: Span {
                start_pts: actual_start,
                end_pts: actual_end,
                units: want_end - start,
            },
            disposition: if aligned {
                Disposition::Exact
            } else {
                Disposition::Approximate
            },
            source: SourceKind::Original,
            proxy_note: ProxyNote::NotRequested,
            original_id: req.grant.content_id.clone(),
            proxy_id: None,
            codec: format.output_name().into(),
            decoder_id: "isobmff-pcm",
            decoder_version: DECODER_VERSION,
            chunks,
            bytes_read: payload,
            complete: true,
            ticks_exact: track.ticks_exact,
        })
    }
}
