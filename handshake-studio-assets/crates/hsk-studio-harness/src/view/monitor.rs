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
    AudioClock, DropPolicy, FrameAction, HeadlessClock, PlaybackScheduler, Transport,
};
use hsk_studio_pulse::{SourceExtents, frame_layers};
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
}

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
        })
    }
    pub fn scheduler(&self) -> &PlaybackScheduler {
        &self.scheduler
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
                Ok(Some(frame))
            }
            Err(e) => {
                self.last_error = Some(e.clone());
                Err(e)
            }
        }
    }

    /// Renders the program frame at `tick` (black opaque background, layers bottom-first).
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
                .frame(&layer.source, layer.source_tick)
                .ok_or_else(|| fail("frame_unavailable", layer.clip_id.clone()))?;
            if (f.width, f.height) != (self.width, self.height) {
                return Err(fail(
                    "frame_size",
                    format!("{}: {}x{}", layer.clip_id, f.width, f.height),
                ));
            }
            scene_layers.push(SceneLayer {
                layer_id: format!("SLYR-019abcde-0000-7000-8000-{i:012x}"),
                content: SceneContent::Tiles(vec![SceneTile {
                    tile_id: i.to_string(),
                    placement: Rect {
                        x: 0,
                        y: 0,
                        width: self.width,
                        height: self.height,
                    },
                    format: SampleFormat::RgbF32Le,
                    alpha: AlphaConvention::Straight,
                    colour_sha256: f.colour_sha256,
                    alpha_sha256: f.alpha_sha256,
                }]),
                mode: StudioBlendMode::Normal,
                opacity: layer.weight,
                fill_opacity: 1.0,
                matrix: Affine2::IDENTITY,
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
            "stats": {"presented": s.stats().presented, "dropped": s.stats().dropped, "max_lag_ticks": s.stats().max_lag_ticks},
            "error": self.last_error.as_ref().map(|e| e.code),
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
                ui.label(match &self.shown {
                    Some(f) => format!("frame {}", f.frame),
                    None => "no frame".to_owned(),
                });
                drawn.push(widget::FRAME);
            });
        });
        drawn
    }
}
