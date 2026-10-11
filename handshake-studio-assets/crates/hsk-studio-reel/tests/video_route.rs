//! Compressed-video decode routing (`video`): which decoder runs which source, and how decoder faults
//! surface. Runs in every build (no cargo feature): the demux is pure Rust, the decoder is a fake
//! external provider, and the in-process rav1d decoder is only expected when feature `av1` is on.
//!
//! Fixture: `tests/fixtures/av1/av1-16x16-3f.{mp4,mkv}` (3 frames of AV1, 16x16). The fake emits a picture
//! whose bytes are a pure function of the packet tag, so every expectation here is hand-computed.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_observe::{FailureCode, Outcome};
use hsk_studio_reel::*;
use std::{
    io::Cursor,
    sync::{Arc, Mutex},
};

mod common;
use common::{Mp4Sample, Mp4Spec, mp4_bytes};

const MP4: &[u8] = include_bytes!("fixtures/av1/av1-16x16-3f.mp4");
const MKV: &[u8] = include_bytes!("fixtures/av1/av1-16x16-3f.mkv");
const MS: u64 = 254_016_000; // Pulse ticks per millisecond
const PTS_MP4: [u64; 3] = [0, 10_584_000_000, 21_168_000_000];
const PTS_MKV: [u64; 3] = [0, 42 * MS, 83 * MS];
const FAKE: DecoderIdentity = DecoderIdentity {
    id: "fake-av1",
    version: "0.0-test",
};

fn picture_format() -> VideoFormat {
    VideoFormat {
        width: 16,
        height: 16,
        bit_depth: 8,
        chroma: ChromaFormat::Yuv420,
        color: ColorInfo {
            primaries: 1,
            transfer: 1,
            matrix: 1,
            full_range: false,
            origin: ColorOrigin::SequenceHeader,
        },
    }
}

/// Y = 256 x tag, U = 64 x 0x80, V = 64 x 0x81 (one 16x16 4:2:0 frame = 384 bytes).
fn packed(tag: u8) -> Vec<u8> {
    let mut bytes = vec![tag; 256];
    bytes.extend_from_slice(&[0x80; 64]);
    bytes.extend_from_slice(&[0x81; 64]);
    bytes
}

#[derive(Default)]
struct Seen {
    opens: Vec<(&'static str, Vec<u8>, u32, u32)>,
    packets: Vec<(u64, Vec<u8>)>,
    finished: u32,
}

#[derive(Clone, Copy)]
enum Plan {
    Works,
    FaultOnOpen(DecoderFault),
    FaultOnPacket(u64, DecoderFault),
    MediaErrorOnPacket(u64),
    FaultOnFinish(DecoderFault),
    ShortPlane,
    /// The picture of the packet with this tag is never released (a decoder that drops a picture).
    DropPicture(u64),
    /// A picture of `width` x `height` whose planes are sized for 8-bit 4:2:0 at that size, whatever
    /// `bit_depth` the format claims (`bit_depth` other than 8 makes planes and format disagree).
    Picture {
        width: u32,
        height: u32,
        bit_depth: u8,
    },
}

struct Fake {
    seen: Arc<Mutex<Seen>>,
    plan: Plan,
    supports: bool,
}

impl Fake {
    fn new(plan: Plan) -> (Self, Arc<Mutex<Seen>>) {
        let seen = Arc::new(Mutex::new(Seen::default()));
        (
            Self {
                seen: Arc::clone(&seen),
                plan,
                supports: true,
            },
            seen,
        )
    }
}

struct FakeSession {
    seen: Arc<Mutex<Seen>>,
    plan: Plan,
}

impl ExternalVideoDecoder for Fake {
    fn identity(&self) -> DecoderIdentity {
        FAKE
    }
    fn supports(&self, codec: VideoCodec) -> bool {
        self.supports && codec == VideoCodec::Av1
    }
    fn open(
        &self,
        spec: &VideoStreamSpec<'_>,
        _cancel: &CancellationToken,
    ) -> Result<Box<dyn VideoDecodeSession>, VideoDecodeError> {
        self.seen.lock().unwrap().opens.push((
            spec.container,
            spec.config.to_vec(),
            spec.width,
            spec.height,
        ));
        if let Plan::FaultOnOpen(fault) = self.plan {
            return Err(fault.into());
        }
        Ok(Box::new(FakeSession {
            seen: Arc::clone(&self.seen),
            plan: self.plan,
        }))
    }
}

impl VideoDecodeSession for FakeSession {
    fn send(
        &mut self,
        packet: &[u8],
        tag: u64,
        _cancel: &CancellationToken,
        out: &mut dyn PictureSink,
    ) -> Result<(), VideoDecodeError> {
        self.seen
            .lock()
            .unwrap()
            .packets
            .push((tag, packet.to_vec()));
        match self.plan {
            Plan::FaultOnPacket(at, fault) if at == tag => return Err(fault.into()),
            Plan::MediaErrorOnPacket(at) if at == tag => {
                return Err(ReelError::Corrupt { what: "fake-media" }.into());
            }
            _ => {}
        }
        if matches!(self.plan, Plan::DropPicture(at) if at == tag) {
            return Ok(());
        }
        if let Plan::Picture {
            width,
            height,
            bit_depth,
        } = self.plan
        {
            let (luma, chroma) = (
                width as usize * height as usize,
                width.div_ceil(2) as usize * height.div_ceil(2) as usize,
            );
            out.accept(DecodedPicture {
                tag,
                format: VideoFormat {
                    width,
                    height,
                    bit_depth,
                    ..picture_format()
                },
                film_grain_applied: false,
                planes: vec![
                    vec![tag as u8; luma],
                    vec![0x80; chroma],
                    vec![0x81; chroma],
                ],
            })?;
            return Ok(());
        }
        let mut planes = vec![vec![tag as u8; 256], vec![0x80; 64], vec![0x81; 64]];
        if matches!(self.plan, Plan::ShortPlane) {
            planes[0].pop();
        }
        out.accept(DecodedPicture {
            tag,
            format: picture_format(),
            film_grain_applied: false,
            planes,
        })?;
        Ok(())
    }

    fn finish(
        &mut self,
        _cancel: &CancellationToken,
        _out: &mut dyn PictureSink,
    ) -> Result<(), VideoDecodeError> {
        self.seen.lock().unwrap().finished += 1;
        match self.plan {
            Plan::FaultOnFinish(fault) => Err(fault.into()),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Collect {
    chunks: Vec<(ChunkKind, i64, u64, bool, Vec<u8>)>,
}

impl ChunkSink for Collect {
    fn accept(&mut self, c: Chunk<'_>) -> Result<(), ReelError> {
        self.chunks
            .push((c.kind, c.pts, c.index, c.sync, c.bytes.to_vec()));
        Ok(())
    }
}

fn request(
    codec: &str,
    bytes: &[u8],
    start: u64,
    count: u64,
    output: OutputMode,
) -> SegmentRequest {
    SegmentRequest {
        codec: codec.to_owned(),
        start_pts: Ticks::new(start),
        count,
        track: None,
        output,
        prefer: SourcePreference::Original,
        purpose: Purpose::Preview,
        grant: Grant {
            content_id: ContentId::parse("av1-fixture").unwrap(),
            revision: 1,
            expected_len: bytes.len() as u64,
        },
        expected_revision: 1,
    }
}

fn decode(
    registry: &Registry,
    bytes: &[u8],
    container: &str,
    (start, count): (u64, u64),
    trust: SourceTrust,
) -> (Result<SegmentReceipt, ReelError>, Collect) {
    let mut sink = Collect::default();
    let result = registry.decode_segment_routed(
        &mut Cursor::new(bytes.to_vec()),
        None,
        &request(container, bytes, start, count, OutputMode::Decoded),
        &Limits::default(),
        &CancellationToken::default(),
        &mut sink,
        trust,
    );
    (result, sink)
}

/// The container's own packets (demux only, no decoder involved): the ground truth the decoder must see.
fn packets_of(registry: &Registry, bytes: &[u8], container: &str) -> Vec<(u64, Vec<u8>)> {
    let mut sink = Collect::default();
    registry
        .decode_segment(
            &mut Cursor::new(bytes.to_vec()),
            None,
            &request(container, bytes, 0, 3, OutputMode::Packets),
            &Limits::default(),
            &CancellationToken::default(),
            &mut sink,
        )
        .unwrap();
    assert_eq!(sink.chunks.len(), 3);
    sink.chunks
        .into_iter()
        .map(|(_, _, index, _, bytes)| (index, bytes))
        .collect()
}

/// `av1C` / CodecPrivate as the container indexes it.
fn config_of(bytes: &[u8], container: &str) -> Vec<u8> {
    let len = bytes.len() as u64;
    let (limits, cancel) = (Limits::default(), CancellationToken::default());
    let mut src = Cursor::new(bytes.to_vec());
    if container == ISOBMFF_PROVIDER_ID {
        let index = build_index(&mut src, len, &limits, &cancel).unwrap();
        index.default_track().unwrap().codec_config.clone()
    } else {
        let index = build_matroska_index(&mut src, len, &limits, &cancel).unwrap();
        index.default_track().unwrap().info.codec_private.clone()
    }
}

#[test]
fn untrusted_video_reaches_the_registered_external_decoder_with_exact_packets_and_receipts() {
    for (name, bytes, pts, container) in [
        ("mp4", MP4, PTS_MP4, ISOBMFF_PROVIDER_ID),
        ("mkv", MKV, PTS_MKV, MATROSKA_PROVIDER_ID),
    ] {
        let (fake, seen) = Fake::new(Plan::Works);
        let registry = Registry::standard().with_video_decoder(Arc::new(fake));
        assert_eq!(registry.video_decoder_identity(), Some(FAKE));
        let truth = packets_of(&registry, bytes, container);
        assert_eq!(truth.iter().map(|p| p.0).collect::<Vec<_>>(), [0, 1, 2]);

        // Whole clip (default route = untrusted): one session, the container's own config and
        // declared size, every packet in decode order, one finish; pictures land as frame chunks.
        let (receipt, sink) = decode(&registry, bytes, container, (0, 3), SourceTrust::Untrusted);
        let receipt = receipt.unwrap();
        {
            let seen = seen.lock().unwrap();
            let want_open = (
                if container == ISOBMFF_PROVIDER_ID {
                    "isobmff"
                } else {
                    "matroska"
                },
                config_of(bytes, container),
                16,
                16,
            );
            assert_eq!(seen.opens, [want_open], "{name}");
            assert_eq!(seen.packets, truth, "{name}");
            assert_eq!(seen.finished, 1, "{name}");
        }
        assert_eq!(sink.chunks.len(), 3, "{name}");
        for (i, (kind, got_pts, index, sync, data)) in sink.chunks.iter().enumerate() {
            assert_eq!(*kind, ChunkKind::VideoFrame, "{name} {i}");
            assert_eq!(*got_pts as u64, pts[i], "{name} {i}");
            assert_eq!((*index, *sync), (i as u64, i == 0), "{name} {i}");
            assert_eq!(*data, packed(i as u8), "{name} {i}");
        }
        assert_eq!(receipt.decoder_id, FAKE.id);
        assert_eq!(receipt.decoder_version, FAKE.version);
        assert!(receipt.complete && receipt.actual.units == 3 && receipt.bytes_read > 0);
        assert_eq!(receipt.disposition, Disposition::Exact);
        assert_eq!(receipt.video, Some(picture_format()));

        // Random access on the last frame: the two earlier packets are fed as pre-roll, their pictures
        // are dropped, exactly one chunk reaches the sink.
        seen.lock().unwrap().packets.clear();
        let (receipt, sink) = decode(
            &registry,
            bytes,
            container,
            (pts[2], 1),
            SourceTrust::Untrusted,
        );
        let receipt = receipt.unwrap();
        assert_eq!(seen.lock().unwrap().packets, truth, "{name} pre-roll");
        assert_eq!(sink.chunks.len(), 1, "{name}");
        assert_eq!(sink.chunks[0].2, 2);
        assert_eq!(sink.chunks[0].4, packed(2));
        assert_eq!(receipt.actual.units, 1);
    }
}

#[test]
fn trust_and_registration_decide_who_decodes() {
    // With the in-process decoder compiled in (feature av1) a TRUSTED source runs it; otherwise the
    // external decoder (if any) serves trusted sources too.
    let in_process = cfg!(feature = "av1");
    for (bytes, container, label) in [
        (MP4, ISOBMFF_PROVIDER_ID, "av01"),
        (MKV, MATROSKA_PROVIDER_ID, "V_AV1"),
    ] {
        let unavailable = ReelError::CodecUnavailable {
            codec: label.to_owned(),
        };
        let decode_with =
            |registry: &Registry, trust| decode(registry, bytes, container, (0, 3), trust).0;

        // Nothing registered: untrusted video is a typed CodecUnavailable naming the codec, for the
        // registry, for a bare provider call (default route) and for the plain `decode_segment`.
        let bare = Registry::standard();
        assert_eq!(
            decode_with(&bare, SourceTrust::Untrusted).unwrap_err(),
            unavailable
        );
        let mut sink = Collect::default();
        let plain = bare
            .resolve_id(container)
            .unwrap()
            .decode_segment(
                &mut Cursor::new(bytes.to_vec()),
                &request(container, bytes, 0, 3, OutputMode::Decoded),
                &Limits::default(),
                &CancellationToken::default(),
                &mut sink,
            )
            .unwrap_err();
        assert_eq!(plain, unavailable);
        assert!(sink.chunks.is_empty());
        let trusted = decode_with(&bare, SourceTrust::Trusted);
        if in_process {
            assert_eq!(trusted.unwrap().decoder_id, "rav1d");
        } else {
            assert_eq!(trusted.unwrap_err(), unavailable);
        }

        // A registered decoder that does not support the codec changes nothing (and is never opened).
        let (mut deaf, deaf_seen) = Fake::new(Plan::Works);
        deaf.supports = false;
        let registry = Registry::standard().with_video_decoder(Arc::new(deaf));
        assert_eq!(
            decode_with(&registry, SourceTrust::Untrusted).unwrap_err(),
            unavailable
        );
        assert!(deaf_seen.lock().unwrap().opens.is_empty());

        // Registered + untrusted: always the external decoder.
        let (fake, seen) = Fake::new(Plan::Works);
        let registry = Registry::standard().with_video_decoder(Arc::new(fake));
        let receipt = decode_with(&registry, SourceTrust::Untrusted).unwrap();
        assert_eq!(receipt.decoder_id, FAKE.id);
        assert_eq!(seen.lock().unwrap().opens.len(), 1);

        // Registered + trusted: in-process when compiled in (the external decoder stays untouched),
        // else the external decoder.
        let receipt = decode_with(&registry, SourceTrust::Trusted).unwrap();
        if in_process {
            assert_eq!(receipt.decoder_id, "rav1d");
            assert_eq!(
                seen.lock().unwrap().opens.len(),
                1,
                "trusted must not use the worker"
            );
        } else {
            assert_eq!(receipt.decoder_id, FAKE.id);
            assert_eq!(seen.lock().unwrap().opens.len(), 2);
        }

        // The same policy through a provider called directly with an explicit route.
        let (fake, seen) = Fake::new(Plan::Works);
        let route = VideoRoute::untrusted().with_external(&fake);
        let mut sink = Collect::default();
        let receipt = bare
            .resolve_id(container)
            .unwrap()
            .decode_segment_routed(
                &mut Cursor::new(bytes.to_vec()),
                &request(container, bytes, 0, 3, OutputMode::Decoded),
                &Limits::default(),
                &CancellationToken::default(),
                &mut sink,
                &route,
            )
            .unwrap();
        assert_eq!((receipt.decoder_id, sink.chunks.len()), (FAKE.id, 3));
        assert_eq!(seen.lock().unwrap().opens.len(), 1);
    }
    // One external decoder per registry; the descriptor list is unchanged by registering it.
    let (a, _) = Fake::new(Plan::Works);
    let (b, _) = Fake::new(Plan::Works);
    let mut registry = Registry::standard();
    let before = registry.descriptors_json();
    registry.set_video_decoder(Arc::new(a)).unwrap();
    assert_eq!(
        registry.set_video_decoder(Arc::new(b)).unwrap_err(),
        ReelError::Unsupported {
            what: "duplicate-video-decoder"
        }
    );
    assert_eq!(registry.descriptors_json(), before);
}

/// Preview decode that prefers a proxy: the original is the fixture itself (`request`'s grant), the proxy
/// is a DIFFERENT source (own content id) with the same layout, so the proxy rules accept it. `proxy_trust`
/// `None` is the `ProxySource::new` default; `stale_proxy_len` makes the proxy's grant stale.
fn decode_preferring_proxy(
    registry: &Registry,
    bytes: &[u8],
    container: &str,
    original_trust: SourceTrust,
    proxy_trust: Option<SourceTrust>,
    stale_proxy_len: u64,
) -> (Result<SegmentReceipt, ReelError>, Collect) {
    let mut req = request(container, bytes, 0, 3, OutputMode::Decoded);
    req.prefer = SourcePreference::Proxy;
    let mut proxy_reader = Cursor::new(bytes.to_vec());
    let proxy_grant = Grant {
        content_id: ContentId::parse("av1-proxy").unwrap(),
        revision: 1,
        expected_len: (bytes.len() as u64) + stale_proxy_len,
    };
    let mut proxy = ProxySource::new(&mut proxy_reader, proxy_grant, 1);
    if let Some(trust) = proxy_trust {
        proxy = proxy.with_trust(trust);
    }
    let mut sink = Collect::default();
    let result = registry.decode_segment_routed(
        &mut Cursor::new(bytes.to_vec()),
        Some(proxy),
        &req,
        &Limits::default(),
        &CancellationToken::default(),
        &mut sink,
        original_trust,
    );
    (result, sink)
}

#[test]
fn trusted_original_never_lends_its_trust_to_a_proxy() {
    // Feature av1 is what makes a TRUSTED source reach the in-process decoder; in a default build a trusted
    // source goes to the external decoder, so the in-process half of each expectation is feature-gated.
    let in_process = cfg!(feature = "av1");
    let proxy_id = Some(ContentId::parse("av1-proxy").unwrap());
    for (bytes, container, label) in [
        (MP4, ISOBMFF_PROVIDER_ID, "av01"),
        (MKV, MATROSKA_PROVIDER_ID, "V_AV1"),
    ] {
        let unavailable = ReelError::CodecUnavailable {
            codec: label.to_owned(),
        };

        // Trusted original + default proxy + a registered external decoder: the PROXY bytes are decoded
        // by the external decoder (never by rav1d, whatever the original's trust says).
        for original_trust in [SourceTrust::Trusted, SourceTrust::Untrusted] {
            let (fake, seen) = Fake::new(Plan::Works);
            let registry = Registry::standard().with_video_decoder(Arc::new(fake));
            let (result, sink) =
                decode_preferring_proxy(&registry, bytes, container, original_trust, None, 0);
            let receipt = result.unwrap();
            assert_eq!(
                receipt.source,
                SourceKind::Proxy,
                "{label} {original_trust:?}"
            );
            assert_eq!(receipt.proxy_note, ProxyNote::ProxyUsed);
            assert_eq!(receipt.proxy_id, proxy_id);
            assert_eq!(
                receipt.decoder_id, FAKE.id,
                "{label} {original_trust:?}: proxy bytes reached a decoder other than the external one"
            );
            assert_eq!(seen.lock().unwrap().opens.len(), 1);
            assert_eq!(sink.chunks.len(), 3);
        }

        // Same, nothing registered: the proxy is CodecUnavailable (no in-process fallback), and no frame
        // reaches the sink.
        for original_trust in [SourceTrust::Trusted, SourceTrust::Untrusted] {
            let bare = Registry::standard();
            let (result, sink) =
                decode_preferring_proxy(&bare, bytes, container, original_trust, None, 0);
            assert_eq!(
                result.unwrap_err(),
                unavailable,
                "{label} {original_trust:?}"
            );
            assert!(sink.chunks.is_empty());
        }

        // An EXPLICIT grant for the proxy is honoured on its own, whatever the original's trust is.
        for original_trust in [SourceTrust::Trusted, SourceTrust::Untrusted] {
            let (fake, seen) = Fake::new(Plan::Works);
            let registry = Registry::standard().with_video_decoder(Arc::new(fake));
            let (result, _) = decode_preferring_proxy(
                &registry,
                bytes,
                container,
                original_trust,
                Some(SourceTrust::Trusted),
                0,
            );
            let receipt = result.unwrap();
            assert_eq!(receipt.source, SourceKind::Proxy);
            let external_opens = seen.lock().unwrap().opens.len();
            if in_process {
                assert_eq!(receipt.decoder_id, "rav1d", "{label} {original_trust:?}");
                assert_eq!(external_opens, 0);
            } else {
                assert_eq!(receipt.decoder_id, FAKE.id);
                assert_eq!(external_opens, 1);
            }
        }
        // ... and an explicitly untrusted proxy next to a trusted original is the default again.
        let (fake, seen) = Fake::new(Plan::Works);
        let registry = Registry::standard().with_video_decoder(Arc::new(fake));
        let (result, _) = decode_preferring_proxy(
            &registry,
            bytes,
            container,
            SourceTrust::Trusted,
            Some(SourceTrust::Untrusted),
            0,
        );
        assert_eq!(result.unwrap().decoder_id, FAKE.id);
        assert_eq!(seen.lock().unwrap().opens.len(), 1);

        // A proxy that cannot be used (stale grant) falls back to the original, which keeps the trust the
        // caller stated for it.
        let (fake, seen) = Fake::new(Plan::Works);
        let registry = Registry::standard().with_video_decoder(Arc::new(fake));
        let (result, _) =
            decode_preferring_proxy(&registry, bytes, container, SourceTrust::Trusted, None, 1);
        let receipt = result.unwrap();
        assert_eq!(receipt.source, SourceKind::Original);
        assert_eq!(receipt.proxy_note, ProxyNote::ProxyUnusableUsedOriginal);
        let external_opens = seen.lock().unwrap().opens.len();
        if in_process {
            assert_eq!(receipt.decoder_id, "rav1d");
            assert_eq!(external_opens, 0);
        } else {
            assert_eq!(receipt.decoder_id, FAKE.id);
            assert_eq!(external_opens, 1);
        }
    }
}

#[test]
fn decoder_faults_are_a_distinct_typed_error_and_media_errors_pass_through() {
    let run = |plan: Plan| {
        let (fake, seen) = Fake::new(plan);
        let registry = Registry::standard().with_video_decoder(Arc::new(fake));
        let (result, sink) = decode(
            &registry,
            MP4,
            ISOBMFF_PROVIDER_ID,
            (0, 3),
            SourceTrust::Untrusted,
        );
        (result, sink, seen)
    };

    // A decoder that dies on the second packet: the route returns the distinct fault (not Corrupt, not
    // Unsupported), the picture delivered before it stays delivered, no receipt exists.
    let (result, sink, seen) = run(Plan::FaultOnPacket(1, DecoderFault::Crashed));
    let error = result.unwrap_err();
    assert_eq!(
        error,
        ReelError::DecoderFault {
            fault: DecoderFault::Crashed
        }
    );
    assert_eq!(error.code(), "MEDIA_DECODER_FAULT");
    assert_eq!(error.to_string(), "MEDIA_DECODER_FAULT decoder_crashed");
    assert_ne!(error.code(), ReelError::Corrupt { what: "x" }.code());
    assert_ne!(error.code(), ReelError::Unsupported { what: "x" }.code());
    assert_eq!(sink.chunks.len(), 1);
    assert_eq!(seen.lock().unwrap().packets.len(), 2);
    // An isolated-decoder fault is not a statement about the media: reported as Unavailable.
    assert_eq!(
        outcome_for(&Err(error)),
        Outcome::Failure(FailureCode::Unavailable)
    );

    // Each fault keeps its own stable code, from open, from a packet and from finish.
    for (plan, fault, code) in [
        (
            Plan::FaultOnOpen(DecoderFault::Deadline),
            DecoderFault::Deadline,
            "decoder_deadline",
        ),
        (
            Plan::FaultOnPacket(0, DecoderFault::Protocol),
            DecoderFault::Protocol,
            "decoder_protocol",
        ),
        (
            Plan::FaultOnFinish(DecoderFault::Crashed),
            DecoderFault::Crashed,
            "decoder_crashed",
        ),
    ] {
        let (result, _, seen) = run(plan);
        assert_eq!(result.unwrap_err(), ReelError::DecoderFault { fault });
        assert_eq!(fault.code(), code);
        if matches!(plan, Plan::FaultOnOpen(_)) {
            assert!(
                seen.lock().unwrap().packets.is_empty(),
                "no packet after a failed open"
            );
        }
    }

    // A media error from the decoder passes through unchanged (still a statement about the media).
    let (result, _, _) = run(Plan::MediaErrorOnPacket(2));
    assert_eq!(
        result.unwrap_err(),
        ReelError::Corrupt { what: "fake-media" }
    );

    // A picture whose planes disagree with its declared format is the decoder breaking the boundary.
    let (result, sink, _) = run(Plan::ShortPlane);
    assert_eq!(
        result.unwrap_err(),
        ReelError::DecoderFault {
            fault: DecoderFault::Protocol
        }
    );
    assert!(sink.chunks.is_empty());

    // The mapping is the route's, not the decoder's: both shapes convert explicitly.
    assert_eq!(
        VideoDecodeError::from(DecoderFault::Deadline).into_reel(),
        ReelError::DecoderFault {
            fault: DecoderFault::Deadline
        }
    );
    assert_eq!(
        VideoDecodeError::from(ReelError::Canceled).into_reel(),
        ReelError::Canceled
    );
}

/// PLR-032: a decoded picture is compared with the size the CONTAINER declares for the track (AV1: the
/// sequence maximum, AV1-ISOBMFF 2.2.4 and the Matroska `V_AV1` mapping), not only with its own format.
/// Larger than declared on either side is a decoder protocol fault at the first picture: nothing reaches
/// the sink and the decoder is not fed a second packet. Equal or smaller is legal. A side the container
/// does not declare (0) is not checked. A picture whose planes disagree with its format stays refused.
#[test]
fn decoded_picture_size_is_checked_against_the_container_declared_size() {
    let protocol = ReelError::DecoderFault {
        fault: DecoderFault::Protocol,
    };
    let decode_with = |bytes: &[u8], container: &str, width, height, bit_depth| {
        let (fake, seen) = Fake::new(Plan::Picture {
            width,
            height,
            bit_depth,
        });
        let registry = Registry::standard().with_video_decoder(Arc::new(fake));
        let (result, sink) = decode(&registry, bytes, container, (0, 3), SourceTrust::Untrusted);
        (result, sink, seen)
    };
    let packed_len = |w: u32, h: u32| (w * h + 2 * w.div_ceil(2) * h.div_ceil(2)) as usize;

    // Both fixtures declare 16 x 16 (mp4: sample entry; mkv: PixelWidth / PixelHeight).
    for (name, bytes, container) in [
        ("mp4", MP4, ISOBMFF_PROVIDER_ID),
        ("mkv", MKV, MATROSKA_PROVIDER_ID),
    ] {
        // Above the declared size on either side, by one sample or by a lot.
        for (w, h) in [(17, 16), (16, 17), (32, 16), (16, 32), (64, 64)] {
            let (result, sink, seen) = decode_with(bytes, container, w, h, 8);
            assert_eq!(result.unwrap_err(), protocol, "{name} {w}x{h}");
            assert!(
                sink.chunks.is_empty(),
                "{name} {w}x{h}: nothing is delivered"
            );
            assert_eq!(seen.lock().unwrap().packets.len(), 1, "{name} {w}x{h}");
        }
        // Planes that disagree with the format (10-bit format, 8-bit planes) are refused as before.
        let (result, sink, _) = decode_with(bytes, container, 16, 16, 10);
        assert_eq!(result.unwrap_err(), protocol, "{name} 10-bit format");
        assert!(sink.chunks.is_empty(), "{name} 10-bit format");
        // The declared size itself and anything smaller are legal (sequence maximum), and the receipt
        // carries the format that was actually delivered.
        for (w, h) in [(16, 16), (8, 8), (16, 1), (1, 16), (1, 1)] {
            let (result, sink, _) = decode_with(bytes, container, w, h, 8);
            let receipt = result.unwrap_or_else(|e| panic!("{name} {w}x{h}: {e:?}"));
            assert_eq!(
                receipt.video.map(|v| (v.width, v.height)),
                Some((w, h)),
                "{name} {w}x{h}"
            );
            assert_eq!(sink.chunks.len(), 3, "{name} {w}x{h}");
            assert!(
                sink.chunks.iter().all(|c| c.4.len() == packed_len(w, h)),
                "{name} {w}x{h}: packed frame size"
            );
        }
    }

    // A side the container does not declare (0) is not checked; the declared side still is.
    let synthetic = |width: u16, height: u16| {
        mp4_bytes(&Mp4Spec {
            movie_timescale: 600,
            media_timescale: 600,
            codec: *b"av01",
            width,
            height,
            samples: (0..3)
                .map(|k| Mp4Sample {
                    duration: 20,
                    cts_offset: 0,
                    size: 16,
                    sync: k == 0,
                })
                .collect(),
            edits: None,
            samples_per_chunk: 2,
            co64: false,
            ctts_version: 0,
            moov_first: true,
            colr_nclx: None,
            pasp: None,
        })
    };
    for (declared, picture, accepted) in [
        ((0, 0), (64, 64), true),
        ((0, 16), (64, 16), true),
        ((0, 16), (64, 17), false),
        ((16, 0), (16, 64), true),
        ((16, 0), (17, 64), false),
        ((16, 16), (16, 16), true),
    ] {
        let bytes = synthetic(declared.0, declared.1);
        let (result, sink, _) = decode_with(&bytes, ISOBMFF_PROVIDER_ID, picture.0, picture.1, 8);
        let label = format!("declared {declared:?} picture {picture:?}");
        if accepted {
            result.unwrap_or_else(|e| panic!("{label}: {e:?}"));
            assert_eq!(sink.chunks.len(), 3, "{label}");
        } else {
            assert_eq!(result.unwrap_err(), protocol, "{label}");
            assert!(sink.chunks.is_empty(), "{label}");
        }
    }
}

/// Export is exact or it fails (STU-ARC-021): a decode that delivers fewer pictures than requested is a
/// `Partial` receipt for a preview and `Incomplete { requested, delivered }` for an export. Pictures that
/// were delivered before the shortfall was known stay delivered; the error is the statement that the
/// export must not publish them. The preview result is unchanged.
#[test]
fn export_refuses_partial_video_decode() {
    for (name, bytes, container) in [
        ("mp4", MP4, ISOBMFF_PROVIDER_ID),
        ("mkv", MKV, MATROSKA_PROVIDER_ID),
    ] {
        let run = |purpose: Purpose, plan: Plan| {
            let (fake, _) = Fake::new(plan);
            let registry = Registry::standard().with_video_decoder(Arc::new(fake));
            let mut req = request(container, bytes, 0, 3, OutputMode::Decoded);
            req.purpose = purpose;
            let mut sink = Collect::default();
            let result = registry.decode_segment(
                &mut Cursor::new(bytes.to_vec()),
                None,
                &req,
                &Limits::default(),
                &CancellationToken::default(),
                &mut sink,
            );
            (result, sink)
        };

        // Control: nothing dropped, the export is exact and complete.
        let (result, sink) = run(Purpose::Export, Plan::Works);
        let receipt = result.unwrap();
        assert_eq!(receipt.disposition, Disposition::Exact, "{name}");
        assert!(receipt.complete && receipt.actual.units == 3, "{name}");
        assert_eq!(sink.chunks.len(), 3, "{name}");

        for dropped in [0, 1, 2] {
            // Preview: a partial receipt that says how many pictures came out.
            let (result, sink) = run(Purpose::Preview, Plan::DropPicture(dropped));
            let receipt = result.unwrap();
            assert_eq!(
                receipt.disposition,
                Disposition::Partial,
                "{name} {dropped}"
            );
            assert!(!receipt.complete, "{name} {dropped}");
            assert_eq!(receipt.requested.units, 3, "{name} {dropped}");
            assert_eq!(receipt.actual.units, 2, "{name} {dropped}");
            let indexes: Vec<u64> = sink.chunks.iter().map(|c| c.2).collect();
            let expected: Vec<u64> = (0..3).filter(|i| *i != dropped).collect();
            assert_eq!(indexes, expected, "{name} {dropped}");

            // Export: the typed error, the delivered pictures stay delivered.
            let (result, sink) = run(Purpose::Export, Plan::DropPicture(dropped));
            let error = result.unwrap_err();
            assert_eq!(
                error,
                ReelError::Incomplete {
                    requested: 3,
                    delivered: 2,
                },
                "{name} {dropped}"
            );
            assert_eq!(error.code(), "MEDIA_INCOMPLETE");
            assert_eq!(
                error.to_string(),
                "MEDIA_INCOMPLETE requested=3 delivered=2"
            );
            assert_eq!(sink.chunks.len(), 2, "{name} {dropped}");
            assert_eq!(
                outcome_for(&Err(error)),
                Outcome::Failure(FailureCode::Loss)
            );
        }
    }
}

/// PLR-057: a refused export opens no decoder session, so no worker process is spawned for it. The
/// exact-start check (and the request's own range check) runs before `ExternalVideoDecoder::open`; the
/// fake decoder records every open. A preview of the same start keeps its order (one session, an
/// `Approximate` receipt), and an export at a whole-frame start opens its session as before.
#[test]
fn export_not_exact_is_refused_before_a_decoder_session_opens() {
    for (name, bytes, container, pts) in [
        ("mp4", MP4, ISOBMFF_PROVIDER_ID, PTS_MP4),
        ("mkv", MKV, MATROSKA_PROVIDER_ID, PTS_MKV),
    ] {
        let (fake, seen) = Fake::new(Plan::Works);
        let registry = Registry::standard().with_video_decoder(Arc::new(fake));
        let run = |purpose: Purpose, start: u64, count: u64| {
            let mut req = request(container, bytes, start, count, OutputMode::Decoded);
            req.purpose = purpose;
            let mut sink = Collect::default();
            let result = registry.decode_segment(
                &mut Cursor::new(bytes.to_vec()),
                None,
                &req,
                &Limits::default(),
                &CancellationToken::default(),
                &mut sink,
            );
            (result, sink)
        };
        let opens = || seen.lock().unwrap().opens.len();

        // Export from the middle of frame 1: refused with the frame start as `nearest`, no session, no
        // picture.
        let inside = pts[1] + 1;
        let (result, sink) = run(Purpose::Export, inside, 1);
        assert_eq!(
            result.unwrap_err(),
            ReelError::NotExact {
                requested: Ticks::new(inside),
                nearest: Ticks::new(pts[1]),
            },
            "{name}"
        );
        assert_eq!(
            opens(),
            0,
            "{name}: a refused export opened a decoder session"
        );
        assert!(sink.chunks.is_empty(), "{name}");

        // Export past the end of the clip: the range refusal comes before a session too.
        let (result, sink) = run(Purpose::Export, pts[2] * 100, 1);
        assert!(
            matches!(result, Err(ReelError::OutOfRange { .. })),
            "{name}: {result:?}"
        );
        assert_eq!(
            opens(),
            0,
            "{name}: an out-of-range export opened a session"
        );
        assert!(sink.chunks.is_empty(), "{name}");

        // Preview of the same start: unchanged, one session and an Approximate receipt.
        let (result, sink) = run(Purpose::Preview, inside, 1);
        let receipt = result.unwrap();
        assert_eq!(receipt.disposition, Disposition::Approximate, "{name}");
        assert_eq!((opens(), sink.chunks.len()), (1, 1), "{name}");

        // Export at the frame start: exact, and it does open its session.
        let (result, sink) = run(Purpose::Export, pts[1], 1);
        let receipt = result.unwrap();
        assert_eq!(receipt.disposition, Disposition::Exact, "{name}");
        assert!(receipt.complete, "{name}");
        assert_eq!((opens(), sink.chunks.len()), (2, 1), "{name}");
    }
}

/// PLR-062 (SMB-REEL-ENG-1 R10): the export pre-check that PLR-057 put before the decoder session covered
/// the START only. It now covers the whole range: a count that runs past the end of the clip is
/// `OutOfRange`, and a range whose edit list presents one media sample more than once is
/// `Incomplete { requested, delivered }`, both before `ExternalVideoDecoder::open` (the fake records
/// every open) and before any picture. A preview keeps its order (a session opens, then the range error)
/// and reports the repeated sample as a `Partial` receipt.
#[test]
fn export_range_is_checked_before_a_decoder_session_opens() {
    // 4 samples of 20 units, two edits of 40 units from media time 0: s0 s1 s0 s1 (av01 so that it routes
    // to the decoder; the fake decodes whatever the packets hold).
    let repeating = mp4_bytes(&Mp4Spec {
        movie_timescale: 600,
        media_timescale: 600,
        codec: *b"av01",
        width: 16,
        height: 16,
        samples: (0..4)
            .map(|k| Mp4Sample {
                duration: 20,
                cts_offset: 0,
                size: 16,
                sync: k == 0,
            })
            .collect(),
        edits: Some(vec![(40, 0), (40, 0)]),
        samples_per_chunk: 2,
        co64: false,
        ctts_version: 0,
        moov_first: true,
        colr_nclx: None,
        pasp: None,
    });
    let unit = 423_360_000u64; // ticks per 1/600 s
    for (name, bytes, container, last_frame) in [
        ("mp4", MP4, ISOBMFF_PROVIDER_ID, PTS_MP4[2]),
        ("mkv", MKV, MATROSKA_PROVIDER_ID, PTS_MKV[2]),
        ("repeating", &repeating[..], ISOBMFF_PROVIDER_ID, 60 * unit),
    ] {
        let (fake, seen) = Fake::new(Plan::Works);
        let registry = Registry::standard().with_video_decoder(Arc::new(fake));
        let run = |purpose: Purpose, start: u64, count: u64| {
            let mut req = request(container, bytes, start, count, OutputMode::Decoded);
            req.purpose = purpose;
            let mut sink = Collect::default();
            let result = registry.decode_segment(
                &mut Cursor::new(bytes.to_vec()),
                None,
                &req,
                &Limits::default(),
                &CancellationToken::default(),
                &mut sink,
            );
            (result, sink)
        };
        let opens = || seen.lock().unwrap().opens.len();
        let clip_frames = if name == "repeating" { 4 } else { 3 };

        // Export from a whole-frame start with a count that runs one past the end, and from the last
        // frame with two: refused as out of range, no session, no picture, no packet sent.
        for (start, count) in [(0, clip_frames + 1), (last_frame, 2)] {
            let (result, sink) = run(Purpose::Export, start, count);
            assert!(
                matches!(result, Err(ReelError::OutOfRange { .. })),
                "{name} export {start}+{count}: {result:?}"
            );
            assert_eq!(
                opens(),
                0,
                "{name} export {start}+{count}: an export that runs past the end opened a session"
            );
            assert!(
                sink.chunks.is_empty() && seen.lock().unwrap().packets.is_empty(),
                "{name} export {start}+{count}"
            );
        }

        // The same request as a preview is unchanged: the session opens, then the range error.
        let (result, sink) = run(Purpose::Preview, 0, clip_frames + 1);
        assert!(
            matches!(result, Err(ReelError::OutOfRange { .. })),
            "{name} preview: {result:?}"
        );
        assert_eq!((opens(), sink.chunks.len()), (1, 0), "{name} preview");
        let after_preview = opens();

        if name == "repeating" {
            // Export of a range that presents a media sample twice: Incomplete with the number of
            // different media samples, and again no session.
            let (result, sink) = run(Purpose::Export, 0, 4);
            assert_eq!(
                result.unwrap_err(),
                ReelError::Incomplete {
                    requested: 4,
                    delivered: 2,
                },
                "{name}"
            );
            assert_eq!(
                opens(),
                after_preview,
                "{name}: Incomplete opened a session"
            );
            assert!(sink.chunks.is_empty(), "{name}");

            // Preview of it: one session, two different media samples decoded, the shortfall reported.
            let (result, sink) = run(Purpose::Preview, 0, 4);
            let receipt = result.unwrap();
            assert_eq!(receipt.disposition, Disposition::Partial, "{name}");
            assert!(!receipt.complete, "{name}");
            assert_eq!(
                (receipt.requested.units, receipt.actual.units),
                (4, 2),
                "{name}"
            );
            assert_eq!(
                (opens(), sink.chunks.len()),
                (after_preview + 1, 2),
                "{name}"
            );

            // Export of a range that holds every media sample once is exact and opens its session.
            let (result, sink) = run(Purpose::Export, 0, 2);
            let receipt = result.unwrap();
            assert_eq!(receipt.disposition, Disposition::Exact, "{name}");
            assert!(receipt.complete && receipt.actual.units == 2, "{name}");
            assert_eq!(
                (opens(), sink.chunks.len()),
                (after_preview + 2, 2),
                "{name}"
            );
        } else {
            // Export of the whole clip: exact, complete, and it does open its session.
            let (result, sink) = run(Purpose::Export, 0, 3);
            let receipt = result.unwrap();
            assert_eq!(receipt.disposition, Disposition::Exact, "{name}");
            assert!(receipt.complete && receipt.actual.units == 3, "{name}");
            assert_eq!((opens(), sink.chunks.len()), (2, 3), "{name}");
        }
    }
}
