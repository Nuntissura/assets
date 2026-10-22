//! Program monitor (lane SMB-PULSE-PLAYBACK; STU-VID-023 "a PROGRAM monitor showing the sequence
//! at the playhead" with transport: play, pause, step forward, step back, scrub). Pulse decides
//! which frame is due ([`PlaybackScheduler`]) and what plays in it ([`frame_layers`]: enabled video
//! tracks bottom-to-top, transitions as weighted pairs); the frame is rendered through composite's
//! scene path (`render_scene`) on render-cpu, each layer one full-frame tile at its weight.
//!
//! Decoded source frames arrive through the [`FrameSource`] port (the media decoder is not wired
//! into the harness: a viewer without one shows nothing for a clip and reports
//! `frame_unavailable`). The master clock is any pulse [`AudioClock`]: the score session of the
//! timeline audio (`hsk_studio_pulse::build_timeline_mix`), a [`WallClock`] in the desktop viewer
//! when there is no audio device, or pulse's `HeadlessClock` in tests. Everything here runs
//! without a window (test `tests/viewer_monitor.rs`).
#![deny(clippy::arithmetic_side_effects)]

use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_composite::{
    RenderedScene, SceneComposition, SceneContent, SceneContext, SceneLayer, SceneLimits,
    ScenePixels, SceneTile, render_scene,
};
use hsk_studio_folio::{ClipSource, StudioSequence};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{BlendSpace, StudioBlendMode};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, ColourTag, Exactness, ExecuteRequest, SampleFormat, TilePlanes,
};
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};
pub use hsk_studio_pulse::{
    AudioClock, DropPolicy, FrameAction, FrameLayer, HeadlessClock, PlaySpeed, PlaybackScheduler,
    Transport,
};
use hsk_studio_pulse::{SourceExtents, frame_layers, prefetch_layers};
use hsk_studio_render_cpu::Renderer;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Instant;

/// Stable widget ids of the monitor panel (egui `Id` sources, prefixed like every viewer id).
pub mod widget {
    pub const PANEL: &str = "monitor";
    pub const PLAY: &str = "monitor.play";
    pub const PAUSE: &str = "monitor.pause";
    pub const STEP_BACK: &str = "monitor.step_back";
    pub const STEP_FORWARD: &str = "monitor.step_forward";
    pub const SCRUB: &str = "monitor.scrub";
    pub const FRAME: &str = "monitor.frame";
    /// Lane SMB-PULSE-PLAYBACK-2: J / K / L, play in to out, loop toggle.
    pub const SHUTTLE_LEFT: &str = "monitor.shuttle_left";
    pub const SHUTTLE_STOP: &str = "monitor.shuttle_stop";
    pub const SHUTTLE_RIGHT: &str = "monitor.shuttle_right";
    pub const PLAY_IN_OUT: &str = "monitor.play_in_out";
    pub const LOOP: &str = "monitor.loop";
}

/// Largest monitor raster (pixels).
pub const MAX_MONITOR_PIXELS: u64 = 16 * 1024 * 1024;

/// One decoded source frame: `rgb_f32le` colour and `f32le` straight alpha, tightly packed, in
/// the monitor's plan colour tag, with their SHA-256 (render-cpu verifies them).
#[derive(Clone, Debug, PartialEq)]
pub struct SourceFrame {
    pub width: u32,
    pub height: u32,
    pub colour: Vec<u8>,
    pub alpha: Vec<u8>,
    pub colour_sha256: [u8; 32],
    pub alpha_sha256: [u8; 32],
}

/// Typed port: the decoded frame of `source` at `source_tick`.
pub trait FrameSource {
    fn frame(&self, source: &ClipSource, source_tick: Ticks) -> Option<Arc<SourceFrame>>;
    /// The frame or the decoder's typed refusal (for example `codec_unavailable`); the default
    /// maps a missing frame to `frame_unavailable`.
    fn frame_result(
        &self,
        source: &ClipSource,
        source_tick: Ticks,
    ) -> Result<Arc<SourceFrame>, MonitorFail> {
        self.frame(source, source_tick)
            .ok_or_else(|| fail("frame_unavailable", "no decoded frame"))
    }
    /// Decodes ahead the layers the monitor will present next (bounded by the source); returns
    /// the frames made ready. The default does nothing.
    fn prefetch(&self, _layers: &[FrameLayer], _cancel: &CancellationToken) -> u64 {
        0
    }
}

/// The rendered frame as an 8-bit straight RGBA egui image (lane SMB-PR-SOURCE-MONITOR: shared by
/// the source monitor panel).
pub fn colour_image(image: &RenderedScene) -> egui::ColorImage {
    let (w, h) = (image.width, image.height);
    let mut rgba = Vec::with_capacity((w as usize).saturating_mul(h as usize).saturating_mul(4));
    for y in 0..h {
        for x in 0..w {
            let p = image.pixel(x, y).unwrap_or([0.0; 4]);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            rgba.extend(p.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8));
        }
    }
    egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba)
}

/// Wall-clock master for a viewer without an audio device: samples elapsed since creation.
#[derive(Clone, Copy, Debug)]
pub struct WallClock {
    start: Instant,
    rate_hz: u32,
}
impl WallClock {
    pub fn new(rate_hz: u32) -> Self {
        Self {
            start: Instant::now(),
            rate_hz,
        }
    }
}
impl AudioClock for WallClock {
    fn sample_rate_hz(&self) -> u32 {
        self.rate_hz
    }
    fn sample_position(&self) -> u64 {
        let nanos = self.start.elapsed().as_nanos();
        u64::try_from(nanos.saturating_mul(u128::from(self.rate_hz)) / 1_000_000_000)
            .unwrap_or(u64::MAX)
    }
}

/// Why a monitor frame could not be produced (stable code).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorFail {
    pub code: &'static str,
    pub detail: String,
}

fn fail(code: &'static str, detail: impl Into<String>) -> MonitorFail {
    MonitorFail {
        code,
        detail: detail.into(),
    }
}

/// The frame on screen.
#[derive(Clone, Debug)]
pub struct ProgramFrame {
    pub frame: u64,
    pub tick: Ticks,
    pub image: RenderedScene,
}

/// Program monitor state over one sequence.
pub struct ProgramMonitor {
    sequence: StudioSequence,
    scheduler: PlaybackScheduler,
    width: u32,
    height: u32,
    shown: Option<ProgramFrame>,
    last_error: Option<MonitorFail>,
    renderer: Renderer,
    /// Frames named to the source's prefetch after each presented frame (0 disables).
    prefetch_ahead: usize,
    prefetched: u64,
}

/// Default prefetch distance in frames.
pub const DEFAULT_PREFETCH_AHEAD: usize = 8;

fn space() -> ColourTag {
    ColourTag {
        profile_sha256: [0; 32],
        blend_space: BlendSpace::Encoded,
    }
}

struct Frames(Vec<Arc<SourceFrame>>);
impl ScenePixels for Frames {
    fn tile(&self, tile_id: &str) -> Option<TilePlanes<'_>> {
        let f = self.0.get(tile_id.parse::<usize>().ok()?)?;
        Some(TilePlanes {
            width: f.width,
            height: f.height,
            colour: &f.colour,
            colour_stride_bytes: u64::from(f.width).checked_mul(12)?,
            alpha: &f.alpha,
            alpha_stride_bytes: u64::from(f.width).checked_mul(4)?,
            colour_sha256: f.colour_sha256,
            alpha_sha256: f.alpha_sha256,
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: space(),
        })
    }
}

impl ProgramMonitor {
    /// A paused monitor of `width` x `height` at the start of the play range.
    pub fn new(
        sequence: StudioSequence,
        width: u32,
        height: u32,
        policy: DropPolicy,
    ) -> Result<Self, MonitorFail> {
        let pixels = u64::from(width).saturating_mul(u64::from(height));
        if pixels == 0 || pixels > MAX_MONITOR_PIXELS {
            return Err(fail("monitor_size", format!("{width}x{height}")));
        }
        let scheduler =
            PlaybackScheduler::new(&sequence, policy).map_err(|e| fail("sequence", e.code()))?;
        Ok(Self {
            sequence,
            scheduler,
            width,
            height,
            shown: None,
            last_error: None,
            renderer: Renderer::new(64).map_err(|e| fail("renderer", format!("{e:?}")))?,
            prefetch_ahead: DEFAULT_PREFETCH_AHEAD,
            prefetched: 0,
        })
    }
    /// Sets the prefetch distance; `prefetch_ahead` above pulse's `MAX_PREFETCH_FRAMES`.
    pub fn set_prefetch_ahead(&mut self, frames: usize) -> Result<(), MonitorFail> {
        if frames > hsk_studio_pulse::MAX_PREFETCH_FRAMES {
            return Err(fail("prefetch_ahead", frames.to_string()));
        }
        self.prefetch_ahead = frames;
        Ok(())
    }
    /// Frames made ready by prefetch so far.
    pub fn prefetched(&self) -> u64 {
        self.prefetched
    }
    /// In point and out point (inclusive), whole frames (STU-VID-023).
    pub fn set_in_out(&mut self, in_frame: u64, out_frame: u64) -> Result<(), MonitorFail> {
        self.scheduler
            .set_in_out(in_frame, out_frame)
            .map(|_| ())
            .map_err(|e| fail("in_out", e.code()))
    }
    pub fn play_in_to_out(&mut self, clock: &dyn AudioClock) -> Result<(), MonitorFail> {
        self.scheduler
            .play_in_to_out(clock)
            .map_err(|e| fail("play_in_out", e.code()))
    }
    pub fn set_looping(&mut self, looping: bool, clock: &dyn AudioClock) -> Result<(), MonitorFail> {
        self.scheduler
            .set_looping(looping, clock)
            .map_err(|e| fail("loop", e.code()))
    }
    /// `J` / `L`: shuttle left / right (see pulse `PlaybackScheduler::shuttle_right`).
    pub fn shuttle(&mut self, reverse: bool, clock: &dyn AudioClock) -> Result<PlaySpeed, MonitorFail> {
        let r = if reverse {
            self.scheduler.shuttle_left(clock)
        } else {
            self.scheduler.shuttle_right(clock)
        };
        r.map_err(|e| fail("shuttle", e.code()))
    }
    /// `Shift+J` / `Shift+L`: slow shuttle.
    pub fn shuttle_slow(&mut self, reverse: bool, clock: &dyn AudioClock) -> Result<PlaySpeed, MonitorFail> {
        self.scheduler
            .shuttle_slow(reverse, clock)
            .map_err(|e| fail("shuttle", e.code()))
    }
    /// `K`: stop the shuttle (pause).
    pub fn shuttle_stop(&mut self, clock: &dyn AudioClock) -> Result<(), MonitorFail> {
        self.scheduler
            .shuttle_stop(clock)
            .map_err(|e| fail("shuttle", e.code()))
    }
    pub fn scheduler(&self) -> &PlaybackScheduler {
        &self.scheduler
    }
    /// The sequence the monitor plays.
    pub fn sequence(&self) -> &StudioSequence {
        &self.sequence
    }
    /// Clears the in and out points (lane SMB-PR-SOURCE-MONITOR).
    pub fn clear_in_out(&mut self) {
        self.scheduler.clear_in_out();
    }
    /// Sets the program in point (`is_in`) or out point at the playhead frame, keeping the other
    /// point (an unset one reads as the first or last frame of the play range); `in_after_out` /
    /// `out_before_in` when the marks would cross (lane SMB-PR-SOURCE-MONITOR).
    pub fn mark_at_playhead(&mut self, is_in: bool) -> Result<(), MonitorFail> {
        let s = &self.scheduler;
        let here = s.frame_of(s.playhead());
        let first = s.frame_of(s.range().start());
        let last = s.frame_of(Ticks::new(s.range().end().value().saturating_sub(1)));
        let (cur_in, cur_out) = match s.in_out() {
            Some(r) => (s.frame_of(r.start()), s.frame_of(Ticks::new(r.end().value().saturating_sub(1)))),
            None => (first, last),
        };
        let (i, o) = if is_in { (here, cur_out) } else { (cur_in, here) };
        if o < i {
            return Err(fail(if is_in { "in_after_out" } else { "out_before_in" }, format!("{i}..{o}")));
        }
        self.set_in_out(i, o)
    }
    /// Plays `sequence` from now on (an edit committed elsewhere, lane SMB-PR-SOURCE-MONITOR):
    /// the scheduler is rebuilt paused with the playhead kept (clamped into the new play range),
    /// the in/out span kept when it still fits, and the shown frame dropped so the next refresh
    /// renders the edited frame.
    pub fn replace_sequence(&mut self, sequence: StudioSequence, clock: &dyn AudioClock) -> Result<(), MonitorFail> {
        let mut scheduler = PlaybackScheduler::new(&sequence, self.scheduler.policy())
            .map_err(|e| fail("sequence", e.code()))?;
        if let Some(r) = self.scheduler.in_out() {
            let (i, o) = (self.scheduler.frame_of(r.start()), self.scheduler.frame_of(Ticks::new(r.end().value().saturating_sub(1))));
            let _ = scheduler.set_in_out(i, o);
        }
        let range = scheduler.range();
        let last = Ticks::new(range.end().value().saturating_sub(1));
        let head = self.scheduler.playhead().max(range.start()).min(last);
        scheduler.scrub(head, clock).map_err(|e| fail("scrub", e.code()))?;
        self.sequence = sequence;
        self.scheduler = scheduler;
        self.shown = None;
        Ok(())
    }
    pub fn shown(&self) -> Option<&ProgramFrame> {
        self.shown.as_ref()
    }
    pub fn last_error(&self) -> Option<&MonitorFail> {
        self.last_error.as_ref()
    }
    pub fn play(&mut self, clock: &dyn AudioClock) -> Result<(), MonitorFail> {
        self.scheduler
            .play(clock)
            .map_err(|e| fail("play", e.code()))
    }
    pub fn pause(&mut self, clock: &dyn AudioClock) -> Result<(), MonitorFail> {
        self.scheduler
            .pause(clock)
            .map_err(|e| fail("pause", e.code()))
    }
    pub fn scrub(&mut self, tick: Ticks, clock: &dyn AudioClock) -> Result<(), MonitorFail> {
        self.scheduler
            .scrub(tick, clock)
            .map_err(|e| fail("scrub", e.code()))
    }
    pub fn step(&mut self, frames: i64, clock: &dyn AudioClock) -> Result<Ticks, MonitorFail> {
        self.scheduler
            .step(frames, clock)
            .map_err(|e| fail("step", e.code()))
    }

    /// One presenter refresh: polls the scheduler and renders the frame it presents. Returns the
    /// presented frame index, `None` for hold/end.
    pub fn refresh(
        &mut self,
        clock: &dyn AudioClock,
        sources: &dyn FrameSource,
        extents: &dyn SourceExtents,
        cancel: &CancellationToken,
    ) -> Result<Option<u64>, MonitorFail> {
        let decision = self
            .scheduler
            .poll(clock)
            .map_err(|e| fail("poll", e.code()))?;
        let FrameAction::Present { frame, tick } = decision.action else {
            return Ok(None);
        };
        match self.render_at(tick, sources, extents, cancel) {
            Ok(image) => {
                self.shown = Some(ProgramFrame { frame, tick, image });
                self.last_error = None;
                self.prefetch(sources, extents, cancel);
                Ok(Some(frame))
            }
            Err(e) => {
                self.last_error = Some(e.clone());
                Err(e)
            }
        }
    }

    /// Names the next `prefetch_ahead` presentation frames' layers to the source.
    fn prefetch(&mut self, sources: &dyn FrameSource, extents: &dyn SourceExtents, cancel: &CancellationToken) {
        if self.prefetch_ahead == 0 {
            return;
        }
        let Ok(frames) = self.scheduler.upcoming_frames(self.prefetch_ahead) else {
            return;
        };
        if let Ok(layers) = prefetch_layers(&self.sequence, &self.scheduler, &frames, extents, cancel) {
            self.prefetched = self.prefetched.saturating_add(sources.prefetch(&layers, cancel));
        }
    }

    /// Renders the program frame at `tick` (black opaque background, layers bottom-first). A
    /// source frame of another size is fitted (uniform scale, centred, bilinear).
    pub fn render_at(
        &self,
        tick: Ticks,
        sources: &dyn FrameSource,
        extents: &dyn SourceExtents,
        cancel: &CancellationToken,
    ) -> Result<RenderedScene, MonitorFail> {
        let layers = frame_layers(&self.sequence, tick, extents, cancel)
            .map_err(|e| fail("frame_layers", e.code()))?;
        let mut frames = Vec::with_capacity(layers.len());
        let mut scene_layers = Vec::with_capacity(layers.len());
        for (i, layer) in layers.iter().enumerate() {
            let f = sources
                .frame_result(&layer.source, layer.source_tick)
                .map_err(|e| MonitorFail {
                    code: e.code,
                    detail: format!("{}: {}", layer.clip_id, e.detail),
                })?;
            if f.width == 0 || f.height == 0 {
                return Err(fail("frame_size", format!("{}: empty", layer.clip_id)));
            }
            let matrix = if (f.width, f.height) == (self.width, self.height) {
                Affine2::IDENTITY
            } else {
                let s = (f64::from(self.width) / f64::from(f.width))
                    .min(f64::from(self.height) / f64::from(f.height));
                let tx = (f64::from(self.width) - f64::from(f.width) * s) / 2.0;
                let ty = (f64::from(self.height) - f64::from(f.height) * s) / 2.0;
                Affine2 {
                    m: [s, 0.0, tx, 0.0, s, ty],
                }
            };
            scene_layers.push(SceneLayer {
                layer_id: format!("SLYR-019abcde-0000-7000-8000-{i:012x}"),
                content: SceneContent::Tiles(vec![SceneTile {
                    tile_id: i.to_string(),
                    placement: Rect {
                        x: 0,
                        y: 0,
                        width: f.width,
                        height: f.height,
                    },
                    format: SampleFormat::RgbF32Le,
                    alpha: AlphaConvention::Straight,
                    colour_sha256: f.colour_sha256,
                    alpha_sha256: f.alpha_sha256,
                }]),
                mode: StudioBlendMode::Normal,
                opacity: layer.weight,
                fill_opacity: 1.0,
                matrix,
                matte: None,
                clipped: false,
            });
            frames.push(f);
        }
        let scene = SceneComposition {
            composition_id: "SCOM-019abcde-0000-7000-8000-00000000000a".into(),
            width: self.width,
            height: self.height,
            background: Some([0.0, 0.0, 0.0, 1.0]),
            filter: ResampleFilter::Bilinear,
            layers: scene_layers,
        };
        let pixels = Frames(frames);
        let ctx = SceneContext {
            backend: &self.renderer,
            pixels: &pixels,
            request: ExecuteRequest {
                expected_revision: 0,
                min_exactness: Exactness::Approximate,
                deadline: None,
                max_working_bytes: 1 << 30,
            },
            space: space(),
            limits: SceneLimits::default(),
        };
        render_scene(&scene, &ctx, cancel).map_err(|e| fail("render", format!("{e:?}")))
    }

    /// Structured projection for models and tests.
    pub fn state_json(&self) -> Value {
        let s = &self.scheduler;
        json!({
            "transport": format!("{:?}", s.transport()),
            "playhead_ticks": s.playhead().value(),
            "frame": s.frame_of(s.playhead()),
            "range_ticks": [s.range().start().value(), s.range().end().value()],
            "shown_frame": self.shown.as_ref().map(|f| f.frame),
            "stats": {"presented": s.stats().presented, "dropped": s.stats().dropped, "max_lag_ticks": s.stats().max_lag_ticks, "wraps": s.stats().wraps},
            "speed": [s.speed().numerator(), s.speed().denominator()],
            "looping": s.looping(),
            "in_out_ticks": s.in_out().map(|r| [r.start().value(), r.end().value()]),
            "active_range_ticks": [s.active_range().start().value(), s.active_range().end().value()],
            "prefetched": self.prefetched,
            "error": self.last_error.as_ref().map(|e| e.code),
            "error_detail": self.last_error.as_ref().map(|e| e.detail.clone()),
        })
    }

    /// Draws the transport row (stable ids) and applies the clicked command; returns the drawn ids.
    pub fn ui(&mut self, ui: &mut egui::Ui, clock: &dyn AudioClock) -> Vec<&'static str> {
        let mut drawn = Vec::new();
        let id = |name: &str| egui::Id::new(format!("hsk-studio-ui.{name}"));
        ui.push_id(id(widget::PANEL), |ui| {
            ui.horizontal(|ui| {
                let playing = self.scheduler.transport() == Transport::Playing;
                if ui
                    .add_enabled(!playing, egui::Button::new("Play"))
                    .clicked()
                {
                    let _ = self.play(clock);
                }
                drawn.push(widget::PLAY);
                if ui
                    .add_enabled(playing, egui::Button::new("Pause"))
                    .clicked()
                {
                    let _ = self.pause(clock);
                }
                drawn.push(widget::PAUSE);
                if ui
                    .button("<|")
                    .on_hover_text("Step back one frame")
                    .clicked()
                {
                    let _ = self.step(-1, clock);
                }
                drawn.push(widget::STEP_BACK);
                if ui
                    .button("|>")
                    .on_hover_text("Step forward one frame")
                    .clicked()
                {
                    let _ = self.step(1, clock);
                }
                drawn.push(widget::STEP_FORWARD);
                if ui.button("J").on_hover_text("Shuttle left").clicked() {
                    let _ = self.shuttle(true, clock);
                }
                drawn.push(widget::SHUTTLE_LEFT);
                if ui.button("K").on_hover_text("Stop shuttle").clicked() {
                    let _ = self.shuttle_stop(clock);
                }
                drawn.push(widget::SHUTTLE_STOP);
                if ui.button("L").on_hover_text("Shuttle right").clicked() {
                    let _ = self.shuttle(false, clock);
                }
                drawn.push(widget::SHUTTLE_RIGHT);
                if ui
                    .add_enabled(self.scheduler.in_out().is_some(), egui::Button::new("In-Out"))
                    .on_hover_text("Play in to out")
                    .clicked()
                {
                    let _ = self.play_in_to_out(clock);
                }
                drawn.push(widget::PLAY_IN_OUT);
                let mut looping = self.scheduler.looping();
                if ui.checkbox(&mut looping, "Loop").changed() {
                    let _ = self.set_looping(looping, clock);
                }
                drawn.push(widget::LOOP);
                let range = self.scheduler.range();
                let first = self.scheduler.frame_of(range.start());
                let last = self
                    .scheduler
                    .frame_of(Ticks::new(range.end().value().saturating_sub(1)));
                let mut frame = self.scheduler.frame_of(self.scheduler.playhead());
                let slider = ui.add(egui::Slider::new(&mut frame, first..=last).text("frame"));
                if slider.changed()
                    && let Ok(tick) = self.scheduler.frame_start(frame)
                {
                    let _ = self.scrub(tick, clock);
                }
                drawn.push(widget::SCRUB);
                ui.label(match (&self.shown, &self.last_error) {
                    (_, Some(e)) => format!("{}: {}", e.code, e.detail),
                    (Some(f), None) => format!("frame {}", f.frame),
                    (None, None) => "no frame".to_owned(),
                });
                drawn.push(widget::FRAME);
            });
        });
        drawn
    }
}
