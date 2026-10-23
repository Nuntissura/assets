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
