//! Instance-owned, enumerable provider registry plus proxy/original arbitration.
//! There is no process-global factory: two registries never share providers.
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{ReelError, check_cancel};
use crate::isobmff::IsobmffProvider;
use crate::matroska::MatroskaProvider;
use crate::model::*;
use crate::video::{DecoderIdentity, ExternalVideoDecoder, SourceTrust, VideoRoute};
use crate::wav::WavProvider;
use crate::y4m::Y4mProvider;
use hsk_studio_accord::CancellationToken;
use std::io::SeekFrom;
use std::sync::Arc;

pub struct Registry {
    providers: Vec<Box<dyn MediaProvider>>,
    /// Instance-owned external decoder for compressed video (see `crate::video`); none by default.
    video_decoder: Option<Arc<dyn ExternalVideoDecoder>>,
}

/// A proxy candidate with its own grant. Used only for preview-purpose requests that prefer a proxy.
///
/// Trust is a property of the bytes that are decoded, and a proxy is a different source from the
/// original (generated or fetched separately, with its own grant). It therefore carries its own
/// `trust`; nothing is inherited from the trust the caller stated for the original. `new` starts
/// `Untrusted`; `with_trust(SourceTrust::Trusted)` vouches for exactly this proxy (the content
/// identity in `grant`), never for any other source.
pub struct ProxySource<'a> {
    pub reader: &'a mut dyn ReadSeek,
    pub grant: Grant,
    pub expected_revision: u64,
    /// How far the caller vouches for THIS proxy's bytes; independent of the original's trust.
    pub trust: SourceTrust,
}

impl<'a> ProxySource<'a> {
    /// A proxy candidate whose bytes are NOT vouched for (`SourceTrust::Untrusted`).
    pub fn new(reader: &'a mut dyn ReadSeek, grant: Grant, expected_revision: u64) -> Self {
        Self {
            reader,
            grant,
            expected_revision,
            trust: SourceTrust::Untrusted,
        }
    }

    /// State how far the caller vouches for this proxy's own bytes (its `grant.content_id`).
    pub fn with_trust(mut self, trust: SourceTrust) -> Self {
        self.trust = trust;
        self
    }
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
            video_decoder: None,
        }
    }

    /// Register the external (isolated) decoder that compressed video of UNTRUSTED sources is routed to.
    /// Builder form of `set_video_decoder`.
    pub fn with_video_decoder(mut self, decoder: Arc<dyn ExternalVideoDecoder>) -> Self {
        self.video_decoder = Some(decoder);
        self
    }

    /// Register the external decoder; one per registry (a second one is a determinate refusal, like a
    /// duplicate provider id).
    pub fn set_video_decoder(
        &mut self,
        decoder: Arc<dyn ExternalVideoDecoder>,
    ) -> Result<(), ReelError> {
        if self.video_decoder.is_some() {
            return Err(ReelError::Unsupported {
                what: "duplicate-video-decoder",
            });
        }
        self.video_decoder = Some(decoder);
        Ok(())
    }

    /// Identity of the registered external video decoder, if any.
    pub fn video_decoder_identity(&self) -> Option<DecoderIdentity> {
        self.video_decoder
            .as_ref()
            .map(|decoder| decoder.identity())
    }

    /// The license-free, pure-Rust provider set shipped by this crate.
    pub fn standard() -> Self {
        let mut registry = Self::new();
        for provider in [
            Box::new(WavProvider) as Box<dyn MediaProvider>,
            Box::new(Y4mProvider),
            Box::new(IsobmffProvider),
            Box::new(MatroskaProvider),
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
            // A `Read` that reports more bytes than the slice it was given is broken: refuse.
            filled = filled
                .checked_add(n)
                .filter(|total| *total <= head.len())
                .ok_or(ReelError::Io(std::io::ErrorKind::InvalidData))?;
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
    ///
    /// Trust routing (see `crate::video`): the ORIGINAL is read as `SourceTrust::Untrusted`, so its
    /// compressed video goes to the registered external decoder or is `CodecUnavailable`. A proxy is a
    /// different source and keeps its own `ProxySource::trust` (`ProxySource::new` is `Untrusted`;
    /// `with_trust(SourceTrust::Trusted)` vouches for that proxy alone), so with cargo feature `av1` the
    /// AV1 of a proxy built with `Trusted` is decoded in-process here as well. The proxy fallback rules
    /// are those of `decode_segment_routed`. Test `trusted_original_never_lends_its_trust_to_a_proxy`.
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
        self.decode_segment_routed(
            original,
            proxy,
            req,
            limits,
            cancel,
            sink,
            SourceTrust::Untrusted,
        )
    }

    /// `decode_segment` with an explicit statement of how far the caller vouches for the bytes of the
    /// ORIGINAL (`req.grant`). `Trusted` additionally allows the in-process decoder (cargo feature
    /// `av1`) for those bytes only. A proxy is a different source: it is decoded under its own
    /// `ProxySource::trust` (default `Untrusted`), not under `trust` (test
    /// `trusted_original_never_lends_its_trust_to_a_proxy`).
    ///
    /// Proxy fallback, decided in two steps. Before any decode, a proxy that cannot stand in for the
    /// original (stale grant or probe error: `ProxyNote::ProxyUnusableUsedOriginal`; layout mismatch:
    /// `ProxyNote::LayoutMismatchUsedOriginal`) is dropped and the original is decoded under `trust`. A
    /// proxy that passes those checks IS the request's source from then on: if its own route cannot
    /// decode it (for example an `Untrusted` proxy of compressed video with no registered external
    /// decoder that supports the codec, which is `CodecUnavailable`) or the decode fails for any other
    /// reason, that error is the request's result and the original is not tried, even when the original
    /// would decode under `trust`. A caller that wants the original then retries with
    /// `SourcePreference::Original`. The `CodecUnavailable` case is pinned by the block "nothing
    /// registered" in `trusted_original_never_lends_its_trust_to_a_proxy`.
    #[allow(clippy::too_many_arguments)]
    pub fn decode_segment_routed(
        &self,
        original: &mut dyn ReadSeek,
        proxy: Option<ProxySource<'_>>,
        req: &SegmentRequest,
        limits: &Limits,
        cancel: &CancellationToken,
        sink: &mut dyn ChunkSink,
        trust: SourceTrust,
    ) -> Result<SegmentReceipt, ReelError> {
        let route = VideoRoute {
            trust,
            external: self.video_decoder.as_deref(),
        };
        check_cancel(cancel)?;
        let provider = self.resolve(req)?;
        if req.prefer == SourcePreference::Proxy && req.purpose == Purpose::Export {
            return Err(ReelError::StrictOriginalRequired);
        }
        let mut note = ProxyNote::NotRequested;
        let mut proxy_id = None;
        if req.prefer == SourcePreference::Proxy {
            // The original's grant is checked before anything is probed or read from either source.
            verify_grant(original, &req.grant, req.expected_revision)?;
            note = ProxyNote::NoProxyAvailable;
            if let Some(mut proxy) = proxy {
                proxy_id = Some(proxy.grant.content_id.clone());
                match proxy_usable(provider, original, &mut proxy, limits, cancel) {
                    Ok(true) => {
                        let ProxySource {
                            reader,
                            grant,
                            expected_revision,
                            trust: proxy_trust,
                        } = proxy;
                        let mut proxy_req = req.clone();
                        proxy_req.grant = grant;
                        proxy_req.expected_revision = expected_revision;
                        // The proxy's own route: its trust is the proxy's, never the original's.
                        let proxy_route = VideoRoute {
                            trust: proxy_trust,
                            external: route.external,
                        };
                        let mut receipt = provider.decode_segment_routed(
                            reader,
                            &proxy_req,
                            limits,
                            cancel,
                            sink,
                            &proxy_route,
                        )?;
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
        let mut receipt =
            provider.decode_segment_routed(original, req, limits, cancel, sink, &route)?;
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
