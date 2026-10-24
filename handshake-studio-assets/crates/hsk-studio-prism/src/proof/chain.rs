//! The two-intent proof chain (STU-COL-022, STU-COL-023, STU-COL-240, STU-COL-241).
//!
//! `prepare_soft_proof` admits the proof, document and display profiles (wide-path admission: identity, size
//! and SHA-256 before any parsing, then bounded framing) and builds three immutable stages:
//! 1. document -> proof device: the wide-path plan for `proof_intent` with the condition's black point
//!    compensation flag (ISO 18619, `bpc.rs`), unfused; a matrix/Gray proof device's linear values are clamped
//!    to 0..1 (the device cannot leave its gamut), a table proof device clamps in its own tables;
//! 2. colour-vision / operator conditions only: the simulation matrix on the linear-light sRGB proof device
//!    values, clamped to 0..1 (`cvd.rs`);
//! 3. proof device -> display: the proof profile's device -> PCS stage and the display's PCS -> device stage
//!    for `display_intent`, with a Prism PCS map between them. Studio readings of the two STU-COL-241 booleans
//!    (Photoshop numerics UNVERIFIED): `simulate_black_ink` = no black point compensation on this stage
//!    (otherwise ISO 18619 maps the proof black onto the display black); `simulate_paper_color` = absolute
//!    colorimetric on this stage (colorimetric tables, PCS scaled by proof `wtpt` / display `wtpt`), applied
//!    AFTER the black point map, so the two booleans stay independent: paper without black ink keeps the
//!    display black (scaled by the paper white), black ink without paper keeps the display white
//!    (tests/soft_proof.rs `paper_and_black_ink_flags_are_independent`).
//!
//! Toggled off, the preview is the ordinary document -> display transform: the wide-path plan for
//! `display_intent` with black point compensation on, fused exactly as `WideEngine::transform` runs it (tests/
//! soft_proof.rs `toggle_off_equals_the_plain_display_transform`). The document buffer is borrowed immutably
//! and never written (`soft_proof_never_changes_document_values`).
#![deny(clippy::arithmetic_side_effects)] // V4-01: hostile sizes, explicit checked/saturating/wrapping math
use super::cvd::{CVD_PROOF_DEVICE_PROFILE_ID, simulate};
use super::{ProofCondition, ProofConditionRecord, ProofError, ProofProfile};
use crate::bpc::{BpcInputs, compute as bpc_compute};
use crate::icc_eval::{self, FromPcs, ToPcs};
use crate::wide::{
    AbsoluteReport, BpcReport, CHUNK_PIXELS, ClipReport, DeviceSpace, MAX_WIDE_PIXELS, Plan, WIDE_ENGINE_NAME,
    WIDE_ENGINE_VERSION, WideError, WideProfileDescriptor, admit, canceled,
};
use crate::{Intent, ProfileInput, WORKING_PROFILE_LINEAR_SRGB, profile_hash};
use hsk_studio_accord::{CancellationToken, DomainId};
use moxcms::ColorProfile;
use sha2::{Digest, Sha256};

/// Display stage: proof device -> PCS, `n' = scale * n + offset` (component-wise), PCS -> display device.
pub(super) struct DisplayStage {
    to_pcs: ToPcs,
    from_pcs: FromPcs,
    scale: [f64; 3],
    offset: [f64; 3],
}
impl DisplayStage {
    pub(super) fn eval(&self, px: &[f32], out: &mut [f32]) {
        let n = self.to_pcs.eval(px);
        let (s, o) = (self.scale, self.offset);
        let m = [s[0] * n[0] + o[0], s[1] * n[1] + o[1], s[2] * n[2] + o[2]];
        match self.from_pcs.linear(m) {
            Some(lin) => self.from_pcs.encode_linear(lin, out),
            None => self.from_pcs.eval(m, out),
        }
    }
}

/// What the display stage applies, reported in every receipt.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayStageReport {
    /// The intent whose tables the display stage uses: `AbsoluteColorimetric` when paper colour is simulated,
    /// else the condition's `display_intent`.
    pub intent: Intent,
    /// Black point compensation of the display stage (`requested` is false when black ink is simulated).
    pub bpc: BpcReport,
    /// Proof `wtpt` / display `wtpt` when the stage is absolute colorimetric (paper colour simulated).
    pub paper_white_scale: Option<[f64; 3]>,
}

/// Rule names of the display stage black point map.
pub const DISPLAY_BPC_RULE_BLACK_INK_SIMULATED: &str = "black_ink_simulated";

fn build_display_stage(
    proof: (&ColorProfile, &WideProfileDescriptor),
    display: (&ColorProfile, &WideProfileDescriptor),
    condition: &ProofCondition<'_>,
    cancel: &CancellationToken,
) -> Result<(DisplayStage, DisplayStageReport), WideError> {
    let (pp, pd) = proof;
    let (dp, dd) = display;
    let intent = if condition.simulate_paper_color {
        Intent::AbsoluteColorimetric
    } else {
        condition.display_intent
    };
    let to_pcs = icc_eval::to_pcs(pp, pd, intent)?;
    let from_pcs = icc_eval::from_pcs(dp, dd, intent)?;
    canceled(cancel)?;
    let paper_white_scale = if intent == Intent::AbsoluteColorimetric {
        let src = pd.media_white.ok_or(WideError::MissingMediaWhite)?;
        let dst = dd.media_white.ok_or(WideError::MissingMediaWhite)?;
        if src.iter().chain(&dst).any(|v| !v.is_finite() || *v <= 0.0) {
            return Err(WideError::MissingMediaWhite);
        }
        Some([src[0] / dst[0], src[1] / dst[1], src[2] / dst[2]])
    } else {
        None
    };
    let (s, o, bpc) = if condition.simulate_black_ink {
        (
            [1.0; 3],
            [0.0; 3],
            BpcReport {
                rule: DISPLAY_BPC_RULE_BLACK_INK_SIMULATED,
                ..Default::default()
            },
        )
    } else {
        // Absolute colorimetric uses the colorimetric tables: the black point map is computed on them as relative.
        let bpc_intent = if intent == Intent::AbsoluteColorimetric {
            Intent::RelativeColorimetric
        } else {
            intent
        };
        let (map, report) = bpc_compute(
            &BpcInputs {
                source: Some((pp, pd)),
                destination: (dp, dd),
                intent: bpc_intent,
                to_pcs: Some(&to_pcs),
                from_pcs: &from_pcs,
            },
            cancel,
        )?;
        let (s, o) = map.affine();
        (s, o, report)
    };
    let k = paper_white_scale.unwrap_or([1.0; 3]);
    let stage = DisplayStage {
        to_pcs,
        from_pcs,
        scale: [k[0] * s[0], k[1] * s[1], k[2] * s[2]],
        offset: [k[0] * o[0], k[1] * o[1], k[2] * o[2]],
    };
    Ok((
        stage,
        DisplayStageReport {
            intent,
            bpc,
            paper_white_scale,
        },
    ))
}

/// The managed proof route (ICC, colour-vision and operator conditions).
pub(super) struct Managed {
    pub(super) to_proof: Plan,
    pub(super) proof_space: DeviceSpace,
    pub(super) simulation: Option<[[f64; 3]; 3]>,
    pub(super) display: DisplayStage,
    pub(super) display_report: DisplayStageReport,
    /// Document -> proof device for the gamut check (`gamut_intent`, condition BPC, unfused).
    pub(super) gamut: Plan,
    pub(super) gamut_intent: Intent,
    /// Proof device -> PCS through the colorimetric tables (round trip of table proof devices).
    pub(super) proof_to_pcs: ToPcs,
    /// Proof device -> document, relative colorimetric, no BPC (in-gamut substitute); refused for a document
    /// profile that cannot be a destination (the proof itself does not need it).
    pub(super) back: Result<Plan, WideError>,
    /// Document device -> PCS, relative colorimetric (delta E of the substitute).
    pub(super) doc_to_pcs: ToPcs,
}
pub(super) enum Route {
    Managed(Box<Managed>),
    /// Legacy uncompensated RGB: document RGB values shown unmanaged.
    Unmanaged,
}

/// A prepared soft proof: immutable, `Send + Sync`, shared by any number of threads.
pub struct SoftProof {
    pub(super) condition: ProofConditionRecord,
    pub(super) proof_profile_id: Option<DomainId>,
    pub(super) proof_profile_sha256: Option<[u8; 32]>,
    pub(super) document_id: DomainId,
    pub(super) document_sha256: [u8; 32],
    pub(super) document_space: DeviceSpace,
    pub(super) display_id: DomainId,
    pub(super) display_sha256: [u8; 32],
    pub(super) off: Plan,
    pub(super) route: Route,
}
impl std::fmt::Debug for SoftProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SoftProof")
            .field("condition", &self.condition)
            .field("document_space", &self.document_space)
            .finish_non_exhaustive()
    }
}

/// Every receipt of a proof run: both intents, both black point reports and the document, proof and display
/// identities (STU-COL-240 "inspectable").
#[derive(Clone, Debug, PartialEq)]
pub struct ProofReceipt {
    pub condition: ProofConditionRecord,
    /// False: the ordinary document -> display transform ran (proof toggled off).
    pub enabled: bool,
    pub document_profile_id: DomainId,
    pub document_sha256: [u8; 32],
    pub document_space: DeviceSpace,
    /// The admitted proof device profile: the ICC profile, or the linear-light sRGB device of a colour-vision or
    /// operator condition; `None` for legacy uncompensated RGB.
    pub proof_profile_id: Option<DomainId>,
    pub proof_profile_sha256: Option<[u8; 32]>,
    pub proof_space: Option<DeviceSpace>,
    pub display_profile_id: DomainId,
    pub display_sha256: [u8; 32],
    /// Document -> proof device black point compensation (the condition's flag).
    pub proof_bpc: BpcReport,
    pub proof_absolute: AbsoluteReport,
    /// Proof device -> display stage (`None` when unmanaged or toggled off).
    pub display_stage: Option<DisplayStageReport>,
    pub simulation_matrix: Option<[[f64; 3]; 3]>,
    /// Proof device clipping of matrix/Gray proof devices (linear values outside 0..1, tolerance 0.001).
    pub proof_clip: ClipReport,
    /// Legacy uncompensated RGB: the document values were shown unmanaged.
    pub unmanaged: bool,
    pub pixel_count: usize,
    pub width: usize,
    pub height: usize,
    pub engine_name: &'static str,
    pub engine_version: &'static str,
    /// SHA-256 of the display output as little-endian f32 bits.
    pub output_sha256: [u8; 32],
}

/// Display RGB preview (interleaved, unclamped like the wide path) and its receipt.
#[derive(Clone, Debug)]
pub struct ProofPreview {
    pub pixels: Vec<f32>,
    pub receipt: ProofReceipt,
}

/// Pixel buffer checks shared by every proof call: `(pixel count, height)`.
pub(super) fn check_pixels(pixels: &[f32], channels: usize, width: usize) -> Result<(usize, usize), ProofError> {
    if pixels.is_empty() || channels == 0 || !pixels.len().is_multiple_of(channels) {
        return Err(WideError::ChannelCount.into());
    }
    let count = pixels.len().checked_div(channels).ok_or(ProofError::Wide(WideError::ChannelCount))?;
    if count > MAX_WIDE_PIXELS {
        return Err(WideError::PixelLimit.into());
    }
    if width == 0 || !count.is_multiple_of(width) {
        return Err(ProofError::Geometry);
    }
    let height = count.checked_div(width).ok_or(ProofError::Geometry)?;
    for v in pixels {
        if !v.is_finite() {
            return Err(WideError::NonfiniteChannel.into());
        }
        if !(0.0..=1.0).contains(v) {
            return Err(WideError::ChannelRange.into());
        }
    }
    Ok((count, height))
}

/// Admit the three profiles and build the proof stages. The proof profile is admitted first, so hostile proof
/// bytes are refused (identity, size, SHA-256, then framing) before any other profile is parsed.
pub fn prepare_soft_proof(
    document: ProfileInput<'_>,
    condition: &ProofCondition<'_>,
    display: ProfileInput<'_>,
    cancel: &CancellationToken,
) -> Result<SoftProof, ProofError> {
    canceled(cancel)?;
    condition.validate()?;
    // Only the colour-vision routes own an id (an ICC proof is admitted before anything is allocated).
    let cvd_id = match condition.proof_profile {
        ProofProfile::ColourVision(_) | ProofProfile::OperatorSimulation(_) => Some(
            DomainId::parse(CVD_PROOF_DEVICE_PROFILE_ID).map_err(|_| ProofError::Wide(WideError::InvalidProfileId))?,
        ),
        _ => None,
    };
    fn cvd_input(id: &DomainId) -> ProfileInput<'_> {
        ProfileInput {
            profile_id: id,
            bytes: WORKING_PROFILE_LINEAR_SRGB,
            expected_sha256: profile_hash(WORKING_PROFILE_LINEAR_SRGB),
        }
    }
    let (proof_input, simulation) = match (condition.proof_profile, &cvd_id) {
        (ProofProfile::Icc(p), _) => (Some(p), None),
        (ProofProfile::ColourVision(kind), Some(id)) => (Some(cvd_input(id)), Some(kind.matrix())),
        (ProofProfile::OperatorSimulation(m), Some(id)) => (Some(cvd_input(id)), Some(m.matrix())),
        _ => (None, None),
    };
    let proof = match proof_input {
        Some(p) => Some(admit(p)?),
        None => None,
    };
    canceled(cancel)?;
    let (docp, docd) = admit(document)?;
    canceled(cancel)?;
    let (dispp, dispd) = admit(display)?;
    if dispd.device != DeviceSpace::Rgb {
        return Err(ProofError::DisplayNotRgb);
    }
    canceled(cancel)?;
    let off = crate::wide_plan::build(
        Some((&docp, &docd)),
        (&dispp, &dispd),
        condition.display_intent,
        true,
        true,
        cancel,
    )?;
    let route = match &proof {
        None => {
            if docd.device != DeviceSpace::Rgb {
                return Err(ProofError::LegacyNeedsRgbDocument);
            }
            Route::Unmanaged
        }
        Some((pp, pd)) => {
            let to_proof = crate::wide_plan::build(
                Some((&docp, &docd)),
                (pp, pd),
                condition.proof_intent,
                condition.black_point_compensation,
                false,
                cancel,
            )?;
            canceled(cancel)?;
            let (display_stage, display_report) = build_display_stage((pp, pd), (&dispp, &dispd), condition, cancel)?;
            canceled(cancel)?;
            // STU-COL-138: perceptual and saturation use the profile's own tables and never clip, so the gamut check
            // runs under relative colorimetric (Studio reading); the colorimetric intents check themselves.
            let gamut_intent = match condition.proof_intent {
                Intent::Perceptual | Intent::Saturation => Intent::RelativeColorimetric,
                other => other,
            };
            let gamut = crate::wide_plan::build(
                Some((&docp, &docd)),
                (pp, pd),
                gamut_intent,
                condition.black_point_compensation,
                false,
                cancel,
            )?;
            canceled(cancel)?;
            let back = crate::wide_plan::build(Some((pp, pd)), (&docp, &docd), Intent::RelativeColorimetric, false, false, cancel);
            if matches!(back, Err(WideError::Canceled)) {
                return Err(WideError::Canceled.into());
            }
            Route::Managed(Box::new(Managed {
                to_proof,
                proof_space: pd.device,
                simulation,
                display: display_stage,
                display_report,
                gamut,
                gamut_intent,
                proof_to_pcs: icc_eval::to_pcs(pp, pd, Intent::RelativeColorimetric)?,
                back,
                doc_to_pcs: icc_eval::to_pcs(&docp, &docd, Intent::RelativeColorimetric)?,
            }))
        }
    };
    canceled(cancel)?;
    Ok(SoftProof {
        condition: condition.record(),
        proof_profile_id: proof.as_ref().map(|(_, d)| d.profile_id.clone()),
        proof_profile_sha256: proof.as_ref().map(|(_, d)| d.sha256),
        document_id: docd.profile_id.clone(),
        document_sha256: docd.sha256,
        document_space: docd.device,
        display_id: dispd.profile_id.clone(),
        display_sha256: dispd.sha256,
        off,
        route,
    })
}

impl Managed {
    /// Document pixel -> proof device values (`out`, proof channel count), clamped to the device range, the
    /// simulation matrix applied when present; counts matrix/Gray proof clipping into `clip`.
    pub(super) fn proof_pixel(&self, px: &[f32], out: &mut [f32], clip: Option<&mut ClipReport>) {
        let channels = self.proof_space.channels();
        if let Some(lin) = self.to_proof.pixel(px, false, out) {
            if let Some(clip) = clip {
                Plan::scan_clip(lin, channels, clip);
            }
            let mut device = lin;
            for v in device.iter_mut().take(channels) {
                *v = v.clamp(0.0, 1.0);
            }
            if let Some(m) = &self.simulation {
                device = simulate(m, device);
            }
            if device != lin {
                self.to_proof.dest.encode_linear(device, out);
            }
        }
    }
}

impl SoftProof {
    pub fn condition(&self) -> &ProofConditionRecord {
        &self.condition
    }
    pub fn document_space(&self) -> DeviceSpace {
        self.document_space
    }
    /// The proof device space (`None` for legacy uncompensated RGB).
    pub fn proof_space(&self) -> Option<DeviceSpace> {
        match &self.route {
            Route::Managed(m) => Some(m.proof_space),
            Route::Unmanaged => None,
        }
    }

    /// Proof (or, `enabled == false`, plain display) preview of `pixels` (document device values, interleaved,
    /// 0..1, `width` pixels per row). The document buffer is only read.
    pub fn run(&self, pixels: &[f32], width: usize, enabled: bool, cancel: &CancellationToken) -> Result<ProofPreview, ProofError> {
        canceled(cancel)?;
        let channels = self.document_space.channels();
        let (count, height) = check_pixels(pixels, channels, width)?;
        let mut output = vec![0.0f32; count.checked_mul(3).ok_or(ProofError::Wide(WideError::PixelLimit))?];
        let mut clip = ClipReport {
            method: match &self.route {
                Route::Managed(m) if enabled => m.to_proof.clip_method(),
                _ => Default::default(),
            },
            ..Default::default()
        };
        let mut hasher = Sha256::new();
        let mut bytes = Vec::with_capacity(CHUNK_PIXELS.saturating_mul(12));
        let chunk_in = CHUNK_PIXELS.saturating_mul(channels);
        let chunk_out = CHUNK_PIXELS.saturating_mul(3);
        let mut proof_px = [0.0f32; 4];
        for (px_chunk, out_chunk) in pixels.chunks(chunk_in).zip(output.chunks_mut(chunk_out)) {
            canceled(cancel)?;
            for (px, out) in px_chunk.chunks_exact(channels).zip(out_chunk.chunks_exact_mut(3)) {
                match (&self.route, enabled) {
                    (_, false) => {
                        self.off.pixel(px, false, out);
                    }
                    (Route::Unmanaged, true) => out.copy_from_slice(px),
                    (Route::Managed(m), true) => {
                        let pch = m.proof_space.channels();
                        m.proof_pixel(px, &mut proof_px[..pch], Some(&mut clip));
                        m.display.eval(&proof_px[..pch], out);
                    }
                }
            }
            if out_chunk.iter().any(|v| !v.is_finite()) {
                return Err(WideError::NonfiniteOutput.into());
            }
            bytes.clear();
            for v in out_chunk.iter() {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            hasher.update(&bytes);
        }
        canceled(cancel)?;
        let (proof_bpc, proof_absolute, display_stage, simulation_matrix, proof_space) = match &self.route {
            Route::Managed(m) => (
                m.to_proof.bpc,
                m.to_proof.absolute,
                enabled.then_some(m.display_report),
                m.simulation,
                Some(m.proof_space),
            ),
            Route::Unmanaged => (BpcReport::default(), AbsoluteReport::default(), None, None, None),
        };
        Ok(ProofPreview {
            pixels: output,
            receipt: ProofReceipt {
                condition: self.condition.clone(),
                enabled,
                document_profile_id: self.document_id.clone(),
                document_sha256: self.document_sha256,
                document_space: self.document_space,
                proof_profile_id: self.proof_profile_id.clone(),
                proof_profile_sha256: self.proof_profile_sha256,
                proof_space,
                display_profile_id: self.display_id.clone(),
                display_sha256: self.display_sha256,
                proof_bpc,
                proof_absolute,
                display_stage,
                simulation_matrix,
                proof_clip: clip,
                unmanaged: enabled && matches!(self.route, Route::Unmanaged),
                pixel_count: count,
                width,
                height,
                engine_name: WIDE_ENGINE_NAME,
                engine_version: WIDE_ENGINE_VERSION,
                output_sha256: hasher.finalize().into(),
            },
        })
    }
}
