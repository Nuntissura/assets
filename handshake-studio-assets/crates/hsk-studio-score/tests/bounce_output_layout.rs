//! R3 regression (bounce.rs): the output side of a bounce (sink chunks, receipt labels, meters,
//! digest) follows the OUTPUT layout, not the input layout, when the chain changes the layout.

use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_score::*;
use std::cell::RefCell;

const RESOURCE: &str = "STRK-019abcde-0000-7000-8000-000000000001";
const DIGEST_DOMAIN: &[u8] = b"hsk-studio-score/bounce-digest@1";
const FRAMES: usize = 1000;
const START: u64 = 48_000;

fn ctx() -> RequestContext {
    RequestContext {
        resource: DomainId::parse(RESOURCE).unwrap(),
        actor: Some(ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap()),
        expected_revision: 7,
    }
}

/// The documented digest scheme, composed here from `sha256` alone (no bounce internals).
fn oracle_digest(channels: &[(u16, &[f32])]) -> [u8; 32] {
    let mut bytes = DIGEST_DOMAIN.to_vec();
    bytes.extend_from_slice(&(channels.len() as u16).to_le_bytes());
    for (label, samples) in channels {
        bytes.extend_from_slice(&label.to_le_bytes());
        bytes.extend_from_slice(&(samples.len() as u64).to_le_bytes());
        let raw: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        bytes.extend_from_slice(&sha256(&raw));
    }
    sha256(&bytes)
}

type Chunk = (u64, Vec<Vec<f32>>);

fn run(
    proc: &mut dyn Processor,
    input_layout: &ChannelLayout,
    output_layout: ChannelLayout,
    input: &[&[f32]],
    block_frames: usize,
) -> (Result<BounceReceipt, ScoreError>, Vec<Chunk>) {
    let engine = Engine::new(DomainId::parse(RESOURCE).unwrap(), 7);
    let request = BounceRequest {
        sample_rate_hz: 48_000,
        layout: input_layout,
        start_sample: START,
        channels: input,
        block_frames,
    };
    let mut out = OutputBlock::with_capacity(output_layout, 256).unwrap();
    let chunks = RefCell::new(Vec::new());
    let mut sink = |chunk: BounceChunk<'_>| {
        chunks.borrow_mut().push((chunk.start_sample, chunk.channels.iter().map(|c| c.to_vec()).collect()));
        Ok(())
    };
    let receipt = engine.bounce(&ctx(), &request, proc, &mut out, &mut sink, &CancellationToken::default());
    (receipt, chunks.into_inner())
}

/// Channel `ch` of every chunk joined; chunks must tile the clip contiguously from `START`.
fn joined(chunks: &[Chunk], planes: usize, ch: usize) -> Vec<f32> {
    let (mut next, mut samples) = (START, Vec::new());
    for (start, chunk) in chunks {
        assert_eq!(*start, next, "chunks tile the clip");
        assert_eq!(chunk.len(), planes, "one plane per OUTPUT channel");
        next += chunk[0].len() as u64;
        samples.extend_from_slice(&chunk[ch]);
    }
    assert_eq!(next, START + FRAMES as u64);
    samples
}

#[test]
fn stereo_to_mono_chain_bounces_the_mono_output() {
    let input_layout = ChannelLayout::stereo();
    let pattern = [0.0_f32, 0.25, -0.25, 0.5];
    let left: Vec<f32> = (0..FRAMES).map(|n| pattern[n % 4]).collect();
    let right = vec![0.25_f32; FRAMES];
    let input: [&[f32]; 2] = [&left, &right];
    let stage = ChainStage { processor: Box::new(ChannelMix::stereo_to_mono()), output: ChannelLayout::mono(200) };
    let mut chain = Chain::new(input_layout.clone(), 256, vec![stage]).unwrap();
    assert_eq!(chain.output_layout(), &ChannelLayout::mono(200));
    let output_layout = chain.output_layout().clone();

    // R3 finding: channel count and labels came from the input layout, so output channel 1 did not
    // exist (ChannelMismatch after the first block) and the receipt carried the input labels.
    let (receipt, chunks) = run(&mut chain, &input_layout, output_layout, &input, 100);
    let receipt = receipt.expect("a layout-changing chain bounces");
    assert_eq!((receipt.requested_frames, receipt.actual_frames, receipt.blocks), (FRAMES as u64, FRAMES as u64, 10));
    let mono: Vec<f32> = left.iter().zip(&right).map(|(l, r)| 0.5 * l + 0.5 * r).collect();
    let got = joined(&chunks, 1, 0);
    assert!(got.iter().zip(&mono).all(|(g, m)| g.to_bits() == m.to_bits()), "mono samples are exact");
    assert_eq!(receipt.labels(), &[200][..], "labels are the output layout's");
    let peak = mono.iter().fold(0.0_f32, |p, s| p.max(s.abs()));
    assert_eq!((receipt.peak(), receipt.rms().len()), (&[peak][..], 1));
    assert_eq!(receipt.digest, oracle_digest(&[(200, &mono)]), "digest covers the one output channel");
}

#[test]
fn mono_to_stereo_pan_keeps_both_output_channels() {
    let input_layout = ChannelLayout::mono(7);
    let pattern = [0.0_f32, 0.25, -0.25, 0.5];
    let source: Vec<f32> = (0..FRAMES).map(|n| pattern[n % 4]).collect();
    let input: [&[f32]; 1] = [&source];
    let mut pan = Pan::new(0.25).unwrap();
    let (gl, gr) = pan.gains();
    assert!(gl > 0.0 && gr > 0.0, "both sides carry signal");

    // R3 finding: only output channel 0 was digested, so the right channel was silently dropped
    // and the receipt labels were the input's single label.
    let (receipt, chunks) = run(&mut pan, &input_layout, ChannelLayout::stereo(), &input, 128);
    let receipt = receipt.expect("mono to stereo bounces");
    assert_eq!((receipt.requested_frames, receipt.actual_frames), (FRAMES as u64, FRAMES as u64));
    let left: Vec<f32> = source.iter().map(|s| s * gl).collect();
    let right: Vec<f32> = source.iter().map(|s| s * gr).collect();
    for (ch, want) in [(0, &left), (1, &right)] {
        let got = joined(&chunks, 2, ch);
        assert!(got.iter().zip(want).all(|(g, w)| g.to_bits() == w.to_bits()), "output channel {ch}");
    }
    assert_eq!(receipt.labels(), &STEREO_LABELS[..]);
    assert_eq!((receipt.peak().len(), receipt.rms().len()), (2, 2));
    assert!(receipt.peak().iter().all(|p| *p > 0.0), "{:?}", receipt.peak());
    assert_eq!(receipt.digest, oracle_digest(&[(100, &left), (101, &right)]), "digest covers both channels");

    // An output block the processor does not accept is still refused before any block runs.
    let (refused, delivered) = run(&mut pan, &input_layout, ChannelLayout::mono(9), &input, 128);
    assert_eq!(refused.unwrap_err(), ScoreError::ChannelMismatch);
    assert!(delivered.is_empty(), "sink must not be called");
}
