//! AAC filterbank (ISO/IEC 14496-3 subclause 4.6.11): IMDCT of 2048 / 256 points through a DCT-IV
//! computed with an N/4-point complex FFT (f64), sine and Kaiser-Bessel-derived windows (alpha 4 long,
//! 6 short), the four window sequences and overlap-add; plus the TNS all-pole filter (4.6.9.3).
//! Studio-owned. All loops run over fixed sizes (1024 / 128 coefficient frames, orders <= 20), no length
//! comes from the bitstream, so integer index arithmetic here cannot overflow.
#![allow(clippy::arithmetic_side_effects, clippy::needless_range_loop)]

use std::f64::consts::PI;

/// Radix-2 complex FFT (forward, exp(-2 pi i nk / n)) of a fixed power-of-two size.
pub(crate) struct Fft {
    n: usize,
    twiddle: Vec<(f64, f64)>,
    rev: Vec<usize>,
}

impl Fft {
    pub(crate) fn new(n: usize) -> Self {
        let bits = n.trailing_zeros();
        let rev = (0..n)
            .map(|i| {
                if bits == 0 {
                    0
                } else {
                    i.reverse_bits() >> (usize::BITS - bits)
                }
            })
            .collect();
        let twiddle = (0..n / 2)
            .map(|k| {
                let a = -2.0 * PI * k as f64 / n as f64;
                (a.cos(), a.sin())
            })
            .collect();
        Self { n, twiddle, rev }
    }

    pub(crate) fn run(&self, re: &mut [f64], im: &mut [f64]) {
        let n = self.n;
        for i in 0..n {
            let j = self.rev[i];
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let step = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..len / 2 {
                    let (wr, wi) = self.twiddle[k * step];
                    let (a, b) = (start + k, start + k + len / 2);
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            len *= 2;
        }
    }
}

/// IMDCT of `2 * m` outputs from `m` coefficients, scaled by 2 / N (ISO/IEC 14496-3 4.6.11.3.1).
pub(crate) struct Imdct {
    m: usize,
    fft: Fft,
    pre: Vec<(f64, f64)>,
    post: Vec<(f64, f64)>,
    re: Vec<f64>,
    im: Vec<f64>,
    u: Vec<f64>,
}

impl Imdct {
    pub(crate) fn new(m: usize) -> Self {
        let h = m / 2;
        let tw = |a: f64| (a.cos(), a.sin());
        Self {
            m,
            fft: Fft::new(h),
            pre: (0..h)
                .map(|n| tw(-PI * (n as f64 + 0.25) / m as f64))
                .collect(),
            post: (0..h).map(|k| tw(-PI * k as f64 / m as f64)).collect(),
            re: vec![0.0; h],
            im: vec![0.0; h],
            u: vec![0.0; m],
        }
    }

    /// `x`: m coefficients; `y`: 2m outputs.
    pub(crate) fn run(&mut self, x: &[f32], y: &mut [f64]) {
        let m = self.m;
        let h = m / 2;
        for n in 0..h {
            let (a, b) = (f64::from(x[2 * n]), f64::from(x[m - 1 - 2 * n]));
            let (c, s) = self.pre[n];
            self.re[n] = a * c - b * s;
            self.im[n] = a * s + b * c;
        }
        self.fft.run(&mut self.re, &mut self.im);
        for k in 0..h {
            let (c, s) = self.post[k];
            let (a, b) = (self.re[k], self.im[k]);
            self.u[2 * k] = a * c - b * s;
            self.u[m - 1 - 2 * k] = -(a * s + b * c);
        }
        let scale = 2.0 / (2 * m) as f64;
        let half = m / 2;
        for n in 0..half {
            y[n] = self.u[half + n] * scale;
        }
        for n in half..3 * half {
            y[n] = -self.u[3 * half - 1 - n] * scale;
        }
        for n in 3 * half..2 * m {
            y[n] = -self.u[n - 3 * half] * scale;
        }
    }
}

fn sine_window(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| (PI / n as f64 * (i as f64 + 0.5)).sin())
        .collect()
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    for k in 1..100 {
        let r = x / (2.0 * k as f64);
        term *= r * r;
        sum += term;
        if term < 1e-16 * sum {
            break;
        }
    }
    sum
}

/// Kaiser-Bessel-derived window of length `n` (4.6.11.3.2).
fn kbd_window(n: usize, alpha: f64) -> Vec<f64> {
    let half = n / 2;
    let kernel: Vec<f64> = (0..=half)
        .map(|i| {
            let r = (i as f64 - half as f64 / 2.0) / (half as f64 / 2.0);
            bessel_i0(PI * alpha * (1.0 - r * r).max(0.0).sqrt())
        })
        .collect();
    let total: f64 = kernel.iter().sum();
    let mut w = vec![0.0; n];
    let mut acc = 0.0;
    for i in 0..half {
        acc += kernel[i];
        w[i] = (acc / total).sqrt();
        w[n - 1 - i] = w[i];
    }
    w
}

pub(crate) const LONG_START: u8 = 1;
pub(crate) const EIGHT_SHORT: u8 = 2;
pub(crate) const LONG_STOP: u8 = 3;

pub(crate) struct Filterbank {
    long: [Vec<f64>; 2],
    short: [Vec<f64>; 2],
    imdct_long: Imdct,
    imdct_short: Imdct,
    buf: Vec<f64>,
    frame: Vec<f64>,
}

impl Filterbank {
    pub(crate) fn new() -> Self {
        Self {
            long: [sine_window(2048), kbd_window(2048, 4.0)],
            short: [sine_window(256), kbd_window(256, 6.0)],
            imdct_long: Imdct::new(1024),
            imdct_short: Imdct::new(128),
            buf: vec![0.0; 2048],
            frame: vec![0.0; 2048],
        }
    }

    /// Synthesize one channel frame: `spec` (1024 coefficients, window-major for EIGHT_SHORT),
    /// `overlap` (1024, updated), `out` (1024 PCM in 16-bit units).
    pub(crate) fn synthesize(
        &mut self,
        spec: &[f32; 1024],
        sequence: u8,
        shape: bool,
        prev_shape: bool,
        overlap: &mut [f64; 1024],
        out: &mut [f64; 1024],
    ) {
        let (cur, prev) = (usize::from(shape), usize::from(prev_shape));
        let frame = &mut self.frame;
        if sequence == EIGHT_SHORT {
            frame.fill(0.0);
            let mut y = [0.0f64; 256];
            for w in 0..8 {
                self.imdct_short.run(&spec[w * 128..w * 128 + 128], &mut y);
                let left = if w == 0 {
                    &self.short[prev]
                } else {
                    &self.short[cur]
                };
                let right = &self.short[cur];
                let at = 448 + 128 * w;
                for n in 0..128 {
                    frame[at + n] += y[n] * left[n];
                    frame[at + 128 + n] += y[128 + n] * right[128 + n];
                }
            }
        } else {
            self.imdct_long.run(&spec[..], &mut self.buf);
            for n in 0..1024 {
                let w = match sequence {
                    LONG_STOP => {
                        if n < 448 {
                            0.0
                        } else if n < 576 {
                            self.short[prev][n - 448]
                        } else {
                            1.0
                        }
                    }
                    _ => self.long[prev][n],
                };
                frame[n] = self.buf[n] * w;
            }
            for n in 1024..2048 {
                let w = match sequence {
                    LONG_START => {
                        let i = n - 1024;
                        if i < 448 {
                            1.0
                        } else if i < 576 {
                            self.short[cur][128 + i - 448]
                        } else {
                            0.0
                        }
                    }
                    _ => self.long[cur][n],
                };
                frame[n] = self.buf[n] * w;
            }
        }
        for n in 0..1024 {
            out[n] = frame[n] + overlap[n];
            overlap[n] = frame[1024 + n];
        }
    }
}

/// TNS all-pole filter over `coef[start..end]` (direction: true = downward), LPC `a[1..=order]`.
pub(crate) fn tns_filter(coef: &mut [f32], start: usize, end: usize, downward: bool, lpc: &[f64]) {
    let size = end.saturating_sub(start);
    if size == 0 || end > coef.len() {
        return;
    }
    let order = lpc.len();
    // Work in f64 on a copy of the region, in filtering order.
    let mut x: Vec<f64> = if downward {
        coef[start..end]
            .iter()
            .rev()
            .map(|v| f64::from(*v))
            .collect()
    } else {
        coef[start..end].iter().map(|v| f64::from(*v)).collect()
    };
    for m in 0..size {
        let mut acc = x[m];
        for i in 1..=order.min(m) {
            acc -= x[m - i] * lpc[i - 1];
        }
        x[m] = acc;
    }
    if downward {
        x.reverse();
    }
    for (dst, v) in coef[start..end].iter_mut().zip(x) {
        *dst = v as f32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The FFT-based IMDCT equals the defining sum (2/N) * sum X[k] cos(2 pi / N (n + n0)(k + 1/2)).
    #[test]
    fn imdct_matches_the_definition() {
        for m in [8usize, 128, 1024] {
            let x: Vec<f32> = (0..m).map(|k| ((k * 7919) % 13) as f32 - 6.0).collect();
            let mut y = vec![0.0; 2 * m];
            Imdct::new(m).run(&x, &mut y);
            let n_total = 2 * m;
            let n0 = (m as f64 + 1.0) / 2.0;
            for n in (0..n_total).step_by(if m > 128 { 37 } else { 1 }) {
                let want: f64 = (0..m)
                    .map(|k| {
                        f64::from(x[k])
                            * (2.0 * PI / n_total as f64 * (n as f64 + n0) * (k as f64 + 0.5)).cos()
                    })
                    .sum::<f64>()
                    * 2.0
                    / n_total as f64;
                assert!(
                    (y[n] - want).abs() < 1e-9,
                    "m={m} n={n}: {} vs {want}",
                    y[n]
                );
            }
        }
    }

    #[test]
    fn windows_satisfy_princen_bradley() {
        for w in [
            sine_window(2048),
            kbd_window(2048, 4.0),
            sine_window(256),
            kbd_window(256, 6.0),
        ] {
            let h = w.len() / 2;
            for i in 0..h {
                assert!((w[i] * w[i] + w[i + h] * w[i + h] - 1.0).abs() < 1e-12);
            }
        }
    }
}
