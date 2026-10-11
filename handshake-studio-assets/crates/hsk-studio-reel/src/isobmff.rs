//! Own ISOBMFF / QuickTime MOV index reader: ftyp, moov, trak, edts/elst, mdhd, hdlr, stsd, stts, ctts,
//! stss, stsz, stsc, stco/co64 -> a per-track sample index with exact decode/presentation times, plus
//! frame-accurate seek (requested vs actual PTS) and bounded packet delivery.
//!
//! Scope: progressive and fragmented (`moof`/`traf`/`tfhd`/`tfdt`/`trun`, `sidx`) files, constant-1.0 edit
//! rate, first sample description only. `stz2` and multi-description tracks are typed `Unsupported`,
//! never guessed. Compressed codec payloads are NOT decoded here: `OutputMode::Decoded` for a compressed
//! sample entry is a determinate `CodecUnavailable` naming the FourCC. Uncompressed PCM audio
//! (`twos`/`sowt`/`in24`/`in32`/`fl32`/`fl64`/`raw `/`lpcm`) decodes to little-endian samples (`pcm`
//! module); `tmcd` timecode tracks expose their raw fields (`timecode` module).
//!
//! Every size, offset, count, duration and index taken from the file uses checked arithmetic and a typed
//! error (V4-01), here and in the `fragments` submodule; `tests/v4_hardening.rs` pushes box sizes,
//! largesize boxes, sample counts, durations, offsets, edit lists and `ctts` to their 32/64-bit limits.
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{ReelError, check_cancel};
use crate::model::*;
use crate::time::media_to_ticks;
use crate::video::{VideoCodec, VideoRoute, VideoTrack};
use hsk_studio_accord::{CancellationToken, Ticks};
use std::io::SeekFrom;

mod fragments;
mod pcm;
mod timecode;

pub use fragments::{SegmentIndex, SegmentIndexEntry};
pub(crate) use pcm::deliver_pcm_stream;
pub use pcm::{PcmFormat, PcmStream, PcmWindow, pcm_format};
pub use timecode::{TimecodeDescription, TimecodeStart, read_timecode_start};

pub const ISOBMFF_PROVIDER_ID: &str = "isobmff";
const DECODER_VERSION: &str = "1";
const MAX_TOP_LEVEL_BOXES: u32 = 65_536;

#[derive(Debug, Clone, Copy)]
pub struct IsobmffProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
    Timecode,
    Other,
}

/// One coded sample in decode order. Times are in the track's media timescale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleEntry {
    pub dts: i64,
    /// Composition time = dts + ctts offset (may be negative before an edit-list shift).
    pub pts: i64,
    pub duration: u32,
    pub offset: u64,
    pub size: u32,
    pub sync: bool,
}

/// Colour tagging carried by the sample entry (`colr`). Absence is `Untagged`: consumers must record an
/// `assumed(rule)` instead of silently guessing primaries, transfer, matrix or range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorTag {
    Untagged,
    /// ITU-T H.273 code points (QuickTime `nclc` carries no range flag: `full_range == None`).
    Nclx {
        primaries: u16,
        transfer: u16,
        matrix: u16,
        full_range: Option<bool>,
    },
    /// Embedded ICC profile (`prof` / `rICC` / `ricc`); only its size is indexed.
    IccProfile {
        bytes: u32,
    },
}

/// Field/frame information carried by the sample entry (`fiel`), stored RAW: the two bytes as written,
/// no interpretation. Apple TN2162 defines `fields` (1 progressive, 2 interlaced) and `detail` (1, 6, 9,
/// 14 for interlaced), but the direction of detail 9 and 14 is disputed (older QuickTime File Format
/// text vs TN2162, reported reversed by an Apple engineer), so reel ships no field-order claim: a
/// consumer that needs one uses the codec's own header first and treats this as a container-level
/// fallback. Advisory metadata: a malformed or absent box does not fail a track (test
/// `fiel_and_clap_are_raw_and_advisory`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldInfo {
    pub fields: u8,
    pub detail: u8,
}

/// Clean aperture carried by the sample entry (`clap`), stored RAW as (numerator, denominator) pairs,
/// the eight signed 32-bit fields of the QuickTime File Format clean aperture atom. No division happens
/// in reel: a zero denominator is stored as read and left to the consumer. Advisory metadata: a
/// truncated or absent box does not fail a track (test `fiel_and_clap_are_raw_and_advisory`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanAperture {
    pub width: (i32, i32),
    pub height: (i32, i32),
    pub horiz_off: (i32, i32),
    pub vert_off: (i32, i32),
}

/// QuickTime sound sample description fields (versions 0, 1 and 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    pub channels: u16,
    pub sample_size: u16,
    /// Integral sample rate; `0` when a version 2 float64 rate is not a positive integer.
    pub sample_rate: u32,
    pub version: u16,
    pub compression_id: u16,
    /// Version 2 (`lpcm`) fields.
    pub lpcm: Option<LpcmDescription>,
}

/// Version 2 sound description fields that define linear PCM layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LpcmDescription {
    pub bits_per_channel: u32,
    /// kAudioFormatFlag* bits: 0x1 float, 0x2 big-endian, 0x4 signed, 0x8 packed, 0x20 non-interleaved.
    pub flags: u32,
    pub bytes_per_packet: u32,
    pub frames_per_packet: u32,
}

/// One edit-list segment on the presentation timeline (ticks). `media_time == None` is an empty edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditSegment {
    pub presentation_start: i64,
    pub duration: i64,
    pub media_time: Option<i64>,
}

/// A sample as shown on the presentation timeline, in ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentedSample {
    pub sample: u32,
    pub pts: i64,
    pub duration: u64,
    pub edit: u32,
    /// Ticks of this sample that precede the edit's `media_time` (an unaligned edit start): `pts` is
    /// clamped to the edit start and decoding begins at this containing sample.
    pub trim_start: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeekPoint {
    pub sample: u32,
    pub pts: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekDisposition {
    /// The requested time is exactly a presented sample's PTS.
    Exact,
    /// The requested time falls inside the displayed interval of the returned (earlier) sample.
    Containing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeekResult {
    pub requested: Ticks,
    /// The frame shown at the requested time.
    pub target: SeekPoint,
    pub presented_index: usize,
    /// First sample to feed a decoder (nearest earlier sync sample, conservative for open GOP).
    pub decode_from: SeekPoint,
    /// Decode-order samples from `decode_from` through `target` inclusive.
    pub samples_to_decode: u32,
    /// Leading ticks of the target sample before the presented start (see `PresentedSample`).
    pub trim_start: u64,
    pub disposition: SeekDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackIndex {
    pub track_id: u32,
    pub kind: TrackKind,
    pub handler: [u8; 4],
    pub enabled: bool,
    pub media_timescale: u32,
    pub media_duration: u64,
    /// First sample-entry FourCC (codec identity); `[0; 4]` when absent.
    pub codec: [u8; 4],
    pub width: u32,
    pub height: u32,
    pub audio: Option<AudioFormat>,
    pub color: ColorTag,
    /// `pasp` pixel aspect ratio (horizontal, vertical spacing).
    pub pixel_aspect: Option<(u32, u32)>,
    /// Codec configuration record of the first sample entry (`av1C` payload); empty when absent or when
    /// the container reader has none.
    pub codec_config: Vec<u8>,
    /// `fiel` field/frame information of the first sample entry, raw (see `FieldInfo`).
    pub field_info: Option<FieldInfo>,
    /// `clap` clean aperture of the first sample entry, raw (see `CleanAperture`).
    pub clean_aperture: Option<CleanAperture>,
    pub edits: Vec<EditSegment>,
    /// False when any media-time to tick conversion on this track needed rounding.
    pub ticks_exact: bool,
    /// `tmcd` sample-entry fields when this is a timecode track.
    pub timecode: Option<TimecodeDescription>,
    /// Track id of the `tmcd` track this track references through `tref`.
    pub timecode_ref: Option<u32>,
    pcm: Option<PcmStream>,
    samples: Vec<SampleEntry>,
    presented: Vec<PresentedSample>,
    sync_points: Vec<u32>,
    all_sync: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaIndex {
    pub major_brand: [u8; 4],
    pub movie_timescale: u32,
    pub tracks: Vec<TrackIndex>,
    /// Bytes read to build the index (box headers + moov + every moof/sidx).
    pub bytes_read: u64,
    /// Length of the source the index was built from; delivery re-checks it.
    pub source_len: u64,
    /// `mvex` present: samples come from movie fragments.
    pub fragmented: bool,
    /// Absolute offset of every `moof` found (fragmented files).
    pub fragment_offsets: Vec<u64>,
    /// `sidx` boxes, with absolute reference offsets.
    pub segment_indexes: Vec<SegmentIndex>,
    /// A fragment, `mdat` or `moof` ran past the end of the file: recording cut short.
    pub truncated_tail: bool,
}

impl MediaIndex {
    /// Approximate resident bytes (cache accounting).
    pub fn approx_bytes(&self) -> usize {
        let part = |len: usize, each: usize| len.saturating_mul(each);
        let tracks = self.tracks.iter().fold(0usize, |total, t| {
            total
                .saturating_add(part(t.samples.len(), std::mem::size_of::<SampleEntry>()))
                .saturating_add(part(
                    t.presented.len(),
                    std::mem::size_of::<PresentedSample>(),
                ))
                .saturating_add(part(t.sync_points.len(), 4))
                .saturating_add(t.pcm.as_ref().map_or(0, PcmStream::approx_bytes))
        });
        let segments = self.segment_indexes.iter().fold(0usize, |total, s| {
            total.saturating_add(part(
                s.entries.len(),
                std::mem::size_of::<SegmentIndexEntry>(),
            ))
        });
        // An estimate for cache accounting: a total past usize::MAX saturates (it is "too big"), it
        // never wraps to a small number.
        tracks
            .saturating_add(part(self.fragment_offsets.len(), 8))
            .saturating_add(segments)
    }

    pub fn track(&self, id: u32) -> Option<&TrackIndex> {
        self.tracks.iter().find(|t| t.track_id == id)
    }
    /// First enabled-or-not video track, else the first audio track.
    pub fn default_track(&self) -> Option<&TrackIndex> {
        self.tracks
            .iter()
            .find(|t| t.kind == TrackKind::Video)
            .or_else(|| self.tracks.iter().find(|t| t.kind == TrackKind::Audio))
    }
}

/// Track header facts supplied by a container reader other than ISOBMFF.
pub(crate) struct ExternalTrack {
    pub track_id: u32,
    pub kind: TrackKind,
    pub handler: [u8; 4],
    pub enabled: bool,
    pub media_timescale: u32,
    pub media_duration: u64,
    pub width: u32,
    pub height: u32,
    pub audio: Option<AudioFormat>,
}

impl TrackIndex {
    /// Assemble a track from samples produced by another container reader (Matroska). The timeline is
    /// the identity timeline (no edit list) and `dts` is whatever the reader could know (often `pts`).
    pub(crate) fn from_external_samples(
        parts: ExternalTrack,
        samples: Vec<SampleEntry>,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> Result<Self, ReelError> {
        let sync_points: Vec<u32> = samples
            .iter()
            .enumerate()
            .filter(|(_, s)| s.sync)
            .map(|(i, _)| u32::try_from(i))
            .collect::<Result<_, _>>()
            .map_err(|_| ReelError::LimitExceeded {
                what: "sample-count",
            })?;
        let all_sync = sync_points.len() == samples.len();
        let mut track = TrackIndex {
            track_id: parts.track_id,
            kind: parts.kind,
            handler: parts.handler,
            enabled: parts.enabled,
            media_timescale: parts.media_timescale,
            media_duration: parts.media_duration,
            codec: [0; 4],
            width: parts.width,
            height: parts.height,
            audio: parts.audio,
            color: ColorTag::Untagged,
            pixel_aspect: None,
            codec_config: Vec::new(),
            field_info: None,
            clean_aperture: None,
            edits: Vec::new(),
            ticks_exact: true,
            timecode: None,
            timecode_ref: None,
            pcm: None,
            samples,
            presented: Vec::new(),
            sync_points: if all_sync { Vec::new() } else { sync_points },
            all_sync,
        };
        build_edits(&mut track, &[], 0, false)?;
        build_presentation(&mut track, limits, cancel)?;
        Ok(track)
    }
}

impl TrackIndex {
    /// Compact PCM representation (audio tracks of uncompressed PCM with a regular sample table).
    pub fn pcm(&self) -> Option<&PcmStream> {
        self.pcm.as_ref()
    }
    pub fn samples(&self) -> &[SampleEntry] {
        &self.samples
    }
    pub fn presented(&self) -> &[PresentedSample] {
        &self.presented
    }
    pub fn is_all_sync(&self) -> bool {
        self.all_sync
    }
    pub fn sync_samples(&self) -> &[u32] {
        &self.sync_points
    }

    fn pts_in_edit(&self, edit: u32, media_pts: i64) -> Result<(i64, bool), ReelError> {
        let e = self.edits[edit as usize];
        let base = e.media_time.unwrap_or(0);
        let delta = media_pts
            .checked_sub(base)
            .ok_or(ReelError::LimitExceeded { what: "tick-range" })?;
        let t = media_to_ticks(delta, self.media_timescale)?;
        let pts = e
            .presentation_start
            .checked_add(t.value)
            .ok_or(ReelError::LimitExceeded { what: "tick-range" })?;
        Ok((pts, t.exact))
    }

    fn presentation_bounds(&self) -> (i64, i64) {
        match (self.presented.first(), self.presented.last()) {
            (Some(first), Some(last)) => (
                first.pts,
                last.pts
                    .saturating_add(i64::try_from(last.duration).unwrap_or(i64::MAX)),
            ),
            _ => (0, 0),
        }
    }

    fn sync_for(&self, target: u32) -> Result<u32, ReelError> {
        if self.all_sync {
            return Ok(target);
        }
        let k = self.sync_points.partition_point(|&s| s <= target);
        let Some(last) = k.checked_sub(1) else {
            return Err(ReelError::Corrupt {
                what: "no-sync-sample-before-target",
            });
        };
        let mut start = self.sync_points[last];
        // Open-GOP leading picture: displayed before its sync sample -> start one GOP earlier.
        if self.samples[start as usize].pts > self.samples[target as usize].pts
            && let Some(earlier) = last.checked_sub(1)
        {
            start = self.sync_points[earlier];
        }
        Ok(start)
    }

    /// Frame-accurate lookup: the sample shown at `requested`, and where decoding must start.
    /// A time outside the presented range (before the first frame, in an edit gap, at or after the end)
    /// is `OutOfRange`; it is never clamped.
    pub fn seek(&self, requested: Ticks) -> Result<SeekResult, ReelError> {
        let (first, end) = self.presentation_bounds();
        let out_of_range = || ReelError::OutOfRange {
            requested,
            first,
            end,
        };
        let r = i128::from(requested.value());
        let k = self.presented.partition_point(|p| i128::from(p.pts) <= r);
        let Some(shown) = k.checked_sub(1) else {
            return Err(out_of_range());
        };
        let p = self.presented[shown];
        // i64 + u64 always fits i128; saturating keeps the lint's contract without a branch.
        if r >= i128::from(p.pts).saturating_add(i128::from(p.duration)) {
            return Err(out_of_range());
        }
        let start = self.sync_for(p.sample)?;
        let start_pts = self
            .pts_in_edit(p.edit, self.samples[start as usize].pts)?
            .0;
        let samples_to_decode = p
            .sample
            .checked_sub(start)
            .and_then(|span| span.checked_add(1))
            .ok_or(ReelError::Corrupt {
                what: "decode-start-after-target",
            })?;
        Ok(SeekResult {
            requested,
            target: SeekPoint {
                sample: p.sample,
                pts: p.pts,
            },
            presented_index: shown,
            decode_from: SeekPoint {
                sample: start,
                pts: start_pts,
            },
            samples_to_decode,
            trim_start: p.trim_start,
            disposition: if r == i128::from(p.pts) && p.trim_start == 0 {
                SeekDisposition::Exact
            } else {
                SeekDisposition::Containing
            },
        })
    }
}

pub fn fourcc_string(code: [u8; 4]) -> String {
    if code.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        String::from_utf8_lossy(&code).into_owned()
    } else {
        format!(
            "0x{:02x}{:02x}{:02x}{:02x}",
            code[0], code[1], code[2], code[3]
        )
    }
}

// ---- box parsing ---------------------------------------------------------------------------------

struct Boxes<'a> {
    data: &'a [u8],
    pos: usize,
    failed: bool,
}

fn boxes(data: &[u8]) -> Boxes<'_> {
    Boxes {
        data,
        pos: 0,
        failed: false,
    }
}

impl<'a> Iterator for Boxes<'a> {
    type Item = Result<([u8; 4], &'a [u8]), ReelError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.pos >= self.data.len() {
            return None;
        }
        let data: &'a [u8] = self.data;
        let rest: &'a [u8] = &data[self.pos..];
        let fail = |this: &mut Self, what| {
            this.failed = true;
            Some(Err(ReelError::Corrupt { what }))
        };
        if rest.len() < 8 {
            return fail(self, "box-header");
        }
        let size32 = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]);
        let kind = [rest[4], rest[5], rest[6], rest[7]];
        let (header, size) = match size32 {
            0 => (8usize, rest.len() as u64),
            1 => {
                if rest.len() < 16 {
                    return fail(self, "box-largesize");
                }
                let mut b = [0u8; 8];
                b.copy_from_slice(&rest[8..16]);
                (16usize, u64::from_be_bytes(b))
            }
            n => (8usize, u64::from(n)),
        };
        // `uuid` boxes carry a 16-byte extended type after the header. A size that does not fit the
        // remaining bytes (a 64-bit largesize can be anything) is a corrupt box, never a wrapped slice.
        let layout = (if kind == *b"uuid" {
            header.checked_add(16)
        } else {
            Some(header)
        })
        .and_then(|header| {
            let size = usize::try_from(size).ok()?;
            (size >= header && size <= rest.len()).then_some((header, size))
        });
        let Some((header, size)) = layout else {
            return fail(self, "box-size");
        };
        let Some(next) = self.pos.checked_add(size) else {
            return fail(self, "box-size");
        };
        let payload = &rest[header..size];
        self.pos = next;
        Some(Ok((kind, payload)))
    }
}

fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Result<Option<&'a [u8]>, ReelError> {
    for item in boxes(data) {
        let (k, payload) = item?;
        if &k == kind {
            return Ok(Some(payload));
        }
    }
    Ok(None)
}

struct Cur<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cur<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], ReelError> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.data.len())
            .ok_or(ReelError::Corrupt {
                what: "box-truncated",
            })?;
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn skip(&mut self, n: usize) -> Result<(), ReelError> {
        self.take(n).map(|_| ())
    }
    fn u16(&mut self) -> Result<u16, ReelError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, ReelError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn i32(&mut self) -> Result<i32, ReelError> {
        self.u32().map(|v| v as i32)
    }
    fn u64(&mut self) -> Result<u64, ReelError> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_be_bytes(a))
    }
    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }
}

/// Full-box header: (version, flags, cursor over the rest).
fn full(payload: &[u8]) -> Result<(u8, u32, Cur<'_>), ReelError> {
    let mut c = Cur::new(payload);
    let vf = c.u32()?;
    Ok(((vf >> 24) as u8, vf & 0x00FF_FFFF, c))
}

/// Count-prefixed table: returns (count, cursor) with `count * entry_bytes <= remaining` verified.
fn table(payload: &[u8], entry_bytes: usize) -> Result<(usize, Cur<'_>), ReelError> {
    let (_, _, mut c) = full(payload)?;
    let count = c.u32()? as usize;
    let need = count
        .checked_mul(entry_bytes)
        .ok_or(ReelError::Corrupt { what: "table-size" })?;
    if need > c.remaining() {
        return Err(ReelError::Corrupt {
            what: "table-exceeds-box",
        });
    }
    Ok((count, c))
}

/// (timescale, duration) of mvhd / mdhd.
fn header_time(payload: &[u8]) -> Result<(u32, u64), ReelError> {
    let (version, _, mut c) = full(payload)?;
    if version == 1 {
        c.skip(16)?;
        let ts = c.u32()?;
        Ok((ts, c.u64()?))
    } else {
        c.skip(8)?;
        let ts = c.u32()?;
        Ok((ts, u64::from(c.u32()?)))
    }
}

struct Stbl<'a> {
    stsd: &'a [u8],
    stts: &'a [u8],
    ctts: Option<&'a [u8]>,
    stss: Option<&'a [u8]>,
    stsz: &'a [u8],
    stsc: &'a [u8],
    chunk_offsets: Offsets<'a>,
}

enum Offsets<'a> {
    Co32(&'a [u8]),
    Co64(&'a [u8]),
}

static EMPTY_TABLE: [u8; 8] = [0; 8];
static EMPTY_STSZ: [u8; 12] = [0; 12];

/// `fragmented`: a movie with `mvex` may omit the (empty) sample tables; absent tables are empty.
fn parse_stbl(stbl: &[u8], fragmented: bool) -> Result<Stbl<'_>, ReelError> {
    let (mut stsd, mut stts, mut ctts, mut stss, mut stsz, mut stsc) =
        (None, None, None, None, None, None);
    let mut offsets = None;
    for item in boxes(stbl) {
        let (kind, payload) = item?;
        match &kind {
            b"stsd" => stsd = Some(payload),
            b"stts" => stts = Some(payload),
            b"ctts" => ctts = Some(payload),
            b"stss" => stss = Some(payload),
            b"stsz" => stsz = Some(payload),
            b"stz2" => {
                return Err(ReelError::Unsupported {
                    what: "compact-sample-size-stz2",
                });
            }
            b"stsc" => stsc = Some(payload),
            b"stco" => offsets = Some(Offsets::Co32(payload)),
            b"co64" => offsets = Some(Offsets::Co64(payload)),
            _ => {}
        }
    }
    let missing = |what| ReelError::Corrupt { what };
    fn or_empty<'a>(
        value: Option<&'a [u8]>,
        empty: &'static [u8],
        fragmented: bool,
        what: &'static str,
    ) -> Result<&'a [u8], ReelError> {
        match value {
            Some(payload) => Ok(payload),
            None if fragmented => Ok(empty),
            None => Err(ReelError::Corrupt { what }),
        }
    }
    Ok(Stbl {
        stsd: stsd.ok_or(missing("stsd-missing"))?,
        stts: or_empty(stts, &EMPTY_TABLE, fragmented, "stts-missing")?,
        ctts,
        stss,
        stsz: or_empty(stsz, &EMPTY_STSZ, fragmented, "stsz-missing")?,
        stsc: or_empty(stsc, &EMPTY_TABLE, fragmented, "stsc-missing")?,
        chunk_offsets: match offsets {
            Some(o) => o,
            None if fragmented => Offsets::Co32(&EMPTY_TABLE),
            None => return Err(missing("stco-missing")),
        },
    })
}

struct SampleDescription {
    codec: [u8; 4],
    width: u32,
    height: u32,
    audio: Option<AudioFormat>,
    color: ColorTag,
    pixel_aspect: Option<(u32, u32)>,
    codec_config: Vec<u8>,
    field_info: Option<FieldInfo>,
    clean_aperture: Option<CleanAperture>,
    timecode: Option<TimecodeDescription>,
}

/// Largest codec configuration record kept (`av1C` carries one sequence header, tens of bytes).
const MAX_CODEC_CONFIG: usize = 64 * 1024;

fn be16(bytes: &[u8], at: usize) -> Option<u16> {
    bytes
        .get(at..)?
        .first_chunk::<2>()
        .map(|b| u16::from_be_bytes(*b))
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..)?
        .first_chunk::<4>()
        .map(|b| u32::from_be_bytes(*b))
}

/// Advisory sample-entry children of a visual sample entry.
#[derive(Default)]
struct VisualChildren {
    color: Option<ColorTag>,
    pixel_aspect: Option<(u32, u32)>,
    codec_config: Vec<u8>,
    field_info: Option<FieldInfo>,
    clean_aperture: Option<CleanAperture>,
}

/// Payload bytes of a `clap` box: eight 32-bit fields.
const CLAP_BYTES: usize = 32;

/// Sample-entry child boxes (`colr`, `pasp`, `av1C`, `fiel`, `clap`). A malformed child ends the scan:
/// metadata is advisory and does not fail an otherwise indexable track. A `fiel` or `clap` shorter
/// than its layout is ignored (the field stays `None`) and is not read past its payload (test
/// `fiel_and_clap_are_raw_and_advisory`).
fn parse_visual_children(rest: &[u8]) -> VisualChildren {
    let mut out = VisualChildren::default();
    for item in boxes(rest) {
        let Ok((kind, payload)) = item else { break };
        match &kind {
            b"colr" if payload.len() >= 4 => match &payload[..4] {
                b"nclx" | b"nclc" if payload.len() >= 10 => {
                    let (Some(primaries), Some(transfer), Some(matrix)) =
                        (be16(payload, 4), be16(payload, 6), be16(payload, 8))
                    else {
                        break;
                    };
                    out.color = Some(ColorTag::Nclx {
                        primaries,
                        transfer,
                        matrix,
                        full_range: (&payload[..4] == b"nclx" && payload.len() >= 11)
                            .then(|| payload[10] & 0x80 != 0),
                    });
                }
                b"prof" | b"rICC" | b"ricc" => {
                    // A profile too large for the u32 size field cannot be a sample-entry child:
                    // left untagged (advisory metadata), never a truncated size.
                    if let Ok(bytes) = u32::try_from(payload.len().saturating_sub(4)) {
                        out.color = Some(ColorTag::IccProfile { bytes });
                    }
                }
                _ => {}
            },
            b"pasp" if payload.len() >= 8 => {
                let (Some(horizontal), Some(vertical)) = (be32(payload, 0), be32(payload, 4))
                else {
                    break;
                };
                if horizontal > 0 && vertical > 0 {
                    out.pixel_aspect = Some((horizontal, vertical));
                }
            }
            b"av1C" if payload.len() <= MAX_CODEC_CONFIG => out.codec_config = payload.to_vec(),
            b"fiel" if payload.len() >= 2 => {
                out.field_info = Some(FieldInfo {
                    fields: payload[0],
                    detail: payload[1],
                });
            }
            b"clap" if payload.len() >= CLAP_BYTES => {
                let field = |at: usize| be32(payload, at).map(|v| v as i32);
                let pair = |at: usize| -> Option<(i32, i32)> {
                    Some((field(at)?, field(at.checked_add(4)?)?))
                };
                if let (Some(width), Some(height), Some(horiz_off), Some(vert_off)) =
                    (pair(0), pair(8), pair(16), pair(24))
                {
                    out.clean_aperture = Some(CleanAperture {
                        width,
                        height,
                        horiz_off,
                        vert_off,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

fn parse_stsd(payload: &[u8], kind: TrackKind) -> Result<SampleDescription, ReelError> {
    let (_, _, mut c) = full(payload)?;
    let entries = c.u32()?;
    if entries == 0 {
        return Ok(SampleDescription {
            codec: [0; 4],
            width: 0,
            height: 0,
            audio: None,
            color: ColorTag::Untagged,
            pixel_aspect: None,
            codec_config: Vec::new(),
            field_info: None,
            clean_aperture: None,
            timecode: None,
        });
    }
    let size = c.u32()? as usize;
    let k = c.take(4)?;
    let codec = [k[0], k[1], k[2], k[3]];
    let body_len =
        size.checked_sub(8)
            .filter(|l| *l <= c.remaining())
            .ok_or(ReelError::Corrupt {
                what: "sample-entry-size",
            })?;
    let mut e = Cur::new(c.take(body_len)?);
    e.skip(8)?; // reserved[6] + data_reference_index
    let (mut width, mut height, mut audio) = (0, 0, None);
    let (mut color, mut pixel_aspect) = (ColorTag::Untagged, None);
    let (mut timecode, mut codec_config) = (None, Vec::new());
    let (mut field_info, mut clean_aperture) = (None, None);
    match kind {
        TrackKind::Video => {
            e.skip(16)?;
            width = u32::from(e.u16()?);
            height = u32::from(e.u16()?);
            // hres, vres, reserved, frame_count, compressorname[32], depth, pre_defined
            e.skip(50)?;
            let remaining = e.remaining();
            let children = parse_visual_children(e.take(remaining)?);
            color = children.color.unwrap_or(ColorTag::Untagged);
            (pixel_aspect, codec_config) = (children.pixel_aspect, children.codec_config);
            (field_info, clean_aperture) = (children.field_info, children.clean_aperture);
        }
        TrackKind::Audio => {
            let version = e.u16()?;
            if version <= 1 {
                e.skip(6)?; // revision + vendor
                let channels = e.u16()?;
                let sample_size = e.u16()?;
                let compression_id = e.u16()?;
                e.skip(2)?; // packet size
                let rate = e.u32()? >> 16;
                audio = Some(AudioFormat {
                    channels,
                    sample_size,
                    sample_rate: rate,
                    version,
                    compression_id,
                    lpcm: None,
                });
            } else if version == 2 {
                // revision, vendor, always3, always16, alwaysMinus2, always0, always65536, sizeOfStructOnly
                e.skip(6 + 8 + 4 + 4)?;
                let float_rate = f64::from_bits(e.u64()?);
                let channels = e.u32()?;
                e.skip(4)?; // always 0x7F000000
                let bits_per_channel = e.u32()?;
                let flags = e.u32()?;
                let bytes_per_packet = e.u32()?;
                let frames_per_packet = e.u32()?;
                let integral = float_rate.is_finite()
                    && float_rate >= 1.0
                    && float_rate <= f64::from(u32::MAX)
                    && float_rate.fract() == 0.0;
                audio = Some(AudioFormat {
                    channels: u16::try_from(channels).unwrap_or(0),
                    sample_size: u16::try_from(bits_per_channel).unwrap_or(0),
                    sample_rate: if integral { float_rate as u32 } else { 0 },
                    version,
                    compression_id: 0,
                    lpcm: Some(LpcmDescription {
                        bits_per_channel,
                        flags,
                        bytes_per_packet,
                        frames_per_packet,
                    }),
                });
            } else {
                return Err(ReelError::Unsupported {
                    what: "quicktime-sound-version",
                });
            }
        }
        TrackKind::Timecode => timecode = Some(timecode::parse_tmcd(&mut e)?),
        TrackKind::Other => {}
    }
    Ok(SampleDescription {
        codec,
        width,
        height,
        audio,
        color,
        pixel_aspect,
        codec_config,
        field_info,
        clean_aperture,
        timecode,
    })
}

pub(crate) struct Budget {
    pub(crate) samples_left: usize,
}

/// Samples the chunk tables can address: for each `stsc` run (1-based `first_chunk`, samples per
/// chunk) the chunks it covers (up to the next run or the last chunk) times its samples per chunk.
/// Saturating on purpose: this is a lower bound compared against a sample count, so a saturated total
/// still answers "enough".
fn addressable_samples(stsc: &[(u32, u32)], chunks: usize) -> u64 {
    let end_of_chunks = u64::try_from(chunks).map_or(u64::MAX, |c| c.saturating_add(1));
    let mut total = 0u64;
    for (i, (first, per_chunk)) in stsc.iter().enumerate() {
        let run_end = stsc
            .get(i.saturating_add(1))
            .map_or(end_of_chunks, |next| u64::from(next.0).min(end_of_chunks));
        let run = run_end.saturating_sub(u64::from(*first));
        total = total.saturating_add(run.saturating_mul(u64::from(*per_chunk)));
    }
    total
}

/// Expand the sample tables into per-sample entries (decode order).
#[allow(clippy::too_many_arguments)]
fn expand_samples(
    n: usize,
    sizes: &[u32],
    stts: &[(u32, u32)],
    ctts: &[(u32, i32)],
    sync_points: &[u32],
    all_sync: bool,
    stsc: &[(u32, u32)],
    chunk_offsets: &[u64],
    cancel: &CancellationToken,
) -> Result<Vec<SampleEntry>, ReelError> {
    let missing = |what| ReelError::Corrupt { what };
    let mut samples = Vec::with_capacity(n);
    let (mut stts_i, mut stts_left) = (0usize, stts.first().map_or(0, |e| e.0));
    let (mut ctts_i, mut ctts_left) = (0usize, ctts.first().map_or(0, |e| e.0));
    let (mut sync_i, mut dts) = (0usize, 0i64);
    let (mut stsc_i, mut index) = (0usize, 0usize);
    for (chunk, base) in chunk_offsets.iter().enumerate() {
        if index == n {
            break;
        }
        // Advance to the last stsc run that starts at or before this (1-based) chunk.
        while let Some(next_run) = stsc.get(stsc_i.saturating_add(1)) {
            if usize::try_from(next_run.0).is_ok_and(|first| first <= chunk.saturating_add(1)) {
                stsc_i = stsc_i.saturating_add(1);
            } else {
                break;
            }
        }
        let per_chunk = stsc.get(stsc_i).map_or(0, |e| e.1);
        let mut offset = *base;
        for _ in 0..per_chunk {
            if index == n {
                return Err(missing("stsc-exceeds-stsz"));
            }
            if index % 65_536 == 0 {
                check_cancel(cancel)?;
            }
            while stts_left == 0 {
                stts_i = stts_i.saturating_add(1);
                stts_left = stts.get(stts_i).ok_or(missing("stts-short"))?.0;
            }
            let duration = stts[stts_i].1;
            stts_left = stts_left.checked_sub(1).ok_or(missing("stts-short"))?;
            let offset_ctts = if ctts.is_empty() {
                0
            } else {
                while ctts_left == 0 {
                    ctts_i = ctts_i.saturating_add(1);
                    ctts_left = ctts.get(ctts_i).ok_or(missing("ctts-short"))?.0;
                }
                ctts_left = ctts_left.checked_sub(1).ok_or(missing("ctts-short"))?;
                ctts[ctts_i].1
            };
            let size = sizes[index];
            let sync = if all_sync {
                true
            } else if u32::try_from(index).is_ok_and(|i| sync_points.get(sync_i) == Some(&i)) {
                sync_i = sync_i.saturating_add(1);
                true
            } else {
                false
            };
            samples.push(SampleEntry {
                dts,
                pts: dts
                    .checked_add(i64::from(offset_ctts))
                    .ok_or(ReelError::LimitExceeded { what: "time-range" })?,
                duration,
                offset,
                size,
                sync,
            });
            dts = dts
                .checked_add(i64::from(duration))
                .ok_or(ReelError::LimitExceeded { what: "time-range" })?;
            offset = offset
                .checked_add(u64::from(size))
                .ok_or(missing("chunk-offset-overflow"))?;
            index = index.saturating_add(1);
        }
    }
    if index != n {
        return Err(missing("stsc-short"));
    }
    Ok(samples)
}

/// Read one `trak`'s sample tables into a track (no edit list, no presentation yet).
#[allow(clippy::too_many_lines)]
fn track_from_tables(
    trak: &[u8],
    limits: &Limits,
    budget: &mut Budget,
    cancel: &CancellationToken,
    fragmented: bool,
) -> Result<TrackIndex, ReelError> {
    let missing = |what| ReelError::Corrupt { what };
    let tkhd = child(trak, b"tkhd")?.ok_or(missing("tkhd-missing"))?;
    let (version, flags, mut c) = full(tkhd)?;
    let track_id = if version == 1 {
        c.skip(16)?;
        c.u32()?
    } else {
        c.skip(8)?;
        c.u32()?
    };
    let timecode_ref = match child(trak, b"tref")? {
        Some(tref) => match child(tref, b"tmcd")? {
            Some(ids) if ids.len() >= 4 => {
                Some(u32::from_be_bytes([ids[0], ids[1], ids[2], ids[3]]))
            }
            _ => None,
        },
        None => None,
    };
    let mdia = child(trak, b"mdia")?.ok_or(missing("mdia-missing"))?;
    let (media_timescale, media_duration) =
        header_time(child(mdia, b"mdhd")?.ok_or(missing("mdhd-missing"))?)?;
    if media_timescale == 0 {
        return Err(missing("zero-media-timescale"));
    }
    let hdlr = child(mdia, b"hdlr")?.ok_or(missing("hdlr-missing"))?;
    let (_, _, mut h) = full(hdlr)?;
    h.skip(4)?;
    let handler = {
        let b = h.take(4)?;
        [b[0], b[1], b[2], b[3]]
    };
    let kind = match &handler {
        b"vide" => TrackKind::Video,
        b"soun" => TrackKind::Audio,
        b"tmcd" => TrackKind::Timecode,
        _ => TrackKind::Other,
    };
    let minf = child(mdia, b"minf")?.ok_or(missing("minf-missing"))?;
    let stbl_payload = child(minf, b"stbl")?.ok_or(missing("stbl-missing"))?;
    let stbl = parse_stbl(stbl_payload, fragmented)?;
    let desc = parse_stsd(stbl.stsd, kind)?;

    // stsz header: constant sample size (or table) and sample count.
    let (_, _, mut z) = full(stbl.stsz)?;
    let fixed_size = z.u32()?;
    let n = z.u32()? as usize;

    // stts: decode durations; the total must equal the sample count.
    let (entries, mut t) = table(stbl.stts, 8)?;
    let mut stts = Vec::with_capacity(entries);
    let mut total = 0u64;
    for _ in 0..entries {
        let (count, delta) = (t.u32()?, t.u32()?);
        // A running total past u64 can never equal a 32-bit sample count: the same mismatch.
        total = total
            .checked_add(u64::from(count))
            .ok_or(missing("stts-count-mismatch"))?;
        stts.push((count, delta));
    }
    if total != n as u64 {
        return Err(missing("stts-count-mismatch"));
    }

    // ctts: composition offsets (version 0 read as signed: QuickTime writers emit negatives).
    let mut ctts: Vec<(u32, i32)> = Vec::new();
    if let Some(payload) = stbl.ctts {
        let (entries, mut t) = table(payload, 8)?;
        let mut total = 0u64;
        for _ in 0..entries {
            let (count, offset) = (t.u32()?, t.i32()?);
            total = total
                .checked_add(u64::from(count))
                .ok_or(missing("ctts-count-mismatch"))?;
            ctts.push((count, offset));
        }
        if total != n as u64 {
            return Err(missing("ctts-count-mismatch"));
        }
    }

    // stss: 1-based ascending sync sample numbers.
    let mut sync_points: Vec<u32> = Vec::new();
    let all_sync = stbl.stss.is_none();
    if let Some(payload) = stbl.stss {
        let (entries, mut t) = table(payload, 4)?;
        let mut prev = 0u32;
        for _ in 0..entries {
            let number = t.u32()?;
            let zero_based = number.checked_sub(1).ok_or(missing("stss-order"))?;
            if number <= prev || number as usize > n {
                return Err(missing("stss-order"));
            }
            prev = number;
            sync_points.push(zero_based);
        }
    }

    // stsc + chunk offsets.
    let (stsc_entries, mut s) = table(stbl.stsc, 12)?;
    let mut stsc = Vec::with_capacity(stsc_entries);
    for _ in 0..stsc_entries {
        let (first, per_chunk, desc_index) = (s.u32()?, s.u32()?, s.u32()?);
        if desc_index != 1 {
            return Err(ReelError::Unsupported {
                what: "multiple-sample-descriptions",
            });
        }
        stsc.push((first, per_chunk));
    }
    if n > 0 && (stsc.first().map(|e| e.0) != Some(1) || stsc.windows(2).any(|w| w[0].0 >= w[1].0))
    {
        return Err(missing("stsc-order"));
    }
    let chunk_offsets: Vec<u64> = match stbl.chunk_offsets {
        Offsets::Co32(p) => {
            let (count, mut t) = table(p, 4)?;
            (0..count)
                .map(|_| t.u32().map(u64::from))
                .collect::<Result<_, _>>()?
        }
        Offsets::Co64(p) => {
            let (count, mut t) = table(p, 8)?;
            (0..count).map(|_| t.u64()).collect::<Result<_, _>>()?
        }
    };

    // Regular PCM audio is kept as chunk runs: never one entry per audio frame.
    let pcm = if kind == TrackKind::Audio && fixed_size != 0 && n > 0 && ctts.is_empty() && all_sync
    {
        pcm::try_compact(
            pcm::CompactInput {
                codec: desc.codec,
                audio: desc.audio,
                fixed_size,
                samples: n,
                stts: &stts,
                stsc: &stsc,
                chunk_offsets: &chunk_offsets,
                media_timescale,
            },
            limits,
            budget,
        )?
    } else {
        None
    };

    let samples = if pcm.is_some() {
        Vec::new()
    } else {
        if n > budget.samples_left || n > limits.max_samples {
            return Err(ReelError::LimitExceeded {
                what: "sample-count",
            });
        }
        budget.samples_left =
            budget
                .samples_left
                .checked_sub(n)
                .ok_or(ReelError::LimitExceeded {
                    what: "sample-count",
                })?;
        if fixed_size == 0 {
            let need = n
                .checked_mul(4)
                .ok_or(ReelError::Corrupt { what: "stsz-size" })?;
            if need > z.remaining() {
                return Err(missing("stsz-exceeds-box"));
            }
        }
        // A constant-size `stsz` carries no per-sample bytes, so the sample count alone could make a few
        // hundred bytes of header reserve millions of entries. The chunk tables must be able to
        // address `n` samples before anything is sized from it (expansion would end in `stsc-short`
        // anyway; this is the same refusal, made before the allocation).
        if n > 0 && addressable_samples(&stsc, chunk_offsets.len()) < n as u64 {
            return Err(missing("stsc-short"));
        }
        let mut sizes = Vec::with_capacity(n);
        for _ in 0..n {
            sizes.push(if fixed_size == 0 {
                z.u32()?
            } else {
                fixed_size
            });
        }
        expand_samples(
            n,
            &sizes,
            &stts,
            &ctts,
            &sync_points,
            all_sync,
            &stsc,
            &chunk_offsets,
            cancel,
        )?
    };

    Ok(TrackIndex {
        track_id,
        kind,
        handler,
        enabled: flags & 1 == 1,
        media_timescale,
        media_duration,
        codec: desc.codec,
        width: desc.width,
        height: desc.height,
        audio: desc.audio,
        color: desc.color,
        pixel_aspect: desc.pixel_aspect,
        codec_config: desc.codec_config,
        field_info: desc.field_info,
        clean_aperture: desc.clean_aperture,
        edits: Vec::new(),
        ticks_exact: true,
        timecode: desc.timecode,
        timecode_ref,
        pcm,
        samples,
        presented: Vec::new(),
        sync_points,
        all_sync,
    })
}

/// Edit list and presentation timeline, after every sample (including fragments) is known.
fn finish_track(
    track: &mut TrackIndex,
    trak: &[u8],
    movie_timescale: u32,
    fragmented: bool,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<(), ReelError> {
    build_edits(track, trak, movie_timescale, fragmented)?;
    if track.pcm.is_some() {
        pcm::apply_window(track);
        Ok(())
    } else {
        build_presentation(track, limits, cancel)
    }
}

/// `fragmented`: the file has `mvex`. Only there may the last edit carry a zero `segment_duration`.
fn build_edits(
    track: &mut TrackIndex,
    trak: &[u8],
    movie_timescale: u32,
    fragmented: bool,
) -> Result<(), ReelError> {
    let elst = match child(trak, b"edts")? {
        Some(edts) => child(edts, b"elst")?,
        None => None,
    };
    let Some(elst) = elst else {
        // No edit list: presentation time equals composition time (negative times stay negative).
        let first = track
            .samples
            .iter()
            .map(|s| s.pts)
            .min()
            .unwrap_or(0)
            .min(0);
        let start = media_to_ticks(first, track.media_timescale)?;
        track.ticks_exact &= start.exact;
        track.edits.push(EditSegment {
            presentation_start: start.value,
            duration: i64::MAX,
            media_time: Some(first),
        });
        return Ok(());
    };
    if movie_timescale == 0 {
        return Err(ReelError::Corrupt {
            what: "zero-movie-timescale",
        });
    }
    let (version, _, mut c) = full(elst)?;
    let count = c.u32()? as usize;
    let entry_bytes = if version == 1 { 20 } else { 12 };
    if count
        .checked_mul(entry_bytes)
        .is_none_or(|b| b > c.remaining())
    {
        return Err(ReelError::Corrupt {
            what: "elst-exceeds-box",
        });
    }
    let mut start = 0i64;
    // Entries read, including a skipped one: the edit-count bound counts what the file declares.
    let mut seen = 0usize;
    for _ in 0..count {
        let (segment, media_time) = if version == 1 {
            (c.u64()?, c.u64()? as i64)
        } else {
            (u64::from(c.u32()?), i64::from(c.i32()?))
        };
        let rate = c.u32()?;
        if rate != 0x0001_0000 {
            return Err(ReelError::Unsupported {
                what: "edit-media-rate-not-1",
            });
        }
        if media_time < -1 {
            return Err(ReelError::Corrupt {
                what: "elst-media-time",
            });
        }
        // `count` is bounded by the box size, so `seen` cannot reach usize::MAX.
        seen = seen.saturating_add(1);
        // `Some((duration in ticks, whether the next edit starts after it))`; `None` = no segment.
        let segment_ticks = match (segment, media_time) {
            // An empty edit of no length shifts nothing: it is not a segment.
            (0, -1) => None,
            // A zero segment_duration on the LAST edit of a fragmented file is open-ended (the edit
            // runs to the end of the media, as in a file without an edit list; ISO/IEC 14496-12 as
            // quoted by W3C bug 26066, secondary source). Anywhere else the length of the edit is
            // unknown and is not guessed.
            (0, _) if fragmented && seen == count => Some((i64::MAX, false)),
            (0, _) => {
                return Err(ReelError::Unsupported {
                    what: "elst-zero-duration",
                });
            }
            _ => {
                let seg = i64::try_from(segment)
                    .map_err(|_| ReelError::LimitExceeded { what: "tick-range" })?;
                let duration = media_to_ticks(seg, movie_timescale)?;
                track.ticks_exact &= duration.exact;
                Some((duration.value, true))
            }
        };
        if let Some((duration, advances)) = segment_ticks {
            track.edits.push(EditSegment {
                presentation_start: start,
                duration,
                media_time: (media_time >= 0).then_some(media_time),
            });
            if advances {
                start = start
                    .checked_add(duration)
                    .ok_or(ReelError::LimitExceeded { what: "tick-range" })?;
            }
        }
        if seen > 4096 {
            return Err(ReelError::LimitExceeded { what: "edit-count" });
        }
    }
    Ok(())
}

fn build_presentation(
    track: &mut TrackIndex,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<(), ReelError> {
    if track.edits.len() > limits.max_edit_entries {
        return Err(ReelError::LimitExceeded { what: "edit-count" });
    }
    let n = u32::try_from(track.samples.len()).map_err(|_| ReelError::LimitExceeded {
        what: "sample-count",
    })?;
    let mut order: Vec<u32> = (0..n).collect();
    order.sort_by_key(|&i| track.samples[i as usize].pts);
    let mut presented = Vec::new();
    for (e, edit) in track.edits.clone().iter().enumerate() {
        check_cancel(cancel)?;
        let Some(media_time) = edit.media_time else {
            continue;
        };
        let first = order.partition_point(|&i| track.samples[i as usize].pts < media_time);
        // An edit that starts inside a sample still presents it: decoding begins at the containing
        // sample, its presented time is clamped to the edit start and the lead-in is reported.
        if let Some(containing) = first.checked_sub(1) {
            let i = order[containing];
            let sample = track.samples[i as usize];
            let end = sample.pts.saturating_add(i64::from(sample.duration));
            if end > media_time {
                let tick_range = || ReelError::LimitExceeded { what: "tick-range" };
                let remain = media_to_ticks(
                    end.checked_sub(media_time).ok_or_else(tick_range)?,
                    track.media_timescale,
                )?;
                let trim = media_to_ticks(
                    media_time.checked_sub(sample.pts).ok_or_else(tick_range)?,
                    track.media_timescale,
                )?;
                track.ticks_exact &= remain.exact && trim.exact;
                presented.push(PresentedSample {
                    sample: i,
                    pts: edit.presentation_start,
                    duration: remain.value.max(0) as u64,
                    edit: e as u32,
                    trim_start: trim.value.max(0) as u64,
                });
            }
        }
        for &i in &order[first..] {
            let sample = track.samples[i as usize];
            let (pts, exact) = track.pts_in_edit(e as u32, sample.pts)?;
            if edit.duration != i64::MAX
                && pts >= edit.presentation_start.saturating_add(edit.duration)
            {
                break;
            }
            let dur = media_to_ticks(i64::from(sample.duration), track.media_timescale)?;
            track.ticks_exact &= exact && dur.exact;
            if presented.len() >= limits.max_samples {
                return Err(ReelError::LimitExceeded {
                    what: "presented-samples",
                });
            }
            presented.push(PresentedSample {
                sample: i,
                pts,
                duration: dur.value.max(0) as u64,
                edit: e as u32,
                trim_start: 0,
            });
        }
    }
    track.presented = presented;
    Ok(())
}

/// Where the movie box ended: the fragment scan (if any) continues from `next_pos`.
struct MoovRead {
    payload: Vec<u8>,
    brand: [u8; 4],
    probed: u64,
    next_pos: u64,
}

/// Locate and read `moov` without loading `mdat`.
fn read_moov(
    src: &mut dyn ReadSeek,
    file_len: u64,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<MoovRead, ReelError> {
    let (mut pos, mut brand, mut probed) = (0u64, [0u8; 4], 0u64);
    for _ in 0..MAX_TOP_LEVEL_BOXES {
        check_cancel(cancel)?;
        if file_len.saturating_sub(pos) < 8 {
            break;
        }
        src.seek(SeekFrom::Start(pos))?;
        let mut hdr = [0u8; 8];
        read_exact_structural(src, &mut hdr, "box-header")?;
        probed = probed.saturating_add(8);
        let size32 = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let kind = [hdr[4], hdr[5], hdr[6], hdr[7]];
        let (header, size) = match size32 {
            0 => (8u64, file_len.saturating_sub(pos)),
            1 => {
                let mut big = [0u8; 8];
                read_exact_structural(src, &mut big, "box-largesize")?;
                probed = probed.saturating_add(8);
                (16, u64::from_be_bytes(big))
            }
            n => (8, u64::from(n)),
        };
        if size < header {
            return Err(ReelError::Corrupt { what: "box-size" });
        }
        match &kind {
            b"moof" => {
                return Err(ReelError::Unsupported {
                    what: "moof-before-moov",
                });
            }
            b"ftyp" if size >= header.saturating_add(4) => {
                let mut b = [0u8; 4];
                read_exact_structural(src, &mut b, "ftyp")?;
                probed = probed.saturating_add(4);
                brand = b;
            }
            b"moov" => {
                if pos.checked_add(size).is_none_or(|end| end > file_len) {
                    return Err(ReelError::Corrupt {
                        what: "moov-exceeds-file",
                    });
                }
                // `pos + size <= file_len` was just checked and `size >= header`, so none of these
                // can fail; the checked forms keep the proof local.
                let payload = size.saturating_sub(header);
                if payload > limits.max_probe_bytes.saturating_sub(probed) {
                    return Err(ReelError::LimitExceeded { what: "moov-bytes" });
                }
                let offset_error = ReelError::LimitExceeded { what: "moov-bytes" };
                let payload_at = pos
                    .checked_add(header)
                    .ok_or_else(|| offset_error.clone())?;
                src.seek(SeekFrom::Start(payload_at))?;
                let mut buf =
                    vec![0u8; usize::try_from(payload).map_err(|_| offset_error.clone())?];
                read_exact_structural(src, &mut buf, "moov-truncated")?;
                return Ok(MoovRead {
                    payload: buf,
                    brand,
                    probed: probed.saturating_add(payload),
                    next_pos: pos.checked_add(size).ok_or(offset_error)?,
                });
            }
            _ => {}
        }
        pos = match pos.checked_add(size) {
            Some(p) if p <= file_len => p,
            // A box running past EOF (cut mdat) ends the scan; moov may not exist.
            _ => break,
        };
    }
    Err(ReelError::Corrupt {
        what: "moov-missing",
    })
}

/// Build the full per-track sample index. Reads only box headers, `moov` and (fragmented files) each
/// `moof`/`sidx`; it never loads `mdat`.
pub fn build_index(
    src: &mut dyn ReadSeek,
    file_len: u64,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<MediaIndex, ReelError> {
    let moov_read = read_moov(src, file_len, limits, cancel)?;
    let moov = &moov_read.payload;
    let mut movie_timescale = 0u32;
    let mut traks = Vec::new();
    let mut mvex = None;
    for item in boxes(moov) {
        let (kind, payload) = item?;
        match &kind {
            b"mvhd" => movie_timescale = header_time(payload)?.0,
            b"mvex" => mvex = Some(payload),
            b"trak" => {
                if traks.len() >= limits.max_tracks {
                    return Err(ReelError::LimitExceeded {
                        what: "track-count",
                    });
                }
                traks.push(payload);
            }
            _ => {}
        }
    }
    let fragmented = mvex.is_some();
    let mut budget = Budget {
        samples_left: limits.max_samples,
    };
    let mut tracks = Vec::with_capacity(traks.len());
    for trak in &traks {
        check_cancel(cancel)?;
        tracks.push(track_from_tables(
            trak,
            limits,
            &mut budget,
            cancel,
            fragmented,
        )?);
    }
    let mut bytes_read = moov_read.probed;
    let (mut fragment_offsets, mut segment_indexes, mut truncated_tail) =
        (Vec::new(), Vec::new(), false);
    if let Some(mvex) = mvex {
        let trex = fragments::parse_trex_list(mvex)?;
        let scan = fragments::scan(
            src,
            file_len,
            moov_read.next_pos,
            limits,
            cancel,
            &mut bytes_read,
            &mut tracks,
            &trex,
            &mut budget,
        )?;
        fragment_offsets = scan.fragment_offsets;
        segment_indexes = scan.segment_indexes;
        truncated_tail = scan.truncated_tail;
    }
    for (track, trak) in tracks.iter_mut().zip(&traks) {
        check_cancel(cancel)?;
        finish_track(track, trak, movie_timescale, fragmented, limits, cancel)?;
    }
    Ok(MediaIndex {
        major_brand: moov_read.brand,
        movie_timescale,
        tracks,
        bytes_read,
        source_len: file_len,
        fragmented,
        fragment_offsets,
        segment_indexes,
        truncated_tail,
    })
}

/// Read one coded sample's bytes (bounded by `max_chunk_bytes`); a short file is `Truncated`.
pub fn read_sample(
    src: &mut dyn ReadSeek,
    sample: &SampleEntry,
    buf: &mut Vec<u8>,
    limits: &Limits,
    delivered: u64,
) -> Result<(), ReelError> {
    let size = sample.size as usize;
    if size > limits.max_chunk_bytes {
        return Err(ReelError::LimitExceeded {
            what: "sample-bytes",
        });
    }
    // The sample table is untrusted: a sample that does not lie inside the source is truncated media,
    // found out before a buffer is sized from its declared length.
    let len = src.seek(SeekFrom::End(0))?;
    if sample
        .offset
        .checked_add(u64::from(sample.size))
        .is_none_or(|end| end > len)
    {
        return Err(ReelError::Truncated { delivered });
    }
    buf.clear();
    buf.resize(size, 0);
    src.seek(SeekFrom::Start(sample.offset))?;
    read_exact_or_truncated(src, buf, delivered)
}

fn select_track(index: &MediaIndex, wanted: Option<u32>) -> Result<&TrackIndex, ReelError> {
    match wanted {
        Some(id) => index.track(id).ok_or(ReelError::Unsupported {
            what: "track-not-found",
        }),
        None => index.default_track().ok_or(ReelError::Unsupported {
            what: "no-audio-or-video-track",
        }),
    }
}

fn to_ticks(value: i64) -> Result<Ticks, ReelError> {
    u64::try_from(value)
        .map(Ticks::new)
        .map_err(|_| ReelError::Unsupported {
            what: "negative-presentation-time",
        })
}

impl MediaProvider for IsobmffProvider {
    fn descriptor(&self) -> CodecDescriptor {
        CodecDescriptor {
            id: ISOBMFF_PROVIDER_ID,
            kind: MediaKind::Container,
            container: "isobmff-mov-mp4",
            bit_depths: &[],
            chroma: None,
            transfer: None,
            decoder_version: DECODER_VERSION,
            can_decode: false,
            can_encode: false,
        }
    }

    fn sniff(&self, head: &[u8]) -> bool {
        head.len() >= 8 && matches!(&head[4..8], b"ftyp" | b"moov" | b"mdat" | b"wide")
    }

    fn probe(
        &self,
        src: &mut dyn ReadSeek,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> Result<StreamLayout, ReelError> {
        let len = src.seek(SeekFrom::End(0))?;
        let index = build_index(src, len, limits, cancel)?;
        let track = index.default_track().ok_or(ReelError::Unsupported {
            what: "no-audio-or-video-track",
        })?;
        let video = track.kind == TrackKind::Video;
        let (ticks_per_unit, units) = match track.pcm.as_ref().map(|p| &p.window) {
            Some(Ok(w)) => (w.ticks_per_frame, w.frames),
            _ => (
                track.presented.first().map_or(0, |p| p.duration),
                track.presented.len() as u64,
            ),
        };
        Ok(StreamLayout {
            kind: if video {
                MediaKind::Video
            } else {
                MediaKind::Audio
            },
            codec: fourcc_string(track.codec),
            width: track.width,
            height: track.height,
            ticks_per_unit,
            units,
            channels: track.audio.map_or(0, |a| a.channels),
            tracks: u32::try_from(index.tracks.len()).map_err(|_| ReelError::LimitExceeded {
                what: "track-count",
            })?,
        })
    }

    fn decode_segment(
        &self,
        src: &mut dyn ReadSeek,
        req: &SegmentRequest,
        limits: &Limits,
        cancel: &CancellationToken,
        sink: &mut dyn ChunkSink,
    ) -> Result<SegmentReceipt, ReelError> {
        self.decode_segment_routed(src, req, limits, cancel, sink, &VideoRoute::default())
    }

    fn decode_segment_routed(
        &self,
        src: &mut dyn ReadSeek,
        req: &SegmentRequest,
        limits: &Limits,
        cancel: &CancellationToken,
        sink: &mut dyn ChunkSink,
        route: &VideoRoute<'_>,
    ) -> Result<SegmentReceipt, ReelError> {
        check_cancel(cancel)?;
        if req.codec != ISOBMFF_PROVIDER_ID {
            return Err(ReelError::CodecUnavailable {
                codec: req.codec.clone(),
            });
        }
        if req.count == 0 {
            return Err(ReelError::Unsupported { what: "zero-count" });
        }
        let file_len = verify_grant(src, &req.grant, req.expected_revision)?;
        let index = build_index(src, file_len, limits, cancel)?;
        let mut receipt = self.deliver(src, &index, req, limits, cancel, sink, route)?;
        receipt.bytes_read = receipt
            .bytes_read
            .checked_add(index.bytes_read)
            .ok_or(ReelError::LimitExceeded { what: "bytes-read" })?;
        Ok(receipt)
    }
}

impl IsobmffProvider {
    /// Packet delivery over a prebuilt (cacheable) index. The grant is verified against the live source
    /// before any payload read; `bytes_read` counts payload only (the index was read earlier).
    pub fn decode_packets(
        &self,
        src: &mut dyn ReadSeek,
        index: &MediaIndex,
        req: &SegmentRequest,
        limits: &Limits,
        cancel: &CancellationToken,
        sink: &mut dyn ChunkSink,
    ) -> Result<SegmentReceipt, ReelError> {
        check_cancel(cancel)?;
        if req.codec != ISOBMFF_PROVIDER_ID {
            return Err(ReelError::CodecUnavailable {
                codec: req.codec.clone(),
            });
        }
        if req.count == 0 {
            return Err(ReelError::Unsupported { what: "zero-count" });
        }
        let len = verify_grant(src, &req.grant, req.expected_revision)?;
        if len != index.source_len {
            // The index describes a different file: replaying it would read the wrong bytes.
            return Err(ReelError::StaleGrant);
        }
        self.deliver(
            src,
            index,
            req,
            limits,
            cancel,
            sink,
            &VideoRoute::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn deliver(
        &self,
        src: &mut dyn ReadSeek,
        index: &MediaIndex,
        req: &SegmentRequest,
        limits: &Limits,
        cancel: &CancellationToken,
        sink: &mut dyn ChunkSink,
        route: &VideoRoute<'_>,
    ) -> Result<SegmentReceipt, ReelError> {
        let track = select_track(index, req.track)?;
        if req.output == OutputMode::Decoded {
            if track.codec == *b"av01" {
                return crate::video::decode_video_track(
                    src,
                    VideoTrack {
                        track,
                        codec: VideoCodec::Av1,
                        container: "isobmff",
                        demux_id: "isobmff-demux",
                        config: &track.codec_config,
                        color: track.color,
                        codec_label: fourcc_string(track.codec),
                        width: track.width,
                        height: track.height,
                        unsupported: None,
                    },
                    req,
                    limits,
                    cancel,
                    sink,
                    route,
                );
            }
            // Only uncompressed PCM decodes here; other compressed sample entries have no registered
            // decoder.
            return match pcm::pcm_format(track.codec, track.audio.as_ref())? {
                Some(_) => match &track.pcm {
                    Some(stream) => self.deliver_pcm(src, track, stream, req, limits, cancel, sink),
                    None => Err(ReelError::Unsupported {
                        what: "pcm-irregular-sample-table",
                    }),
                },
                None => Err(ReelError::CodecUnavailable {
                    codec: fourcc_string(track.codec),
                }),
            };
        }
        if track.pcm.is_some() {
            return Err(ReelError::Unsupported {
                what: "packets-of-compact-pcm-track",
            });
        }
        deliver_packets(
            src,
            track,
            fourcc_string(track.codec),
            "isobmff-demux",
            req,
            limits,
            cancel,
            sink,
        )
    }
}

/// Packet delivery for any container whose index is a `TrackIndex`: frame-accurate start, pre-roll
/// from the nearest earlier sync sample (conservative for open GOP), exact requested vs actual spans.
/// `bytes_read` counts payload only.
#[allow(clippy::too_many_arguments)]
pub(crate) fn deliver_packets(
    src: &mut dyn ReadSeek,
    track: &TrackIndex,
    codec_label: String,
    decoder_id: &'static str,
    req: &SegmentRequest,
    limits: &Limits,
    cancel: &CancellationToken,
    sink: &mut dyn ChunkSink,
) -> Result<SegmentReceipt, ReelError> {
    let first = track.seek(req.start_pts)?;
    // Export is exact or it fails, before the first sample is read or the sink is touched: the start
    // must be a presented sample's own PTS and not inside an edit-trimmed first sample. `nearest` is
    // that sample's PTS (a negative PTS, which ticks cannot hold, is the same typed refusal a preview
    // of that sample gets).
    require_exact_start(
        req,
        first.disposition == SeekDisposition::Exact && first.trim_start == 0,
        || to_ticks(first.target.pts),
    )?;
    let last_presented = req
        .count
        .checked_sub(1)
        .ok_or(ReelError::Unsupported { what: "zero-count" })?;
    let last_presented = usize::try_from(last_presented)
        .ok()
        .and_then(|c| first.presented_index.checked_add(c))
        .filter(|i| *i < track.presented.len())
        .ok_or_else(|| {
            let (f, e) = track.presentation_bounds();
            ReelError::OutOfRange {
                requested: req.start_pts,
                first: f,
                end: e,
            }
        })?;
    let wanted = &track.presented[first.presented_index..=last_presented];
    let (mut lo, mut hi) = (u32::MAX, 0u32);
    for p in wanted {
        lo = lo.min(p.sample);
        hi = hi.max(p.sample);
    }
    let from = track.sync_for(lo)?.min(track.sync_for(wanted[0].sample)?);
    // `from <= lo <= hi` by construction (sync_for never moves forward); a violation is a corrupt index.
    let order_error = || ReelError::Corrupt {
        what: "sample-order",
    };
    let span = hi
        .checked_sub(from)
        .and_then(|d| usize::try_from(d).ok())
        .and_then(|d| d.checked_add(1))
        .ok_or_else(order_error)?;
    let mut slots: Vec<Option<(i64, u64, u64)>> = vec![None; span];
    for p in wanted {
        let slot = p.sample.checked_sub(from).ok_or_else(order_error)?;
        slots[slot as usize] = Some((p.pts, p.duration, p.trim_start));
    }
    let last = *wanted.last().expect("count >= 1");
    let start_actual = to_ticks(wanted[0].pts)?;
    let end_actual = to_ticks(
        last.pts
            .checked_add(i64::try_from(last.duration).unwrap_or(i64::MAX))
            .ok_or(ReelError::LimitExceeded { what: "tick-range" })?,
    )?;
    let length =
        end_actual
            .value()
            .checked_sub(start_actual.value())
            .ok_or(ReelError::Corrupt {
                what: "presentation-order",
            })?;
    let (mut chunks, mut payload, mut delivered) = (0u32, 0u64, 0u64);
    let mut buf = Vec::new();
    let mut ticks_exact = track.ticks_exact;
    for (slot, sample_index) in (from..=hi).enumerate() {
        check_cancel(cancel)?;
        let sample = track.samples[sample_index as usize];
        read_sample(src, &sample, &mut buf, limits, delivered)?;
        let (pts, duration, preroll, trim_start) = match slots[slot] {
            Some((pts, duration, trim)) => (pts, duration, false, trim),
            None => {
                let (pts, exact) = track.pts_in_edit(wanted[0].edit, sample.pts)?;
                ticks_exact &= exact;
                let d = media_to_ticks(i64::from(sample.duration), track.media_timescale)?;
                (pts, d.value.max(0) as u64, true, 0)
            }
        };
        sink.accept(Chunk {
            kind: ChunkKind::Packet,
            pts,
            duration: Ticks::new(duration),
            index: u64::from(sample_index),
            units: 1,
            sync: sample.sync,
            preroll,
            trim_start,
            bytes: &buf,
        })?;
        let counter_error = || ReelError::LimitExceeded {
            what: "chunk-count",
        };
        chunks = chunks.checked_add(1).ok_or_else(counter_error)?;
        payload = u64::try_from(buf.len())
            .ok()
            .and_then(|len| payload.checked_add(len))
            .ok_or(ReelError::LimitExceeded { what: "bytes-read" })?;
        if !preroll {
            delivered = delivered.checked_add(1).ok_or_else(counter_error)?;
        }
    }
    Ok(SegmentReceipt {
        requested: Span {
            start_pts: req.start_pts,
            end_pts: Ticks::new(
                req.start_pts
                    .value()
                    .checked_add(length)
                    .ok_or(ReelError::LimitExceeded { what: "tick-range" })?,
            ),
            units: req.count,
        },
        actual: Span {
            start_pts: start_actual,
            end_pts: end_actual,
            units: delivered,
        },
        disposition: if first.disposition == SeekDisposition::Exact && first.trim_start == 0 {
            Disposition::Exact
        } else {
            Disposition::Approximate
        },
        source: SourceKind::Original,
        proxy_note: ProxyNote::NotRequested,
        original_id: req.grant.content_id.clone(),
        proxy_id: None,
        codec: codec_label,
        decoder_id,
        decoder_version: DECODER_VERSION,
        chunks,
        bytes_read: payload,
        complete: true,
        ticks_exact,
        video: None,
    })
}
