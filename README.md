# StageTune

Low-latency live vocal pitch correction, written in Rust. It runs as an
**Audio Unit in MainStage / Logic** and as a CLAP or VST3 in other hosts, and
makes a weak singer sound like a better singer.

It uses YIN pitch detection (de Cheveigné & Kawahara, 2002) and
period-synchronous resampling, with its own note logic, vibrato detection
and splice alignment.

## Build and install

Requires Rust (`rustup`), CMake and the Xcode command-line tools.

```sh
scripts/build.sh             # → target/bundled/StageTune.{component,clap,vst3}
scripts/build.sh --install   # also copies to ~/Library/Audio/Plug-Ins and runs auval
```

The plugin is built as a CLAP. [clap-wrapper](https://github.com/free-audio/clap-wrapper)
generates the AU and VST3 from it, so all three formats run the same code.
All bundles are universal binaries (Apple Silicon + Intel), and the AU passes
`auval` (`auval -v aumf StTn BHmm`).

## Using it in MainStage

1. Restart MainStage after installing. StageTune is listed under
   **Audio Units → MIDI-controlled Effects → Benjamin Hamm → StageTune**.
   It is a MIDI-controlled effect (type `aumf`), so it receives target notes.
2. Put it in the **first insert** of the vocal channel strip, before EQ,
   compression and reverb. A mono channel strip works best.
3. In *Preferences → Audio*, set the I/O buffer to **32 or 64 samples**.
4. Per song (patch), set **Key** and **Scale**. This matters most.
5. Optional, and best for a weak singer: **MIDI target notes**. In the plugin
   window's MIDI source menu, choose a keyboard or a MIDI track that plays the
   melody. While MIDI notes are held, StageTune uses exactly those pitch
   classes as targets (any octave) and puts the singer on the intended note.

## Controls

The defaults make a weak singer sound *better* while keeping a natural voice.

| Control | What it does | Natural (default) | Firm | Hard-tune effect |
|---|---|---|---|---|
| Key / Scale | Allowed target notes. *Custom* uses the 12 "Note" switches. | song key | song key | song key |
| Retune Speed | How gently (ms) a note's average pitch is moved onto the target once the note has settled. | 120 | 80 | 0 |
| Steady | How much of the wobble on a settled note is pulled straight onto the target, instantly. Scoops, slides and note changes pass through as sung. 0 = only the note's average is moved. | 50 % | 70 % | 100 % |
| Keep Vibrato | How much *detected* regular vibrato (3.5–9 Hz) is exempt from Steady. | 100 % | 100 % | 0 % |
| Humanize | Gentler correction on long held notes. | 30 % | 30 % | 0 % |
| Flex | Corrects only notes already near the target. | 0 % | 0 % | 0 % |
| Learn Singer Offset | Learns a singer who is consistently flat/sharp (≈8 s memory) and reads which note they *mean*. | on | on | either |
| Voice Range | Narrower = faster, more reliable tracking. | Alto/Tenor or Low Male | | |
| Tracking | Higher follows breathy/rough voices; lower ignores noise. | 50 % | | |
| A4 Reference | Concert pitch of the band/tracks. | 440 | | |
| Gate | Input level from which correction applies. | −55 dB | | |

How the natural mode keeps the voice natural:

- **One target per note.** The target locks once the note settles and stays
  until the singer moves to another note.
- **Slides, scoops and note changes pass through as sung.** A new note gets
  its correction once it settles, right after the onset.
- **Smooth decisions.** Halfway between two scale notes the correction fades
  to zero, so the output follows the singer continuously.
- **Steady works instantly.** It pulls wobble onto the target with only the
  detection delay, so it calms the voice.
- **Note ends.** The correction holds, then fades gently.

## Offline testing

```sh
cargo build --release -p stagetune-cli
target/release/stagetune analyze vocal.mp3                 # key, singer's tuning, intonation stats
target/release/stagetune process vocal.mp3 out.wav --key G --scale major --steady 70
target/release/stagetune process vocal.mp3 out.wav --csv trace.csv   # per-hop pitch/correction trace
```

`process` reports:

- frame-level intonation before and after;
- **held-note accuracy**: the average pitch of each note against its target,
  which is what the ear judges;
- **artefact proxies**: target flips, correction glides inside notes, and
  in-note pitch movement in vs out. Out ≪ in means flattened/robotic;
  out > in means added wobble.

## How it works

```
in ──┬─────────────────────────────────► SpliceShifter ──► out
     │                                      ▲ ratio, period
     └─► history ─(every 1.3 ms)─► YIN ─► Controller
```

- **Detector** (`crates/core/src/detector.rs`): YIN with the difference
  function computed by FFT cross-correlation, in f64. The window is anchored
  on the *newest* samples and sized to ~1.5 periods of the current note, so
  the pitch estimate trails real time by about one period (~5–7 ms for a
  male voice).
- **Controller** (`controller.rs`): segments notes, locks one target per
  settled note, and moves each note's *running average* pitch onto it with a
  smooth S-curve, like nudging a note in a vocal editor. Correction strength
  fades with note ambiguity, and notes are read relative to the singer's
  learned average offset. A vibrato detector exempts *regular* vibrato from
  Steady. The module docs describe each mechanism.
- **Shifter** (`shifter.rs`): reads a delay line at a variable rate. When
  the read tap drifts far enough, it jumps by exactly one pitch period,
  refined by normalised cross-correlation, and crossfades over one period.
  Latency stays within about 0.2 ms + one pitch period (≈2–5 ms for voice)
  and re-centres to the minimum between phrases.
- **Engine** (`engine.rs`): per-sample glue. All buffers are allocated up
  front; the audio thread reuses them. CPU use is about 1.3 % of one core at
  48 kHz.

## Scope

- Formants move with the pitch (it is resampling): about 6 % for a
  1-semitone correction.
- Monophonic: one voice per instance; stereo input is folded to mono.
- The plugin uses the host's generic parameter view.
- For songs where a singer is a semitone or more off, MIDI target notes
  give the tuner the melody.

Planned: formant preservation for larger corrections, a stronger octave guard
at note onsets, and a custom GUI with a pitch display.

## Layout

```
crates/core     DSP engine (pure Rust, unit-tested: cargo test -p stagetune-core)
crates/plugin   nih-plug plugin (CLAP)
crates/cli      offline test tool
wrappers/       CMake project wrapping the CLAP as AUv2 and VST3 (clap-wrapper)
scripts/        build/install script
```

## License

Apache License 2.0, see [LICENSE](LICENSE). Third-party components in the
built plugins are listed in [NOTICE](NOTICE); all are under permissive
licenses (MIT, ISC, Apache-2.0).
