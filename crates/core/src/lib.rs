//! StageTune DSP core.
//!
//! Signal flow, per sample:
//!
//! ```text
//!  in ──┬──────────────────────────────► SpliceShifter ──► out
//!       │                                    ▲ ratio, period
//!       └─► history ─(every hop)─► YinDetector ─► Controller
//! ```
//!
//! * [`detector`]: FFT-accelerated YIN pitch detection.
//! * [`controller`]: turns a pitch track into a correction curve (note
//!   selection, retune speed, flex window, humanize, vibrato handling).
//! * [`shifter`]: low-latency pitch shifting by variable-rate delay-line
//!   reading with period-synchronous, correlation-aligned splices.
//! * [`engine`]: ties the three together into a per-sample processor.
//!
//! [`Engine::new`] allocates every buffer; the audio path reuses them.

pub mod controller;
pub mod detector;
pub mod engine;
pub mod scale;
pub mod shifter;

pub use controller::{ControlOut, Controller};
pub use detector::{PitchEstimate, YinDetector};
pub use engine::{Engine, Settings, Telemetry, VoiceRange};
pub use scale::{Key, Scale};
pub use shifter::SpliceShifter;

/// Frequency in Hz to fractional MIDI note number.
#[inline]
pub fn hz_to_midi(hz: f32, a4: f32) -> f32 {
    69.0 + 12.0 * (hz / a4).log2()
}

/// Fractional MIDI note number to frequency in Hz.
#[inline]
pub fn midi_to_hz(midi: f32, a4: f32) -> f32 {
    a4 * 2f32.powf((midi - 69.0) / 12.0)
}
