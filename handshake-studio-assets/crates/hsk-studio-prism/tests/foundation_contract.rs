use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{DeliveryClass, DeliveryError, MAX_FRAME_BYTES, Outcome, SinkPort};
use hsk_studio_prism::*;
const SRGB: &[u8] = include_bytes!("profiles/sRGB-v4.icc");
const P3: &[u8] = include_bytes!("profiles/DisplayP3-v4.icc");
const PIXELS: &[[f32; 3]] = &[
    [1., 0., 0.],
    [0., 1., 0.],
    [0., 0., 1.],
    [0.2, 0.4, 0.8],
    [0., 0., 0.],
    [1., 1., 1.],
];
fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost30s deadline: {e}"),
    }
}
struct Context {
    actor: ActorContext,
    source: DomainId,
    destination: DomainId,
    resource: DomainId,
}
impl Context {
    fn new() -> Self {
        Self {
            actor: ActorContext::new(
                "account",
                "principal",
                "owner-account",
                "owner-principal",
                "space",
                "session",
            )
            .unwrap(),
            source: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(),
            destination: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000002").unwrap(),
            resource: DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000003").unwrap(),
        }
    }
    fn request<'a>(
        &'a self,
        source: &'a [u8],
        destination: &'a [u8],
        pixels: &'a [[f32; 3]],
    ) -> TransformRequest<'a> {
        TransformRequest {
            source: ProfileInput {
                profile_id: &self.source,
                bytes: source,
                expected_sha256: profile_hash(source),
            },
            destination: ProfileInput {
                profile_id: &self.destination,
                bytes: destination,
                expected_sha256: profile_hash(destination),
            },
            pixels,
            intent: Intent::RelativeColorimetric,
            bit_depth: 32,
            black_point_compensation: false,
            revision: 7,
            expected_revision: 7,
            correlation_id: 42,
            resource_id: &self.resource,
            actor: &self.actor,
            private_project_text: Some("PRIVACY-SECRET-ICC-PROJECT-DO-NOT-EMIT"),
        }
    }
}
#[derive(Default)]
struct Collector {
    frames: Vec<Vec<u8>>,
    forced: Option<DeliveryError>,
}
impl SinkPort for Collector {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        if let Some(e) = self.forced {
            return Err(e);
        }
        if class != DeliveryClass::Terminal
            || bytes.len() > MAX_FRAME_BYTES
            || !self.frames.is_empty()
        {
            return Err(DeliveryError::Saturated);
        }
        self.frames.push(bytes.to_vec());
        Ok(())
    }
}
fn reject(r: &TransformRequest<'_>, expected: Error) {
    let source = r.source.bytes.to_vec();
    let destination = r.destination.bytes.to_vec();
    let original: Vec<_> = r.pixels.iter().map(|p| p.map(f32::to_bits)).collect();
    let mut engine = Prism::new(r.actor.clone(), 1).unwrap();
    let mut sink = Collector::default();
    let error = engine
        .transform(r, &CancellationToken::default(), &mut sink)
        .unwrap_err();
    assert_eq!(error.error, expected);
    assert_eq!(error.operation_error, Some(expected));
    assert!(error.delivery.is_ok());
    assert_eq!(error.diagnostic_state.acknowledged, 1);
    assert_eq!(sink.frames.len(), 1);
    assert_eq!(
        sink.frames[0][5],
        Outcome::Failure(hsk_studio_observe::FailureCode::Validation).wire()
    );
    assert_eq!(r.source.bytes, source);
    assert_eq!(r.destination.bytes, destination);
    assert_eq!(
        r.pixels
            .iter()
            .map(|p| p.map(f32::to_bits))
            .collect::<Vec<_>>(),
        original
    );
    assert_eq!(engine.cache_stats().entries, 0);
}
// Fixture-only independent ICC oracle: signed fixed16.16 XYZ matrix and para type3.
// No moxcms parsing, matrix helpers or transfer evaluators are used below.
fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}
fn fixed(b: &[u8], at: usize) -> f64 {
    i32::from_be_bytes(b[at..at + 4].try_into().unwrap()) as f64 / 65536.
}
fn tag(b: &[u8], name: &[u8; 4]) -> (usize, usize, usize) {
    for i in 0..be32(b, 128) as usize {
        let at = 132 + i * 12;
        if &b[at..at + 4] == name {
            return (be32(b, at + 4) as usize, be32(b, at + 8) as usize, at);
        }
    }
    panic!("fixture missing tag");
}
fn matrix(b: &[u8]) -> [[f64; 3]; 3] {
    let mut m = [[0.; 3]; 3];
    for (column, name) in [b"rXYZ", b"gXYZ", b"bXYZ"].iter().enumerate() {
        let (offset, size, _) = tag(b, name);
        assert_eq!(size, 20);
        assert_eq!(&b[offset..offset + 4], b"XYZ ");
        for (row, values) in m.iter_mut().enumerate() {
            values[column] = fixed(b, offset + 8 + row * 4);
        }
    }
    m
}
fn inverse(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut a = [[0.; 6]; 3];
    for i in 0..3 {
        a[i][..3].copy_from_slice(&m[i]);
        a[i][i + 3] = 1.;
    }
    for i in 0..3 {
        let pivot = (i..3)
            .max_by(|x, y| a[*x][i].abs().total_cmp(&a[*y][i].abs()))
            .unwrap();
        a.swap(i, pivot);
        let divisor = a[i][i];
        assert!(divisor.abs() > 1e-10);
        for v in &mut a[i] {
            *v /= divisor;
        }
        let pivot_row = a[i];
        for (row, values) in a.iter_mut().enumerate() {
            if row != i {
                let amount = values[i];
                for (value, pivot_value) in values.iter_mut().zip(pivot_row) {
                    *value -= amount * pivot_value;
                }
            }
        }
    }
    std::array::from_fn(|i| [a[i][3], a[i][4], a[i][5]])
}
fn curve(b: &[u8]) -> [f64; 5] {
    let (offset, size, _) = tag(b, b"rTRC");
    assert_eq!(size, 32);
    assert_eq!(&b[offset..offset + 4], b"para");
    assert_eq!(
        u16::from_be_bytes(b[offset + 8..offset + 10].try_into().unwrap()),
        3
    );
    std::array::from_fn(|i| fixed(b, offset + 12 + i * 4))
}
fn oracle(source: &[u8], destination: &[u8], pixel: [f32; 3]) -> [f64; 3] {
    let [g, a, b, c, d] = curve(source);
    let linear = pixel.map(|x| {
        let x = x as f64;
        if x >= d { (a * x + b).powf(g) } else { c * x }
    });
    let src = matrix(source);
    let xyz: [f64; 3] = std::array::from_fn(|r| (0..3).map(|c| src[r][c] * linear[c]).sum());
    let dst = inverse(matrix(destination));
    let out: [f64; 3] = std::array::from_fn(|r| (0..3).map(|c| dst[r][c] * xyz[c]).sum());
    let [g, a, b, c, d] = curve(destination);
    let threshold = (a * d + b).powf(g);
    out.map(|y| {
        if y >= threshold {
            (y.max(0.).powf(1. / g) - b) / a
        } else {
            y / c
        }
    })
}
#[test]
fn external_consumer_nonidentity_independent_icc_oracle_and_receipt() {
    bounded_case("nonidentity", || {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Prism>();
        assert_send_sync::<Collector>();
        assert_eq!(
            hash_hex(&profile_hash(SRGB)),
            "c56e1685d888f5edb92fe07f2750f387f8fe8e91b32ff8fb0b56bfbbb9458353"
        );
        assert_eq!(
            hash_hex(&profile_hash(P3)),
            "cb51de38e482ee974c0c76b9689e16aad04bad16e226fed2f30c842d15ff3a3d"
        );
        let c = Context::new();
        let original = PIXELS.to_vec();
        let request = c.request(SRGB, P3, PIXELS);
        let mut engine = Prism::new(c.actor.clone(), 1).unwrap();
        let mut sink = Collector::default();
        let out = engine
            .transform(&request, &CancellationToken::default(), &mut sink)
            .unwrap();
        for (pixel, actual) in PIXELS.iter().zip(&out.pixels) {
            let expected = oracle(SRGB, P3, *pixel);
            for i in 0..3 {
                assert!(
                    (actual[i] as f64 - expected[i]).abs() <= 0.0001,
                    "actual={actual:?} expected={expected:?}"
                );
            }
        }
        assert!((out.pixels[0][1] - PIXELS[0][1]).abs() > 0.1);
        assert_eq!(PIXELS, original);
        assert_eq!(out.receipt.engine_version, "0.9.1");
        assert_eq!(out.receipt.engine_name, "moxcms");
        assert_eq!(out.receipt.source_profile_id, c.source);
        assert_eq!(out.receipt.destination_sha256, profile_hash(P3));
        assert_eq!(out.receipt.revision, 7);
        assert_eq!(out.receipt.correlation_id, 42);
        assert!(out.receipt.analytical);
        assert!(!out.receipt.cache_hit);
        assert_eq!(out.delivery.outcome, Outcome::Success);
        let frame = &sink.frames[0];
        assert_eq!(&frame[..4], b"HSKO");
        assert_eq!(frame[5], 2);
        assert_eq!(u64::from_be_bytes(frame[16..24].try_into().unwrap()), 7);
        let mut at = 48;
        let mut strings = Vec::new();
        for _ in 0..7 {
            let n = u16::from_be_bytes(frame[at..at + 2].try_into().unwrap()) as usize;
            at += 2;
            strings.push(std::str::from_utf8(&frame[at..at + n]).unwrap());
            at += n;
        }
        assert_eq!(at, frame.len());
        assert_eq!(strings[0], c.resource.as_str());
        assert_eq!(strings[1], c.actor.account_id());
        assert!(!String::from_utf8_lossy(frame).contains("PRIVACY-SECRET"));
        let mut without_private = c.request(SRGB, P3, PIXELS);
        without_private.private_project_text = None;
        let mut baseline = Collector::default();
        engine
            .transform(
                &without_private,
                &CancellationToken::default(),
                &mut baseline,
            )
            .unwrap();
        assert_eq!(baseline.frames, sink.frames);
        eprintln!(
            "nonidentity RGB {:?}; moxcms{}; hashes verified; independent ICC f64 oracle tolerance0.0001",
            out.pixels[0], ENGINE_VERSION
        );
    });
}
#[test]
fn stale_revision_rejected_immutable() {
    bounded_case("stale", || {
        let c = Context::new();
        let mut r = c.request(SRGB, P3, PIXELS);
        r.expected_revision = 8;
        reject(&r, Error::RevisionMismatch);
    });
}
#[test]
fn wrong_depth_intent_and_bpc_rejected() {
    bounded_case("depth-intent-bpc", || {
        let c = Context::new();
        let mut r = c.request(SRGB, P3, PIXELS);
        for d in [8, 16, 64] {
            r.bit_depth = d;
            reject(&r, Error::UnsupportedDepth);
        }
        r.bit_depth = 32;
        r.intent = Intent::Perceptual;
        reject(&r, Error::UnsupportedIntent);
        r.intent = Intent::RelativeColorimetric;
        r.black_point_compensation = true;
        reject(&r, Error::UnsupportedBpc);
    });
}
#[test]
fn malformed_profile_rejected_without_identity() {
    bounded_case("malformed", || {
        let c = Context::new();
        let mut bytes = SRGB.to_vec();
        bytes[36..40].copy_from_slice(b"FAIL");
        reject(&c.request(&bytes, P3, PIXELS), Error::MalformedProfile);
        reject(
            &c.request(&SRGB[..100], P3, PIXELS),
            Error::MalformedProfile,
        );
    });
}
#[test]
fn wrong_hash_and_identity_rejected() {
    bounded_case("hash-id", || {
        let c = Context::new();
        let mut r = c.request(SRGB, P3, PIXELS);
        r.source.expected_sha256 = [0; 32];
        reject(&r, Error::HashMismatch);
        r.source.expected_sha256 = profile_hash(SRGB);
        r.source.profile_id = &c.resource;
        reject(&r, Error::InvalidProfileId);
    });
}
#[test]
fn unsupported_profile_class_rejected() {
    bounded_case("unsupported", || {
        let c = Context::new();
        let mut bytes = SRGB.to_vec();
        bytes[12..16].copy_from_slice(b"abst");
        reject(&c.request(&bytes, P3, PIXELS), Error::UnsupportedProfile);
    });
}
#[test]
fn cicp_excluded_before_analytical_construction() {
    bounded_case("cicp", || {
        let c = Context::new();
        let n = be32(SRGB, 128) as usize;
        let table_end = 132 + n * 12;
        let mut b = SRGB[..table_end].to_vec();
        b.extend_from_slice(b"cicp");
        b.extend_from_slice(&((SRGB.len() + 12) as u32).to_be_bytes());
        b.extend_from_slice(&12u32.to_be_bytes());
        b.extend_from_slice(&SRGB[table_end..]);
        b.extend_from_slice(b"cicp\0\0\0\0\x01\x0d\0\x01");
        for i in 0..n {
            let at = 132 + i * 12 + 4;
            let old = be32(&b, at);
            b[at..at + 4].copy_from_slice(&(old + 12).to_be_bytes());
        }
        b[128..132].copy_from_slice(&((n + 1) as u32).to_be_bytes());
        let size = b.len() as u32;
        b[..4].copy_from_slice(&size.to_be_bytes());
        reject(&c.request(&b, P3, PIXELS), Error::UnsupportedProfile);
    });
}
#[test]
fn singular_matrix_and_mismatched_curves_rejected() {
    bounded_case("matrix-curves", || {
        let c = Context::new();
        let mut b = SRGB.to_vec();
        let (offset, _, _) = tag(&b, b"rXYZ");
        b[offset + 8..offset + 20].fill(0);
        reject(&c.request(&b, P3, PIXELS), Error::SingularMatrix);
        let mut b = SRGB.to_vec();
        let (offset, size, entry) = tag(&b, b"gTRC");
        let extra = b[offset..offset + size].to_vec();
        let start = b.len();
        b.extend_from_slice(&extra);
        b[entry + 4..entry + 8].copy_from_slice(&(start as u32).to_be_bytes());
        b[start + 12..start + 16].copy_from_slice(&(3i32 * 65536).to_be_bytes());
        let size = b.len() as u32;
        b[..4].copy_from_slice(&size.to_be_bytes());
        reject(&c.request(&b, P3, PIXELS), Error::InvalidCurve);
    });
}
#[test]
fn nonfinite_and_out_of_range_channels_rejected() {
    bounded_case("nonfinite", || {
        let c = Context::new();
        for x in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let p = [[x, 0.4, 0.5]];
            reject(&c.request(SRGB, P3, &p), Error::NonfiniteChannel);
        }
        reject(&c.request(SRGB, P3, &[[1.01, 0., 0.]]), Error::ChannelRange);
    });
}
#[test]
fn cancellation_delivered_without_numeric_success() {
    bounded_case("cancel", || {
        let c = Context::new();
        let r = c.request(SRGB, P3, PIXELS);
        let token = CancellationToken::default();
        token.cancel();
        let mut engine = Prism::new(c.actor.clone(), 1).unwrap();
        let mut sink = Collector::default();
        let e = engine.transform(&r, &token, &mut sink).unwrap_err();
        assert_eq!(e.error, Error::Canceled);
        assert_eq!(e.delivery.unwrap().outcome, Outcome::Canceled);
        assert_eq!(e.diagnostic_state.canceled_delivered, 1);
        assert_eq!(engine.cache_stats().entries, 0);
        assert_eq!(PIXELS, r.pixels);
    });
}
#[test]
fn delivery_rejection_and_indeterminate_withhold_output() {
    bounded_case("delivery", || {
        let c = Context::new();
        let r = c.request(SRGB, P3, PIXELS);
        for forced in [
            DeliveryError::Rejected,
            DeliveryError::Saturated,
            DeliveryError::Unavailable,
            DeliveryError::Indeterminate,
        ] {
            let mut engine = Prism::new(c.actor.clone(), 1).unwrap();
            let mut sink = Collector {
                forced: Some(forced),
                ..Default::default()
            };
            let e = engine
                .transform(&r, &CancellationToken::default(), &mut sink)
                .unwrap_err();
            assert_eq!(e.error, Error::DeliveryFailed);
            assert_eq!(e.operation_error, None);
            assert_eq!(
                e.delivery.unwrap_err(),
                hsk_studio_observe::Error::Delivery(forced)
            );
            assert_eq!(e.diagnostic_state.dropped_attempts, 1);
            assert_eq!(
                e.diagnostic_state.reconciliation_required,
                forced == DeliveryError::Indeterminate
            );
            assert!(sink.frames.is_empty());
        }
    });
}
#[test]
fn cache_scope_capacity_hit_and_explicit_recovery() {
    bounded_case("cache", || {
        let c = Context::new();
        let mut engine = Prism::new(c.actor.clone(), 1).unwrap();
        let r = c.request(SRGB, P3, PIXELS);
        let first = engine
            .transform(&r, &CancellationToken::default(), &mut Collector::default())
            .unwrap();
        let second = engine
            .transform(&r, &CancellationToken::default(), &mut Collector::default())
            .unwrap();
        assert!(second.receipt.cache_hit);
        assert_eq!(first.pixels, second.pixels);
        assert_eq!(engine.cache_stats().hits, 1);
        let reverse = c.request(P3, SRGB, PIXELS);
        assert_eq!(
            engine
                .transform(
                    &reverse,
                    &CancellationToken::default(),
                    &mut Collector::default()
                )
                .unwrap_err()
                .error,
            Error::CacheFull
        );
        assert_eq!(engine.cache_stats().entries, 1);
        engine.clear_cache();
        assert_eq!(engine.cache_stats().entries, 0);
        assert!(
            engine
                .transform(
                    &reverse,
                    &CancellationToken::default(),
                    &mut Collector::default()
                )
                .is_ok()
        );
        let mut other = c.request(SRGB, P3, PIXELS);
        let actor = ActorContext::new(
            "other",
            "principal",
            "owner-account",
            "owner-principal",
            "space",
            "session",
        )
        .unwrap();
        other.actor = &actor;
        assert_eq!(
            engine
                .transform(
                    &other,
                    &CancellationToken::default(),
                    &mut Collector::default()
                )
                .unwrap_err()
                .error,
            Error::ContextMismatch
        );
    });
}
#[test]
fn bounded_resources_and_shared_descriptor() {
    bounded_case("bounds-descriptor", || {
        let c = Context::new();
        assert!(matches!(
            Prism::new(c.actor.clone(), 0),
            Err(Error::InvalidBudget)
        ));
        assert!(matches!(
            Prism::new(c.actor.clone(), MAX_CACHE_ENTRIES + 1),
            Err(Error::InvalidBudget)
        ));
        let bytes = vec![0; MAX_PROFILE_BYTES + 1];
        reject(&c.request(&bytes, P3, PIXELS), Error::ProfileLimit);
        let pixels = vec![[0.; 3]; MAX_PIXELS + 1];
        reject(&c.request(SRGB, P3, &pixels), Error::PixelLimit);
        reject(&c.request(SRGB, P3, &[]), Error::PixelLimit);
        assert!(DESCRIPTOR.contains("same descriptor"));
        assert!(DESCRIPTOR.contains("cross-host bit-identical promotion"));
        assert!(DESCRIPTOR.contains("65536"));
    });
}

const LINEAR_SRGB: &[u8] = include_bytes!("profiles/sRGB-linear-v4.icc");
const LINEAR_P3: &[u8] = include_bytes!("profiles/DisplayP3-linear-v4.icc");
#[test]
fn validated_transfer_inspection_identity_hash_and_encoded() {
    bounded_case("inspection", || {
        let c = Context::new();
        let engine = Prism::new(c.actor.clone(), 1).unwrap();
        for (bytes, expected) in [
            (LINEAR_SRGB, Transfer::LinearLight),
            (LINEAR_P3, Transfer::LinearLight),
            (SRGB, Transfer::Encoded),
            (P3, Transfer::Encoded),
        ] {
            let input = ProfileInput {
                profile_id: &c.source,
                bytes,
                expected_sha256: profile_hash(bytes),
            };
            let d = engine
                .inspect_profile(input, &CancellationToken::default())
                .unwrap();
            assert_eq!(d.transfer(), expected);
            assert_eq!(d.profile_id(), &c.source);
            assert_eq!(d.sha256(), &profile_hash(bytes));
            assert_eq!(d.engine_name(), ENGINE_NAME);
            assert_eq!(d.engine_version(), ENGINE_VERSION);
            assert!(d.matches(input));
            assert!(!d.matches(ProfileInput {
                profile_id: &c.destination,
                ..input
            }));
            assert!(!d.matches(ProfileInput {
                expected_sha256: [0; 32],
                ..input
            }));
            assert!(!d.matches(ProfileInput {
                bytes: b"changed",
                ..input
            }));
        }
        assert_eq!(engine.cache_stats().entries, 0);
    });
}
#[test]
fn inspection_rejects_malformed_hash_unsupported_and_cancel_without_mutation() {
    bounded_case("inspection_reject", || {
        let c = Context::new();
        let engine = Prism::new(c.actor.clone(), 1).unwrap();
        let original = LINEAR_SRGB.to_vec();
        let input = ProfileInput {
            profile_id: &c.source,
            bytes: LINEAR_SRGB,
            expected_sha256: profile_hash(LINEAR_SRGB),
        };
        let token = CancellationToken::default();
        assert_eq!(
            engine
                .inspect_profile(
                    ProfileInput {
                        expected_sha256: [0; 32],
                        ..input
                    },
                    &token
                )
                .unwrap_err(),
            Error::HashMismatch
        );
        let malformed = b"not ICC";
        assert_eq!(
            engine
                .inspect_profile(
                    ProfileInput {
                        bytes: malformed,
                        expected_sha256: profile_hash(malformed),
                        ..input
                    },
                    &token
                )
                .unwrap_err(),
            Error::MalformedProfile
        );
        let mut unsupported = original.clone();
        unsupported[8] = 2;
        assert_eq!(
            engine
                .inspect_profile(
                    ProfileInput {
                        bytes: &unsupported,
                        expected_sha256: profile_hash(&unsupported),
                        ..input
                    },
                    &token
                )
                .unwrap_err(),
            Error::UnsupportedProfile
        );
        token.cancel();
        assert_eq!(
            engine.inspect_profile(input, &token).unwrap_err(),
            Error::Canceled
        );
        assert_eq!(LINEAR_SRGB, original);
        assert_eq!(engine.cache_stats().entries, 0);
    });
}
// Linear derivative oracle: only decoded ICC XYZ fixed16.16 matrices, independent f64 solve.
fn linear_oracle(source: &[u8], destination: &[u8], pixel: [f32; 3]) -> [f64; 3] {
    for bytes in [source, destination] {
        for name in [b"rTRC", b"gTRC", b"bTRC"] {
            let (offset, size, _) = tag(bytes, name);
            assert_eq!(size, 16);
            assert_eq!(&bytes[offset..offset + 4], b"para");
            assert_eq!(&bytes[offset + 8..offset + 12], &[0; 4]);
            assert_eq!(fixed(bytes, offset + 12), 1.0);
        }
    }
    let src = matrix(source);
    let dst = inverse(matrix(destination));
    let xyz: [f64; 3] =
        std::array::from_fn(|r| (0..3).map(|c| src[r][c] * f64::from(pixel[c])).sum());
    std::array::from_fn(|r| (0..3).map(|c| dst[r][c] * xyz[c]).sum())
}
#[test]
fn linear_cross_profile_both_directions_independent_matrix_oracle_unclamped() {
    bounded_case("linear_transform", || {
        assert_eq!(
            hash_hex(&profile_hash(LINEAR_SRGB)),
            "4813c25a76abcf572f146c5a36b56ed83526c82327ee9f863553bf1e99c8b64d"
        );
        assert_eq!(
            hash_hex(&profile_hash(LINEAR_P3)),
            "138ca6af5c8709c8dab4123a48ba2509ac8c89e53004f0909fc948f87f37f3f7"
        );
        let c = Context::new();
        let mut nonidentity = false;
        let mut outside_unit = false;
        for (source, destination) in [(LINEAR_SRGB, LINEAR_P3), (LINEAR_P3, LINEAR_SRGB)] {
            let original = PIXELS.to_vec();
            let request = c.request(source, destination, PIXELS);
            let mut engine = Prism::new(c.actor.clone(), 1).unwrap();
            let mut sink = Collector::default();
            let result = engine
                .transform(&request, &CancellationToken::default(), &mut sink)
                .unwrap();
            for (input, actual) in PIXELS.iter().zip(&result.pixels) {
                let expected = linear_oracle(source, destination, *input);
                for channel in 0..3 {
                    assert!(
                        (f64::from(actual[channel]) - expected[channel]).abs() < 0.0001,
                        "actual={actual:?} expected={expected:?}"
                    );
                    nonidentity |= (actual[channel] - input[channel]).abs() > 0.01;
                    outside_unit |= actual[channel] < -0.01 || actual[channel] > 1.01;
                }
            }
            assert_eq!(result.receipt.source_sha256, profile_hash(source));
            assert_eq!(result.receipt.destination_sha256, profile_hash(destination));
            assert!(!result.receipt.options.clamped_output);
            assert_eq!(result.receipt.engine_version, ENGINE_VERSION);
            assert_eq!(result.receipt.intent, Intent::RelativeColorimetric);
            assert_eq!(result.receipt.bit_depth, 32);
            assert!(!result.receipt.black_point_compensation);
            assert_eq!(PIXELS, original);
            assert_eq!(sink.frames.len(), 1);
        }
        assert!(nonidentity);
        assert!(outside_unit);
    });
}

const ADMITTED_SRGB: &[u8] = include_bytes!("profiles/sRGB-admitted-linear-v4.icc");
const ADMITTED_P3: &[u8] = include_bytes!("profiles/DisplayP3-admitted-linear-v4.icc");
#[derive(Default)]
struct AllocationLedger {
    live: std::sync::atomic::AtomicU64,
    calls: std::sync::atomic::AtomicUsize,
    deny: std::sync::atomic::AtomicUsize,
    refuse_retire: std::sync::atomic::AtomicBool,
}
#[derive(Clone)]
struct AllocationAdmission {
    ledger: std::sync::Arc<AllocationLedger>,
    supported: bool,
}
struct AllocationLease {
    ledger: std::sync::Arc<AllocationLedger>,
    bytes: u64,
}
impl Drop for AllocationLease {
    fn drop(&mut self) {
        self.ledger
            .live
            .fetch_sub(self.bytes, std::sync::atomic::Ordering::SeqCst);
    }
}
impl ProviderReservation for AllocationLease {
    fn reserved_bytes(&self) -> u64 {
        self.bytes
    }
    fn retirement_ready(&self) -> Result<(), Error> {
        if self
            .ledger
            .refuse_retire
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            Err(Error::RetirementUnavailable)
        } else {
            Ok(())
        }
    }
}
impl ProviderAdmission for AllocationAdmission {
    type Reservation = AllocationLease;
    fn synchronous_retirement(&self) -> bool {
        self.supported
    }
    fn reserve(&self, bytes: u64) -> Result<AllocationLease, Error> {
        use std::sync::atomic::Ordering::SeqCst;
        let call = self.ledger.calls.fetch_add(1, SeqCst) + 1;
        if self.ledger.deny.load(SeqCst) == call {
            return Err(Error::AdmissionDenied);
        }
        self.ledger.live.fetch_add(bytes, SeqCst);
        Ok(AllocationLease {
            ledger: self.ledger.clone(),
            bytes,
        })
    }
}
fn allocation_admission() -> AllocationAdmission {
    AllocationAdmission {
        ledger: std::sync::Arc::new(AllocationLedger::default()),
        supported: true,
    }
}
fn live(a: &AllocationAdmission) -> u64 {
    a.ledger.live.load(std::sync::atomic::Ordering::SeqCst)
}

#[test]
fn admitted_cross_profile_oracle_cache_and_owned_result_lifetimes() {
    bounded_case("admitted_matrix_and_lifetimes", || {
        assert_eq!(
            hash_hex(&profile_hash(ADMITTED_SRGB)),
            "63fc75efa59e9499411634a4298e48fc0f45d073f214a1c1de35fd76241aed27"
        );
        assert_eq!(
            hash_hex(&profile_hash(ADMITTED_P3)),
            "c94737d225dec435f0119910080395e63d5efc6a6cbe594507d0c84544becfde"
        );
        for (source, destination) in [(ADMITTED_SRGB, ADMITTED_P3), (ADMITTED_P3, ADMITTED_SRGB)] {
            let c = Context::new();
            let a = allocation_admission();
            let mut engine = AdmittedPrism::new(&c.actor, 1, a.clone()).unwrap();
            let base = live(&a);
            assert_eq!(base, engine.provider_reserved_bytes());
            let request = c.request(source, destination, PIXELS);
            let descriptor = engine
                .inspect_profile(request.source, &CancellationToken::default())
                .unwrap();
            assert_eq!(descriptor.descriptor().transfer(), Transfer::LinearLight);
            assert_eq!(live(&a), base + descriptor.reserved_bytes());
            drop(descriptor);
            assert_eq!(live(&a), base);
            let first = engine
                .transform(
                    &request,
                    &CancellationToken::default(),
                    &mut Collector::default(),
                )
                .unwrap();
            let mut different = false;
            for (input, output) in PIXELS.iter().zip(&first.result().pixels) {
                let expected = linear_oracle(source, destination, *input);
                for i in 0..3 {
                    assert!((f64::from(output[i]) - expected[i]).abs() < 0.0001);
                }
                different |= input
                    .iter()
                    .zip(output)
                    .any(|(a, b)| (*a - *b).abs() > 0.01);
            }
            assert!(different);
            assert!(!first.result().receipt.cache_hit);
            let cached = live(&a) - base - first.reserved_bytes();
            assert_eq!(cached, analytical_executor_bytes().unwrap());
            let second = engine
                .transform(
                    &request,
                    &CancellationToken::default(),
                    &mut Collector::default(),
                )
                .unwrap();
            assert!(second.result().receipt.cache_hit);
            assert_eq!(
                live(&a),
                base + cached + first.reserved_bytes() + second.reserved_bytes()
            );
            engine.try_clear_cache().unwrap();
            assert_eq!(
                live(&a),
                base + first.reserved_bytes() + second.reserved_bytes()
            );
            drop(engine);
            assert_eq!(live(&a), first.reserved_bytes() + second.reserved_bytes());
            assert_eq!(first.result().receipt.source_sha256, profile_hash(source));
            drop(first);
            drop(second);
            assert_eq!(live(&a), 0);
        }
    });
}
#[test]
fn admitted_denial_precedes_parser_and_partial_admission_rolls_back() {
    bounded_case("admitted_denial", || {
        use std::sync::atomic::Ordering::SeqCst;
        for denied_call in 1..=4 {
            let c = Context::new();
            let a = allocation_admission();
            a.ledger.deny.store(denied_call, SeqCst);
            let construction = AdmittedPrism::new(&c.actor, 1, a.clone());
            if denied_call == 1 {
                assert!(matches!(construction, Err(Error::AdmissionDenied)));
                assert_eq!(live(&a), 0);
                continue;
            }
            let mut engine = construction.unwrap();
            let base = live(&a);
            // Structurally bounded but singular matrix: parser would fail if called.
            let mut singular = ADMITTED_SRGB.to_vec();
            for name in [b"rXYZ", b"gXYZ", b"bXYZ"] {
                let (offset, _, _) = tag(&singular, name);
                singular[offset + 8..offset + 20].fill(0);
            }
            let r = c.request(&singular, ADMITTED_P3, PIXELS);
            let mut sink = Collector::default();
            assert!(matches!(
                engine.transform(&r, &CancellationToken::default(), &mut sink),
                Err(Error::AdmissionDenied)
            ));
            assert_eq!(live(&a), base);
            assert!(sink.frames.is_empty());
            assert_eq!(engine.cache_stats().entries, 0);
            drop(engine);
            assert_eq!(live(&a), 0);
        }
        let c = Context::new();
        let mut a = allocation_admission();
        a.supported = false;
        assert!(matches!(
            AdmittedPrism::new(&c.actor, 1, a.clone()),
            Err(Error::UnsupportedAdmission)
        ));
        assert_eq!(a.ledger.calls.load(SeqCst), 0);
        assert_eq!(live(&a), 0);
    });
}
#[test]
fn admitted_metadata_cancel_delivery_and_retirement_refusal_preserve_ownership() {
    bounded_case("admitted_refusals", || {
        use std::sync::atomic::Ordering::SeqCst;
        let c = Context::new();
        let a = allocation_admission();
        let mut engine = AdmittedPrism::new(&c.actor, 1, a.clone()).unwrap();
        let base = live(&a);
        let legacy = c.request(LINEAR_SRGB, LINEAR_P3, PIXELS);
        let calls = a.ledger.calls.load(SeqCst);
        assert!(matches!(
            engine.inspect_profile(legacy.source, &CancellationToken::default()),
            Err(Error::UnsupportedProfile)
        ));
        assert_eq!(a.ledger.calls.load(SeqCst), calls);
        let r = c.request(ADMITTED_SRGB, ADMITTED_P3, PIXELS);
        let canceled = CancellationToken::default();
        canceled.cancel();
        assert!(matches!(
            engine.transform(&r, &canceled, &mut Collector::default()),
            Err(Error::Canceled)
        ));
        assert_eq!(live(&a), base);
        for forced in [
            DeliveryError::Rejected,
            DeliveryError::Unavailable,
            DeliveryError::Indeterminate,
        ] {
            let mut sink = Collector {
                frames: Vec::new(),
                forced: Some(forced),
            };
            assert!(
                matches!(engine.transform(&r,&CancellationToken::default(),&mut sink),Err(Error::DiagnosticDelivery(hsk_studio_observe::Error::Delivery(e))) if e==forced)
            );
            assert_eq!(live(&a), base);
            assert_eq!(engine.cache_stats().entries, 0);
        }
        let result = engine
            .transform(&r, &CancellationToken::default(), &mut Collector::default())
            .unwrap();
        drop(result);
        let retained = live(&a);
        a.ledger.refuse_retire.store(true, SeqCst);
        assert_eq!(engine.try_clear_cache(), Err(Error::RetirementUnavailable));
        assert_eq!(engine.cache_stats().entries, 1);
        assert_eq!(live(&a), retained);
        let engine = engine
            .try_retire()
            .expect("refusal returns entire provider");
        assert_eq!(engine.cache_stats().entries, 1);
        assert_eq!(live(&a), retained);
        a.ledger.refuse_retire.store(false, SeqCst);
        assert!(engine.try_retire().is_none());
        assert_eq!(live(&a), 0);
    });
}

struct CancelingAdmission {
    owner: AllocationAdmission,
    token: CancellationToken,
    cancel_call: usize,
}
impl ProviderAdmission for CancelingAdmission {
    type Reservation = AllocationLease;
    fn synchronous_retirement(&self) -> bool {
        true
    }
    fn reserve(&self, bytes: u64) -> Result<AllocationLease, Error> {
        let lease = self.owner.reserve(bytes)?;
        if self
            .owner
            .ledger
            .calls
            .load(std::sync::atomic::Ordering::SeqCst)
            == self.cancel_call
        {
            self.token.cancel();
        }
        Ok(lease)
    }
}
#[test]
fn admitted_cancellation_during_partial_admission_retires_every_token() {
    bounded_case("admitted_partial_cancel", || {
        for cancel_call in 2..=4 {
            let c = Context::new();
            let owner = allocation_admission();
            let token = CancellationToken::default();
            let a = CancelingAdmission {
                owner: owner.clone(),
                token: token.clone(),
                cancel_call,
            };
            let mut engine = AdmittedPrism::new(&c.actor, 1, a).unwrap();
            let base = live(&owner);
            let request = c.request(ADMITTED_SRGB, ADMITTED_P3, PIXELS);
            let mut sink = Collector::default();
            assert!(matches!(
                engine.transform(&request, &token, &mut sink),
                Err(Error::Canceled)
            ));
            assert_eq!(live(&owner), base);
            assert_eq!(engine.cache_stats().entries, 0);
            assert!(sink.frames.is_empty());
            assert_eq!(
                owner.ledger.calls.load(std::sync::atomic::Ordering::SeqCst),
                cancel_call
            );
            drop(engine);
            assert_eq!(live(&owner), 0);
        }
    });
}
