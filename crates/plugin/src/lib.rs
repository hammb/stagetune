use nih_plug::prelude::*;
use stagetune_core::{Engine, Key, Scale, Settings, VoiceRange};
use std::num::NonZeroU32;
use std::sync::Arc;

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
enum KeyParam {
    C,
    #[name = "C#/Db"]
    Cs,
    D,
    #[name = "D#/Eb"]
    Ds,
    E,
    F,
    #[name = "F#/Gb"]
    Fs,
    G,
    #[name = "G#/Ab"]
    Gs,
    A,
    #[name = "A#/Bb"]
    As,
    B,
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
enum ScaleParam {
    Major,
    Minor,
    #[name = "Harmonic Minor"]
    HarmonicMinor,
    #[name = "Major Pentatonic"]
    MajorPentatonic,
    #[name = "Minor Pentatonic"]
    MinorPentatonic,
    Blues,
    Dorian,
    Mixolydian,
    Chromatic,
    /// Takes its notes from the twelve "Note" switches below.
    Custom,
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
enum RangeParam {
    Soprano,
    #[name = "Alto/Tenor"]
    AltoTenor,
    #[name = "Low Male"]
    LowMale,
    Bass,
}

#[derive(Params)]
struct StageTuneParams {
    #[id = "key"]
    key: EnumParam<KeyParam>,
    #[id = "scale"]
    scale: EnumParam<ScaleParam>,
    #[id = "retune"]
    retune: FloatParam,
    #[id = "flex"]
    flex: FloatParam,
    #[id = "humanize"]
    humanize: FloatParam,
    #[id = "steady"]
    steady: FloatParam,
    #[id = "vibrato"]
    vibrato: FloatParam,
    #[id = "range"]
    range: EnumParam<RangeParam>,
    #[id = "tracking"]
    tracking: FloatParam,
    #[id = "a4"]
    a4: FloatParam,
    #[id = "gate"]
    gate: FloatParam,
    #[id = "offset"]
    learn_offset: BoolParam,

    #[id = "n_c"]
    note_c: BoolParam,
    #[id = "n_cs"]
    note_cs: BoolParam,
    #[id = "n_d"]
    note_d: BoolParam,
    #[id = "n_ds"]
    note_ds: BoolParam,
    #[id = "n_e"]
    note_e: BoolParam,
    #[id = "n_f"]
    note_f: BoolParam,
    #[id = "n_fs"]
    note_fs: BoolParam,
    #[id = "n_g"]
    note_g: BoolParam,
    #[id = "n_gs"]
    note_gs: BoolParam,
    #[id = "n_a"]
    note_a: BoolParam,
    #[id = "n_as"]
    note_as: BoolParam,
    #[id = "n_b"]
    note_b: BoolParam,
}

fn percent(name: &str, default: f32) -> FloatParam {
    FloatParam::new(name, default, FloatRange::Linear { min: 0.0, max: 1.0 })
        .with_unit("%")
        .with_value_to_string(formatters::v2s_f32_percentage(0))
        .with_string_to_value(formatters::s2v_f32_percentage())
}

impl Default for StageTuneParams {
    fn default() -> Self {
        let note = |name: &str| BoolParam::new(name, true);
        Self {
            key: EnumParam::new("Key", KeyParam::C),
            scale: EnumParam::new("Scale", ScaleParam::Major),
            retune: FloatParam::new(
                "Retune Speed",
                120.0,
                FloatRange::Skewed {
                    min: 0.0,
                    max: 400.0,
                    factor: FloatRange::skew_factor(-1.5),
                },
            )
            .with_unit(" ms")
            .with_step_size(1.0),
            flex: percent("Flex", 0.0),
            humanize: percent("Humanize", 0.3),
            steady: percent("Steady", 0.5),
            vibrato: percent("Keep Vibrato", 1.0),
            range: EnumParam::new("Voice Range", RangeParam::AltoTenor),
            tracking: percent("Tracking", 0.5),
            a4: FloatParam::new("A4 Reference", 440.0, FloatRange::Linear { min: 420.0, max: 460.0 })
                .with_unit(" Hz")
                .with_step_size(0.1),
            gate: FloatParam::new("Gate", -55.0, FloatRange::Linear { min: -80.0, max: -20.0 })
                .with_unit(" dB")
                .with_step_size(0.5),
            learn_offset: BoolParam::new("Learn Singer Offset", true),
            note_c: note("Note C"),
            note_cs: note("Note C#"),
            note_d: note("Note D"),
            note_ds: note("Note D#"),
            note_e: note("Note E"),
            note_f: note("Note F"),
            note_fs: note("Note F#"),
            note_g: note("Note G"),
            note_gs: note("Note G#"),
            note_a: note("Note A"),
            note_as: note("Note A#"),
            note_b: note("Note B"),
        }
    }
}

impl StageTuneParams {
    fn custom_mask(&self) -> u16 {
        [
            &self.note_c, &self.note_cs, &self.note_d, &self.note_ds, &self.note_e, &self.note_f,
            &self.note_fs, &self.note_g, &self.note_gs, &self.note_a, &self.note_as, &self.note_b,
        ]
        .iter()
        .enumerate()
        .fold(0u16, |m, (i, p)| if p.value() { m | (1 << i) } else { m })
    }

    fn settings(&self, midi_targets: u16) -> Settings {
        let scale = match self.scale.value() {
            ScaleParam::Major => Scale::Major,
            ScaleParam::Minor => Scale::Minor,
            ScaleParam::HarmonicMinor => Scale::HarmonicMinor,
            ScaleParam::MajorPentatonic => Scale::MajorPentatonic,
            ScaleParam::MinorPentatonic => Scale::MinorPentatonic,
            ScaleParam::Blues => Scale::Blues,
            ScaleParam::Dorian => Scale::Dorian,
            ScaleParam::Mixolydian => Scale::Mixolydian,
            ScaleParam::Chromatic => Scale::Chromatic,
            ScaleParam::Custom => Scale::Custom(self.custom_mask()),
        };
        let range = match self.range.value() {
            RangeParam::Soprano => VoiceRange::Soprano,
            RangeParam::AltoTenor => VoiceRange::AltoTenor,
            RangeParam::LowMale => VoiceRange::LowMale,
            RangeParam::Bass => VoiceRange::Bass,
        };
        Settings {
            key: Key(self.key.value() as u8),
            scale,
            retune_ms: self.retune.value(),
            humanize: self.humanize.value(),
            steady: self.steady.value(),
            flex: self.flex.value(),
            vibrato: self.vibrato.value(),
            range,
            tracking: self.tracking.value(),
            a4: self.a4.value(),
            gate_db: self.gate.value(),
            learn_offset: self.learn_offset.value(),
            midi_targets,
        }
    }
}

struct StageTune {
    params: Arc<StageTuneParams>,
    engine: Option<Engine>,
    /// Number of held MIDI notes per pitch class (MIDI target notes).
    held: [u8; 12],
}

impl Default for StageTune {
    fn default() -> Self {
        Self {
            params: Arc::new(StageTuneParams::default()),
            engine: None,
            held: [0; 12],
        }
    }
}

impl Plugin for StageTune {
    const NAME: &'static str = "StageTune";
    const VENDOR: &'static str = "Benjamin Hamm";
    const URL: &'static str = "";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(1),
            main_output_channels: NonZeroU32::new(1),
            ..AudioIOLayout::const_default()
        },
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(2),
            main_output_channels: NonZeroU32::new(2),
            ..AudioIOLayout::const_default()
        },
    ];

    /// Held MIDI notes become the only allowed targets (any octave), so a
    /// melody track or keyboard can tell the tuner which note is meant.
    const MIDI_INPUT: MidiConfig = MidiConfig::Basic;
    const SAMPLE_ACCURATE_AUTOMATION: bool = false;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn initialize(
        &mut self,
        _layout: &AudioIOLayout,
        config: &BufferConfig,
        context: &mut impl InitContext<Self>,
    ) -> bool {
        let engine = Engine::new(config.sample_rate, self.params.settings(0));
        context.set_latency_samples(engine.latency_samples());
        self.engine = Some(engine);
        true
    }

    fn reset(&mut self) {
        self.held = [0; 12];
        if let Some(e) = self.engine.as_mut() {
            e.reset();
        }
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        // Held notes update once per block, precise enough for targets.
        while let Some(event) = context.next_event() {
            match event {
                NoteEvent::NoteOn { note, velocity, .. } if velocity > 0.0 => {
                    let pc = &mut self.held[note as usize % 12];
                    *pc = pc.saturating_add(1);
                }
                NoteEvent::NoteOn { note, .. } | NoteEvent::NoteOff { note, .. } => {
                    let pc = &mut self.held[note as usize % 12];
                    *pc = pc.saturating_sub(1);
                }
                NoteEvent::MidiCC { cc: 123, .. } => self.held = [0; 12],
                _ => {}
            }
        }
        let midi_targets = self
            .held
            .iter()
            .enumerate()
            .fold(0u16, |m, (i, &n)| if n > 0 { m | (1 << i) } else { m });

        let Some(engine) = self.engine.as_mut() else {
            return ProcessStatus::Normal;
        };
        engine.set_settings(self.params.settings(midi_targets));
        let channels = buffer.channels() as f32;
        for mut frame in buffer.iter_samples() {
            // Pitch correction is monophonic: fold to mono, process, copy back.
            let mut sum = 0.0;
            for s in frame.iter_mut() {
                sum += *s;
            }
            let y = engine.process_sample(sum / channels);
            for s in frame.iter_mut() {
                *s = y;
            }
        }
        ProcessStatus::Normal
    }
}

impl ClapPlugin for StageTune {
    const CLAP_ID: &'static str = "com.benjaminhamm.stagetune";
    const CLAP_DESCRIPTION: Option<&'static str> = Some("Low-latency live vocal pitch correction");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::PitchCorrection,
        ClapFeature::Mono,
        ClapFeature::Stereo,
    ];
}

nih_export_clap!(StageTune);
