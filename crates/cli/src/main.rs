//! Offline test tool for the StageTune engine.
//!
//! ```text
//! stagetune analyze vocal.mp3
//! stagetune process vocal.mp3 tuned.wav --key G --scale major
//! ```
//!
//! Non-WAV input is decoded with `ffmpeg` (must be on PATH).

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use stagetune_core::scale::nearest_note;
use stagetune_core::{hz_to_midi, Engine, Key, Scale, Settings, VoiceRange, YinDetector};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

#[derive(Parser)]
#[command(name = "stagetune", about = "Offline test tool for the StageTune pitch corrector")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Estimate the key and measure how in-tune a vocal is.
    Analyze {
        input: PathBuf,
        #[arg(long, default_value = "alto-tenor")]
        range: String,
        /// Key to score against (default: the estimated key).
        #[arg(long)]
        key: Option<String>,
        #[arg(long, default_value = "major")]
        scale: String,
    },
    /// Run a file through the corrector and report before/after accuracy.
    Process {
        input: PathBuf,
        output: PathBuf,
        #[command(flatten)]
        opts: Opts,
        /// Write a per-hop pitch/correction trace as CSV.
        #[arg(long)]
        csv: Option<PathBuf>,
    },
}

#[derive(Args)]
struct Opts {
    /// Key (C, C#, Db, … B), or "auto" to estimate it from the input.
    #[arg(long, default_value = "auto")]
    key: String,
    /// major, minor, harmonic-minor, major-pentatonic, minor-pentatonic,
    /// blues, dorian, mixolydian, chromatic. With --key auto, the estimate
    /// picks major or minor.
    #[arg(long, default_value = "major")]
    scale: String,
    /// Retune speed in ms (0 = hard tune).
    #[arg(long, default_value_t = 120.0)]
    retune: f32,
    /// Steady, 0–100: how much wobble around the note is removed.
    #[arg(long, default_value_t = 50.0)]
    steady: f32,
    /// Flex amount, 0–100: only correct notes already near the target.
    #[arg(long, default_value_t = 0.0)]
    flex: f32,
    /// Humanize, 0–100.
    #[arg(long, default_value_t = 30.0)]
    humanize: f32,
    /// Detected vibrato exempt from Steady, 0–100.
    #[arg(long, default_value_t = 100.0)]
    vibrato: f32,
    /// soprano, alto-tenor, low-male, bass.
    #[arg(long, default_value = "alto-tenor")]
    range: String,
    /// Tracking, 0–100.
    #[arg(long, default_value_t = 50.0)]
    tracking: f32,
    #[arg(long, default_value_t = 440.0)]
    a4: f32,
    /// Turn singer-offset learning off.
    #[arg(long)]
    no_offset: bool,
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Analyze { input, range, key, scale } => {
            let (x, sr) = load_mono(&input)?;
            let range = VoiceRange::parse(&range).context("unknown --range")?;
            let track = pitch_track(&x, sr, range, 440.0);
            let (est_key, est_scale, conf) = estimate_key(&track);
            println!(
                "{}: {:.1} s, {:.0} Hz, {:.0}% voiced",
                input.display(),
                x.len() as f32 / sr,
                sr,
                100.0 * track.len() as f32 / (x.len() as f32 / sr * 100.0)
            );
            println!("estimated key: {} {} (confidence {:.2})", est_key.name(), scale_name(est_scale), conf);
            println!("average tuning of steady notes: {:+.0} cents from A440", tuning_offset(&track));
            let (k, s) = match key {
                Some(k) => (Key::parse(&k).context("unknown --key")?, Scale::parse(&scale).context("unknown --scale")?),
                None => (est_key, est_scale),
            };
            print_accuracy("intonation", &accuracy(&track, s.mask(k)));
        }
        Cmd::Process { input, output, opts, csv } => {
            let (x, sr) = load_mono(&input)?;
            let range = VoiceRange::parse(&opts.range).context("unknown --range")?;
            let before = pitch_track(&x, sr, range, opts.a4);
            let (key, scale) = if opts.key.eq_ignore_ascii_case("auto") {
                let (k, s, conf) = estimate_key(&before);
                println!("estimated key: {} {} (confidence {:.2})", k.name(), scale_name(s), conf);
                (k, s)
            } else {
                (
                    Key::parse(&opts.key).context("unknown --key")?,
                    Scale::parse(&opts.scale).context("unknown --scale")?,
                )
            };
            let settings = Settings {
                key,
                scale,
                retune_ms: opts.retune,
                humanize: opts.humanize / 100.0,
                steady: opts.steady / 100.0,
                flex: opts.flex / 100.0,
                vibrato: opts.vibrato / 100.0,
                range,
                tracking: opts.tracking / 100.0,
                a4: opts.a4,
                learn_offset: !opts.no_offset,
                ..Settings::default()
            };
            let mut engine = Engine::new(sr, settings);
            let mut csv = csv.map(std::fs::File::create).transpose()?.map(std::io::BufWriter::new);
            if let Some(w) = csv.as_mut() {
                writeln!(w, "time_s,input_hz,aperiodicity,voiced,input_midi,target_midi,correction_cents,vibrato")?;
            }

            let t0 = Instant::now();
            let hop = engine.hop();
            let mut y = Vec::with_capacity(x.len());
            let mut hops = Vec::with_capacity(x.len() / hop + 1);
            let mut delays = Vec::with_capacity(x.len() / hop + 1);
            for (i, &s) in x.iter().enumerate() {
                y.push(engine.process_sample(s));
                if (i + 1) % hop == 0 {
                    hops.push(engine.telemetry().control);
                    delays.push(engine.current_delay() as f32 / sr * 1000.0);
                    if let Some(w) = csv.as_mut() {
                        let t = engine.telemetry();
                        writeln!(
                            w,
                            "{:.4},{:.2},{:.3},{},{},{},{:.1},{:.2}",
                            i as f32 / sr,
                            t.estimate.freq,
                            t.estimate.aperiodicity,
                            t.control.voiced as u8,
                            t.control.input_midi.map(|m| format!("{m:.3}")).unwrap_or_default(),
                            t.control.target.map(|m| m.to_string()).unwrap_or_default(),
                            t.control.correction * 100.0,
                            t.control.vibrato
                        )?;
                    }
                }
            }
            let secs = t0.elapsed().as_secs_f32();
            write_wav(&output, &y, sr)?;
            println!(
                "processed {:.1} s of audio in {:.2} s ({:.0}x real time, {:.2}% of one core)",
                x.len() as f32 / sr,
                secs,
                x.len() as f32 / sr / secs,
                100.0 * secs / (x.len() as f32 / sr)
            );
            println!(
                "settings: {} {}, retune {} ms, steady {}%, flex {}%, humanize {}%, vibrato {}%; reported latency {:.2} ms",
                key.name(),
                scale_name(scale),
                opts.retune,
                opts.steady,
                opts.flex,
                opts.humanize,
                opts.vibrato,
                engine.latency_samples() as f32 / sr * 1000.0
            );
            delays.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let pct = |p: f32| delays[((delays.len() - 1) as f32 * p) as usize];
            println!(
                "plugin delay: median {:.1} ms, 99% ≤ {:.1} ms, max {:.1} ms",
                pct(0.5),
                pct(0.99),
                pct(1.0)
            );
            let after = pitch_track(&y, sr, range, opts.a4);
            let mask = scale.mask(key);
            print_accuracy("before", &accuracy(&before, mask));
            print_accuracy("after ", &accuracy(&after, mask));
            print_artifacts(&hops, hop as f32 / sr);
            if let Some(h) = hops.iter().rev().find(|h| h.voiced) {
                println!("learned singer offset at end: {:+.0} cents", h.offset * 100.0);
            }
            println!("wrote {}", output.display());
        }
    }
    Ok(())
}

fn scale_name(s: Scale) -> &'static str {
    match s {
        Scale::Minor => "minor",
        Scale::Chromatic => "chromatic",
        Scale::HarmonicMinor => "harmonic minor",
        Scale::MajorPentatonic => "major pentatonic",
        Scale::MinorPentatonic => "minor pentatonic",
        Scale::Blues => "blues",
        Scale::Dorian => "dorian",
        Scale::Mixolydian => "mixolydian",
        Scale::Custom(_) => "custom",
        Scale::Major => "major",
    }
}

fn load_mono(path: &Path) -> Result<(Vec<f32>, f32)> {
    let is_wav = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav"));
    let tmp;
    let wav_path = if is_wav {
        path.to_path_buf()
    } else {
        tmp = std::env::temp_dir().join(format!("stagetune-decode-{}.wav", std::process::id()));
        let status = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(path)
            // Keep the channels and average them below: ffmpeg's own float
            // downmix sums at −3 dB per channel, i.e. +3 dB for centred mono.
            .args(["-c:a", "pcm_f32le"])
            .arg(&tmp)
            .status()
            .context("running ffmpeg (needed for non-WAV input)")?;
        if !status.success() {
            bail!("ffmpeg failed to decode {}", path.display());
        }
        tmp.clone()
    };
    let mut r = hound::WavReader::open(&wav_path).with_context(|| format!("opening {}", wav_path.display()))?;
    let spec = r.spec();
    let ch = spec.channels as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect::<Result<_, _>>()?
        }
    };
    if !is_wav {
        let _ = std::fs::remove_file(&wav_path);
    }
    let mono = samples.chunks(ch).map(|c| c.iter().sum::<f32>() / ch as f32).collect();
    Ok((mono, spec.sample_rate as f32))
}

fn write_wav(path: &Path, y: &[f32], sr: f32) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: sr as u32,
        bits_per_sample: 24,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &s in y {
        w.write_sample((s.clamp(-1.0, 1.0) * 8_388_607.0) as i32)?;
    }
    w.finalize()?;
    Ok(())
}

/// Voiced pitch (MIDI) every 10 ms, with RMS weight.
fn pitch_track(x: &[f32], sr: f32, range: VoiceRange, a4: f32) -> Vec<(f32, f32)> {
    let (fmin, fmax) = range.bounds();
    let tau_max = (sr / fmin).ceil() as usize;
    let tau_min = (sr / fmax) as usize;
    let n = YinDetector::frame_len(tau_max);
    let mut det = YinDetector::new(sr, tau_max);
    let hop = (sr / 100.0) as usize;
    let mut out = Vec::new();
    let mut i = n;
    while i <= x.len() {
        let e = det.analyze(&x[i - n..i], tau_min, tau_max, tau_max, 0.15);
        if e.aperiodicity < 0.15 && e.rms > 0.003 && e.freq >= fmin && e.freq <= fmax {
            out.push((hz_to_midi(e.freq, a4), e.rms));
        }
        i += hop;
    }
    out
}

/// Average offset (cents) of the singer's steady notes from the A440 grid.
/// Circular mean, so −50 and +50 cents count as the same thing.
fn tuning_offset(track: &[(f32, f32)]) -> f32 {
    let (mut sx, mut sy) = (0f32, 0f32);
    for w in track.windows(2) {
        // Only frames where the pitch is steady (< 5 cents per 10 ms).
        if (w[1].0 - w[0].0).abs() < 0.05 {
            let a = std::f32::consts::TAU * w[1].0;
            sx += a.cos();
            sy += a.sin();
        }
    }
    sy.atan2(sx) / std::f32::consts::TAU * 100.0
}

/// Krumhansl–Kessler key profiles against a duration/level-weighted
/// pitch-class histogram, after removing the singer's average tuning offset
/// (a singer who is consistently a quarter-tone flat otherwise splits every
/// note across two pitch classes).
fn estimate_key(track: &[(f32, f32)]) -> (Key, Scale, f32) {
    let offset = tuning_offset(track) / 100.0;
    const MAJOR: [f32; 12] = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
    const MINOR: [f32; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];
    let mut hist = [0f32; 12];
    for &(m, w) in track {
        hist[((m - offset).round() as i32).rem_euclid(12) as usize] += w.sqrt();
    }
    let corr = |profile: &[f32; 12], root: usize| {
        let p: Vec<f32> = (0..12).map(|i| profile[(i + 12 - root) % 12]).collect();
        let mh = hist.iter().sum::<f32>() / 12.0;
        let mp = p.iter().sum::<f32>() / 12.0;
        let (mut num, mut dh, mut dp) = (0.0, 0.0, 0.0);
        for i in 0..12 {
            num += (hist[i] - mh) * (p[i] - mp);
            dh += (hist[i] - mh).powi(2);
            dp += (p[i] - mp).powi(2);
        }
        num / (dh * dp).sqrt().max(1e-9)
    };
    let mut best = (Key(0), Scale::Major, f32::MIN);
    for root in 0..12 {
        for (profile, scale) in [(&MAJOR, Scale::Major), (&MINOR, Scale::Minor)] {
            let c = corr(profile, root);
            if c > best.2 {
                best = (Key(root as u8), scale, c);
            }
        }
    }
    best
}

struct Accuracy {
    frames: usize,
    median_cents: f32,
    mean_cents: f32,
    within_10: f32,
    within_25: f32,
}

fn accuracy(track: &[(f32, f32)], mask: u16) -> Accuracy {
    let mut c: Vec<f32> = track
        .iter()
        .map(|&(m, _)| ((m - nearest_note(m, mask) as f32) * 100.0).abs())
        .collect();
    c.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = c.len().max(1) as f32;
    Accuracy {
        frames: c.len(),
        median_cents: c.get(c.len() / 2).copied().unwrap_or(0.0),
        mean_cents: c.iter().sum::<f32>() / n,
        within_10: 100.0 * c.iter().filter(|&&v| v <= 10.0).count() as f32 / n,
        within_25: 100.0 * c.iter().filter(|&&v| v <= 25.0).count() as f32 / n,
    }
}

/// Proxies for audible "auto-tune" artefacts, from the per-hop control trace.
fn print_artifacts(hops: &[stagetune_core::ControlOut], dt: f32) {
    // Warble: target A → B → A within 300 ms inside one voiced stretch.
    let mut warbles = 0;
    let mut changes: Vec<(usize, i32, i32)> = Vec::new();
    for (i, w) in hops.windows(2).enumerate() {
        match (w[0].target, w[1].target) {
            (Some(a), Some(b)) if a != b => changes.push((i, a, b)),
            (_, None) => changes.clear(),
            _ => {}
        }
        if changes.len() >= 2 {
            let (i0, a0, b0) = changes[changes.len() - 2];
            let (i1, a1, b1) = changes[changes.len() - 1];
            if i1 == i && a1 == b0 && b1 == a0 && (i1 - i0) as f32 * dt < 0.3 {
                warbles += 1;
            }
        }
    }
    // Glides: the note correction moving > 30 cents within 50 ms in the
    // body of a note, i.e. with the same target for the previous 150 ms.
    // This is where the ear hears a glide as a bend.
    let lag = (0.15 / dt).round() as usize;
    let span = (0.05 / dt).round() as usize;
    let same_note = |a: usize, b: usize| hops[a].target.is_some() && hops[a..=b].iter().all(|h| h.target == hops[a].target);
    let (mut yanks, mut voiced) = (0usize, 0usize);
    for i in lag..hops.len() {
        if same_note(i - lag, i) {
            voiced += 1;
            if (hops[i].glide - hops[i - span].glide).abs() > 0.3 {
                yanks += 1;
            }
        }
    }
    // Movement: pitch std-dev over 200 ms windows inside one note.
    let win = (0.2 / dt).round() as usize;
    let (mut in_sd, mut out_sd) = (Vec::new(), Vec::new());
    for c in hops.chunks(win) {
        if c.len() == win && c[0].target.is_some() && c.iter().all(|h| h.target == c[0].target && h.input_midi.is_some()) {
            let sd = |f: &dyn Fn(&stagetune_core::ControlOut) -> f32| {
                let v: Vec<f32> = c.iter().map(f).collect();
                let m = v.iter().sum::<f32>() / v.len() as f32;
                (v.iter().map(|x| (x - m).powi(2)).sum::<f32>() / v.len() as f32).sqrt() * 100.0
            };
            in_sd.push(sd(&|h| h.input_midi.unwrap()));
            out_sd.push(sd(&|h| h.input_midi.unwrap() + h.correction));
        }
    }
    // Note-level intonation: average output pitch of each held note (same
    // target for ≥ 150 ms) against its target. This is what the ear judges
    // as "in tune" when the voice keeps its natural movement.
    let min_len = (0.15 / dt).round() as usize;
    let (mut note_in, mut note_out) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < hops.len() {
        let mut j = i;
        while j < hops.len() && hops[j].target.is_some() && hops[j].target == hops[i].target {
            j += 1;
        }
        if j - i >= min_len {
            let t = hops[i].target.unwrap() as f32;
            let seg = &hops[i..j];
            let n = seg.len() as f32;
            let mi = seg.iter().map(|h| h.input_midi.unwrap_or(t)).sum::<f32>() / n;
            let mo = seg.iter().map(|h| h.input_midi.unwrap_or(t) + h.correction).sum::<f32>() / n;
            note_in.push(((mi - t) * 100.0).abs());
            note_out.push(((mo - t) * 100.0).abs());
        }
        i = j.max(i + 1);
    }
    let median = |v: &mut Vec<f32>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v.get(v.len() / 2).copied().unwrap_or(0.0)
    };
    println!(
        "artifacts: {} target flips A→B→A <300 ms, {:.1}% of in-note time with >30c glides per 50 ms, in-note pitch movement {:.1}c in → {:.1}c out",
        warbles,
        100.0 * yanks as f32 / voiced.max(1) as f32,
        median(&mut in_sd),
        median(&mut out_sd)
    );
    let n_notes = note_in.len();
    let within = |v: &[f32]| 100.0 * v.iter().filter(|&&c| c <= 15.0).count() as f32 / v.len().max(1) as f32;
    let (w_in, w_out) = (within(&note_in), within(&note_out));
    println!(
        "held notes ({}): average pitch {:.0}c → {:.0}c from target (median), {:.0}% → {:.0}% of notes within ±15c",
        n_notes,
        median(&mut note_in),
        median(&mut note_out),
        w_in,
        w_out
    );
}

fn print_accuracy(label: &str, a: &Accuracy) {
    println!(
        "{label}: median {:5.1} cents off, mean {:5.1}, {:5.1}% within ±10c, {:5.1}% within ±25c  ({} voiced frames)",
        a.median_cents, a.mean_cents, a.within_10, a.within_25, a.frames
    );
}
