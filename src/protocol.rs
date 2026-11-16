//! Switch 2 Pro Controller wire protocol: commands, calibration, input
//! report parsing and rumble encoding. Pure logic, no I/O.
//!
//! See docs/PROTOCOL.md for the layout tables and sources.

pub const VID: u16 = 0x057E;
pub const PID_PRO2: u16 = 0x2069;

/// Input report carrying the full controller state (enabled by the handshake).
pub const REPORT_INPUT: u8 = 0x05;
/// Output report carrying HD rumble for the Pro Controller's two actuators.
pub const REPORT_RUMBLE: u8 = 0x02;

pub const FLASH_SERIAL: u32 = 0x13000;
pub const FLASH_GYRO_BIAS: u32 = 0x13040;
pub const FLASH_LEFT_STICK: u32 = 0x13080;
pub const FLASH_RIGHT_STICK: u32 = 0x130C0;
pub const FLASH_USER_LEFT_STICK: u32 = 0x1FC040;
pub const FLASH_USER_RIGHT_STICK: u32 = 0x1FC080;

/// Default actuator frequencies (raw protocol codes) and amplitude ceiling,
/// taken from SDL. The ceiling protects the actuators from overdrive.
pub const RUMBLE_LOW_FREQ: u16 = 0x112;
pub const RUMBLE_HIGH_FREQ: u16 = 0x187;
pub const RUMBLE_MAX_AMP: u32 = 29000;
/// Ceiling for the voice and the chime, which are quiet at the game-rumble
/// ceiling: about 4 dB louder. They last a few seconds at most, and are tones
/// in the hundreds of hertz rather than the strong low-frequency shaking the
/// lower ceiling is there for.
pub const TONE_MAX_AMP: u32 = 46000;

/// Button bits, as a little-endian u32 of input report bytes 5..9.
pub mod btn {
    pub const Y: u32 = 1 << 0;
    pub const X: u32 = 1 << 1;
    pub const B: u32 = 1 << 2;
    pub const A: u32 = 1 << 3;
    pub const R: u32 = 1 << 6;
    pub const ZR: u32 = 1 << 7;
    pub const MINUS: u32 = 1 << 8;
    pub const PLUS: u32 = 1 << 9;
    pub const RS: u32 = 1 << 10;
    pub const LS: u32 = 1 << 11;
    pub const HOME: u32 = 1 << 12;
    pub const CAPTURE: u32 = 1 << 13;
    pub const C: u32 = 1 << 14;
    pub const DOWN: u32 = 1 << 16;
    pub const UP: u32 = 1 << 17;
    pub const RIGHT: u32 = 1 << 18;
    pub const LEFT: u32 = 1 << 19;
    pub const L: u32 = 1 << 22;
    pub const ZL: u32 = 1 << 23;
    pub const GR: u32 = 1 << 24;
    pub const GL: u32 = 1 << 25;

    pub const NAMES: [(u32, &str); 21] = [
        (A, "A"),
        (B, "B"),
        (X, "X"),
        (Y, "Y"),
        (L, "L"),
        (R, "R"),
        (ZL, "ZL"),
        (ZR, "ZR"),
        (MINUS, "-"),
        (PLUS, "+"),
        (LS, "LS"),
        (RS, "RS"),
        (HOME, "Home"),
        (CAPTURE, "Capture"),
        (C, "C"),
        (UP, "Up"),
        (DOWN, "Down"),
        (LEFT, "Left"),
        (RIGHT, "Right"),
        (GL, "GL"),
        (GR, "GR"),
    ];
}

/// Builds a host-to-controller command: 8-byte header followed by the payload.
pub fn command(cmd: u8, sub: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&[cmd, 0x91, 0x00, sub, 0x00, payload.len() as u8, 0x00, 0x00]);
    out.extend_from_slice(payload);
    out
}

/// Reads one 0x40-byte flash block. The data starts at byte 0x10 of the reply.
pub fn flash_read(address: u32) -> Vec<u8> {
    let mut payload = [0u8; 8];
    payload[4..8].copy_from_slice(&address.to_le_bytes());
    command(0x02, 0x01, &payload)
}

pub const FLASH_REPLY_LEN: usize = 0x50;
pub const FLASH_DATA_OFFSET: usize = 0x10;

/// Sets the player LEDs (bit 0 = first LED).
pub fn set_leds(pattern: u8) -> Vec<u8> {
    command(0x09, 0x07, &[pattern, 0, 0, 0, 0, 0, 0, 0])
}

/// Feature bits: buttons | sticks | IMU | rumble.
const FEATURES: u8 = 0x01 | 0x02 | 0x04 | 0x20;

/// The handshake that makes the controller start streaming input reports.
/// Mirrors what the console sends, as captured by SDL's developers.
pub fn init_sequence() -> Vec<Vec<u8>> {
    vec![
        command(0x07, 0x01, &[]),
        // Select which features may be enabled.
        command(0x0C, 0x02, &[FEATURES, 0, 0, 0]),
        command(0x11, 0x01, &[]),
        // Rumble configuration.
        command(
            0x0A,
            0x08,
            &[
                0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x35, 0x00, 0x46, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ],
        ),
        // Enable the selected features.
        command(0x0C, 0x04, &[FEATURES, 0, 0, 0]),
        command(0x01, 0x0C, &[]),
        command(0x01, 0x01, &[]),
        // Enable the GL/GR grip buttons.
        command(0x08, 0x02, &[0x01, 0, 0, 0]),
        // Select input report 0x05.
        command(0x03, 0x0A, &[REPORT_INPUT, 0, 0, 0]),
        // Start streaming over USB (host address left as broadcast).
        command(0x03, 0x0D, &[0x01, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]),
    ]
}

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct AxisCal {
    pub neutral: u16,
    pub max: u16,
    pub min: u16,
}

impl AxisCal {
    fn valid(&self) -> bool {
        self.neutral != 0 && self.max != 0 && self.min != 0
    }

    /// Maps a raw 12-bit reading to -32767..=32767.
    pub fn apply(&self, raw: u16) -> i16 {
        let v = if self.valid() {
            let d = raw as f32 - self.neutral as f32;
            if d < 0.0 {
                d / self.min as f32
            } else {
                d / self.max as f32
            }
        } else {
            (raw as f32 - 2048.0) / 2048.0
        };
        (v * 32767.0).clamp(-32767.0, 32767.0) as i16
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct StickCal {
    pub x: AxisCal,
    pub y: AxisCal,
}

impl StickCal {
    /// Parses 9 bytes of packed 12-bit values: neutral, max, min (x then y each).
    pub fn parse(d: &[u8]) -> Self {
        let pair = |b: &[u8]| -> (u16, u16) {
            (
                b[0] as u16 | ((b[1] as u16 & 0x0F) << 8),
                (b[1] as u16 >> 4) | ((b[2] as u16) << 4),
            )
        };
        let (nx, ny) = pair(&d[0..3]);
        let (max_x, max_y) = pair(&d[3..6]);
        let (min_x, min_y) = pair(&d[6..9]);
        StickCal {
            x: AxisCal { neutral: nx, max: max_x, min: min_x },
            y: AxisCal { neutral: ny, max: max_y, min: min_y },
        }
    }

    pub fn valid(&self) -> bool {
        self.x.valid() && self.y.valid()
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Calibration {
    pub left: StickCal,
    pub right: StickCal,
    /// Gyro bias in raw sensor units, in the sensor's own axis order.
    pub gyro_bias: [f32; 3],
}

/// Raw gyro units per rad/s (int16 full scale is about 2000 deg/s).
pub const GYRO_LSB_PER_RAD: f32 = 32767.0 / 34.8;

impl Calibration {
    /// `block` is the 0x40-byte flash block at FLASH_GYRO_BIAS (floats, rad/s).
    pub fn set_gyro_bias(&mut self, block: &[u8]) {
        for (i, bias) in self.gyro_bias.iter_mut().enumerate() {
            let o = 4 + i * 4;
            let rad = f32::from_le_bytes([block[o], block[o + 1], block[o + 2], block[o + 3]]);
            // Ignore blank or corrupt flash rather than skewing the gyro.
            *bias = if rad.is_finite() && rad.abs() < 1.0 {
                rad * GYRO_LSB_PER_RAD
            } else {
                0.0
            };
        }
    }
}

/// Decoded controller state.
///
/// Sticks: -32767..=32767, right and up positive.
/// Motion uses the common gamepad frame (same as SDL and the DualShock 4),
/// controller lying flat: X to the right, Y up, Z toward the player.
/// Accel is 4096 units per g; gyro is about 16.4 units per deg/s.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct State {
    pub buttons: u32,
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
    pub accel: [i16; 3],
    pub gyro: [i16; 3],
    /// Sensor timestamp in microseconds (0 while the IMU is warming up).
    pub motion_time: u32,
}

impl State {
    pub fn pressed(&self, mask: u32) -> bool {
        self.buttons & mask != 0
    }
}

fn i16_at(d: &[u8], o: usize) -> i16 {
    i16::from_le_bytes([d[o], d[o + 1]])
}

fn sat(v: f32) -> i16 {
    v.clamp(-32768.0, 32767.0) as i16
}

/// Parses input report 0x05 (`d[0]` is the report ID). Returns None for
/// anything else.
pub fn parse(d: &[u8], cal: &Calibration) -> Option<State> {
    if d.len() < 64 || d[0] != REPORT_INPUT {
        return None;
    }
    let raw_lx = d[11] as u16 | ((d[12] as u16 & 0x0F) << 8);
    let raw_ly = (d[12] as u16 >> 4) | ((d[13] as u16) << 4);
    let raw_rx = d[14] as u16 | ((d[15] as u16 & 0x0F) << 8);
    let raw_ry = (d[15] as u16 >> 4) | ((d[16] as u16) << 4);

    let (ax, ay, az) = (i16_at(d, 0x31), i16_at(d, 0x33), i16_at(d, 0x35));
    let (gx, gy, gz) = (
        i16_at(d, 0x37) as f32 - cal.gyro_bias[0],
        i16_at(d, 0x39) as f32 - cal.gyro_bias[1],
        i16_at(d, 0x3B) as f32 - cal.gyro_bias[2],
    );

    Some(State {
        buttons: u32::from_le_bytes([d[5], d[6], d[7], d[8]]),
        lx: cal.left.x.apply(raw_lx),
        ly: cal.left.y.apply(raw_ly),
        rx: cal.right.x.apply(raw_rx),
        ry: cal.right.y.apply(raw_ry),
        // Sensor frame -> gamepad frame: (x, z, -y).
        accel: [ax, az, ay.saturating_neg()],
        gyro: [sat(gx), sat(gz), sat(-gy)],
        motion_time: u32::from_le_bytes([d[0x2B], d[0x2C], d[0x2D], d[0x2E]]),
    })
}

/// One rumble band: (frequency code, amplitude 0..=65535).
pub type Band = (u16, u16);

/// Two simultaneous tones, one per rumble band.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Voices {
    pub low: Band,
    pub high: Band,
}

/// What each of the Pro Controller's two actuators plays.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stereo {
    pub left: Voices,
    pub right: Voices,
}

impl Stereo {
    /// The same voices on both actuators.
    pub fn both(voices: Voices) -> Self {
        Stereo { left: voices, right: voices }
    }
}

/// Converts a frequency in hertz to the 10-bit rumble frequency code.
/// The scale is logarithmic, 96 steps per octave starting at 10 Hz
/// (community measurement, see docs/PROTOCOL.md).
pub fn freq_code(hz: f32) -> u16 {
    ((hz / 10.0).log2() * 96.0).round().clamp(0.0, 1023.0) as u16
}

/// Packs one actuator's rumble sample (10-bit frequencies, 16-bit amplitudes
/// of which the top 10 bits are sent) into 5 bytes.
fn encode_rumble(high_freq: u16, high_amp: u16, low_freq: u16, low_amp: u16) -> [u8; 5] {
    [
        (high_freq & 0xFF) as u8,
        (((high_amp >> 4) & 0xFC) | ((high_freq >> 8) & 0x03)) as u8,
        ((high_amp >> 12) | (low_freq << 4)) as u8,
        ((low_amp & 0xC0) | ((low_freq >> 4) & 0x3F)) as u8,
        (low_amp >> 8) as u8,
    ]
}

/// Builds the 64-byte rumble output report: one sample for the left
/// actuator and one for the right. Amplitudes are 0..=65535 and are scaled
/// to `ceiling`; `seq` increments with every report sent.
pub fn rumble_report(seq: u8, sound: Stereo, ceiling: u32) -> [u8; 64] {
    let scale = |a: u16| (a as u32 * ceiling.min(65535) / 65535) as u16;
    let mut r = [0u8; 64];
    r[0] = REPORT_RUMBLE;
    for (base, v) in [(0x01, sound.left), (0x11, sound.right)] {
        r[base] = 0x50 | (seq & 0x0F);
        let sample = encode_rumble(v.high.0, scale(v.high.1), v.low.0, scale(v.low.1));
        r[base + 1..base + 6].copy_from_slice(&sample);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_header() {
        assert_eq!(
            command(0x03, 0x0A, &[5, 0, 0, 0]),
            [0x03, 0x91, 0x00, 0x0A, 0x00, 0x04, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn flash_read_address_is_little_endian_at_byte_12() {
        let c = flash_read(0x1FC040);
        assert_eq!(c.len(), 16);
        assert_eq!(&c[..8], &[0x02, 0x91, 0x00, 0x01, 0x00, 0x08, 0x00, 0x00]);
        assert_eq!(&c[12..], &[0x40, 0xC0, 0x1F, 0x00]);
    }

    #[test]
    fn stick_calibration_unpacks_12_bit_pairs() {
        // neutral (0x800, 0x7FF), max (0x600, 0x5FF), min (0x601, 0x602)
        let cal = StickCal::parse(&[0x00, 0xF8, 0x7F, 0x00, 0xF6, 0x5F, 0x01, 0x26, 0x60]);
        assert_eq!(cal.x, AxisCal { neutral: 0x800, max: 0x600, min: 0x601 });
        assert_eq!(cal.y, AxisCal { neutral: 0x7FF, max: 0x5FF, min: 0x602 });
    }

    #[test]
    fn axis_maps_to_full_range_and_clamps() {
        let a = AxisCal { neutral: 2000, max: 1500, min: 1400 };
        assert_eq!(a.apply(2000), 0);
        assert_eq!(a.apply(3500), 32767);
        assert_eq!(a.apply(600), -32767);
        assert_eq!(a.apply(4095), 32767);
        assert_eq!(a.apply(0), -32767);
        // Uncalibrated fallback is centred on 2048.
        assert_eq!(AxisCal::default().apply(2048), 0);
    }

    #[test]
    fn parses_buttons_sticks_and_motion() {
        let mut d = [0u8; 64];
        d[0] = REPORT_INPUT;
        d[5] = 0x08 | 0x80; // A + ZR
        d[6] = 0x10; // Home
        d[7] = 0x02; // Up
        d[8] = 0x02; // GL
        // Left stick raw x = 0xABC, y = 0x123
        d[11] = 0xBC;
        d[12] = 0x3A;
        d[13] = 0x12;
        d[0x20] = 0xA0; // 4000 mV
        d[0x21] = 0x0F;
        d[0x31..0x33].copy_from_slice(&100i16.to_le_bytes()); // accel x
        d[0x33..0x35].copy_from_slice(&200i16.to_le_bytes()); // accel y
        d[0x35..0x37].copy_from_slice(&4096i16.to_le_bytes()); // accel z
        d[0x39..0x3B].copy_from_slice(&50i16.to_le_bytes()); // gyro y

        let cal = Calibration {
            left: StickCal {
                x: AxisCal { neutral: 0x800, max: 0x2BC, min: 0x700 },
                y: AxisCal { neutral: 0x800, max: 0x700, min: 0x6DD },
            },
            ..Default::default()
        };
        let s = parse(&d, &cal).unwrap();
        assert!(s.pressed(btn::A) && s.pressed(btn::ZR) && s.pressed(btn::HOME));
        assert!(s.pressed(btn::UP) && s.pressed(btn::GL));
        assert!(!s.pressed(btn::B) && !s.pressed(btn::GR));
        assert_eq!(s.lx, 32767);
        assert_eq!(s.ly, -32767);
        assert_eq!(s.accel, [100, 4096, -200]);
        assert_eq!(s.gyro, [0, 0, -50]);
    }

    #[test]
    fn rejects_other_reports() {
        let mut d = [0u8; 64];
        d[0] = 0x09;
        assert!(parse(&d, &Calibration::default()).is_none());
        assert!(parse(&[REPORT_INPUT; 10], &Calibration::default()).is_none());
    }

    #[test]
    fn frequency_codes_follow_the_log_scale() {
        assert_eq!(freq_code(10.0), 0);
        assert_eq!(freq_code(20.0), 96);
        assert_eq!(freq_code(160.0), 384);
        assert_eq!(freq_code(440.0), 524);
        assert_eq!(freq_code(1.0), 0);
        assert_eq!(freq_code(1.0e9), 1023);
    }

    #[test]
    fn rumble_report_layout() {
        let voices = |amp: u16| Voices { low: (RUMBLE_LOW_FREQ, amp), high: (RUMBLE_HIGH_FREQ, amp) };
        let r = rumble_report(0x13, Stereo::both(voices(0)), RUMBLE_MAX_AMP);
        assert_eq!(r[0], REPORT_RUMBLE);
        assert_eq!(r[1], 0x53);
        // Zero amplitude leaves only the frequency bits.
        assert_eq!(&r[2..7], &[0x87, 0x01, 0x20, 0x11, 0x00]);
        // Both actuators carry the same sample.
        assert_eq!(&r[0x11..0x17], &r[0x01..0x07]);

        let full = rumble_report(0, Stereo::both(voices(65535)), RUMBLE_MAX_AMP);
        let amp = RUMBLE_MAX_AMP as u16; // 29000 = 0x7148
        assert_eq!(full[6], (amp >> 8) as u8);
        assert_eq!(full[3], (((amp >> 4) & 0xFC) | 0x01) as u8);

        // Left and right are independent: silent left, full right.
        let split = rumble_report(0, Stereo { left: voices(0), right: voices(65535) }, RUMBLE_MAX_AMP);
        // The voice's higher ceiling scales the same sample up.
        let loud = rumble_report(0, Stereo::both(voices(65535)), TONE_MAX_AMP);
        assert_eq!(loud[6], (TONE_MAX_AMP >> 8) as u8);
        assert_eq!(&split[2..7], &r[2..7]);
        assert_eq!(&split[0x12..0x17], &full[2..7]);
    }
}
