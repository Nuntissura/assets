use crate::block::{InputBlock, OutputBlock};
use crate::dsp::{db_to_linear, per_channel};
use crate::engine::Processor;
use crate::error::ScoreError;
use crate::layout::{ChannelLayout, MAX_CHANNELS, MAX_SAMPLE_RATE_HZ};
use hsk_studio_accord::CancellationToken;
use std::f64::consts::PI;

/// Parametric Equalizer hard ranges (STU-FX-136a `ParametricEQ`): frequency `20..=sample_rate/2`,
/// band amplitude -48..48, Q 0.0001..10000, master gain -96..48. The sheets declare no units
/// (136b); authored units: Hz, dB, ratio, dB.
pub const EQ_FREQUENCY_MIN_HZ: f64 = 20.0;
pub const EQ_AMP_DB: (f64, f64) = (-48.0, 48.0);
pub const EQ_Q: (f64, f64) = (0.0001, 10000.0);
pub const EQ_MASTER_GAIN_DB: (f64, f64) = (-96.0, 48.0);
/// Widest section list accepted (authored bound).
pub const MAX_EQ_SECTIONS: usize = 16;

/// Second-order section shapes (formulas: W3C "Audio EQ Cookbook", Q form).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BiquadKind {
    Peaking,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
}

/// One section. `gain_db` is ignored by `LowPass`/`HighPass`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BiquadSpec {
    pub kind: BiquadKind,
    pub frequency_hz: f64,
    pub gain_db: f64,
    pub q: f64,
}

#[derive(Clone, Copy, Debug)]
struct Coefficients {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl BiquadSpec {
    fn validate(&self, sample_rate_hz: u32) -> Result<(), ScoreError> {
        if !(self.frequency_hz.is_finite() && self.gain_db.is_finite() && self.q.is_finite()) {
            return Err(ScoreError::NotFinite);
        }
        let nyquist = f64::from(sample_rate_hz) / 2.0;
        let gain_ok = matches!(self.kind, BiquadKind::LowPass | BiquadKind::HighPass)
            || (EQ_AMP_DB.0..=EQ_AMP_DB.1).contains(&self.gain_db);
        if !(EQ_FREQUENCY_MIN_HZ..=nyquist).contains(&self.frequency_hz)
            || !gain_ok
            || !(EQ_Q.0..=EQ_Q.1).contains(&self.q)
        {
            return Err(ScoreError::OutOfRange);
        }
        Ok(())
    }

    fn coefficients(&self, sample_rate_hz: u32) -> Coefficients {
        let w0 = 2.0 * PI * self.frequency_hz / f64::from(sample_rate_hz);
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * self.q);
        let a = 10.0_f64.powf(self.gain_db / 40.0);
        let root = 2.0 * a.sqrt() * alpha;
        let (b0, b1, b2, a0, a1, a2) = match self.kind {
            BiquadKind::Peaking => (
                1.0 + alpha * a,
                -2.0 * cos,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos,
                1.0 - alpha / a,
            ),
            BiquadKind::LowShelf => (
                a * ((a + 1.0) - (a - 1.0) * cos + root),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - root),
                (a + 1.0) + (a - 1.0) * cos + root,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - root,
            ),
            BiquadKind::HighShelf => (
                a * ((a + 1.0) + (a - 1.0) * cos + root),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - root),
                (a + 1.0) - (a - 1.0) * cos + root,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - root,
            ),
            BiquadKind::LowPass => (
                (1.0 - cos) / 2.0,
                1.0 - cos,
                (1.0 - cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
            BiquadKind::HighPass => (
                (1.0 + cos) / 2.0,
                -(1.0 + cos),
                (1.0 + cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
        };
        Coefficients {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }
}

impl Coefficients {
    /// `|H(e^{jw})|` evaluated directly from the transfer function.
    fn magnitude(&self, w: f64) -> f64 {
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let num = ((self.b0 + self.b1 * c1 + self.b2 * c2).powi(2)
            + (self.b1 * s1 + self.b2 * s2).powi(2))
        .sqrt();
        let den = ((1.0 + self.a1 * c1 + self.a2 * c2).powi(2)
            + (self.a1 * s1 + self.a2 * s2).powi(2))
        .sqrt();
        num / den
    }
}

/// Cascade of second-order sections plus a master gain, run in `f64` per channel
/// (transposed direct form II). A 0 dB peaking section and 0 dB master are bit-exact identity.
#[derive(Clone, Debug)]
pub struct Equalizer {
    sample_rate_hz: u32,
    channels: usize,
    sections: Vec<Coefficients>,
    state: Vec<[f64; 2]>,
    master: f64,
    identity: String,
}

impl Equalizer {
    pub fn new(
        sample_rate_hz: u32,
        channels: usize,
        sections: &[BiquadSpec],
        master_gain_db: f64,
    ) -> Result<Self, ScoreError> {
        if sample_rate_hz == 0 || sample_rate_hz > MAX_SAMPLE_RATE_HZ {
            return Err(ScoreError::InvalidRate);
        }
        if !(1..=MAX_CHANNELS).contains(&channels) {
            return Err(ScoreError::InvalidLayout);
        }
        if sections.len() > MAX_EQ_SECTIONS {
            return Err(ScoreError::CapacityExceeded);
        }
        if !master_gain_db.is_finite() {
            return Err(ScoreError::NotFinite);
        }
        if !(EQ_MASTER_GAIN_DB.0..=EQ_MASTER_GAIN_DB.1).contains(&master_gain_db) {
            return Err(ScoreError::OutOfRange);
        }
        for spec in sections {
            spec.validate(sample_rate_hz)?;
        }
        let described: Vec<String> = sections
            .iter()
            .map(|s| {
                format!(
                    "{:?}:{:016x}:{:016x}:{:016x}",
                    s.kind,
                    s.frequency_hz.to_bits(),
                    s.gain_db.to_bits(),
                    s.q.to_bits()
                )
            })
            .collect();
        let identity = format!(
            "equalizer:rate={sample_rate_hz};channels={channels};master={:016x};sections=[{}]",
            master_gain_db.to_bits(),
            described.join("|")
        );
        Ok(Self {
            sample_rate_hz,
            channels,
            sections: sections
                .iter()
                .map(|s| s.coefficients(sample_rate_hz))
                .collect(),
            state: vec![[0.0; 2]; sections.len() * channels],
            master: db_to_linear(master_gain_db),
            identity,
        })
    }

    /// Analytic magnitude response of the whole cascade at `frequency_hz`, in dB.
    pub fn magnitude_db(&self, frequency_hz: f64) -> f64 {
        let w = 2.0 * PI * frequency_hz / f64::from(self.sample_rate_hz);
        let linear = self
            .sections
            .iter()
            .fold(self.master, |acc, c| acc * c.magnitude(w));
        20.0 * linear.log10()
    }
}

impl Processor for Equalizer {
    fn identity(&self) -> String {
        self.identity.clone()
    }
    fn latency_samples(&self) -> u32 {
        0
    }
    fn accepts(&self, input: &ChannelLayout, output: &ChannelLayout) -> bool {
        input == output && input.channels() == self.channels
    }
    fn required_sample_rate_hz(&self) -> Option<u32> {
        Some(self.sample_rate_hz)
    }
    fn process(
        &mut self,
        input: &InputBlock<'_>,
        out: &mut OutputBlock,
        cancel: &CancellationToken,
    ) -> Result<(), ScoreError> {
        let (sections, master) = (&self.sections, self.master);
        let state = &mut self.state;
        let count = sections.len();
        per_channel(input, out, cancel, |ch, src, dst| {
            let history = &mut state[ch * count..(ch + 1) * count];
            for (d, s) in dst.iter_mut().zip(src) {
                let mut x = f64::from(*s);
                for (c, z) in sections.iter().zip(history.iter_mut()) {
                    let y = c.b0 * x + z[0];
                    z[0] = c.b1 * x - c.a1 * y + z[1];
                    z[1] = c.b2 * x - c.a2 * y;
                    x = y;
                }
                *d = (x * master) as f32;
            }
        })
    }
    fn reset(&mut self) {
        self.state.fill([0.0; 2]);
    }
}
