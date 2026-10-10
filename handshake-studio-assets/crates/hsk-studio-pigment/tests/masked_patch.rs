use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_folio::{ContentDigest, GeometryUnit, Length, TileRef, TileSchema};
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort, TileAddress};
use hsk_studio_pigment::{
    caller::{MemoryAdmission, SerializedOwner},
    *,
};
use hsk_studio_prism::{ProfileInput, profile_hash};
const LINEAR: &[u8] =
    include_bytes!("../../hsk-studio-prism/tests/profiles/sRGB-admitted-linear-v4.icc");
const P3: &[u8] =
    include_bytes!("../../hsk-studio-prism/tests/profiles/DisplayP3-admitted-linear-v4.icc");
const ENCODED: &[u8] = include_bytes!("../../hsk-studio-prism/tests/profiles/sRGB-v4.icc");
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

fn linear_oracle(source: &[u8], destination: &[u8], pixel: [f32; 3]) -> [f64; 3] {
    let src = matrix(source);
    let dst = inverse(matrix(destination));
    let xyz: [f64; 3] =
        std::array::from_fn(|r| (0..3).map(|c| src[r][c] * f64::from(pixel[c])).sum());
    std::array::from_fn(|r| (0..3).map(|c| dst[r][c] * xyz[c]).sum())
}
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
    match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost20s deadline:{e}"),
    }
}
#[derive(Default)]
struct Sink {
    frames: Vec<Vec<u8>>,
    failure: Option<DeliveryError>,
    cancel: Option<CancellationToken>,
}
impl SinkPort for Sink {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        assert_eq!(class, DeliveryClass::Terminal);
        if let Some(token) = &self.cancel {
            token.cancel()
        }
        if let Some(error) = self.failure {
            return Err(error);
        }
        self.frames.push(bytes.to_vec());
        Ok(())
    }
}
struct Fixture {
    actor: ActorContext,
    document: DomainId,
    layer: DomainId,
    profile: DomainId,
    tile: TileRef,
    before: Vec<u8>,
    replacement: Vec<u8>,
    alpha0: Vec<u8>,
    alpha1: Vec<u8>,
    mask: Vec<u8>,
}
impl Fixture {
    fn new() -> Self {
        let actor = ActorContext::new("a", "p", "oa", "op", "s", "session").unwrap();
        let document = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
        let layer = DomainId::parse("SLYR-019abcde-0000-7000-8000-000000000002").unwrap();
        let profile = DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000003").unwrap();
        let tile = TileRef {
            object_key: "tile_0".into(),
            layer_id: layer.as_str().into(),
            column: 0,
            row: 0,
            width: Length {
                value: 2.,
                unit: GeometryUnit::Px,
            },
            height: Length {
                value: 2.,
                unit: GeometryUnit::Px,
            },
            format: "rgb_f32le".into(),
            colour_profile_id: profile.as_str().into(),
            schema_id: TileSchema::V1,
            content_digest: ContentDigest {
                algorithm: "sha256".into(),
                digest: "artifact-stream-not-decoded-hash".into(),
            },
            artifact_manifest_id: "caller-artifact".into(),
        };
        let mut before = vec![0xAA; 56];
        let mut replacement = vec![0xBB; 56];
        let mut alpha0 = vec![0xCC; 24];
        let mut alpha1 = vec![0xDD; 24];
        for y in 0..2 {
            for x in 0..2 {
                for (i, v) in [0.2f32, 0.4, 0.6].iter().enumerate() {
                    let at = y * 28 + x * 12 + i * 4;
                    before[at..at + 4].copy_from_slice(&v.to_le_bytes())
                }
                for (i, v) in [0.8f32, 0.6, 0.4].iter().enumerate() {
                    let at = y * 28 + x * 12 + i * 4;
                    replacement[at..at + 4].copy_from_slice(&v.to_le_bytes())
                }
                alpha0[y * 12 + x * 4..y * 12 + x * 4 + 4].copy_from_slice(&0.25f32.to_le_bytes());
                alpha1[y * 12 + x * 4..y * 12 + x * 4 + 4].copy_from_slice(&0.75f32.to_le_bytes())
            }
        }
        Self {
            actor,
            document,
            layer,
            profile,
            tile,
            before,
            replacement,
            alpha0,
            alpha1,
            mask: vec![0, 128, 0xEE, 0, 0, 0xEF],
        }
    }
    fn tile<'a>(&'a self, replacement: bool) -> ResolvedTile<'a> {
        let (c, a) = if replacement {
            (&self.replacement, &self.alpha1)
        } else {
            (&self.before, &self.alpha0)
        };
        ResolvedTile {
            tile_ref: &self.tile,
            bounds: Rect {
                x: -2,
                y: -1,
                width: 2,
                height: 2,
            },
            sample_format: "rgb_f32le",
            colour_bytes: c,
            colour_stride_bytes: 28,
            colour_sha256: profile_hash(c),
            alpha_bytes: a,
            alpha_stride_bytes: 12,
            alpha_sha256: profile_hash(a),
            transfer: "linear_light",
            alpha_association: "straight",
            profile: ProfileInput {
                profile_id: &self.profile,
                bytes: LINEAR,
                expected_sha256: profile_hash(LINEAR),
            },
        }
    }
    fn item(&self) -> TileItem<'_> {
        TileItem {
            before: self.tile(false),
            replacement: self.tile(true),
            expected_property_revision: 7,
        }
    }
    fn request<'a>(&'a self, items: &'a [TileItem<'a>]) -> Request<'a> {
        Request {
            transport_version: 1,
            operation: "masked_replace",
            document_id: &self.document,
            layer_id: &self.layer,
            command_id: "source-patch",
            correlation_id: 5,
            actor: &self.actor,
            base_revision: 200,
            cancel_epoch: 4,
            grid: Grid {
                origin_x: -2,
                origin_y: -1,
                tile_width: 2,
                tile_height: 2,
            },
            rectangle: Rect {
                x: -2,
                y: -1,
                width: 2,
                height: 2,
            },
            mask: Mask {
                bounds: Rect {
                    x: -2,
                    y: -1,
                    width: 2,
                    height: 2,
                },
                sample_format: "coverage_u8",
                bytes: &self.mask,
                stride_bytes: 3,
                sha256: profile_hash(&self.mask),
            },
            tiles: items,
            byte_limit: 1024 * 1024,
        }
    }
    fn owner(&self, sink: Sink) -> SerializedOwner<Sink> {
        SerializedOwner::new(
            self.document.clone(),
            self.layer.clone(),
            self.actor.clone(),
            4,
            vec![(
                TileAddress::new(self.layer.clone(), "tile_0", 0, 0).unwrap(),
                7,
            )],
            MemoryAdmission::new(1024 * 1024).unwrap(),
            sink,
        )
        .unwrap()
    }
    fn fingerprint(&self) -> ([u8; 32], [u8; 32], [u8; 32]) {
        (
            profile_hash(&self.before),
            profile_hash(&self.alpha0),
            profile_hash(&self.mask),
        )
    }
}
fn accepted(out: Outcome) -> Box<Patch> {
    match out {
        Outcome::Accepted(p) => p,
        other => panic!("expected accepted:{other:?}"),
    }
}
fn rejected(out: Outcome, error: Error) {
    match out {
        Outcome::Rejected(f) => assert_eq!(f.error, error),
        other => panic!("expected {error:?}:{other:?}"),
    }
}
fn decode_resource(frame: &[u8]) -> (u8, u64, [u32; 3], [u64; 3]) {
    assert_eq!(&frame[..4], b"HSKO");
    assert_eq!(frame[4], 3);
    let mut at = 48;
    for _ in 0..7 {
        let len = u16::from_be_bytes(frame[at..at + 2].try_into().unwrap()) as usize;
        at += 2 + len
    }
    let disposition = frame[at];
    at += 3;
    let epoch = u64::from_be_bytes(frame[at..at + 8].try_into().unwrap());
    at += 8;
    let tiles = std::array::from_fn(|_| {
        let n = u32::from_be_bytes(frame[at..at + 4].try_into().unwrap());
        at += 4;
        n
    });
    at += 4;
    let bytes = std::array::from_fn(|_| {
        let n = u64::from_be_bytes(frame[at..at + 8].try_into().unwrap());
        at += 8;
        n
    });
    (disposition, epoch, tiles, bytes)
}

struct Changing {
    inner: SerializedOwner<Sink>,
    token: CancellationToken,
    calls: usize,
    cancel_at: usize,
    stale_at_final: bool,
}
impl ExecutionPort for Changing {
    fn provider_peak_bytes(&mut self) -> u64 {
        self.inner.provider_peak_bytes()
    }
    fn current_epoch(&mut self) -> Result<u64, Error> {
        self.calls += 1;
        if self.calls == self.cancel_at {
            self.token.cancel()
        }
        self.inner.current_epoch()
    }
    fn current_membership(&mut self, document: &DomainId, layer: &DomainId) -> Result<(), Error> {
        self.inner.current_membership(document, layer)
    }
    fn current_revision(
        &mut self,
        d: &DomainId,
        l: &DomainId,
        a: &TileAddress,
    ) -> Result<u64, Error> {
        self.inner.current_revision(d, l, a)
    }
    fn provider_admission(&mut self, limit: u64) -> Result<MemoryAdmission, Error> {
        self.inner.provider_admission(limit)
    }
    fn lease_layout_bytes(&mut self) -> Result<u64, Error> {
        self.inner.lease_layout_bytes()
    }
    fn reserve(&mut self, c: Counts, l: u64) -> Result<Lease, Error> {
        self.inner.reserve(c, l)
    }
    fn finalize(&mut self, r: &Request<'_>, p: Prepared, t: &CancellationToken) -> Outcome {
        if self.stale_at_final {
            self.inner.set_epoch(r.cancel_epoch + 1)
        }
        self.inner.finalize(r, p, t)
    }
    fn reject(&mut self, r: &Request<'_>, e: Error, c: Counts, t: &CancellationToken) -> Outcome {
        self.inner.reject(r, e, c, t)
    }
}
#[test]
fn masked_patch_changes_only_covered_samples() {
    bounded_case("masked_patch_changes_only_covered_samples", || {
        let f = Fixture::new();
        let fingerprint = f.fingerprint();
        let items = [f.item()];
        let r = f.request(&items);
        let mut owner = f.owner(Sink::default());
        let ledger = owner.admission.clone();
        let patch = accepted(masked_replace(
            &r,
            &mut owner,
            &CancellationToken::default(),
        ));
        assert_eq!(patch.updates.len(), 1);
        let u = &patch.updates[0];
        assert_eq!(
            u.damage,
            Rect {
                x: -1,
                y: -1,
                width: 1,
                height: 1
            }
        );
        assert_eq!((u.previous_revision, u.revision), (7, 8));
        let m = 128. / 255.;
        let a = (1. - m) * 0.25 + m * 0.75;
        for (i, (c0, c1)) in [0.2f32, 0.4, 0.6]
            .into_iter()
            .zip([0.8f32, 0.6, 0.4])
            .enumerate()
        {
            let value = (((1. - m) * 0.25 * f64::from(c0) + m * 0.75 * f64::from(c1)) / a) as f32;
            assert_eq!(
                &u.after.colour[12 + i * 4..16 + i * 4],
                &value.to_le_bytes()
            )
        }
        assert_eq!(&u.after.alpha[4..8], &(a as f32).to_le_bytes());
        for i in 0..f.before.len() {
            if !(12..24).contains(&i) {
                assert_eq!(u.after.colour[i], f.before[i])
            }
        }
        for i in 0..f.alpha0.len() {
            if !(4..8).contains(&i) {
                assert_eq!(u.after.alpha[i], f.alpha0[i])
            }
        }
        assert_eq!(&*u.preimage.colour, &f.before);
        assert_eq!(&*u.preimage.alpha, &f.alpha0);
        assert_eq!(f.fingerprint(), fingerprint);
        assert!(ledger.accounting().live_bytes > 0);
        assert!(ledger.accounting().scratch_released > 0);
        let (d, e, t, b) = decode_resource(&owner.sink.frames[0]);
        assert_eq!((d, e, t), (1, 4, [1, 1, 1]));
        assert!(b[0] > 0);
        assert!(b[2] > 0);
        assert!(!owner.sink.frames[0].windows(6).any(|s| s == b"secret"));
        let retained = patch.updates[0].after.colour.clone();
        drop(patch);
        assert!(ledger.accounting().live_bytes > 0);
        assert_eq!(&retained[..12], &f.before[..12]);
        drop(retained);
        assert_eq!(ledger.accounting().live_bytes, 0);
        // U16 fractional coverage, clipped rectangle and unchanged padding.
        let mask = [0u16, 32768, 0, 0]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let mut r = f.request(&items);
        r.mask = Mask {
            bounds: r.rectangle,
            sample_format: "coverage_u16le",
            bytes: &mask,
            stride_bytes: 4,
            sha256: profile_hash(&mask),
        };
        r.rectangle = Rect {
            x: -1,
            y: -1,
            width: 1,
            height: 1,
        };
        let mut owner = f.owner(Sink::default());
        let p = accepted(masked_replace(
            &r,
            &mut owner,
            &CancellationToken::default(),
        ));
        let m = 32768. / 65535.;
        let a = (1. - m) * 0.25 + m * 0.75;
        assert_eq!(&p.updates[0].after.alpha[4..8], &(a as f32).to_le_bytes());
        let zero = [0u8; 6];
        let mut r = f.request(&items);
        r.mask.bytes = &zero;
        r.mask.sha256 = profile_hash(&zero);
        let mut owner = f.owner(Sink::default());
        let p = accepted(masked_replace(
            &r,
            &mut owner,
            &CancellationToken::default(),
        ));
        assert!(p.updates.is_empty());
        assert!(p.lease.is_none());
        assert_eq!(
            owner.admission.accounting().live_bytes,
            p.metadata_reserved_bytes()
        );
        drop(p);
        assert_eq!(owner.admission.accounting().live_bytes, 0);
        let mut r = f.request(&items);
        r.rectangle.width = 0;
        let mut owner = f.owner(Sink::default());
        assert!(
            accepted(masked_replace(
                &r,
                &mut owner,
                &CancellationToken::default()
            ))
            .updates
            .is_empty()
        );
    })
}
#[test]
fn tile_preimage_survives_cancel() {
    bounded_case("tile_preimage_survives_cancel", || {
        let f = Fixture::new();
        let fingerprint = f.fingerprint();
        let items = [f.item()];
        let r = f.request(&items);
        let token = CancellationToken::default();
        token.cancel();
        let mut owner = f.owner(Sink::default());
        rejected(masked_replace(&r, &mut owner, &token), Error::Canceled);
        assert_eq!(owner.admission.accounting().live_bytes, 0);
        assert_eq!(f.fingerprint(), fingerprint);
        let token = CancellationToken::default();
        let mut changing = Changing {
            inner: f.owner(Sink::default()),
            token: token.clone(),
            calls: 0,
            cancel_at: 5,
            stale_at_final: false,
        };
        rejected(masked_replace(&r, &mut changing, &token), Error::Canceled);
        assert_eq!(changing.inner.admission.accounting().live_bytes, 0);
        assert_eq!(f.fingerprint(), fingerprint);
        // Actual callback cancellation retains a pending token instead of falsely accepting.
        let token = CancellationToken::default();
        let mut owner = f.owner(Sink {
            cancel: Some(token.clone()),
            ..Sink::default()
        });
        let out = masked_replace(&r, &mut owner, &token);
        let pending = match out {
            Outcome::ReconciliationRequired { token, .. } => token,
            other => panic!("expected pending:{other:?}"),
        };
        assert_eq!(owner.pending_count(), 1);
        assert!(owner.admission.accounting().live_bytes > 0);
        owner.reconcile_rejected(&pending).unwrap();
        assert_eq!(owner.admission.accounting().live_bytes, 0);
        assert_eq!(f.fingerprint(), fingerprint);
        for failure in [
            DeliveryError::Rejected,
            DeliveryError::Saturated,
            DeliveryError::Unavailable,
            DeliveryError::Indeterminate,
        ] {
            let mut owner = f.owner(Sink {
                failure: Some(failure),
                ..Sink::default()
            });
            let out = masked_replace(&r, &mut owner, &CancellationToken::default());
            if failure == DeliveryError::Indeterminate {
                let token = match out {
                    Outcome::ReconciliationRequired { token, .. } => token,
                    other => panic!("pending:{other:?}"),
                };
                assert!(owner.admission.accounting().live_bytes > 0);
                owner.reconcile_rejected(&token).unwrap()
            } else {
                rejected(out, Error::DeliveryFailed)
            }
            assert_eq!(owner.admission.accounting().live_bytes, 0)
        }
        let token = CancellationToken::default();
        let mut owner = f.owner(Sink::default());
        let p = accepted(masked_replace(&r, &mut owner, &token));
        token.cancel();
        assert_eq!(&*p.updates[0].preimage.colour, &f.before);
        assert!(owner.admission.accounting().live_bytes > 0);
        drop(p);
        assert_eq!(owner.admission.accounting().live_bytes, 0);
    })
}
#[test]
fn stale_target_patch_refused() {
    bounded_case("stale_target_patch_refused", || {
        let f = Fixture::new();
        let fingerprint = f.fingerprint();
        let items = [f.item()];
        let r = f.request(&items);
        let mut owner = f.owner(Sink::default());
        owner.set_revision(address(&items[0]).unwrap(), 8).unwrap();
        rejected(
            masked_replace(&r, &mut owner, &CancellationToken::default()),
            Error::RevisionConflict,
        );
        assert_eq!(owner.admission.accounting().live_bytes, 0);
        let mut owner = f.owner(Sink::default());
        owner.set_epoch(5);
        rejected(
            masked_replace(&r, &mut owner, &CancellationToken::default()),
            Error::StaleEpoch,
        );
        let token = CancellationToken::default();
        let mut changing = Changing {
            inner: f.owner(Sink::default()),
            token: token.clone(),
            calls: 0,
            cancel_at: usize::MAX,
            stale_at_final: true,
        };
        rejected(masked_replace(&r, &mut changing, &token), Error::StaleEpoch);
        assert_eq!(changing.inner.admission.accounting().live_bytes, 0);
        let mut missing = SerializedOwner::new(
            f.document.clone(),
            f.layer.clone(),
            f.actor.clone(),
            4,
            vec![],
            MemoryAdmission::new(1024 * 1024).unwrap(),
            Sink::default(),
        )
        .unwrap();
        rejected(
            masked_replace(&r, &mut missing, &CancellationToken::default()),
            Error::RevisionUnavailable,
        );
        let other = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000099").unwrap();
        let mut r = f.request(&items);
        r.document_id = &other;
        let mut owner = f.owner(Sink::default());
        rejected(
            masked_replace(&r, &mut owner, &CancellationToken::default()),
            Error::MembershipUnavailable,
        );
        let empty: [TileItem<'_>; 0] = [];
        let other_layer = DomainId::parse("SLYR-019abcde-0000-7000-8000-000000000099").unwrap();
        for (document, layer) in [(&other, &f.layer), (&f.document, &other_layer)] {
            let mut empty_request = f.request(&empty);
            empty_request.rectangle.width = 0;
            empty_request.document_id = document;
            empty_request.layer_id = layer;
            let mut owner = f.owner(Sink::default());
            rejected(
                masked_replace(&empty_request, &mut owner, &CancellationToken::default()),
                Error::MembershipUnavailable,
            );
            assert_eq!(owner.admission.accounting().live_bytes, 0);
        }
        let zero_mask = [0u8; 6];
        let mut no_change = f.request(&items);
        no_change.document_id = &other;
        no_change.mask.bytes = &zero_mask;
        no_change.mask.sha256 = profile_hash(&zero_mask);
        let mut owner = f.owner(Sink::default());
        rejected(
            masked_replace(&no_change, &mut owner, &CancellationToken::default()),
            Error::MembershipUnavailable,
        );
        assert_eq!(f.fingerprint(), fingerprint);
        let mut r = f.request(&items);
        r.rectangle.width = 3;
        r.mask.bounds.width = 3;
        let mut owner = f.owner(Sink::default());
        assert!(matches!(
            masked_replace(&r, &mut owner, &CancellationToken::default()),
            Outcome::Rejected(_)
        ));
        let omitted: [TileItem<'_>; 0] = [];
        let r = f.request(&omitted);
        let mut owner = f.owner(Sink::default());
        rejected(
            masked_replace(&r, &mut owner, &CancellationToken::default()),
            Error::CoverageGap,
        );
        let duplicate = [f.item(), f.item()];
        let r = f.request(&duplicate);
        let mut owner = f.owner(Sink::default());
        rejected(
            masked_replace(&r, &mut owner, &CancellationToken::default()),
            Error::DuplicateTarget,
        );
    })
}
#[test]
fn malformed_stride_and_lease_exhaustion() {
    bounded_case("malformed_stride_and_lease_exhaustion", || {
        let f = Fixture::new();
        let fingerprint = f.fingerprint();
        let mut item = f.item();
        item.before.colour_stride_bytes = 27;
        let items = [item];
        let r = f.request(&items);
        let mut owner = f.owner(Sink::default());
        rejected(
            masked_replace(&r, &mut owner, &CancellationToken::default()),
            Error::MalformedStride,
        );
        assert_eq!(owner.admission.accounting().live_bytes, 0);
        let items = [f.item()];
        let mut r = f.request(&items);
        r.byte_limit = 1;
        let mut owner = f.owner(Sink::default());
        rejected(
            masked_replace(&r, &mut owner, &CancellationToken::default()),
            Error::BudgetExceeded,
        );
        let r = f.request(&items);
        let mut owner = SerializedOwner::new(
            f.document.clone(),
            f.layer.clone(),
            f.actor.clone(),
            4,
            vec![(address(&items[0]).unwrap(), 7)],
            MemoryAdmission::new(1).unwrap(),
            Sink::default(),
        )
        .unwrap();
        rejected(
            masked_replace(&r, &mut owner, &CancellationToken::default()),
            Error::LeaseUnavailable,
        );
        assert_eq!(owner.admission.accounting().live_bytes, 0);
        let mut item = f.item();
        item.before.colour_sha256 = [0; 32];
        let items = [item];
        let r = f.request(&items);
        let mut owner = f.owner(Sink::default());
        rejected(
            masked_replace(&r, &mut owner, &CancellationToken::default()),
            Error::HashMismatch,
        );
        assert_eq!(owner.admission.accounting().live_bytes, 0);
        let mut bad = Fixture::new();
        bad.replacement[..4].copy_from_slice(&f32::NAN.to_le_bytes());
        let items = [bad.item()];
        let r = bad.request(&items);
        let mut owner = bad.owner(Sink::default());
        rejected(
            masked_replace(&r, &mut owner, &CancellationToken::default()),
            Error::Nonfinite,
        );
        assert_eq!(owner.admission.accounting().live_bytes, 0);
        let mut item = f.item();
        item.replacement.profile = ProfileInput {
            profile_id: &f.profile,
            bytes: ENCODED,
            expected_sha256: profile_hash(ENCODED),
        };
        let items = [item];
        let r = f.request(&items);
        let mut owner = f.owner(Sink::default());
        rejected(
            masked_replace(&r, &mut owner, &CancellationToken::default()),
            Error::Prism(hsk_studio_prism::Error::UnsupportedProfile),
        );
        let items = [f.item()];
        let mut r = f.request(&items);
        r.rectangle.x = i64::MAX;
        let mut owner = f.owner(Sink::default());
        rejected(
            masked_replace(&r, &mut owner, &CancellationToken::default()),
            Error::Overflow,
        );
        assert_eq!(f.fingerprint(), fingerprint);
        let mut invalid_alpha = f.alpha1.clone();
        invalid_alpha[4..8].copy_from_slice(&1.1f32.to_le_bytes());
        let mut item = f.item();
        item.replacement.alpha_bytes = &invalid_alpha;
        item.replacement.alpha_sha256 = profile_hash(&invalid_alpha);
        let items = [item];
        let r = f.request(&items);
        let mut owner = f.owner(Sink::default());
        rejected(
            masked_replace(&r, &mut owner, &CancellationToken::default()),
            Error::AlphaRange,
        );
        let items = [f.item()];
        let mut r = f.request(&items);
        r.mask.sha256 = [0; 32];
        let mut owner = f.owner(Sink::default());
        rejected(
            masked_replace(&r, &mut owner, &CancellationToken::default()),
            Error::HashMismatch,
        );
        let mut item = f.item();
        item.before.colour_stride_bytes = u64::MAX - 3;
        let items = [item];
        let r = f.request(&items);
        let mut owner = f.owner(Sink::default());
        rejected(
            masked_replace(&r, &mut owner, &CancellationToken::default()),
            Error::Overflow,
        );
        let mut item = f.item();
        item.expected_property_revision = u64::MAX;
        let items = [item];
        let r = f.request(&items);
        let mut owner = f.owner(Sink::default());
        owner
            .set_revision(address(&items[0]).unwrap(), u64::MAX)
            .unwrap();
        rejected(
            masked_replace(&r, &mut owner, &CancellationToken::default()),
            Error::Overflow,
        );
        assert_eq!(owner.admission.accounting().live_bytes, 0);
        // Actual admitted native-Rust cross-profile conversion, both matrix directions.
        for (source, destination) in [(P3, LINEAR), (LINEAR, P3)] {
            let mut item = f.item();
            item.before.profile = ProfileInput {
                profile_id: &f.profile,
                bytes: destination,
                expected_sha256: profile_hash(destination),
            };
            item.replacement.profile = ProfileInput {
                profile_id: &f.profile,
                bytes: source,
                expected_sha256: profile_hash(source),
            };
            let items = [item];
            let r = f.request(&items);
            let mut owner = f.owner(Sink::default());
            let patch = accepted(masked_replace(
                &r,
                &mut owner,
                &CancellationToken::default(),
            ));
            let converted = linear_oracle(source, destination, [0.8, 0.6, 0.4]);
            let m = 128.0 / 255.0;
            let alpha = (1.0 - m) * 0.25 + m * 0.75;
            for (i, expected) in converted.iter().enumerate() {
                let expected = (((1.0 - m) * 0.25 * f64::from([0.2f32, 0.4, 0.6][i])
                    + m * 0.75 * expected)
                    / alpha) as f32;
                let actual = f32::from_le_bytes(
                    patch.updates[0].after.colour[12 + i * 4..16 + i * 4]
                        .try_into()
                        .unwrap(),
                );
                assert!(
                    (actual - expected).abs() < 0.0001,
                    "independent ICC matrix masked oracle {actual}/{expected}"
                );
            }
            assert_eq!(patch.updates[0].conversions.len(), 1);
            assert_eq!(patch.updates[0].conversions[0].engine_version, "0.9.1");
            assert_eq!(
                owner.sink.frames.len(),
                2,
                "actual Prism frame then Observe resource frame"
            );
            assert_eq!(patch.updates[0].conversion_frames[0], owner.sink.frames[0]);
            drop(patch);
            assert_eq!(owner.admission.accounting().live_bytes, 0);
        }
        let mut invalid = f.replacement.clone();
        invalid[12..16].copy_from_slice(&1.01f32.to_le_bytes());
        let mut item = f.item();
        item.replacement.colour_bytes = &invalid;
        item.replacement.colour_sha256 = profile_hash(&invalid);
        item.replacement.profile = ProfileInput {
            profile_id: &f.profile,
            bytes: P3,
            expected_sha256: profile_hash(P3),
        };
        let items = [item];
        let r = f.request(&items);
        let mut owner = f.owner(Sink::default());
        rejected(
            masked_replace(&r, &mut owner, &CancellationToken::default()),
            Error::Prism(hsk_studio_prism::Error::ChannelRange),
        );
        assert_eq!(owner.admission.accounting().live_bytes, 0);
    })
}
