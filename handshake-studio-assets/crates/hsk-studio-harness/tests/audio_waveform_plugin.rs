//! Lane SMB-AU-WAVEFORM-EDITOR slice 4b (feature `audio-plugin`): a hosted plugin applied to a
//! waveform selection through score-plugin's isolated worker, one undo step; VST3 refused at
//! registration; an unregistered plugin refused without a step. The plugin is score-plugin's
//! reference worker backend (gain parameter 0; it hashes the approved file and never loads it), so the
//! oracle is `x * gain` inside the selection and `x` outside. The worker program is the
//! `score-plugin-worker` binary of the same target directory (built when the round also selects
//! hsk-studio-score-plugin); when it is absent the test prints NOT_PROVEN (V4-03).
#![cfg(feature = "audio-plugin")]

mod support_audio;
mod support_waveform;

use hsk_studio_accord::ActorContext;
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::waveform::{PluginEdit, WaveformEdit};
use hsk_studio_score::{WaveSelection, sha256};
use hsk_studio_score_plugin::{ApprovedPlugin, Arch, HostOptions, PluginFormat, PluginGrant, SUPPORTED_CLAP_ABI};

const RATE: u32 = 48_000;

fn worker() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.parent()?;
    let name = if cfg!(windows) { "score-plugin-worker.exe" } else { "score-plugin-worker" };
    let path = dir.join(name);
    path.is_file().then_some(path)
}

#[test]
fn hosted_plugin_on_a_selection_is_one_undo_step() {
    let Some(worker) = worker() else {
        println!("NOT_PROVEN: audio_waveform_plugin hosted_plugin_on_a_selection_is_one_undo_step reason=score-plugin-worker not built in this target directory");
        return;
    };
    let dir = support_audio::scratch("waveform-plugin");
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&support_waveform::package(&dir)).unwrap();
    let src = support_waveform::source();
    v.open_waveform(RATE, src.clone()).unwrap();
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let content_sha256 = sha256(&std::fs::read(&worker).unwrap());
    let approved = |format| ApprovedPlugin { path: worker.clone(), content_sha256, format, arch: Arch::host().unwrap(), abi_version: SUPPORTED_CLAP_ABI };
    let grant = PluginGrant { actor, content_sha256 };
    let options = HostOptions::default().with_worker_program(&worker);
    let refused = v.register_waveform_plugin(approved(PluginFormat::Vst3), grant.clone(), options.clone()).unwrap_err();
    assert!(refused.starts_with("waveform_plugin_format_unsupported"), "{refused}");
    let key = v.register_waveform_plugin(approved(PluginFormat::Clap), grant, options).unwrap();
    let steps = |v: &Viewer| v.run().history()["undo"].as_array().unwrap().len();
    let base = steps(&v);
    // Unregistered plugin: refused, no step.
    let other = PluginEdit { selection: WaveSelection::all(0, 10), content_sha256: "0".repeat(64), parameters: vec![] };
    assert!(v.waveform_edit(WaveformEdit::Plugin(other)).unwrap_err().starts_with("waveform_plugin_unregistered"));
    assert_eq!(steps(&v), base);
    // Gain 0.5 on channel 0 samples 1000 .. 3000.
    let edit = PluginEdit { selection: WaveSelection { start: 1000, end: 3000, channels: 0b01 }, content_sha256: key, parameters: vec![(0, 0.5)] };
    v.waveform_edit(WaveformEdit::Plugin(edit)).unwrap();
    assert_eq!(steps(&v), base + 1);
    let b = v.waveform_buffer().unwrap();
    for (n, (got, x)) in b.channels()[0].iter().zip(&src[0]).enumerate() {
        let want = if (1000..3000).contains(&n) { x * 0.5 } else { *x };
        assert_eq!(*got, want, "n {n}");
    }
    assert_eq!(b.channels()[1], src[1]);
    v.waveform_undo(true).unwrap();
    assert_eq!(v.waveform_buffer().unwrap().channels()[0], src[0]);
}
