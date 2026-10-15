//! Loudness normalisation to an ITU-R BS.1770 integrated target with a true-peak ceiling (lane
//! SMB-AUDITION-SESSION; the Audition "match loudness" command shape, Adobe parity UNVERIFIED).
//!
//! Measurement is score's own meter ([`LoudnessMeter`]: BS.1770-4 K-weighting, gating, 4x true
//! peak); nothing is re-implemented here. The command:
//!
//! 1. measures the input: integrated loudness `I` (LUFS) and the largest per-channel true peak `P`
//!    (dBTP); a measurement without a gated block (silence, or under 400 ms) is `OutOfRange`;
//! 2. computes the gain `g = target - I` (dB);
//! 3. if `P + g` stays at or under the ceiling, applies `g` and stops (no limiting);
//! 4. otherwise, per [`PeakPolicy`]: `ReduceGain` applies `g = ceiling - P` instead, so the output
//!    peak sits on the ceiling and the output is quieter than the target (reported); `Limit`
//!    applies `g` and runs score's [`LookaheadLimiter`] with `true_peak` at the ceiling (latency
//!    compensated: the input is followed by `latency` zeros and the first `latency` output frames
//!    are dropped, so the output is aligned and as long as the input), then re-measures and
//!    corrects `g` by the remaining error, at most [`MAX_NORMALIZE_PASSES`] passes, until the
//!    output is within [`NORMALIZE_TOLERANCE_LU`] of the target.
//!
//! The receipt reports both measurements, the applied gain and whether the limiter ran. The target
//! range is authored ([`NORMALIZE_TARGET_LUFS`]); which broadcast standard selects a target is the
//! declared gap STU-FX-147 and is the caller's value, not an enumeration invented here.
use crate::block::{InputBlock, OutputBlock};
use crate::dsp::db_to_linear;
use crate::dynamics::{LimiterParams, LookaheadLimiter};
use crate::engine::Processor;
use crate::error::ScoreError;
use crate::layout::{BlockSpec, ChannelLayout, MAX_CHANNELS, ticks_per_sample};
use crate::loudness::LoudnessMeter;
use hsk_studio_accord::CancellationToken;

/// Accepted integrated targets in LUFS (authored: the absolute gate up to full scale).
pub const NORMALIZE_TARGET_LUFS: (f64, f64) = (-70.0, 0.0);
/// Output integrated loudness is accepted within this many LU of the target when limiting.
pub const NORMALIZE_TOLERANCE_LU: f64 = 0.05;
/// Most limiter passes.
pub const MAX_NORMALIZE_PASSES: u32 = 6;
const BLOCK: usize = 4096;

/// What to do when the gain would push the true peak over the ceiling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PeakPolicy {
    /// Lower the gain until the peak sits on the ceiling (the output misses the target).
    ReduceGain,
    /// Keep the target and limit the peaks (score's true-peak lookahead limiter).
    Limit { lookahead_ms: f64, release_ms: f64 },
}

/// One normalisation request over planar audio.
#[derive(Clone, Copy)]
pub struct NormalizeRequest<'a> {
    pub sample_rate_hz: u32,
    pub channels: &'a [&'a [f32]],
    /// BS.1770 channel weights, one per channel (see [`LoudnessMeter::new`]).
    pub weights: &'a [f64],
    pub target_lufs: f64,
    pub ceiling_dbtp: f64,
    pub policy: PeakPolicy,
}

/// Exact accounting of one normalisation.
#[derive(Clone, Debug, PartialEq)]
pub struct NormalizeReceipt {
    pub input_integrated_lufs: f64,
    pub input_true_peak_dbtp: f64,
    /// Gain applied before the limiter, in dB.
    pub gain_db: f64,
    pub output_integrated_lufs: f64,
    pub output_true_peak_dbtp: f64,
    pub limited: bool,
    /// `ReduceGain` lowered the gain below `target - input`.
    pub gain_reduced: bool,
    pub passes: u32,
    pub limiter_latency_frames: u32,
}

/// (integrated LUFS, largest true peak dBTP) of planar audio.
pub fn measure_loudness(
    sample_rate_hz: u32,
    channels: &[&[f32]],
    weights: &[f64],
    cancel: &CancellationToken,
) -> Result<(f64, f64), ScoreError> {
    let mut meter = LoudnessMeter::new(sample_rate_hz, weights)?;
    let frames = channels.first().map_or(0, |c| c.len());
    let mut at = 0;
    while at < frames {
        cancel.check().map_err(|_| ScoreError::Canceled)?;
        let end = (at + BLOCK).min(frames);
        let block: Vec<&[f32]> = channels.iter().map(|c| &c[at..end]).collect();
        meter.feed(&block)?;
        at = end;
    }
    let s = meter.snapshot();
    let integrated = s.integrated_lufs.filter(|v| v.is_finite()).ok_or(ScoreError::OutOfRange)?;
    let peak = s.true_peak_dbtp.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Ok((integrated, peak))
}

fn apply_gain(channels: &[&[f32]], gain_db: f64) -> Vec<Vec<f32>> {
    let g = db_to_linear(gain_db);
    channels.iter().map(|c| c.iter().map(|v| (f64::from(*v) * g) as f32).collect()).collect()
}

/// Runs `limiter` over `input` with latency compensation (output aligned, same length).
fn limit(
    rate: u32,
    input: &[Vec<f32>],
    limiter: &mut LookaheadLimiter,
    cancel: &CancellationToken,
) -> Result<Vec<Vec<f32>>, ScoreError> {
    let channels = input.len();
    let labels: Vec<u16> = (0..channels as u16).collect();
    let layout = ChannelLayout::new(&labels)?;
    let latency = limiter.latency_samples() as usize;
    let frames = input[0].len();
    let total = frames + latency;
    let mut out = vec![Vec::with_capacity(frames); channels];
    let mut block = OutputBlock::with_capacity(layout.clone(), BLOCK)?;
    let mut scratch = vec![vec![0.0f32; BLOCK]; channels];
    let mut at = 0usize;
    while at < total {
        cancel.check().map_err(|_| ScoreError::Canceled)?;
        let n = BLOCK.min(total - at);
        for (c, s) in scratch.iter_mut().enumerate() {
            for (k, v) in s[..n].iter_mut().enumerate() {
                *v = input[c].get(at + k).copied().unwrap_or(0.0);
            }
        }
        let spec = BlockSpec { sample_rate_hz: rate, layout: layout.clone(), frames: n, start_sample: at as u64 };
        let slices: Vec<&[f32]> = scratch.iter().map(|s| &s[..n]).collect();
        limiter.process(&InputBlock::new(&spec, &slices), &mut block, cancel)?;
        let cap = block.capacity();
        for (c, o) in out.iter_mut().enumerate() {
            let produced = &block.backing()[c * cap..c * cap + n];
            for (k, v) in produced.iter().enumerate() {
                let t = at + k;
                if t >= latency && t - latency < frames {
                    o.push(*v);
                }
            }
        }
        at += n;
    }
    Ok(out)
}

/// Normalises `request.channels` (module docs). Refusals before any output: rate not admitted ->
/// `InvalidRate`; no channel, more than `MAX_CHANNELS`, a weight count that differs -> `ChannelMismatch`;
/// unequal lengths -> `LengthMismatch`; a non-finite sample, target or ceiling -> `NotFinite`; target
/// outside [`NORMALIZE_TARGET_LUFS`], ceiling outside the limiter's range, or an input with no gated
/// block -> `OutOfRange`; cancel -> `Canceled`.
pub fn loudness_normalize(
    request: &NormalizeRequest<'_>,
    cancel: &CancellationToken,
) -> Result<(Vec<Vec<f32>>, NormalizeReceipt), ScoreError> {
    ticks_per_sample(request.sample_rate_hz)?;
    let channels = request.channels;
    if channels.is_empty() || channels.len() > MAX_CHANNELS || request.weights.len() != channels.len() {
        return Err(ScoreError::ChannelMismatch);
    }
    if channels.iter().any(|c| c.len() != channels[0].len()) {
        return Err(ScoreError::LengthMismatch);
    }
    if !request.target_lufs.is_finite() || !request.ceiling_dbtp.is_finite() {
        return Err(ScoreError::NotFinite);
    }
    let (lo, hi) = NORMALIZE_TARGET_LUFS;
    if request.target_lufs < lo || request.target_lufs > hi {
        return Err(ScoreError::OutOfRange);
    }
    let limiter_params = |lookahead_ms: f64, release_ms: f64| LimiterParams {
        ceiling_db: request.ceiling_dbtp,
        lookahead_ms,
        release_ms,
        true_peak: true,
    };
    // The ceiling is validated against the limiter's range for both policies.
    limiter_params(LimiterParams::default().lookahead_ms, LimiterParams::default().release_ms).validate()?;
    let rate = request.sample_rate_hz;
    let (input_i, input_tp) = measure_loudness(rate, channels, request.weights, cancel)?;
    let wanted = request.target_lufs - input_i;
    let mut receipt = NormalizeReceipt {
        input_integrated_lufs: input_i,
        input_true_peak_dbtp: input_tp,
        gain_db: wanted,
        output_integrated_lufs: input_i,
        output_true_peak_dbtp: input_tp,
        limited: false,
        gain_reduced: false,
        passes: 1,
        limiter_latency_frames: 0,
    };
    let fits = input_tp + wanted <= request.ceiling_dbtp;
    let output = match (fits, request.policy) {
        (true, _) => apply_gain(channels, wanted),
        (false, PeakPolicy::ReduceGain) => {
            receipt.gain_db = request.ceiling_dbtp - input_tp;
            receipt.gain_reduced = true;
            apply_gain(channels, receipt.gain_db)
        }
        (false, PeakPolicy::Limit { lookahead_ms, release_ms }) => {
            let params = limiter_params(lookahead_ms, release_ms);
            params.validate()?;
            receipt.limited = true;
            let mut gain = wanted;
            let mut passes = 0;
            loop {
                passes += 1;
                let mut limiter = LookaheadLimiter::new(rate, channels.len(), params)?;
                receipt.limiter_latency_frames = limiter.latency_samples();
                let out = limit(rate, &apply_gain(channels, gain), &mut limiter, cancel)?;
                let views: Vec<&[f32]> = out.iter().map(Vec::as_slice).collect();
                let (i, _) = measure_loudness(rate, &views, request.weights, cancel)?;
                let error = request.target_lufs - i;
                if error.abs() <= NORMALIZE_TOLERANCE_LU || passes >= MAX_NORMALIZE_PASSES {
                    receipt.gain_db = gain;
                    receipt.passes = passes;
                    break out;
                }
                gain += error;
            }
        }
    };
    let views: Vec<&[f32]> = output.iter().map(Vec::as_slice).collect();
    if views.iter().any(|c| c.iter().any(|v| !v.is_finite())) {
        return Err(ScoreError::NotFinite);
    }
    let (out_i, out_tp) = measure_loudness(rate, &views, request.weights, cancel)?;
    receipt.output_integrated_lufs = out_i;
    receipt.output_true_peak_dbtp = out_tp;
    Ok((output, receipt))
}
