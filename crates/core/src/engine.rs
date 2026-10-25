//! Per-sample processor tying detector, controller and shifter together.

use crate::controller::{ControlOut, Controller};
use crate::detector::{PitchEstimate, YinDetector};
use crate::scale::{Key, Scale};
use crate::shifter::SpliceShifter;

/// Lowest frequency any range preset analyses, which sizes the buffers.
const ABS_MIN_HZ: f32 = 50.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VoiceRange {
    Soprano,
    #[default]
    AltoTenor,
    LowMale,
    Bass,
}

impl VoiceRange {
    /// (lowest, highest) fundamental in Hz. A narrower range means a
    /// shorter analysis window, so faster and more robust tracking.
    pub fn bounds(self) -> (f32, f32) {
        match self {
            VoiceRange::Soprano => (150.0, 1400.0),
            VoiceRange::AltoTenor => (90.0, 1000.0),
            VoiceRange::LowMale => (65.0, 700.0),
            VoiceRange::Bass => (ABS_MIN_HZ, 500.0),
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().replace(['-', '_', ' ', '/'], "").as_str() {
            "soprano" => VoiceRange::Soprano,
            "altotenor" | "alto" | "tenor" => VoiceRange::AltoTenor,
            "lowmale" | "baritone" => VoiceRange::LowMale,
            "bass" => VoiceRange::Bass,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    pub key: Key,
    pub scale: Scale,
    /// Time to move ~90 % of the way to the target note, in ms. 0 = hard tune.
    pub retune_ms: f32,
    /// 0–1. Slows retune on sustained notes so they keep natural drift.
    pub humanize: f32,
    /// 0–1. 0 corrects everything; higher values only correct notes that
    /// are already close to the target.
    pub flex: f32,
    /// 0–1. How much of the singer's pitch movement around the note center
    /// (wobble, drift) is removed. 0 = only the note center is corrected
    /// (most natural), 1 = flattened (hard tune).
    pub steady: f32,
    /// 0–1. How much *detected* vibrato is exempt from `steady`.
    pub vibrato: f32,
    pub range: VoiceRange,
    /// 0–1. Higher tracks breathier/rougher voices but may follow noise.
    pub tracking: f32,
    /// Concert pitch of A4 in Hz.
    pub a4: f32,
    /// Learn the singer's average tuning offset and read notes relative to it.
    pub learn_offset: bool,
    /// Input level from which correction applies, in dBFS.
    pub gate_db: f32,
    /// Pitch classes of currently held MIDI notes (bit n = pitch class n).
    /// When non-zero these are the only targets, overriding key and scale.
    pub midi_targets: u16,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            key: Key(0),
            scale: Scale::Major,
            retune_ms: 120.0,
            humanize: 0.3,
            flex: 0.0,
            steady: 0.5,
            vibrato: 1.0,
            range: VoiceRange::AltoTenor,
            tracking: 0.5,
            a4: 440.0,
            learn_offset: true,
            gate_db: -55.0,
            midi_targets: 0,
        }
    }
}

impl Settings {
    pub(crate) fn yin_threshold(&self) -> f32 {
        0.08 + 0.22 * self.tracking.clamp(0.0, 1.0)
    }

    pub(crate) fn gate_linear(&self) -> f32 {
        10f32.powf(self.gate_db / 20.0)
    }

    pub(crate) fn mask(&self) -> u16 {
        if self.midi_targets & 0x0fff != 0 {
            self.midi_targets & 0x0fff
        } else {
            self.scale.mask(self.key)
        }
    }
}

/// Per-hop state, for meters and offline analysis.
#[derive(Debug, Clone, Copy, Default)]
pub struct Telemetry {
    pub estimate: PitchEstimate,
    pub control: ControlOut,
}

pub struct Engine {
    sample_rate: f32,
    hop: usize,
    hop_pos: usize,
    history: Vec<f32>,
    hist_pos: usize,
    frame: Vec<f32>,
    detector: YinDetector,
    controller: Controller,
    shifter: SpliceShifter,
    settings: Settings,
    ratio: f32,
    ratio_step: f32,
    period: f32,
    voiced: bool,
    telemetry: Telemetry,
}

impl Engine {
    pub fn new(sample_rate: f32, settings: Settings) -> Self {
        let max_tau = (sample_rate / ABS_MIN_HZ).ceil() as usize;
        let hist_len = YinDetector::frame_len(max_tau);
        // ~1.3 ms analysis hop at 48 kHz.
        let hop = ((sample_rate / 750.0).round() as usize).max(16);
        Self {
            sample_rate,
            hop,
            hop_pos: 0,
            history: vec![0.0; hist_len],
            hist_pos: 0,
            frame: vec![0.0; hist_len],
            detector: YinDetector::new(sample_rate, max_tau),
            controller: Controller::new(),
            shifter: SpliceShifter::new(sample_rate, max_tau as f32),
            settings,
            ratio: 1.0,
            ratio_step: 0.0,
            period: sample_rate / 200.0,
            voiced: false,
            telemetry: Telemetry::default(),
        }
    }

    pub fn reset(&mut self) {
        self.history.fill(0.0);
        self.hist_pos = 0;
        self.hop_pos = 0;
        self.controller.reset();
        self.shifter.reset();
        self.ratio = 1.0;
        self.ratio_step = 0.0;
        self.period = self.sample_rate / 200.0;
        self.voiced = false;
    }

    pub fn set_settings(&mut self, settings: Settings) {
        self.settings = settings;
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Analysis hop in samples (telemetry updates once per hop).
    pub fn hop(&self) -> usize {
        self.hop
    }

    /// Latency to report to the host: minimum tap delay plus half a typical
    /// voice period. The real delay varies by up to one pitch period.
    pub fn latency_samples(&self) -> u32 {
        (self.shifter.min_latency() as f32 + 0.5 * self.sample_rate / 200.0).round() as u32
    }

    /// Current delay of the shifter's read tap, in samples.
    pub fn current_delay(&self) -> f64 {
        self.shifter.current_delay()
    }

    pub fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }

    #[inline]
    pub fn process_sample(&mut self, x: f32) -> f32 {
        self.history[self.hist_pos] = x;
        self.hist_pos += 1;
        if self.hist_pos == self.history.len() {
            self.hist_pos = 0;
        }
        self.hop_pos += 1;
        if self.hop_pos >= self.hop {
            self.hop_pos = 0;
            self.analyze();
        }
        self.ratio += self.ratio_step;
        self.shifter.process(x, self.ratio, self.period, self.voiced)
    }

    pub fn process_block(&mut self, io: &mut [f32]) {
        for s in io.iter_mut() {
            *s = self.process_sample(*s);
        }
    }

    fn analyze(&mut self) {
        let (fmin, fmax) = self.settings.range.bounds();
        let tau_max = ((self.sample_rate / fmin).ceil() as usize).min(self.detector.max_tau());
        let tau_min = (self.sample_rate / fmax).floor() as usize;
        // While a note is tracked, integrate over ~1.5 periods (low lag);
        // otherwise over the full range so any onset pitch is caught.
        let window = if self.voiced {
            ((1.5 * self.period) as usize).clamp(tau_min, tau_max)
        } else {
            tau_max
        };
        let n = window + tau_max;

        // Unroll the newest `n` samples of the ring into a contiguous frame.
        let len = self.history.len();
        let start = (self.hist_pos + len - n) % len;
        let first = (len - start).min(n);
        self.frame[..first].copy_from_slice(&self.history[start..start + first]);
        self.frame[first..n].copy_from_slice(&self.history[..n - first]);

        let est = self
            .detector
            .analyze(&self.frame[..n], tau_min, tau_max, window, self.settings.yin_threshold());
        let dt = self.hop as f32 / self.sample_rate;
        let ctl = self.controller.update(&est, &self.settings, dt);

        let target_ratio = 2f32.powf(ctl.correction / 12.0);
        self.ratio_step = (target_ratio - self.ratio) / self.hop as f32;
        self.voiced = ctl.voiced;
        if ctl.voiced && est.period > 0.0 {
            self.period = est.period;
        }
        self.telemetry = Telemetry {
            estimate: est,
            control: ctl,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    /// Voice-like tone with an arbitrary pitch curve (Hz per sample).
    fn synth(freqs: &[f32], sr: f32) -> Vec<f32> {
        let mut phase = 0f32;
        freqs
            .iter()
            .map(|&f| {
                phase = (phase + f / sr).fract();
                (1..=10)
                    .map(|h| (TAU * h as f32 * phase).sin() / (h * h) as f32)
                    .sum::<f32>()
                    * 0.3
            })
            .collect()
    }

    fn measure(y: &[f32], sr: f32) -> f32 {
        let mut det = YinDetector::new(sr, 1000);
        det.analyze(y, 40, 600, 600, 0.15).freq
    }

    #[test]
    fn hard_tune_pulls_sharp_note_onto_pitch() {
        let sr = 48_000.0;
        // A3 + 35 cents, held for 1 s.
        let f = 220.0 * 2f32.powf(0.35 / 12.0);
        let x = synth(&vec![f; 48_000], sr);
        let s = Settings {
            key: Key(9),
            scale: Scale::Chromatic,
            retune_ms: 0.0,
            humanize: 0.0,
            ..Settings::default()
        };
        let mut e = Engine::new(sr, s);
        let y: Vec<f32> = x.iter().map(|&v| e.process_sample(v)).collect();
        let got = measure(&y[40_000..], sr);
        let cents = 1200.0 * (got / 220.0).log2();
        assert!(cents.abs() < 3.0, "output {got} Hz ({cents:.1} cents off A3)");
    }

    #[test]
    fn flat_note_corrected_with_natural_retune() {
        let sr = 48_000.0;
        // G3 − 30 cents in G major, natural settings.
        let g3 = 195.998;
        let f = g3 * 2f32.powf(-0.3 / 12.0);
        let x = synth(&vec![f; 48_000], sr);
        let s = Settings {
            key: Key(7),
            ..Settings::default()
        };
        let mut e = Engine::new(sr, s);
        let y: Vec<f32> = x.iter().map(|&v| e.process_sample(v)).collect();
        let cents = 1200.0 * (measure(&y[40_000..], sr) / g3).log2();
        assert!(cents.abs() < 4.0, "{cents:.1} cents off");
    }

    #[test]
    fn vibrato_is_preserved_around_corrected_center() {
        let sr = 48_000.0;
        let n = 96_000;
        // A3 + 30 cents center, ±40 cents vibrato at 5.5 Hz.
        let freqs: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / sr;
                220.0 * 2f32.powf((0.3 + 0.4 * (TAU * 5.5 * t).sin()) / 12.0)
            })
            .collect();
        let x = synth(&freqs, sr);
        let s = Settings {
            key: Key(9),
            scale: Scale::Chromatic,
            retune_ms: 20.0,
            humanize: 0.0,
            ..Settings::default()
        };
        let mut e = Engine::new(sr, s);
        let y: Vec<f32> = x.iter().map(|&v| e.process_sample(v)).collect();
        // Output pitch over the last second, with a sliding detector.
        let mut track = Vec::new();
        let mut det = YinDetector::new(sr, 1000);
        let mut i = 48_000;
        while i + 1200 < n {
            let f = det.analyze(&y[i..i + 1200], 40, 600, 600, 0.2).freq;
            track.push(1200.0 * (f / 220.0).log2());
            i += 240;
        }
        let mean = track.iter().sum::<f32>() / track.len() as f32;
        let max = track.iter().cloned().fold(f32::MIN, f32::max);
        let min = track.iter().cloned().fold(f32::MAX, f32::min);
        assert!(mean.abs() < 8.0, "center {mean:.1} cents");
        assert!(max - min > 55.0, "vibrato flattened: {:.1} cents p-p", max - min);
    }
}
