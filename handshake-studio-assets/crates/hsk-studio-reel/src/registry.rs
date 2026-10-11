//! Instance-owned, enumerable provider registry plus proxy/original arbitration.
//! There is no process-global factory: two registries never share providers.
use crate::error::{ReelError, check_cancel};
use crate::isobmff::IsobmffProvider;
use crate::model::*;
use crate::wav::WavProvider;
use crate::y4m::Y4mProvider;
use hsk_studio_accord::CancellationToken;
use std::io::SeekFrom;

pub struct Registry {
    providers: Vec<Box<dyn MediaProvider>>,
}

/// A proxy candidate with its own grant. Used only for preview-purpose requests that prefer a proxy.
pub struct ProxySource<'a> {
    pub reader: &'a mut dyn ReadSeek,
    pub grant: Grant,
    pub expected_revision: u64,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    /// The license-free, pure-Rust provider set shipped by this crate.
    pub fn standard() -> Self {
        let mut registry = Self::new();
        for provider in [
            Box::new(WavProvider) as Box<dyn MediaProvider>,
            Box::new(Y4mProvider),
            Box::new(IsobmffProvider),
        ] {
            registry
                .register(provider)
                .expect("built-in provider ids are unique");
        }
        registry
    }

    pub fn register(&mut self, provider: Box<dyn MediaProvider>) -> Result<(), ReelError> {
        let id = provider.descriptor().id;
        if self.providers.iter().any(|p| p.descriptor().id == id) {
            return Err(ReelError::Unsupported {
                what: "duplicate-provider-id",
            });
        }
        self.providers.push(provider);
        Ok(())
    }

    pub fn descriptors(&self) -> Vec<CodecDescriptor> {
        self.providers.iter().map(|p| p.descriptor()).collect()
    }

    pub fn descriptors_json(&self) -> String {
        let items = self
            .providers
            .iter()
            .map(|p| p.descriptor().to_json())
            .collect::<Vec<_>>()
            .join(",");
        format!("{{\"providers\":[{items}]}}")
    }

    pub fn resolve_id(&self, id: &str) -> Result<&dyn MediaProvider, ReelError> {
        self.providers
            .iter()
            .find(|p| p.descriptor().id == id)
            .map(|p| p.as_ref())
            .ok_or_else(|| ReelError::CodecUnavailable {
                codec: id.to_owned(),
            })
    }

    pub fn resolve(&self, req: &SegmentRequest) -> Result<&dyn MediaProvider, ReelError> {
        self.resolve_id(&req.codec)
    }

    /// Identify the provider for a source by magic bytes. An unrecognised source is a determinate
    /// `ImportUnsupported` naming the detected type.
    pub fn detect(&self, src: &mut dyn ReadSeek) -> Result<&dyn MediaProvider, ReelError> {
        let mut head = [0u8; 32];
        src.seek(SeekFrom::Start(0))?;
        let mut filled = 0;
        while filled < head.len() {
            let n = std::io::Read::read(src, &mut head[filled..])?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        src.seek(SeekFrom::Start(0))?;
        let head = &head[..filled];
        self.providers
            .iter()
            .find(|p| p.sniff(head))
            .map(|p| p.as_ref())
            .ok_or_else(|| ReelError::ImportUnsupported {
                detected: detect_type(head).to_owned(),
            })
    }

    /// Decode a segment honouring the proxy/original policy; the receipt says which source was read.
    #[allow(clippy::too_many_arguments)]
    pub fn decode_segment(
        &self,
        original: &mut dyn ReadSeek,
        proxy: Option<ProxySource<'_>>,
        req: &SegmentRequest,
        limits: &Limits,
        cancel: &CancellationToken,
        sink: &mut dyn ChunkSink,
    ) -> Result<SegmentReceipt, ReelError> {
        check_cancel(cancel)?;
        let provider = self.resolve(req)?;
        if req.prefer == SourcePreference::Proxy && req.purpose == Purpose::Export {
            return Err(ReelError::StrictOriginalRequired);
        }
        let mut note = ProxyNote::NotRequested;
        let mut proxy_id = None;
        if req.prefer == SourcePreference::Proxy {
            note = ProxyNote::NoProxyAvailable;
            if let Some(mut proxy) = proxy {
                proxy_id = Some(proxy.grant.content_id.clone());
                match proxy_usable(provider, original, &mut proxy, limits, cancel) {
                    Ok(true) => {
                        let ProxySource {
                            reader,
                            grant,
                            expected_revision,
                        } = proxy;
                        let mut proxy_req = req.clone();
                        proxy_req.grant = grant;
                        proxy_req.expected_revision = expected_revision;
                        let mut receipt =
                            provider.decode_segment(reader, &proxy_req, limits, cancel, sink)?;
                        receipt.source = SourceKind::Proxy;
                        receipt.proxy_note = ProxyNote::ProxyUsed;
                        receipt.original_id = req.grant.content_id.clone();
                        receipt.proxy_id = proxy_id;
                        return Ok(receipt);
                    }
                    Ok(false) => note = ProxyNote::LayoutMismatchUsedOriginal,
                    Err(ReelError::Canceled) => return Err(ReelError::Canceled),
                    Err(_) => note = ProxyNote::ProxyUnusableUsedOriginal,
                }
            }
        }
        let mut receipt = provider.decode_segment(original, req, limits, cancel, sink)?;
        receipt.proxy_note = note;
        receipt.proxy_id = proxy_id;
        Ok(receipt)
    }
}

/// A proxy may stand in for the original only when timebase, unit count, channel layout, kind and track
/// count match exactly; proxy pixel dimensions may be smaller than or equal to the original's.
fn proxy_usable(
    provider: &dyn MediaProvider,
    original: &mut dyn ReadSeek,
    proxy: &mut ProxySource<'_>,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<bool, ReelError> {
    verify_grant(&mut *proxy.reader, &proxy.grant, proxy.expected_revision)?;
    let o = provider.probe(original, limits, cancel)?;
    let p = provider.probe(&mut *proxy.reader, limits, cancel)?;
    Ok(p.kind == o.kind
        && p.ticks_per_unit == o.ticks_per_unit
        && p.units == o.units
        && p.channels == o.channels
        && p.tracks == o.tracks
        && p.width <= o.width
        && p.height <= o.height)
}

/// Magic-number naming of well-known containers/images we do not decode (for `ImportUnsupported`).
pub fn detect_type(head: &[u8]) -> &'static str {
    let starts = |magic: &[u8]| head.len() >= magic.len() && &head[..magic.len()] == magic;
    if starts(&[0x1A, 0x45, 0xDF, 0xA3]) {
        "matroska-webm"
    } else if starts(b"OggS") {
        "ogg"
    } else if starts(b"fLaC") {
        "flac"
    } else if starts(b"RIFF") && head.len() >= 12 && &head[8..12] == b"AVI " {
        "avi"
    } else if starts(b"RIFF") {
        "riff-other"
    } else if starts(&[0x06, 0x0E, 0x2B, 0x34]) {
        "mxf"
    } else if starts(b"\x89PNG") {
        "png"
    } else if starts(&[0xFF, 0xD8, 0xFF]) {
        "jpeg"
    } else if starts(b"GIF8") {
        "gif"
    } else if starts(b"ID3") {
        "mp3-id3"
    } else if starts(&[0x47]) {
        "mpeg-ts-candidate"
    } else {
        "unknown"
    }
}
