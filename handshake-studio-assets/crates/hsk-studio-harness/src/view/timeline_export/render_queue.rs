//! Render queue items from Folio records (lane SMB-AE-MOTION-RECORDS slice 2b; feature `media`;
//! STU-MOT-120..125): a `StudioRenderQueueItem` (composition, render settings, one or more output
//! modules) becomes one reel `ExportJob` per output module through [`super::composition`], with
//! the composition evaluated by composite's `CompositionTemporalSource` (Folio composition records
//! plus motion records: shape layers and text animators render; [`TemporalScenes`]).
//!
//! Every setting is either honoured or refused before anything renders (STU-VID-062, never a
//! silent substitute). Honoured: `time_span` (length of comp, custom; whole frames), `frame_rate`
//! (the comp's own), `quality` best / current, `resolution` full / current, the switches whose
//! Studio value is fixed (disk cache, proxies, solo, guide layers), colour depth 8 / current,
//! output format MOV VC-3 (`codec_option` = DNxHR profile) or MOV / MP4 AV1 (`codec_option` =
//! quantizer), RGB channels, depth 8 / 10, the audio group (auto = on when the composition has
//! audio layers, on = a track even when silent, off), Rec.709 / unset output colour space,
//! `post_render_action` none, relative output paths under the caller's output root. Refused with a
//! stable code: work area (Folio stores none), a frame rate other than the comp's, draft /
//! wireframe, reduced resolutions, effects all off, colour depth 16 / 32, field rendering, 3:2
//! pulldown, motion blur and frame blending switched on, alpha channels, resize / crop other than
//! the composition size / zero, mono audio, any other output colour space (the STU-MOT-122a
//! mismatch surfaces before the render), project links and post-render actions.
//!
//! STU-MOT-120's one-evaluation-several-outputs is not reproduced: each output module is its own
//! queue item and evaluates the composition again (same frames, same receipts per item).
use super::composition::{CompositionAudioLayer, CompositionExport, CompositionScenes, composition_export_job};
use crate::view::monitor::MonitorFail;
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::temporal::composition::CompositionTemporalSource;
use hsk_studio_composite::{SceneComposition, ScenePixels};
use hsk_studio_folio::{
    StudioAudioOutput, StudioColorDepth, StudioComposition, StudioEffectsSwitch, StudioFieldRender, StudioLayerSwitch,
    StudioMotionRecords, StudioOutputChannels, StudioOutputFormat, StudioOutputModule, StudioPostRenderAction,
    StudioPulldown, StudioRenderQueueItem, StudioRenderQuality, StudioRenderResolution, StudioTimeSpan,
};
use hsk_studio_pigment::render_plan::ColourTag;
use hsk_studio_reel::{
    Av1Settings, ChromaFormat, Container, ExportJob, ExportRecipe, PcmChoice, Vc3Profile, VideoCodecChoice,
};
use std::path::Path;
use std::sync::Arc;

fn refuse(code: &'static str, detail: impl Into<String>) -> MonitorFail {
    MonitorFail { code, detail: detail.into() }
}

/// [`CompositionScenes`] over composite's composition evaluator.
pub struct TemporalScenes<P> {
    pub source: CompositionTemporalSource,
    pub pixels: P,
}

impl<P: ScenePixels + Send> CompositionScenes for TemporalScenes<P> {
    fn scene_at(&mut self, tick: u64, cancel: &CancellationToken) -> Result<SceneComposition, String> {
        let tick = i64::try_from(tick).map_err(|_| "tick_overflow".to_owned())?;
        self.source.scene_at(tick, cancel).map_err(|e| format!("{e:?}"))
    }
    fn pixels(&self) -> &dyn ScenePixels {
        &self.pixels
    }
}

/// The span `[in, out)` of an item (module docs).
pub fn item_span(item: &StudioRenderQueueItem, comp: &StudioComposition) -> Result<(u64, u64, u64), MonitorFail> {
    let tpf = comp.frame_rate.ticks_per_frame().ok_or_else(|| refuse("render_frame_rate", "composition rate has no whole tick count"))?;
    if let Some(r) = item.settings.frame_rate
        && i128::from(r.numerator).saturating_mul(i128::from(comp.frame_rate.denominator))
            != i128::from(comp.frame_rate.numerator).saturating_mul(i128::from(r.denominator))
    {
        return Err(refuse("render_frame_rate_conversion", "only the composition's own frame rate renders"));
    }
    let (start, end) = match item.settings.time_span {
        StudioTimeSpan::LengthOfComp => (0, comp.duration),
        StudioTimeSpan::WorkAreaOnly => return Err(refuse("render_work_area_unavailable", "Folio stores no work area")),
        StudioTimeSpan::Custom { start_tick, end_tick } => (start_tick, end_tick),
    };
    // Whole frames: the start rounds down, the end up (a partial last frame renders whole).
    let first = start.checked_div(tpf).unwrap_or(0).saturating_mul(tpf);
    let last = end.div_ceil(tpf).saturating_mul(tpf);
    Ok((first, last, tpf))
}

fn check_settings(item: &StudioRenderQueueItem) -> Result<(), MonitorFail> {
    let s = &item.settings;
    if !matches!(s.quality, StudioRenderQuality::Best | StudioRenderQuality::CurrentSettings) {
        return Err(refuse("render_quality", "draft and wireframe are not built"));
    }
    if !matches!(s.resolution, StudioRenderResolution::Full | StudioRenderResolution::CurrentSettings) {
        return Err(refuse("render_resolution", "reduced resolutions are not built"));
    }
    if s.effects == StudioEffectsSwitch::AllOff {
        return Err(refuse("render_effects_switch", "effects all off is not built"));
    }
    if !matches!(s.color_depth, StudioColorDepth::Bits8 | StudioColorDepth::CurrentSettings) {
        return Err(refuse("render_color_depth", "16 / 32 bpc composition rendering is not built"));
    }
    if s.field_render != StudioFieldRender::Off || s.pulldown_3_2 != StudioPulldown::Off {
        return Err(refuse("render_fields", "field rendering and 3:2 pulldown are not built"));
    }
    if s.motion_blur == StudioLayerSwitch::OnForCheckedLayers || s.frame_blending == StudioLayerSwitch::OnForCheckedLayers {
        return Err(refuse("render_motion_blur", "motion blur and frame blending are not built"));
    }
    Ok(())
}

fn recipe(item: &StudioRenderQueueItem, m: &StudioOutputModule, has_audio: bool) -> Result<(ExportRecipe, u8, ChromaFormat), MonitorFail> {
    if m.channels != StudioOutputChannels::Rgb {
        return Err(refuse("output_alpha", "alpha output is not built"));
    }
    if m.post_render_action != StudioPostRenderAction::None || m.include_project_link {
        return Err(refuse("output_post_render", "post-render actions and project links are not built"));
    }
    if let Some(space) = m.colour.output_color_space.as_deref()
        && !m.colour.preserve_rgb
        && !matches!(space, "rec709" | "bt709" | "srgb")
    {
        return Err(refuse("output_color_space", format!("composition renders BT.709; output space {space} would need a conversion")));
    }
    if m.crop.is_some_and(|c| [c.top, c.left, c.bottom, c.right] != [0, 0, 0, 0]) {
        return Err(refuse("output_crop", "crop is not built"));
    }
    let (container, video, depth) = match m.format {
        StudioOutputFormat::MovVc3 => {
            let profile = match m.codec_option.as_str() {
                "dnxhr_lb" => Vc3Profile::DnxhrLb,
                "dnxhr_sq" => Vc3Profile::DnxhrSq,
                "dnxhr_hq" => Vc3Profile::DnxhrHq,
                "dnxhr_hqx" => Vc3Profile::DnxhrHqx,
                other => return Err(refuse("output_codec_option", format!("unknown VC-3 profile {other}"))),
            };
            let depth = if profile == Vc3Profile::DnxhrHqx { 10 } else { 8 };
            (Container::Mov, VideoCodecChoice::Vc3(profile), depth)
        }
        StudioOutputFormat::MovAv1 | StudioOutputFormat::Mp4Av1 => {
            let quantizer = m.codec_option.parse::<u8>().map_err(|_| refuse("output_codec_option", "AV1 quantizer 0..=255"))?;
            let c = if m.format == StudioOutputFormat::Mp4Av1 { Container::Mp4 } else { Container::Mov };
            (c, VideoCodecChoice::Av1(Av1Settings { quantizer, ..Av1Settings::default() }), m.depth)
        }
    };
    if m.depth != depth && !(matches!(video, VideoCodecChoice::Av1(_)) && matches!(m.depth, 8 | 10)) {
        return Err(refuse("output_depth", format!("depth {} does not match the codec option", m.depth)));
    }
    let audio = match m.audio.output {
        StudioAudioOutput::Off => false,
        StudioAudioOutput::On => true,
        StudioAudioOutput::Auto => has_audio,
    };
    if audio && m.audio.channels != 2 {
        return Err(refuse("output_audio_channels", "the composition mix is stereo"));
    }
    let pcm = audio.then_some(PcmChoice { bits: m.audio.bit_depth, float: m.audio.float });
    Ok((
        ExportRecipe { recipe_id: format!("{}.{}", item.item_id, m.module_id), container, video: Some(video), audio: pcm },
        depth,
        ChromaFormat::Yuv422,
    ))
}

/// Builds a fresh composition evaluator per job.
pub type ScenesFactory<'a, S> = &'a mut dyn FnMut() -> Result<S, MonitorFail>;
/// Resolves an `audio_id` to interleaved stereo f32 at a sample rate.
pub type AudioResolver<'a> = &'a dyn Fn(&str, u32) -> Option<Arc<Vec<f32>>>;
/// Module id and its reel job.
pub type ModuleJobs = Vec<(String, ExportJob)>;

/// One reel job per output module of `item` (module docs). `scenes` builds a fresh evaluator per
/// job; `audio` resolves an audio layer's `audio_id` to interleaved stereo f32 at `sample_rate`;
/// outputs land at `output_root / output_path`.
#[allow(clippy::too_many_arguments)]
pub fn render_queue_jobs<S: CompositionScenes + 'static>(
    item: &StudioRenderQueueItem,
    comp: &StudioComposition,
    motion: Option<&StudioMotionRecords>,
    scenes: ScenesFactory<'_, S>,
    audio: AudioResolver<'_>,
    space: ColourTag,
    output_root: &Path,
    overwrite: bool,
) -> Result<ModuleJobs, MonitorFail> {
    if item.composition_id != comp.composition_id {
        return Err(refuse("render_queue_composition", "item names another composition"));
    }
    check_settings(item)?;
    let (in_tick, out_tick, tpf) = item_span(item, comp)?;
    let layers = motion.and_then(|m| m.composition_audio(&comp.composition_id)).map_or(&[][..], |a| a.layers.as_slice());
    let mut jobs = Vec::with_capacity(item.output_modules.len());
    for m in &item.output_modules {
        if let Some(r) = m.resize
            && (r.width != comp.width || r.height != comp.height)
        {
            return Err(refuse("output_resize", "resize is not built"));
        }
        let (recipe, bit_depth, chroma) = recipe(item, m, !layers.is_empty())?;
        let audio_layers = if recipe.audio.is_some() {
            let rate = m.audio.sample_rate;
            let mut placed = Vec::with_capacity(layers.len());
            for l in layers {
                let samples = audio(&l.audio_id, rate).ok_or_else(|| refuse("output_audio_unavailable", l.audio_id.clone()))?;
                placed.push(CompositionAudioLayer { samples, start_tick: l.start_tick, gain: l.gain as f32 });
            }
            Some((placed, rate))
        } else {
            None
        };
        let job = composition_export_job(CompositionExport {
            scenes: scenes()?,
            width: comp.width,
            height: comp.height,
            ticks_per_frame: tpf,
            in_tick,
            out_tick,
            recipe,
            bit_depth,
            chroma,
            space,
            audio: audio_layers,
            output: output_root.join(&m.output_path),
            overwrite,
            artifact: format!("{} {}", item.item_id, m.module_id),
        })?;
        jobs.push((m.module_id.clone(), job));
    }
    Ok(jobs)
}
