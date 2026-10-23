//! Hosted plugins on a waveform selection (lane SMB-AU-WAVEFORM-EDITOR slice 4b, parity row AU-19;
//! feature `audio-plugin`, edge harness -> score-plugin). The editor keeps a registry of approved
//! plugins (path + SHA-256 + format + grant + host options); registering probes the plugin once in
//! score-plugin's isolated worker (the viewer process never loads plugin code). Running a
//! [`PluginEdit`] activates a fresh isolated instance at the buffer's rate on the selection's one or
//! two channels, sets its parameters, bounces the selection through it with score's `Engine::bounce`
//! via `PluginProcessor` (latency compensated; any worker fault discards the result) and retires the
//! instance (worker reaped). Only CLAP is a supported format (score-plugin's worker backends); VST3
//! and every other format are refused at registration with `waveform_plugin_format_unsupported`
//! (STU-AUT-020: unsupported plugins stay disclosed and unavailable, no fallback).
#![deny(clippy::arithmetic_side_effects)]

use super::waveform::{PluginEdit, PluginHost};
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::{self as score, PasteMode, WaveBuffer, WaveClip, WaveEditReport};
use hsk_studio_score_plugin::{
    ApprovedPlugin, HostOptions, IsolatedHost, Parameter, PluginFormat, PluginGrant, PluginProcessor, PluginProvider, ProcessConfig,
};
use std::collections::BTreeMap;
use std::time::Duration;

/// Deadline of each worker call (probe, activation, one block).
pub const PLUGIN_CALL_DEADLINE: Duration = Duration::from_secs(20);
/// Block size of a plugin bounce.
const PLUGIN_BLOCK: usize = 4096;
/// State bound passed to the worker.
const PLUGIN_STATE_BYTES: u32 = 65_536;
/// Most registered plugins.
pub const MAX_REGISTERED_PLUGINS: usize = 64;

/// One approved plugin.
#[derive(Clone, Debug)]
pub struct RegisteredPlugin {
    pub plugin: ApprovedPlugin,
    pub grant: PluginGrant,
    pub options: HostOptions,
}

/// The editor's plugins by content SHA-256 (lower-case hex).
#[derive(Default)]
pub struct PluginRegistry {
    plugins: BTreeMap<String, RegisteredPlugin>,
}

impl PluginRegistry {
    pub fn get(&self, sha256_hex: &str) -> Option<&RegisteredPlugin> {
        self.plugins.get(sha256_hex)
    }
    pub fn len(&self) -> usize {
        self.plugins.len()
    }
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }
}

fn perr(e: hsk_studio_score_plugin::PluginError) -> String {
    format!("waveform_plugin_{}", e.code())
}

impl PluginHost for PluginRegistry {
    fn run(&self, buffer: &mut WaveBuffer, edit: &PluginEdit, cancel: &CancellationToken) -> Result<WaveEditReport, String> {
        let reg = self.plugins.get(&edit.content_sha256).ok_or_else(|| format!("waveform_plugin_unregistered: {}", edit.content_sha256))?;
        let clip = buffer.copy(edit.selection).map_err(|e| format!("waveform_{}", e.code()))?;
        let channels = clip.channels.len();
        let layout = match channels {
            1 => score::ChannelLayout::mono(score::STEREO_LABELS[0]),
            2 => score::ChannelLayout::stereo(),
            _ => return Err("waveform_plugin_channels: plugins run on one or two channels".into()),
        };
        let rate = buffer.sample_rate_hz();
        let host = IsolatedHost::new(reg.options.clone()).map_err(perr)?;
        let config = ProcessConfig {
            plugin: reg.plugin.clone(),
            grant: Some(reg.grant.clone()),
            sample_rate_hz: rate,
            layout: layout.clone(),
            max_block_frames: PLUGIN_BLOCK,
            parameters: edit.parameters.iter().map(|(id, value)| Parameter { id: *id, value: *value }).collect(),
            max_state_bytes: PLUGIN_STATE_BYTES,
        };
        let mut instance = host.activate(&config, cancel, PLUGIN_CALL_DEADLINE).map_err(perr)?;
        let frames = clip.frames();
        let mut out: Vec<Vec<f32>> = (0..channels).map(|_| Vec::new()).collect();
        let result = {
            let mut processor = PluginProcessor::new(instance.as_mut(), PLUGIN_CALL_DEADLINE);
            let resource = hsk_studio_accord::DomainId::parse(super::audio_import::IMPORT_CLIP_ID).map_err(|_| "waveform_plugin: resource id".to_owned())?;
            let engine = score::Engine::new(resource.clone(), 1);
            let ctx = score::RequestContext { resource, actor: Some(crate::harness::default_actor()), expected_revision: 1 };
            let inputs: Vec<&[f32]> = clip.channels.iter().map(Vec::as_slice).collect();
            let request = score::BounceRequest { sample_rate_hz: rate, layout: &layout, start_sample: 0, channels: &inputs, block_frames: PLUGIN_BLOCK };
            let mut block = score::OutputBlock::with_capacity(layout.clone(), PLUGIN_BLOCK).map_err(|e| format!("waveform_{}", e.code()))?;
            let mut sink = |chunk: score::BounceChunk<'_>| {
                for (dst, src) in out.iter_mut().zip(chunk.channels) {
                    dst.extend_from_slice(src);
                }
                Ok(())
            };
            let bounced = engine.bounce(&ctx, &request, &mut processor, &mut block, &mut sink, cancel);
            bounced.map_err(|e| match processor.take_error() {
                Some(p) => perr(p),
                None => format!("waveform_{}", e.code()),
            })
        };
        let _ = instance.retire();
        result?;
        if out.iter().any(|c| c.len() as u64 != frames) {
            return Err("waveform_plugin_length".into());
        }
        buffer
            .paste(edit.selection.start, &WaveClip { channels: out }, PasteMode::Overwrite, edit.selection.channels, cancel)
            .map_err(|e| format!("waveform_{}", e.code()))
    }
}

impl super::Viewer {
    /// Registers an approved plugin with the open waveform editor after probing it in an isolated
    /// worker. Refusals: editor closed; a format other than CLAP
    /// (`waveform_plugin_format_unsupported`); a full registry; any probe failure (typed
    /// `waveform_plugin_<score-plugin code>`, e.g. unauthorized, hash_mismatch, unavailable).
    /// Returns the registry key (content SHA-256, lower-case hex).
    pub fn register_waveform_plugin(&mut self, plugin: ApprovedPlugin, grant: PluginGrant, options: HostOptions) -> Result<String, String> {
        let cancel = self.cancel;
        let w = self.waveform.as_mut().ok_or("waveform: not open")?;
        if plugin.format != PluginFormat::Clap {
            return Err(format!("waveform_plugin_format_unsupported: {:?} (no score-plugin worker backend)", plugin.format));
        }
        let key = score::to_hex(&plugin.content_sha256);
        if !w.plugins.plugins.contains_key(&key) && w.plugins.plugins.len() >= MAX_REGISTERED_PLUGINS {
            return Err("waveform_plugin_registry_full".into());
        }
        let host = IsolatedHost::new(options.clone()).map_err(perr)?;
        host.probe(&plugin, Some(&grant), cancel, PLUGIN_CALL_DEADLINE).map_err(perr)?;
        w.plugins.plugins.insert(key.clone(), RegisteredPlugin { plugin, grant, options });
        Ok(key)
    }
}
