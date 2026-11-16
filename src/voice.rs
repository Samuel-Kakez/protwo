//! What the controller says, on its own rumble actuators: the mode, when a
//! controller connects and when the mode changes ("DirectInput" or "XInput").
//!
//! The actuators cannot play recorded audio; between them they produce four
//! pure tones. The clips are therefore stored as tone tracks: every 5 ms, the
//! four overtones of the voice's pitch that matter most (see
//! tools/voice/build.mjs, which derives them from the recordings).

use crate::protocol::{Stereo, Voices};
use crate::vigem::Kind;

/// What the actuators play from `at_us` (microseconds from the start) onward.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub at_us: u64,
    pub sound: Stereo,
}

/// The mode names.
const DIRECTINPUT: &[u8] = include_bytes!("../assets/speech/directinput.bin");
const XINPUT: &[u8] = include_bytes!("../assets/speech/xinput.bin");

/// Bytes per slice: four tones, each a frequency code and an amplitude
/// (u16 LE). A clip starts with one header slice: the slice period in
/// microseconds (u32 LE), then padding.
const SLICE_BYTES: usize = 16;

fn clip(data: &[u8]) -> Vec<Frame> {
    let slice_us = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as u64;
    slices(&data[SLICE_BYTES..], slice_us)
}

/// The name of the mode, as a list of changes ending in silence.
pub fn mode(kind: Kind) -> Vec<Frame> {
    clip(match kind {
        Kind::DualShock4 => DIRECTINPUT,
        Kind::Xbox360 => XINPUT,
    })
}

/// Decodes tone slices, each lasting `slice_us`: four (frequency code,
/// amplitude) pairs from the lowest tone to the highest. The first and third
/// go to the left actuator, the second and fourth to the right.
pub fn slices(data: &[u8], slice_us: u64) -> Vec<Frame> {
    let slices = data.as_chunks::<SLICE_BYTES>().0;
    let mut frames: Vec<Frame> = Vec::with_capacity(slices.len() + 1);
    let mut end = 0;
    for (i, slice) in slices.iter().enumerate() {
        let tone = |k: usize| (u16::from_le_bytes([slice[k * 4], slice[k * 4 + 1]]), u16::from_le_bytes([slice[k * 4 + 2], slice[k * 4 + 3]]));
        let sound = Stereo { left: Voices { low: tone(0), high: tone(2) }, right: Voices { low: tone(1), high: tone(3) } };
        if frames.last().map(|f| f.sound) != Some(sound) {
            frames.push(Frame { at_us: i as u64 * slice_us, sound });
        }
        end = (i as u64 + 1) * slice_us;
    }
    frames.push(Frame { at_us: end, sound: Stereo::default() });
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_clip_is_short_loud_and_ends_silent() {
        let clips = [mode(Kind::DualShock4), mode(Kind::Xbox360)];
        for (n, frames) in clips.iter().enumerate() {
            let seconds = frames.last().unwrap().at_us as f32 / 1e6;
            assert!((0.4..1.5).contains(&seconds), "clip {n}: {seconds} s");
            let tones = |f: &Frame| [f.sound.left.low, f.sound.right.low, f.sound.left.high, f.sound.right.high];
            // As loud as it can be.
            assert_eq!(frames.iter().flat_map(tones).map(|t| t.1).max(), Some(65535), "clip {n}");
            assert!(frames.windows(2).all(|pair| pair[0].at_us < pair[1].at_us));
            // Every tone is a valid 10-bit frequency code, and the four are in rising order.
            for frame in frames {
                let sounding: Vec<u16> = tones(frame).iter().filter(|t| t.1 > 0).map(|t| t.0).collect();
                assert!(sounding.iter().all(|&code| code < 1024));
                assert!(sounding.windows(2).all(|pair| pair[0] <= pair[1]), "clip {n}: {sounding:?}");
            }
            assert_eq!(frames.last().unwrap().sound, Stereo::default());
        }
    }

    #[test]
    fn the_clips_all_differ() {
        assert!(DIRECTINPUT != XINPUT);
    }

}
