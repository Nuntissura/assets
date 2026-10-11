//! Working -> output profile conversion through Prism's ADMITTED route only (STU-CON-044): both
//! profiles are inspected into verified linear-light descriptors first, pixels convert in chunks of
//! at most [`PRISM_CHUNK`] with relative-colorimetric / depth 32 / BPC off, and refusals (including
//! Prism's own range refusal outside 0..1) surface as typed errors; nothing is clipped here.
//! There is no fallback to Prism's legacy unbudgeted route.
use crate::DevelopError;
use crate::recipe::ProfileBinding;
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::SinkPort;
use hsk_studio_prism::{
    AdmittedPrism, Intent, ProfileInput, ProviderAdmission, Transfer, TransformRequest,
};

/// Pixels per Prism transform call (STU-CON-044).
pub const PRISM_CHUNK: usize = 64;

/// Port used by [`crate::develop_with`]; implemented by [`PrismConverter`].
pub trait ProfileConverter {
    /// Identity + hash of the source (working) profile this converter was built for.
    fn source(&self) -> &ProfileBinding;
    /// Identity + hash of the destination (output) profile.
    fn destination(&self) -> &ProfileBinding;
    /// Admitted inspection of both profiles (verified linear-light descriptors).
    fn inspect(&mut self, cancel: &CancellationToken) -> Result<(), DevelopError>;
    /// Convert working-profile linear RGB to output-profile linear RGB.
    fn convert(
        &mut self,
        pixels: &[[f32; 3]],
        cancel: &CancellationToken,
    ) -> Result<Vec<[f32; 3]>, DevelopError>;
    /// Transform calls made so far.
    fn calls(&self) -> u32;
}

/// Exact ICC bytes of one profile with its recipe binding.
#[derive(Clone, Copy)]
pub struct IccProfile<'a> {
    pub binding: &'a ProfileBinding,
    pub icc: &'a [u8],
}

/// Request-scoped Prism context (revision is a local precondition, equal on both sides).
#[derive(Clone, Copy)]
pub struct PrismContext<'a> {
    pub actor: &'a ActorContext,
    pub resource_id: &'a DomainId,
    pub correlation_id: u64,
    pub revision: u64,
}

pub struct PrismConverter<'a, A: ProviderAdmission> {
    prism: &'a mut AdmittedPrism<A>,
    source: IccProfile<'a>,
    destination: IccProfile<'a>,
    context: PrismContext<'a>,
    /// Each Prism call emits one terminal observation here: the sink must accept many.
    sink: &'a mut dyn SinkPort,
    calls: u32,
}

impl<'a, A: ProviderAdmission> PrismConverter<'a, A> {
    pub fn new(
        prism: &'a mut AdmittedPrism<A>,
        source: IccProfile<'a>,
        destination: IccProfile<'a>,
        context: PrismContext<'a>,
        sink: &'a mut dyn SinkPort,
    ) -> Self {
        Self {
            prism,
            source,
            destination,
            context,
            sink,
            calls: 0,
        }
    }
}

fn input<'a>(profile: &IccProfile<'a>) -> ProfileInput<'a> {
    ProfileInput {
        profile_id: &profile.binding.profile_id,
        bytes: profile.icc,
        expected_sha256: profile.binding.sha256,
    }
}

fn map(error: hsk_studio_prism::Error) -> DevelopError {
    match error {
        hsk_studio_prism::Error::Canceled => DevelopError::Canceled,
        other => DevelopError::Prism(other.code()),
    }
}

impl<A: ProviderAdmission> ProfileConverter for PrismConverter<'_, A> {
    fn source(&self) -> &ProfileBinding {
        self.source.binding
    }

    fn destination(&self) -> &ProfileBinding {
        self.destination.binding
    }

    fn inspect(&mut self, cancel: &CancellationToken) -> Result<(), DevelopError> {
        for profile in [&self.source, &self.destination] {
            let request = input(profile);
            let admitted = self.prism.inspect_profile(request, cancel).map_err(map)?;
            let descriptor = admitted.descriptor();
            if descriptor.transfer() != Transfer::LinearLight || !descriptor.matches(request) {
                return Err(DevelopError::InvalidProfile);
            }
        }
        Ok(())
    }

    fn convert(
        &mut self,
        pixels: &[[f32; 3]],
        cancel: &CancellationToken,
    ) -> Result<Vec<[f32; 3]>, DevelopError> {
        let mut out = Vec::with_capacity(pixels.len());
        for chunk in pixels.chunks(PRISM_CHUNK) {
            cancel.check()?;
            let request = TransformRequest {
                source: input(&self.source),
                destination: input(&self.destination),
                pixels: chunk,
                intent: Intent::RelativeColorimetric,
                bit_depth: 32,
                black_point_compensation: false,
                revision: self.context.revision,
                expected_revision: self.context.revision,
                correlation_id: self.context.correlation_id,
                resource_id: self.context.resource_id,
                actor: self.context.actor,
                private_project_text: None,
            };
            let admitted = self
                .prism
                .transform(&request, cancel, &mut *self.sink)
                .map_err(map)?;
            self.calls += 1;
            out.extend_from_slice(&admitted.result().pixels);
        }
        Ok(out)
    }

    fn calls(&self) -> u32 {
        self.calls
    }
}
