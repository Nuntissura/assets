//! Lane SMB-ATTRIBUTION-1 (RF-06; GUI-DESKTOP-029/041/045/050, STU-ARC-014/015, feature `viewer`):
//! a `Viewer` built with `Viewer::with_context` attributes what it commits to that context: an
//! edit, a fill and (feature `audio-batch`) a waveform effect write op records and history steps
//! carrying the context's actor, client session and correlations from its source (7000 up), and
//! FILE > Close keeps the same context and correlation source. Oracle: the run's own records and
//! history (`Viewer::run`). Not observable here: the actor a score bounce or the audio batch queue
//! names on its own score requests (threaded from `self.run.actor()`, no receipt carries it).
#![cfg(feature = "viewer")]

#[path = "../../hsk-studio-harness/tests/common/mod.rs"]
mod common;
#[cfg(feature = "audio-batch")]
#[path = "../../hsk-studio-harness/tests/support_waveform/mod.rs"]
mod support_waveform;

use common::Scratch;
use hsk_studio_accord::ActorContext;
use hsk_studio_harness_core::{ClientSessionId, RunContext, TransientCorrelation};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::fill_tools::FillSettings;
use hsk_studio_harness_view::view::render::Prefer;
use serde_json::Value;
use std::sync::Arc;

const FIRST: u64 = 7000;

fn viewer() -> Viewer {
    let actor = ActorContext::new("erin-account", "erin-principal", "owner", "owner-principal", "space", "erin-session").unwrap();
    let context = RunContext::new(actor, ClientSessionId::parse("erin-window").unwrap(), Arc::new(TransientCorrelation::starting_at(FIRST)));
    let gpu = hsk_studio_render_gpu::DeviceOptions::default();
    Viewer::with_context(Prefer::Cpu, gpu, context)
}

/// Every record and history step of the viewer's run names erin; every command correlation comes
/// from erin's source. Returns the command ids of the records, oldest first.
fn attributed(v: &Viewer) -> Vec<String> {
    let mut commands = Vec::new();
    for record in v.run().ops() {
        assert_eq!(record["actor"]["session_id"], "erin-session", "{record}");
        if let Some(c) = record["correlation"].as_u64() {
            assert!(c >= FIRST, "correlation from the context's source: {record}");
        }
        if let Some(id) = record["command_id"].as_str() {
            commands.push(id.to_string());
        }
    }
    let history = v.run().history();
    for step in history["undo_steps"].as_array().into_iter().chain(history["redo_steps"].as_array()).flatten() {
        assert_eq!((step["actor"]["session_id"].as_str(), step["client_session"].as_str()), (Some("erin-session"), Some("erin-window")), "{step}");
        assert!(step["correlation"].as_u64().is_some_and(|c| c >= FIRST), "{step}");
    }
    commands
}

fn last(v: &Viewer) -> Value {
    v.run().ops().last().cloned().unwrap()
}

#[test]
fn edits_and_fills_through_a_viewer_attribute_to_its_context() {
    let scratch = Scratch::new("attribution");
    let path = scratch.0.join("two.psd");
    std::fs::write(&path, common::two_layer_psd()).unwrap();
    let mut v = viewer();
    assert_eq!(v.run().actor().session_id(), "erin-session");
    v.open(&path).unwrap_or_else(|e| panic!("{e}"));
    let base = v.layers().into_iter().find(|r| r.name == "base").unwrap_or_else(|| panic!("{}", v.state_json())).layer_id;
    v.rename_layer(&base, "renamed-by-erin").unwrap_or_else(|e| panic!("{e}"));
    let rename = last(&v);
    assert_eq!(rename["command_id"], "layer.rename", "{rename}");
    v.flood_fill(&base, 0.0, 0.0, &FillSettings::default()).unwrap_or_else(|e| panic!("{e}"));
    let fill = last(&v);
    assert!(fill["command_id"].as_str().is_some_and(|c| c.starts_with("layer.")), "a fill is a layer command: {fill}");
    assert_ne!(fill["correlation"], rename["correlation"]);
    v.undo().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(last(&v)["causation"], fill["correlation"], "the undo names the fill it reverses");
    let commands = attributed(&v);
    assert!(commands.iter().any(|c| c == "history.undo"), "{commands:?}");

    // FILE > Close starts a fresh run on the SAME context: same actor, the source continues.
    let before = last(&v)["correlation"].as_u64().unwrap();
    v.close();
    v.open(&path).unwrap_or_else(|e| panic!("{e}"));
    let base = v.layers().into_iter().find(|r| r.name == "base").unwrap_or_else(|| panic!("{}", v.state_json())).layer_id;
    v.rename_layer(&base, "again").unwrap_or_else(|e| panic!("{e}"));
    let again = last(&v);
    assert!(again["correlation"].as_u64().is_some_and(|c| c > before), "one correlation source across runs: {again}");
    attributed(&v);
}

#[cfg(feature = "audio-batch")]
#[test]
fn a_waveform_effect_through_a_viewer_attributes_to_its_context() {
    use hsk_studio_harness_view::view::audio_batch::EffectSpec;
    use hsk_studio_harness_view::view::waveform::WaveformEdit;
    use hsk_studio_score::WaveSelection;
    let scratch = Scratch::new("attribution-waveform");
    let mut v = viewer();
    v.open(&support_waveform::package(&scratch.0)).unwrap_or_else(|e| panic!("{e}"));
    v.open_waveform(support_waveform::RATE, support_waveform::source()).unwrap_or_else(|e| panic!("{e}"));
    let steps = v.run().history()["undo_steps"].as_array().map_or(0, Vec::len);
    let gain = EffectSpec::Gain { db: -6.0 }.to_preset_instance("fx-attr");
    v.waveform_edit(WaveformEdit::Effect { selection: WaveSelection { start: 0, end: 480, channels: 0b01 }, effect: gain }).unwrap_or_else(|e| panic!("{e}"));
    let history = v.run().history();
    let added = &history["undo_steps"].as_array().unwrap()[steps..];
    assert_eq!(added.len(), 1, "one undo step: {history}");
    assert_eq!(added[0]["actor"]["account_id"], "erin-account", "{history}");
    attributed(&v);
}
