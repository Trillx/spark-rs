//! The meeting pop-up's chime, played as a meeting appears in it, as Spark
//! plays its own. Ours is made here rather than copied, with the same shape:
//! a soft C, then an E 40 ms later over a low C and G, gone in about half a
//! second. Windows plays it (`PlaySound`), so it sounds even before the
//! pop-up's page has loaded, and without anything taking the focus.

// Elsewhere it's only made in the tests.
#![cfg_attr(not(windows), allow(dead_code))]

use std::f32::consts::TAU;

const RATE: u32 = 44_100;
const LENGTH: f32 = 0.7;
/// The loudest it gets, of full scale. Spark plays its own at 12% volume;
/// this holds its notes about as loud as that.
const PEAK: f32 = 0.075;

struct Note {
    /// Seconds in.
    start: f32,
    hz: f32,
    level: f32,
    /// Seconds before it starts to die away.
    held: f32,
    /// Struck like a mallet, or swelling in quietly underneath.
    struck: bool,
}

const NOTES: [Note; 4] = [
    // C5, then E5.
    Note {
        start: 0.0,
        hz: 523.25,
        level: 1.0,
        held: 0.20,
        struck: true,
    },
    Note {
        start: 0.04,
        hz: 659.25,
        level: 1.0,
        held: 0.24,
        struck: true,
    },
    // C4 and G4.
    Note {
        start: 0.0,
        hz: 261.63,
        level: 0.8,
        held: 0.17,
        struck: false,
    },
    Note {
        start: 0.06,
        hz: 392.0,
        level: 0.6,
        held: 0.08,
        struck: false,
    },
];

/// Plays the chime and returns at once. Only Windows has a sound for now.
pub fn play() {
    #[cfg(windows)]
    {
        use std::sync::OnceLock;

        use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
        use windows::core::PCWSTR;

        // Windows reads it while it plays, after this returns.
        static WAV: OnceLock<Vec<u8>> = OnceLock::new();
        let wav = WAV.get_or_init(|| wav(&samples()));
        // SAFETY: with SND_MEMORY the "name" is a whole WAV file in memory,
        // and this one lasts as long as the app.
        unsafe {
            let _ = PlaySoundW(
                PCWSTR(wav.as_ptr().cast()),
                None,
                SND_ASYNC | SND_MEMORY | SND_NODEFAULT,
            );
        }
    }
}

fn samples() -> Vec<f32> {
    let mut out = vec![0.0; (LENGTH * RATE as f32) as usize];
    for note in &NOTES {
        for (i, sample) in out.iter_mut().enumerate() {
            let t = i as f32 / RATE as f32 - note.start;
            if t >= 0.0 {
                *sample += note.level * note.at(t);
            }
        }
    }
    let loudest = out.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    let fade = (0.02 * RATE as f32) as usize;
    let len = out.len();
    for (i, sample) in out.iter_mut().enumerate() {
        *sample *= PEAK / loudest * ((len - i) as f32 / fade as f32).min(1.0);
    }
    out
}

impl Note {
    /// The note `t` seconds after it starts. A struck one is up in 4 ms,
    /// louder and brighter (a little of the octave) for the strike's first
    /// few tens of ms; one that swells takes 20 ms. Both hold, then ring away.
    fn at(&self, t: f32) -> f32 {
        let (rise, strike) = if self.struck {
            (0.004, (-t / 0.015).exp())
        } else {
            (0.02, 0.0)
        };
        let ring = if t < self.held {
            1.0
        } else {
            (-(t - self.held) / 0.06).exp()
        };
        let tone = (TAU * self.hz * t).sin() + 0.25 * strike * (2.0 * TAU * self.hz * t).sin();
        (t / rise).min(1.0) * (1.0 + 2.0 * strike) * ring * tone
    }
}

/// A 16-bit mono WAV file.
fn wav(samples: &[f32]) -> Vec<u8> {
    let data = samples.len() as u32 * 2;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1_u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes()); // bytes a second
    out.extend_from_slice(&2_u16.to_le_bytes()); // bytes a sample
    out.extend_from_slice(&16_u16.to_le_bytes()); // bits a sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&((s * f32::from(i16::MAX)).round() as i16).to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_soft_short_and_ends_silent() {
        let s = samples();
        let loudest = s.iter().fold(0.0_f32, |m, x| m.max(x.abs()));
        assert!((loudest - PEAK).abs() < 1e-4);
        assert!(s[s.len() - 1].abs() < 1e-6);
        // Next to nothing after half a second.
        let tail = s[RATE as usize / 2..]
            .iter()
            .fold(0.0_f32, |m, x| m.max(x.abs()));
        assert!(tail < PEAK * 0.05, "{tail}");
    }

    #[test]
    fn makes_a_wav_file_windows_can_read() {
        let file = wav(&samples());
        assert_eq!(&file[..4], b"RIFF");
        assert_eq!(&file[8..16], b"WAVEfmt ");
        assert_eq!(
            u32::from_le_bytes(file[4..8].try_into().unwrap()) as usize,
            file.len() - 8
        );
        assert_eq!(
            u32::from_le_bytes(file[40..44].try_into().unwrap()) as usize,
            file.len() - 44
        );
    }
}
