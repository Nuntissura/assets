use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe as obs;
use hsk_studio_type::*;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
pub fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => std::panic::resume_unwind(e),
        Err(e) => panic!("{name}: declared60s case failed: {e}"),
    }
}
pub fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
pub fn unhex(s: &str) -> Vec<u8> {
    assert_eq!(s.len() % 2, 0);
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
/// Pinned MT-35536 evidence root: the nearest ancestor of this crate's manifest directory that
/// holds it, so the worktree and any export below the same root resolve it without env vars.
pub fn research() -> PathBuf {
    const RELATIVE: &str =
        "Handshake_Artifacts/WP-KERNEL-STUDIO/MT-35536/studio-assets-kernel-builder/research";
    if let Some(root) = std::env::var_os("HSK_TYPE_RESEARCH_ROOT") {
        return PathBuf::from(root);
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .map(|ancestor| ancestor.join(RELATIVE))
        .find(|root| root.join("type-exact-oracle-recipe.json").is_file())
        .unwrap_or_else(|| {
            panic!(
                "required evidence root {RELATIVE} not found above {}",
                manifest.display()
            )
        })
}
pub fn json(path: &Path) -> Value {
    serde_json::from_slice(
        &std::fs::read(path)
            .unwrap_or_else(|e| panic!("required evidence {}: {e}", path.display())),
    )
    .unwrap()
}
pub fn id(prefix: &str) -> DomainId {
    DomainId::parse(&format!("{prefix}-019abcde-0000-7000-8000-000000000001")).unwrap()
}
#[derive(Clone, Copy, Default)]
pub struct Slot {
    handle: u64,
    bytes: u64,
    refs: u64,
}
pub struct State {
    slots: [Slot; 1024],
    generations: [u64; 32],
    next: u64,
    current: u64,
    peak: u64,
    history: u64,
    retained: u64,
    reserves: u64,
}
pub struct Ledger {
    state: Mutex<State>,
    denial: AtomicU64,
}
impl Ledger {
    fn new() -> Self {
        Self {
            state: Mutex::new(State {
                slots: [Slot::default(); 1024],
                generations: [0; 32],
                next: 1,
                current: 0,
                peak: 0,
                history: 1 << 40,
                retained: 0,
                reserves: 0,
            }),
            denial: AtomicU64::new(u64::MAX),
        }
    }
    fn begin(&self) {
        let mut s = self.state.lock().unwrap();
        s.peak = s.current;
        s.reserves = 0;
    }
    fn borrow(&self, handle: u64, bytes: u64) -> Result<Reservation<'_>, Error> {
        let mut s = self.state.lock().unwrap();
        let slot = s
            .slots
            .iter_mut()
            .find(|x| x.handle == handle && x.bytes == bytes)
            .ok_or(Error::LeaseUnavailable)?;
        slot.refs = slot.refs.checked_add(1).ok_or(Error::Overflow)?;
        Reservation::new(self, handle, bytes)
    }
    fn base(&self, bytes: u64) -> (u64, Reservation<'_>) {
        let r = self.reserve(bytes, 64 << 20).unwrap();
        let s = self.state.lock().unwrap();
        let h = s.next - 1;
        (h, r)
    }
}
impl RetirementPort for Ledger {
    fn retire(&self, h: u64, n: u64) {
        let mut s = self.state.lock().unwrap();
        let i = s
            .slots
            .iter()
            .position(|x| x.handle == h)
            .expect("unique live lease");
        assert_eq!(s.slots[i].bytes, n);
        assert!(s.slots[i].refs > 0);
        s.slots[i].refs -= 1;
        if s.slots[i].refs == 0 {
            s.current = s.current.checked_sub(n).unwrap();
            s.slots[i] = Slot::default();
        }
    }
}
impl GenerationRetirement for Ledger {
    fn retire_generation(&self, h: u64) {
        let mut s = self.state.lock().unwrap();
        let i = s
            .generations
            .iter()
            .position(|x| *x == h)
            .expect("unique retained generation");
        s.generations[i] = 0;
        s.retained -= 1;
    }
}
impl AdmissionPort for Ledger {
    fn reserve(&self, n: u64, limit: u64) -> Result<Reservation<'_>, Error> {
        if n == 0 {
            return Err(Error::LeaseUnavailable);
        }
        let mut s = self.state.lock().unwrap();
        s.reserves += 1;
        if s.reserves >= self.denial.load(Ordering::SeqCst) {
            return Err(Error::Budget);
        }
        let next = s.current.checked_add(n).ok_or(Error::Overflow)?;
        if next > limit {
            return Err(Error::Budget);
        }
        let i = s
            .slots
            .iter()
            .position(|x| x.handle == 0)
            .ok_or(Error::Budget)?;
        let h = s.next;
        s.next = s.next.checked_add(1).ok_or(Error::Overflow)?;
        s.slots[i] = Slot {
            handle: h,
            bytes: n,
            refs: 1,
        };
        s.current = next;
        s.peak = s.peak.max(next);
        s.history = s.history.max(next);
        Reservation::new(self, h, n)
    }
    fn snapshot(&self) -> Result<AllocationSnapshot, Error> {
        let s = self.state.lock().unwrap();
        Ok(AllocationSnapshot {
            current_requested_bytes: s.current,
            operation_peak_requested_bytes: s.peak,
            historical_peak_requested_bytes: s.history,
            retained_generations: s.retained,
        })
    }
    fn retain_generation(&self, limit: u64) -> Result<Generation<'_>, Error> {
        let mut s = self.state.lock().unwrap();
        if s.retained >= limit {
            return Err(Error::Budget);
        }
        let i = s
            .generations
            .iter()
            .position(|h| *h == 0)
            .ok_or(Error::Budget)?;
        let h = s.next;
        s.next = s.next.checked_add(1).ok_or(Error::Overflow)?;
        s.generations[i] = h;
        s.retained += 1;
        Generation::new(self, h)
    }
}
/// Vendored OFL-1.1 fonts 0..2 and their adjacent licenses: byte-identical to the recipe's
/// hash-pinned originals, whose absolute paths serve only as provenance file names.
pub fn font_root() -> PathBuf {
    std::env::var_os("HSK_TYPE_FONT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fonts"))
}
pub struct Fonts {
    data: [Vec<u8>; 4],
    hashes: [[u8; 32]; 4],
}
impl Fonts {
    fn load() -> Self {
        let recipe = json(&research().join("type-exact-oracle-recipe.json"));
        let values = recipe["fonts"].as_array().unwrap();
        let prep = json(&research().join("type-foundation-preparation.json"));
        for record in prep["source_identities"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["path"].as_str().is_some_and(|p| p.ends_with("OFL.txt")))
        {
            let original = PathBuf::from(record["path"].as_str().unwrap());
            let path = font_root().join(original.file_name().unwrap());
            assert_eq!(
                hash(&std::fs::read(&path).unwrap()).as_slice(),
                unhex(record["sha256"].as_str().unwrap()),
                "original adjacent font license"
            );
        }
        let variable = json(&research().join("type-variable-fixture-and-oracle.json"));
        assert_eq!(
            hash(&std::fs::read(research().join("Inter-variable-OFL.txt")).unwrap()).as_slice(),
            unhex(variable["license_sha256"].as_str().unwrap())
        );

        let data = std::array::from_fn(|i| {
            let original = PathBuf::from(values[i]["path"].as_str().unwrap());
            let path = if i == 3 {
                research().join("Inter-variable-opsz-wght.ttf")
            } else {
                font_root().join(original.file_name().unwrap())
            };
            std::fs::read(&path)
                .unwrap_or_else(|e| panic!("required original font {}: {e}", path.display()))
        });
        let hashes = std::array::from_fn(|i| {
            let expected: [u8; 32] = unhex(values[i]["hash"].as_str().unwrap())
                .try_into()
                .unwrap();
            assert_eq!(hash(&data[i]), expected, "original licensed font {i}");
            expected
        });
        Self { data, hashes }
    }
}
const NAMES: [&str; 4] = [
    "Inter-Regular",
    "NotoSansArabic-Regular",
    "NotoSansSC-Regular",
    "Inter-Variable",
];
pub struct Resolver<'a> {
    fonts: &'a Fonts,
    ledger: &'a Ledger,
    handles: [u64; 4],
    revision: AtomicU64,
    mode: AtomicU64,
}
impl FontResolver for Resolver<'_> {
    fn revision(&self) -> Result<u64, Error> {
        Ok(self.revision.load(Ordering::SeqCst))
    }
    fn resolve(&self, r: &FontResource<'_>) -> Result<FontRead<'_>, Error> {
        let i = NAMES
            .iter()
            .position(|n| *n == r.identity)
            .ok_or(Error::MissingFont)?;
        match self.mode.load(Ordering::SeqCst) {
            1 => return Err(Error::UnavailableFont),
            2 => return Err(Error::RevokedFont),
            _ => {}
        }
        let bytes = &self.fonts.data[i];
        Ok(FontRead {
            bytes,
            resource_revision: 1,
            grant_revision: 1,
            lease: self
                .ledger
                .borrow(self.handles[i], bytes.capacity() as u64)?,
        })
    }
    fn verify_lifetime(&self, r: &FontResource<'_>, read: &FontRead<'_>) -> Result<(), Error> {
        let i = NAMES
            .iter()
            .position(|n| *n == r.identity)
            .ok_or(Error::MissingFont)?;
        if read.bytes.as_ptr() != self.fonts.data[i].as_ptr()
            || read.lease.requested_bytes() < read.bytes.len() as u64
        {
            return Err(Error::LeaseUnavailable);
        }
        Ok(())
    }
}
pub struct Input {
    bytes: u64,
}
impl InputLifetime for Input {
    fn verify(&self, _: &Request<'_>) -> Result<InputCharge, Error> {
        Ok(InputCharge {
            lifetime_id: 1,
            requested_bytes: self.bytes,
        })
    }
}
pub struct Scene<'a> {
    gate: Mutex<()>,
    text: &'a str,
    styles: &'a [Style<'a>],
    epoch: AtomicU64,
    revision: AtomicU64,
    member: AtomicU64,
}
impl EpochPort for Scene<'_> {
    fn current_epoch(&self) -> Result<u64, Error> {
        Ok(self.epoch.load(Ordering::SeqCst))
    }
}
impl SourceContext for Scene<'_> {
    fn member(&self, _: &Request<'_>) -> Result<(), Error> {
        if self.member.load(Ordering::SeqCst) == 1 {
            Ok(())
        } else {
            Err(Error::UnavailableRead)
        }
    }
    fn read(&self, a: ReadAddress<'_>) -> Result<CurrentRead<'_>, Error> {
        let bytes: &[u8] = if a.property == "text" {
            self.text.as_bytes()
        } else if a.property == "result" {
            b"original-preimage"
        } else {
            b"resolved-style"
        };
        Ok(CurrentRead {
            revision: self.revision.load(Ordering::SeqCst),
            fingerprint: hash(bytes),
            bytes,
        })
    }
    fn resolved_style(&self, a: ReadAddress<'_>) -> Result<Style<'_>, Error> {
        self.styles
            .iter()
            .find(|s| {
                s.read_index
                    == a.property
                        .strip_prefix("style")
                        .and_then(|s| s.parse::<u32>().ok())
                        .unwrap_or(0)
            })
            .copied()
            .ok_or(Error::AbsentRead)
    }
}
pub fn limits() -> Limits {
    Limits {
        input_bytes: 4096,
        input_scalars: 4096,
        styles: 32,
        features: 64,
        axes: 16,
        font_resources: 32,
        font_bytes: 16 << 20,
        parser_tables: 128,
        lookups: 65536,
        runs: 128,
        glyphs: 4096,
        fallback_attempts: 64,
        recursion: 64,
        work_units: 50_000_000,
        requested_owned_bytes: 64 << 20,
        retained_generations: 8,
    }
}
pub struct Capture {
    frames: [[u8; 2048]; 4],
    lengths: [usize; 4],
    count: usize,
    mode: Option<obs::DeliveryError>,
}
impl Capture {
    fn new(mode: Option<obs::DeliveryError>) -> Self {
        Self {
            frames: [[0; 2048]; 4],
            lengths: [0; 4],
            count: 0,
            mode,
        }
    }
}
impl obs::SinkPort for Capture {
    fn try_send(&mut self, _: obs::DeliveryClass, b: &[u8]) -> Result<(), obs::DeliveryError> {
        if let Some(e) = self.mode {
            return Err(e);
        }
        if self.count == 4 || b.len() > 2048 {
            return Err(obs::DeliveryError::Saturated);
        }
        self.frames[self.count][..b.len()].copy_from_slice(b);
        self.lengths[self.count] = b.len();
        self.count += 1;
        Ok(())
    }
}
pub struct Target<'a> {
    bytes: [u8; 65536],
    len: usize,
    hash: Option<[u8; 32]>,
    commits: u64,
    cancel_before_delivery: Option<&'a CancellationToken>,
    attempt: Option<PendingKey>,
}
impl TargetPublication for Target<'_> {
    fn with_publication(
        &mut self,
        p: &Prepared<'_>,
        deliver: impl FnOnce() -> Result<(), obs::DeliveryError>,
    ) -> Result<(), obs::DeliveryError> {
        if p.serialized().len() > self.bytes.len() {
            return Err(obs::DeliveryError::Rejected);
        }
        if let Some(token) = self.cancel_before_delivery {
            token.cancel();
        }
        self.attempt = Some(PendingKey {
            result_sha256: hash(p.serialized()),
            source_sha256: p.request().text_sha256,
            target_revision: p.request().result_target.address.expected_revision,
            cancel_epoch: p.request().cancel_epoch,
        });
        deliver()?;
        self.bytes[..p.serialized().len()].copy_from_slice(p.serialized());
        self.len = p.serialized().len();
        self.hash = Some(hash(p.serialized()));
        self.commits += 1;
        Ok(())
    }
}
pub struct FinalPort<'a, 'b> {
    ports: &'b Ports<'a>,
    scene: &'b Scene<'a>,
    observer: obs::Observe,
    capture: Capture,
    target: Target<'a>,
    reconciliation: Reconciliation,
    diagnostics: Capture,
    diagnostic_mode: Option<obs::DeliveryError>,
    reconciliation_key: Option<PendingKey>,
    _observer_lease: Reservation<'a>,
}
impl<'a> FinalizationPort<'a> for FinalPort<'a, '_> {
    fn with_exclusive<T>(
        &mut self,
        call: impl FnOnce(&Ports<'a>, &mut dyn PublicationPort) -> T,
    ) -> Result<T, Error> {
        let _guard = self.scene.gate.lock().unwrap();
        let mut publisher = ObservedPublication {
            observer: &mut self.observer,
            sink: &mut self.capture,
            target: &mut self.target,
        };
        Ok(call(self.ports, &mut publisher))
    }
    fn with_reconciliation<T>(
        &mut self,
        key: PendingKey,
        call: impl FnOnce(&Ports<'a>, Result<Reconciliation, Error>) -> T,
    ) -> Result<T, Error> {
        let _guard = self.scene.gate.lock().unwrap();
        assert_eq!(
            self.target.attempt,
            Some(key),
            "query is bound to actual prior publication attempt"
        );
        if let Some(previous) = self.reconciliation_key {
            assert_eq!(previous, key, "query exact prior attempt");
        }
        self.reconciliation_key = Some(key);
        Ok(call(self.ports, Ok(self.reconciliation)))
    }
}
pub fn observer(request: &Request<'_>) -> obs::Observe {
    obs::Observe::new(
        request.observation_correlation,
        request.result_target.address.expected_revision,
        request.document_id.clone(),
        request.actor.clone(),
        obs::Budget::new(1, 2048).unwrap(),
    )
}

pub fn final_port<'a, 'b>(
    request: Request<'a>,
    ports: &'b Ports<'a>,
    scene: &'b Scene<'a>,
    ledger: &'a Ledger,
    mode: Option<obs::DeliveryError>,
) -> FinalPort<'a, 'b> {
    let bytes = request.document_id.as_str().len() as u64
        + [
            request.actor.account_id(),
            request.actor.principal_id(),
            request.actor.owner_account_id(),
            request.actor.owner_principal_id(),
            request.actor.access_space_id(),
            request.actor.session_id(),
        ]
        .iter()
        .map(|s| s.len() as u64)
        .sum::<u64>();
    let lease = ledger
        .reserve(bytes, request.limits.requested_owned_bytes)
        .unwrap();
    FinalPort {
        ports,
        scene,
        observer: observer(&request),
        capture: Capture::new(mode),
        target: Target {
            bytes: std::array::from_fn(|i| b"original-preimage".get(i).copied().unwrap_or(0)),
            len: b"original-preimage".len(),
            hash: None,
            commits: 0,
            cancel_before_delivery: None,
            attempt: None,
        },
        reconciliation: Reconciliation::StillIndeterminate,
        diagnostics: Capture::new(None),
        diagnostic_mode: None,
        reconciliation_key: None,
        _observer_lease: lease,
    }
}
pub fn observer_bytes(request: &Request<'_>) -> u64 {
    request.document_id.as_str().len() as u64
        + [
            request.actor.account_id(),
            request.actor.principal_id(),
            request.actor.owner_account_id(),
            request.actor.owner_principal_id(),
            request.actor.access_space_id(),
            request.actor.session_id(),
        ]
        .iter()
        .map(|s| s.len() as u64)
        .sum::<u64>()
}
pub fn diagnostic_transition<'a, 'b>(
    request: Request<'a>,
    port: &mut FinalPort<'a, 'b>,
    call: impl FnOnce(&mut FinalPort<'a, 'b>, &mut DiagnosticPorts<'_, 'a>) -> Finalized<'a>,
) -> Finalized<'a> {
    let admission = port.ports.admission;
    let lease = admission
        .reserve(
            observer_bytes(&request),
            request.limits.requested_owned_bytes,
        )
        .unwrap();
    let mut fresh = observer(&request);
    let mut capture = Capture::new(port.diagnostic_mode);
    let mut diagnostics = DiagnosticPorts {
        observer: &mut fresh,
        sink: &mut capture,
        admission,
    };
    let outcome = call(port, &mut diagnostics);
    assert!(outcome.inspection().counts_fresh);
    assert_eq!(
        outcome.inspection().counts.current_requested_bytes,
        admission.snapshot().unwrap().current_requested_bytes,
        "transition snapshot includes still-live caller diagnostics/publication storage"
    );
    for i in 0..capture.count {
        obs::SinkPort::try_send(
            &mut port.diagnostics,
            obs::DeliveryClass::Terminal,
            &capture.frames[i][..capture.lengths[i]],
        )
        .unwrap();
    }
    drop(fresh);
    drop(lease);
    outcome
}
pub fn finalize_with_diagnostics<'a>(
    proposal: Prepared<'a>,
    port: &mut FinalPort<'a, '_>,
) -> Finalized<'a> {
    let request = *proposal.request();
    diagnostic_transition(request, port, |port, diagnostics| {
        finalize(proposal, port, diagnostics)
    })
}
pub fn reconcile_with_diagnostics<'a>(
    token: PendingToken<'a>,
    port: &mut FinalPort<'a, '_>,
) -> Finalized<'a> {
    let request = *token.proposal().request();
    diagnostic_transition(request, port, |port, diagnostics| {
        reconcile(token, port, diagnostics)
    })
}
pub fn diagnostic_frame(capture: &Capture, index: usize) -> (u8, u8, u64, u64, u64) {
    let frame = &capture.frames[index][..capture.lengths[index]];
    assert_eq!(&frame[..5], b"HSKO\x05");
    let mut at = 48;
    for _ in 0..7 {
        let n = u16::from_be_bytes(frame[at..at + 2].try_into().unwrap()) as usize;
        at += 2 + n;
    }
    let get =
        |offset: usize| u64::from_be_bytes(frame[at + offset..at + offset + 8].try_into().unwrap());
    (
        frame[at],
        frame[at + 1],
        get(4 + 6 * 8),
        get(4 + 7 * 8),
        get(4 + 8 * 8),
    )
}
pub fn with_case(
    fonts: &Fonts,
    case: &Value,
    candidate_override: Option<&[usize]>,
    body: impl for<'a> FnOnce(Request<'a>, &Ports<'a>, &Scene<'a>, &Ledger, &Resolver<'a>),
) {
    let text = case["text"].as_str().unwrap();
    assert_eq!(
        hash(text.as_bytes()).as_slice(),
        unhex(case["text_sha256"].as_str().unwrap())
    );
    let document = id("SDOC");
    let story = id("STXT");
    let layer = id("SLYR");
    let style_id = id("STYS");
    let actor = ActorContext::new(
        "account",
        "principal",
        "owner",
        "owner-principal",
        "space",
        "session",
    )
    .unwrap();
    let cancel = CancellationToken::default();
    let ledger = Ledger::new();
    let mut handles = [0; 4];
    let mut font_leases = Vec::with_capacity(4);
    for i in 0..4 {
        let (h, r) = ledger.base(fonts.data[i].capacity() as u64);
        handles[i] = h;
        font_leases.push(r);
    }
    let resolver = Resolver {
        fonts,
        ledger: &ledger,
        handles,
        revision: AtomicU64::new(1),
        mode: AtomicU64::new(0),
    };
    let segments:Vec<Value>=case["segments"].as_array().cloned().unwrap_or_else(||vec![serde_json::json!({"start":0,"end":text.len(),"font_index":case["font_index"],"script":case["script"],"language":case["language"]})]);
    let mut candidates = Vec::with_capacity(segments.len());
    let mut features = Vec::with_capacity(segments.len());
    let mut axes = Vec::with_capacity(segments.len());
    for segment in &segments {
        let indices = if let Some(indices) = candidate_override {
            indices.to_vec()
        } else {
            vec![segment["font_index"].as_u64().unwrap() as usize]
        };
        candidates.push(
            indices
                .iter()
                .map(|i| FontResource {
                    identity: NAMES[*i],
                    location: NAMES[*i],
                    content_hash: if case["bad_font_hash"].as_bool() == Some(true) {
                        [0; 32]
                    } else {
                        fonts.hashes[*i]
                    },
                    face_index: case["face_index"].as_u64().unwrap_or(0) as u32,
                    resource_revision: 1,
                    grant_revision: case["grant_revision"].as_u64().unwrap_or(1),
                })
                .collect::<Vec<_>>(),
        );
        let range = SourceRange {
            start: segment["start"].as_u64().unwrap(),
            end: segment["end"].as_u64().unwrap(),
        };
        features.push(
            case["features"]
                .as_array()
                .map(|values| {
                    values
                        .iter()
                        .map(|f| Feature {
                            tag: f["tag"].as_str().unwrap().as_bytes().try_into().unwrap(),
                            value: f["value"].as_u64().unwrap() as u32,
                            range,
                            declared: f["declared"].as_bool().unwrap_or_else(|| {
                                // Original Inter oracle liga options are unavailable and explicitly inert.
                                !(f["tag"] == "liga"
                                    && matches!(
                                        case["case"].as_str(),
                                        Some("latin_liga_0" | "latin_liga_1")
                                    ))
                            }),
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        );
        axes.push(
            case["axes"]
                .as_array()
                .map(|values| {
                    values
                        .iter()
                        .map(|a| Axis {
                            tag: a["tag"].as_str().unwrap().as_bytes().try_into().unwrap(),
                            value: if case["axis_nan"].as_bool() == Some(true) {
                                f32::NAN
                            } else {
                                a["value"].as_f64().unwrap() as f32
                            },
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        );
    }
    let mut styles = Vec::with_capacity(segments.len());
    for (i, segment) in segments.iter().enumerate() {
        styles.push(Style {
            read_index: (i + 1) as u32,
            range: SourceRange {
                start: segment["start"].as_u64().unwrap(),
                end: segment["end"].as_u64().unwrap(),
            },
            font_size_pt: 12.0,
            requested_identity: case["requested_identity"]
                .as_str()
                .unwrap_or(candidates[i][0].identity),
            candidates: &candidates[i],
            substitution: if case["explicit_substitution"].as_bool() == Some(true) {
                Some(Substitution {
                    requested_identity: case["requested_identity"].as_str().unwrap(),
                    resolved_identity: candidates[i][0].identity,
                    reason: SubstitutionReason::MissingRequested,
                })
            } else {
                None
            },
            script: segment["script"]
                .as_str()
                .unwrap()
                .as_bytes()
                .try_into()
                .unwrap(),
            language: segment["language"].as_str().unwrap(),
            character_override: CharacterOverride::Default,
            features: &features[i],
            axes: &axes[i],
        });
    }
    let property_names: Vec<String> = (1..=styles.len()).map(|i| format!("style{i}")).collect();
    let mut reads = Vec::with_capacity(styles.len() + 1);
    reads.push(ReadAddress {
        owner: &story,
        property: "text",
        expected_revision: 1,
        fingerprint: hash(text.as_bytes()),
    });
    for property in &property_names {
        reads.push(ReadAddress {
            owner: &style_id,
            property,
            expected_revision: 1,
            fingerprint: hash(b"resolved-style"),
        });
    }
    let rtl = case["direction"].as_str() == Some("rtl");
    let direction = if rtl {
        Direction::RightToLeft
    } else {
        Direction::LeftToRight
    };
    let paragraphs: Vec<ParagraphInput> = if text.is_empty() {
        Vec::new()
    } else {
        vec![ParagraphInput {
            range: SourceRange {
                start: 0,
                end: text.len() as u64,
            },
            direction,
            provenance: DirectionProvenance::Explicit,
        }]
    };
    // Preowned caller storage remains charged for every borrower. Exact Vec capacities and
    // string byte lengths are charged once; font borrower leases share the base reservation.
    let mut input_bytes = [
        actor.account_id(),
        actor.principal_id(),
        actor.owner_account_id(),
        actor.owner_principal_id(),
        actor.access_space_id(),
        actor.session_id(),
    ]
    .iter()
    .map(|s| s.len() as u64)
    .sum::<u64>();
    input_bytes += [&document, &story, &layer, &style_id]
        .iter()
        .map(|id| id.as_str().len() as u64)
        .sum::<u64>();
    input_bytes += match &case["text"] {
        Value::String(s) => s.capacity() as u64,
        _ => panic!("text String"),
    };
    // Exact pinned ArcInner layout for the caller CancellationToken Arc<AtomicBool>.
    let arc = std::alloc::Layout::new::<[std::sync::atomic::AtomicUsize; 2]>()
        .extend(std::alloc::Layout::new::<std::sync::atomic::AtomicBool>())
        .unwrap()
        .0
        .pad_to_align();
    input_bytes += arc.size() as u64;
    input_bytes += (paragraphs.capacity() * std::mem::size_of::<ParagraphInput>()) as u64;
    input_bytes += (styles.capacity() * std::mem::size_of::<Style>()
        + reads.capacity() * std::mem::size_of::<ReadAddress>()
        + font_leases.capacity() * std::mem::size_of::<Reservation>()
        + candidates.capacity() * std::mem::size_of::<Vec<FontResource>>()
        + features.capacity() * std::mem::size_of::<Vec<Feature>>()
        + axes.capacity() * std::mem::size_of::<Vec<Axis>>()
        + property_names.capacity() * std::mem::size_of::<String>()) as u64;
    for i in 0..segments.len() {
        input_bytes += (candidates[i].capacity() * std::mem::size_of::<FontResource>()
            + features[i].capacity() * std::mem::size_of::<Feature>()
            + axes[i].capacity() * std::mem::size_of::<Axis>()
            + property_names[i].capacity()) as u64;
        if let Value::String(s) = &segments[i]["language"] {
            input_bytes += s.capacity() as u64;
        }
    }
    let (_, input_lease) = ledger.base(input_bytes.max(1));
    let input = Input {
        bytes: input_bytes.max(1),
    };
    let scene = Scene {
        gate: Mutex::new(()),
        text,
        styles: &styles,
        epoch: AtomicU64::new(1),
        revision: AtomicU64::new(1),
        member: AtomicU64::new(1),
    };
    let ports = Ports {
        context: &scene,
        resolver: &resolver,
        input_lifetime: &input,
        admission: &ledger,
    };
    let request = Request {
        document_id: &document,
        document_revision: 77,
        story_id: &story,
        layer_id: &layer,
        text_utf8: text,
        text_sha256: hash(text.as_bytes()),
        text_read_index: 0,
        reads: &reads,
        result_target: ResultTarget {
            address: ReadAddress {
                owner: &story,
                property: "result",
                expected_revision: 1,
                fingerprint: hash(b"original-preimage"),
            },
            preimage: b"original-preimage",
        },
        composition_version: COMPOSITION_VERSION,
        normalization: Normalization::Preserve,
        story_direction: if rtl {
            StoryDirection::RightToLeft
        } else {
            StoryDirection::LeftToRight
        },
        paragraphs: &paragraphs,
        styles: &styles,
        resolver_revision: 1,
        cancel_epoch: 1,
        cancellation: &cancel,
        actor: &actor,
        correlation_id: "foundation-case",
        observation_correlation: 42,
        limits: limits(),
    };
    ledger.begin();
    let base = ledger.snapshot().unwrap().current_requested_bytes;
    body(request, &ports, &scene, &ledger, &resolver);
    let after = ledger.snapshot().unwrap();
    assert_eq!(
        after.current_requested_bytes, base,
        "all operation owners retired"
    );
    assert_eq!(after.retained_generations, 0);
    assert_eq!(
        text.as_bytes(),
        unhex(case["text_utf8_hex"].as_str().unwrap())
    );
    drop(input_lease);
    drop(font_leases);
    assert_eq!(ledger.snapshot().unwrap().current_requested_bytes, 0);
}
pub fn prepared<'a>(request: Request<'a>, ports: &Ports<'a>) -> Prepared<'a> {
    TextEngine.shaped_runs(request, ports).unwrap_or_else(|r| {
        panic!(
            "required positive refused {:?} at {:?}, counts {:?}",
            r.error, r.source, r.counts
        )
    })
}
pub fn verify_coverage(result: &Prepared<'_>) {
    let request = result.request();
    let mut end = 0;
    for c in result.coverage() {
        assert_eq!(c.source.start, end);
        assert!(c.source.end > c.source.start);
        assert!(
            request.text_utf8.is_char_boundary(c.source.start as usize)
                && request.text_utf8.is_char_boundary(c.source.end as usize)
        );
        match c.kind {
            CoverageKind::ShapedCluster => assert!(c.glyphs.is_some()),
            _ => assert!(c.glyphs.is_none()),
        }
        end = c.source.end;
    }
    assert_eq!(end, request.text_utf8.len() as u64);
    for p in result.paragraphs() {
        for r in p.runs() {
            assert_eq!(
                r.direction(),
                if r.bidi_level() % 2 == 0 {
                    Direction::LeftToRight
                } else {
                    Direction::RightToLeft
                }
            );
            for g in r.glyphs() {
                assert_ne!(g.glyph_id, 0);
                assert!(g.cluster.start < g.cluster.end);
                assert!(
                    g.x_advance_pt.is_finite()
                        && g.y_advance_pt.is_finite()
                        && g.x_offset_pt.is_finite()
                        && g.y_offset_pt.is_finite()
                );
            }
        }
    }
}
pub fn reject(request: Request<'_>, ports: &Ports<'_>, expected: Error) {
    let before = ports.admission.snapshot().unwrap();
    match TextEngine.shaped_runs(request, ports) {
        Ok(_) => panic!("expected {expected:?}, got prepared"),
        Err(r) => assert_eq!(r.error, expected),
    }
    let after = ports.admission.snapshot().unwrap();
    assert_eq!(
        after.current_requested_bytes,
        before.current_requested_bytes
    );
    assert_eq!(after.retained_generations, before.retained_generations);
}

pub fn compare_reference(result: &Prepared<'_>, reference: &Value) {
    assert_eq!(
        result.request().text_utf8,
        reference["original_text"]
            .as_str()
            .expect("original reference text")
    );
    assert_eq!(
        result.request().text_sha256.as_slice(),
        unhex(
            reference["text_sha256"]
                .as_str()
                .expect("reference text SHA256")
        )
    );
    let actual: Vec<&Run<'_>> = result.paragraphs().iter().flat_map(|p| p.runs()).collect();
    let expected = reference["runs"]
        .as_array()
        .expect("independent reference runs");
    // Empty control-only runs may be compacted; retain exact visible ordering and indices.
    let actual_visible: Vec<(usize, &Run<'_>)> = actual
        .iter()
        .enumerate()
        .filter(|(_, r)| !r.glyphs().is_empty())
        .map(|(i, r)| (i, *r))
        .collect();
    let expected_visible: Vec<(usize, &Value)> = expected
        .iter()
        .enumerate()
        .filter(|(_, r)| !r["glyphs"].as_array().unwrap().is_empty())
        .collect();
    assert_eq!(actual_visible.len(), expected_visible.len());
    let mut run_map = vec![None; actual.len()];
    for ((actual_index, run), (expected_index, r)) in actual_visible.iter().zip(&expected_visible) {
        run_map[*actual_index] = Some(*expected_index);
        assert_eq!(
            run.resolved().face_index as u64,
            r["face_index"].as_u64().unwrap()
        );
        assert_eq!(
            run.resolved().content_hash.as_slice(),
            unhex(r["font_sha256"].as_str().unwrap())
        );
        assert_eq!(
            run.units_per_em() as u64,
            r["units_per_em"].as_u64().unwrap()
        );
        let axes = r["axes"].as_array().unwrap();
        assert_eq!(run.axes().len(), axes.len());
        for axis in run.axes() {
            let e = axes
                .iter()
                .find(|e| e["tag"].as_str().unwrap().as_bytes() == axis.axis.tag)
                .expect("reference axis");
            assert_eq!(
                axis.axis.value.to_bits(),
                (e["value"].as_f64().unwrap() as f32).to_bits()
            );
        }
        let features = r["features"].as_array().unwrap();
        assert_eq!(run.features().len(), features.len());
        for feature in run.features() {
            let e = features
                .iter()
                .find(|e| e["tag"].as_str().unwrap().as_bytes() == feature.feature.tag)
                .expect("reference feature");
            assert_eq!(feature.feature.value as u64, e["value"].as_u64().unwrap());
            assert_eq!(e["range"], "whole original UTF8 bytes");
            assert_eq!(
                feature.feature.range,
                SourceRange {
                    start: 0,
                    end: result.request().text_utf8.len() as u64
                }
            );
            if r["font_index"] == 0 && e["tag"] == "liga" {
                assert!(!feature.feature.declared);
                assert_eq!(feature.state, FeatureState::UnavailableInert);
            } else {
                assert_eq!(feature.state, FeatureState::Applied);
            }
        }
        assert_eq!(
            (run.source().start, run.source().end),
            (
                r["source_start"].as_u64().unwrap(),
                r["source_end"].as_u64().unwrap()
            )
        );
        assert_eq!(
            run.resolved().identity,
            NAMES[r["font_index"].as_u64().unwrap() as usize]
        );
        assert_eq!(run.bidi_level() as u64, r["bidi_level"].as_u64().unwrap());
        assert_eq!(
            run.direction(),
            if r["direction"] == "rtl" {
                Direction::RightToLeft
            } else {
                Direction::LeftToRight
            }
        );
        assert_eq!(
            run.script().as_slice(),
            r["script"].as_str().unwrap().as_bytes()
        );
        assert_eq!(run.language(), r["language"].as_str().unwrap());
        assert_eq!(
            run.provider_scale() as u64,
            r["provider_scale"].as_u64().unwrap()
        );
        assert_eq!(run.font_size_pt().to_bits(), 12.0f64.to_bits());
        let glyphs = r["glyphs"].as_array().unwrap();
        assert_eq!(run.glyphs().len(), glyphs.len());
        for (g, e) in run.glyphs().iter().zip(glyphs) {
            assert_eq!(g.glyph_id as u64, e["glyph_id"].as_u64().unwrap());
            assert_eq!(
                (g.cluster.start, g.cluster.end),
                (
                    e["cluster_start"].as_u64().unwrap(),
                    e["cluster_end"].as_u64().unwrap()
                )
            );
            let scale = run.provider_scale() as f64;
            for (actual, raw, point) in [
                (g.x_advance_pt, "x_advance", "x_advance_pt"),
                (g.y_advance_pt, "y_advance", "y_advance_pt"),
                (g.x_offset_pt, "x_offset", "x_offset_pt"),
                (g.y_offset_pt, "y_offset", "y_offset_pt"),
            ] {
                let raw = e[raw].as_i64().unwrap() as f64;
                let expected = raw * 12.0 / scale;
                assert_eq!(
                    actual.to_bits(),
                    if expected == 0.0 {
                        0.0f64.to_bits()
                    } else {
                        expected.to_bits()
                    }
                );
                let bit_index = match point {
                    "x_advance_pt" => 0,
                    "y_advance_pt" => 1,
                    "x_offset_pt" => 2,
                    "y_offset_pt" => 3,
                    _ => unreachable!(),
                };
                let reference_bits = u64::from_str_radix(
                    e["point_bits"][bit_index]
                        .as_str()
                        .expect("independent hex point bits"),
                    16,
                )
                .unwrap();
                assert_eq!(actual.to_bits(), reference_bits);
                let stated = e[point].as_f64().unwrap();
                assert_eq!(
                    actual.to_bits(),
                    if stated == 0.0 {
                        0.0f64.to_bits()
                    } else {
                        stated.to_bits()
                    }
                );
            }
        }
    }
    let coverage = reference["coverage"]
        .as_array()
        .expect("independent coverage");
    assert_eq!(result.coverage().len(), coverage.len());
    for (c, e) in result.coverage().iter().zip(coverage) {
        assert_eq!(
            (c.source.start, c.source.end),
            (e["start"].as_u64().unwrap(), e["end"].as_u64().unwrap())
        );
        let kind = match c.kind {
            CoverageKind::ShapedCluster => "shaped_cluster",
            CoverageKind::NonrenderingControl => "nonrendering_control",
            CoverageKind::ParagraphSeparator => "paragraph_separator",
        };
        assert_eq!(e["kind"], kind);
        if let Some(g) = c.glyphs {
            let flat = result
                .paragraphs()
                .iter()
                .take(g.paragraph as usize)
                .map(|p| p.runs().len())
                .sum::<usize>()
                + g.run as usize;
            assert_eq!(
                run_map[flat].expect("visible reference run") as u64,
                e["run"].as_u64().unwrap()
            );
            assert_eq!(g.start as u64, e["glyph_start"].as_u64().unwrap());
            assert_eq!(g.end as u64, e["glyph_end"].as_u64().unwrap());
        } else {
            assert!(e["run"].is_null() && e["glyph_start"].is_null() && e["glyph_end"].is_null());
        }
    }
    for run in actual.iter().filter(|r| r.glyphs().is_empty()) {
        let covered: u64 = result
            .coverage()
            .iter()
            .filter(|c| {
                c.source.start >= run.source().start
                    && c.source.end <= run.source().end
                    && c.kind == CoverageKind::NonrenderingControl
            })
            .map(|c| c.source.end - c.source.start)
            .sum();
        assert_eq!(
            covered,
            run.source().end - run.source().start,
            "only fully covered nonrendering runs may be compacted"
        );
    }
    let text = result.request().text_utf8;
    assert_eq!(
        text.as_bytes(),
        unhex(reference["original_utf8_hex"].as_str().unwrap())
    );
    let starts: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let bidi = &reference["bidi"];
    let scalar_bytes: Vec<usize> = bidi["scalar_to_utf8"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as usize)
        .collect();
    assert_eq!(starts, scalar_bytes);
    let levels = bidi["levels_l1"].as_array().unwrap();
    assert_eq!(levels.len(), starts.len());
    let visible = |i: usize| {
        result.coverage().iter().any(|c| {
            c.kind == CoverageKind::ShapedCluster
                && c.source.start <= starts[i] as u64
                && (starts[i] as u64) < c.source.end
        })
    };
    let mut visual = Vec::new();
    for (_, run) in &actual_visible {
        let mut indices: Vec<usize> = starts
            .iter()
            .enumerate()
            .filter(|(_, b)| **b >= run.source().start as usize && **b < run.source().end as usize)
            .map(|(i, _)| i)
            .collect();
        for &i in &indices {
            assert_eq!(run.bidi_level() as u64, levels[i].as_u64().unwrap());
        }
        if run.direction() == Direction::RightToLeft {
            indices.reverse();
        }
        visual.extend(indices.into_iter().filter(|i| visible(*i)));
    }
    let expected_order: Vec<usize> = bidi["visual_scalar_order"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as usize)
        .collect();
    let mut permutation = expected_order.clone();
    permutation.sort_unstable();
    assert_eq!(permutation, (0..starts.len()).collect::<Vec<_>>());
    assert_eq!(
        visual,
        expected_order
            .into_iter()
            .filter(|i| visible(*i))
            .collect::<Vec<_>>()
    );
    let byte_runs = bidi["visual_byte_runs"].as_array().unwrap();
    for (_, run) in &actual_visible {
        let e = byte_runs
            .iter()
            .find(|e| {
                e["start"].as_u64().unwrap() <= run.source().start
                    && e["end"].as_u64().unwrap() >= run.source().end
            })
            .expect("actual run inside independent bidi partition");
        assert_eq!(run.bidi_level() as u64, e["level"].as_u64().unwrap());
        assert_eq!(
            e["direction"],
            if run.direction() == Direction::RightToLeft {
                "rtl"
            } else {
                "ltr"
            }
        );
    }
}

pub fn reference_file() -> Value {
    const EXPECTED: &str = "6579f9a90162edaf6dc1bcb91b964d8014eaeaf8d7fef7536243e55bd52c2bbe";
    let path = std::env::var_os("HSK_TYPE_REFERENCE_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            research()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("studio-assets-validator/type-reference-producer/type-reference-corrected-with-dlig.json")
        });
    if let Ok(binding) = std::env::var("HSK_TYPE_REFERENCE_SHA256") {
        assert_eq!(
            binding, EXPECTED,
            "reference binding must be independently verified"
        );
    }
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("required independent oracle {}: {e}", path.display()));
    assert_eq!(hash(&bytes).as_slice(), unhex(EXPECTED));
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["schema"], "handshake.studio.type_reference@1");
    assert_eq!(v["composition_binding"], COMPOSITION_VERSION);
    v
}
pub fn find_reference<'a>(reference: &'a Value, name: &str) -> &'a Value {
    let cases = reference["cases"]
        .as_array()
        .expect("required independent cases array");
    cases
        .iter()
        .find(|c| c["case"] == name)
        .unwrap_or_else(|| panic!("required independently executed case {name} missing"))
}
