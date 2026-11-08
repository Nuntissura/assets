//! Lane SMB-AU-WAVEFORM-EDITOR slice 4 (feature `audio-batch`): score effects applied to a waveform
//! selection, one undo step each, latency compensated and replayed after reopen. Oracles: the gain
//! formula `x * 10^(dB/20)`, an untouched quiet tail after a lookahead limiter (alignment), samples
//! outside the selection unchanged, and typed refusals with no step.
#![cfg(feature = "audio-batch")]

mod support_audio;

mod support_waveform;

use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::audio_batch::EffectSpec;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::waveform::{WaveformDocument, WaveformEdit};
use hsk_studio_score::{LimiterParams, WaveSelection};

const RATE: u32 = 48_000;

fn undo_len(v: &Viewer) -> usize {
    v.run().history()["undo"].as_array().unwrap().len()
}

#[test]
fn effects_on_a_selection_are_aligned_single_undo_steps() {
    let dir = support_audio::scratch("waveform-effects");
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&support_waveform::package(&dir)).unwrap();
    // Channel 0: 2400 samples of a full-scale 440 Hz sine, then 2400 of a 0.1 sine; channel 1: ramp.
    let mut src = support_waveform::source();
    for (n, s) in src[0].iter_mut().enumerate() {
        let a = if n < 2400 { 1.0 } else { 0.1 };
        *s = (a * (2.0 * std::f64::consts::PI * 440.0 * n as f64 / f64::from(RATE)).sin()) as f32;
    }
    v.open_waveform(RATE, src.clone()).unwrap();
    let base = undo_len(&v);

    // Gain -6.0206 dB on channel 1 samples 100 .. 900: x * 10^(dB/20) inside, untouched outside.
    let db = 20.0 * 0.5f64.log10();
    let gain = EffectSpec::Gain { db }.to_preset_instance("fx-0");
    v.waveform_edit(WaveformEdit::Effect { selection: WaveSelection { start: 100, end: 900, channels: 0b10 }, effect: gain }).unwrap();
    assert_eq!(undo_len(&v), base + 1);
    let b = v.waveform_buffer().unwrap();
    let lin = 10f64.powf(db / 20.0);
    for (n, (got, x)) in b.channels()[1].iter().zip(&src[1]).enumerate() {
        let want = if (100..900).contains(&n) { f64::from(*x) * lin } else { f64::from(*x) };
        assert!((f64::from(*got) - want).abs() < 1e-6, "n {n}");
    }
    assert_eq!(b.channels()[0], src[0]);

    // Lookahead limiter at -6 dB over all of channel 0: the loud half is held under the ceiling, the
    // quiet tail (well under the ceiling, after the release) comes out aligned and unchanged.
    let limiter = LimiterParams { ceiling_db: -6.0, lookahead_ms: 5.0, release_ms: 1.0, true_peak: false };
    let lim = EffectSpec::Limiter(limiter).to_preset_instance("fx-1");
    v.waveform_edit(WaveformEdit::Effect { selection: WaveSelection { start: 0, end: 4800, channels: 0b01 }, effect: lim }).unwrap();
    let b = v.waveform_buffer().unwrap();
    let ceiling = 10f64.powf(-6.0 / 20.0);
    assert!(b.channels()[0][..2400].iter().all(|s| f64::from(s.abs()) <= ceiling + 1e-6));
    for (n, (got, x)) in b.channels()[0].iter().zip(&src[0]).enumerate().skip(4000) {
        assert!((got - x).abs() < 1e-6, "tail n {n}: {got} vs {x}");
    }
    assert_eq!(undo_len(&v), base + 2);

    // Refusals: keyframes, an unknown kind, more than two channels in the selection; no step.
    let mut keyed = EffectSpec::Gain { db: -1.0 }.to_preset_instance("fx-2");
    keyed.keyframes.push(hsk_studio_folio::StudioAudioPresetKeyframe { key: "db".into(), offset_ticks: 0, value: -2.0 });
    let mut unknown = EffectSpec::Gain { db: -1.0 }.to_preset_instance("fx-3");
    unknown.filter_kind = "flux_capacitor".into();
    for effect in [keyed, unknown] {
        assert!(v.waveform_edit(WaveformEdit::Effect { selection: WaveSelection::all(0, 10), effect }).is_err());
    }
    assert_eq!(undo_len(&v), base + 2);

    // Undo the limiter; save, reopen: the gain edit replays.
    v.waveform_undo(true).unwrap();
    let after_gain = v.waveform_buffer().unwrap();
    let saved = dir.join("saved.handshake");
    v.save(&saved, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&saved).unwrap();
    w.open_waveform(RATE, src).unwrap();
    assert_eq!(w.waveform_buffer().unwrap(), after_gain);
    let doc = WaveformDocument::parse(w.run().document_entry("waveform_edit").unwrap()).unwrap();
    assert_eq!(doc.ops.len(), 1);
}

/// Lane SMB-SCORE-EFFECTS-6 item 6: noise reduction on a waveform selection from a stored noise
/// print (range plus profile digest), one undo step, replayed after reopen; a stored digest that
/// differs on replay recomputes the print and is recorded (review F3), another source refuses;
/// hostile parameters refuse typed with no step. Oracle: a tone-plus-noise fixture whose
/// noise-only spans and tone are measured before and after.
#[test]
fn noise_reduction_from_a_stored_print_is_one_step_and_replays() {
    use hsk_studio_harness::view::waveform::{NoiseReduceParams, PluginRegistry, WaveformOp, replay};
    use hsk_studio_harness::{Harness, References, Selection, asset_local_test_context};
    use hsk_studio_score::{ALL_CHANNELS, NoiseReductionParams, WaveBuffer};
    let dir = support_audio::scratch("waveform-noise");
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&support_waveform::package(&dir)).unwrap();
    // 1 s stereo: noise 0.05 everywhere, a 1 kHz tone of 0.5 over 12000 .. 40000.
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    let mut noise = || {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        ((s.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0) * 0.05
    };
    let src: Vec<Vec<f32>> = (0..2)
        .map(|_| {
            (0..48_000)
                .map(|n| {
                    let tone = if (12_000..40_000).contains(&n) { 0.5 * (2.0 * std::f64::consts::PI * 1000.0 * n as f64 / f64::from(RATE)).sin() } else { 0.0 };
                    (tone + noise()) as f32
                })
                .collect()
        })
        .collect();
    v.open_waveform(RATE, src.clone()).unwrap();
    let base = undo_len(&v);
    let reduction = NoiseReductionParams::default();
    let params = NoiseReduceParams { fft_size: 1024, reduction };
    let all = |a: u64, b: u64| WaveSelection { start: a, end: b, channels: ALL_CHANNELS };
    let edit = WaveformEdit::NoiseReduce { selection: all(0, 48_000), print: all(0, 12_000), params };
    v.waveform_edit(edit.clone()).unwrap();
    assert_eq!(undo_len(&v), base + 1, "one step");
    let b = v.waveform_buffer().unwrap();
    let rms = |x: &[f32]| (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64).sqrt();
    let db = |x: f64| 20.0 * x.log10();
    // Noise-only span after the tone (42000 .. 48000): the floor drops by about reduction_db.
    let drop = db(rms(&src[0][42_000..48_000])) - db(rms(&b.channels()[0][42_000..48_000]));
    // The tone band (1 kHz projection over 20000 .. 36000, 16 whole periods per 768 samples).
    let tone = |x: &[f32]| {
        let (mut c, mut q) = (0.0, 0.0);
        for (i, v) in x[20_000..36_000].iter().enumerate() {
            let a = 2.0 * std::f64::consts::PI * 1000.0 * (20_000 + i) as f64 / f64::from(RATE);
            c += f64::from(*v) * a.cos();
            q += f64::from(*v) * a.sin();
        }
        (c * c + q * q).sqrt() * 2.0 / 16_000.0
    };
    let moved = db(tone(&b.channels()[0])) - db(tone(&src[0]));
    println!("MEASURED: noise floor drop {drop:.2} dB (reduction_db {}), tone change {moved:.3} dB", reduction.reduction_db);
    assert!(drop >= reduction.reduction_db - 6.0, "noise floor dropped {drop} dB");
    assert!(moved.abs() <= 1.0, "tone moved {moved} dB");
    let stored = WaveformDocument::parse(v.run().document_entry("waveform_edit").unwrap()).unwrap();
    let WaveformOp::NoiseReduce { print, print_digest, .. } = &stored.ops[0] else { panic!("{:?}", stored.ops[0]) };
    assert_eq!((print.start, print.end, print_digest.len()), (0, 12_000, 64));

    // Undo restores the bytes; redo and save, reopen: the print replays to the same digest.
    v.waveform_undo(true).unwrap();
    assert_eq!(v.waveform_buffer().unwrap().channels(), &src[..]);
    v.waveform_undo(false).unwrap();
    let saved = dir.join("noise.handshake");
    v.save(&saved, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&saved).unwrap();
    w.open_waveform(RATE, src.clone()).unwrap();
    assert_eq!(w.waveform_buffer().unwrap(), b);

    // Review F3 (lead ruling 2026-11-08): a digest that differs on replay is a cache miss, not a
    // refusal. The print is recomputed from the stored range and applied; the viewer records it.
    let actor = hsk_studio_accord::ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let source = WaveBuffer::new(RATE, src.clone()).unwrap();
    let plugins = PluginRegistry::default();
    let token = hsk_studio_accord::CancellationToken::default();
    let louder = vec![WaveformOp::Amplify { selection: all(0, 6000), db: 3.0 }, stored.ops[0].clone()];
    assert!(replay(&source, &louder, &plugins, &actor, &token).is_ok(), "an earlier edit under the print recomputes it");
    let digest = print_digest.clone();
    let mut tampered = stored.clone();
    if let WaveformOp::NoiseReduce { print_digest, .. } = &mut tampered.ops[0] {
        *print_digest = "0".repeat(64);
    }
    assert_eq!(replay(&source, &tampered.ops, &plugins, &actor, &token).unwrap(), b, "the recomputed print gives the same audio");
    // Through the document: a package whose stored digest differs reopens, replays and records
    // `waveform_noise_print_recomputed` (op record with one observe frame) with both digests.
    let h = Harness::new(Selection { providers: Vec::new(), references: References::TrustAll }).unwrap();
    let forced = dir.join("forced.handshake");
    {
        let mut run = h.run(asset_local_test_context(), &token);
        assert!(run.open_path(&saved));
        run.set_document_entry("waveform_edit", Some(tampered.to_bytes())).unwrap();
        assert!(run.save_to(&forced));
    }
    let mut x = Viewer::new(Prefer::Cpu);
    x.open(&forced).unwrap();
    x.open_waveform(RATE, src.clone()).unwrap();
    assert_eq!(x.waveform_buffer().unwrap(), b);
    let record = x.run().ops().iter().find(|o| o["op"] == "waveform_noise_print_recomputed").cloned().unwrap_or_else(|| panic!("no record: {:?}", x.run().ops()));
    assert_eq!(record["ok"], true, "{record}");
    assert_eq!(record["observe"]["frames"], 1, "{record}");
    assert_eq!(record["detail"]["stored_digest"], "0".repeat(64), "{record}");
    assert_eq!(record["detail"]["recomputed_digest"], digest, "{record}");
    assert_eq!(x.waveform_json()["noise_print_recomputed"][0]["op"], 0);
    // Another source refuses typed.
    let mut other = src.clone();
    other[0][0] += 0.25;
    let mut y = Viewer::new(Prefer::Cpu);
    y.open(&forced).unwrap();
    assert!(y.open_waveform(RATE, other).unwrap_err().contains("waveform_source_mismatch"));

    // Hostile parameters and prints refuse typed with no step.
    let steps = undo_len(&v);
    let refused = |v: &mut Viewer, selection: WaveSelection, print: WaveSelection, params: NoiseReduceParams| v.waveform_edit(WaveformEdit::NoiseReduce { selection, print, params }).unwrap_err();
    assert!(refused(&mut v, all(0, 48_000), all(0, 500), params).contains("waveform_noise_print_short"));
    assert!(refused(&mut v, all(0, 48_000), all(0, 60_000), params).starts_with("waveform_"));
    assert!(refused(&mut v, all(0, 48_000), WaveSelection { start: 0, end: 12_000, channels: 0b01 }, params).contains("waveform_noise_print_channels"));
    for fft_size in [1000, 128, 16_384] {
        assert!(refused(&mut v, all(0, 48_000), all(0, 12_000), NoiseReduceParams { fft_size, reduction }).starts_with("waveform_"), "fft {fft_size}");
    }
    let loud = NoiseReduceParams { fft_size: 1024, reduction: NoiseReductionParams { reduction_db: 100.0, ..reduction } };
    assert!(refused(&mut v, all(0, 48_000), all(0, 12_000), loud).starts_with("waveform_"));
    assert_eq!(undo_len(&v), steps);
    // The stored form refuses a hostile FFT size and digest on parse.
    let mut json = stored.ops[0].to_json();
    json["fft_size"] = serde_json::json!(1000);
    assert!(WaveformOp::from_json(&json, 0).unwrap_err().contains("fft_size"));
    let mut json = stored.ops[0].to_json();
    json["print_digest"] = serde_json::json!("XYZ");
    assert!(WaveformOp::from_json(&json, 0).unwrap_err().contains("print_digest"));
    assert_eq!(WaveformOp::from_json(&stored.ops[0].to_json(), 0).unwrap(), stored.ops[0]);
}

/// Review F2: `waveform.noise_reduce` is a typed drive row (`Viewer::run_command`): one undo step
/// named `waveform_noise_reduce`, the engine's refusal code for a bad FFT size, a missing print a
/// typed `parameter_missing`, nothing changed by a refusal.
#[test]
fn noise_reduce_runs_as_a_typed_drive_row() {
    use serde_json::{Map, Value, json};
    let dir = support_audio::scratch("waveform-noise-row");
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&support_waveform::package(&dir)).unwrap();
    v.open_waveform(RATE, support_waveform::source()).unwrap();
    let params = |value: Value| -> Map<String, Value> { value.as_object().unwrap().clone() };
    let base = undo_len(&v);
    let reply = v
        .run_command("waveform.noise_reduce", &params(json!({"start": 0, "end": 4800, "print_start": 0, "print_end": 1200, "fft_size": 256, "reduction_db": 12.0})))
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(reply["result"]["op"], "noise_reduce", "{reply}");
    assert_eq!(undo_len(&v), base + 1);
    let refused = v.run_command("waveform.noise_reduce", &params(json!({"start": 0, "end": 4800, "print_start": 0, "print_end": 1200, "fft_size": 1000}))).unwrap_err();
    assert_eq!(refused.code, "waveform_out_of_range", "{refused:?}");
    let missing = v.run_command("waveform.noise_reduce", &params(json!({"start": 0, "end": 4800, "print_end": 1200, "fft_size": 256}))).unwrap_err();
    assert_eq!(missing.code, "parameter_missing", "{missing:?}");
    assert_eq!(undo_len(&v), base + 1);
}
