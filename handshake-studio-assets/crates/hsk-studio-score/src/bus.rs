//! Session routing with sends, buses, aux returns and a master bus (lane SMB-AUDITION-SESSION;
//! STU-FX-137 "Send: a named routing from a track to a submix with its own level").
//!
//! A [`MixGraph`] holds tracks ([`StripSpec`]: a [`TrackSpec`] with its clip, fades and effect stack,
//! or any [`StripSource`] such as a pulse timeline track, plus a volume and a pan lane, an output and
//! sends), buses ([`BusSpec`]: an effect chain, a volume
//! and a pan lane, an output and sends of their own) and the master ([`MasterSpec`]: an effect chain
//! and a volume lane). An aux return is an ordinary bus whose input comes from sends and whose
//! output returns to the master (or to another bus).
//!
//! **Signals.** A track's pre-fader signal is its clip through its fades and effect stack, latency
//! compensated by a one-track [`Session`] (the track's own volume and pan are taken out of the
//! session and applied here, so the session renders the stack at unity: stereo at centre balance,
//! mono hard left, which is the mono signal itself). Post-fader: `g = 10^(volume_db(t)/20)` and the
//! session pan laws (mono equal-power [`pan_gains`], stereo balance `L min(1, 1 - pan)`,
//! `R min(1, 1 + pan)`). A bus sums its inputs in `f64`, runs its effect chain (its pre-fader
//! signal), then its volume and balance pan (post-fader). A send ([`SendSpec`]) taps the source pre
//! or post fader, scales by `10^(level_db(t)/20)` and pans by the send's own pan (equal-power for a
//! mono pre-fader track signal, balance otherwise). Every lane is evaluated at the timeline sample
//! the signal it scales describes. Bus effects carry no automation here.
//!
//! **Latency compensation.** Track signals describe timeline sample `t` at internal sample `t`
//! (the sessions compensate the track stacks). A bus `b` receives inputs with path latencies
//! `d_i` (0 for tracks, `D(c)` for a bus `c`); each input is delayed by `A(b) - d_i` with
//! `A(b) = max d_i`, so all inputs are aligned, and the bus output has `D(b) = A(b) + L(b)` where
//! `L(b)` is its chain latency. The master does the same with `A(M)` and its chain latency; the
//! graph latency `P = A(M) + L(M)` is removed by a pre-roll of `P` internal samples on the first
//! render (outputs discarded), so output sample `t` is timeline sample `t`, sample-exact on every
//! path. Bus routing must be acyclic (`OutOfRange` otherwise).
//!
//! Blocks render contiguously from sample 0 like [`Session::render`]; an error resets the graph.
use crate::block::{InputBlock, OutputBlock};
use crate::chain::{Chain, ChainStage};
use crate::dsp::db_to_linear;
use crate::engine::Processor;
use crate::error::ScoreError;
use crate::layout::{BlockSpec, ChannelLayout, MAX_BLOCK_FRAMES, ticks_per_sample};
use crate::session::{AutomationLane, MAX_TRACK_LATENCY, MAX_TRACKS, Session, TRACK_PAN, TRACK_VOLUME_DB, TrackSpec, pan_gains};
use hsk_studio_accord::CancellationToken;

/// Most buses in one graph (authored bound).
pub const MAX_BUSES: usize = 64;
/// Most sends per track or bus (authored bound).
pub const MAX_SENDS_PER_SOURCE: usize = 16;
/// Send level lane range in dB (authored, as the track volume range).
pub const SEND_LEVEL_DB: (f64, f64) = TRACK_VOLUME_DB;

/// Where a send taps its source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendTap {
    PreFader,
    PostFader,
}

/// Where a track or bus output goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    Master,
    Bus(usize),
}

/// One send to bus `bus`.
#[derive(Clone, Debug, PartialEq)]
pub struct SendSpec {
    pub bus: usize,
    pub tap: SendTap,
    pub level_db: AutomationLane,
    /// Send pan `-1 ..= 1`.
    pub pan: f64,
}

/// The pre-fader signal of a strip from outside score (lane SMB-AUDITION-SESSION-2: pulse renders a
/// timeline track's clips at unity). Mono sources write the left plane only.
pub trait StripSource: Send {
    /// 1 or 2.
    fn channels(&self) -> usize;
    /// Last clip end on the timeline.
    fn length_samples(&self) -> u64;
    /// Next timeline sample `render` continues from.
    fn position(&self) -> u64;
    /// Moves to timeline `sample` (state rebuilt as the source defines).
    fn seek(&mut self, sample: u64, cancel: &CancellationToken) -> Result<(), ScoreError>;
    /// Renders `left.len()` frames at [`StripSource::position`] and advances it.
    fn render(&mut self, left: &mut [f32], right: &mut [f32], cancel: &CancellationToken) -> Result<(), ScoreError>;
}

/// A one-track [`Session`] as a strip source (seek = reset and render up to the sample).
struct SessionStrip {
    session: Session,
    channels: usize,
    block: usize,
    scratch: [Vec<f32>; 2],
}

impl StripSource for SessionStrip {
    fn channels(&self) -> usize {
        self.channels
    }
    fn length_samples(&self) -> u64 {
        self.session.length_samples()
    }
    fn position(&self) -> u64 {
        self.session.position()
    }
    fn seek(&mut self, sample: u64, cancel: &CancellationToken) -> Result<(), ScoreError> {
        if sample < self.session.position() {
            self.session.reset();
        }
        while self.session.position() < sample {
            let n = (sample - self.session.position()).min(self.block as u64) as usize;
            let at = self.session.position();
            let [l, r] = &mut self.scratch;
            self.session.render(at, &mut l[..n], &mut r[..n], cancel)?;
        }
        Ok(())
    }
    fn render(&mut self, left: &mut [f32], right: &mut [f32], cancel: &CancellationToken) -> Result<(), ScoreError> {
        let at = self.session.position();
        self.session.render(at, left, right, cancel)
    }
}

/// Where a strip's pre-fader signal comes from.
pub enum StripInput {
    /// A score track (clip, fades, effect stack); its own volume and pan lanes are not used (the
    /// strip's are).
    Track(TrackSpec),
    /// An outside source (for example a pulse timeline track rendered at unity).
    Source(Box<dyn StripSource>),
}

/// One track of the graph.
pub struct StripSpec {
    pub input: StripInput,
    pub volume_db: AutomationLane,
    pub pan: AutomationLane,
    pub output: Destination,
    pub sends: Vec<SendSpec>,
}

impl StripSpec {
    /// A score track whose volume and pan lanes become the strip fader and pan.
    pub fn track(mut track: TrackSpec, output: Destination, sends: Vec<SendSpec>) -> Self {
        let volume_db = std::mem::replace(&mut track.volume_db, AutomationLane::constant(0.0));
        let pan = std::mem::replace(&mut track.pan, AutomationLane::constant(0.0));
        Self { input: StripInput::Track(track), volume_db, pan, output, sends }
    }
    /// An outside pre-fader source.
    pub fn source(
        source: Box<dyn StripSource>,
        volume_db: AutomationLane,
        pan: AutomationLane,
        output: Destination,
        sends: Vec<SendSpec>,
    ) -> Self {
        Self { input: StripInput::Source(source), volume_db, pan, output, sends }
    }
}

/// Warm-up rendered before a seek target (bus effect state and delays), in seconds (authored).
pub const GRAPH_SEEK_WARMUP_SECONDS: u64 = 1;

/// One bus (submix or aux return).
pub struct BusSpec {
    pub name: String,
    /// Stereo-to-stereo processors in order (empty: none).
    pub effects: Vec<Box<dyn Processor + Send>>,
    pub volume_db: AutomationLane,
    pub pan: AutomationLane,
    pub output: Destination,
    pub sends: Vec<SendSpec>,
}

impl BusSpec {
    /// A bus with no effects at unity, centre, to the master.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            effects: Vec::new(),
            volume_db: AutomationLane::constant(0.0),
            pan: AutomationLane::constant(0.0),
            output: Destination::Master,
            sends: Vec::new(),
        }
    }
}

/// The master bus.
pub struct MasterSpec {
    pub effects: Vec<Box<dyn Processor + Send>>,
    pub volume_db: AutomationLane,
}

impl Default for MasterSpec {
    fn default() -> Self {
        Self {
            effects: Vec::new(),
            volume_db: AutomationLane::constant(0.0),
        }
    }
}

/// Latencies the graph compensates (samples).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphLatency {
    /// Per bus: (input alignment `A(b)`, chain latency `L(b)`, output path latency `D(b)`).
    pub buses: Vec<(u64, u64, u64)>,
    pub master_alignment: u64,
    pub master_effects: u64,
    /// `P`: the pre-roll removed on the first render.
    pub total: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Strip(usize),
    Bus(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tap {
    Pre,
    Post,
}

/// A fixed delay of `d` frames, stereo.
struct Delay {
    buf: Vec<[f32; 2]>,
    at: usize,
}

impl Delay {
    fn new(d: u64) -> Self {
        Self { buf: vec![[0.0; 2]; d as usize], at: 0 }
    }
    #[inline]
    fn push(&mut self, v: [f32; 2]) -> [f32; 2] {
        if self.buf.is_empty() {
            return v;
        }
        let out = self.buf[self.at];
        self.buf[self.at] = v;
        self.at = (self.at + 1) % self.buf.len();
        out
    }
    fn reset(&mut self) {
        self.buf.fill([0.0; 2]);
        self.at = 0;
    }
}

struct Edge {
    from: Source,
    tap: Tap,
    /// `None`: an output assignment at unity, no pan.
    level_db: Option<AutomationLane>,
    pan: f64,
    to: Option<usize>,
    delay: Delay,
}

struct Strip {
    source: Box<dyn StripSource>,
    mono: bool,
    volume_db: AutomationLane,
    pan: AutomationLane,
    pre: [Vec<f32>; 2],
    post: [Vec<f32>; 2],
}

struct Bus {
    chain: Option<Chain>,
    out: OutputBlock,
    volume_db: AutomationLane,
    pan: AutomationLane,
    /// Output path latency `D(b)`.
    path: u64,
    pre: [Vec<f32>; 2],
    post: [Vec<f32>; 2],
}

/// Tracks, buses and master rendered as one stereo mix (module docs).
pub struct MixGraph {
    rate: u32,
    block: usize,
    strips: Vec<Strip>,
    buses: Vec<Bus>,
    order: Vec<usize>,
    edges: Vec<Edge>,
    master_chain: Option<Chain>,
    master_out: OutputBlock,
    master_volume: AutomationLane,
    master_planes: [Vec<f32>; 2],
    latency: GraphLatency,
    internal: u64,
    position: u64,
    length: u64,
    /// Input sum of the bus being rendered.
    sum: [Vec<f64>; 2],
    scratch: [Vec<f32>; 2],
    discard: [Vec<f32>; 2],
}

fn lane_within(lane: &AutomationLane, range: (f64, f64)) -> Result<(), ScoreError> {
    if lane.points().iter().any(|(_, v)| !v.is_finite()) {
        return Err(ScoreError::NotFinite);
    }
    if lane.points().iter().any(|(_, v)| *v < range.0 || *v > range.1) {
        return Err(ScoreError::OutOfRange);
    }
    Ok(())
}

fn chain(effects: Vec<Box<dyn Processor + Send>>, rate: u32, block: usize) -> Result<Option<Chain>, ScoreError> {
    if effects.is_empty() {
        return Ok(None);
    }
    for p in &effects {
        if p.required_sample_rate_hz().is_some_and(|r| r != rate) {
            return Err(ScoreError::InvalidRate);
        }
    }
    let stages = effects
        .into_iter()
        .map(|processor| ChainStage { processor, output: ChannelLayout::stereo() })
        .collect();
    Chain::new(ChannelLayout::stereo(), block, stages).map(Some)
}

/// Balance-pans a stereo pair.
#[inline]
fn balance(l: f64, r: f64, pan: f64) -> (f64, f64) {
    (l * (1.0 - pan).min(1.0), r * (1.0 + pan).min(1.0))
}

impl MixGraph {
    /// Refusals (all before any rendering): rate not admitted -> `InvalidRate` (also a bus or master
    /// processor bound to another rate); no track, `max_block_frames` 0, a send or output naming a
    /// missing bus, a bus routed to itself or a routing cycle, a lane value or send pan outside its
    /// range -> `OutOfRange`; more than [`MAX_TRACKS`] tracks, [`MAX_BUSES`] buses,
    /// [`MAX_SENDS_PER_SOURCE`] sends on one source, a block above `MAX_BLOCK_FRAMES` or a total
    /// latency above `MAX_TRACK_LATENCY` -> `CapacityExceeded`; a processor that is not stereo to
    /// stereo -> `ChannelMismatch`; non-finite lane values -> `NotFinite`; every refusal of
    /// [`Session::new`] for a track.
    pub fn new(
        sample_rate_hz: u32,
        max_block_frames: usize,
        strips: Vec<StripSpec>,
        buses: Vec<BusSpec>,
        master: MasterSpec,
    ) -> Result<Self, ScoreError> {
        ticks_per_sample(sample_rate_hz)?;
        if strips.is_empty() || max_block_frames == 0 {
            return Err(ScoreError::OutOfRange);
        }
        if strips.len() > MAX_TRACKS || buses.len() > MAX_BUSES || max_block_frames > MAX_BLOCK_FRAMES {
            return Err(ScoreError::CapacityExceeded);
        }
        let nb = buses.len();
        let dest_ok = |d: Destination| match d {
            Destination::Master => true,
            Destination::Bus(i) => i < nb,
        };
        let check_sends = |sends: &[SendSpec]| -> Result<(), ScoreError> {
            if sends.len() > MAX_SENDS_PER_SOURCE {
                return Err(ScoreError::CapacityExceeded);
            }
            for s in sends {
                if s.bus >= nb {
                    return Err(ScoreError::OutOfRange);
                }
                lane_within(&s.level_db, SEND_LEVEL_DB)?;
                if !s.pan.is_finite() {
                    return Err(ScoreError::NotFinite);
                }
                if s.pan < TRACK_PAN.0 || s.pan > TRACK_PAN.1 {
                    return Err(ScoreError::OutOfRange);
                }
            }
            Ok(())
        };
        let mut edges = Vec::new();
        let mut built_strips = Vec::with_capacity(strips.len());
        let mut length = 0u64;
        for (i, s) in strips.into_iter().enumerate() {
            if !dest_ok(s.output) {
                return Err(ScoreError::OutOfRange);
            }
            check_sends(&s.sends)?;
            lane_within(&s.volume_db, TRACK_VOLUME_DB)?;
            lane_within(&s.pan, TRACK_PAN)?;
            let source: Box<dyn StripSource> = match s.input {
                StripInput::Track(mut track) => {
                    let channels = track.clip.len();
                    track.volume_db = AutomationLane::constant(0.0);
                    track.pan = AutomationLane::constant(if channels == 1 { -1.0 } else { 0.0 });
                    let session = Session::new(sample_rate_hz, max_block_frames, vec![track])?;
                    Box::new(SessionStrip {
                        session,
                        channels,
                        block: max_block_frames,
                        scratch: [vec![0.0; max_block_frames], vec![0.0; max_block_frames]],
                    })
                }
                StripInput::Source(source) => source,
            };
            if !(1..=2).contains(&source.channels()) {
                return Err(ScoreError::InvalidLayout);
            }
            let mono = source.channels() == 1;
            let (volume_db, pan) = (s.volume_db, s.pan);
            length = length.max(source.length_samples());
            edges.push((Source::Strip(i), Tap::Post, None, 0.0, s.output));
            for send in s.sends {
                let tap = if send.tap == SendTap::PreFader { Tap::Pre } else { Tap::Post };
                edges.push((Source::Strip(i), tap, Some(send.level_db), send.pan, Destination::Bus(send.bus)));
            }
            built_strips.push(Strip {
                source,
                mono,
                volume_db,
                pan,
                pre: [vec![0.0; max_block_frames], vec![0.0; max_block_frames]],
                post: [vec![0.0; max_block_frames], vec![0.0; max_block_frames]],
            });
        }
        let mut built_buses = Vec::with_capacity(nb);
        let mut bus_latency = Vec::with_capacity(nb);
        for (i, b) in buses.into_iter().enumerate() {
            if !dest_ok(b.output) || b.output == Destination::Bus(i) || b.sends.iter().any(|s| s.bus == i) {
                return Err(ScoreError::OutOfRange);
            }
            check_sends(&b.sends)?;
            lane_within(&b.volume_db, TRACK_VOLUME_DB)?;
            lane_within(&b.pan, TRACK_PAN)?;
            let chain = chain(b.effects, sample_rate_hz, max_block_frames)?;
            bus_latency.push(chain.as_ref().map_or(0, |c| u64::from(c.latency_samples())));
            edges.push((Source::Bus(i), Tap::Post, None, 0.0, b.output));
            for send in b.sends {
                let tap = if send.tap == SendTap::PreFader { Tap::Pre } else { Tap::Post };
                edges.push((Source::Bus(i), tap, Some(send.level_db), send.pan, Destination::Bus(send.bus)));
            }
            built_buses.push(Bus {
                chain,
                out: OutputBlock::with_capacity(ChannelLayout::stereo(), max_block_frames)?,
                volume_db: b.volume_db,
                pan: b.pan,
                path: 0,
                pre: [vec![0.0; max_block_frames], vec![0.0; max_block_frames]],
                post: [vec![0.0; max_block_frames], vec![0.0; max_block_frames]],
            });
        }
        lane_within(&master.volume_db, TRACK_VOLUME_DB)?;
        let master_chain = chain(master.effects, sample_rate_hz, max_block_frames)?;
        // Topological order of the buses (Kahn): bus -> bus edges.
        let mut indegree = vec![0usize; nb];
        for (from, _, _, _, to) in &edges {
            if let (Source::Bus(_), Destination::Bus(t)) = (from, to) {
                indegree[*t] += 1;
            }
        }
        let mut order = Vec::with_capacity(nb);
        let mut ready: Vec<usize> = (0..nb).filter(|b| indegree[*b] == 0).collect();
        while let Some(b) = ready.pop() {
            order.push(b);
            for (from, _, _, _, to) in &edges {
                if let (Source::Bus(f), Destination::Bus(t)) = (from, to)
                    && *f == b
                {
                    indegree[*t] -= 1;
                    if indegree[*t] == 0 {
                        ready.push(*t);
                    }
                }
            }
        }
        if order.len() != nb {
            return Err(ScoreError::OutOfRange);
        }
        // Path latencies in topological order.
        let source_latency = |s: Source, paths: &[u64]| match s {
            Source::Strip(_) => 0,
            Source::Bus(b) => paths[b],
        };
        let mut paths = vec![0u64; nb];
        let mut alignment = vec![0u64; nb];
        for &b in &order {
            let a = edges
                .iter()
                .filter(|e| e.4 == Destination::Bus(b))
                .map(|e| source_latency(e.0, &paths))
                .max()
                .unwrap_or(0);
            alignment[b] = a;
            paths[b] = a.checked_add(bus_latency[b]).ok_or(ScoreError::CapacityExceeded)?;
            if paths[b] > MAX_TRACK_LATENCY {
                return Err(ScoreError::CapacityExceeded);
            }
        }
        let master_alignment = edges
            .iter()
            .filter(|e| e.4 == Destination::Master)
            .map(|e| source_latency(e.0, &paths))
            .max()
            .unwrap_or(0);
        let master_effects = master_chain.as_ref().map_or(0, |c| u64::from(c.latency_samples()));
        let total = master_alignment + master_effects;
        if total > MAX_TRACK_LATENCY {
            return Err(ScoreError::CapacityExceeded);
        }
        for (b, bus) in built_buses.iter_mut().enumerate() {
            bus.path = paths[b];
        }
        let edges = edges
            .into_iter()
            .map(|(from, tap, level_db, pan, to)| {
                let target = match to {
                    Destination::Master => master_alignment,
                    Destination::Bus(t) => alignment[t],
                };
                Edge {
                    from,
                    tap,
                    level_db,
                    pan,
                    to: match to {
                        Destination::Master => None,
                        Destination::Bus(t) => Some(t),
                    },
                    delay: Delay::new(target - source_latency(from, &paths)),
                }
            })
            .collect();
        Ok(Self {
            rate: sample_rate_hz,
            block: max_block_frames,
            strips: built_strips,
            buses: built_buses,
            order,
            edges,
            master_chain,
            master_out: OutputBlock::with_capacity(ChannelLayout::stereo(), max_block_frames)?,
            master_volume: master.volume_db,
            master_planes: [vec![0.0; max_block_frames], vec![0.0; max_block_frames]],
            latency: GraphLatency {
                buses: (0..nb).map(|b| (alignment[b], bus_latency[b], paths[b])).collect(),
                master_alignment,
                master_effects,
                total,
            },
            internal: 0,
            position: 0,
            length,
            sum: [vec![0.0; max_block_frames], vec![0.0; max_block_frames]],
            scratch: [vec![0.0; max_block_frames], vec![0.0; max_block_frames]],
            discard: [vec![0.0; max_block_frames], vec![0.0; max_block_frames]],
        })
    }

    pub fn sample_rate_hz(&self) -> u32 {
        self.rate
    }
    /// Last clip end on the timeline (effect tails may continue past it).
    pub fn length_samples(&self) -> u64 {
        self.length
    }
    /// Next timeline sample `render` expects.
    pub fn position(&self) -> u64 {
        self.position
    }
    pub fn max_block_frames(&self) -> usize {
        self.block
    }
    pub fn latency(&self) -> &GraphLatency {
        &self.latency
    }

    /// Returns to sample 0 and resets every track, bus, effect and delay.
    pub fn reset(&mut self) {
        let none = CancellationToken::default();
        for s in &mut self.strips {
            // Sources rebuild their state at 0; a failure there resurfaces on the next render.
            let _ = s.source.seek(0, &none);
        }
        self.reset_buses();
    }

    fn reset_buses(&mut self) {
        self.internal = 0;
        self.position = 0;
        for b in &mut self.buses {
            if let Some(c) = &mut b.chain {
                c.reset();
            }
        }
        if let Some(c) = &mut self.master_chain {
            c.reset();
        }
        for e in &mut self.edges {
            e.delay.reset();
        }
    }

    /// Moves playback to timeline `sample`: bus and master chains and delays are reset, the strips
    /// seek to `w = sample - P - W` (`W` = [`GRAPH_SEEK_WARMUP_SECONDS`] of samples, clamped at 0)
    /// and the graph renders from `w` up to `sample`, discarding. A seek with `w = 0` is exactly the
    /// continuous render; a later seek is exact when every bus and master effect forgets its input
    /// within `W` samples (delay lines refill within the pre-roll `P`).
    pub fn seek(&mut self, sample: u64, cancel: &CancellationToken) -> Result<(), ScoreError> {
        let warm = u64::from(self.rate).saturating_mul(GRAPH_SEEK_WARMUP_SECONDS);
        let from = sample.saturating_sub(self.latency.total.saturating_add(warm));
        let result = (|| {
            self.reset_buses();
            for s in &mut self.strips {
                s.source.seek(from, cancel)?;
            }
            self.internal = from;
            let target = sample.checked_add(self.latency.total).ok_or(ScoreError::PositionOverflow)?;
            while self.internal < target {
                let n = (target - self.internal).min(self.block as u64) as usize;
                let [mut l, mut r] = std::mem::take(&mut self.discard);
                let out = self.step(n, &mut l[..n], &mut r[..n], cancel);
                self.discard = [l, r];
                out?;
            }
            self.position = sample;
            Ok(())
        })();
        if result.is_err() {
            self.reset();
        }
        result
    }

    /// Renders `left.len()` frames at timeline `start_sample` (refusals as [`Session::render`]).
    pub fn render(&mut self, start_sample: u64, left: &mut [f32], right: &mut [f32], cancel: &CancellationToken) -> Result<(), ScoreError> {
        cancel.check().map_err(|_| ScoreError::Canceled)?;
        if start_sample != self.position {
            return Err(ScoreError::TimelineMismatch);
        }
        let frames = left.len();
        if right.len() != frames {
            return Err(ScoreError::LengthMismatch);
        }
        if frames == 0 {
            return Err(ScoreError::OutOfRange);
        }
        if frames > self.block {
            return Err(ScoreError::CapacityExceeded);
        }
        let result = (|| {
            // Pre-roll: the graph latency, outputs discarded.
            while self.internal < self.latency.total {
                let n = (self.latency.total - self.internal).min(self.block as u64) as usize;
                let [mut l, mut r] = std::mem::take(&mut self.discard);
                let out = self.step(n, &mut l[..n], &mut r[..n], cancel);
                self.discard = [l, r];
                out?;
            }
            self.step(frames, left, right, cancel)
        })();
        match result {
            Ok(()) => {
                self.position += frames as u64;
                Ok(())
            }
            Err(e) => {
                self.reset();
                Err(e)
            }
        }
    }

    /// One internal block of `n` frames at internal sample `self.internal`.
    fn step(&mut self, n: usize, left: &mut [f32], right: &mut [f32], cancel: &CancellationToken) -> Result<(), ScoreError> {
        let q = self.internal;
        // Tracks: pre-fader through the session at unity, then fader and pan.
        for s in &mut self.strips {
            cancel.check().map_err(|_| ScoreError::Canceled)?;
            let [pl, pr] = &mut s.pre;
            if s.source.position() != q {
                s.source.seek(q, cancel)?;
            }
            s.source.render(&mut pl[..n], &mut pr[..n], cancel)?;
            for k in 0..n {
                let t = q + k as u64;
                let g = db_to_linear(s.volume_db.value_at(t));
                let pan = s.pan.value_at(t);
                let (l, r) = if s.mono {
                    let (gl, gr) = pan_gains(pan);
                    let y = f64::from(s.pre[0][k]) * g;
                    (y * gl, y * gr)
                } else {
                    balance(f64::from(s.pre[0][k]) * g, f64::from(s.pre[1][k]) * g, pan)
                };
                s.post[0][k] = l as f32;
                s.post[1][k] = r as f32;
            }
        }
        // Buses in topological order, then the master.
        for oi in 0..=self.order.len() {
            let target = self.order.get(oi).copied();
            self.sum[0][..n].fill(0.0);
            self.sum[1][..n].fill(0.0);
            for e in self.edges.iter_mut().filter(|e| e.to == target) {
                let (pre, post, mono, path) = match e.from {
                    Source::Strip(i) => {
                        let s = &self.strips[i];
                        (&s.pre, &s.post, s.mono, 0u64)
                    }
                    Source::Bus(i) => {
                        let b = &self.buses[i];
                        (&b.pre, &b.post, false, b.path)
                    }
                };
                for k in 0..n {
                    let (l, r) = match &e.level_db {
                        None => (f64::from(post[0][k]), f64::from(post[1][k])),
                        Some(level) => {
                            let t = (q + k as u64).saturating_sub(path);
                            let g = db_to_linear(level.value_at(t));
                            match (e.tap, mono) {
                                (Tap::Pre, true) => {
                                    let (gl, gr) = pan_gains(e.pan);
                                    let y = f64::from(pre[0][k]) * g;
                                    (y * gl, y * gr)
                                }
                                (Tap::Pre, false) => balance(f64::from(pre[0][k]) * g, f64::from(pre[1][k]) * g, e.pan),
                                (Tap::Post, _) => balance(f64::from(post[0][k]) * g, f64::from(post[1][k]) * g, e.pan),
                            }
                        }
                    };
                    let [dl, dr] = e.delay.push([l as f32, r as f32]);
                    self.sum[0][k] += f64::from(dl);
                    self.sum[1][k] += f64::from(dr);
                }
            }
            for k in 0..n {
                self.scratch[0][k] = self.sum[0][k] as f32;
                self.scratch[1][k] = self.sum[1][k] as f32;
            }
            match target {
                Some(b) => {
                    let bus = &mut self.buses[b];
                    run_chain(bus.chain.as_mut(), &mut bus.out, self.rate, q, &self.scratch, n, &mut bus.pre, cancel)?;
                    for k in 0..n {
                        let t = (q + k as u64).saturating_sub(bus.path);
                        let g = db_to_linear(bus.volume_db.value_at(t));
                        let (l, r) = balance(f64::from(bus.pre[0][k]) * g, f64::from(bus.pre[1][k]) * g, bus.pan.value_at(t));
                        bus.post[0][k] = l as f32;
                        bus.post[1][k] = r as f32;
                    }
                }
                None => {
                    run_chain(self.master_chain.as_mut(), &mut self.master_out, self.rate, q, &self.scratch, n, &mut self.master_planes, cancel)?;
                    for k in 0..n {
                        let t = (q + k as u64).saturating_sub(self.latency.total);
                        let g = db_to_linear(self.master_volume.value_at(t));
                        left[k] = (f64::from(self.master_planes[0][k]) * g) as f32;
                        right[k] = (f64::from(self.master_planes[1][k]) * g) as f32;
                    }
                }
            }
        }
        if left.iter().chain(right.iter()).any(|v| !v.is_finite()) {
            return Err(ScoreError::NotFinite);
        }
        self.internal += n as u64;
        Ok(())
    }
}

/// `out_planes[..][..n]` = `chain(input[..][..n])`, or the input when there is no chain.
#[allow(clippy::too_many_arguments)]
fn run_chain(
    chain: Option<&mut Chain>,
    block: &mut OutputBlock,
    rate: u32,
    start: u64,
    input: &[Vec<f32>; 2],
    n: usize,
    out_planes: &mut [Vec<f32>; 2],
    cancel: &CancellationToken,
) -> Result<(), ScoreError> {
    match chain {
        None => {
            out_planes[0][..n].copy_from_slice(&input[0][..n]);
            out_planes[1][..n].copy_from_slice(&input[1][..n]);
        }
        Some(c) => {
            let spec = BlockSpec { sample_rate_hz: rate, layout: ChannelLayout::stereo(), frames: n, start_sample: start };
            let slices = [&input[0][..n], &input[1][..n]];
            c.process(&InputBlock::new(&spec, &slices), block, cancel)?;
            let cap = block.capacity();
            out_planes[0][..n].copy_from_slice(&block.backing()[..n]);
            out_planes[1][..n].copy_from_slice(&block.backing()[cap..cap + n]);
        }
    }
    Ok(())
}
