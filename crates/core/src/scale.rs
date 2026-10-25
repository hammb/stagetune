//! Keys, scales and nearest-allowed-note lookup.

/// Root key as a pitch class (C = 0 … B = 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Key(pub u8);

impl Key {
    pub const NAMES: [&'static str; 12] =
        ["C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];

    pub fn parse(s: &str) -> Option<Key> {
        let s = s.trim().to_ascii_lowercase();
        let pc = match s.as_str() {
            "c" | "b#" => 0,
            "c#" | "db" => 1,
            "d" => 2,
            "d#" | "eb" => 3,
            "e" | "fb" => 4,
            "f" | "e#" => 5,
            "f#" | "gb" => 6,
            "g" => 7,
            "g#" | "ab" => 8,
            "a" => 9,
            "a#" | "bb" => 10,
            "b" | "cb" => 11,
            _ => return None,
        };
        Some(Key(pc))
    }

    pub fn name(self) -> &'static str {
        Self::NAMES[(self.0 % 12) as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scale {
    Chromatic,
    #[default]
    Major,
    Minor,
    HarmonicMinor,
    MajorPentatonic,
    MinorPentatonic,
    Blues,
    Dorian,
    Mixolydian,
    /// Pitch classes given explicitly as a 12-bit mask relative to C
    /// (bit 0 = C … bit 11 = B), used as is for every key.
    Custom(u16),
}

impl Scale {
    fn intervals(self) -> &'static [u8] {
        match self {
            Scale::Chromatic | Scale::Custom(_) => &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
            Scale::Major => &[0, 2, 4, 5, 7, 9, 11],
            Scale::Minor => &[0, 2, 3, 5, 7, 8, 10],
            Scale::HarmonicMinor => &[0, 2, 3, 5, 7, 8, 11],
            Scale::MajorPentatonic => &[0, 2, 4, 7, 9],
            Scale::MinorPentatonic => &[0, 3, 5, 7, 10],
            Scale::Blues => &[0, 3, 5, 6, 7, 10],
            Scale::Dorian => &[0, 2, 3, 5, 7, 9, 10],
            Scale::Mixolydian => &[0, 2, 4, 5, 7, 9, 10],
        }
    }

    pub fn parse(s: &str) -> Option<Scale> {
        Some(match s.trim().to_ascii_lowercase().replace(['-', '_', ' '], "").as_str() {
            "chromatic" | "chrom" => Scale::Chromatic,
            "major" | "maj" | "ionian" => Scale::Major,
            "minor" | "min" | "aeolian" | "naturalminor" => Scale::Minor,
            "harmonicminor" | "harmminor" => Scale::HarmonicMinor,
            "majorpentatonic" | "majpent" | "pentatonic" => Scale::MajorPentatonic,
            "minorpentatonic" | "minpent" => Scale::MinorPentatonic,
            "blues" => Scale::Blues,
            "dorian" => Scale::Dorian,
            "mixolydian" | "mixo" => Scale::Mixolydian,
            _ => return None,
        })
    }

    /// 12-bit mask of allowed pitch classes (bit n = pitch class n).
    pub fn mask(self, key: Key) -> u16 {
        if let Scale::Custom(m) = self {
            return if m & 0x0fff == 0 { 0x0fff } else { m & 0x0fff };
        }
        let mut m = 0u16;
        for &i in self.intervals() {
            m |= 1 << ((key.0 as u16 + i as u16) % 12);
        }
        m
    }
}

/// Nearest MIDI note to `midi` whose pitch class is set in `mask`.
/// Ties resolve downward. `mask` must be non-zero.
pub fn nearest_note(midi: f32, mask: u16) -> i32 {
    let center = midi.round() as i32;
    let mut best = center;
    let mut best_d = f32::INFINITY;
    for off in -6..=6 {
        let n = center + off;
        if mask & (1 << n.rem_euclid(12)) == 0 {
            continue;
        }
        let d = (midi - n as f32).abs();
        if d < best_d || (d == best_d && n < best) {
            best_d = d;
            best = n;
        }
    }
    best
}

/// How ambiguous `midi` is between the allowed notes around it: 0 when it
/// sits on an allowed note, 0.5 exactly halfway between the two nearest.
pub fn ambiguity(midi: f32, mask: u16) -> f32 {
    let mut lo = midi.floor() as i32;
    while mask & (1 << lo.rem_euclid(12)) == 0 {
        lo -= 1;
    }
    let mut hi = midi.ceil() as i32;
    while mask & (1 << hi.rem_euclid(12)) == 0 {
        hi += 1;
    }
    let (d_lo, d_hi) = (midi - lo as f32, hi as f32 - midi);
    if d_lo + d_hi <= 0.0 {
        0.0
    } else {
        d_lo.min(d_hi) / (d_lo + d_hi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g_major_mask() {
        let m = Scale::Major.mask(Key(7));
        // G A B C D E F#
        for pc in [7, 9, 11, 0, 2, 4, 6] {
            assert!(m & (1 << pc) != 0, "pc {pc} missing");
        }
        assert_eq!(m.count_ones(), 7);
    }

    #[test]
    fn nearest_in_c_major() {
        let m = Scale::Major.mask(Key(0));
        assert_eq!(nearest_note(61.4, m), 62); // C#+40c -> D
        assert_eq!(nearest_note(60.6, m), 60); // C+60c: C (60.6-60=0.6 < 62-60.6=1.4)
        assert_eq!(nearest_note(63.6, m), 64); // Eb+60c -> E
        assert_eq!(nearest_note(65.0, m), 65);
    }

    #[test]
    fn chromatic_rounds() {
        let m = Scale::Chromatic.mask(Key(0));
        assert_eq!(nearest_note(57.49, m), 57);
        assert_eq!(nearest_note(57.51, m), 58);
    }

    #[test]
    fn ambiguity_midpoints() {
        let c_major = Scale::Major.mask(Key(0));
        assert_eq!(ambiguity(60.0, c_major), 0.0);
        assert!((ambiguity(61.0, c_major) - 0.5).abs() < 1e-6); // C#: between C and D
        assert!((ambiguity(64.5, c_major) - 0.5).abs() < 1e-6); // E–F midpoint
        assert!((ambiguity(62.5, c_major) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn parse_names() {
        assert_eq!(Key::parse("Bb"), Some(Key(10)));
        assert_eq!(Key::parse("f#"), Some(Key(6)));
        assert_eq!(Scale::parse("harmonic-minor"), Some(Scale::HarmonicMinor));
    }
}
