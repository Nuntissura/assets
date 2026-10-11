//! Native CLAP backend of the worker (cargo feature `clap`, off by default): the ONLY module in this
//! crate that loads foreign plugin code, and the only one where `unsafe` is allowed.
//!
//! It runs inside the isolated `score-plugin-worker` process, after the worker re-verified the approved
//! file's SHA-256 (`verify_file`), and hosts the library through `clack-host` 0.2.0: load the entry,
//! take the FIRST plugin of the plugin factory, instantiate, activate at the negotiated rate and block
//! bound, start processing, and process one planar `f32` block per request.
//!
//! What this backend does NOT do yet (each is reported honestly, not guessed):
//! * latency: the CLAP latency extension is not queried, so `latency_samples` is 0 (UNVERIFIED for
//!   plugins that add latency);
//! * state: the CLAP state extension is not wired, so state capture is `Unavailable` and the receipt
//!   completeness is `Unavailable`;
//! * audio ports: the audio-ports extension is not queried, so a probe reports 0 inputs/outputs
//!   ("unknown") and the block is offered as one input and one output port with the configured channels;
//! * parameters: a parameter is delivered as a CLAP `ParamValueEvent` (time 0, all notes/ports) at the
//!   start of the next block; parameter metadata (ranges) is not read;
//! * threads: load, activation and processing all run on the worker's single thread (CLAP main-thread
//!   and audio-thread affinity is NOT_PROVEN);
//! * hash-then-load (TOCTOU): closed by a private staging copy. The worker hashes the bytes it read
//!   through one handle, writes exactly those bytes into a file it creates exclusively in the directory
//!   the host made for it (`--stage-dir`), re-verifies the copy through a kept handle and gives the
//!   LOADER the copy's path (`staging.rs`). Without `--stage-dir` the backend refuses to load. A plugin
//!   that needs sibling files does not work from a lone copy.
//!
//! Proof status: CLAP stays NOT_PROVEN until a real plugin runs through this path
//! (`tests/clap_fixture.rs` does that when a fixture library is supplied).
#![deny(clippy::arithmetic_side_effects)]
use crate::backend::{
    Backend, Captured, Configured, MAX_PLUGIN_FILE_BYTES, ProbeInfo, Processed, Refusal,
    check_supported, refusal_of_stage,
};
use crate::staging::{StagedFile, stage_verified};
use crate::types::{Completeness, PluginFormat, SUPPORTED_CLAP_ABI};
use crate::wire::{
    ConfigureSpec, E_BUDGET, E_CONFIG, E_NOT_CONFIGURED, E_PLUGIN, E_STALE, E_UNAVAILABLE,
    PluginSpec,
};
use clack_host::events::event_types::ParamValueEvent;
use clack_host::prelude::*;
use hsk_studio_score::{MAX_BLOCK_FRAMES, MAX_CHANNELS, MAX_SAMPLE_RATE_HZ};
use std::ffi::CString;
use std::path::{Path, PathBuf};

/// Identity the CLAP backend reports in HELLO and receipts.
pub const CLAP_IDENTITY: &str = "clack-host@0.2.0";

struct WorkerShared;

impl SharedHandler<'_> for WorkerShared {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
}

struct WorkerHost;

impl HostHandlers for WorkerHost {
    type Shared<'a> = WorkerShared;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
}

/// First plugin of a loaded entry: its id (for instantiation) and display name.
struct Chosen {
    id: CString,
    name: String,
}

/// Loads the library at the path of the VERIFIED STAGING COPY and returns the entry.
fn load_entry(staged: &Path) -> Result<PluginEntry, Refusal> {
    // SAFETY: loading a dynamic library runs its initializers, which is why this only happens in the
    // isolated worker process: the host kills this process at any deadline or cancel, bounds its
    // lifetime and reaps it. `staged` is the private copy whose bytes `stage_verified` hashed (twice,
    // the second time through the handle that stays open until the library is unloaded) against the
    // caller-approved SHA-256; the original path is never given to the loader.
    unsafe { PluginEntry::load(staged) }.map_err(|_| (E_UNAVAILABLE, 0))
}

fn first_plugin(entry: &PluginEntry) -> Result<Chosen, Refusal> {
    let factory = entry.get_plugin_factory().ok_or((E_UNAVAILABLE, 0))?;
    let descriptor = factory
        .plugin_descriptors()
        .next()
        .ok_or((E_UNAVAILABLE, 0))?;
    let id = descriptor.id().ok_or((E_UNAVAILABLE, 0))?.to_owned();
    let name = descriptor
        .name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(Chosen { id, name })
}

/// One activated, processing plugin instance.
struct Loaded {
    instance: PluginInstance<WorkerHost>,
    processor: Option<StartedPluginAudioProcessor<WorkerHost>>,
    input_ports: AudioPorts,
    output_ports: AudioPorts,
    channels: usize,
    max_frames: usize,
    version: u64,
    pending: Vec<(ClapId, f64)>,
    /// Declared last: fields drop in order, so the library (held by `instance`) is unloaded before the
    /// staging copy is removed.
    _staged: StagedFile,
}

impl Drop for Loaded {
    fn drop(&mut self) {
        if let Some(started) = self.processor.take() {
            let stopped = started.stop_processing();
            self.instance.deactivate(stopped);
        }
    }
}

pub(crate) struct ClapBackend {
    loaded: Option<Loaded>,
    /// Private directory the host made for this worker; `None`: loading is refused.
    stage_dir: Option<PathBuf>,
    staged: u64,
}

impl ClapBackend {
    pub(crate) fn new(stage_dir: Option<PathBuf>) -> Self {
        Self {
            loaded: None,
            stage_dir,
            staged: 0,
        }
    }

    /// Gate, then the verified private copy of the approved file. The only way to obtain a path for
    /// `load_entry`.
    fn stage(&mut self, spec: &PluginSpec) -> Result<StagedFile, Refusal> {
        check_supported(spec)?;
        let dir = self.stage_dir.as_deref().ok_or((E_UNAVAILABLE, 0))?;
        self.staged = self.staged.checked_add(1).ok_or((E_UNAVAILABLE, 0))?;
        stage_verified(
            Path::new(&spec.path),
            &spec.sha,
            MAX_PLUGIN_FILE_BYTES,
            dir,
            self.staged,
        )
        .map_err(refusal_of_stage)
    }
}

impl Backend for ClapBackend {
    fn identity(&self) -> &'static str {
        CLAP_IDENTITY
    }

    fn clap_abi(&self) -> u32 {
        SUPPORTED_CLAP_ABI
    }

    fn probe(&mut self, spec: &PluginSpec) -> Result<ProbeInfo, Refusal> {
        // Declared before `entry`: locals drop in reverse order, so the library is unloaded first.
        let staged = self.stage(spec)?;
        let entry = load_entry(staged.path())?;
        let chosen = first_plugin(&entry)?;
        Ok(ProbeInfo {
            plugin_id: chosen.id.to_string_lossy().into_owned(),
            name: chosen.name,
            format: PluginFormat::Clap,
            abi: SUPPORTED_CLAP_ABI,
            inputs: 0,
            outputs: 0,
        })
    }

    fn configure(&mut self, spec: &PluginSpec, cfg: &ConfigureSpec) -> Result<Configured, Refusal> {
        self.loaded = None;
        check_supported(spec)?;
        let channels = usize::from(cfg.channels);
        let max_frames = usize::try_from(cfg.max_frames).unwrap_or(usize::MAX);
        if channels == 0
            || channels > MAX_CHANNELS
            || max_frames == 0
            || max_frames > MAX_BLOCK_FRAMES
            || cfg.sample_rate_hz == 0
            || cfg.sample_rate_hz > MAX_SAMPLE_RATE_HZ
        {
            return Err((E_CONFIG, 0));
        }
        let mut pending = Vec::with_capacity(cfg.params.len());
        for (param, value) in &cfg.params {
            let id = ClapId::from_raw(*param).ok_or((E_CONFIG, u64::from(*param)))?;
            pending.push((id, *value));
        }
        // `staged` outlives `entry`, `instance` and the processor (see `Loaded::_staged`).
        let staged = self.stage(spec)?;
        let entry = load_entry(staged.path())?;
        let chosen = first_plugin(&entry)?;
        let host_info = HostInfo::new(
            "Handshake Studio score-plugin worker",
            "Handshake",
            "",
            env!("CARGO_PKG_VERSION"),
        )
        .map_err(|_| (E_CONFIG, 0))?;
        let mut instance = PluginInstance::<WorkerHost>::new(
            |_| WorkerShared,
            |_| (),
            &entry,
            &chosen.id,
            &host_info,
        )
        .map_err(|_| (E_PLUGIN, 0))?;
        let stopped = instance
            .activate(
                |_, _| (),
                PluginAudioConfiguration {
                    sample_rate: f64::from(cfg.sample_rate_hz),
                    min_frames_count: 1,
                    max_frames_count: cfg.max_frames,
                },
            )
            .map_err(|_| (E_PLUGIN, 0))?;
        let started = stopped.start_processing().map_err(|_| (E_PLUGIN, 0))?;
        self.loaded = Some(Loaded {
            instance,
            processor: Some(started),
            input_ports: AudioPorts::with_capacity(channels, 1),
            output_ports: AudioPorts::with_capacity(channels, 1),
            channels,
            max_frames,
            version: 1,
            pending,
            _staged: staged,
        });
        Ok(Configured {
            latency: 0,
            version: 1,
        })
    }

    fn process(
        &mut self,
        expected_version: u64,
        start_sample: u64,
        frames: usize,
        channels: usize,
        samples: &[f32],
    ) -> Result<Processed, Refusal> {
        let loaded = self.loaded.as_mut().ok_or((E_NOT_CONFIGURED, 0))?;
        if expected_version != loaded.version {
            return Err((E_STALE, loaded.version));
        }
        if channels != loaded.channels || frames == 0 || frames > loaded.max_frames {
            return Err((E_BUDGET, u64::try_from(frames).unwrap_or(u64::MAX)));
        }
        let processor = loaded.processor.as_mut().ok_or((E_PLUGIN, 0))?;
        let mut input: Vec<Vec<f32>> = samples.chunks_exact(frames).map(<[f32]>::to_vec).collect();
        let mut output: Vec<Vec<f32>> = vec![vec![0.0; frames]; channels];
        let mut events = EventBuffer::new();
        for (id, value) in loaded.pending.drain(..) {
            events.push(&ParamValueEvent::new(0, id, Pckn::match_all(), value));
        }
        let mut produced_events = EventBuffer::new();
        let input_events = InputEvents::from_buffer(&events);
        let mut output_events = OutputEvents::from_buffer(&mut produced_events);
        let input_audio = loaded.input_ports.with_input_buffers([AudioPortBuffer {
            latency: 0,
            channels: AudioPortBufferType::f32_input_only(
                input.iter_mut().map(InputChannel::variable),
            ),
        }]);
        let mut output_audio = loaded.output_ports.with_output_buffers([AudioPortBuffer {
            latency: 0,
            channels: AudioPortBufferType::f32_output_only(
                output.iter_mut().map(|b| b.as_mut_slice()),
            ),
        }]);
        processor
            .process(
                &input_audio,
                &mut output_audio,
                &input_events,
                &mut output_events,
                Some(start_sample),
                None,
            )
            .map_err(|_| (E_PLUGIN, 0))?;
        Ok(Processed {
            version: loaded.version,
            completeness: Completeness::Unavailable,
            latency: 0,
            samples: output.concat(),
        })
    }

    fn set_parameter(&mut self, param: u32, value: f64) -> Result<u64, Refusal> {
        let loaded = self.loaded.as_mut().ok_or((E_NOT_CONFIGURED, 0))?;
        let id = ClapId::from_raw(param).ok_or((E_CONFIG, u64::from(param)))?;
        if !value.is_finite() {
            return Err((E_CONFIG, u64::from(param)));
        }
        loaded.pending.push((id, value));
        loaded.version = loaded.version.checked_add(1).ok_or((E_CONFIG, 0))?;
        Ok(loaded.version)
    }

    fn capture_state(&mut self, _max_bytes: u32) -> Result<Captured, Refusal> {
        let loaded = self.loaded.as_ref().ok_or((E_NOT_CONFIGURED, 0))?;
        Ok(Captured {
            version: loaded.version,
            completeness: Completeness::Unavailable,
            bytes: Vec::new(),
        })
    }
}
