//! Render queue items from Folio records (lanes SMB-AE-MOTION-RECORDS, -2; feature `media`;
//! STU-MOT-120..125): a `StudioRenderQueueItem` (composition, render settings, one or more output
//! modules) becomes one reel `ExportJob` per output module through [`super::composition`]: the
//! composition is evaluated by composite's `CompositionTemporalSource` (Folio composition records
//! plus motion records; [`TemporalScenes`]) and RENDERED ONCE for all modules of the item
//! (`SharedFrames`, STU-MOT-120: one evaluation, several outputs).
//!
//! A saved document carries the records itself: [`session_render_queue_jobs`] reads the
//! composition and motion records from a `Session` (document entries `studio/composition` and
//! `studio/motion`, validated on read) and builds the item's jobs.
//!
//! Honoured: `time_span` (length of comp, the composition's work area [Folio `composition_work_areas`;
//! none stored = the whole composition], custom; whole frames), `frame_rate` (the comp's own),
//! `quality` best / current and `draft` (rendered as best, noted `render_quality_draft_is_best`:
//! composite has no draft switch), `resolution` full / current / half / third / quarter / custom
//! divisors (lane SMB-AE-RENDER-QUEUE-2: the scene renders at `ceil(size / divisor)`, see
//! `composition::RenderOptions`), colour depth 8 / 16 / 32 bits per channel (`composition::RenderDepth`;
//! `current_settings` = the document's `bits_per_channel` 8 / 16 / 32, else the engine's binary32
//! default, noted), `frame_blending` (`current_settings` and `on_for_checked_layers` = each composition
//! layer's own Folio `frame_blending`, which composite renders; `off_for_all_layers` forces every layer
//! off through [`apply_frame_blending_switch`] on the records the evaluator is built from),
//! `motion_blur` (current settings = the composition's master switch and checked layers, on = the
//! checked layers, off = none; shutter angle, phase and samples from the composition settings,
//! STU-MOT-004 defaults 180 / 0 / 16), output format MOV VC-3 (`codec_option` = DNxHR profile) or
//! MOV / MP4 AV1 (`codec_option` = quantizer), crop, resize, channels `rgb` (straight or
//! premultiplied colour) and `alpha` (grey matte), depth 8 / 10, the audio group (auto = on when the
//! composition has audio layers, on, off), Rec.709 / unset output colour space, `post_render_action`
//! none, relative output paths under the caller's root. Refused with a stable code before rendering:
//! a frame rate other than the comp's, wireframe, effects all off, field rendering and 3:2 pulldown
//! (no reel encoder writes field flags, so the output could not say which field is first),
//! `rgb_alpha` (no reel encoder carries alpha: `output_alpha_codec`; `OutputPost::apply_rgba` is the
//! frame such an encoder would take), mono audio, any other output colour space (STU-MOT-122a before
//! the render), project links, post-render actions.
use super::composition::{
    CompositionAudioLayer, CompositionScenes, CompositionSpan, DEFAULT_SHARED_CACHE_BYTES, FrameRenderer, ModuleOutput,
    MotionBlur, OutputChannels, OutputPost, RenderDepth, RenderOptions, SharedFrames, shared_module_jobs,
};
use crate::view::monitor::MonitorFail;
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::temporal::composition::{CompositionInput, CompositionTemporalSource};
use hsk_studio_composite::text::animate::AnimatedGlyph;
use hsk_studio_composite::{RenderInputs, SceneComposition, ScenePixels, TileInfoSource};
use hsk_studio_folio::{
    Budget, Resolver, StudioAudioOutput, StudioColorDepth, StudioComposition, StudioCompositionRecords, StudioEffectsSwitch,
    StudioFieldRender, StudioFrameBlending,
    StudioLayerSwitch, StudioMotionRecords, StudioOutputChannels, StudioOutputColor, StudioOutputFormat, StudioOutputModule,
    StudioPostRenderAction, StudioPulldown, StudioRenderQueueItem, StudioRenderQuality, StudioRenderResolution, StudioTimeSpan,
};
use hsk_studio_pigment::render_plan::ColourTag;
use hsk_studio_reel::{Av1Settings, ChromaFormat, Container, ExportJob, ExportRecipe, PcmChoice, Vc3Profile, VideoCodecChoice};
use hsk_studio_session::Session;
use std::collections::{BTreeMap, BTreeSet};
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
    fn scene_at(&mut self, tick: i64, cancel: &CancellationToken) -> Result<SceneComposition, String> {
        self.source.scene_at(tick, cancel).map_err(|e| format!("{e:?}"))
    }
    fn pixels(&self) -> &dyn ScenePixels {
        &self.pixels
    }
}

/// Builds a fresh composition evaluator.
pub type ScenesFactory<'a, S> = &'a mut dyn FnMut() -> Result<S, MonitorFail>;
/// Resolves an `audio_id` to interleaved stereo f32 at a sample rate.
pub type AudioResolver<'a> = &'a dyn Fn(&str, u32) -> Option<Arc<Vec<f32>>>;
/// Module id and its reel job.
pub type ModuleJobs = Vec<(String, ExportJob)>;

/// The span `[in, out)` and frame duration of an item (module docs); `work_area` is the
/// composition's stored work area `(start, end)` in ticks, `None` = the whole composition.
pub fn item_span(item: &StudioRenderQueueItem, comp: &StudioComposition, work_area: Option<(u64, u64)>) -> Result<(u64, u64, u64), MonitorFail> {
    let tpf = comp.frame_rate.ticks_per_frame().ok_or_else(|| refuse("render_frame_rate", "composition rate has no whole tick count"))?;
    if let Some(r) = item.settings.frame_rate
        && i128::from(r.numerator).saturating_mul(i128::from(comp.frame_rate.denominator))
            != i128::from(comp.frame_rate.numerator).saturating_mul(i128::from(r.denominator))
    {
        return Err(refuse("render_frame_rate_conversion", "only the composition's own frame rate renders"));
    }
    let (start, end) = match item.settings.time_span {
        StudioTimeSpan::LengthOfComp => (0, comp.duration),
        StudioTimeSpan::WorkAreaOnly => work_area.unwrap_or((0, comp.duration)),
        StudioTimeSpan::Custom { start_tick, end_tick } => (start_tick, end_tick),
    };
    if start >= end || end > comp.duration {
        return Err(refuse("render_span_range", "the span must satisfy 0 <= start < end <= the composition length"));
    }
    // Whole frames: the start rounds down, the end up (a partial last frame renders whole).
    let first = start.checked_div(tpf).unwrap_or(0).saturating_mul(tpf);
    let last = end.div_ceil(tpf).saturating_mul(tpf);
    Ok((first, last, tpf))
}

/// A setting that rendered differently from its name (never silent; lane SMB-AE-RENDER-QUEUE-2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderNote {
    pub code: &'static str,
    pub detail: String,
}

fn note(code: &'static str, detail: impl Into<String>) -> RenderNote {
    RenderNote { code, detail: detail.into() }
}

/// Forces the Folio `frame_blending` of every layer of every composition off when the item's
/// `frame_blending` setting is `off_for_all_layers` (the other members keep each layer's own value,
/// module docs). Build the evaluator from the changed records and pass the matching composition.
pub fn apply_frame_blending_switch(records: &mut StudioCompositionRecords, switch: StudioLayerSwitch) {
    if switch == StudioLayerSwitch::OffForAllLayers {
        for c in &mut records.compositions {
            for l in &mut c.layers {
                l.frame_blending = StudioFrameBlending::Off;
            }
        }
    }
}

/// The render options and notes of an item's settings (module docs); refuses what is not built.
/// `document_bits` is the document's `bits_per_channel` (what `color_depth: current_settings` takes).
pub fn render_settings(item: &StudioRenderQueueItem, comp: &StudioComposition, document_bits: Option<u8>) -> Result<(RenderOptions, Vec<RenderNote>), MonitorFail> {
    let s = &item.settings;
    let mut notes = Vec::new();
    match s.quality {
        StudioRenderQuality::Best | StudioRenderQuality::CurrentSettings => {}
        StudioRenderQuality::Draft => notes.push(note("render_quality_draft_is_best", "composite has no draft quality switch; the item renders at best quality")),
        StudioRenderQuality::Wireframe => return Err(refuse("render_quality", "wireframe is not built")),
    }
    let divisor = match s.resolution {
        StudioRenderResolution::Full | StudioRenderResolution::CurrentSettings => (1, 1),
        StudioRenderResolution::Half => (2, 2),
        StudioRenderResolution::Third => (3, 3),
        StudioRenderResolution::Quarter => (4, 4),
        StudioRenderResolution::Custom { x, y } => (x, y),
    };
    if s.effects == StudioEffectsSwitch::AllOff {
        return Err(refuse("render_effects_switch", "effects all off is not built"));
    }
    let depth = match s.color_depth {
        StudioColorDepth::Bits8 => RenderDepth::Bits8,
        StudioColorDepth::Bits16 => RenderDepth::Bits16,
        StudioColorDepth::Bits32 => RenderDepth::Bits32,
        StudioColorDepth::CurrentSettings => match document_bits {
            Some(8) => RenderDepth::Bits8,
            Some(16) => RenderDepth::Bits16,
            Some(32) => RenderDepth::Bits32,
            other => {
                notes.push(note("render_color_depth_current_unset", format!("no 8 / 16 / 32 document depth ({other:?}); the engine's binary32 default renders")));
                RenderDepth::Float
            }
        },
    };
    if s.field_render != StudioFieldRender::Off || s.pulldown_3_2 != StudioPulldown::Off {
        return Err(refuse("render_fields", "field rendering and 3:2 pulldown are not built: no reel encoder writes field flags"));
    }
    if s.frame_blending == StudioLayerSwitch::OffForAllLayers && comp.layers.iter().any(|l| !l.frame_blending.is_off()) {
        return Err(refuse("render_frame_blending_off_unapplied", "build the evaluator from apply_frame_blending_switch records and pass that composition"));
    }
    let options = RenderOptions { divisor, depth };
    options.size(comp.width, comp.height)?;
    Ok((options, notes))
}

/// The motion blur of an item (module docs; `None` = no layer blurs).
pub fn item_motion_blur(item: &StudioRenderQueueItem, motion: Option<&StudioMotionRecords>) -> Option<MotionBlur> {
    let settings = motion.and_then(|m| m.composition_settings(&item.composition_id))?;
    let on = match item.settings.motion_blur {
        StudioLayerSwitch::OffForAllLayers => false,
        StudioLayerSwitch::OnForCheckedLayers => true,
        StudioLayerSwitch::CurrentSettings => settings.motion_blur,
    };
    if !on || settings.motion_blur_layers.is_empty() {
        return None;
    }
    Some(MotionBlur {
        shutter_angle: settings.shutter_angle,
        shutter_phase: settings.shutter_phase,
        samples_per_frame: settings.samples_per_frame,
        layers: settings.motion_blur_layers.iter().cloned().collect::<BTreeSet<_>>(),
    })
}

fn module_output(
    item: &StudioRenderQueueItem,
    comp: &StudioComposition,
    m: &StudioOutputModule,
    has_audio: bool,
) -> Result<(ExportRecipe, OutputPost, u8), MonitorFail> {
    let channels = match m.channels {
        StudioOutputChannels::Rgb => OutputChannels::Rgb,
        StudioOutputChannels::Alpha => OutputChannels::Alpha,
        StudioOutputChannels::RgbAlpha => return Err(refuse("output_alpha_codec", "no reel encoder carries an alpha channel")),
    };
    if m.post_render_action != StudioPostRenderAction::None || m.include_project_link {
        return Err(refuse("output_post_render", "post-render actions and project links are not built"));
    }
    if let Some(space) = m.colour.output_color_space.as_deref()
        && !m.colour.preserve_rgb
        && !matches!(space, "rec709" | "bt709" | "srgb")
    {
        return Err(refuse("output_color_space", format!("composition renders BT.709; output space {space} would need a conversion")));
    }
    let post = OutputPost {
        crop: m.crop.map_or([0; 4], |c| [c.top, c.left, c.bottom, c.right]),
        resize: m.resize.map(|r| (r.width, r.height)),
        channels,
        premultiplied: m.color == StudioOutputColor::Premultiplied,
    };
    post.size(comp.width, comp.height)?;
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
    Ok((ExportRecipe { recipe_id: format!("{}.{}", item.item_id, m.module_id), container, video: Some(video), audio: pcm }, post, depth))
}

/// One reel job per output module of `item`, rendering the composition ONCE for all of them
/// (module docs). `scenes` builds the evaluator; `audio` resolves an audio layer's `audio_id` to
/// interleaved stereo f32 at a sample rate; outputs land at `output_root / output_path`. The
/// returned frames handle counts renders (`renders()`).
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
    render_queue_jobs_shared(item, comp, motion, scenes, audio, space, output_root, overwrite).map(|(jobs, _)| jobs)
}

/// [`render_queue_jobs`] plus the shared frames handle (render count for diagnostics).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn render_queue_jobs_shared<S: CompositionScenes + 'static>(
    item: &StudioRenderQueueItem,
    comp: &StudioComposition,
    motion: Option<&StudioMotionRecords>,
    scenes: ScenesFactory<'_, S>,
    audio: AudioResolver<'_>,
    space: ColourTag,
    output_root: &Path,
    overwrite: bool,
) -> Result<(ModuleJobs, Arc<SharedFrames<S>>), MonitorFail> {
    render_queue_build(item, comp, motion, None, scenes, audio, space, output_root, overwrite).map(|b| (b.jobs, b.shared))
}

/// The jobs of a queue item, the shared frames handle and the [`RenderNote`]s of settings that
/// rendered differently from their name.
pub struct RenderQueueBuild<S> {
    pub jobs: ModuleJobs,
    pub shared: Arc<SharedFrames<S>>,
    pub notes: Vec<RenderNote>,
}

/// [`render_queue_jobs_shared`] with the notes; `document_bits` is the document's
/// `bits_per_channel` (what `color_depth: current_settings` takes, [`render_settings`]).
#[allow(clippy::too_many_arguments)]
pub fn render_queue_build<S: CompositionScenes + 'static>(
    item: &StudioRenderQueueItem,
    comp: &StudioComposition,
    motion: Option<&StudioMotionRecords>,
    document_bits: Option<u8>,
    scenes: ScenesFactory<'_, S>,
    audio: AudioResolver<'_>,
    space: ColourTag,
    output_root: &Path,
    overwrite: bool,
) -> Result<RenderQueueBuild<S>, MonitorFail> {
    if item.composition_id != comp.composition_id {
        return Err(refuse("render_queue_composition", "item names another composition"));
    }
    let (options, mut notes) = render_settings(item, comp, document_bits)?;
    let work_area = motion.and_then(|m| m.work_area(&comp.composition_id));
    if item.settings.time_span == StudioTimeSpan::WorkAreaOnly && work_area.is_none() {
        notes.push(note("render_work_area_whole_comp", "no work area is stored for the composition; the whole composition renders"));
    }
    let (in_tick, out_tick, tpf) = item_span(item, comp, work_area)?;
    let span = CompositionSpan { ticks_per_frame: tpf, in_tick, out_tick };
    let frames = span.frames()?;
    let layers = motion.and_then(|m| m.composition_audio(&comp.composition_id)).map_or(&[][..], |a| a.layers.as_slice());
    let mut modules = Vec::with_capacity(item.output_modules.len());
    let mut ids = Vec::with_capacity(item.output_modules.len());
    for m in &item.output_modules {
        let (recipe, post, bit_depth) = module_output(item, comp, m, !layers.is_empty())?;
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
        ids.push(m.module_id.clone());
        modules.push(ModuleOutput {
            recipe,
            post,
            bit_depth,
            chroma: ChromaFormat::Yuv422,
            audio: audio_layers,
            output: output_root.join(&m.output_path),
            overwrite,
            artifact: format!("{} {}", item.item_id, m.module_id),
        });
    }
    let renderer = FrameRenderer::new(scenes()?, space, comp.width, comp.height, tpf, item_motion_blur(item, motion))?.with_options(options)?;
    let (width, height) = renderer.size();
    let consumers = u32::try_from(modules.len()).map_err(|_| refuse("output_modules", "too many output modules"))?;
    let shared = SharedFrames::new(renderer, in_tick, frames, consumers, DEFAULT_SHARED_CACHE_BYTES)?;
    let jobs = shared_module_jobs(&shared, width, height, span, modules)?;
    Ok(RenderQueueBuild { jobs: ids.into_iter().zip(jobs).collect(), shared, notes })
}

/// [`render_queue_jobs_shared`] for the queue item `item_id` stored in a session's document
/// (module docs): the composition and motion records come from the session (validated on read),
/// the evaluator from the session's document; `glyphs` holds the placed glyphs of text-animator
/// layers (`crate::view::text::animated_glyphs`), `pixels` builds the tile pixels of the scenes.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn session_render_queue_jobs<P: ScenePixels + Send + 'static>(
    session: &Session,
    item_id: &str,
    resolver: &dyn Resolver,
    info: &dyn TileInfoSource,
    pixels: &dyn Fn() -> P,
    glyphs: &BTreeMap<String, (Vec<AnimatedGlyph>, [f32; 4])>,
    audio: AudioResolver<'_>,
    space: ColourTag,
    output_root: &Path,
    overwrite: bool,
    cancel: &CancellationToken,
) -> Result<(ModuleJobs, Arc<SharedFrames<TemporalScenes<P>>>), MonitorFail> {
    session_render_queue_build(session, item_id, resolver, info, pixels, glyphs, audio, space, output_root, overwrite, cancel).map(|b| (b.jobs, b.shared))
}

/// [`session_render_queue_jobs`] with the notes: the item's frame-blending switch is applied to the
/// records the evaluator is built from ([`apply_frame_blending_switch`]) and `color_depth:
/// current_settings` takes the document's `bits_per_channel`.
#[allow(clippy::too_many_arguments)]
pub fn session_render_queue_build<P: ScenePixels + Send + 'static>(
    session: &Session,
    item_id: &str,
    resolver: &dyn Resolver,
    info: &dyn TileInfoSource,
    pixels: &dyn Fn() -> P,
    glyphs: &BTreeMap<String, (Vec<AnimatedGlyph>, [f32; 4])>,
    audio: AudioResolver<'_>,
    space: ColourTag,
    output_root: &Path,
    overwrite: bool,
    cancel: &CancellationToken,
) -> Result<RenderQueueBuild<TemporalScenes<P>>, MonitorFail> {
    let session_fail = |e: hsk_studio_session::SessionError| refuse("session_records", e.code());
    let mut compositions = session.composition_records(resolver, cancel).map_err(session_fail)?.ok_or_else(|| refuse("session_records", "no composition records"))?;
    let motion = session.motion_records(resolver, cancel).map_err(session_fail)?.ok_or_else(|| refuse("session_records", "no motion records"))?;
    let item = motion.render_queue.iter().find(|q| q.item_id == item_id).ok_or_else(|| refuse("render_queue_item", item_id.to_owned()))?;
    apply_frame_blending_switch(&mut compositions, item.settings.frame_blending);
    let comp = compositions.composition(&item.composition_id).ok_or_else(|| refuse("render_queue_composition", item.composition_id.clone()))?;
    let snapshot = session.snapshot();
    let mut scenes = || {
        let source = CompositionTemporalSource::new(
            &CompositionInput {
                snapshot,
                expected_revision: snapshot.document().revision,
                records: &compositions,
                composition_id: &comp.composition_id,
                budget: Budget::default(),
                resolver,
                blend_space: space.blend_space,
                info,
                space,
                inputs: RenderInputs::default(),
            },
            cancel,
        )
        .and_then(|s| s.with_motion_records(&motion, glyphs, space, snapshot.document().revision, None))
        .map_err(|e| refuse("session_composition", format!("{e:?}")))?;
        Ok(TemporalScenes { source, pixels: pixels() })
    };
    render_queue_build(item, comp, Some(&motion), Some(snapshot.document().bits_per_channel), &mut scenes, audio, space, output_root, overwrite)
}
