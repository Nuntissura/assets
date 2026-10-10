//! Immutable proposed patches. All publication/ownership authority remains in injected ports.
use crate::{Error, Grid, MAX_BYTES, MAX_TILES, Mask, Planes, Rect, ResolvedTile};
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{ResourceCounts, TileAddress};
use hsk_studio_prism::{
    AdmittedPrism, Intent, Transfer, TransformReceipt, TransformRequest, profile_hash,
};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub attempted: u32,
    pub admitted: u32,
    pub changed: u32,
    pub output_bytes: u64,
    pub scratch_bytes: u64,
    pub retained_preimage_bytes: u64,
}
impl Counts {
    pub fn bytes(self) -> Result<u64, Error> {
        self.output_bytes
            .checked_add(self.scratch_bytes)
            .and_then(|n| n.checked_add(self.retained_preimage_bytes))
            .ok_or(Error::Overflow)
    }
    pub fn diagnostic(self, limit: u64) -> Result<ResourceCounts, Error> {
        ResourceCounts::new(
            [self.attempted, self.admitted, self.changed],
            MAX_TILES as u32,
            [
                self.output_bytes,
                self.scratch_bytes,
                self.retained_preimage_bytes,
            ],
            limit,
        )
        .map_err(|_| Error::BudgetExceeded)
    }
}
/// Existing caller-owned retirement authority. Context clones its already allocated Arc.
/// Retirement must be synchronous and infallible. Indeterminate owners cannot issue a lease.
pub trait RetirementPort: Send + Sync {
    fn retire(&self, handle: u64, counts: Counts);
}
pub struct RetirementContext {
    pub(crate) port: std::sync::Arc<dyn RetirementPort>,
    pub(crate) handle: u64,
    pub(crate) counts: Counts,
}
impl RetirementContext {
    /// The port Arc/control allocation belongs to the caller's pre-admitted lifetime.
    /// This constructor only moves it and validates bounded counters; it allocates nothing.
    pub fn new(
        port: std::sync::Arc<dyn RetirementPort>,
        handle: u64,
        counts: Counts,
    ) -> Result<Self, Error> {
        counts.diagnostic(crate::MAX_BYTES)?;
        Ok(Self {
            port,
            handle,
            counts,
        })
    }
}
impl Drop for RetirementContext {
    fn drop(&mut self) {
        self.port.retire(self.handle, self.counts);
    }
}
pub trait LeasePort: Send + Sync {
    fn handle(&self) -> u64;
    fn counts(&self) -> Counts;
    fn reduce(&mut self, counts: Counts) -> Result<(), Error>;
    fn mark_transferred(&mut self);
    fn retirement(&self) -> RetirementContext;
}
struct LeaseInner {
    port: std::sync::Mutex<Box<dyn LeasePort>>,
}
/// Every clone invokes into_inner; exactly one obtains the value after Arc storage is freed.
#[derive(Clone)]
pub struct Lease {
    inner: Option<std::sync::Arc<LeaseInner>>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(inner) = std::sync::Arc::into_inner(self.inner.take().expect("owned lease")) {
            let port = inner.port.into_inner().unwrap_or_else(|p| p.into_inner());
            let retirement = port.retirement(); // inline, no new allocation
            drop(port); // Box<MemoryLease> is freed while its charge remains live
            drop(retirement); // Arc control and boxed provider are already freed
        }
    }
}
impl Lease {
    pub fn allocation_bytes() -> Result<u64, Error> {
        crate::allocation::arc(std::alloc::Layout::new::<LeaseInner>())
    }
    pub fn new(port: Box<dyn LeasePort>) -> Self {
        Self {
            inner: Some(std::sync::Arc::new(LeaseInner {
                port: std::sync::Mutex::new(port),
            })),
        }
    }
    fn inner(&self) -> &LeaseInner {
        self.inner.as_ref().expect("owned lease")
    }
    pub fn handle(&self) -> u64 {
        self.inner()
            .port
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .handle()
    }
    pub fn counts(&self) -> Counts {
        self.inner()
            .port
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .counts()
    }
    pub(crate) fn reduce(&mut self, counts: Counts) -> Result<(), Error> {
        self.inner()
            .port
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .reduce(counts)
    }
    pub(crate) fn mark_transferred(&mut self) {
        self.inner()
            .port
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .mark_transferred()
    }
}
impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease")
            .field("handle", &self.handle())
            .field("counts", &self.counts())
            .finish()
    }
}
pub struct TileItem<'a> {
    pub before: ResolvedTile<'a>,
    pub replacement: ResolvedTile<'a>,
    pub expected_property_revision: u64,
}
pub struct Request<'a> {
    pub transport_version: u8,
    pub operation: &'a str,
    pub document_id: &'a DomainId,
    pub layer_id: &'a DomainId,
    pub command_id: &'a str,
    pub correlation_id: u64,
    pub actor: &'a ActorContext,
    pub base_revision: u64,
    pub cancel_epoch: u64,
    pub grid: Grid,
    pub rectangle: Rect,
    pub mask: Mask<'a>,
    pub tiles: &'a [TileItem<'a>],
    pub byte_limit: u64,
}
#[derive(Debug)]
pub struct Update {
    pub address: TileAddress,
    pub previous_revision: u64,
    pub revision: u64,
    pub damage: Rect,
    pub after: Planes,
    pub preimage: Planes,
    pub conversions: Vec<TransformReceipt>,
    pub conversion_frames: Vec<Vec<u8>>,
}
#[derive(Debug)]
pub struct Patch {
    pub document_id: DomainId,
    pub command_id: String,
    pub correlation_id: u64,
    pub actor: ActorContext,
    pub cancel_epoch: u64,
    pub updates: Vec<Update>,
    pub counts: Counts,
    pub lease: Option<Lease>,
    pub diagnostic: hsk_studio_observe::Receipt,
}
/// Immutable public view; container allocation is freed before outer accounting retires.
#[derive(Debug)]
pub struct AcceptedPatch {
    data: Option<Box<Patch>>,
    metadata_lease: crate::caller::ProviderLease,
}
impl AcceptedPatch {
    pub(crate) fn new(data: Patch, metadata_lease: crate::caller::ProviderLease) -> Self {
        Self {
            data: Some(Box::new(data)),
            metadata_lease,
        }
    }
    pub fn metadata_reserved_bytes(&self) -> u64 {
        hsk_studio_prism::ProviderReservation::reserved_bytes(&self.metadata_lease)
    }
}
impl std::ops::Deref for AcceptedPatch {
    type Target = Patch;
    fn deref(&self) -> &Patch {
        self.data.as_deref().expect("owned accepted patch")
    }
}
impl Drop for AcceptedPatch {
    fn drop(&mut self) {
        drop(self.data.take());
    }
}
/// Allocation-free named reconciliation identity. Wire rendering borrows this value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingToken {
    bytes: [u8; 64],
    len: u8,
}
impl PendingToken {
    pub(crate) fn new(sequence: u64) -> Self {
        let mut token = Self {
            bytes: [0; 64],
            len: 0,
        };
        use std::fmt::Write as _;
        write!(&mut token, "pigment-pending-{sequence}").expect("bounded pending identity");
        token
    }
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).expect("ASCII pending identity")
    }
}
impl std::fmt::Write for PendingToken {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let start = usize::from(self.len);
        let end = start
            .checked_add(s.len())
            .filter(|n| *n <= 64)
            .ok_or(std::fmt::Error)?;
        self.bytes[start..end].copy_from_slice(s.as_bytes());
        self.len = end as u8;
        Ok(())
    }
}
impl std::ops::Deref for PendingToken {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}
#[derive(Debug)]
pub struct Failure {
    pub error: Error,
    pub counts: Counts,
    pub delivery: Result<hsk_studio_observe::Receipt, hsk_studio_observe::Error>,
}
#[derive(Debug)]
pub enum Outcome {
    Accepted(AcceptedPatch),
    Rejected(Failure),
    ReconciliationRequired {
        token: PendingToken,
        counts: Counts,
        delivery: hsk_studio_observe::Error,
    },
}
/// Prepared never means accepted. Finalization consumes it, retaining it on indeterminate ownership.
pub struct Prepared {
    pub updates: Vec<Update>,
    pub counts: Counts,
    pub lease: Option<Lease>,
}
/// Caller-owned, source-local ports. Finalize MUST serialize fresh membership/property/epoch/
/// cancellation validation, required sink delivery and lease transfer as one boundary.
/// It must retain Prepared under a named token on any indeterminate decision; no blind retry.
pub trait ExecutionPort {
    fn current_epoch(&mut self) -> Result<u64, Error>;
    fn current_membership(&mut self, document: &DomainId, layer: &DomainId) -> Result<(), Error>;
    fn current_revision(
        &mut self,
        document: &DomainId,
        layer: &DomainId,
        address: &TileAddress,
    ) -> Result<u64, Error>;
    /// Bound admitted provider scratch separately from the leaf's exact scratch allocation.
    fn provider_admission(
        &mut self,
        byte_limit: u64,
    ) -> Result<crate::caller::MemoryAdmission, Error>;
    fn lease_layout_bytes(&mut self) -> Result<u64, Error>;
    fn reserve(&mut self, counts: Counts, byte_limit: u64) -> Result<Lease, Error>;
    fn finalize(
        &mut self,
        request: &Request<'_>,
        prepared: Prepared,
        cancel: &CancellationToken,
    ) -> Outcome;
    fn reject(
        &mut self,
        request: &Request<'_>,
        error: Error,
        counts: Counts,
        cancel: &CancellationToken,
    ) -> Outcome;
}
pub fn check_epoch(
    port: &mut impl ExecutionPort,
    r: &Request<'_>,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    if cancel.check().is_err() {
        return Err(Error::Canceled);
    }
    if port.current_epoch()? != r.cancel_epoch {
        return Err(Error::StaleEpoch);
    }
    Ok(())
}
pub fn address(item: &TileItem<'_>) -> Result<TileAddress, Error> {
    TileAddress::new(
        DomainId::parse(&item.before.tile_ref.layer_id).map_err(|_| Error::InvalidInput)?,
        &item.before.tile_ref.object_key,
        item.before.tile_ref.column,
        item.before.tile_ref.row,
    )
    .map_err(|_| Error::InvalidInput)
}
pub fn fresh(
    port: &mut impl ExecutionPort,
    r: &Request<'_>,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    check_epoch(port, r, cancel)?;
    port.current_membership(r.document_id, r.layer_id)?;
    let address_bytes = r
        .tiles
        .iter()
        .map(|t| t.before.tile_ref.layer_id.len() + t.before.tile_ref.object_key.len())
        .max()
        .unwrap_or(0) as u64;
    let admission = port.provider_admission(r.byte_limit)?;
    let _address_lease = hsk_studio_prism::ProviderAdmission::reserve(&admission, address_bytes)
        .map_err(Error::Prism)?;
    for item in r.tiles {
        let a = address(item)?;
        if port.current_revision(r.document_id, r.layer_id, &a)? != item.expected_property_revision
        {
            return Err(Error::RevisionConflict);
        }
    }
    Ok(())
}
pub fn masked_replace(
    r: &Request<'_>,
    port: &mut impl ExecutionPort,
    cancel: &CancellationToken,
) -> Outcome {
    let mut counts = Counts {
        attempted: u32::try_from(r.tiles.len()).unwrap_or(u32::MAX),
        ..Counts::default()
    };
    match compute(r, port, cancel, &mut counts) {
        Ok(prepared) => port.finalize(r, prepared, cancel),
        Err(error) => {
            counts.changed = 0;
            port.reject(r, error, counts, cancel)
        }
    }
}
fn compute(
    r: &Request<'_>,
    port: &mut impl ExecutionPort,
    cancel: &CancellationToken,
    counts: &mut Counts,
) -> Result<Prepared, Error> {
    if r.transport_version != 1 || r.operation != "masked_replace" {
        return Err(Error::Unsupported);
    }
    if r.document_id.prefix() != "SDOC"
        || r.layer_id.prefix() != "SLYR"
        || r.command_id.is_empty()
        || r.command_id.len() > 128
        || r.command_id.chars().any(char::is_control)
        || r.tiles.len() > MAX_TILES
        || r.byte_limit == 0
        || r.byte_limit > MAX_BYTES
    {
        return Err(Error::InvalidInput);
    }
    r.rectangle.end()?;
    if r.rectangle.area() > 4096 {
        return Err(Error::BudgetExceeded);
    }
    r.mask.validate(r.rectangle)?;
    check_epoch(port, r, cancel)?;
    port.current_membership(r.document_id, r.layer_id)?;
    let mut area = 0u64;
    for (i, item) in r.tiles.iter().enumerate() {
        check_epoch(port, r, cancel)?;
        let before = &item.before;
        let replacement = &item.replacement;
        let key = &before.tile_ref.object_key;
        if key.is_empty()
            || key.len() > 128
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(Error::InvalidInput);
        }
        before.validate_shape(r.grid)?;
        replacement.validate_shape(r.grid)?;
        if before.tile_ref.layer_id != r.layer_id.as_str()
            || replacement.tile_ref.layer_id != before.tile_ref.layer_id
            || replacement.tile_ref.object_key != before.tile_ref.object_key
            || replacement.tile_ref.column != before.tile_ref.column
            || replacement.tile_ref.row != before.tile_ref.row
            || replacement.bounds != before.bounds
        {
            return Err(Error::InvalidInput);
        }
        for other in &r.tiles[..i] {
            if other.before.tile_ref.object_key == before.tile_ref.object_key
                || (other.before.tile_ref.column == before.tile_ref.column
                    && other.before.tile_ref.row == before.tile_ref.row)
                || other.before.bounds.intersect(before.bounds)?.is_some()
            {
                return Err(Error::DuplicateTarget);
            }
        }
        if let Some(intersection) = before.bounds.intersect(r.rectangle)? {
            area = area
                .checked_add(intersection.area())
                .ok_or(Error::Overflow)?;
            let colour = before.colour_bytes.len();
            let alpha = before.alpha_bytes.len();
            let mut output = crate::allocation::array::<u8>(colour)?;
            crate::allocation::add(&mut output, crate::allocation::array::<u8>(alpha)?)?;
            crate::allocation::add(&mut output, crate::allocation::arc_bytes(colour)?)?;
            crate::allocation::add(&mut output, crate::allocation::arc_bytes(alpha)?)?;
            let reference = tile_ref_string_bytes(before.tile_ref)?;
            crate::allocation::add(&mut output, reference)?;
            // Stable diagnostic address owns two exact-length strings.
            crate::allocation::add(
                &mut output,
                (r.layer_id.as_str().len() + before.tile_ref.object_key.len()) as u64,
            )?;
            crate::allocation::add(&mut counts.output_bytes, output)?;
            let mut retained = crate::allocation::arc_bytes(colour)?;
            crate::allocation::add(&mut retained, crate::allocation::arc_bytes(alpha)?)?;
            crate::allocation::add(&mut retained, reference)?;
            crate::allocation::add(&mut counts.retained_preimage_bytes, retained)?;
            if before.profile.profile_id != replacement.profile.profile_id
                || before.profile.expected_sha256 != replacement.profile.expected_sha256
            {
                let chunks = intersection.area().div_ceil(64) as usize;
                crate::allocation::add(
                    &mut counts.output_bytes,
                    crate::allocation::array::<TransformReceipt>(chunks)?,
                )?;
                crate::allocation::add(
                    &mut counts.output_bytes,
                    crate::allocation::array::<Vec<u8>>(chunks)?,
                )?;
                let per_chunk = (before.profile.profile_id.as_str().len()
                    + replacement.profile.profile_id.as_str().len()
                    + hsk_studio_observe::MAX_FRAME_BYTES) as u64;
                crate::allocation::add(
                    &mut counts.output_bytes,
                    per_chunk
                        .checked_mul(chunks as u64)
                        .ok_or(Error::Overflow)?,
                )?;
            }
        }
    }
    if area != r.rectangle.area() {
        return Err(Error::CoverageGap);
    }
    if area > 0 {
        counts.scratch_bytes = crate::allocation::array::<(i64, i64, f64)>(64)?;
        crate::allocation::add(
            &mut counts.scratch_bytes,
            crate::allocation::array::<[f32; 3]>(64)?,
        )?;
        crate::allocation::add(
            &mut counts.scratch_bytes,
            (r.layer_id.as_str().len() + 128) as u64,
        )?;
        crate::allocation::add(
            &mut counts.output_bytes,
            crate::allocation::array::<Update>(r.tiles.len())?,
        )?;
        crate::allocation::add(
            &mut counts.output_bytes,
            (r.document_id.as_str().len() * 2
                + r.layer_id.as_str().len()
                + r.command_id.len()
                + actor_bytes(r.actor) * 2
                + 128
                + hsk_studio_observe::MAX_FRAME_BYTES
                + r.layer_id.as_str().len()
                + 128) as u64,
        )?;
        crate::allocation::add(&mut counts.output_bytes, port.lease_layout_bytes()?)?
    }
    if counts.bytes()? > r.byte_limit {
        return Err(Error::BudgetExceeded);
    }
    let mut lease = if r.rectangle.area() > 0 {
        Some(port.reserve(*counts, r.byte_limit)?)
    } else {
        None
    };
    counts.admitted = counts.attempted;
    fresh(port, r, cancel)?;
    if r.rectangle.area() == 0 {
        return Ok(Prepared {
            updates: Vec::new(),
            counts: *counts,
            lease: None,
        });
    }
    let provider_operation = port
        .provider_admission(r.byte_limit)?
        .begin_operation()
        .map_err(Error::Prism)?;
    let mut engine =
        AdmittedPrism::new(r.actor, 1, provider_operation.admission()).map_err(Error::Prism)?;
    let mut updates = Vec::with_capacity(r.tiles.len());
    for item in r.tiles {
        let before = &item.before;
        let replacement = &item.replacement;
        check_epoch(port, r, cancel)?;
        before.validate_hashes()?;
        replacement.validate_hashes()?;
        let target = engine
            .inspect_profile(before.profile, cancel)
            .map_err(Error::Prism)?;
        let source = engine
            .inspect_profile(replacement.profile, cancel)
            .map_err(Error::Prism)?;
        if target.descriptor().transfer() != Transfer::LinearLight
            || source.descriptor().transfer() != Transfer::LinearLight
            || !target.descriptor().matches(before.profile)
            || !source.descriptor().matches(replacement.profile)
        {
            return Err(Error::Unsupported);
        }
        // Scan all declared samples (padding excluded); never hide malformed unused input.
        let (endx, endy) = before.bounds.end()?;
        for y in before.bounds.y..endy {
            check_epoch(port, r, cancel)?;
            for x in before.bounds.x..endx {
                for t in [before, replacement] {
                    let (c, a) = t.pixel(x, y);
                    if c.iter().any(|v| !v.is_finite()) || !a.is_finite() {
                        return Err(Error::Nonfinite);
                    }
                    if !(0.0..=1.0).contains(&a) {
                        return Err(Error::AlphaRange);
                    }
                }
            }
        }
        let Some(region) = before.bounds.intersect(r.rectangle)? else {
            continue;
        };
        let (ex, ey) = region.end()?;
        let conversion = target.descriptor().profile_id() != source.descriptor().profile_id()
            || target.descriptor().sha256() != source.descriptor().sha256();
        let mut colours: Option<Vec<u8>> = None;
        let mut alphas: Option<Vec<u8>> = None;
        let mut damage: Option<Rect> = None;
        let receipt_capacity = if conversion {
            region.area().div_ceil(64) as usize
        } else {
            0
        };
        let mut receipts = Vec::with_capacity(receipt_capacity);
        let mut frames = Vec::with_capacity(receipt_capacity);
        let mut chunk = Vec::with_capacity(64); // scratch admitted before fanout.
        for y in region.y..ey {
            for x in region.x..ex {
                check_epoch(port, r, cancel)?;
                let m = r.mask.coverage(x, y);
                if m == 0.0 {
                    continue;
                }
                chunk.push((x, y, m));
                if chunk.len() == 64 {
                    apply_chunk(
                        &chunk,
                        item,
                        r,
                        &mut engine,
                        cancel,
                        conversion,
                        &mut colours,
                        &mut alphas,
                        &mut damage,
                        &mut receipts,
                        &mut frames,
                    )?;
                    chunk.clear()
                }
            }
        }
        if !chunk.is_empty() {
            apply_chunk(
                &chunk,
                item,
                r,
                &mut engine,
                cancel,
                conversion,
                &mut colours,
                &mut alphas,
                &mut damage,
                &mut receipts,
                &mut frames,
            )?
        }
        if let Some(damage) = damage {
            let colour: std::sync::Arc<[u8]> = colours.expect("changed colour allocation").into();
            let alpha: std::sync::Arc<[u8]> = alphas.expect("changed alpha allocation").into();
            let after = Planes {
                tile_ref: before.tile_ref.clone(),
                profile_sha256: before.profile.expected_sha256,
                bounds: before.bounds,
                colour_sha256: profile_hash(&colour),
                alpha_sha256: profile_hash(&alpha),
                colour: crate::tile::OwnedPlane::new(
                    colour,
                    lease.as_ref().expect("changed reservation").clone(),
                ),
                alpha: crate::tile::OwnedPlane::new(
                    alpha,
                    lease.as_ref().expect("changed reservation").clone(),
                ),
                colour_stride_bytes: before.colour_stride_bytes,
                alpha_stride_bytes: before.alpha_stride_bytes,
            };
            updates.push(Update {
                address: address(item)?,
                previous_revision: item.expected_property_revision,
                revision: item
                    .expected_property_revision
                    .checked_add(1)
                    .ok_or(Error::Overflow)?,
                damage,
                after,
                preimage: Planes::copied(
                    before,
                    lease.as_ref().expect("changed reservation").clone(),
                ),
                conversions: receipts,
                conversion_frames: frames,
            });
        }
    }
    counts.changed = updates.len() as u32;
    drop(engine); // Retire provider cache and all parsed/converted owners before transfer.
    counts.scratch_bytes = counts
        .scratch_bytes
        .checked_add(provider_operation.peak_bytes())
        .ok_or(Error::Overflow)?;
    if counts.bytes()? > r.byte_limit {
        return Err(Error::BudgetExceeded);
    }
    if let Some(l) = &mut lease {
        let retained = Counts {
            scratch_bytes: 0,
            changed: counts.changed,
            ..*counts
        };
        l.reduce(retained)?;
    }
    if updates.is_empty() {
        updates = Vec::new(); // Retire reserved vector capacity before its lease.
        lease = None;
    }
    fresh(port, r, cancel)?;
    Ok(Prepared {
        updates,
        counts: *counts,
        lease,
    })
}
#[allow(clippy::too_many_arguments)]
fn apply_chunk(
    chunk: &[(i64, i64, f64)],
    item: &TileItem<'_>,
    r: &Request<'_>,
    engine: &mut AdmittedPrism<crate::caller::MemoryAdmission>,
    cancel: &CancellationToken,
    conversion: bool,
    colours: &mut Option<Vec<u8>>,
    alphas: &mut Option<Vec<u8>>,
    damage: &mut Option<Rect>,
    receipts: &mut Vec<TransformReceipt>,
    frames: &mut Vec<Vec<u8>>,
) -> Result<(), Error> {
    let mut pixels = Vec::with_capacity(64);
    for (x, y, _) in chunk {
        pixels.push(item.replacement.pixel(*x, *y).0);
    }
    let mut converted = None;
    if conversion {
        let mut collector = crate::caller::ConversionFrame::new();
        let result = engine
            .transform(
                &TransformRequest {
                    source: item.replacement.profile,
                    destination: item.before.profile,
                    pixels: &pixels,
                    intent: Intent::RelativeColorimetric,
                    bit_depth: 32,
                    black_point_compensation: false,
                    revision: r.base_revision,
                    expected_revision: r.base_revision,
                    correlation_id: r.correlation_id,
                    resource_id: r.document_id,
                    actor: r.actor,
                    private_project_text: None,
                },
                cancel,
                &mut collector,
            )
            .map_err(Error::Prism)?;
        receipts.push(result.result().receipt.clone());
        frames.push(collector.into_frame());
        converted = Some(result);
    }
    let transformed = converted
        .as_ref()
        .map(|v| v.result().pixels.as_slice())
        .unwrap_or(&pixels);
    for ((x, y, m), c1) in chunk.iter().zip(transformed) {
        let (c0, a0) = item.before.pixel(*x, *y);
        let (_, a1) = item.replacement.pixel(*x, *y);
        let a = (1.0 - m) * f64::from(a0) + m * f64::from(a1);
        let out = std::array::from_fn::<f32, 3, _>(|i| {
            if a > 0.0 {
                (((1.0 - m) * f64::from(a0) * f64::from(c0[i])
                    + m * f64::from(a1) * f64::from(c1[i]))
                    / a) as f32
            } else {
                0.0
            }
        });
        let alpha = a as f32;
        if out.iter().any(|v| !v.is_finite()) || !alpha.is_finite() {
            return Err(Error::Nonfinite);
        }
        if out == c0 && alpha == a0 {
            continue;
        }
        let colours = colours.get_or_insert_with(|| item.before.colour_bytes.to_vec());
        let alphas = alphas.get_or_insert_with(|| item.before.alpha_bytes.to_vec());
        let row = (*y - item.before.bounds.y) as usize;
        let col = (*x - item.before.bounds.x) as usize;
        let at = row * item.before.colour_stride_bytes as usize + col * 12;
        for i in 0..3 {
            if out[i] != c0[i] {
                colours[at + i * 4..at + i * 4 + 4].copy_from_slice(&out[i].to_le_bytes())
            }
        }
        let at = row * item.before.alpha_stride_bytes as usize + col * 4;
        if alpha != a0 {
            alphas[at..at + 4].copy_from_slice(&alpha.to_le_bytes())
        }
        *damage = Some(match *damage {
            None => Rect {
                x: *x,
                y: *y,
                width: 1,
                height: 1,
            },
            Some(d) => {
                let (ex, ey) = d.end()?;
                let min_x = d.x.min(*x);
                let min_y = d.y.min(*y);
                Rect {
                    x: min_x,
                    y: min_y,
                    width: u32::try_from(ex.max(*x + 1) - min_x).map_err(|_| Error::Overflow)?,
                    height: u32::try_from(ey.max(*y + 1) - min_y).map_err(|_| Error::Overflow)?,
                }
            }
        });
    }
    Ok(())
}

pub(crate) fn actor_bytes(actor: &ActorContext) -> usize {
    actor.account_id().len()
        + actor.principal_id().len()
        + actor.owner_account_id().len()
        + actor.owner_principal_id().len()
        + actor.access_space_id().len()
        + actor.session_id().len()
}
fn tile_ref_string_bytes(t: &hsk_studio_folio::TileRef) -> Result<u64, Error> {
    let mut total = 0;
    for s in [
        &t.object_key,
        &t.layer_id,
        &t.format,
        &t.colour_profile_id,
        &t.content_digest.algorithm,
        &t.content_digest.digest,
        &t.artifact_manifest_id,
    ] {
        crate::allocation::add(&mut total, s.len() as u64)?
    }
    Ok(total)
}
