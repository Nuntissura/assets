use crate::provider::Context;
use crate::*;
use hsk_studio_observe as observe;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProposalDisposition {
    Prepared,
    Accepted,
    Rejected,
    ReconciliationRequired,
}
#[derive(Clone, Copy, Debug)]
pub struct Inspection {
    pub disposition: ProposalDisposition,
    pub error: Option<Error>,
    pub counts: Counts,
    pub cancel_epoch: u64,
    pub delivery: Option<observe::DeliveryError>,
    pub diagnostic_delivery: Option<observe::Error>,
    pub counts_fresh: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingKey {
    pub result_sha256: [u8; 32],
    pub source_sha256: [u8; 32],
    pub target_revision: u64,
    pub cancel_epoch: u64,
}
pub struct PendingToken<'a> {
    key: PendingKey,
    proposal: Prepared<'a>,
    inspection: Inspection,
}
impl<'a> PendingToken<'a> {
    pub fn key(&self) -> PendingKey {
        self.key
    }
    pub fn proposal(&self) -> &Prepared<'a> {
        &self.proposal
    }
    pub fn inspection(&self) -> Inspection {
        self.inspection
    }
}
pub enum Finalized<'a> {
    Accepted {
        proposal: Prepared<'a>,
        inspection: Inspection,
    },
    Rejected {
        inspection: Inspection,
    },
    ReconciliationRequired {
        token: PendingToken<'a>,
    },
}
impl Finalized<'_> {
    pub fn inspection(&self) -> Inspection {
        match self {
            Self::Accepted { inspection, .. } | Self::Rejected { inspection } => *inspection,
            Self::ReconciliationRequired { token } => token.inspection,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reconciliation {
    Accepted,
    Rejected,
    StillIndeterminate,
}
/// Caller owns the complete readset/epoch/grant lock and transactional delivery/transfer.
/// Unsupported serialization returns UnsupportedFinalization before calling the callback.
pub trait FinalizationPort<'a> {
    fn with_exclusive<T>(
        &mut self,
        call: impl FnOnce(&Ports<'a>, &mut dyn PublicationPort) -> T,
    ) -> Result<T, Error>;
    /// Lock the exact prior attempt query and its diagnostic callback together.
    fn with_reconciliation<T>(
        &mut self,
        key: PendingKey,
        call: impl FnOnce(&Ports<'a>, Result<Reconciliation, Error>) -> T,
    ) -> Result<T, Error>;
}
/// Fresh, caller-preowned Observe state for this transition, with the same admission ledger.
/// Publication and transition diagnostics use distinct preowned Observe states.
pub struct DiagnosticPorts<'d, 'a> {
    pub observer: &'d mut observe::Observe,
    pub sink: &'d mut dyn observe::SinkPort,
    pub admission: &'a dyn AdmissionPort,
}
/// All-or-none: definite failures leave target/preimage unchanged. Indeterminate forbids retry.
/// Success includes required Observe delivery and caller publication/lease responsibility.
pub trait PublicationPort {
    fn commit(
        &mut self,
        proposal: &Prepared<'_>,
        event: observe::Observation<'_>,
        detail: &observe::ShapingDetail<'_>,
    ) -> Result<PublicationReceipt, observe::DeliveryError>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicationReceipt {
    Delivered,
    CanceledWithoutPublication,
}
/// The caller stages only the granted ephemeral target, commits after delivery succeeds,
/// and rolls back a definite failure. It never increments source/document authority.
pub trait TargetPublication {
    // After delivery may have escaped, any failed transfer/commit must be Indeterminate.
    fn with_publication(
        &mut self,
        proposal: &Prepared<'_>,
        deliver: impl FnOnce() -> Result<(), observe::DeliveryError>,
    ) -> Result<(), observe::DeliveryError>;
}
/// Concrete shared Observe consumer; transaction mechanics remain caller-owned.
pub struct ObservedPublication<'a, T> {
    pub observer: &'a mut observe::Observe,
    pub sink: &'a mut dyn observe::SinkPort,
    pub target: &'a mut T,
}
impl<T: TargetPublication> PublicationPort for ObservedPublication<'_, T> {
    fn commit(
        &mut self,
        proposal: &Prepared<'_>,
        event: observe::Observation<'_>,
        detail: &observe::ShapingDetail<'_>,
    ) -> Result<PublicationReceipt, observe::DeliveryError> {
        let observer = &mut self.observer;
        let sink = &mut self.sink;
        let mut canceled = false;
        let result = self.target.with_publication(proposal, || {
            observer
                .emit_shaping(event, detail, proposal.request.cancellation, *sink)
                .and_then(|receipt| {
                    if receipt.outcome == observe::Outcome::Canceled {
                        canceled = true;
                        Err(observe::Error::Delivery(observe::DeliveryError::Rejected))
                    } else {
                        Ok(())
                    }
                })
                .map_err(|error| match error {
                    observe::Error::Delivery(error) => error,
                    observe::Error::ReconciliationRequired => observe::DeliveryError::Indeterminate,
                    observe::Error::Saturated => observe::DeliveryError::Saturated,
                    _ => observe::DeliveryError::Rejected,
                })
        });
        match (result, canceled) {
            (Ok(()), false) => Ok(PublicationReceipt::Delivered),
            (Err(observe::DeliveryError::Rejected), true) => {
                Ok(PublicationReceipt::CanceledWithoutPublication)
            }
            (Ok(()), true) => Err(observe::DeliveryError::Indeterminate),
            (Err(error), _) => Err(error),
        }
    }
}
impl Prepared<'_> {
    pub fn inspection(&self) -> Inspection {
        Inspection {
            disposition: ProposalDisposition::Prepared,
            error: None,
            counts: self.counts,
            cancel_epoch: self.request.cancel_epoch,
            delivery: None,
            diagnostic_delivery: None,
            counts_fresh: true,
        }
    }
}
fn diagnostic<'a>(
    proposal: &Prepared<'a>,
    disposition: observe::ShapingDisposition,
    code: Option<observe::ShapingCode>,
) -> Result<observe::ShapingDetail<'a>, Error> {
    let r = &proposal.request;
    let c = proposal.counts;
    let l = r.limits;
    let counts = observe::ShapingCounts::new(
        [
            c.input_bytes,
            c.scalars,
            c.fonts,
            c.runs,
            c.glyphs,
            c.fallback_attempts,
            c.work_units,
            c.retained_generations,
        ],
        [
            c.current_requested_bytes,
            c.operation_peak_requested_bytes,
            c.current_requested_bytes,
        ],
        1,
        observe::ShapingBudget {
            counts: [
                l.input_bytes,
                l.input_scalars,
                l.font_resources,
                l.runs,
                l.glyphs,
                l.fallback_attempts,
                l.work_units,
                l.retained_generations,
            ],
            byte_limit: l.requested_owned_bytes,
            delivery_limit: 1,
        },
    )
    .map_err(|_| Error::InvalidInput)?;
    observe::ShapingDetail::new(
        disposition,
        code,
        Some(match proposal.disposition {
            TextDisposition::Shaped => observe::TextDisposition::Shaped,
            TextDisposition::EmptyInput => observe::TextDisposition::EmptyInput,
            TextDisposition::ControlOnly => observe::TextDisposition::ControlOnly,
        }),
        Some(
            observe::TextAddress::new(
                r.result_target.address.owner,
                observe::TextProperty::ShapedRuns,
            )
            .map_err(|_| Error::InvalidInput)?,
        ),
        observe::ShapingRead {
            source_revision: Some(r.reads[r.text_read_index as usize].expected_revision),
            resolver_revision: Some(r.resolver_revision),
            cancel_epoch: Some(r.cancel_epoch),
            target_revision: Some(r.result_target.address.expected_revision),
        },
        counts,
    )
    .map_err(|_| Error::InvalidInput)
}
pub fn rejected_diagnostic<'a>(
    request: &Request<'a>,
    rejected: &Rejected,
) -> Result<observe::ShapingDetail<'a>, Error> {
    rejection_detail(request, rejected, 0)
}
fn rejection_detail<'a>(
    request: &Request<'a>,
    rejected: &Rejected,
    attempts: u64,
) -> Result<observe::ShapingDetail<'a>, Error> {
    let l = request.limits;
    let c = rejected.counts;
    let counts = observe::ShapingCounts::for_rejection(
        [
            c.input_bytes,
            c.scalars,
            c.fonts,
            c.runs,
            c.glyphs,
            c.fallback_attempts,
            c.work_units,
            0,
        ],
        [
            c.current_requested_bytes,
            c.operation_peak_requested_bytes,
            0,
        ],
        attempts,
        observe::ShapingBudget {
            counts: [
                l.input_bytes,
                l.input_scalars,
                l.font_resources,
                l.runs,
                l.glyphs,
                l.fallback_attempts,
                l.work_units,
                l.retained_generations,
            ],
            byte_limit: l.requested_owned_bytes,
            delivery_limit: 1,
        },
    )
    .map_err(|_| Error::InvalidInput)?;
    observe::ShapingDetail::new(
        observe::ShapingDisposition::Rejected,
        Some(code(rejected.error)),
        None,
        Some(
            observe::TextAddress::new(request.story_id, observe::TextProperty::Text)
                .map_err(|_| Error::InvalidInput)?,
        ),
        observe::ShapingRead {
            source_revision: request
                .reads
                .get(request.text_read_index as usize)
                .map(|v| v.expected_revision),
            resolver_revision: Some(request.resolver_revision),
            cancel_epoch: Some(request.cancel_epoch),
            target_revision: Some(request.result_target.address.expected_revision),
        },
        counts,
    )
    .map_err(|_| Error::InvalidInput)
}
pub fn prepared_diagnostic<'a>(
    proposal: &Prepared<'a>,
) -> Result<observe::ShapingDetail<'a>, Error> {
    diagnostic(proposal, observe::ShapingDisposition::Prepared, None)
}
pub fn pending_diagnostic<'a>(
    token: &PendingToken<'a>,
) -> Result<observe::ShapingDetail<'a>, Error> {
    diagnostic(
        &token.proposal,
        observe::ShapingDisposition::ReconciliationRequired,
        Some(observe::ShapingCode::ReconciliationRequired),
    )
}
fn code(error: Error) -> observe::ShapingCode {
    use observe::ShapingCode as C;
    match error {
        Error::InvalidInput => C::InvalidInput,
        Error::Overflow => C::Overflow,
        Error::Budget => C::BudgetExceeded,
        Error::Allocation => C::Allocation,
        Error::Canceled => C::Canceled,
        Error::StaleEpoch => C::StaleEpoch,
        Error::CorruptFont => C::CorruptFont,
        Error::MissingFont => C::MissingFont,
        Error::MissingGlyph => C::MissingGlyph,
        Error::UnsupportedProfile => C::UnsupportedProfile,
        Error::UnsupportedFeature => C::UnsupportedFeature,
        Error::HashMismatch => C::HashMismatch,
        Error::RevokedFont => C::RevokedFont,
        Error::UnavailableFace => C::UnavailableFace,
        Error::UnavailableFont => C::UnavailableFont,
        Error::UnsupportedFont => C::UnsupportedFont,
        Error::InvalidAxis => C::InvalidAxis,
        Error::AbsentRead => C::AbsentRead,
        Error::UnavailableRead => C::UnavailableRead,
        Error::StaleRead => C::StaleRead,
        Error::UnsupportedFinalization => C::UnsupportedFinalization,
        Error::RejectedDelivery => C::DeliveryRejected,
        Error::UnavailableDelivery => C::DeliveryUnavailable,
        Error::SaturatedDelivery => C::DeliverySaturated,
        Error::IndeterminateDelivery => C::DeliveryIndeterminate,
        Error::LeaseUnavailable => C::LeaseUnavailable,
        Error::UnknownComposition => C::UnknownComposition,
        Error::UnsupportedNormalization => C::UnsupportedNormalization,
        Error::InternalInvariant => C::InternalInvariant,
        Error::InvalidBidi => C::InvalidBidi,
        Error::InvalidMapping => C::InvalidMapping,
        Error::PartialOutput => C::PartialOutput,
    }
}
fn validate(proposal: &Prepared<'_>, ports: &Ports<'_>, ctx: &Context<'_>) -> Result<(), Error> {
    let request = &proposal.request;
    ctx.set_shape_limits(request.limits.glyphs, request.limits.lookups)?;
    let charge = ports.input_lifetime.verify(request)?;
    if charge.lifetime_id == 0 || charge.requested_bytes == 0 {
        return Err(Error::LeaseUnavailable);
    }
    crate::readset::verify(request, ports, ctx)?;
    for para in proposal.paragraphs.iter() {
        ctx.step()?;
        for run in para.runs.iter() {
            ctx.step()?;
            let font = crate::fonts::load(run.resolved, ports, request.limits, ctx)?;
            ports.resolver.verify_lifetime(&font.resource, &font.read)?;
        }
    }
    ctx.poll()
}
fn refresh_counts(inspection: &mut Inspection, admission: &dyn AdmissionPort) -> Result<(), Error> {
    inspection.counts_fresh = false;
    let snapshot = admission.snapshot()?;
    inspection.counts.current_requested_bytes = snapshot.current_requested_bytes;
    inspection.counts.operation_peak_requested_bytes = snapshot.operation_peak_requested_bytes;
    inspection.counts_fresh = true;
    Ok(())
}
fn report(
    request: &Request<'_>,
    detail: &observe::ShapingDetail<'_>,
    diagnostics: &mut DiagnosticPorts<'_, '_>,
    inspection: &mut Inspection,
) {
    let event = observe::Observation {
        correlation_id: request.observation_correlation,
        revision: request.result_target.address.expected_revision,
        outcome: if inspection.disposition == ProposalDisposition::Accepted {
            observe::Outcome::Success
        } else {
            observe::Outcome::Failure(
                if inspection.disposition == ProposalDisposition::ReconciliationRequired {
                    observe::FailureCode::Loss
                } else {
                    observe::FailureCode::Validation
                },
            )
        },
        progress: None,
        private_project_text: None,
    };
    inspection.diagnostic_delivery = diagnostics
        .observer
        .emit_shaping(event, detail, request.cancellation, diagnostics.sink)
        .err();
}
fn reject<'a>(
    proposal: Prepared<'a>,
    mut inspection: Inspection,
    error: Error,
    diagnostics: &mut DiagnosticPorts<'_, 'a>,
) -> Finalized<'a> {
    let request = proposal.request;
    drop(proposal);
    inspection.disposition = ProposalDisposition::Rejected;
    inspection.error = Some(error);
    inspection.counts.retained_generations = 0;
    inspection.delivery = match error {
        Error::RejectedDelivery => Some(observe::DeliveryError::Rejected),
        Error::UnavailableDelivery => Some(observe::DeliveryError::Unavailable),
        Error::SaturatedDelivery => Some(observe::DeliveryError::Saturated),
        _ => inspection.delivery,
    };
    if refresh_counts(&mut inspection, diagnostics.admission).is_err() {
        inspection.diagnostic_delivery = Some(observe::Error::InvalidDetail);
    } else {
        let rejected = Rejected {
            error,
            source: None,
            counts: inspection.counts,
            cancel_epoch: request.cancel_epoch,
        };
        match rejection_detail(&request, &rejected, 1) {
            Ok(detail) => report(&request, &detail, diagnostics, &mut inspection),
            Err(_) => inspection.diagnostic_delivery = Some(observe::Error::InvalidDetail),
        }
    }
    Finalized::Rejected { inspection }
}
pub fn finalize<'a>(
    proposal: Prepared<'a>,
    port: &mut impl FinalizationPort<'a>,
    diagnostics: &mut DiagnosticPorts<'_, 'a>,
) -> Finalized<'a> {
    let mut owned = Some(proposal);
    let mut inspection = owned.as_ref().unwrap().inspection();
    let mut key = None;
    let mut completed = None;
    let guarded = port.with_exclusive(|ports, publication| {
        let result = (|| {
            if !std::ptr::eq(ports.admission, diagnostics.admission) {
                inspection.counts_fresh = false;
                return Err(Error::LeaseUnavailable);
            }
            let proposal = owned.as_ref().unwrap();
            let r = &proposal.request;
            let remaining = r
                .limits
                .work_units
                .checked_sub(inspection.counts.work_units)
                .ok_or(Error::Budget)?;
            let ctx = Context::new(
                r.cancellation,
                ports.context,
                r.cancel_epoch,
                ports.admission,
                remaining,
                r.limits.requested_owned_bytes,
                r.limits.recursion,
            )?;
            let checked = (|| {
                validate(proposal, ports, &ctx)?;
                key = Some(PendingKey {
                    result_sha256: crate::validate::hash(proposal.serialized(), &ctx)?,
                    source_sha256: r.text_sha256,
                    target_revision: r.result_target.address.expected_revision,
                    cancel_epoch: r.cancel_epoch,
                });
                ctx.poll()
            })();
            inspection.counts.work_units = inspection
                .counts
                .work_units
                .checked_add(ctx.work())
                .ok_or(Error::Overflow)?;
            refresh_counts(&mut inspection, ports.admission)?;
            checked?;
            owned.as_mut().unwrap().counts = inspection.counts;
            let proposal = owned.as_ref().unwrap();
            let detail = diagnostic(proposal, observe::ShapingDisposition::Accepted, None)?;
            let event = observe::Observation {
                correlation_id: proposal.request.observation_correlation,
                revision: proposal.request.result_target.address.expected_revision,
                outcome: observe::Outcome::Success,
                progress: None,
                private_project_text: None,
            };
            match publication.commit(proposal, event, &detail) {
                Ok(PublicationReceipt::Delivered) => Ok(()),
                Ok(PublicationReceipt::CanceledWithoutPublication) => Err(Error::Canceled),
                Err(delivery) => Err(match delivery {
                    observe::DeliveryError::Rejected => Error::RejectedDelivery,
                    observe::DeliveryError::Unavailable => Error::UnavailableDelivery,
                    observe::DeliveryError::Saturated => Error::SaturatedDelivery,
                    observe::DeliveryError::Indeterminate => Error::IndeterminateDelivery,
                }),
            }
        })();
        completed = Some(match result {
            Ok(()) => {
                inspection.disposition = ProposalDisposition::Accepted;
                Finalized::Accepted {
                    proposal: owned.take().unwrap(),
                    inspection,
                }
            }
            Err(Error::IndeterminateDelivery) if key.is_some() => {
                inspection.disposition = ProposalDisposition::ReconciliationRequired;
                inspection.error = Some(Error::IndeterminateDelivery);
                inspection.delivery = Some(observe::DeliveryError::Indeterminate);
                let mut token = PendingToken {
                    key: key.unwrap(),
                    proposal: owned.take().unwrap(),
                    inspection,
                };
                match pending_diagnostic(&token) {
                    Ok(detail) => report(
                        &token.proposal.request,
                        &detail,
                        diagnostics,
                        &mut token.inspection,
                    ),
                    Err(_) => {
                        token.inspection.diagnostic_delivery = Some(observe::Error::InvalidDetail)
                    }
                }
                Finalized::ReconciliationRequired { token }
            }
            Err(error) => reject(owned.take().unwrap(), inspection, error, diagnostics),
        });
    });
    if let Some(outcome) = completed {
        return outcome;
    }
    reject(
        owned.take().unwrap(),
        inspection,
        guarded.err().unwrap_or(Error::UnsupportedFinalization),
        diagnostics,
    )
}
/// Query only the exact prior attempt under caller serialization; never replay publication.
/// A failed acceptance diagnostic retains the original pending owner without rolling back a known published target.
pub fn reconcile<'a>(
    token: PendingToken<'a>,
    port: &mut impl FinalizationPort<'a>,
    diagnostics: &mut DiagnosticPorts<'_, 'a>,
) -> Finalized<'a> {
    let key = token.key;
    let mut owned = Some(token);
    let mut completed = None;
    let guarded = port.with_reconciliation(key, |ports, decision| {
        completed = Some((|| {
            let mut token = owned.take().unwrap();
            if !std::ptr::eq(ports.admission, diagnostics.admission) {
                token.inspection.counts_fresh = false;
                token.inspection.diagnostic_delivery = Some(observe::Error::InvalidDetail);
                return Finalized::ReconciliationRequired { token };
            }
            if refresh_counts(&mut token.inspection, ports.admission).is_err() {
                token.inspection.diagnostic_delivery = Some(observe::Error::InvalidDetail);
                return Finalized::ReconciliationRequired { token };
            }
            token.proposal.counts = token.inspection.counts;
            match decision {
                Ok(Reconciliation::Rejected) => reject(
                    token.proposal,
                    token.inspection,
                    Error::RejectedDelivery,
                    diagnostics,
                ),
                Ok(Reconciliation::Accepted) => {
                    token.inspection.disposition = ProposalDisposition::Accepted;
                    token.inspection.error = None;
                    match diagnostic(&token.proposal, observe::ShapingDisposition::Accepted, None) {
                        Ok(detail) => report(
                            &token.proposal.request,
                            &detail,
                            diagnostics,
                            &mut token.inspection,
                        ),
                        Err(_) => {
                            token.inspection.diagnostic_delivery =
                                Some(observe::Error::InvalidDetail)
                        }
                    }
                    if token.inspection.diagnostic_delivery.is_some() {
                        token.inspection.disposition = ProposalDisposition::ReconciliationRequired;
                        Finalized::ReconciliationRequired { token }
                    } else {
                        Finalized::Accepted {
                            proposal: token.proposal,
                            inspection: token.inspection,
                        }
                    }
                }
                Ok(Reconciliation::StillIndeterminate) | Err(_) => {
                    if let Err(error) = decision {
                        token.inspection.error = Some(error);
                    }
                    token.inspection.disposition = ProposalDisposition::ReconciliationRequired;
                    match pending_diagnostic(&token) {
                        Ok(detail) => report(
                            &token.proposal.request,
                            &detail,
                            diagnostics,
                            &mut token.inspection,
                        ),
                        Err(_) => {
                            token.inspection.diagnostic_delivery =
                                Some(observe::Error::InvalidDetail)
                        }
                    }
                    Finalized::ReconciliationRequired { token }
                }
            }
        })());
    });
    if let Some(outcome) = completed {
        return outcome;
    }
    {
        let mut token = owned.take().unwrap();
        token.inspection.error = Some(guarded.err().unwrap_or(Error::UnsupportedFinalization));
        if refresh_counts(&mut token.inspection, diagnostics.admission).is_err() {
            token.inspection.diagnostic_delivery = Some(observe::Error::InvalidDetail);
            return Finalized::ReconciliationRequired { token };
        }
        token.proposal.counts = token.inspection.counts;
        match pending_diagnostic(&token) {
            Ok(detail) => report(
                &token.proposal.request,
                &detail,
                diagnostics,
                &mut token.inspection,
            ),
            Err(_) => token.inspection.diagnostic_delivery = Some(observe::Error::InvalidDetail),
        }
        Finalized::ReconciliationRequired { token }
    }
}
