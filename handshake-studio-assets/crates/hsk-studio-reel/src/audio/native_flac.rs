//! Native FLAC stream reader ("fLaC" + metadata + frames): a bounded look-ahead window over the source,
//! one frame decoded at a time. The window never grows past the worst-case frame size STREAMINFO allows
//! plus one read; a frame that does not fit it is `flac-frame-too-large`, never a larger buffer.
#![deny(clippy::arithmetic_side_effects)]
use super::engine::FrameSource;
use super::flac::{FlacDecoder, FlacLimits, FrameOutcome, NativeHeader, parse_native_header};
use crate::error::{ReelError, check_cancel};
use crate::model::ReadSeek;
use hsk_studio_accord::CancellationToken;
use std::io::SeekFrom;

const READ_STEP: usize = 64 * 1024;

pub(crate) struct NativeFlac<'a> {
    src: &'a mut dyn ReadSeek,
    decoder: FlacDecoder,
    window: Vec<u8>,
    pos: usize,
    eof: bool,
    max_window: usize,
    bytes_read: u64,
    decoded: u64,
    total: u64,
    ints: Option<Vec<i32>>,
}

/// Read and check the metadata of a native FLAC stream (bounded by `max_probe_bytes`).
pub(crate) fn read_header(
    src: &mut dyn ReadSeek,
    file_len: u64,
    max_probe_bytes: u64,
    limits: &FlacLimits,
) -> Result<NativeHeader, ReelError> {
    let mut read_at = |at: u64, buf: &mut [u8]| -> Result<(), ReelError> {
        src.seek(SeekFrom::Start(at))?;
        src.read_exact(buf).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                ReelError::Corrupt {
                    what: "flac-metadata-truncated",
                }
            } else {
                ReelError::Io(e.kind())
            }
        })
    };
    parse_native_header(&mut read_at, max_probe_bytes.min(file_len), limits)
}

impl<'a> NativeFlac<'a> {
    pub(crate) fn open(
        src: &'a mut dyn ReadSeek,
        header: NativeHeader,
        limits: FlacLimits,
    ) -> Result<Self, ReelError> {
        let decoder = FlacDecoder::new(header.info, limits)?;
        let worst = header.info.worst_frame_bytes()?;
        let max_window = worst
            .checked_add(READ_STEP)
            .ok_or(ReelError::LimitExceeded {
                what: "flac-frame-size",
            })?;
        src.seek(SeekFrom::Start(header.frames_offset))?;
        Ok(Self {
            src,
            decoder,
            window: Vec::new(),
            pos: 0,
            eof: false,
            max_window,
            bytes_read: header.frames_offset,
            decoded: 0,
            total: header.info.total_samples,
            ints: None,
        })
    }

    /// Keep the integer samples of every frame (for bit-exact checks).
    pub(crate) fn keep_integers(&mut self) {
        self.ints = Some(Vec::new());
    }

    pub(crate) fn take_integers(&mut self) -> Vec<i32> {
        self.ints.take().unwrap_or_default()
    }

    /// MD5 verdict after a whole-stream decode (`None` when the stream declares no MD5).
    pub(crate) fn md5_matches(self) -> Option<bool> {
        self.decoder.md5_matches()
    }

    fn fill(&mut self) -> Result<(), ReelError> {
        if self.pos > 0 {
            self.window.drain(..self.pos);
            self.pos = 0;
        }
        let room = self
            .max_window
            .saturating_sub(self.window.len())
            .min(READ_STEP);
        if room == 0 {
            return Err(ReelError::Corrupt {
                what: "flac-frame-too-large",
            });
        }
        let old = self.window.len();
        self.window.resize(old.saturating_add(room), 0);
        let mut got = 0usize;
        while got < room {
            let dst = self
                .window
                .get_mut(old.saturating_add(got)..)
                .unwrap_or(&mut []);
            let n = self.src.read(dst)?;
            if n == 0 {
                self.eof = true;
                break;
            }
            got = got.saturating_add(n);
        }
        self.window.truncate(old.saturating_add(got));
        self.bytes_read = self.bytes_read.saturating_add(got as u64);
        Ok(())
    }
}

impl FrameSource for NativeFlac<'_> {
    fn next_block(
        &mut self,
        out: &mut Vec<f32>,
        cancel: &CancellationToken,
    ) -> Result<Option<usize>, ReelError> {
        // A declared total ends the stream: whatever follows (a trailing tag) is not audio.
        if self.total != 0 && self.decoded >= self.total {
            return Ok(None);
        }
        loop {
            check_cancel(cancel)?;
            let window = self.window.get(self.pos..).unwrap_or(&[]);
            if window.is_empty() {
                if self.eof {
                    return if self.total != 0 && self.decoded < self.total {
                        Err(ReelError::Truncated { delivered: 0 })
                    } else {
                        Ok(None)
                    };
                }
                self.fill()?;
                continue;
            }
            match self
                .decoder
                .decode_frame(window, self.eof, out, self.ints.as_mut())?
            {
                FrameOutcome::Frame { consumed, frames } => {
                    self.pos = self.pos.saturating_add(consumed);
                    self.decoded = self.decoded.saturating_add(frames as u64);
                    return Ok(Some(frames));
                }
                FrameOutcome::NeedMore => self.fill()?,
            }
        }
    }

    fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}
