#![cfg(feature = "audio-batch")]
//! Lane SMB-SCORE-EFFECTS-6 item 5 (feature `audio-batch`): every row of score's filter-kind table
//! reaches documents through the production paths: a bus and the master effect stack
//! (`MixerEdit::AddEffect`, one undo step, undo restores the mix), stored presets, keyframed bus
//! automation, batch jobs, waveform effects, save and reopen, and stems. Oracles: the bypass mix
//! of the same document (the mix differs with the effect and equals it again after undo), score's
//! own table (`score::audio_filter_defaults`) and the stem sum check (a linear master effect sums,
//! a non-linear one shows in the residual).

mod support_audio;
mod support_waveform;

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::audio_batch::{
    AudioBatchJob, AudioBatchQueue, BatchOutcome, EffectSpec, chain_from_preset, load_preset_set, preset_from_chain, save_preset_set,
};
use hsk_studio_harness::view::audio_import::{ImportLimits, ImportOptions, MixFormat};
use hsk_studio_harness::view::mixer::{MixerEdit, effect_templates};
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::waveform::WaveformEdit;
use hsk_studio_pulse::{AudioSources, SourceExtents};
use hsk_studio_score::{self as score, WaveSelection};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const F: u64 = 10_160_640_000;
const SPF: usize = 1920;
const RATE: u32 = 48_000;
const TICKS_PER_SAMPLE: u64 = 5_292_000;
const FRAMES: usize = 9600;

fn id(n: u32) -> String {
    format!("STRK-019abcde-0000-7000-8000-{n:012x}")
}
fn clip(n: u32, src: &str, frames: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: format!("SCLP-019abcde-0000-7000-8000-{n:012x}"),
        source_ref: ClipSource::Media { artifact_manifest_id: src.into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() }, asset_id: None },
        source_in: Ticks::new(0),
        source_out: Ticks::new(frames * F),
        timeline_start: Ticks::new(0),
        duration: Ticks::new(frames * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
        retime: Default::default(),
    }
}
fn track(n: u32, kind: TrackKind, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: id(n),
        kind,
        index: if kind == TrackKind::Submix { 0 } else { n - 1 },
        name: format!("T{n}"),
        enabled: true,
        locked: false,
        sync_locked: true,
        targeted: true,
        muted: false,
        soloed: false,
        clips,
        volume: 1.0,
        pan: 0.0,
    }
}
/// Track 1 (stereo "src") to the submix track 3; track 2 (mono "two") straight to the master.
fn sequence() -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(10 * F),
        tracks: vec![track(1, TrackKind::Audio, vec![clip(1, "src", 10)]), track(2, TrackKind::Audio, vec![clip(2, "two", 10)]), track(3, TrackKind::Submix, vec![])],
        markers: Vec::new(),
        work_area: None,
    }
}

fn xorshift(seed: u64, n: usize, scale: f64) -> Vec<f64> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s ^= s >> 12;
            s ^= s << 25;
            s ^= s >> 27;
            ((s.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0) * scale
        })
        .collect()
}

/// "src": a 1 kHz tone of 0.3, noise 0.01 and a 0.5 click every 997 samples (so the click repair,
/// hum notches, dynamics and modulation all act); "two": mono noise 0.1.
#[derive(Clone)]
struct Sources(Arc<Vec<(String, Vec<Vec<f32>>)>>);
fn sources() -> Sources {
    let n = 12 * SPF;
    let channel = |seed: u64| -> Vec<f32> {
        let noise = xorshift(seed, n, 0.01);
        (0..n)
            .map(|k| {
                let tone = 0.3 * (2.0 * std::f64::consts::PI * 1000.0 * k as f64 / 48_000.0).sin();
                let click = if k % 997 == 500 { 0.5 } else { 0.0 };
                (tone + noise[k] + click) as f32
            })
            .collect()
    };
    let two: Vec<f32> = xorshift(77, n, 0.1).into_iter().map(|v| v as f32).collect();
    Sources(Arc::new(vec![("src".into(), vec![channel(3), channel(4)]), ("two".into(), vec![two])]))
}
impl AudioSources for Sources {
    fn audio(&self, s: &ClipSource) -> Option<&[Vec<f32>]> {
        let ClipSource::Media { artifact_manifest_id: id, .. } = s else { return None };
        self.0.iter().find(|(n, _)| n == id).map(|(_, v)| v.as_slice())
    }
}
struct Ext;
impl SourceExtents for Ext {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(12 * F))
    }
}

fn package(dir: &Path) -> PathBuf {
    let doc = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000001","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[serde_json::to_value(sequence()).unwrap()]
    });
    struct Any;
    impl folio::Resolver for Any {
        fn primitive(&self, _: &str, _: &str) -> Resolution {
            Resolution::Available
        }
        fn profile(&self, _: &str) -> Resolution {
            Resolution::Available
        }
        fn tile(&self, _: &TileRef) -> Resolution {
            Resolution::Available
        }
        fn format_supported(&self, _: &str) -> bool {
            true
        }
        fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
            false
        }
        fn media(&self, _: &str, _: &ContentDigest) -> Resolution {
            Resolution::Available
        }
    }
    let token = CancellationToken::default();
    let snapshot = match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), Budget::default(), &token, &Any).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read-only fixture"),
    };
    let assets = hsk_studio_package::MemoryAssetSource::new();
    let mut bytes = Vec::new();
    hsk_studio_package::write_package_with(
        &snapshot,
        &assets as &dyn hsk_studio_package::AssetSource,
        &hsk_studio_package::Extras { quarantined: &[], previews: &[] },
        &hsk_studio_package::WriteOptions::default(),
        &hsk_studio_package::Limits::default(),
        &token,
        &mut bytes,
    )
    .unwrap();
    let path = dir.join("kinds.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

/// A viewer on a fresh session with the mixer open and track 1 routed to the submix.
fn session(label: &str) -> (PathBuf, Viewer) {
    let dir = support_audio::scratch(&format!("filter-kinds-{label}"));
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&package(&dir)).unwrap_or_else(|e| panic!("{e}: {}", v.state_json()));
    v.open_mixer("seq-main", RATE, 960, sources(), Arc::new(Ext)).unwrap();
    v.mixer_edit(MixerEdit::Output { track_id: id(1), bus: Some(id(3)) }).unwrap();
    (dir, v)
}

fn steps(v: &Viewer) -> usize {
    v.run().history()["undo"].as_array().unwrap().len()
}

/// The document's mix from the start (the mixer's stream builder).
fn render(v: &Viewer) -> Vec<u32> {
    let token = CancellationToken::default();
    let mut stream = v.bus_stream("seq-main", RATE, 480, sources(), &Ext).unwrap();
    let (mut l, mut r) = (vec![0.0f32; FRAMES], vec![0.0f32; FRAMES]);
    for at in (0..FRAMES).step_by(480) {
        stream.render(&mut l[at..at + 480], &mut r[at..at + 480], &token).unwrap();
    }
    l.iter().chain(&r).map(|v| v.to_bits()).collect()
}

/// The table's templates, a graphic equalizer with band 3 at +6 dB (flat is the identity).
fn table_templates() -> Vec<(&'static str, EffectSpec)> {
    effect_templates()
        .into_iter()
        .filter(|(_, s)| matches!(s, EffectSpec::Filter { .. }))
        .map(|(name, mut spec)| {
            if let EffectSpec::Filter { filter_kind, params } = &mut spec
                && filter_kind.starts_with("graphic_eq")
            {
                params.iter_mut().find(|(k, _)| k == "amp3").unwrap().1 = 6.0;
            }
            (name, spec)
        })
        .collect()
}

#[test]
fn every_table_kind_on_a_bus_and_the_master_is_one_undo_step_and_undo_restores_the_mix() {
    let (_dir, mut v) = session("bus");
    let templates = table_templates();
    assert_eq!(templates.len(), score::FILTER_KINDS.len());
    for (name, row) in templates.iter().map(|(n, _)| *n).zip(score::FILTER_KINDS.iter()) {
        assert_eq!(name, row.filter_kind, "templates follow the table");
    }
    let bypass = render(&v);
    for (name, spec) in &templates {
        for bus in [Some(id(3)), None] {
            let before = steps(&v);
            let instance = spec.to_preset_instance("fx-0");
            v.mixer_edit(MixerEdit::AddEffect { bus: bus.clone(), instance, at: None }).unwrap_or_else(|e| panic!("{name} on {bus:?}: {e}"));
            assert_eq!(steps(&v), before + 1, "{name} on {bus:?} is one step");
            assert_ne!(render(&v), bypass, "{name} on {bus:?} changes the mix");
            v.mixer_undo(true).unwrap();
            assert_eq!(render(&v), bypass, "{name} on {bus:?}: undo restores the mix");
        }
    }
}

#[test]
fn refusals_take_no_step_and_the_instance_survives_save_and_reopen() {
    let (dir, mut v) = session("refusals");
    let geq = table_templates().into_iter().find(|(n, _)| *n == "graphic_eq_10_bands").unwrap().1;
    v.mixer_edit(MixerEdit::AddEffect { bus: Some(id(3)), instance: geq.to_preset_instance("fx-0"), at: None }).unwrap();
    let s0 = steps(&v);
    // Out of range (amp past 70 dB), not a whole number (multiband bands 2.5), an unknown kind.
    assert!(v.mixer_edit(MixerEdit::EffectParam { bus: Some(id(3)), instance_id: "fx-0".into(), key: "amp3".into(), value: 500.0 }).is_err());
    let mut mb = table_templates().into_iter().find(|(n, _)| *n == "multiband_compressor").unwrap().1.to_preset_instance("fx-1");
    mb.params.iter_mut().find(|p| p.key == "bands").unwrap().value = 2.5;
    assert!(v.mixer_edit(MixerEdit::AddEffect { bus: None, instance: mb, at: None }).is_err());
    let mut unknown = geq.to_preset_instance("fx-2");
    unknown.filter_kind = "flux_capacitor".into();
    assert!(v.mixer_edit(MixerEdit::AddEffect { bus: None, instance: unknown, at: None }).is_err());
    let mut keyed = geq.to_preset_instance("fx-3");
    keyed.keyframes.push(StudioAudioPresetKeyframe { key: "accuracy".into(), offset_ticks: 0, value: 100.0 });
    assert!(v.mixer_edit(MixerEdit::AddEffect { bus: None, instance: keyed, at: None }).is_err(), "accuracy is not automatable");
    assert_eq!(steps(&v), s0);
    // A valid parameter edit is one step.
    v.mixer_edit(MixerEdit::EffectParam { bus: Some(id(3)), instance_id: "fx-0".into(), key: "amp5".into(), value: -9.0 }).unwrap();
    assert_eq!(steps(&v), s0 + 1);
    let stored = v.mixer_json()["strips"][2]["effects"][0].clone();
    assert_eq!(stored["filter_kind"], "graphic_eq_10_bands");
    let saved = dir.join("saved.handshake");
    v.save(&saved, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&saved).unwrap();
    w.open_mixer("seq-main", RATE, 960, sources(), Arc::new(Ext)).unwrap();
    assert_eq!(w.mixer_json()["strips"][2]["effects"][0], stored);
    assert_eq!(render(&w), render(&v));
}

#[test]
fn a_keyframed_band_on_a_bus_moves_the_response() {
    let (_dir, mut v) = session("keys");
    let bypass = render(&v);
    let flat = effect_templates().into_iter().find(|(n, _)| *n == "graphic_eq_10_bands").unwrap().1;
    let mut instance = flat.to_preset_instance("fx-0");
    instance.keyframes = vec![
        StudioAudioPresetKeyframe { key: "amp3".into(), offset_ticks: 0, value: 0.0 },
        StudioAudioPresetKeyframe { key: "amp3".into(), offset_ticks: 4800 * TICKS_PER_SAMPLE, value: 12.0 },
    ];
    let before = steps(&v);
    v.mixer_edit(MixerEdit::AddEffect { bus: Some(id(3)), instance, at: None }).unwrap();
    assert_eq!(steps(&v), before + 1);
    let keyed = render(&v);
    // Flat at sample 0 (the flat equalizer is the identity), moved once the ramp is designed.
    let (l, lb) = (&keyed[..FRAMES], &bypass[..FRAMES]);
    assert_eq!(&l[..200], &lb[..200], "amp3 = 0 at the start: identity");
    assert_ne!(&l[4800..], &lb[4800..], "the keyframed band moves the response");
}

#[test]
fn table_kinds_round_trip_through_stored_presets() {
    let dir = support_audio::scratch("filter-kinds-presets");
    let specs: Vec<EffectSpec> = table_templates().into_iter().map(|(_, s)| s).collect();
    for spec in &specs {
        let back = EffectSpec::from_preset_instance(&spec.to_preset_instance("fx-0")).unwrap().unwrap();
        assert_eq!(&back, spec);
    }
    let mut set = StudioAudioEffectPresetSet::new();
    set.presets.push(preset_from_chain("all-kinds", "All kinds", &["Lane"], &specs));
    let path = dir.join("presets.json");
    save_preset_set(&path, &set, false).unwrap();
    let loaded = load_preset_set(&path).unwrap();
    assert_eq!(chain_from_preset(&loaded.presets[0]).unwrap(), specs);
    // A duplicate key is refused by Folio; an unknown key by the table (preset_effect).
    let mut dup = set.clone();
    let first = dup.presets[0].effects[0].params[0].clone();
    dup.presets[0].effects[0].params.push(first);
    assert_eq!(save_preset_set(&dir.join("dup.json"), &dup, false).unwrap_err().code, "preset_invalid");
    let mut extra = set.presets[0].clone();
    extra.effects[0].params.push(StudioAudioPresetParam { key: "wet".into(), value: 1.0 });
    let e = chain_from_preset(&extra).unwrap_err();
    assert_eq!(e.code, "preset_effect");
    assert!(e.detail.contains("audio_filter_unknown_parameter"), "{}", e.detail);
}

#[test]
fn batch_jobs_and_waveform_effects_run_a_graphic_eq_and_the_multiband_compressor() {
    let dir = support_audio::scratch("filter-kinds-batch");
    let actor = hsk_studio_accord::ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let pick = |name: &str| table_templates().into_iter().find(|(n, _)| *n == name).unwrap().1;
    let mut queue = AudioBatchQueue::new(actor);
    let mut expected = Vec::new();
    for channels in [1u16, 2] {
        let input = dir.join(format!("in-{channels}.wav"));
        std::fs::write(&input, support_audio::wav(channels, RATE, support_audio::WavFormat::Float32, None, &support_audio::signal(6000, usize::from(channels)))).unwrap();
        for name in ["graphic_eq_10_bands", "multiband_compressor"] {
            let output = dir.join(format!("{name}-{channels}.flac"));
            queue.submit(AudioBatchJob {
                input: input.clone(),
                artifact: output.file_name().unwrap().to_string_lossy().into_owned(),
                output,
                overwrite: false,
                options: ImportOptions::default(),
                limits: ImportLimits::default(),
                chain: vec![pick(name)],
                keyframes: Vec::new(),
                format: MixFormat::Flac { bits: 24 },
            });
            expected.push(name);
        }
    }
    let receipts = queue.run();
    for (r, name) in receipts.iter().zip(&expected) {
        assert_eq!(r.outcome, BatchOutcome::Complete, "{name}: {}", r.to_json());
        let tag = if *name == "multiband_compressor" { "multiband:" } else { "graphic_eq_10_bands:" };
        assert!(r.chain[0].starts_with(tag), "{name}: {:?}", r.chain);
    }

    // Waveform effects over a selection: one step each, the samples outside unchanged.
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&support_waveform::package(&dir)).unwrap();
    let src = support_waveform::source();
    v.open_waveform(RATE, src.clone()).unwrap();
    let base = steps(&v);
    for (i, name) in ["graphic_eq_10_bands", "multiband_compressor"].iter().enumerate() {
        let effect = pick(name).to_preset_instance(format!("fx-{i}"));
        v.waveform_edit(WaveformEdit::Effect { selection: WaveSelection { start: 1000, end: 3000, channels: 0b01 }, effect }).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    assert_eq!(steps(&v), base + 2);
    let b = v.waveform_buffer().unwrap();
    assert_eq!(b.channels()[1], src[1], "unselected channel unchanged");
    assert_eq!(b.channels()[0][..1000], src[0][..1000]);
    assert_eq!(b.channels()[0][3000..], src[0][3000..]);
    assert_ne!(b.channels()[0][1000..3000], src[0][1000..3000]);
}

/// Review F4: one aggregate budget over the estimated state of every bus and master effect, before
/// the graph builds any. A hostile stored routing past it opens no stream; an edit that would pass it
/// is refused with no step.
#[test]
fn effect_state_past_the_aggregate_budget_is_refused_before_the_graph_builds() {
    let dir = support_audio::scratch("filter-kinds-budget");
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&package(&dir)).unwrap();
    let mut delay = effect_templates().into_iter().find(|(n, _)| *n == "feedback_delay").unwrap().1;
    if let EffectSpec::Filter { params, .. } = &mut delay {
        params.iter_mut().find(|(k, _)| k == "time_ms").unwrap().1 = 5000.0;
    }
    let list = |n: usize, from: usize| (from..from + n).map(|i| delay.to_preset_instance(format!("fx-{i}"))).collect::<Vec<_>>();
    let routing = |bus: usize| {
        let mut r = StudioAudioRouting::new("seq-main");
        r.routes = vec![StudioTrackRoute { track_id: id(1), output: Some(id(3)), sends: vec![] }];
        r.master.effects = list(64, 0);
        r.bus_effects = vec![StudioBusEffects { track_id: id(3), effects: list(bus, 64) }];
        StudioAudioRoutingSet { routings: vec![r] }
    };
    // 64 + 64 five-second stereo delays at 48 kHz: about 491 MB of lines, past the 256 MiB budget.
    v.set_audio_routing(&routing(64)).unwrap();
    let e = v.open_mixer("seq-main", RATE, 960, sources(), Arc::new(Ext)).unwrap_err();
    assert!(e.contains("effect_state_over_budget"), "{e}");
    assert!(v.mixer_json()["error"].as_str().is_some_and(|t| t.contains("effect_state_over_budget")), "{}", v.mixer_json());
    // Gap ruling 1: every builder of the bus graph holds the same budget (here the document's bus
    // stream, the path of playback, mixdown and stems).
    let refused = v.bus_stream("seq-main", RATE, 480, sources(), &Ext).err().expect("refused");
    assert!(refused.contains("limit_exceeded"), "{refused}");
    // 64 + 5 fit (about 265 MB); a sixth delay on the bus would pass the budget: refused, no step.
    let (_dir, mut w) = session("budget-edit");
    w.set_audio_routing(&routing(5)).unwrap();
    w.open_mixer("seq-main", RATE, 960, sources(), Arc::new(Ext)).unwrap();
    let before = steps(&w);
    let e = w.mixer_edit(MixerEdit::AddEffect { bus: Some(id(3)), instance: delay.to_preset_instance("fx-200"), at: None }).unwrap_err();
    assert!(e.contains("effect_state_over_budget"), "{e}");
    assert_eq!(steps(&w), before);
}

/// Gap ruling 2: the mixdown's repair replay (the stored waveform edit list of a clip source) records
/// a recomputed noise print the same way the editor does: one op record with one observe frame, and
/// a status line.
#[test]
fn the_mixdown_repair_replay_records_a_recomputed_noise_print() {
    let (dir, forced) = forced_print_package("repair-record");
    let mut x = Viewer::new(Prefer::Cpu);
    x.open(&forced).unwrap();
    x.set_export_audio(sources(), Arc::new(Ext), RATE).unwrap();
    let before = x.run().ops().len();
    x.export_sequence_mix("seq-main", MixFormat::WavFloat, &dir.join("print.wav"), false).unwrap();
    assert_recomputed_once(&x, before, "mixdown");
}

/// One `waveform_noise_print_recomputed` record (one observe frame, the forced stored digest) among
/// the ops after `before`.
fn assert_recomputed_once(x: &Viewer, before: usize, path: &str) {
    let records: Vec<_> = x.run().ops()[before..].iter().filter(|o| o["op"] == "waveform_noise_print_recomputed").cloned().collect();
    assert_eq!(records.len(), 1, "{path}: {:?}", x.run().ops());
    assert_eq!(records[0]["observe"]["frames"], 1, "{path}: {}", records[0]);
    assert_eq!(records[0]["detail"]["stored_digest"], "0".repeat(64), "{path}: {}", records[0]);
}

/// A package whose stored waveform edit list (a noise reduction on clip 1's source) carries a stored
/// print digest that no replay reproduces (review F3): `(dir, package)`.
fn forced_print_package(label: &str) -> (PathBuf, PathBuf) {
    use hsk_studio_harness::view::waveform::{NoiseReduceParams, WaveformDocument, WaveformOp};
    use hsk_studio_harness::{Harness, References, Selection, asset_local_test_context};
    let (dir, mut v) = session(label);
    v.set_export_audio(sources(), Arc::new(Ext), RATE).unwrap();
    v.open_waveform_from_clip("SCLP-019abcde-0000-7000-8000-000000000001").unwrap();
    let all = |a: u64, b: u64| WaveSelection { start: a, end: b, channels: score::ALL_CHANNELS };
    let params = NoiseReduceParams { fft_size: 1024, reduction: score::NoiseReductionParams::default() };
    v.waveform_edit(WaveformEdit::NoiseReduce { selection: all(0, 20_000), print: all(0, 12_000), params }).unwrap();
    let saved = dir.join("repair.handshake");
    v.save(&saved, false).unwrap();
    let mut doc = WaveformDocument::parse(v.run().document_entry("waveform_edit").unwrap()).unwrap();
    if let WaveformOp::NoiseReduce { print_digest, .. } = &mut doc.ops[0] {
        *print_digest = "0".repeat(64);
    }
    let token = CancellationToken::default();
    let h = Harness::new(Selection { providers: Vec::new(), references: References::TrustAll }).unwrap();
    let forced = dir.join("repair-forced.handshake");
    {
        let mut run = h.run(asset_local_test_context(), &token);
        assert!(run.open_path(&saved));
        run.set_document_entry("waveform_edit", Some(doc.to_bytes())).unwrap();
        assert!(run.save_to(&forced));
    }
    (dir, forced)
}

/// Last gap ruling: program playback and Export Media replay the repair through `sequence_sound` and
/// record a recomputed print like the mixdown (software output device only: nothing reaches the
/// speakers; Export Media over the reel DNxHR fixture bound to the program monitor).
#[cfg(feature = "audio-device")]
#[test]
fn playback_and_export_media_record_a_recomputed_noise_print() {
    use hsk_studio_harness::view::timeline::ExportSequenceRequest;
    use hsk_studio_score_device::{DeviceGrant, Direction, Limits, SOFTWARE_OUTPUT_DEVICE, SoftwareOptions, SoftwareProvider};
    const DNX: &[u8] = include_bytes!("../../hsk-studio-reel/tests/fixtures/dnx/dnxhr-lb-264x120.mov");
    let (dir, forced) = forced_print_package("repair-play-export");
    // Playback: the program output opens on the sequence over the repaired sound.
    let mut x = Viewer::new(Prefer::Cpu);
    x.open(&forced).unwrap();
    x.set_export_audio(sources(), Arc::new(Ext), RATE).unwrap();
    let provider = SoftwareProvider::with_options(Limits::default(), SoftwareOptions { loopback_frames: 1 << 18 });
    let actor = hsk_studio_accord::ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let grant = DeviceGrant::new(actor, hsk_studio_accord::DomainId::parse("STRK-019abcde-0000-7000-8000-000000000001").unwrap(), &[Direction::Output], None);
    let before = x.run().ops().len();
    x.audio_out_open(&provider, &grant, SOFTWARE_OUTPUT_DEVICE, 480, Some("seq-main")).unwrap_or_else(|e| panic!("{} {}", e.code, e.detail));
    assert_recomputed_once(&x, before, "playback");
    let _ = x.audio_out_close();
    // Export Media: the export's sound port reads the same repaired sound.
    let mut y = Viewer::new(Prefer::Cpu);
    y.open(&forced).unwrap();
    y.bind_program_media("seq-main", vec![DNX.to_vec()], 64, 32).unwrap();
    y.set_export_audio(sources(), Arc::new(Ext), RATE).unwrap();
    let mut request = ExportSequenceRequest::new("studio.dnxhr.hq.mov", dir.join("forced.mov"));
    request.width = 64;
    request.height = 32;
    let before = y.run().ops().len();
    let _ = y.export_sequence("seq-main", request);
    assert_recomputed_once(&y, before, "export media");
}

#[test]
fn stems_with_a_master_graphic_eq_sum_to_the_print_and_a_master_limiter_shows_in_the_residual() {
    for (name, sums) in [("graphic_eq_10_bands", true), ("hard_limiter", false)] {
        let (dir, mut v) = session(&format!("stems-{name}"));
        v.set_export_audio(sources(), Arc::new(Ext), RATE).unwrap();
        let spec = table_templates().into_iter().find(|(n, _)| *n == name).unwrap().1;
        v.mixer_edit(MixerEdit::AddEffect { bus: None, instance: spec.to_preset_instance("fx-0"), at: None }).unwrap();
        let out = dir.join("stems");
        std::fs::create_dir_all(&out).unwrap();
        let report = v.export_sequence_stems("seq-main", &[id(3)], true, MixFormat::WavFloat, &out, false).unwrap();
        println!("MEASURED: stems with master {name}: residual {:?} dBFS", report.residual_dbfs());
        assert_eq!(report.sums_to_print(), sums, "{name}: {}", report.to_json());
    }
}

/// Review F2: the drive rows `mixer.add_effect` and `waveform.effect` declare exactly the panel's
/// templates (`effect_templates()` names joined by `|`), so every table kind is drivable by name.
#[test]
fn drive_template_lists_equal_the_panel_templates() {
    let names: Vec<&str> = effect_templates().into_iter().map(|(n, _)| n).collect();
    assert_eq!(names.len(), 4 + score::FILTER_KINDS.len());
    let joined = names.join("|");
    for id in ["mixer.add_effect", "waveform.effect"] {
        let row = hsk_studio_harness_core::action::command(id).unwrap_or_else(|| panic!("{id}"));
        let template = row.parameters.iter().find(|p| p.key == "template").unwrap_or_else(|| panic!("{id} template"));
        assert_eq!(template.shape, joined, "{id}");
    }
}
