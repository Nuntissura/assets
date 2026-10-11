//! Provider port, request/receipt vocabulary, limits. Instance-owned: there is no global registry.
use crate::error::ReelError;
use hsk_studio_accord::{CancellationToken, Ticks};
use std::io::{Read, Seek, SeekFrom, Write};

pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek + ?Sized> ReadSeek for T {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Video,
    Audio,
    Container,
}

impl MediaKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Container => "container",
        }
    }
}

/// Enumerable capability descriptor (STU-VID-051).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecDescriptor {
    pub id: &'static str,
    pub kind: MediaKind,
    pub container: &'static str,
    pub bit_depths: &'static [u8],
    pub chroma: Option<&'static str>,
    pub transfer: Option<&'static str>,
    pub decoder_version: &'static str,
    pub can_decode: bool,
    pub can_encode: bool,
}

impl CodecDescriptor {
    pub fn to_json(&self) -> String {
        let depths = self
            .bit_depths
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let opt = |v: Option<&str>| v.map_or_else(|| "null".to_string(), |s| format!("\"{s}\""));
        format!(
            "{{\"id\":\"{}\",\"kind\":\"{}\",\"container\":\"{}\",\"bit_depths\":[{}],\"chroma\":{},\"transfer\":{},\"decoder_version\":\"{}\",\"can_decode\":{},\"can_encode\":{}}}",
            self.id,
            self.kind.as_str(),
            self.container,
            depths,
            opt(self.chroma),
            opt(self.transfer),
            self.decoder_version,
            self.can_decode,
            self.can_encode
        )
    }
}

/// Opaque identity of a source (for example a content hash). Printable ASCII, 1..=128 bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentId(String);

impl ContentId {
    pub fn parse(value: &str) -> Result<Self, ReelError> {
        if value.is_empty() || value.len() > 128 || !value.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(ReelError::Corrupt { what: "content-id" });
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Explicit resource bounds. Every provider checks them before allocating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Container structure bytes a probe may read (ISOBMFF `moov`, headers).
    pub max_probe_bytes: u64,
    /// Largest single chunk (frame / packet / audio slice) buffer.
    pub max_chunk_bytes: usize,
    /// Largest sample table a container index may expand.
    pub max_samples: usize,
    pub max_tracks: usize,
    pub max_edit_entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_probe_bytes: 64 * 1024 * 1024,
            max_chunk_bytes: 8 * 1024 * 1024,
            max_samples: 4_000_000,
            max_tracks: 64,
            max_edit_entries: 4096,
        }
    }
}

/// Caller-issued read grant for one source revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub content_id: ContentId,
    pub revision: u64,
    pub expected_len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// Decoded frames / PCM samples.
    Decoded,
    /// Encoded packets in decode order (demux only).
    Packets,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourcePreference {
    Original,
    Proxy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Preview,
    /// Full-resolution export: proxy preference is refused (`StrictOriginalRequired`).
    Export,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentRequest {
    /// Provider id (see `Registry::descriptors`).
    pub codec: String,
    pub start_pts: Ticks,
    /// Frames, audio samples or presentation samples (packets), depending on the provider.
    pub count: u64,
    /// Container track id; `None` selects the first video track, else the first audio track.
    pub track: Option<u32>,
    pub output: OutputMode,
    pub prefer: SourcePreference,
    pub purpose: Purpose,
    pub grant: Grant,
    /// The caller's current revision of the source; must equal `grant.revision`.
    pub expected_revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkKind {
    VideoFrame,
    AudioSamples,
    Packet,
}

/// One bounded delivery unit. `bytes` borrows a reused buffer: valid only during `accept`.
#[derive(Debug)]
pub struct Chunk<'a> {
    pub kind: ChunkKind,
    /// Presentation time in ticks; negative only for pre-roll packets that precede presentation zero.
    pub pts: i64,
    pub duration: Ticks,
    /// Zero-based index of the first unit in the source stream (frame, sample, or decode index).
    pub index: u64,
    pub units: u64,
    pub sync: bool,
    /// Decode-only packet needed to reach the requested frames; not part of the requested span.
    pub preroll: bool,
    pub bytes: &'a [u8],
}

impl Chunk<'_> {
    pub fn pts_ticks(&self) -> Option<Ticks> {
        u64::try_from(self.pts).ok().map(Ticks::new)
    }
}

pub trait ChunkSink {
    fn accept(&mut self, chunk: Chunk<'_>) -> Result<(), ReelError>;
}

/// Pull side of an encode: fill `buf` with the next contiguous chunk, return its unit count.
pub trait ChunkSource {
    fn next_chunk(&mut self, buf: &mut Vec<u8>) -> Result<Option<u64>, ReelError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeParams {
    Audio {
        rate: u32,
        channels: u16,
        bits: u16,
        float: bool,
    },
    Video {
        width: u32,
        height: u32,
        ticks_per_frame: u64,
        chroma: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeSpec {
    pub codec: String,
    pub params: EncodeParams,
    /// Declared total frames/samples: the segment is `complete` only when exactly this many were written.
    pub total_units: u64,
}

/// Stream shape used to decide whether a proxy may stand in for the original.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamLayout {
    pub kind: MediaKind,
    pub codec: String,
    pub width: u32,
    pub height: u32,
    /// Ticks per frame or per audio sample.
    pub ticks_per_unit: u64,
    pub units: u64,
    pub channels: u16,
    pub tracks: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Requested time was unit-aligned and the exact requested units were delivered.
    Exact,
    /// Requested start fell inside a unit; the containing unit was delivered (actual differs from requested).
    Approximate,
    /// Fewer units than requested were produced; the receipt says how many.
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Original,
    Proxy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyNote {
    NotRequested,
    ProxyUsed,
    NoProxyAvailable,
    LayoutMismatchUsedOriginal,
    /// Proxy failed grant verification or probing; the original was read instead.
    ProxyUnusableUsedOriginal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start_pts: Ticks,
    pub end_pts: Ticks,
    pub units: u64,
}

/// Exact requested vs actual accounting (STU-ARC-015/021). Never a silent clamp or truncation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentReceipt {
    pub requested: Span,
    pub actual: Span,
    pub disposition: Disposition,
    pub source: SourceKind,
    pub proxy_note: ProxyNote,
    pub original_id: ContentId,
    pub proxy_id: Option<ContentId>,
    pub codec: String,
    pub decoder_id: &'static str,
    pub decoder_version: &'static str,
    pub chunks: u32,
    pub bytes_read: u64,
    pub complete: bool,
    /// False when any media-time to tick conversion in this segment needed rounding.
    pub ticks_exact: bool,
}

pub trait MediaProvider: Send + Sync {
    fn descriptor(&self) -> CodecDescriptor;
    /// Cheap magic-number check on the first bytes of a source.
    fn sniff(&self, _head: &[u8]) -> bool {
        false
    }
    fn probe(
        &self,
        src: &mut dyn ReadSeek,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> Result<StreamLayout, ReelError>;
    #[allow(clippy::too_many_arguments)]
    fn decode_segment(
        &self,
        src: &mut dyn ReadSeek,
        req: &SegmentRequest,
        limits: &Limits,
        cancel: &CancellationToken,
        sink: &mut dyn ChunkSink,
    ) -> Result<SegmentReceipt, ReelError>;
    fn encode_segment(
        &self,
        _spec: &EncodeSpec,
        _source: &mut dyn ChunkSource,
        _out: &mut dyn Write,
        _limits: &Limits,
        _cancel: &CancellationToken,
    ) -> Result<EncodeReceipt, ReelError> {
        Err(ReelError::Unsupported {
            what: "encode-not-implemented",
        })
    }
}

/// Result of staging an encode: not published unless `complete`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeReceipt {
    pub codec: String,
    pub complete: bool,
    pub units_written: u64,
    pub units_declared: u64,
    pub bytes_written: u64,
    pub canceled: bool,
}

/// Verify the grant against the live source length and the caller's revision before any payload read.
/// Returns the verified length.
pub(crate) fn verify_grant(
    src: &mut dyn ReadSeek,
    grant: &Grant,
    expected_revision: u64,
) -> Result<u64, ReelError> {
    if grant.revision != expected_revision {
        return Err(ReelError::StaleGrant);
    }
    let len = src.seek(SeekFrom::End(0))?;
    if len != grant.expected_len {
        return Err(ReelError::StaleGrant);
    }
    src.seek(SeekFrom::Start(0))?;
    Ok(len)
}

/// `read_exact` that maps a short read to `Truncated { delivered }`.
pub(crate) fn read_exact_or_truncated(
    src: &mut dyn ReadSeek,
    buf: &mut [u8],
    delivered: u64,
) -> Result<(), ReelError> {
    src.read_exact(buf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            ReelError::Truncated { delivered }
        } else {
            ReelError::Io(e.kind())
        }
    })
}

/// `read_exact` for structural parsing: a short read is corruption of the structure.
pub(crate) fn read_exact_structural(
    src: &mut dyn ReadSeek,
    buf: &mut [u8],
    what: &'static str,
) -> Result<(), ReelError> {
    src.read_exact(buf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            ReelError::Corrupt { what }
        } else {
            ReelError::Io(e.kind())
        }
    })
}
