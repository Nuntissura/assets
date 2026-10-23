//! Shared helpers for the SMB-SCORE-DSP test files (`mod common_dsp;`).
#![allow(dead_code)]
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_score::*;

pub const RESOURCE: &str = "STRK-019abcde-0000-7000-8000-000000000001";
pub const RATE: u32 = 48_000;

pub fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 120s deadline: {e}"),
    }
}

pub fn ctx() -> RequestContext {
    RequestContext {
        resource: DomainId::parse(RESOURCE).unwrap(),
        actor: Some(
            ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session")
                .unwrap(),
        ),
        expected_revision: 1,
    }
}
pub fn engine() -> Engine {
    Engine::new(DomainId::parse(RESOURCE).unwrap(), 1)
}
pub fn mono() -> ChannelLayout {
    ChannelLayout::mono(7)
}

/// Streams planar `signal` (one `Vec` per channel) through `proc` in blocks of the given sizes
/// (repeating the last size until the signal is consumed) and returns planar output.
pub fn run_blocks(
    proc: &mut dyn Processor,
    layout: &ChannelLayout,
    rate: u32,
    signal: &[Vec<f32>],
    blocks: &[usize],
) -> Vec<Vec<f32>> {
    let frames = signal[0].len();
    let eng = engine();
    let token = CancellationToken::default();
    let mut result = vec![Vec::with_capacity(frames); signal.len()];
    let (mut at, mut i) = (0, 0);
    while at < frames {
        let len = blocks[i.min(blocks.len() - 1)].min(frames - at);
        i += 1;
        let sp = BlockSpec { sample_rate_hz: rate, layout: layout.clone(), frames: len, start_sample: at as u64 };
        let slices: Vec<&[f32]> = signal.iter().map(|c| &c[at..at + len]).collect();
        let mut out = OutputBlock::with_capacity(layout.clone(), len).unwrap();
        eng.process(&ctx(), &InputBlock::new(&sp, &slices), proc, &mut out, &token).unwrap();
        for (ch, dst) in result.iter_mut().enumerate() {
            dst.extend_from_slice(out.published_channel(ch).unwrap());
        }
        at += len;
    }
    result
}

/// Deterministic uniform noise in `-1..1` (xorshift64*).
pub fn noise(seed: u64, frames: usize) -> Vec<f32> {
    let mut s = seed | 1;
    (0..frames)
        .map(|_| {
            s ^= s >> 12;
            s ^= s << 25;
            s ^= s >> 27;
            let v = s.wrapping_mul(0x2545_F491_4F6C_DD1D);
            ((v >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0) as f32
        })
        .collect()
}
pub fn sine(hz: f64, amplitude: f64, phase: f64, rate: u32, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|n| (amplitude * (2.0 * std::f64::consts::PI * hz * n as f64 / f64::from(rate) + phase).sin()) as f32)
        .collect()
}
pub fn rms(s: &[f32]) -> f64 {
    (s.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / s.len() as f64).sqrt()
}
pub fn db(linear: f64) -> f64 {
    20.0 * linear.log10()
}
pub fn bits(s: &[f32]) -> Vec<u32> {
    s.iter().map(|v| v.to_bits()).collect()
}
