//! The musical part: turns a pitch track into a correction curve that makes
//! a weak singer sound like a better singer.
//!
//! The controller tracks three pitch curves, in fractional MIDI notes:
//!
//! * `fast`: the detected pitch, lightly smoothed (≈3 ms).
//! * `center`: a slow (≈2.6 Hz) average of `fast` that follows the note the
//!   singer is aiming for. When `fast` stays more than 0.6 semitone away from
//!   it for 25 ms, `center` snaps to `fast`.
//! * `note_center`: the running average pitch of the current note, from the
//!   moment the note settles.
//!
//! Note handling:
//!
//! * **Settle and lock.** A new note gets its target once its pitch has been
//!   steady for 25 ms. The target stays locked until the singer lands on a
//!   different note (a center snap to another note, or 80 ms more than 0.75
//!   semitone away) or stops singing.
//! * **Note-level correction.** The smoothed correction moves `note_center`
//!   onto the target, so within a note it settles to a near-constant offset,
//!   like moving a note in a vocal editor. The singer's movement around it
//!   passes through.
//! * **Steady.** A share `k` of the remaining deviation is pulled straight
//!   onto the target, unsmoothed (only the ~5 ms detection lag), on settled
//!   notes within a semitone of the target.
//! * **Ambiguity fade.** Correction strength fades to zero towards the
//!   midpoint between two allowed notes, so the correction is a continuous
//!   function of pitch.
//! * **Onset apply.** When a new note starts, the previous note's correction
//!   releases within ~25 ms. New notes, slides and fast runs pass through as
//!   sung until they settle; a settled note's correction is then applied
//!   within ~30 ms, close to the onset where the note change masks it. After
//!   that the retune time governs drift within the note, and fast movement
//!   inside a settled note holds the correction. All moves are 2-pole
//!   S-curves.
//! * **Release.** When the voice drops out (consonants, breath, note tails)
//!   the correction holds for 150 ms, then fades over ~200 ms.
//! * **Singer offset.** The controller learns the singer's average tuning
//!   from settled notes (circular mean, ~8 s memory) and reads which note is
//!   meant relative to it. The correction moves the singer's actual pitch.
//! * **Vibrato.** `VibratoDetector` recognises regular 3.5–9 Hz
//!   oscillation; Keep Vibrato exempts it from Steady.
//!
//! ```text
//! out = in + (1 − k)·S(w·e) + k·w·(target − fast),   e = target − note_center
//! w   = flex(e) · (1 − smoothstep(0.3, 0.47, ambiguity))
//! k   = steady · (1 − vibrato·v) · [note settled] · (1 − smoothstep(0.6, 1.0, |target − fast|)),
//!       smoothed over 15 ms; v = vibrato-detector confidence
//! ```
//!
//! `S` is the retune smoother; humanize stretches it on sustained notes.
//! Hard tune is Retune 0 + Steady 100 % + Keep Vibrato 0, which puts every
//! frame on the target, slides included.

use crate::detector::PitchEstimate;
use crate::engine::Settings;
use crate::hz_to_midi;
use crate::scale::{ambiguity, nearest_note};

const FAST_TAU: f32 = 0.003;
const CENTER_TAU: f32 = 0.06;
const SNAP_SEMIS: f32 = 0.6;
const SNAP_TIME: f32 = 0.025;
/// Pitch slope (semitones/s) above which the singer is in a transition.
/// Above the ~20 semitones/s peak slope of a wide vibrato.
const MOVING_SLOPE: f32 = 25.0;
const SLOPE_TAU: f32 = 0.015;
/// Steady time before a note's target is chosen and locked.
const SETTLE_TIME: f32 = 0.025;
/// After a note locks, its correction is applied with this rise time…
const ONSET_RISE: f32 = 0.03;
/// … for this long; then the (slower) retune time takes over.
const ONSET_WINDOW: f32 = 0.08;
/// Distance from the locked target, and duration, that counts as a new note.
const DRIFT_SEMIS: f32 = 0.75;
const DRIFT_TIME: f32 = 0.08;
/// Hysteresis for the provisional (pre-settle) target.
const HYSTERESIS: f32 = 0.15;
/// Longest averaging time of the per-note center; humanize stretches it.
const NOTE_TAU: f32 = 0.25;
/// Memory of the singer-offset estimate, in seconds of settled singing.
const OFFSET_TAU: f32 = 8.0;
const MAX_OFFSET: f32 = 0.45;
/// Distance from the target (semitones) over which Steady fades out.
const STEADY_FADE: (f32, f32) = (0.6, 1.0);
const STEADY_TAU: f32 = 0.015;
/// How fast a previous note's correction is dropped when a new note starts.
const NEW_NOTE_RELEASE: f32 = 0.025;
const DROPOUT_HOLD: f32 = 0.04;
/// Hold, then fade, of the correction once the voice has dropped out.
const RELEASE_HOLD: f32 = 0.15;
const RELEASE_TAU: f32 = 0.2;
const GLITCH_SEMIS: f32 = 5.0;
const GLITCH_HOLD: f32 = 0.04;
const MAX_CORRECTION: f32 = 3.0;

#[derive(Debug, Clone, Copy, Default)]
pub struct ControlOut {
    /// Pitch correction to apply, in semitones.
    pub correction: f32,
    /// Whether the controller considers the input voiced.
    pub voiced: bool,
    /// Smoothed input pitch (MIDI), if voiced.
    pub input_midi: Option<f32>,
    /// Current target note (MIDI), if voiced.
    pub target: Option<i32>,
    /// Confidence (0–1) that the singer is currently doing vibrato.
    pub vibrato: f32,
    /// Learned singer tuning offset in semitones (negative = flat).
    pub offset: f32,
    /// The smoothed note-center part of `correction` (excludes Steady).
    pub glide: f32,
}

#[derive(Default)]
pub struct Controller {
    voiced: bool,
    unvoiced_time: f32,
    fast: f32,
    center: f32,
    away_time: f32,
    glitch_time: f32,
    note_center: f32,
    slope: f32,
    settle_time: f32,
    locked: bool,
    drift_time: f32,
    target: Option<i32>,
    note_age: f32,
    /// Goal for the center correction (held during transitions).
    center_goal: f32,
    /// Correction strength for the current note (flex × ambiguity).
    weight: f32,
    /// Running circular mean of settled-note deviation (unit phasor).
    offset_x: f32,
    offset_y: f32,
    /// Learned singer offset in semitones (negative = flat).
    offset: f32,
    /// Smoothed Steady blend factor.
    steady_k: f32,
    /// Two-stage smoothed center correction.
    c1: f32,
    c2: f32,
    correction: f32,
    vibrato: VibratoDetector,
}

/// Recognises vocal vibrato as a regular oscillation of the pitch around
/// its center, from the last three half-cycles (zero crossings of the
/// deviation, with a small dead band).
#[derive(Default)]
struct VibratoDetector {
    sign: i8,
    since_cross: f32,
    peak: f32,
    half_periods: [f32; 3],
    peaks: [f32; 3],
    count: usize,
    regular: bool,
    confidence: f32,
}

impl VibratoDetector {
    const DEADBAND: f32 = 0.04;
    const HALF_MIN: f32 = 0.055; // 9 Hz
    const HALF_MAX: f32 = 0.145; // 3.5 Hz
    const DEPTH_MIN: f32 = 0.1; // ±10 cents
    const DEPTH_MAX: f32 = 1.5;

    fn reset(&mut self) {
        *self = Self::default();
    }

    fn update(&mut self, dev: f32, dt: f32) -> f32 {
        self.since_cross += dt;
        self.peak = self.peak.max(dev.abs());
        let sign = if dev > Self::DEADBAND {
            1
        } else if dev < -Self::DEADBAND {
            -1
        } else {
            0
        };
        if sign != 0 && sign != self.sign {
            if self.sign != 0 {
                let i = self.count % 3;
                self.half_periods[i] = self.since_cross;
                self.peaks[i] = self.peak;
                self.count += 1;
                self.regular = self.count >= 3 && self.is_regular();
            }
            self.sign = sign;
            self.since_cross = 0.0;
            self.peak = dev.abs();
        }
        // The verdict from the last crossing holds until a cycle runs late.
        let goal = self.regular && self.since_cross <= Self::HALF_MAX * 1.3;
        let tau = if goal { 0.06 } else { 0.1 };
        self.confidence += (goal as u8 as f32 - self.confidence) * alpha(dt, tau);
        self.confidence
    }

    fn is_regular(&self) -> bool {
        let hp = &self.half_periods;
        let pk = &self.peaks;
        let (hmin, hmax) = (hp.iter().cloned().fold(f32::MAX, f32::min), hp.iter().cloned().fold(0.0, f32::max));
        let (pmin, pmax) = (pk.iter().cloned().fold(f32::MAX, f32::min), pk.iter().cloned().fold(0.0, f32::max));
        hmin >= Self::HALF_MIN
            && hmax <= Self::HALF_MAX
            && hmax <= 1.6 * hmin
            && pmin >= Self::DEPTH_MIN
            && pmax <= Self::DEPTH_MAX
            && pmax <= 2.5 * pmin
    }
}

#[inline]
fn alpha(dt: f32, tau: f32) -> f32 {
    if tau <= 0.0 {
        1.0
    } else {
        1.0 - (-dt / tau).exp()
    }
}

#[inline]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl Controller {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn new_note(&mut self, provisional: i32) {
        self.target = Some(provisional);
        self.note_center = self.fast;
        self.locked = false;
        self.settle_time = 0.0;
        self.drift_time = 0.0;
        self.note_age = 0.0;
    }

    /// Advance by one analysis hop of `dt` seconds.
    pub fn update(&mut self, est: &PitchEstimate, s: &Settings, dt: f32) -> ControlOut {
        let (fmin, fmax) = s.range.bounds();
        let thr = s.yin_threshold();
        let thr = if self.voiced { thr * 1.5 } else { thr };
        let periodic = est.aperiodicity < thr && est.freq >= fmin * 0.97 && est.freq <= fmax * 1.03;
        let loud = est.rms > s.gate_linear();
        let mask = s.mask();

        if periodic && loud {
            self.unvoiced_time = 0.0;
            let mut m = hz_to_midi(est.freq, s.a4);
            if !self.voiced {
                self.voiced = true;
                self.fast = m;
                self.center = m;
                self.slope = 0.0;
                self.glitch_time = 0.0;
                self.vibrato.reset();
                self.new_note(nearest_note(m, mask));
            } else {
                // A sudden jump of ≥5 semitones is usually an octave error or
                // a consonant; hold the previous pitch for a few hops before
                // believing it.
                if (m - self.fast).abs() > GLITCH_SEMIS {
                    if self.glitch_time < GLITCH_HOLD {
                        self.glitch_time += dt;
                        m = self.fast;
                    } else {
                        self.fast = m;
                        self.center = m;
                        self.glitch_time = 0.0;
                        self.new_note(nearest_note(m, mask));
                    }
                } else {
                    self.glitch_time = 0.0;
                }
                let prev = self.fast;
                self.fast += (m - self.fast) * alpha(dt, FAST_TAU);
                self.slope += ((self.fast - prev) / dt - self.slope) * alpha(dt, SLOPE_TAU);
                self.center += (self.fast - self.center) * alpha(dt, CENTER_TAU);
                if (self.fast - self.center).abs() > SNAP_SEMIS {
                    self.away_time += dt;
                    if self.away_time >= SNAP_TIME {
                        self.center = self.fast;
                        self.away_time = 0.0;
                        // A snap starts a new note when it lands on a
                        // different note; a wide wobble on the same note
                        // keeps the note and its correction.
                        let cand = nearest_note(self.center - self.offset, mask);
                        if Some(cand) != self.target {
                            self.new_note(cand);
                        }
                    }
                } else {
                    self.away_time = 0.0;
                }
                self.vibrato.update(self.fast - self.center, dt);
            }

            let moving = self.slope.abs() > MOVING_SLOPE || self.away_time > 0.0;
            self.settle_time = if moving { 0.0 } else { self.settle_time + dt };
            // Notes are read relative to the singer's learned tuning.
            let heard = self.center - self.offset;
            let cand = nearest_note(heard, mask);
            if !self.locked {
                // Provisional target follows the pitch until the note settles.
                let off = self.target.map_or(f32::MAX, |t| (heard - t as f32).abs());
                if (heard - cand as f32).abs() + HYSTERESIS < off {
                    self.target = Some(cand);
                }
                if self.settle_time >= SETTLE_TIME {
                    self.locked = true;
                    // Average from here on, after the onset.
                    self.note_center = self.center;
                    self.note_age = 0.0;
                }
            } else if Some(cand) != self.target {
                let off = self.target.map_or(f32::MAX, |t| (heard - t as f32).abs());
                self.drift_time = if off > DRIFT_SEMIS { self.drift_time + dt } else { 0.0 };
                if self.drift_time >= DRIFT_TIME {
                    self.new_note(cand);
                }
            } else {
                self.drift_time = 0.0;
            }

            // Per-note average pitch: follows the center until the note
            // locks, then a cumulative mean that becomes an exponential one
            // (≤ NOTE_TAU), frozen during slides.
            let hard = s.retune_ms <= 5.0;
            if hard || !self.locked {
                self.note_center = self.center;
            } else if !moving {
                let tau = self.note_age.clamp(dt, NOTE_TAU * (1.0 + 2.0 * s.humanize));
                self.note_center += (self.fast - self.note_center) * (dt / tau).min(1.0);
                if s.learn_offset && self.note_age > 0.1 {
                    let ph = std::f32::consts::TAU * self.note_center;
                    let a = dt / OFFSET_TAU;
                    self.offset_x += (ph.cos() - self.offset_x) * a;
                    self.offset_y += (ph.sin() - self.offset_y) * a;
                    let r = self.offset_x.hypot(self.offset_y);
                    let mean = self.offset_y.atan2(self.offset_x) / std::f32::consts::TAU;
                    // Trust it only as far as the singer is consistent.
                    self.offset = (mean * smoothstep(0.15, 0.4, r)).clamp(-MAX_OFFSET, MAX_OFFSET);
                }
            }
            if !s.learn_offset {
                self.offset = 0.0;
            }

            let t = self.target.unwrap_or(cand) as f32;
            let e = t - self.note_center;
            self.weight = if hard {
                flex_amount(e, s.flex)
            } else {
                flex_amount(e, s.flex) * (1.0 - smoothstep(0.3, 0.47, ambiguity(self.note_center - self.offset, mask)))
            };
            if hard {
                self.center_goal = self.weight * e;
            } else if !self.locked {
                // New note, still settling: pass it through as sung.
                self.center_goal = 0.0;
            } else if !moving {
                self.center_goal = self.weight * e;
            } // else: movement inside a settled note, hold.
            self.note_age += dt;
        } else if self.voiced {
            self.unvoiced_time += dt;
            if self.unvoiced_time > DROPOUT_HOLD {
                self.voiced = false;
                self.target = None;
                self.locked = false;
            }
        } else {
            self.unvoiced_time += dt;
        }

        // Center correction: 2-pole smoother, retune = its 10→90 % rise time.
        let hard = s.retune_ms <= 5.0;
        let tau = if self.voiced && !self.locked && !hard {
            // New, unsettled note: drop the previous note's correction
            // quickly; the note change itself masks it.
            NEW_NOTE_RELEASE / 2.0
        } else if self.voiced && self.locked && self.note_age < ONSET_WINDOW {
            ONSET_RISE.min(s.retune_ms * 0.001) / 3.36
        } else if self.voiced {
            let base = s.retune_ms * 0.001 / 3.36;
            let sustain = smoothstep(0.15, 0.6, self.note_age);
            base * (1.0 + 6.0 * s.humanize * sustain)
        } else if self.unvoiced_time < RELEASE_HOLD {
            f32::INFINITY
        } else {
            self.center_goal = 0.0;
            RELEASE_TAU / 2.0
        };
        let a = if tau.is_infinite() { 0.0 } else { alpha(dt, tau) };
        self.c1 += (self.center_goal - self.c1) * a;
        self.c2 += (self.c1 - self.c2) * a;

        // Steady: blend straight onto the target, unsmoothed. Applies inside
        // a settled note and fades out between 0.6 and 1 semitone from the
        // target, where note changes begin.
        let (k_goal, direct) = match self.target {
            Some(t) if self.voiced && (self.locked || hard) => {
                let d = t as f32 - self.fast;
                let near = if hard { 1.0 } else { 1.0 - smoothstep(STEADY_FADE.0, STEADY_FADE.1, d.abs()) };
                let k = s.steady * (1.0 - s.vibrato * self.vibrato.confidence) * near;
                (k, self.weight * d)
            }
            _ => (0.0, 0.0),
        };
        self.steady_k += (k_goal - self.steady_k) * if hard { 1.0 } else { alpha(dt, STEADY_TAU) };
        let k = self.steady_k;
        self.correction = ((1.0 - k) * self.c2 + k * direct).clamp(-MAX_CORRECTION, MAX_CORRECTION);

        ControlOut {
            correction: self.correction,
            voiced: self.voiced,
            input_midi: self.voiced.then_some(self.fast),
            target: if self.voiced { self.target } else { None },
            vibrato: self.vibrato.confidence,
            offset: self.offset,
            glide: self.c2,
        }
    }
}

/// Flex window: full correction when the singer is within half
/// the window of the target, none beyond the window. `flex = 0` always
/// corrects.
fn flex_amount(error: f32, flex: f32) -> f32 {
    if flex <= 0.0 {
        return 1.0;
    }
    let window = 0.9 - 0.75 * flex.clamp(0.0, 1.0);
    1.0 - smoothstep(window * 0.5, window, error.abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vibrato_detector_accepts_regular_rejects_wobble() {
        let dt = 64.0 / 48_000.0;
        let mut v = VibratoDetector::default();
        let mut conf = 0.0;
        for i in 0..1500 {
            let t = i as f32 * dt;
            conf = v.update(0.35 * (std::f32::consts::TAU * 5.5 * t).sin(), dt);
        }
        assert!(conf > 0.9, "regular 5.5 Hz vibrato: {conf}");

        // Irregular drift: random walk, smoothed.
        let mut v = VibratoDetector::default();
        let (mut x, mut vel, mut seed, mut max_conf) = (0f32, 0f32, 12345u32, 0f32);
        for _ in 0..3000 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            vel = 0.97 * vel + 0.4 * ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * dt * 60.0;
            x = (0.995 * x + vel * dt * 10.0).clamp(-0.6, 0.6);
            max_conf = max_conf.max(v.update(x, dt));
        }
        assert!(max_conf < 0.5, "wobble mistaken for vibrato: {max_conf}");
    }

    #[test]
    fn flex_window() {
        assert_eq!(flex_amount(0.45, 0.0), 1.0);
        assert!(flex_amount(0.05, 1.0) > 0.99);
        assert!(flex_amount(0.4, 1.0) < 1e-6);
    }
}
