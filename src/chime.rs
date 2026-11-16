//! Soft chimes on the controller's rumble actuators, two notes each. When a
//! controller connects, a fifth rising and left open. When ProTwo quits, a
//! fourth falling to rest, slower and softer.
//!
//! A note swells quickly and rings out, like a soft mallet on wood. The two
//! actuators take the notes in turn, so the first rings on under the second.
//! Each actuator can play two tones at once: the second plays the note's
//! octave, far softer and dying away faster. It rounds the start of the note
//! and carries it to the ear, since the actuators are weak below about
//! 330 Hz; what is left ringing is the low, warm part.

use crate::protocol::{freq_code, Band, Stereo, Voices};
use crate::voice::Frame;

/// One struck note: when (milliseconds), which MIDI key, how hard (0..=1).
type Strike = (u32, u8, f32);

// Kept low for warmth, but not below what the actuators can sound.
const E4: u8 = 64; // 330 Hz
const A4: u8 = 69; // 440 Hz
const B4: u8 = 71; // 494 Hz

// Soft: about a third of the strength the voice is driven at.
const ON: &[Strike] = &[(0, E4, 0.34), (240, B4, 0.38)];
// The way out is slower and quieter than the way in.
const OFF: &[Strike] = &[(0, A4, 0.31), (320, E4, 0.29)];

/// Time between two steps of the rendering.
const STEP_MS: u32 = 5;
/// A note reaches its peak in ATTACK_MS and then dies away with the time
/// constant RING_MS; its octave starts at OCTAVE of the note's level and dies
/// away with OCTAVE_RING_MS.
const ATTACK_MS: f32 = 22.0;
const RING_MS: f32 = 300.0;
const OCTAVE: f32 = 0.25;
const OCTAVE_RING_MS: f32 = 120.0;
/// A chime ends this long after its last strike, by when that note has all
/// but died away; a short fade takes it to silence.
const TAIL_MS: u32 = 800;
const FADE_MS: u32 = 120;

fn hertz(key: u8) -> f32 {
    440.0 * 2f32.powf((key as f32 - 69.0) / 12.0)
}

/// One actuator's two tones: the note it struck last and that note's octave.
fn voices(strike: Option<&Strike>, at: u32, fade: f32) -> Voices {
    let Some(&(start, key, level)) = strike else { return Voices::default() };
    let age = (at - start) as f32;
    let swell = (age / ATTACK_MS).min(1.0);
    let tone = |hz: f32, share: f32, ring: f32| -> Band {
        let loudness = level * share * swell * (-(age - ATTACK_MS).max(0.0) / ring).exp() * fade;
        (freq_code(hz), (loudness.clamp(0.0, 1.0) * 65535.0) as u16)
    };
    Voices { low: tone(hertz(key), 1.0, RING_MS), high: tone(hertz(key) * 2.0, OCTAVE, OCTAVE_RING_MS) }
}

fn render(strikes: &[Strike]) -> Vec<Frame> {
    let end = strikes.iter().map(|s| s.0).max().unwrap_or(0) + TAIL_MS;
    let mut frames: Vec<Frame> = Vec::with_capacity((end / STEP_MS) as usize + 1);
    for at in (0..=end).step_by(STEP_MS as usize) {
        let fade = ((end - at) as f32 / FADE_MS as f32).min(1.0);
        // The strikes alternate between the actuators: each plays its latest.
        let latest = |side: usize| strikes.iter().enumerate().filter(|(i, s)| i % 2 == side && s.0 <= at).map(|(_, s)| s).next_back();
        // The last step is plain silence.
        let sound = if at == end { Stereo::default() } else { Stereo { left: voices(latest(0), at, fade), right: voices(latest(1), at, fade) } };
        if frames.last().map(|f| f.sound) != Some(sound) {
            frames.push(Frame { at_us: at as u64 * 1000, sound });
        }
    }
    frames
}

/// The chime for a controller connecting, as a list of changes ending in silence.
pub fn on() -> Vec<Frame> {
    render(ON)
}

/// The chime for ProTwo quitting.
pub fn off() -> Vec<Frame> {
    render(OFF)
}

/// One sound after another, with a pause between them.
pub fn then(mut first: Vec<Frame>, pause_ms: u64, second: Vec<Frame>) -> Vec<Frame> {
    let start = first.last().map_or(0, |frame| frame.at_us) + pause_ms * 1000;
    first.extend(second.into_iter().map(|frame| Frame { at_us: frame.at_us + start, sound: frame.sound }));
    first
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(frames: &[Frame], ms: u64) -> Stereo {
        frames.iter().rev().find(|f| f.at_us <= ms * 1000).unwrap().sound
    }

    fn code(key: u8) -> u16 {
        freq_code(hertz(key))
    }

    #[test]
    fn connecting_rises_a_fifth() {
        let up = on();
        // The first note on the left actuator, the second on the right, the first still ringing under it.
        let both = at(&up, 300);
        assert_eq!((both.left.low.0, both.right.low.0), (code(E4), code(B4)));
        assert!(both.left.low.1 > 3000 && both.right.low.1 > both.left.low.1);
        assert_eq!(up.last().unwrap().at_us, 1_040_000);
    }

    #[test]
    fn quitting_falls_a_fourth_more_softly() {
        let (up, down) = (on(), off());
        let both = at(&down, 400);
        assert_eq!((both.left.low.0, both.right.low.0), (code(A4), code(E4)));
        let peak = |frames: &[Frame]| frames.iter().map(|f| f.sound.left.low.1.max(f.sound.right.low.1)).max().unwrap();
        assert!(peak(&down) < peak(&up));
    }

    #[test]
    fn the_chimes_are_soft_and_swell_in() {
        for chime in [on(), off()] {
            let loudest = |s: Stereo| [s.left.low.1, s.left.high.1, s.right.low.1, s.right.high.1].into_iter().max().unwrap();
            // Never more than about two fifths of full strength.
            assert!(chime.iter().all(|f| loudest(f.sound) < 26000));
            // Not struck hard: 10 ms in, the first note is under half its peak.
            assert!(at(&chime, 10).left.low.1 < at(&chime, 25).left.low.1 / 2 + 1500);
            // The octave is a small part of the note.
            let early = at(&chime, 25).left;
            assert!(early.high.1 > 0 && early.high.1 < early.low.1 / 3);
            // Starts from silence, ends in silence, and is already quiet just before.
            assert_eq!(loudest(chime[0].sound), 0);
            assert_eq!(chime.last().unwrap().sound, Stereo::default());
            assert!(loudest(chime[chime.len() - 2].sound) < 1500);
            assert!(chime.windows(2).all(|pair| pair[0].at_us < pair[1].at_us));
        }
    }

    #[test]
    fn one_sound_follows_another_after_a_pause() {
        let chime = on();
        let length = chime.last().unwrap().at_us;
        let both = then(chime.clone(), 200, on());
        assert_eq!(both.len(), chime.len() * 2);
        assert_eq!(both[chime.len()].at_us, length + 200_000);
        assert_eq!(both.last().unwrap().at_us, length * 2 + 200_000);
        assert!(both.windows(2).all(|pair| pair[0].at_us < pair[1].at_us));
    }
}
