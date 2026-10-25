//! YIN pitch detection (de Cheveigné & Kawahara, 2002) with the difference
//! function computed through an FFT cross-correlation: one analysis costs two
//! forward FFTs and one inverse FFT for any lag range.
//!
//! The frame is analysed newest sample first. With `x[0]` the most recent
//! sample and `x[j]` the one `j` samples before it,
//!
//! ```text
//! d(τ) = Σ_{j<W} (x[j] − x[j+τ])²  =  E(0) + E(τ) − 2·r(τ)
//! ```
//!
//! compares the newest `W` samples with the ones `τ` earlier. `E(τ)` is the
//! energy of `x[τ..τ+W]` (from prefix sums) and `r(τ)` the cross-correlation
//! of `x[..W]` with `x`. Anchoring on the newest samples and keeping `W` near
//! 1.5 periods puts the estimate about one period behind real time. All math
//! is f64, which keeps `E + E − 2r` precise on near-periodic signals.

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, Default)]
pub struct PitchEstimate {
    /// Period in samples (fractional).
    pub period: f32,
    /// Fundamental frequency in Hz.
    pub freq: f32,
    /// YIN cumulative-mean-normalized difference at the chosen lag:
    /// ~0 for a perfectly periodic frame, ~1 for noise.
    pub aperiodicity: f32,
    /// RMS level of the analysis frame.
    pub rms: f32,
}

pub struct YinDetector {
    sample_rate: f32,
    fft_len: usize,
    max_tau: usize,
    r2c: Arc<dyn RealToComplex<f64>>,
    c2r: Arc<dyn ComplexToReal<f64>>,
    buf_a: Vec<f64>,
    buf_b: Vec<f64>,
    spec_a: Vec<Complex<f64>>,
    spec_b: Vec<Complex<f64>>,
    scratch_fwd: Vec<Complex<f64>>,
    scratch_inv: Vec<Complex<f64>>,
    corr: Vec<f64>,
    prefix: Vec<f64>,
    diff: Vec<f64>,
    cmndf: Vec<f64>,
}

impl YinDetector {
    /// Allocates everything needed for lags up to `max_tau` samples
    /// (i.e. down to `sample_rate / max_tau` Hz).
    pub fn new(sample_rate: f32, max_tau: usize) -> Self {
        let max_tau = max_tau.max(8);
        let fft_len = (2 * max_tau).next_power_of_two();
        let mut planner = RealFftPlanner::<f64>::new();
        let r2c = planner.plan_fft_forward(fft_len);
        let c2r = planner.plan_fft_inverse(fft_len);
        let spec_len = fft_len / 2 + 1;
        Self {
            sample_rate,
            fft_len,
            max_tau,
            scratch_fwd: r2c.make_scratch_vec(),
            scratch_inv: c2r.make_scratch_vec(),
            r2c,
            c2r,
            buf_a: vec![0.0; fft_len],
            buf_b: vec![0.0; fft_len],
            spec_a: vec![Complex::default(); spec_len],
            spec_b: vec![Complex::default(); spec_len],
            corr: vec![0.0; fft_len],
            prefix: vec![0.0; 2 * max_tau + 1],
            diff: vec![0.0; max_tau + 2],
            cmndf: vec![1.0; max_tau + 2],
        }
    }

    pub fn max_tau(&self) -> usize {
        self.max_tau
    }

    /// Longest frame [`analyze`](Self::analyze) can need for a given `tau_max`.
    pub fn frame_len(tau_max: usize) -> usize {
        2 * tau_max
    }

    /// Analyse the end of `frame` (oldest sample first, newest last), which
    /// must hold at least `window + tau_max` samples. `window` is the
    /// integration length `W` (clamped to `[tau_min, tau_max]`; ≥ one period
    /// of the expected pitch). `threshold` is YIN's absolute threshold
    /// (typ. 0.1–0.2).
    pub fn analyze(
        &mut self,
        frame: &[f32],
        tau_min: usize,
        tau_max: usize,
        window: usize,
        threshold: f32,
    ) -> PitchEstimate {
        let tau_max = tau_max.min(self.max_tau);
        let tau_min = tau_min.clamp(2, tau_max.saturating_sub(2));
        let w = window.clamp(tau_min, tau_max);
        let n = w + tau_max;
        assert!(frame.len() >= n, "frame too short for window + tau_max");
        let frame = &frame[frame.len() - n..];
        // Newest first.
        let x = |i: usize| frame[n - 1 - i];

        // Prefix sums of x² for E(τ) and the RMS (over the W + period span).
        self.prefix[0] = 0.0;
        for i in 0..n {
            let s = x(i) as f64;
            self.prefix[i + 1] = self.prefix[i] + s * s;
        }
        let rms = (self.prefix[n] / n as f64).sqrt() as f32;
        if rms < 1e-7 {
            return PitchEstimate {
                period: 0.0,
                freq: 0.0,
                aperiodicity: 1.0,
                rms,
            };
        }

        // r(τ) = Σ_{j<W} x[j]·x[j+τ] via FFT; fft_len ≥ n makes it a linear
        // correlation for every τ ≤ τmax.
        for (i, v) in self.buf_a.iter_mut().enumerate() {
            *v = if i < w { x(i) as f64 } else { 0.0 };
        }
        for (i, v) in self.buf_b.iter_mut().enumerate() {
            *v = if i < n { x(i) as f64 } else { 0.0 };
        }
        self.r2c
            .process_with_scratch(&mut self.buf_a, &mut self.spec_a, &mut self.scratch_fwd)
            .expect("fft size");
        self.r2c
            .process_with_scratch(&mut self.buf_b, &mut self.spec_b, &mut self.scratch_fwd)
            .expect("fft size");
        for (a, b) in self.spec_a.iter_mut().zip(self.spec_b.iter()) {
            *a = a.conj() * b;
        }
        let last = self.spec_a.len() - 1;
        self.spec_a[0].im = 0.0;
        self.spec_a[last].im = 0.0;
        self.c2r
            .process_with_scratch(&mut self.spec_a, &mut self.corr, &mut self.scratch_inv)
            .expect("fft size");
        let norm = 1.0 / self.fft_len as f64;

        // Difference function and its cumulative-mean normalisation.
        let e0 = self.prefix[w];
        self.diff[0] = 0.0;
        self.cmndf[0] = 1.0;
        let mut running = 0.0;
        for tau in 1..=tau_max {
            let e_tau = self.prefix[tau + w] - self.prefix[tau];
            let d = (e0 + e_tau - 2.0 * self.corr[tau] * norm).max(0.0);
            self.diff[tau] = d;
            running += d;
            self.cmndf[tau] = if running > 0.0 {
                d * tau as f64 / running
            } else {
                1.0
            };
        }

        // First dip under the threshold, walked down to its local minimum;
        // otherwise the global minimum (reported with its high aperiodicity).
        let search_end = tau_max - 1;
        let thr = threshold as f64;
        let mut chosen = None;
        let mut tau = tau_min;
        while tau < search_end {
            if self.cmndf[tau] < thr {
                while tau + 1 < search_end && self.cmndf[tau + 1] < self.cmndf[tau] {
                    tau += 1;
                }
                chosen = Some(tau);
                break;
            }
            tau += 1;
        }
        let tau = chosen.unwrap_or_else(|| {
            let mut best = tau_min;
            for t in tau_min..search_end {
                if self.cmndf[t] < self.cmndf[best] {
                    best = t;
                }
            }
            best
        });

        // Parabolic interpolation on the raw difference function (less
        // biased than interpolating the normalised curve).
        let (a, b, c) = (self.diff[tau - 1], self.diff[tau], self.diff[tau + 1]);
        let denom = a - 2.0 * b + c;
        let shift = if denom > 1e-18 {
            (0.5 * (a - c) / denom).clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let period = (tau as f64 + shift) as f32;
        PitchEstimate {
            period,
            freq: self.sample_rate / period,
            aperiodicity: self.cmndf[tau].clamp(0.0, 1.0) as f32,
            rms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    fn saw(freq: f32, sr: f32, n: usize) -> Vec<f32> {
        // Band-limited-ish sawtooth: sum of first harmonics, voice-like spectrum.
        (0..n)
            .map(|i| {
                let t = i as f32 / sr;
                (1..=12)
                    .filter(|h| (*h as f32) * freq < sr / 2.0)
                    .map(|h| (TAU * h as f32 * freq * t).sin() / h as f32)
                    .sum::<f32>()
                    * 0.3
            })
            .collect()
    }

    #[test]
    fn detects_voice_range_accurately() {
        let sr = 48_000.0;
        let tau_max = (sr / 70.0) as usize;
        let mut det = YinDetector::new(sr, tau_max);
        for &f in &[82.41f32, 110.0, 146.83, 220.0, 329.63, 440.0, 659.26, 880.0] {
            let x = saw(f, sr, 2 * tau_max);
            let est = det.analyze(&x, (sr / 1100.0) as usize, tau_max, tau_max, 0.15);
            let cents = 1200.0 * (est.freq / f).log2();
            assert!(cents.abs() < 2.0, "f={f} got {} ({cents:.2} cents)", est.freq);
            assert!(est.aperiodicity < 0.1);
        }
    }

    #[test]
    fn noise_is_aperiodic() {
        let sr = 48_000.0;
        let tau_max = 600;
        let mut det = YinDetector::new(sr, tau_max);
        let mut s = 0x1234_5678u32;
        let x: Vec<f32> = (0..2 * tau_max)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                (s as f32 / u32::MAX as f32) * 2.0 - 1.0
            })
            .collect();
        let est = det.analyze(&x, 40, tau_max, tau_max, 0.15);
        assert!(est.aperiodicity > 0.3, "aperiodicity {}", est.aperiodicity);
    }
}
