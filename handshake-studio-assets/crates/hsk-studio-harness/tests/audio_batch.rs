//! Lane SMB-AUDIO-IMPORT-2 (feature `audio-batch`): Audition-style batch processing. A queue of typed
//! jobs runs import (with the BS.775 downmix), a score effect chain (bounced with latency
//! compensation) and export (FLAC with MD5, AIFF), with determinate progress, cancel (before and
//! during an item) and one receipt per item; outputs are published only after reel re-decodes them.
//! Oracles: the samples written into test WAV files (`support_audio::wav`) and reel's decode of the
//! committed MP3 fixture; a -6.0206 dB gain is checked against 0.5 x the input.
#![cfg(feature = "audio-batch")]

mod support_audio;

use hsk_studio_accord::{ActorContext, CancellationToken};
use hsk_studio_harness::view::audio_batch::{
    AudioBatchJob, AudioBatchQueue, BatchOutcome, BatchState, EffectSpec,
};
use hsk_studio_harness::view::audio_import::{ImportLimits, ImportOptions, MixFormat};
use hsk_studio_reel as reel;
use hsk_studio_score as score;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use support_audio::{WavFormat, fixture, signal, wav};

fn actor() -> ActorContext {
    ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap()
}

fn job(input: &Path, output: PathBuf, chain: Vec<EffectSpec>, format: MixFormat) -> AudioBatchJob {
    AudioBatchJob {
        input: input.to_path_buf(),
        artifact: output.file_name().unwrap().to_string_lossy().into_owned(),
        output,
        overwrite: false,
        options: ImportOptions::default(),
        limits: ImportLimits::default(),
        chain,
        format,
    }
}

fn decode(path: &Path) -> reel::DecodedAudio {
    let bytes = std::fs::read(path).unwrap();
    reel::decode_audio_file(&mut Cursor::new(bytes), &reel::Limits::default(), 1 << 26, &CancellationToken::default()).unwrap()
}

fn leftovers(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".hsk-partial"))
        .collect()
}

#[test]
fn batch_runs_import_effect_chain_and_export_with_progress_and_receipts() {
    let dir = support_audio::scratch("batch-ok");
    let surround = dir.join("surround.wav");
    std::fs::write(&surround, wav(6, 48_000, WavFormat::Float32, Some(0x3F), &signal(9000, 6))).unwrap();
    let mono_in = signal(5000, 1);
    let mono = dir.join("mono.wav");
    std::fs::write(&mono, wav(1, 44_100, WavFormat::Int16, None, &mono_in)).unwrap();

    let mut queue = AudioBatchQueue::new(actor());
    let a = queue.submit(job(
        &fixture("mp3-44k1-2ch-cbr.mp3"),
        dir.join("a.flac"),
        vec![EffectSpec::Gain { db: -6.020_599_913_279_624 }],
        MixFormat::Flac { bits: 24 },
    ));
    let b = queue.submit(job(
        &surround,
        dir.join("b.aifc"),
        vec![
            EffectSpec::Equalizer {
                sections: vec![score::BiquadSpec { kind: score::BiquadKind::HighPass, frequency_hz: 80.0, gain_db: 0.0, q: 0.707 }],
                master_gain_db: 0.0,
            },
            EffectSpec::Dynamics(score::DynamicsParams::compressor(-20.0, 4.0)),
            EffectSpec::Limiter(score::LimiterParams::default()),
        ],
        MixFormat::AiffFloat,
    ));
    let c = queue.submit(job(&mono, dir.join("c.aiff"), Vec::new(), MixFormat::Aiff { bits: 16 }));
    assert_eq!(a.progress().state, BatchState::Queued);
    let receipts = queue.run();
    assert_eq!(receipts.len(), 3);
    for (r, h) in receipts.iter().zip([&a, &b, &c]) {
        assert_eq!(r.outcome, BatchOutcome::Complete, "{}", r.to_json());
        assert!(r.verified, "{}", r.to_json());
        let p = h.progress();
        assert_eq!(p.state, BatchState::Complete);
        assert!(p.total > 0 && p.done == p.total, "{p:?}");
        let json = r.to_json().to_string();
        assert!(!json.contains(dir.to_string_lossy().as_ref()), "receipts carry no paths: {json}");
        let out = dir.join(&r.artifact);
        let sha = reel::hex(&reel::full_digest(&mut Cursor::new(std::fs::read(&out).unwrap()), 1 << 20, &CancellationToken::default()).unwrap());
        assert_eq!(r.output_sha256.as_deref(), Some(sha.as_str()));
    }
    // a: -6.0206 dB is 0.5 x the decode (24-bit quantisation), FLAC MD5 verified.
    let input = decode(&fixture("mp3-44k1-2ch-cbr.mp3"));
    let out = decode(&dir.join("a.flac"));
    assert_eq!((out.frames(), out.channels, out.checksum_verified), (input.frames(), 2, Some(true)));
    for (i, (o, x)) in out.interleaved.iter().zip(&input.interleaved).enumerate() {
        assert!((o - 0.5 * x).abs() <= 1.0 / 8_388_608.0 + 1e-6, "sample {i}: {o} vs {}", 0.5 * x);
    }
    assert!(receipts[0].chain_identity.is_some() && receipts[0].bounce_digest.is_some());
    assert!(receipts[0].export.as_ref().unwrap().flac_md5.is_some());
    // b: 5.1 folded to stereo, three effects, latency compensated (same frame count).
    let out = decode(&dir.join("b.aifc"));
    assert_eq!((out.frames(), out.channels), (9000, 2));
    assert_eq!(receipts[1].chain.len(), 3);
    assert_eq!(receipts[1].import.as_ref().unwrap()["downmix"]["layout_source"], "decoder");
    assert!(out.interleaved.iter().all(|v| v.is_finite()) && out.interleaved.iter().any(|v| *v != 0.0));
    // c: no chain, mono 16-bit in and out: the samples come back exactly.
    let out = decode(&dir.join("c.aiff"));
    assert_eq!(out.channels, 1);
    assert_eq!(out.interleaved, mono_in);
    assert_eq!(receipts[2].chain_identity, None);
    assert!(leftovers(&dir).is_empty());
}

#[test]
fn batch_refuses_before_reading_and_cancels_without_publishing() {
    let dir = support_audio::scratch("batch-refuse");
    let input = dir.join("in.wav");
    std::fs::write(&input, wav(2, 48_000, WavFormat::Int16, None, &signal(2000, 2))).unwrap();
    let existing = dir.join("existing.flac");
    std::fs::write(&existing, b"keep me").unwrap();
    let garbage = dir.join("garbage.wav");
    std::fs::write(&garbage, b"RIFF\x10\x00\x00\x00WAVEdata\xff\xff").unwrap();
    let flac = MixFormat::Flac { bits: 16 };

    let mut queue = AudioBatchQueue::new(actor());
    queue.submit(job(&input, existing.clone(), Vec::new(), flac));
    queue.submit(job(&input, input.clone(), Vec::new(), flac));
    queue.submit(job(&input, dir.join("missing-dir").join("x.flac"), Vec::new(), flac));
    queue.submit(job(&input, dir.join("long.flac"), vec![EffectSpec::Gain { db: 0.0 }; score::MAX_CHAIN_STAGES + 1], flac));
    queue.submit(job(&input, dir.join("bits.flac"), Vec::new(), MixFormat::Flac { bits: 20 }));
    queue.submit(job(&garbage, dir.join("garbage.flac"), Vec::new(), flac));
    let early = queue.submit(job(&input, dir.join("early.flac"), Vec::new(), flac));
    early.cancel();
    let receipts = queue.run();
    let codes: Vec<&str> = receipts
        .iter()
        .map(|r| match &r.outcome {
            BatchOutcome::Failed { code, .. } => *code,
            BatchOutcome::Canceled => "canceled",
            BatchOutcome::Complete => "complete",
        })
        .collect();
    assert_eq!(
        codes,
        ["output_exists", "output_path", "output_path", "effect_chain", "export_format", "audio_decode", "canceled"]
    );
    assert_eq!(std::fs::read(&existing).unwrap(), b"keep me");
    assert_eq!(early.progress().state, BatchState::Canceled);
    for name in ["garbage.flac", "early.flac", "long.flac", "bits.flac"] {
        assert!(!dir.join(name).exists(), "{name}");
    }
    assert!(receipts.iter().all(|r| !r.verified && r.output_sha256.is_none()));

    // Cancel while running: a 60 s stereo file through a limiter on a worker thread, canceled as soon
    // as progress moves; nothing is published and the temp file is gone.
    let long = dir.join("long-in.wav");
    std::fs::write(&long, wav(2, 48_000, WavFormat::Int16, None, &signal(48_000 * 60, 2))).unwrap();
    let mut queue = AudioBatchQueue::new(actor());
    let handle = queue.submit(job(&long, dir.join("long-out.flac"), vec![EffectSpec::Limiter(score::LimiterParams::default())], flac));
    let worker = std::thread::spawn(move || queue.run());
    let started = std::time::Instant::now();
    while handle.progress().done == 0 && started.elapsed() < std::time::Duration::from_secs(60) {
        std::thread::yield_now();
    }
    let seen = handle.progress();
    handle.cancel();
    let receipts = worker.join().unwrap();
    assert_eq!(receipts[0].outcome, BatchOutcome::Canceled, "progress at cancel {seen:?}: {}", receipts[0].to_json());
    assert!(seen.done > 0 && seen.done < seen.total, "{seen:?}");
    assert_eq!(handle.progress().state, BatchState::Canceled);
    assert!(!dir.join("long-out.flac").exists());
    assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
}
