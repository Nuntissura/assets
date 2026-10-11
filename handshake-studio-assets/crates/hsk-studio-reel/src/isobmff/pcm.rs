//! Uncompressed PCM audio in MOV/MP4 (`twos`, `sowt`, `in24`, `in32`, `fl32`, `fl64`, `raw `, `lpcm`) and
//! in Matroska (`A_PCM/INT/LIT`, `A_PCM/INT/BIG`, `A_PCM/FLOAT/IEEE`).
//!
//! A regular PCM track (constant `stsz`, one `stts` run, no `ctts`/`stss`) is not expanded per audio
//! frame: it is kept as chunk runs (`stsc` + `stco`/`co64`), so hours of 48 kHz audio index in O(chunks).
//! Decoded output is interleaved little-endian (`PcmFormat::output_name`), delivered in bounded
//! slices that may span chunk runs. Timing is exact: ticks per audio frame must divide the tick rate.
//! Delivery (`deliver_pcm_stream`) is shared by every container that builds a `PcmStream`.
//!
//! Every size and offset below comes from the file: all arithmetic is checked (V4-01).
#![deny(clippy::arithmetic_side_effects)]
use super::*;
use crate::time::exact_ticks_per_unit;
use hsk_studio_accord::{Rounding, Timescale};

fn tick_range() -> ReelError {
    ReelError::LimitExceeded { what: "tick-range" }
}

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
        usize::from(self.channels).saturating_mul(usize::from(self.bits / 8))
    }

    /// The one admission rule for a PCM layout (shared by every container): bit depth, channel count
    /// and rate are checked here and refused with a typed error when unsupported. 8-bit integer PCM is unsigned (`signed == false`).
    pub(crate) fn validated(
        bits: u16,
        float: bool,
        big_endian: bool,
        signed: bool,
        channels: u16,
        rate: u32,
    ) -> Result<Self, ReelError> {
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
        if channels == 0 || channels > 64 {
            return Err(ReelError::Unsupported {
                what: "pcm-channel-count",
            });
        }
        if rate == 0 {
            return Err(ReelError::Unsupported {
                what: "pcm-sample-rate",
            });
        }
        Ok(Self {
            bits,
            float,
            big_endian,
            signed,
            channels,
            rate,
        })
    }
}

/// Interpret a sample entry as PCM. `Ok(None)`: not a PCM FourCC (compressed or unknown).
/// `Err`: a PCM FourCC whose layout this reader cannot honour (refused, not guessed).
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
            let expected = u32::from(a.channels).saturating_mul(u32::from(bits / 8));
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
    PcmFormat::validated(bits, float, big_endian, signed, a.channels, a.sample_rate).map(Some)
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

impl Run {
    /// One past the last media frame of the run.
    fn end(&self) -> u64 {
        self.first_frame.saturating_add(self.frames)
    }
}

/// Compact index of a regular PCM track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcmStream {
    pub format: PcmFormat,
    pub total_frames: u64,
    runs: Vec<Run>,
    /// `Err` carries the determinate reason this edit list / rate cannot be honoured.
    pub window: Result<PcmWindow, ReelError>,
    /// False when the window start needed rounding to reach ticks (container timescales that do not
    /// divide the tick rate); ISOBMFF windows come from exact edit arithmetic and leave it true.
    pub(crate) ticks_exact: bool,
}

impl PcmStream {
    /// Compact stream over `(file offset, frames)` blocks in stream order (Matroska blocks). Blocks must
    /// hold whole frames; the caller supplies the already-validated presentation window.
    pub(crate) fn from_blocks(
        format: PcmFormat,
        blocks: impl IntoIterator<Item = (u64, u64)>,
        window: Result<PcmWindow, ReelError>,
        ticks_exact: bool,
    ) -> Result<Self, ReelError> {
        let (mut runs, mut next) = (Vec::new(), 0u64);
        for (offset, frames) in blocks {
            runs.push(Run {
                first_frame: next,
                frames,
                offset,
            });
            next = next.checked_add(frames).ok_or(ReelError::LimitExceeded {
                what: "pcm-frame-count",
            })?;
        }
        Ok(Self {
            format,
            total_frames: next,
            runs,
            window,
            ticks_exact,
        })
    }

    pub(super) fn approx_bytes(&self) -> usize {
        self.runs.len().saturating_mul(std::mem::size_of::<Run>())
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
    let frames_per_sample = u64::from(input.fixed_size.checked_div(block).unwrap_or(0));
    // One table sample covers `frames_per_sample` frames: duration * rate == frames * timescale.
    let lhs = u128::from(delta).checked_mul(u128::from(format.rate));
    let rhs = u128::from(frames_per_sample).checked_mul(u128::from(input.media_timescale));
    if delta == 0 || lhs.is_none() || lhs != rhs {
        return Ok(None);
    }
    let n = input.samples as u64;
    let (mut runs, mut consumed, mut media_frame, mut stsc_i) = (Vec::new(), 0u64, 0u64, 0usize);
    for (chunk, base) in input.chunk_offsets.iter().enumerate() {
        if consumed == n {
            break;
        }
        // The last `stsc` entry whose first chunk (1-based) is at or before this chunk.
        let this_chunk = chunk.saturating_add(1);
        while let Some(next) = input.stsc.get(stsc_i.saturating_add(1)) {
            if (next.0 as usize) <= this_chunk {
                stsc_i = stsc_i.saturating_add(1);
            } else {
                break;
            }
        }
        let per_chunk = u64::from(input.stsc.get(stsc_i).map_or(0, |e| e.1));
        if per_chunk == 0 {
            continue;
        }
        if consumed
            .checked_add(per_chunk)
            .is_none_or(|total| total > n)
        {
            return Err(ReelError::Corrupt {
                what: "stsc-exceeds-stsz",
            });
        }
        if runs.len() >= limits.max_samples.min(budget.samples_left) {
            return Err(ReelError::LimitExceeded {
                what: "chunk-count",
            });
        }
        let frames = per_chunk
            .checked_mul(frames_per_sample)
            .ok_or(ReelError::LimitExceeded {
                what: "pcm-frame-count",
            })?;
        runs.push(Run {
            first_frame: media_frame,
            frames,
            offset: *base,
        });
        media_frame = media_frame
            .checked_add(frames)
            .ok_or(ReelError::LimitExceeded {
                what: "pcm-frame-count",
            })?;
        // `consumed + per_chunk <= n` was checked above.
        consumed = consumed.saturating_add(per_chunk);
    }
    if consumed != n {
        return Err(ReelError::Corrupt { what: "stsc-short" });
    }
    // Chunk runs are what a compact PCM track holds in memory: they, not frames, spend the budget
    // (`runs.len() <= samples_left` was enforced while pushing).
    budget.samples_left = budget.samples_left.saturating_sub(runs.len());
    Ok(Some(PcmStream {
        format,
        total_frames: media_frame,
        runs,
        window: Err(ReelError::Unsupported {
            what: "pcm-window-unset",
        }),
        ticks_exact: true,
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
            duration.checked_div(tps).unwrap_or(0).min(available)
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
        .ok_or_else(tick_range)
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
        deliver_pcm_stream(
            src,
            stream,
            track.ticks_exact,
            "isobmff-pcm",
            req,
            limits,
            cancel,
            sink,
        )
    }
}

/// Frame-range delivery of any compact PCM stream (ISOBMFF chunk runs, Matroska blocks): exact requested
/// vs actual accounting, bounded slices, interleaved little-endian output.
#[allow(clippy::too_many_arguments)]
pub(crate) fn deliver_pcm_stream(
    src: &mut dyn ReadSeek,
    stream: &PcmStream,
    track_ticks_exact: bool,
    decoder_id: &'static str,
    req: &SegmentRequest,
    limits: &Limits,
    cancel: &CancellationToken,
    sink: &mut dyn ChunkSink,
) -> Result<SegmentReceipt, ReelError> {
    let window = stream.window.clone()?;
    let format = stream.format;
    let tps = window.ticks_per_frame;
    let block = format.block_bytes();
    let tps_wide = i128::from(tps);
    let end_tick = i128::from(window.frames)
        .checked_mul(tps_wide)
        .and_then(|span| span.checked_add(i128::from(window.presentation_start)))
        .ok_or_else(tick_range)?;
    let out_of_range = || ReelError::OutOfRange {
        requested: req.start_pts,
        first: window.presentation_start,
        end: i64::try_from(end_tick).unwrap_or(i64::MAX),
    };
    let rel = i128::from(req.start_pts.value())
        .checked_sub(i128::from(window.presentation_start))
        .ok_or_else(tick_range)?;
    if rel < 0 {
        return Err(out_of_range());
    }
    let start = rel
        .checked_div(tps_wide)
        .and_then(|frames| u64::try_from(frames).ok())
        .ok_or_else(out_of_range)?;
    let aligned = rel.checked_rem(tps_wide) == Some(0);
    let want_end = start
        .checked_add(req.count)
        .filter(|end| *end <= window.frames)
        .ok_or_else(out_of_range)?;
    // Export is exact or it fails, before any frame is read: the start must be a whole frame.
    require_exact_start(req, aligned, || {
        to_ticks(
            window
                .presentation_start
                .checked_add(tick_span(start, tps)?)
                .ok_or_else(tick_range)?,
        )
    })?;
    let chunk_frames = limits.max_chunk_bytes.checked_div(block).unwrap_or(0) as u64;
    if chunk_frames == 0 {
        return Err(ReelError::LimitExceeded {
            what: "chunk-smaller-than-sample",
        });
    }
    let swap = format.big_endian && format.bits > 8;
    let sample_bytes = usize::from(format.bits / 8);
    // One reused buffer of at most `max_chunk_bytes` (chunk_frames * block <= max_chunk_bytes).
    let buf_len = usize::try_from(chunk_frames.min(req.count))
        .ok()
        .and_then(|frames| frames.checked_mul(block))
        .ok_or(ReelError::LimitExceeded {
            what: "chunk-bytes",
        })?;
    let mut buf = vec![0u8; buf_len];
    let (mut cursor, mut chunks, mut payload) = (start, 0u32, 0u64);
    let first_media = window
        .first_frame
        .checked_add(start)
        .ok_or_else(tick_range)?;
    let mut run_i = stream.runs.partition_point(|r| r.end() <= first_media);
    while cursor < want_end {
        check_cancel(cancel)?;
        let media_frame = window
            .first_frame
            .checked_add(cursor)
            .ok_or_else(tick_range)?;
        while stream
            .runs
            .get(run_i)
            .is_some_and(|r| r.end() <= media_frame)
        {
            run_i = run_i.saturating_add(1);
        }
        let run = stream.runs.get(run_i).ok_or(ReelError::Corrupt {
            what: "pcm-frame-outside-chunks",
        })?;
        let n = want_end
            .saturating_sub(cursor)
            .min(run.end().saturating_sub(media_frame))
            .min(chunk_frames);
        let offset = media_frame
            .saturating_sub(run.first_frame)
            .checked_mul(block as u64)
            .and_then(|b| b.checked_add(run.offset))
            .ok_or(ReelError::LimitExceeded {
                what: "byte-offset",
            })?;
        // `n <= min(chunk_frames, req.count)`, so `bytes <= buf.len()`.
        let bytes = usize::try_from(n)
            .ok()
            .and_then(|frames| frames.checked_mul(block))
            .filter(|bytes| *bytes <= buf.len())
            .ok_or(ReelError::LimitExceeded {
                what: "chunk-bytes",
            })?;
        src.seek(SeekFrom::Start(offset))?;
        read_exact_or_truncated(src, &mut buf[..bytes], cursor.saturating_sub(start))?;
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
                .ok_or_else(tick_range)?,
            duration: Ticks::new(n.checked_mul(tps).ok_or_else(tick_range)?),
            index: cursor,
            units: n,
            sync: true,
            preroll: false,
            trim_start: 0,
            bytes: &buf[..bytes],
        })?;
        cursor = cursor.saturating_add(n);
        chunks = chunks.saturating_add(1);
        payload = payload.saturating_add(bytes as u64);
    }
    let tick_at = |frames: u64| -> Result<Ticks, ReelError> {
        to_ticks(
            window
                .presentation_start
                .checked_add(tick_span(frames, tps)?)
                .ok_or_else(tick_range)?,
        )
    };
    let actual_start = tick_at(start)?;
    let actual_end = tick_at(want_end)?;
    let requested_span = u64::try_from(tick_span(req.count, tps)?).map_err(|_| tick_range())?;
    Ok(SegmentReceipt {
        requested: Span {
            start_pts: req.start_pts,
            end_pts: Ticks::new(
                req.start_pts
                    .value()
                    .checked_add(requested_span)
                    .ok_or_else(tick_range)?,
            ),
            units: req.count,
        },
        actual: Span {
            start_pts: actual_start,
            end_pts: actual_end,
            units: want_end.saturating_sub(start),
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
        decoder_id,
        decoder_version: DECODER_VERSION,
        chunks,
        bytes_read: payload,
        complete: true,
        ticks_exact: track_ticks_exact && stream.ticks_exact,
        video: None,
    })
}
