# AGENTS.md

Guidance for coding agents working on StageTune, a low-latency live vocal
pitch corrector in Rust. It ships as a CLAP, plus an AUv2 for MainStage/Logic
and a VST3, both generated from the CLAP by clap-wrapper. README.md has the
user-facing view.

## Goal

Make a weak singer sound like a better singer, live, with a natural voice.
Natural sound comes first; measured accuracy second. Hard tune is one
setting (Retune 0, Steady 100 %, Keep Vibrato 0).

## Layout

```
crates/core/src/detector.rs    FFT YIN, newest-sample anchored, adaptive window
crates/core/src/controller.rs  note logic: settle/lock, note-center correction,
                               ambiguity fade, Steady, vibrato detector, singer offset
crates/core/src/shifter.rs     delay-line resampler with period-synchronous splices
crates/core/src/engine.rs      per-sample glue, Settings, VoiceRange
crates/core/src/scale.rs       keys, scales, nearest_note, ambiguity
crates/plugin/src/lib.rs       nih-plug params → Settings, MIDI target notes
crates/cli/src/main.rs         offline analyze/process with metrics
wrappers/CMakeLists.txt        clap-wrapper AUv2 (type aumf, BHmm/StTn) and VST3
scripts/build.sh               universal CLAP + AU + VST3 build, --install runs auval
```

## Commands

```sh
source ~/.cargo/env                          # puts cargo on PATH
cargo test -p stagetune-core --release       # DSP unit tests (fast)
cargo clippy --workspace --release           # expected: clean
cargo build --release -p stagetune-cli
scripts/build.sh --install                   # build, install to ~/Library/Audio/Plug-Ins, auval
```

## Evaluating a DSP change

Judge every controller or shifter change on a real vocal with the CLI,
comparing before and after:

```sh
target/release/stagetune process vocal.wav out.wav --key G --scale major
```

Read the last lines of its output:

- `glides per 50 ms`: correction moving more than 30 cents in the body of a
  note. Target: around 1–3 %.
- `in-note pitch movement in → out`: out close to in, slightly lower, is
  natural; out ≪ in is flattened; out > in is added wobble.
- `held notes … average pitch`: note-level accuracy, which is what the ear
  judges. Improve it together with the two metrics above.
- `plugin delay`: the shifter's read delay over the whole file. Reference on
  the test vocal (Alto/Tenor): median 2.5 ms, 99 % ≤ 10.6 ms. Keep it there
  or lower.

For a listener's report at a timestamp, run with `--csv trace.csv` and read
the rows around it (input pitch, target, correction, vibrato) first. Keep
traces, renders and plots in a scratch directory.

Test audio is private recordings of real singers and stays out of git
(`*.wav`, `*.mp3` and `/renders` are gitignored).

## Invariants

- **Real-time safety:** `Engine::new` allocates every buffer. The audio path
  (`Engine::process_sample` and everything it calls) reuses them and stays
  lock-free; I/O and logging happen outside it. Size buffers for the worst case
  (`ABS_MIN_HZ`, 96 kHz).
- **Latency:** the shifter stays within about `d_min` plus one pitch period.
  The audio path is per-sample and time-domain.
- **Continuity:** the correction is a continuous function of the singer's
  pitch. The ambiguity fade, Steady fade, 15 ms Steady smoothing and S-curve
  retune keep it that way. Keep the module docs in sync with the code.
- **Note changes pass through as sung.** Scoops, slides and fast runs keep
  their shape until a note settles.
- **Plugin parameter `#[id]`s are stored in saved MainStage patches.** Keep
  existing ids; add new ones for new parameters.

## Build notes

- `scripts/build.sh` rebuilds `wrappers/build` from scratch each time; this
  regenerates the AU's Info.plist with its `AudioComponents` entry, and the
  script checks for it.
- After installing, the AU registry takes a few seconds; the install step
  waits for it before running `auval`.
- The AU is type `aumf` (MIDI-controlled effect) for MIDI target notes. Its
  type, manufacturer and subtype codes identify it in saved patches; keep
  them stable.

## Documentation style

Docs and comments describe what the code does. State behaviour, mechanisms
and numbers positively.

## Licensing and naming

- The project is Apache-2.0, and every shipped component is permissive
  (MIT/ISC/BSD/Apache). nih-plug builds the CLAP only; the VST3 comes from
  clap-wrapper with Steinberg's MIT-licensed VST3 SDK 3.8+. Add dependencies
  with permissive licenses, implement algorithms from papers, and update
  NOTICE when a bundled third-party component changes.
- UI strings and parameter names use StageTune's own terms (Flex, Steady,
  Keep Vibrato).

## Roadmap

1. Formant preservation (LPC/cepstral whitening around the shifter) for
   corrections beyond ~1 semitone.
2. Octave robustness at onsets: multi-candidate YIN with a causal HMM.
3. Custom GUI with a live pitch/target display.

## Git

Small, focused commits with a summary line plus a short body explaining why.
Commit source only; `target/`, `wrappers/build/` and `wrappers/clap-wrapper/`
are build output.
