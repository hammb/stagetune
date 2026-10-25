//! Low-latency monophonic pitch shifter.
//!
//! The input is written into a ring buffer and read back by a tap whose
//! delay changes by `1 − ratio` samples per sample, which resamples the
//! signal by `ratio`. The delay therefore drifts: shifting up it shrinks
//! towards the write head, shifting down it grows. Before it runs out of
//! room the tap jumps by a whole number of pitch periods (cycle insertion or
//! deletion), so the old and new taps read near-identical waveforms, and the
//! two are crossfaded over about one period. Each jump is refined by
//! normalised cross-correlation over ±12 % of the period, so the splice
//! lands on the matching point of the waveform.
//!
//! The delay stays within roughly `[d_min, d_min + period]`: 1–5 ms for a
//! singing voice.
//!
//! While the input is unvoiced and unshifted, the shifter re-centres the tap
//! to `d_min`, so every phrase starts at minimum latency.
//!
//! Formants move with the shift (it is resampling): about 6 % for a
//! 1-semitone correction.

use std::f32::consts::PI;

pub struct SpliceShifter {
    buf: Vec<f32>,
    mask: usize,
    /// Index of the most recently written sample.
    w: usize,
    delay: f64,
    old_delay: f64,
    fading: bool,
    fade_len: usize,
    fade_pos: usize,
    d_min: f64,
    max_delay: f64,
    min_period: f32,
    max_period: f32,
    recenter_fade: usize,
}

impl SpliceShifter {
    /// `max_period` is the longest pitch period (samples) the shifter must
    /// splice on, i.e. `sample_rate / lowest_frequency`.
    pub fn new(sample_rate: f32, max_period: f32) -> Self {
        let size = ((max_period * 4.0) as usize + 1024).next_power_of_two();
        let d_min = (sample_rate / 6000.0).round().max(6.0) as f64; // ~8 samples @48k
        Self {
            buf: vec![0.0; size],
            mask: size - 1,
            w: 0,
            delay: d_min,
            old_delay: d_min,
            fading: false,
            fade_len: 1,
            fade_pos: 0,
            d_min,
            max_delay: (size - 8) as f64,
            min_period: sample_rate / 1500.0,
            max_period,
            recenter_fade: (sample_rate * 0.004) as usize,
        }
    }

    pub fn reset(&mut self) {
        self.buf.fill(0.0);
        self.w = 0;
        self.delay = self.d_min;
        self.old_delay = self.d_min;
        self.fading = false;
        self.fade_pos = 0;
    }

    /// Minimum delay of the read tap, in samples.
    pub fn min_latency(&self) -> usize {
        self.d_min as usize
    }

    /// Current delay of the active read tap, in samples.
    pub fn current_delay(&self) -> f64 {
        self.delay
    }

    #[inline]
    fn read(&self, delay: f64) -> f32 {
        let pos = (self.w + self.buf.len()) as f64 - delay;
        let i = pos.floor();
        let t = (pos - i) as f32;
        let i = i as usize;
        let m = self.mask;
        let xm1 = self.buf[(i.wrapping_sub(1)) & m];
        let x0 = self.buf[i & m];
        let x1 = self.buf[(i + 1) & m];
        let x2 = self.buf[(i + 2) & m];
        // 4-point, 3rd-order Hermite.
        let c1 = 0.5 * (x1 - xm1);
        let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
        let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
        ((c3 * t + c2) * t + c1) * t + x0
    }

    #[inline]
    fn at(&self, delay: usize) -> f32 {
        self.buf[(self.w + self.buf.len() - delay) & self.mask]
    }

    /// Process one sample.
    ///
    /// * `ratio`: output/input frequency ratio (1.0 = unchanged).
    /// * `period`: current pitch period in samples (any sensible value if unvoiced).
    /// * `voiced`: whether the input is currently pitched.
    #[inline]
    pub fn process(&mut self, x: f32, ratio: f32, period: f32, voiced: bool) -> f32 {
        self.w = (self.w + 1) & self.mask;
        self.buf[self.w] = x;
        let step = 1.0 - ratio as f64;

        let y = if self.fading {
            let p = (self.fade_pos as f32 + 0.5) / self.fade_len as f32;
            let g = 0.5 - 0.5 * (PI * p).cos();
            let y = self.read(self.delay) * g + self.read(self.old_delay) * (1.0 - g);
            self.old_delay = (self.old_delay + step).clamp(2.0, self.max_delay);
            self.fade_pos += 1;
            if self.fade_pos >= self.fade_len {
                self.fading = false;
            }
            y
        } else {
            self.read(self.delay)
        };

        self.delay = (self.delay + step).clamp(2.0, self.max_delay);
        if !self.fading {
            self.maybe_splice(ratio, period, voiced);
        }
        y
    }

    fn maybe_splice(&mut self, ratio: f32, period: f32, voiced: bool) {
        let p = period.clamp(self.min_period, self.max_period);
        let fade = p.round() as usize;
        if ratio > 1.0 + 1e-4 {
            // Tap is approaching the write head; leave room for the old tap
            // to keep running through the crossfade.
            let drift = (ratio as f64 - 1.0) * fade as f64;
            if self.delay <= self.d_min + drift {
                let j = self.best_jump(p, true);
                self.start_fade(self.delay + j, fade);
            }
        } else if ratio < 1.0 - 1e-4 {
            let margin = 0.15 * p as f64 + 2.0;
            if self.delay >= self.d_min + p as f64 + margin {
                let j = self.best_jump(p, false);
                let target = (self.delay - j).max(self.d_min);
                self.start_fade(target, fade);
            }
        } else if !voiced && self.delay > self.d_min + 32.0 {
            // Unvoiced and unshifted: re-centre to minimum latency.
            self.start_fade(self.d_min, self.recenter_fade);
        }
    }

    fn start_fade(&mut self, new_delay: f64, fade_len: usize) {
        self.old_delay = self.delay;
        self.delay = new_delay.clamp(2.0, self.max_delay);
        self.fade_len = fade_len.max(16);
        self.fade_pos = 0;
        self.fading = true;
    }

    /// Jump length (samples) near one period that best aligns the waveform
    /// just behind the current tap with the waveform one jump further
    /// back (`back = true`) or forward.
    fn best_jump(&self, period: f32, back: bool) -> f64 {
        let d0 = self.delay.round() as isize;
        let p = period.round() as isize;
        let span = ((period * 0.12) as isize).max(2);
        let n = (p as usize).clamp(32, 512);
        let stride = if n > 256 { 2 } else { 1 };

        let lo = (p - span).max(8);
        let hi = p + span;
        let mut scores = [0f32; 160];
        let count = ((hi - lo + 1) as usize).min(scores.len());
        let mut best_i = 0usize;
        let mut best = f32::NEG_INFINITY;
        for (k, slot) in scores.iter_mut().enumerate().take(count) {
            let j = lo + k as isize;
            let d1 = if back { d0 + j } else { d0 - j };
            if d1 < 2 || (d1 + n as isize) as f64 >= self.max_delay {
                *slot = f32::NEG_INFINITY;
                continue;
            }
            let (mut xy, mut xx, mut yy) = (0f32, 0f32, 0f32);
            let mut i = 0;
            while i < n {
                let a = self.at(d0 as usize + i);
                let b = self.at(d1 as usize + i);
                xy += a * b;
                xx += a * a;
                yy += b * b;
                i += stride;
            }
            let s = xy / (xx * yy).sqrt().max(1e-12);
            *slot = s;
            if s > best {
                best = s;
                best_i = k;
            }
        }
        if best == f32::NEG_INFINITY {
            return period as f64;
        }
        let mut j = (lo + best_i as isize) as f64;
        if best_i > 0 && best_i + 1 < count {
            let (a, b, c) = (scores[best_i - 1], scores[best_i], scores[best_i + 1]);
            let denom = a - 2.0 * b + c;
            if a.is_finite() && c.is_finite() && denom < -1e-9 {
                j += (0.5 * (a - c) / denom).clamp(-0.5, 0.5) as f64;
            }
        }
        j
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detector::YinDetector;
    use std::f32::consts::TAU;

    fn voice_like(freq: f32, sr: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let t = i as f32 / sr;
                (1..=10)
                    .map(|h| (TAU * h as f32 * freq * t).sin() / (h * h) as f32)
                    .sum::<f32>()
                    * 0.4
            })
            .collect()
    }

    fn run(freq: f32, ratio: f32) -> (Vec<f32>, f32) {
        let sr = 48_000.0;
        let x = voice_like(freq, sr, 48_000);
        let mut sh = SpliceShifter::new(sr, sr / 60.0);
        let period = sr / freq;
        let y: Vec<f32> = x.iter().map(|&s| sh.process(s, ratio, period, true)).collect();
        let mut det = YinDetector::new(sr, 900);
        let est = det.analyze(&y[30_000..], 30, 800, 800, 0.15);
        (y, est.freq)
    }

    #[test]
    fn shifts_up_and_down() {
        for &(f, semis) in &[(220.0f32, 1.0f32), (220.0, -1.0), (110.0, 0.5), (440.0, -0.7), (330.0, 2.0)] {
            let ratio = 2f32.powf(semis / 12.0);
            let (_, got) = run(f, ratio);
            let cents = 1200.0 * (got / (f * ratio)).log2();
            assert!(cents.abs() < 5.0, "f={f} semis={semis}: got {got} ({cents:.1}c)");
        }
    }

    #[test]
    fn splices_are_smooth() {
        // Period-aligned splices keep the output as smooth as the input:
        // compare the largest sample-to-sample jump of both.
        let sr = 48_000.0;
        let f = 196.0;
        let x = voice_like(f, sr, 48_000);
        let max_in = x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        for semis in [-1.0f32, 1.0] {
            let (y, _) = run(f, 2f32.powf(semis / 12.0));
            let max_out = y[2000..].windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
            assert!(max_out < max_in * 1.3, "semis={semis}: {max_out} vs {max_in}");
        }
    }

    #[test]
    fn latency_stays_low() {
        let sr = 48_000.0;
        let f = 220.0;
        let x = voice_like(f, sr, 24_000);
        for semis in [-1.0f32, 1.0] {
            let mut sh = SpliceShifter::new(sr, sr / 60.0);
            let mut max_d = 0f64;
            for &s in &x {
                sh.process(s, 2f32.powf(semis / 12.0), sr / f, true);
                max_d = max_d.max(sh.current_delay());
            }
            // d_min + ~1.3 periods
            assert!(max_d < 8.0 + 1.35 * (sr / f) as f64 + 4.0, "max delay {max_d}");
        }
    }
}
