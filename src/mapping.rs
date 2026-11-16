//! Translates controller state into virtual Xbox 360 / DualShock 4 reports.
//!
//! GL, GR and C are intentionally left unmapped in this version.

use crate::protocol::{btn, State};
use crate::vigem::{DS4_REPORT_LEN, XUSB_REPORT_LEN};

/// The face buttons are mapped by position: the bottom one (Nintendo B) is
/// Xbox A / PS Cross. In order: south, east, west, north.
const FACE: (u32, u32, u32, u32) = (btn::B, btn::A, btn::Y, btn::X);

pub fn xusb_report(s: &State) -> [u8; XUSB_REPORT_LEN] {
    let (south, east, west, north) = FACE;
    let map = [
        (btn::UP, 0x0001u16),
        (btn::DOWN, 0x0002),
        (btn::LEFT, 0x0004),
        (btn::RIGHT, 0x0008),
        (btn::PLUS, 0x0010),
        (btn::MINUS, 0x0020),
        (btn::LS, 0x0040),
        (btn::RS, 0x0080),
        (btn::L, 0x0100),
        (btn::R, 0x0200),
        (btn::HOME, 0x0400),
        (south, 0x1000),
        (east, 0x2000),
        (west, 0x4000),
        (north, 0x8000),
    ];
    let mut buttons = 0u16;
    for (mask, bit) in map {
        if s.pressed(mask) {
            buttons |= bit;
        }
    }

    let mut r = [0u8; XUSB_REPORT_LEN];
    r[0..2].copy_from_slice(&buttons.to_le_bytes());
    r[2] = if s.pressed(btn::ZL) { 255 } else { 0 };
    r[3] = if s.pressed(btn::ZR) { 255 } else { 0 };
    r[4..6].copy_from_slice(&s.lx.to_le_bytes());
    r[6..8].copy_from_slice(&s.ly.to_le_bytes());
    r[8..10].copy_from_slice(&s.rx.to_le_bytes());
    r[10..12].copy_from_slice(&s.ry.to_le_bytes());
    r
}

/// DS4 hat value from the four directions (8 = released).
fn hat(s: &State) -> u8 {
    let (u, d) = (s.pressed(btn::UP), s.pressed(btn::DOWN));
    let (l, r) = (s.pressed(btn::LEFT), s.pressed(btn::RIGHT));
    match (u && !d, d && !u, l && !r, r && !l) {
        (true, _, false, false) => 0,
        (true, _, _, true) => 1,
        (false, false, _, true) => 2,
        (_, true, _, true) => 3,
        (_, true, false, false) => 4,
        (_, true, true, _) => 5,
        (false, false, true, _) => 6,
        (true, _, true, _) => 7,
        _ => 8,
    }
}

/// What games are told about the virtual DualShock 4's battery: full (10 on
/// its 0..=10 scale), and on its cable. The real controller's charge is not
/// passed on; its readings over USB are not reliable.
const BATTERY_FULL: u8 = 10;
const ON_CABLE: u8 = 0x10;

pub fn ds4_report(s: &State) -> [u8; DS4_REPORT_LEN] {
    let (south, east, west, north) = FACE;
    let map = [
        (west, 1u16 << 4),
        (south, 1 << 5),
        (east, 1 << 6),
        (north, 1 << 7),
        (btn::L, 1 << 8),
        (btn::R, 1 << 9),
        (btn::ZL, 1 << 10),
        (btn::ZR, 1 << 11),
        (btn::MINUS, 1 << 12),
        (btn::PLUS, 1 << 13),
        (btn::LS, 1 << 14),
        (btn::RS, 1 << 15),
    ];
    let mut buttons = hat(s) as u16;
    for (mask, bit) in map {
        if s.pressed(mask) {
            buttons |= bit;
        }
    }
    let mut special = 0u8;
    if s.pressed(btn::HOME) {
        special |= 1; // PS
    }
    if s.pressed(btn::CAPTURE) {
        special |= 2; // touchpad click
    }

    let x = |v: i16| ((v as i32 + 32768) >> 8) as u8;
    let y = |v: i16| ((32767 - v as i32) >> 8) as u8; // DS4 Y grows downward

    let mut r = [0u8; DS4_REPORT_LEN];
    r[0] = x(s.lx);
    r[1] = y(s.ly);
    r[2] = x(s.rx);
    r[3] = y(s.ry);
    r[4..6].copy_from_slice(&buttons.to_le_bytes());
    r[6] = special;
    r[7] = if s.pressed(btn::ZL) { 255 } else { 0 };
    r[8] = if s.pressed(btn::ZR) { 255 } else { 0 };
    // DS4 timestamps tick every 5.33 microseconds.
    let ticks = (s.motion_time as u64 * 3 / 16) as u16;
    r[9..11].copy_from_slice(&ticks.to_le_bytes());
    r[11] = BATTERY_FULL;
    for i in 0..3 {
        // Same gyro scale as the DS4; accel is 4096/g here vs 8192/g there.
        r[12 + i * 2..14 + i * 2].copy_from_slice(&s.gyro[i].to_le_bytes());
        let accel = s.accel[i].saturating_mul(2);
        r[18 + i * 2..20 + i * 2].copy_from_slice(&accel.to_le_bytes());
    }
    r[29] = ON_CABLE | BATTERY_FULL;
    // No fingers on the (non-existent) touchpad: bit 7 set = not touching.
    for off in [34, 38, 43, 47, 52, 56] {
        r[off] = 0x80;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(buttons: u32) -> State {
        State { buttons, ..Default::default() }
    }

    #[test]
    fn xusb_buttons_go_by_position() {
        let s = state(btn::B | btn::PLUS | btn::ZL);
        let r = xusb_report(&s);
        assert_eq!(u16::from_le_bytes([r[0], r[1]]), 0x1000 | 0x0010); // A + Start
        assert_eq!((r[2], r[3]), (255, 0));
    }

    #[test]
    fn xusb_sticks_pass_through_with_y_up() {
        let s = State { lx: 1000, ly: -2000, rx: 32767, ry: -32767, ..Default::default() };
        let r = xusb_report(&s);
        assert_eq!(i16::from_le_bytes([r[4], r[5]]), 1000);
        assert_eq!(i16::from_le_bytes([r[6], r[7]]), -2000);
        assert_eq!(i16::from_le_bytes([r[8], r[9]]), 32767);
        assert_eq!(i16::from_le_bytes([r[10], r[11]]), -32767);
    }

    #[test]
    fn unmapped_buttons_do_nothing() {
        let s = state(btn::GL | btn::GR | btn::C);
        assert_eq!(xusb_report(&s), [0u8; XUSB_REPORT_LEN]);
        let idle = ds4_report(&state(0));
        assert_eq!(ds4_report(&s), idle);
    }

    #[test]
    fn ds4_hat_directions() {
        let cases = [
            (0, 8),
            (btn::UP, 0),
            (btn::UP | btn::RIGHT, 1),
            (btn::RIGHT, 2),
            (btn::DOWN | btn::RIGHT, 3),
            (btn::DOWN, 4),
            (btn::DOWN | btn::LEFT, 5),
            (btn::LEFT, 6),
            (btn::UP | btn::LEFT, 7),
            (btn::UP | btn::DOWN, 8),
            (btn::LEFT | btn::RIGHT | btn::UP, 0),
        ];
        for (buttons, expected) in cases {
            assert_eq!(hat(&state(buttons)), expected, "buttons {buttons:#x}");
        }
    }

    #[test]
    fn ds4_sticks_centre_and_invert_y() {
        let r = ds4_report(&state(0));
        assert_eq!(&r[0..4], &[128, 127, 128, 127]);
        let s = State { lx: 32767, ly: 32767, rx: -32767, ry: -32767, ..Default::default() };
        let r = ds4_report(&s);
        assert_eq!(&r[0..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn ds4_buttons_and_motion() {
        let s = State {
            buttons: btn::B | btn::HOME | btn::ZR | btn::UP,
            gyro: [10, -20, 30],
            accel: [100, 4096, -30000],
            ..Default::default()
        };
        let r = ds4_report(&s);
        assert_eq!(u16::from_le_bytes([r[4], r[5]]), (1 << 5) | (1 << 11)); // Cross + R2, hat up = 0
        assert_eq!(r[6], 1);
        assert_eq!((r[7], r[8]), (0, 255));
        assert_eq!(i16::from_le_bytes([r[14], r[15]]), -20);
        assert_eq!(i16::from_le_bytes([r[20], r[21]]), 8192);
        assert_eq!(i16::from_le_bytes([r[22], r[23]]), -32768); // saturates
        assert_eq!(r[11], 10);
        assert_eq!(r[29], 0x1A);
    }
}
