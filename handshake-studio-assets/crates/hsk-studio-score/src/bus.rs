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
use crate::dsp::db_to_linear;
use crate::loudness::{LoudnessMeter, LoudnessSnapshot};
use crate::engine::Processor;
use crate::error::ScoreError;
use crate::layout::{BlockSpec, ChannelLayout, MAX_BLOCK_FRAMES, ticks_per_sample};
use crate::session::{
    AutomationLane, EffectSlot, MAX_TRACK_LATENCY, MAX_TRACKS, Session, TRACK_PAN, TRACK_VOLUME_DB, TrackSpec, pan_gains,
};
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

/// How [`MixGraph::seek`] rebuilds bus and master effect state (lane SMB-AUDITION-UI).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SeekMode {
    /// The state the continuous render would have: a seek forward renders on from the current
    /// position, a seek backward renders again from sample 0 (pre-roll = the whole history), so the
    /// output after any seek is the continuous render bit for bit, whatever the effects' tails.
    #[default]
    Exact,
    /// Fast: reset, then render a pre-roll of `P + seconds` before the target; exact only when every
    /// bus and master effect forgets its input within `seconds`.
    Preroll { seconds: u64 },
}

/// Checkpoints of exact seeks (lane SMB-AUDITION-UI-2): during forward rendering the graph saves
/// the state of every bus and master stage (`Processor::save_state`) and of every alignment delay
/// at least every `interval_frames` of timeline, keeping the total under `budget_bytes`: when a new
/// checkpoint would exceed it, every second checkpoint is dropped and the interval doubles. An
/// exact seek resumes from the nearest checkpoint at or before the target (restore, strips seek to
/// the checkpoint, render on) instead of re-rendering from sample 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckpointPolicy {
    pub interval_frames: u64,
    pub budget_bytes: usize,
}

impl CheckpointPolicy {
    /// One checkpoint per second of timeline, 64 MiB (authored defaults).
    pub fn for_rate(sample_rate_hz: u32) -> Self {
        Self { interval_frames: u64::from(sample_rate_hz), budget_bytes: 64 << 20 }
    }
}

/// What the checkpoints hold right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckpointStats {
    /// Every bus and master stage supports checkpoints (else exact seeks backward re-render from 0).
    pub enabled: bool,
    pub count: usize,
    pub bytes: usize,
    /// Current interval (the policy interval doubled once per thinning).
    pub interval_frames: u64,
    /// The most frames an exact seek renders after resuming: `interval_frames + max_block_frames`
    /// once a checkpoint at or before the target exists (before the first checkpoint, the distance
    /// from sample 0).
    pub seek_bound_frames: u64,
    /// Frames the last exact seek rendered (restore or reset to target).
    pub last_seek_frames: u64,
}

struct Checkpoint {
    position: u64,
    internal: u64,
    buses: Vec<Vec<Vec<u8>>>,
    master: Vec<Vec<u8>>,
    delays: Vec<(Vec<[f32; 2]>, usize)>,
    bytes: usize,
}

/// Meter readings of the graph since the last [`MixGraph::reset_meters`] (seek and reset clear
/// them): per strip and bus the post-fader sample peak per channel, the master output sample peak,
/// and the master output loudness (score's BS.1770 meter, stereo weights 1, 1).
#[derive(Clone, Debug, PartialEq)]
pub struct GraphMeters {
    pub strips: Vec<[f32; 2]>,
    pub buses: Vec<[f32; 2]>,
    pub master: [f32; 2],
    pub loudness: LoudnessSnapshot,
}

/// One bus (submix or aux return).
pub struct BusSpec {
    pub name: String,
    /// Stereo-to-stereo stages in order (empty: none), each with its automation lanes (lane
    /// SMB-AUDITION-UI: bus effect keyframes), evaluated at the timeline sample the stage reads.
    pub effects: Vec<EffectSlot>,
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
    pub effects: Vec<EffectSlot>,
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
    stack: Stack,
    /// Input alignment `A(b)`: the bus input at internal sample `q` is timeline `q - A(b)`.
    align: u64,
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
    master_stack: Stack,
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
    seek_mode: SeekMode,
    policy: CheckpointPolicy,
    interval: u64,
    checkpoints: Vec<Checkpoint>,
    checkpoint_bytes: usize,
    checkpointable: bool,
    next_checkpoint: u64,
    last_seek_frames: u64,
    metering: bool,
    strip_peaks: Vec<[f32; 2]>,
    bus_peaks: Vec<[f32; 2]>,
    master_peak: [f32; 2],
    meter: LoudnessMeter,
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

struct StackStage {
    slot: EffectSlot,
    /// Latency of the stages before this one.
    offset: u64,
    spec: BlockSpec,
    out: OutputBlock,
    last: Vec<f64>,
}

/// An automatable stereo effect stack with stage latencies (the bus / master counterpart of a
/// session track stack: lanes are set before each sample their value changes at, evaluated at the
/// timeline sample the stage reads, the stages run on the sub-blocks between changes).
struct Stack {
    stages: Vec<StackStage>,
    latency: u64,
}

impl Stack {
    fn new(slots: Vec<EffectSlot>, rate: u32, block: usize) -> Result<Self, ScoreError> {
        let layout = ChannelLayout::stereo();
        let mut stages = Vec::with_capacity(slots.len());
        let mut offset = 0u64;
        for slot in slots {
            let p = &slot.processor;
            if p.required_sample_rate_hz().is_some_and(|r| r != rate) {
                return Err(ScoreError::InvalidRate);
            }
            if !p.accepts(&layout, &layout) {
                return Err(ScoreError::ChannelMismatch);
            }
            if p.max_frames().is_some_and(|m| m < block) {
                return Err(ScoreError::OutOfRange);
            }
            let params = p.automation_parameters();
            for (index, lane) in &slot.lanes {
                let param = params.get(*index).ok_or(ScoreError::OutOfRange)?;
                lane_within(lane, (param.min, param.max))?;
            }
            let latency = u64::from(p.latency_samples());
            stages.push(StackStage {
                last: vec![f64::NAN; slot.lanes.len()],
                slot,
                offset,
                spec: BlockSpec { sample_rate_hz: rate, layout: layout.clone(), frames: 1, start_sample: 0 },
                out: OutputBlock::with_capacity(layout.clone(), block)?,
            });
            offset = offset.checked_add(latency).ok_or(ScoreError::CapacityExceeded)?;
        }
        Ok(Self { stages, latency: offset })
    }

    fn reset(&mut self) {
        for s in &mut self.stages {
            s.slot.processor.reset();
            s.last.fill(f64::NAN);
        }
    }

    fn checkpointable(&self) -> bool {
        self.stages.iter().all(|s| s.slot.processor.save_state().is_some())
    }

    fn save(&self) -> Option<Vec<Vec<u8>>> {
        self.stages.iter().map(|s| s.slot.processor.save_state()).collect()
    }

    /// Restores every stage; lanes are re-applied on the next run (`last` = NaN).
    fn restore(&mut self, states: &[Vec<u8>]) -> Result<(), ScoreError> {
        if states.len() != self.stages.len() {
            return Err(ScoreError::LengthMismatch);
        }
        for (s, state) in self.stages.iter_mut().zip(states) {
            s.slot.processor.restore_state(state)?;
            s.last.fill(f64::NAN);
        }
        Ok(())
    }

    /// `out[..][..n]` = the stack over `input[..][..n]`, whose first frame is timeline `start`.
    fn run(&mut self, start: u64, input: &[Vec<f32>; 2], n: usize, out: &mut [Vec<f32>; 2], cancel: &CancellationToken) -> Result<(), ScoreError> {
        if self.stages.is_empty() {
            out[0][..n].copy_from_slice(&input[0][..n]);
            out[1][..n].copy_from_slice(&input[1][..n]);
            return Ok(());
        }
        let end = start + n as u64;
        let mut at = 0usize;
        while at < n {
            cancel.check().map_err(|_| ScoreError::Canceled)?;
            let t = start + at as u64;
            let mut stop = end;
            for stage in &mut self.stages {
                let u = t.saturating_sub(stage.offset);
                for (k, (index, lane)) in stage.slot.lanes.iter().enumerate() {
                    let v = lane.value_at(u);
                    if v.to_bits() != stage.last[k].to_bits() {
                        stage.slot.processor.set_automated(*index, v)?;
                        stage.last[k] = v;
                    }
                    stop = stop.min(stage.offset.saturating_add(lane.next_change(u)));
                }
            }
            let sub = (stop - t) as usize;
            for k in 0..self.stages.len() {
                let (before, rest) = self.stages.split_at_mut(k);
                let stage = &mut rest[0];
                stage.spec.frames = sub;
                stage.spec.start_sample = t.saturating_sub(stage.offset);
                let slices: [&[f32]; 2] = match before.last() {
                    None => [&input[0][at..at + sub], &input[1][at..at + sub]],
                    Some(prev) => {
                        let cap = prev.out.capacity();
                        let b = prev.out.backing();
                        [&b[..sub], &b[cap..cap + sub]]
                    }
                };
                let block = InputBlock::new(&stage.spec, &slices);
                stage.slot.processor.process(&block, &mut stage.out, cancel)?;
            }
            if let Some(last) = self.stages.last() {
                let cap = last.out.capacity();
                out[0][at..at + sub].copy_from_slice(&last.out.backing()[..sub]);
                out[1][at..at + sub].copy_from_slice(&last.out.backing()[cap..cap + sub]);
            }
            at += sub;
        }
        Ok(())
    }
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
            let stack = Stack::new(b.effects, sample_rate_hz, max_block_frames)?;
            bus_latency.push(stack.latency);
            edges.push((Source::Bus(i), Tap::Post, None, 0.0, b.output));
            for send in b.sends {
                let tap = if send.tap == SendTap::PreFader { Tap::Pre } else { Tap::Post };
                edges.push((Source::Bus(i), tap, Some(send.level_db), send.pan, Destination::Bus(send.bus)));
            }
            built_buses.push(Bus {
                stack,
                align: 0,
                volume_db: b.volume_db,
                pan: b.pan,
                path: 0,
                pre: [vec![0.0; max_block_frames], vec![0.0; max_block_frames]],
                post: [vec![0.0; max_block_frames], vec![0.0; max_block_frames]],
            });
        }
        lane_within(&master.volume_db, TRACK_VOLUME_DB)?;
        let master_stack = Stack::new(master.effects, sample_rate_hz, max_block_frames)?;
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
        let master_effects = master_stack.latency;
        let total = master_alignment + master_effects;
        if total > MAX_TRACK_LATENCY {
            return Err(ScoreError::CapacityExceeded);
        }
        for (b, bus) in built_buses.iter_mut().enumerate() {
            bus.path = paths[b];
            bus.align = alignment[b];
        }
        let strip_count = built_strips.len();
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
        let mut graph = Self {
            rate: sample_rate_hz,
            block: max_block_frames,
            strips: built_strips,
            buses: built_buses,
            order,
            edges,
            master_stack,
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
            seek_mode: SeekMode::Exact,
            policy: CheckpointPolicy::for_rate(sample_rate_hz),
            interval: u64::from(sample_rate_hz).max(1),
            checkpoints: Vec::new(),
            checkpoint_bytes: 0,
            checkpointable: false,
            next_checkpoint: 0,
            last_seek_frames: 0,
            metering: false,
            strip_peaks: vec![[0.0; 2]; strip_count],
            bus_peaks: vec![[0.0; 2]; nb],
            master_peak: [0.0; 2],
            meter: LoudnessMeter::new(sample_rate_hz, &[1.0, 1.0])?,
        };
        graph.init_checkpoints();
        Ok(graph)
    }

    fn init_checkpoints(&mut self) {
        self.checkpointable = self.buses.iter().all(|b| b.stack.checkpointable()) && self.master_stack.checkpointable();
        self.next_checkpoint = self.interval;
    }

    /// Replaces the checkpoint policy (drops the checkpoints taken so far).
    pub fn set_checkpoint_policy(&mut self, policy: CheckpointPolicy) {
        self.policy = policy;
        self.interval = policy.interval_frames.max(1);
        self.checkpoints.clear();
        self.checkpoint_bytes = 0;
        self.next_checkpoint = self.position.saturating_add(self.interval);
    }

    pub fn checkpoint_stats(&self) -> CheckpointStats {
        CheckpointStats {
            enabled: self.checkpointable,
            count: self.checkpoints.len(),
            bytes: self.checkpoint_bytes,
            interval_frames: self.interval,
            seek_bound_frames: self.interval.saturating_add(self.block as u64),
            last_seek_frames: self.last_seek_frames,
        }
    }

    /// Saves the current state as a checkpoint at `self.position` (module docs of
    /// [`CheckpointPolicy`]); called before a render block once the position reaches the next
    /// checkpoint.
    fn take_checkpoint(&mut self) {
        let buses: Option<Vec<Vec<Vec<u8>>>> = self.buses.iter().map(|b| b.stack.save()).collect();
        let (Some(buses), Some(master)) = (buses, self.master_stack.save()) else {
            self.checkpointable = false;
            return;
        };
        let delays: Vec<(Vec<[f32; 2]>, usize)> = self.edges.iter().map(|e| (e.delay.buf.clone(), e.delay.at)).collect();
        let bytes = buses.iter().flatten().chain(master.iter()).map(Vec::len).sum::<usize>()
            + delays.iter().map(|(b, _)| b.len() * 8).sum::<usize>();
        if bytes > self.policy.budget_bytes {
            // One checkpoint does not fit the budget: checkpoints are off for this graph.
            self.checkpointable = false;
            self.checkpoints.clear();
            self.checkpoint_bytes = 0;
            return;
        }
        while self.checkpoint_bytes + bytes > self.policy.budget_bytes && self.checkpoints.len() > 1 {
            let mut keep = 0usize;
            let mut i = 0usize;
            self.checkpoints.retain(|_| {
                let k = i.is_multiple_of(2);
                i += 1;
                k
            });
            for c in &self.checkpoints {
                keep += c.bytes;
            }
            self.checkpoint_bytes = keep;
            self.interval = self.interval.saturating_mul(2);
        }
        if self.checkpoint_bytes + bytes > self.policy.budget_bytes {
            self.checkpoints.clear();
            self.checkpoint_bytes = 0;
        }
        let at = self.checkpoints.partition_point(|c| c.position < self.position);
        if self.checkpoints.get(at).is_some_and(|c| c.position == self.position) {
            return;
        }
        self.checkpoints.insert(at, Checkpoint { position: self.position, internal: self.internal, buses, master, delays, bytes });
        self.checkpoint_bytes += bytes;
    }

    /// Resumes from checkpoint `index`.
    fn restore_checkpoint(&mut self, index: usize, cancel: &CancellationToken) -> Result<(), ScoreError> {
        self.reset_buses();
        let cp = &self.checkpoints[index];
        for (bus, states) in self.buses.iter_mut().zip(&cp.buses) {
            bus.stack.restore(states)?;
        }
        self.master_stack.restore(&cp.master)?;
        for (e, (buf, at)) in self.edges.iter_mut().zip(&cp.delays) {
            if e.delay.buf.len() != buf.len() {
                return Err(ScoreError::LengthMismatch);
            }
            e.delay.buf.copy_from_slice(buf);
            e.delay.at = *at;
        }
        let (internal, position) = (cp.internal, cp.position);
        for s in &mut self.strips {
            s.source.seek(internal, cancel)?;
        }
        self.internal = internal;
        self.position = position;
        self.next_checkpoint = position.saturating_add(self.interval);
        Ok(())
    }

    /// The seek behaviour ([`SeekMode::Exact`] by default).
    pub fn set_seek_mode(&mut self, mode: SeekMode) {
        self.seek_mode = mode;
    }
    pub fn seek_mode(&self) -> SeekMode {
        self.seek_mode
    }

    /// Meter readings since the last reset (see [`GraphMeters`]).
    pub fn meters(&self) -> GraphMeters {
        GraphMeters {
            strips: self.strip_peaks.clone(),
            buses: self.bus_peaks.clone(),
            master: self.master_peak,
            loudness: self.meter.snapshot(),
        }
    }

    /// Clears the peaks and the loudness measurement.
    pub fn reset_meters(&mut self) {
        self.strip_peaks.iter_mut().for_each(|p| *p = [0.0; 2]);
        self.bus_peaks.iter_mut().for_each(|p| *p = [0.0; 2]);
        self.master_peak = [0.0; 2];
        self.meter.reset();
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
        self.next_checkpoint = self.interval;
        for b in &mut self.buses {
            b.stack.reset();
        }
        self.master_stack.reset();
        for e in &mut self.edges {
            e.delay.reset();
        }
        self.reset_meters();
    }

    /// Moves playback to timeline `sample` per the [`SeekMode`]; meters restart there.
    /// `Exact`: forward renders on (discarding) from the current position, backward resets and
    /// renders from sample 0, so effect tails of any length are the continuous render's.
    /// `Preroll { seconds }`: bus and master stacks and delays are reset, the strips seek to
    /// `w = sample - P - W` (`W` = `seconds` of samples, clamped at 0) and the graph renders from
    /// `w` up to `sample`, discarding; exact when `w = 0` or every effect forgets its input within
    /// `W` samples (delay lines refill within the pre-roll `P`).
    pub fn seek(&mut self, sample: u64, cancel: &CancellationToken) -> Result<(), ScoreError> {
        let seconds = match self.seek_mode {
            SeekMode::Preroll { seconds } => seconds,
            SeekMode::Exact => {
                let result = (|| {
                    // The nearest checkpoint at or before the target, when it is closer than the
                    // current position (or the target is behind it).
                    let cp = self.checkpoints.partition_point(|c| c.position <= sample).checked_sub(1);
                    let ahead = sample >= self.position;
                    match cp {
                        Some(i) if !ahead || self.checkpoints[i].position > self.position => self.restore_checkpoint(i, cancel)?,
                        None if !ahead => self.reset(),
                        _ => {}
                    }
                    let (mut l, mut r) = (vec![0.0f32; self.block], vec![0.0f32; self.block]);
                    self.last_seek_frames = sample.saturating_sub(self.position);
                    while self.position < sample {
                        let n = (sample - self.position).min(self.block as u64) as usize;
                        let at = self.position;
                        self.render(at, &mut l[..n], &mut r[..n], cancel)?;
                    }
                    Ok(())
                })();
                if result.is_err() {
                    self.reset();
                }
                self.reset_meters();
                return result;
            }
        };
        let warm = u64::from(self.rate).saturating_mul(seconds);
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
        self.reset_meters();
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
        if self.checkpointable && self.position >= self.next_checkpoint && self.position > 0 {
            self.take_checkpoint();
            self.next_checkpoint = self.position.saturating_add(self.interval);
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
            self.metering = true;
            let out = self.step(frames, left, right, cancel);
            self.metering = false;
            out
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
        if self.metering {
            for (s, peak) in self.strips.iter().zip(&mut self.strip_peaks) {
                for (p, post) in peak.iter_mut().zip(&s.post) {
                    *p = post[..n].iter().fold(*p, |m, v| m.max(v.abs()));
                }
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
                    bus.stack.run(q.saturating_sub(bus.align), &self.scratch, n, &mut bus.pre, cancel)?;
                    for k in 0..n {
                        let t = (q + k as u64).saturating_sub(bus.path);
                        let g = db_to_linear(bus.volume_db.value_at(t));
                        let (l, r) = balance(f64::from(bus.pre[0][k]) * g, f64::from(bus.pre[1][k]) * g, bus.pan.value_at(t));
                        bus.post[0][k] = l as f32;
                        bus.post[1][k] = r as f32;
                    }
                    if self.metering {
                        let peak = &mut self.bus_peaks[b];
                        for (p, post) in peak.iter_mut().zip(&bus.post) {
                            *p = post[..n].iter().fold(*p, |m, v| m.max(v.abs()));
                        }
                    }
                }
                None => {
                    let start = q.saturating_sub(self.latency.master_alignment);
                    self.master_stack.run(start, &self.scratch, n, &mut self.master_planes, cancel)?;
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
        if self.metering {
            self.master_peak[0] = left.iter().fold(self.master_peak[0], |m, v| m.max(v.abs()));
            self.master_peak[1] = right.iter().fold(self.master_peak[1], |m, v| m.max(v.abs()));
            self.meter.feed(&[&*left, &*right])?;
        }
        self.internal += n as u64;
        Ok(())
    }
}

