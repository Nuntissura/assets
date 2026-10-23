//! Lane SMB-AUDITION-UI (feature `audio-batch`): the viewer mixer and routing editor, headless.
//! Strips for audio and submix tracks with fader, pan, mute, solo, output, sends and meters; every
//! edit is one undo step (track fields through the session timeline step, routing through the
//! routing entry), undo and redo restore the document and the mix; the panel draws its widgets in
//! one egui frame. Oracles: hand-computed samples of constant and impulse clips, the Tech 3341 fact
//! that a stereo 1 kHz sine at -23 dBFS reads -23.0 LUFS (+-0.1), and the history labels.
#![cfg(feature = "audio-batch")]

mod support_audio;

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::mixer::{MixerEdit, widget};
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_pulse::{AudioSources, SourceExtents};
use std::sync::Arc;

const F: u64 = 10_160_640_000;
const SPF: usize = 1920;
const RATE: u32 = 48_000;

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
    }
}
fn track(n: u32, index: u32, kind: TrackKind, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: id(n),
        kind,
        index,
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
fn sequence() -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(50 * F),
        tracks: vec![
            track(1, 0, TrackKind::Audio, vec![clip(1, "tone", 50)]),
            track(2, 1, TrackKind::Audio, vec![clip(2, "imp", 50)]),
            track(3, 0, TrackKind::Submix, vec![]),
        ],
        markers: Vec::new(),
        work_area: None,
    }
}

/// "tone": stereo 1 kHz sine at 2 x 10^(-23/20) (the fader halves it to -23 dBFS); "imp": mono, 0.5
/// at sample 1000.
#[derive(Clone)]
struct Sources(Arc<Vec<(String, Vec<Vec<f32>>)>>);
fn sources() -> Sources {
    let n = 52 * SPF;
    let a = 2.0 * 10f64.powf(-23.0 / 20.0);
    let tone: Vec<f32> = (0..n).map(|k| (a * (2.0 * std::f64::consts::PI * 1000.0 * k as f64 / 48_000.0).sin()) as f32).collect();
    let mut imp = vec![0.0; n];
    imp[1000] = 0.5;
    Sources(Arc::new(vec![("tone".into(), vec![tone.clone(), tone]), ("imp".into(), vec![imp])]))
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
        Some(Ticks::new(52 * F))
    }
}

fn package(dir: &std::path::Path) -> std::path::PathBuf {
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
    let path = dir.join("mixer.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

fn undo_labels(v: &Viewer) -> Vec<String> {
    v.run().history()["undo"].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_owned()).collect()
}

#[test]
fn mixer_edits_are_single_undo_steps_and_drive_the_bus_mix_and_meters() {
    let dir = support_audio::scratch("mixer");
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&package(&dir)).unwrap_or_else(|e| panic!("{e}: {}", v.state_json()));
    v.open_mixer("seq-main", RATE, 960, sources(), Arc::new(Ext)).unwrap();
    let s = v.state_json();
    assert_eq!(s["mixer"]["strips"].as_array().unwrap().len(), 3, "{s}");
    assert_eq!(s["mixer"]["strips"][2]["kind"], "submix");

    // Edits, one undo step each.
    let edits = [
        MixerEdit::Volume { track_id: id(1), linear: 0.5 },
        MixerEdit::Output { track_id: id(1), bus: Some(id(3)) },
        MixerEdit::AddSend { track_id: id(2), target: id(3), level: 1.0, pre_fader: false },
        MixerEdit::Send { track_id: id(2), send_id: "send-0".into(), level: 0.5, pre_fader: true, pan: 0.0 },
        MixerEdit::MasterVolume { linear: 1.0 },
    ];
    for (i, edit) in edits.into_iter().enumerate() {
        let before = undo_labels(&v).len();
        v.mixer_edit(edit.clone()).unwrap_or_else(|e| panic!("{edit:?}: {e}"));
        assert_eq!(undo_labels(&v).len(), before + 1, "edit {i} {edit:?} is one step: {:?}", undo_labels(&v));
    }
    let s = v.state_json();
    let strip1 = &s["mixer"]["strips"][0];
    assert_eq!((strip1["volume"].as_f64(), strip1["output"].as_str()), (Some(0.5), Some(id(3).as_str())));
    assert_eq!(s["mixer"]["strips"][1]["sends"][0]["pre_fader"], true);
    // Refusals change nothing (no step): a send to an audio track, a fader above +12 dB.
    let before = undo_labels(&v).len();
    assert!(v.mixer_edit(MixerEdit::AddSend { track_id: id(2), target: id(1), level: 1.0, pre_fader: false }).is_err());
    assert!(v.mixer_edit(MixerEdit::Volume { track_id: id(1), linear: 10.0 }).is_err());
    assert_eq!(undo_labels(&v).len(), before);

    // The mix: tone (fader 0.5) via the submix; impulse direct (mono centre) + pre-fader send 0.5.
    let [l, r] = v.mixer_render(RATE as usize).unwrap();
    let c = std::f64::consts::FRAC_1_SQRT_2;
    let a = 10f64.powf(-23.0 / 20.0);
    for k in [0usize, 12, 999, 1000, 1001, 24_000] {
        let tone = a * (2.0 * std::f64::consts::PI * 1000.0 * k as f64 / 48_000.0).sin();
        let imp = if k == 1000 { 0.5 * c * 1.5 } else { 0.0 };
        assert!((f64::from(l[k]) - (tone + imp)).abs() < 2e-6, "L {k}: {} vs {}", l[k], tone + imp);
        assert!((f64::from(r[k]) - (tone + imp)).abs() < 2e-6, "R {k}");
    }
    let s = v.state_json();
    let peak1 = s["mixer"]["strips"][0]["peak"][0].as_f64().unwrap();
    assert!((peak1 - a).abs() < 1e-6, "track 1 post-fader peak {peak1} vs {a}");
    let peak2 = s["mixer"]["strips"][1]["peak"][0].as_f64().unwrap();
    assert!((peak2 - 0.5 * c).abs() < 1e-6, "track 2 peak {peak2}");
    // Loudness after 1 s of the -23 dBFS tone (+ one impulse): -23 LUFS within 0.1.
    let integrated = s["mixer"]["master"]["integrated_lufs"].as_f64().unwrap();
    assert!((integrated + 23.0).abs() <= 0.1, "{integrated}");

    // Undo the send pre/post edit: the impulse reaches the bus post-fader at level 1 again.
    v.mixer_undo(true).unwrap();
    v.mixer_undo(true).unwrap();
    assert_eq!(v.state_json()["mixer"]["strips"][1]["sends"][0]["pre_fader"], false);
    v.mixer_undo(true).unwrap();
    v.mixer_undo(true).unwrap();
    v.mixer_undo(true).unwrap();
    let s = v.state_json();
    assert_eq!(s["mixer"]["strips"][0]["volume"].as_f64(), Some(1.0), "the fader edit is undone in the document");
    assert_eq!(s["mixer"]["strips"][0]["output"], serde_json::Value::Null);
    v.mixer_undo(false).unwrap();
    assert_eq!(v.state_json()["mixer"]["strips"][0]["volume"].as_f64(), Some(0.5), "redo");

    // Mute, solo through the timeline step; one egui frame draws the panel.
    v.mixer_edit(MixerEdit::Mute { track_id: id(2), muted: true }).unwrap();
    assert_eq!(v.state_json()["mixer"]["strips"][1]["muted"], true);
    let ctx = egui::Context::default();
    let _ = ctx.run(egui::RawInput::default(), |ctx| v.ui(ctx));
    for name in [widget::PANEL.to_owned(), widget::MASTER_VOLUME.into(), widget::MASTER_METER.into(), format!("mixer.{}.volume", id(1)), format!("mixer.{}.pan", id(1)), format!("mixer.{}.solo", id(2)), format!("mixer.{}.output", id(1)), format!("mixer.{}.meter", id(3)), format!("mixer.{}.add_send", id(1))] {
        assert!(v.drawn().contains(&name), "{name} not drawn: {:?}", v.drawn());
    }
    // The routing survives save and reopen with the session.
    let saved = dir.join("saved.handshake");
    v.save(&saved, false).unwrap();
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&saved).unwrap();
    w.open_mixer("seq-main", RATE, 960, sources(), Arc::new(Ext)).unwrap();
    assert_eq!(w.mixer_json()["strips"][0]["volume"].as_f64(), Some(0.5));
    assert_eq!(w.mixer_json()["strips"][1]["muted"], true);
}

/// Lane SMB-AUDITION-UI-2: bus and master effect editing from the mixer, each edit one undo step:
/// add (gain on the submix, limiter on the master), change a parameter (-6.0206 dB halves the bus),
/// reorder, disable, remove; an out-of-range parameter is refused with no step; undo restores the
/// stack; the panel draws the effect rows.
#[test]
fn mixer_edits_bus_and_master_effects_as_single_undo_steps() {
    use hsk_studio_harness::view::mixer::{effect_templates, fresh_effect_id};
    let dir = support_audio::scratch("mixer-fx");
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&package(&dir)).unwrap();
    v.open_mixer("seq-main", RATE, 960, sources(), Arc::new(Ext)).unwrap();
    v.mixer_edit(MixerEdit::Output { track_id: id(2), bus: Some(id(3)) }).unwrap();
    v.mixer_edit(MixerEdit::Mute { track_id: id(1), muted: true }).unwrap();
    let tmpl = |name: &str| effect_templates().into_iter().find(|(n, _)| *n == name).unwrap().1;
    let steps = |v: &Viewer| undo_labels(v).len();
    let s0 = steps(&v);
    let gain = tmpl("gain").to_preset_instance(fresh_effect_id(&[]));
    v.mixer_edit(MixerEdit::AddEffect { bus: Some(id(3)), instance: gain, at: None }).unwrap();
    v.mixer_edit(MixerEdit::EffectParam { bus: Some(id(3)), instance_id: "fx-0".into(), key: "db".into(), value: -6.020_599_913_279_624 }).unwrap();
    let lim = tmpl("lookahead_limiter").to_preset_instance("fx-1");
    v.mixer_edit(MixerEdit::AddEffect { bus: Some(id(3)), instance: lim, at: Some(0) }).unwrap();
    v.mixer_edit(MixerEdit::MoveEffect { bus: Some(id(3)), instance_id: "fx-1".into(), to: 1 }).unwrap();
    v.mixer_edit(MixerEdit::AddEffect { bus: None, instance: tmpl("gain").to_preset_instance("fx-0"), at: None }).unwrap();
    assert_eq!(steps(&v), s0 + 5, "{:?}", undo_labels(&v));
    let fx = &v.mixer_json()["strips"][2]["effects"];
    assert_eq!((fx[0]["instance_id"].as_str(), fx[1]["instance_id"].as_str()), (Some("fx-0"), Some("fx-1")));
    assert_eq!(v.mixer_json()["master"]["effects"][0]["filter_kind"], "gain");
    // Refused: a limiter ceiling outside score's range; no step.
    assert!(v.mixer_edit(MixerEdit::EffectParam { bus: Some(id(3)), instance_id: "fx-1".into(), key: "ceiling_db".into(), value: 500.0 }).is_err());
    assert_eq!(steps(&v), s0 + 5);
    // The impulse (mono centre 0.5 cos pi/4) through the bus: halved by the gain, delayed by the limiter
    // latency, aligned (latency compensated) at sample 1000.
    let [l, _] = v.mixer_render(4800).unwrap();
    let want = 0.5 * std::f64::consts::FRAC_1_SQRT_2 * 0.5;
    assert!((f64::from(l[1000]) - want).abs() < 1e-6, "{} vs {want}", l[1000]);
    // Disable, then remove the gain: back to the full level.
    v.mixer_edit(MixerEdit::EffectEnabled { bus: Some(id(3)), instance_id: "fx-0".into(), enabled: false }).unwrap();
    v.mixer_edit(MixerEdit::RemoveEffect { bus: Some(id(3)), instance_id: "fx-0".into() }).unwrap();
    assert_eq!(steps(&v), s0 + 7);
    v.mixer_undo(true).unwrap();
    v.mixer_undo(true).unwrap();
    assert_eq!(v.mixer_json()["strips"][2]["effects"][0]["enabled"], true, "undo restores the enabled gain");
    // One egui frame draws the effect rows and the add combos.
    let ctx = egui::Context::default();
    let _ = ctx.run(egui::RawInput::default(), |ctx| v.ui(ctx));
    for name in [format!("mixer.{}.fx.fx-0.db", id(3)), format!("mixer.{}.fx.fx-1.remove", id(3)), format!("mixer.{}.add_effect", id(3)), widget::MASTER_ADD_EFFECT.to_owned(), "mixer.master.fx.fx-0.enabled".into()] {
        assert!(v.drawn().contains(&name), "{name} not drawn");
    }
}
