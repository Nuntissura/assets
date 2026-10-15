//! MPEG audio elementary streams (.mp3 / .mp2): ID3v2 tags skipped by their declared (syncsafe) size,
//! frame sync established by `mp3::find_frame` (a header confirmed by its successors), the Xing/Info
//! frame of an encoder recognised and not decoded, gapless trimming from its LAME tag (encoder delay +
//! padding, plus the 529-sample filterbank delay, the convention libavformat applies), trailing tags
//! (ID3v1, APE) ending the stream. Without a Xing frame count the total comes from a header-only walk.
#![deny(clippy::arithmetic_side_effects)]
use super::engine::{FrameSource, StreamShape};
use super::mp3::{
    Mp3Decoder, find_frame, frame_len, hdr_channels, hdr_frame_samples, hdr_sample_rate_hz,
    header_is_valid, headers_compatible,
};
use crate::error::{ReelError, check_cancel};
use crate::model::{Limits, ReadSeek};
use hsk_studio_accord::CancellationToken;
use std::io::SeekFrom;

const WINDOW: usize = 64 * 1024;
/// Decoder delay of the MPEG audio filterbank (528 + 1 samples), added to the LAME encoder delay.
const DECODER_DELAY: u64 = 529;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Mp3Info {
    pub(crate) first_header: [u8; 4],
    pub(crate) free_format: usize,
    /// Offset of the first audio frame (after ID3v2 and the Xing/Info frame).
    pub(crate) frames_offset: u64,
    pub(crate) shape: StreamShape,
}

fn read_full(src: &mut dyn ReadSeek, buf: &mut [u8]) -> Result<usize, ReelError> {
    let mut got = 0usize;
    while got < buf.len() {
        let n = src.read(buf.get_mut(got..).unwrap_or(&mut []))?;
        if n == 0 {
            break;
        }
        got = got.saturating_add(n);
    }
    Ok(got)
}

/// Bytes of the ID3v2 tag at `at` (0 when there is none).
fn id3v2_len(head: &[u8]) -> Result<u64, ReelError> {
    if head.len() < 10 || head.get(..3) != Some(b"ID3") {
        return Ok(0);
    }
    let size = head.get(6..10).unwrap_or(&[]);
    if size.iter().any(|b| b & 0x80 != 0) {
        return Err(ReelError::Corrupt { what: "id3v2-size" });
    }
    let n = size.iter().fold(0u64, |acc, b| (acc << 7) | u64::from(*b));
    let footer = if head.get(5).is_some_and(|f| f & 0x10 != 0) {
        10
    } else {
        0
    };
    Ok(n.saturating_add(10).saturating_add(footer))
}

/// Xing/Info frame of an encoder: (frame count, encoder delay, padding) when present.
/// (frame count, (encoder delay, padding)) of a Xing/Info frame.
type XingInfo = (Option<u32>, Option<(u16, u16)>);

fn xing(frame: &[u8], header: &[u8; 4]) -> Option<XingInfo> {
    let mpeg1 = header[1] & 0x08 != 0;
    let mono = header[3] & 0xC0 == 0xC0;
    let side = match (mpeg1, mono) {
        (true, false) => 32,
        (true, true) => 17,
        (false, false) => 17,
        (false, true) => 9,
    };
    let crc = if header[1] & 1 == 0 { 2 } else { 0 };
    let at = 4usize.saturating_add(crc).saturating_add(side);
    let tag = frame.get(at..at.saturating_add(4))?;
    if tag != b"Xing" && tag != b"Info" {
        return None;
    }
    let be32 = |p: usize| -> Option<u32> {
        frame
            .get(p..p.checked_add(4)?)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map(u32::from_be_bytes)
    };
    let flags = be32(at.saturating_add(4))?;
    let mut p = at.saturating_add(8);
    let mut frames = None;
    if flags & 1 != 0 {
        frames = be32(p);
        p = p.saturating_add(4);
    }
    if flags & 2 != 0 {
        p = p.saturating_add(4);
    }
    if flags & 4 != 0 {
        p = p.saturating_add(100);
    }
    if flags & 8 != 0 {
        p = p.saturating_add(4);
    }
    // LAME tag: 9-byte encoder string, then the encoder delay / padding (12 + 12 bits) at offset 21.
    let lame = frame.get(p..p.saturating_add(24)).and_then(|t| {
        let is_lame = t.starts_with(b"LAME") || t.starts_with(b"Lavf") || t.starts_with(b"Lavc");
        let d = t.get(21..24)?;
        let v = (u32::from(d[0]) << 16) | (u32::from(d[1]) << 8) | u32::from(d[2]);
        is_lame.then_some(((v >> 12) as u16, (v & 0xFFF) as u16))
    });
    Some((frames, lame))
}

/// Find the stream's first frame and its totals, reading only headers.
pub(crate) fn probe(
    src: &mut dyn ReadSeek,
    file_len: u64,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<Mp3Info, ReelError> {
    let mut start = 0u64;
    // Skip any number of ID3v2 tags (bounded by the probe budget).
    loop {
        src.seek(SeekFrom::Start(start))?;
        let mut head = [0u8; 10];
        let n = read_full(src, &mut head)?;
        let tag = id3v2_len(head.get(..n).unwrap_or(&[]))?;
        if tag == 0 {
            break;
        }
        start = start.saturating_add(tag);
        if start > limits.max_probe_bytes || start > file_len {
            return Err(ReelError::LimitExceeded { what: "id3v2-size" });
        }
    }
    src.seek(SeekFrom::Start(start))?;
    let mut window = vec![0u8; WINDOW];
    let n = read_full(src, &mut window)?;
    window.truncate(n);
    let mut free_format = 0usize;
    let (at, len) = find_frame(&window, &mut free_format).ok_or(ReelError::ImportUnsupported {
        detected: "mpeg-audio-no-frame-sync".into(),
    })?;
    let first_at = start.saturating_add(at as u64);
    let header: [u8; 4] = window
        .get(at..at.saturating_add(4))
        .and_then(|h| h.try_into().ok())
        .ok_or(ReelError::Corrupt {
            what: "mpeg-audio-header",
        })?;
    let rate = hdr_sample_rate_hz(&header);
    let channels = hdr_channels(&header);
    let spf = hdr_frame_samples(&header) as u64;
    let frame = window.get(at..at.saturating_add(len)).unwrap_or(&[]);
    let (frames_offset, total, skip) = match xing(frame, &header) {
        Some((frames, gapless)) => {
            let after = first_at.saturating_add(len as u64);
            let frames = match frames {
                Some(f) => u64::from(f),
                None => walk(src, after, file_len, &header, free_format, limits, cancel)?,
            };
            let decoded = frames.saturating_mul(spf);
            match gapless {
                Some((delay, padding)) => {
                    let skip = u64::from(delay).saturating_add(DECODER_DELAY);
                    let total = decoded
                        .saturating_sub(u64::from(delay))
                        .saturating_sub(u64::from(padding));
                    (after, total, skip)
                }
                None => (after, decoded, 0),
            }
        }
        None => {
            let frames = walk(
                src,
                first_at,
                file_len,
                &header,
                free_format,
                limits,
                cancel,
            )?;
            (first_at, frames.saturating_mul(spf), 0)
        }
    };
    Ok(Mp3Info {
        first_header: header,
        free_format,
        frames_offset,
        shape: StreamShape {
            rate,
            channels,
            total: Some(total),
            skip,
        },
    })
}

/// Count frames from `at` by their headers until the first header that does not continue the stream.
fn walk(
    src: &mut dyn ReadSeek,
    mut at: u64,
    file_len: u64,
    first: &[u8; 4],
    free_format: usize,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<u64, ReelError> {
    let mut frames = 0u64;
    loop {
        if frames.is_multiple_of(4096) {
            check_cancel(cancel)?;
        }
        if at.saturating_add(4) > file_len {
            break;
        }
        src.seek(SeekFrom::Start(at))?;
        let mut h = [0u8; 4];
        if read_full(src, &mut h)? < 4 || !headers_compatible(first, &h) {
            break;
        }
        let len = frame_len(&h, free_format) as u64;
        if len < 4 || at.saturating_add(len) > file_len {
            break;
        }
        frames = frames.saturating_add(1);
        if frames > limits.max_samples as u64 {
            return Err(ReelError::LimitExceeded {
                what: "mpeg-audio-frames",
            });
        }
        at = at.saturating_add(len);
    }
    Ok(frames)
}

/// Decoding reader over the frames of an elementary MPEG audio stream.
pub(crate) struct Mp3Stream<'a> {
    src: &'a mut dyn ReadSeek,
    info: Mp3Info,
    window: Vec<u8>,
    pos: usize,
    eof: bool,
    bytes_read: u64,
    decoder: Mp3Decoder,
    free_format: usize,
}

impl<'a> Mp3Stream<'a> {
    pub(crate) fn open(src: &'a mut dyn ReadSeek, info: Mp3Info) -> Result<Self, ReelError> {
        src.seek(SeekFrom::Start(info.frames_offset))?;
        Ok(Self {
            src,
            info,
            window: Vec::new(),
            pos: 0,
            eof: false,
            bytes_read: info.frames_offset,
            decoder: Mp3Decoder::new(),
            free_format: info.free_format,
        })
    }

    fn fill(&mut self, need: usize) -> Result<(), ReelError> {
        if self.pos > 0 {
            self.window.drain(..self.pos);
            self.pos = 0;
        }
        while !self.eof && self.window.len() < need.max(WINDOW / 4) {
            let old = self.window.len();
            self.window.resize(old.saturating_add(WINDOW / 4), 0);
            let n = read_full(self.src, self.window.get_mut(old..).unwrap_or(&mut []))?;
            self.window.truncate(old.saturating_add(n));
            self.bytes_read = self.bytes_read.saturating_add(n as u64);
            if n == 0 {
                self.eof = true;
            }
        }
        Ok(())
    }
}

impl FrameSource for Mp3Stream<'_> {
    fn next_block(
        &mut self,
        out: &mut Vec<f32>,
        cancel: &CancellationToken,
    ) -> Result<Option<usize>, ReelError> {
        check_cancel(cancel)?;
        let available = self.window.len().saturating_sub(self.pos);
        if available < 2 * 2308 {
            self.fill(2 * 2308)?;
        }
        let rest = self.window.get(self.pos..).unwrap_or(&[]);
        let header: Option<[u8; 4]> = rest.get(..4).and_then(|h| h.try_into().ok());
        let mut frame_at = None;
        if let Some(h) = header
            && header_is_valid(&h)
            && headers_compatible(&self.info.first_header, &h)
        {
            let len = frame_len(&h, self.free_format);
            if len >= 4 && len <= rest.len() {
                frame_at = Some((0usize, len));
            }
        }
        if frame_at.is_none() {
            // Resynchronise (or reach the trailing tags at the end of the file).
            if rest.len() < 4 {
                return Ok(None);
            }
            match find_frame(rest, &mut self.free_format) {
                Some((at, len))
                    if rest
                        .get(at..at.saturating_add(4))
                        .and_then(|h| <[u8; 4]>::try_from(h).ok())
                        .is_some_and(|h| headers_compatible(&self.info.first_header, &h)) =>
                {
                    frame_at = Some((at, len));
                }
                _ if self.eof => return Ok(None),
                _ => {
                    return Err(ReelError::Corrupt {
                        what: "mpeg-audio-lost-sync",
                    });
                }
            }
        }
        let Some((at, len)) = frame_at else {
            return Ok(None);
        };
        let start = self.pos.saturating_add(at);
        let frame = self
            .window
            .get(start..start.saturating_add(len))
            .ok_or(ReelError::Truncated { delivered: 0 })?;
        let header: [u8; 4] = frame
            .get(..4)
            .and_then(|h| h.try_into().ok())
            .ok_or(ReelError::Truncated { delivered: 0 })?;
        let before = out.len();
        let frames = self.decoder.decode_frame(frame, out)?;
        let spf = hdr_frame_samples(&header);
        let channels = usize::from(self.info.shape.channels);
        if frames == 0 {
            // Main data from before the stream start: keep the timeline with silence, as the frame is
            // there.
            out.truncate(before);
            out.resize(before.saturating_add(spf.saturating_mul(channels)), 0.0);
        }
        self.pos = start.saturating_add(len);
        Ok(Some(spf))
    }

    fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}
