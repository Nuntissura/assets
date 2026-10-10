//! Requested allocation-layout accounting for the selected moxcms analytical path.
//! Provenance: moxcms0.9.1 profile.rs984/reader.rs703 parametric trusted-length
//! collection; transform.rs822-838 and conversions/rgbxyz_float.rs46-65,127-179
//! (two Box<ParametricCurve>, one RGB scalar Arc, no LUTs or execution allocation).
//! Rust1.97.1 commit8bab26f4f68e0e26f0bb7960be334d5b520ea452 alloc/src/vec/
//! spec_from_iter_nested.rs TrustedLen, alloc/src/slice.rs to_vec, alloc/src/sync.rs
//! ArcInner repr(C,align(2)) and Layout.extend().pad_to_align(). Counts are requested
//! owned layouts, not allocator bookkeeping/RSS. No text tags => no lossy string,
//! metadata-vector growth or hidden old/new reallocation overlap. All owned vectors
//! have fixed admitted capacity; output and temporary chunk vectors never grow.
//! Pinned raw source URLs: https://raw.githubusercontent.com/rust-lang/rust/
//! 8bab26f4f68e0e26f0bb7960be334d5b520ea452/library/alloc/src/{path}:
//! vec/spec_from_iter_nested.rs48-59 SHA256
//! f3c1a06f43734a65e102b6a107e2fd5adadd7d94d9a226d6ffe1610d1e7e1466;
//! slice.rs403-454 SHA256
//! b1f0a5f6e3d104d6176699cf05efdfe1725095cd984fdf4422ddc3093726361c;
//! sync.rs387-405,424-432 SHA256
//! 768776d4a37cfeb28c71a0c6d8b2ad33332d11ceaef7e47cb01463bd71b9048a.
use super::*;
use std::{
    alloc::Layout as AllocationLayout,
    mem::{align_of, size_of},
    sync::atomic::AtomicUsize,
};

/// Caller-owned scoped ledger. The handle must be inline (no uncharged allocation),
/// retain a borrowed/shared ledger lifetime, and synchronously retire infallibly on
/// Drop. An owner with uncertain/fallible retirement is unsupported before admission.
pub trait ProviderReservation: Send + Sync {
    fn reserved_bytes(&self) -> u64;
    fn retirement_ready(&self) -> Result<(), Error> {
        Ok(())
    }
}
pub trait ProviderAdmission: Send + Sync {
    type Reservation: ProviderReservation;
    fn synchronous_retirement(&self) -> bool;
    fn reserve(&self, bytes: u64) -> Result<Self::Reservation, Error>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileAllocation {
    pub parser_bytes: u64,
    pub descriptor_bytes: u64,
}
fn supported_target() -> Result<(), Error> {
    if !cfg!(all(
        target_arch = "x86_64",
        target_os = "windows",
        target_env = "msvc",
        target_pointer_width = "64"
    )) {
        return Err(Error::UnsupportedAdmission);
    }
    Ok(())
}
fn add(a: u64, b: u64) -> Result<u64, Error> {
    a.checked_add(b).ok_or(Error::InvalidBudget)
}
fn bytes_for<T>(n: usize) -> Result<u64, Error> {
    n.checked_mul(size_of::<T>())
        .and_then(|n| u64::try_from(n).ok())
        .ok_or(Error::InvalidBudget)
}
fn word(bytes: &[u8], at: usize) -> Result<u32, Error> {
    let slice = bytes
        .get(at..at.checked_add(4).ok_or(Error::MalformedProfile)?)
        .ok_or(Error::MalformedProfile)?;
    Ok(u32::from_be_bytes(
        slice.try_into().map_err(|_| Error::MalformedProfile)?,
    ))
}
/// Borrowed, allocation-free cardinality/framing gate. It does not manufacture a
/// validated profile descriptor; actual moxcms parsing/semantic checks follow admission.
pub fn linear_profile_cost(
    input: ProfileInput<'_>,
    token: &CancellationToken,
) -> Result<ProfileAllocation, Error> {
    supported_target()?;
    canceled(token)?;
    if input.profile_id.prefix() != "SCPF" {
        return Err(Error::InvalidProfileId);
    }
    let b = input.bytes;
    if b.len() > MAX_PROFILE_BYTES {
        return Err(Error::ProfileLimit);
    }
    if b.len() < 132 || word(b, 0)? as usize != b.len() {
        return Err(Error::MalformedProfile);
    }
    if profile_hash(b) != input.expected_sha256 {
        return Err(Error::HashMismatch);
    }
    if b[8] != 4
        || &b[12..16] != b"mntr"
        || &b[16..20] != b"RGB "
        || &b[20..24] != b"XYZ "
        || &b[36..40] != b"acsp"
    {
        return Err(Error::UnsupportedProfile);
    }
    let n = word(b, 128)? as usize;
    if n > 8 {
        return Err(Error::UnsupportedProfile);
    }
    let end = n
        .checked_mul(12)
        .and_then(|v| v.checked_add(132))
        .filter(|v| *v <= b.len())
        .ok_or(Error::MalformedProfile)?;
    let names = [
        b"wtpt", b"chad", b"rXYZ", b"gXYZ", b"bXYZ", b"rTRC", b"gTRC", b"bTRC",
    ];
    let mut seen = [false; 8];
    for tag in b[132..end].chunks_exact(12) {
        canceled(token)?;
        let index = names
            .iter()
            .position(|name| tag[..4] == name[..])
            .ok_or(Error::UnsupportedProfile)?;
        if seen[index] {
            return Err(Error::MalformedProfile);
        }
        seen[index] = true;
        let offset = word(tag, 4)? as usize;
        let length = word(tag, 8)? as usize;
        let last = offset
            .checked_add(length)
            .filter(|v| *v <= b.len())
            .ok_or(Error::MalformedProfile)?;
        if offset < end {
            return Err(Error::MalformedProfile);
        }
        let data = &b[offset..last];
        if index >= 5 {
            if length != 16
                || &data[..4] != b"para"
                || data[8..12] != [0, 0, 0, 0]
                || word(data, 12)? != 65536
            {
                return Err(Error::UnsupportedProfile);
            }
        } else if index == 1 {
            if length != 44 || &data[..4] != b"sf32" {
                return Err(Error::UnsupportedProfile);
            }
        } else if length != 20 || &data[..4] != b"XYZ " {
            return Err(Error::UnsupportedProfile);
        }
    }
    if seen.iter().enumerate().any(|(i, v)| i != 1 && !v) {
        return Err(Error::UnsupportedProfile);
    }
    canceled(token)?;
    Ok(ProfileAllocation {
        parser_bytes: bytes_for::<f32>(3)?,
        descriptor_bytes: input.profile_id.as_str().len() as u64,
    })
}
// Each member gets its maximum pre-field padding; final rounding covers tail padding.
// This conservative upper layout remains valid under Rust struct field reordering.
fn field_slot<T>() -> Result<usize, Error> {
    size_of::<T>()
        .checked_add(align_of::<T>() - 1)
        .ok_or(Error::InvalidBudget)
}
fn aligned(n: usize, a: usize) -> Result<usize, Error> {
    n.checked_add(a - 1)
        .map(|v| v / a * a)
        .ok_or(Error::InvalidBudget)
}
pub fn analytical_executor_bytes() -> Result<u64, Error> {
    supported_target()?;
    let a = align_of::<Box<dyn Send + Sync>>().max(align_of::<moxcms::Matrix3f>());
    let profile = aligned(
        field_slot::<Box<dyn Send + Sync>>()?
            .checked_mul(2)
            .and_then(|v| v.checked_add(field_slot::<moxcms::Matrix3f>().ok()?))
            .ok_or(Error::InvalidBudget)?,
        a,
    )?;
    let executor = aligned(
        profile
            .checked_add(a - 1)
            .and_then(|v| v.checked_add(field_slot::<usize>().ok()?))
            .ok_or(Error::InvalidBudget)?,
        a.max(align_of::<usize>()),
    )?;
    let payload = AllocationLayout::from_size_align(executor, a.max(align_of::<usize>()))
        .map_err(|_| Error::InvalidBudget)?;
    let header = AllocationLayout::new::<[AtomicUsize; 2]>();
    let arc = header
        .extend(payload)
        .map_err(|_| Error::InvalidBudget)?
        .0
        .pad_to_align()
        .size() as u64;
    add(arc, bytes_for::<ParametricCurve>(2)?)
}
fn actor_bytes(a: &ActorContext) -> u64 {
    [
        a.account_id(),
        a.principal_id(),
        a.owner_account_id(),
        a.owner_principal_id(),
        a.access_space_id(),
        a.session_id(),
    ]
    .iter()
    .map(|s| s.len() as u64)
    .sum()
}
fn reserve<A: ProviderAdmission>(
    admission: &A,
    bytes: u64,
    t: &CancellationToken,
) -> Result<A::Reservation, Error> {
    if !admission.synchronous_retirement() {
        return Err(Error::UnsupportedAdmission);
    }
    canceled(t)?;
    let lease = admission.reserve(bytes)?;
    if lease.reserved_bytes() < bytes {
        return Err(Error::AdmissionDenied);
    }
    canceled(t)?;
    Ok(lease)
}
// Field order matters: owned allocation drops BEFORE its accounting token.
struct AdmittedEntry<R: ProviderReservation> {
    source: [u8; 32],
    destination: [u8; 32],
    executor: Arc<TransformF32Executor>,
    lease: R,
}
pub struct AdmittedProfile<R: ProviderReservation> {
    descriptor: ProfileDescriptor,
    lease: R,
}
impl<R: ProviderReservation> AdmittedProfile<R> {
    pub fn descriptor(&self) -> &ProfileDescriptor {
        &self.descriptor
    }
    pub fn reserved_bytes(&self) -> u64 {
        self.lease.reserved_bytes()
    }
}
pub struct AdmittedTransform<R: ProviderReservation> {
    result: TransformResult,
    lease: R,
}
impl<R: ProviderReservation> AdmittedTransform<R> {
    pub fn result(&self) -> &TransformResult {
        &self.result
    }
    pub fn reserved_bytes(&self) -> u64 {
        self.lease.reserved_bytes()
    }
}
/// Same moxcms owner/engine; separate narrow admission route, no legacy fallback.
pub struct AdmittedPrism<A: ProviderAdmission> {
    actor: ActorContext,
    capacity: usize,
    cache: Vec<AdmittedEntry<A::Reservation>>,
    stats: CacheStats,
    provider_lease: A::Reservation,
    admission: A,
}
impl<A: ProviderAdmission> AdmittedPrism<A> {
    pub fn new(actor: &ActorContext, cache_entries: usize, admission: A) -> Result<Self, Error> {
        supported_target()?;
        if cache_entries == 0 || cache_entries > MAX_CACHE_ENTRIES {
            return Err(Error::InvalidBudget);
        }
        let cost = add(
            actor_bytes(actor),
            bytes_for::<AdmittedEntry<A::Reservation>>(cache_entries)?,
        )?;
        let lease = reserve(&admission, cost, &CancellationToken::default())?;
        Ok(Self {
            actor: actor.clone(),
            capacity: cache_entries,
            cache: Vec::with_capacity(cache_entries),
            stats: CacheStats {
                entries: 0,
                hits: 0,
                misses: 0,
                rejected: 0,
            },
            admission,
            provider_lease: lease,
        })
    }
    pub fn cache_stats(&self) -> CacheStats {
        self.stats
    }
    pub fn provider_reserved_bytes(&self) -> u64 {
        self.provider_lease.reserved_bytes()
    }
    pub fn try_clear_cache(&mut self) -> Result<(), Error> {
        for entry in &self.cache {
            entry.lease.retirement_ready()?;
        }
        self.cache.clear();
        self.stats.entries = 0;
        Ok(())
    }
    /// None means retired; Some returns the complete reachable provider on refusal.
    pub fn try_retire(self) -> Option<Self> {
        if self.provider_lease.retirement_ready().is_err()
            || self
                .cache
                .iter()
                .any(|e| e.lease.retirement_ready().is_err())
        {
            return Some(self);
        }
        drop(self);
        None
    }
    pub fn inspect_profile(
        &self,
        input: ProfileInput<'_>,
        t: &CancellationToken,
    ) -> Result<AdmittedProfile<A::Reservation>, Error> {
        let cost = linear_profile_cost(input, t)?;
        let descriptor_lease = reserve(&self.admission, cost.descriptor_bytes, t)?;
        let _parse_lease = reserve(&self.admission, cost.parser_bytes, t)?;
        let parsed = parse_profile(input)?;
        canceled(t)?;
        if !matches!(&parsed.red_trc,Some(ToneReprCurve::Parametric(v)) if v.as_slice()==[1.0]) {
            return Err(Error::UnsupportedProfile);
        }
        let descriptor = ProfileDescriptor {
            profile_id: input.profile_id.clone(),
            sha256: input.expected_sha256,
            transfer: Transfer::LinearLight,
        };
        canceled(t)?;
        Ok(AdmittedProfile {
            descriptor,
            lease: descriptor_lease,
        })
    }
    pub fn transform(
        &mut self,
        r: &TransformRequest<'_>,
        t: &CancellationToken,
        sink: &mut dyn SinkPort,
    ) -> Result<AdmittedTransform<A::Reservation>, Error> {
        let result = self.transform_inner(r, t, sink);
        if result.is_err() {
            self.stats.rejected = self.stats.rejected.saturating_add(1);
        }
        result
    }
    fn transform_inner(
        &mut self,
        r: &TransformRequest<'_>,
        t: &CancellationToken,
        sink: &mut dyn SinkPort,
    ) -> Result<AdmittedTransform<A::Reservation>, Error> {
        canceled(t)?;
        if r.actor != &self.actor {
            return Err(Error::ContextMismatch);
        }
        if r.revision != r.expected_revision {
            return Err(Error::RevisionMismatch);
        }
        if r.intent != Intent::RelativeColorimetric {
            return Err(Error::UnsupportedIntent);
        }
        if r.bit_depth != 32 {
            return Err(Error::UnsupportedDepth);
        }
        if r.black_point_compensation {
            return Err(Error::UnsupportedBpc);
        }
        if r.pixels.is_empty() || r.pixels.len() > MAX_PIXELS {
            return Err(Error::PixelLimit);
        }
        for pixel in r.pixels {
            canceled(t)?;
            for channel in pixel {
                if !channel.is_finite() {
                    return Err(Error::NonfiniteChannel);
                }
                if !(0.0..=1.0).contains(channel) {
                    return Err(Error::ChannelRange);
                }
            }
        }
        let source_cost = linear_profile_cost(r.source, t)?;
        let destination_cost = linear_profile_cost(r.destination, t)?;
        let hit = self.cache.iter().position(|e| {
            e.source == r.source.expected_sha256 && e.destination == r.destination.expected_sha256
        });
        if hit.is_none() && self.cache.len() >= self.capacity {
            return Err(Error::CacheFull);
        }
        let transient = add(
            add(source_cost.parser_bytes, destination_cost.parser_bytes)?,
            add(
                bytes_for::<f32>(r.pixels.len().min(64) * 6)?,
                add(
                    actor_bytes(&self.actor),
                    add(
                        r.resource_id.as_str().len() as u64,
                        hsk_studio_observe::MAX_FRAME_BYTES as u64,
                    )?,
                )?,
            )?,
        )?;
        let _transient_lease = reserve(&self.admission, transient, t)?;
        let output_lease = reserve(
            &self.admission,
            add(
                bytes_for::<[f32; 3]>(r.pixels.len())?,
                add(
                    source_cost.descriptor_bytes,
                    destination_cost.descriptor_bytes,
                )?,
            )?,
            t,
        )?;
        let executor_lease = if hit.is_none() {
            Some(reserve(&self.admission, analytical_executor_bytes()?, t)?)
        } else {
            None
        };
        // All provider allocations are admitted before either real parser is called.
        let source = parse_profile(r.source)?;
        canceled(t)?;
        let destination = parse_profile(r.destination)?;
        canceled(t)?;
        let executor = if let Some(i) = hit {
            self.cache[i].executor.clone()
        } else {
            source
                .create_transform_f32(
                    Layout::Rgb,
                    &destination,
                    Layout::Rgb,
                    TransformOptions {
                        rendering_intent: RenderingIntent::RelativeColorimetric,
                        allow_use_cicp_transfer: false,
                        prefer_fixed_point: false,
                        allow_extended_range_rgb_xyz: true,
                        ..Default::default()
                    },
                )
                .map_err(|_| Error::EngineFailure)?
        };
        canceled(t)?;
        let mut output = Vec::with_capacity(r.pixels.len());
        let chunk_capacity = r.pixels.len().min(64) * 3;
        let mut flat = Vec::with_capacity(chunk_capacity);
        let mut converted = Vec::with_capacity(chunk_capacity);
        for chunk in r.pixels.chunks(64) {
            canceled(t)?;
            flat.clear();
            flat.extend(chunk.iter().flatten().copied());
            converted.clear();
            converted.resize(flat.len(), 0.0);
            executor
                .transform(&flat, &mut converted)
                .map_err(|_| Error::EngineFailure)?;
            if converted.iter().any(|v| !v.is_finite()) {
                return Err(Error::NonfiniteOutput);
            }
            for pixel in converted.chunks_exact(3) {
                output.push([pixel[0], pixel[1], pixel[2]]);
            }
        }
        canceled(t)?;
        let mut observe = Observe::new(
            r.correlation_id,
            r.revision,
            r.resource_id.clone(),
            self.actor.clone(),
            Budget::new(0, 0).map_err(|_| Error::InvalidBudget)?,
        );
        let delivery = observe
            .emit(
                Observation {
                    correlation_id: r.correlation_id,
                    revision: r.revision,
                    outcome: Outcome::Success,
                    progress: None,
                    private_project_text: None,
                },
                t,
                &mut ForwardSink(sink),
            )
            .map_err(Error::DiagnosticDelivery)?;
        canceled(t)?;
        if delivery.outcome == Outcome::Canceled {
            return Err(Error::Canceled);
        }
        let receipt = TransformReceipt {
            source_profile_id: r.source.profile_id.clone(),
            destination_profile_id: r.destination.profile_id.clone(),
            source_sha256: r.source.expected_sha256,
            destination_sha256: r.destination.expected_sha256,
            engine_name: ENGINE_NAME,
            engine_version: ENGINE_VERSION,
            icc_version: "v4",
            intent: r.intent,
            bit_depth: r.bit_depth,
            black_point_compensation: false,
            analytical: true,
            options: EvaluationOptions {
                allow_use_cicp_transfer: false,
                prefer_fixed_point: false,
                allow_extended_range_rgb_xyz: true,
                precision_bits: 32,
                clamped_output: false,
            },
            cache_hit: hit.is_some(),
            revision: r.revision,
            correlation_id: r.correlation_id,
            pixel_count: r.pixels.len(),
        };
        if let Some(lease) = executor_lease {
            self.cache.push(AdmittedEntry {
                source: r.source.expected_sha256,
                destination: r.destination.expected_sha256,
                executor,
                lease,
            });
            self.stats.misses = self.stats.misses.saturating_add(1);
        } else {
            self.stats.hits = self.stats.hits.saturating_add(1);
        }
        self.stats.entries = self.cache.len();
        Ok(AdmittedTransform {
            result: TransformResult {
                pixels: output,
                receipt,
                delivery,
                diagnostic_state: observe.state(),
            },
            lease: output_lease,
        })
    }
}
