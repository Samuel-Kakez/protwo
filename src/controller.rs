//! Finds a Switch 2 Pro Controller, performs the USB handshake and exposes
//! its input stream and rumble.

use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows_sys::core::GUID;

use crate::hid::Hid;
use crate::voice::Frame;
use crate::protocol::{self as proto, Calibration, StickCal, Stereo, Voices};
use crate::usb::Usb;
use crate::win::interface_paths;

/// Device interface class the controller's firmware registers for its
/// WinUSB function (via Microsoft OS descriptors, so no driver install).
pub const WINUSB_CLASS: GUID = GUID {
    data1: 0x6F13725E,
    data2: 0xEF0E,
    data3: 0x4FD3,
    data4: [0xAE, 0x5F, 0xB2, 0xDE, 0x98, 0x9E, 0xC8, 0x25],
};

const HID_CLASS: GUID = GUID {
    data1: 0x4D1E55B2,
    data2: 0xF16F,
    data3: 0x11CF,
    data4: [0x88, 0xCB, 0x00, 0x11, 0x11, 0x00, 0x00, 0x30],
};

fn needle() -> String {
    format!("vid_{:04x}&pid_{:04x}", proto::VID, proto::PID_PRO2)
}

pub struct Controller {
    /// Kept open so the command channel stays ours while connected.
    pub usb: Usb,
    pub hid: Arc<Hid>,
    pub cal: Calibration,
    pub serial: String,
}

impl Controller {
    /// Returns Ok(None) when no controller is plugged in.
    pub fn connect() -> io::Result<Option<Self>> {
        let needle = needle();
        let Some(usb_path) = interface_paths(&WINUSB_CLASS, &needle).into_iter().next() else {
            return Ok(None);
        };

        // Right after plug-in the interfaces can take a moment to come up.
        let mut last_err = None;
        for _ in 0..10 {
            match Self::open(&usb_path, &needle) {
                Ok(c) => return Ok(Some(c)),
                Err(e) => last_err = Some(e),
            }
            thread::sleep(Duration::from_millis(200));
        }
        Err(last_err.unwrap())
    }

    fn open(usb_path: &[u16], needle: &str) -> io::Result<Self> {
        let usb = Usb::open(usb_path).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("cannot open the controller's USB interface ({e}); is another app using it?"),
            )
        })?;
        let hid_path = interface_paths(&HID_CLASS, needle)
            .into_iter()
            .next()
            .ok_or_else(|| io::Error::other("HID interface not found"))?;

        let mut cal = Calibration::default();
        let mut serial = String::new();

        if let Ok(block) = read_flash(&usb, proto::FLASH_SERIAL) {
            serial = block[2..18]
                .iter()
                .take_while(|&&b| b.is_ascii_graphic())
                .map(|&b| b as char)
                .collect();
        }
        if let Ok(block) = read_flash(&usb, proto::FLASH_GYRO_BIAS) {
            cal.set_gyro_bias(&block);
        }
        if let Ok(block) = read_flash(&usb, proto::FLASH_LEFT_STICK) {
            cal.left = StickCal::parse(&block[0x28..]);
        }
        if let Ok(block) = read_flash(&usb, proto::FLASH_RIGHT_STICK) {
            cal.right = StickCal::parse(&block[0x28..]);
        }
        // User calibration (done on a console) overrides the factory one.
        for (addr, stick) in [
            (proto::FLASH_USER_LEFT_STICK, &mut cal.left),
            (proto::FLASH_USER_RIGHT_STICK, &mut cal.right),
        ] {
            if let Ok(block) = read_flash(&usb, addr) {
                if block[0] == 0xB2 && block[1] == 0xA1 {
                    let user = StickCal::parse(&block[2..]);
                    if user.valid() {
                        *stick = user;
                    }
                }
            }
        }

        let mut reply = [0u8; 64];
        for cmd in proto::init_sequence() {
            usb.write(&cmd)?;
            // Replies are acknowledgements; a missing one is not fatal.
            let _ = usb.read(&mut reply);
        }
        let _ = usb.command(&proto::set_leds(0x01), &mut reply);

        let hid = Arc::new(Hid::open(&hid_path)?);
        Ok(Controller { usb, hid, cal, serial })
    }
}

fn read_flash(usb: &Usb, address: u32) -> io::Result<[u8; 0x40]> {
    let mut reply = [0u8; proto::FLASH_REPLY_LEN];
    let n = usb.command(&proto::flash_read(address), &mut reply)?;
    if n < proto::FLASH_REPLY_LEN {
        return Err(io::Error::other("short flash reply"));
    }
    let mut block = [0u8; 0x40];
    block.copy_from_slice(&reply[proto::FLASH_DATA_OFFSET..]);
    Ok(block)
}

/// Drives the rumble actuators from a dedicated thread, so the input path
/// never waits on an output report. The controller needs the current sample
/// re-sent continuously while vibrating; the thread sleeps when idle.
///
/// Two sources feed it: game rumble (fixed frequencies) and tones (the voice),
/// which take over while they play.
pub struct Rumble {
    shared: Arc<(Mutex<Inner>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct Inner {
    /// Game rumble amplitudes (low band, high band), 0..=65535.
    game: (u16, u16),
    tones: Option<Stereo>,
    /// Which sound may set the tones: the one started last.
    playing: u32,
    dirty: bool,
    stop: bool,
}

impl Inner {
    fn sound(&self) -> Stereo {
        self.tones.unwrap_or(Stereo::both(Voices {
            low: (proto::RUMBLE_LOW_FREQ, self.game.0),
            high: (proto::RUMBLE_HIGH_FREQ, self.game.1),
        }))
    }
}

const RUMBLE_INTERVAL: Duration = Duration::from_millis(12);

impl Rumble {
    pub fn start(hid: Arc<Hid>) -> Self {
        let shared = Arc::new((Mutex::new(Inner::default()), Condvar::new()));
        let pair = shared.clone();
        let thread = thread::spawn(move || {
            let (lock, wake) = &*pair;
            let mut seq = 0u8;
            let mut inner = lock.lock().unwrap();
            loop {
                let stopping = inner.stop;
                let sound = if stopping { Stereo::default() } else { inner.sound() };
                // The voice and the chime are driven harder than game rumble.
                let ceiling = if inner.tones.is_some() && !stopping { proto::TONE_MAX_AMP } else { proto::RUMBLE_MAX_AMP };
                let active = [sound.left, sound.right].iter().any(|v| v.low.1 != 0 || v.high.1 != 0);
                if !active && !inner.dirty && !stopping {
                    inner = wake.wait(inner).unwrap();
                    continue;
                }
                inner.dirty = false;
                drop(inner);

                let report = proto::rumble_report(seq, sound, ceiling);
                seq = seq.wrapping_add(1);
                let len = hid.out_len.clamp(1, report.len());
                // A failed write is not the end: while vibrating, the sample
                // is sent again after the interval anyway.
                let _ = hid.write(&report[..len]);
                if stopping {
                    break;
                }

                inner = lock.lock().unwrap();
                if active && !inner.dirty && !inner.stop {
                    // Re-send after the interval, or at once if something changes.
                    inner = wake.wait_timeout(inner, RUMBLE_INTERVAL).unwrap().0;
                }
            }
        });
        Rumble { shared, thread: Some(thread) }
    }

    /// Applies a change and wakes the thread if it made one.
    fn update<T>(&self, change: impl FnOnce(&mut Inner) -> (bool, T)) -> T {
        let (lock, wake) = &*self.shared;
        let mut inner = lock.lock().unwrap();
        let (changed, result) = change(&mut inner);
        if changed {
            inner.dirty = true;
            wake.notify_one();
        }
        result
    }

    /// Game rumble: amplitudes 0..=65535 for the low and high bands. Games
    /// repeat themselves a lot; a repeat sends nothing.
    pub fn set(&self, low: u16, high: u16) {
        self.update(|i| {
            let changed = i.game != (low, high);
            i.game = (low, high);
            (changed, ())
        });
    }

    /// Ends the sound that is playing, if any, and returns the number under
    /// which the next one sets its tones.
    pub fn new_sound(&self) -> u32 {
        self.update(|i| {
            i.playing = i.playing.wrapping_add(1);
            (i.tones.take().is_some(), i.playing)
        })
    }

    /// Tones override game rumble until cleared with `None`. Returns false,
    /// changing nothing, when a newer sound has taken over.
    pub fn set_tones(&self, sound: u32, tones: Option<Stereo>) -> bool {
        self.update(|i| {
            if i.playing != sound {
                return (false, false);
            }
            i.tones = tones;
            (true, true)
        })
    }
}

impl Drop for Rumble {
    /// Silences the actuators and stops the thread.
    fn drop(&mut self) {
        self.update(|i| {
            i.stop = true;
            (true, ())
        });
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Plays a sound on its own thread, in place of any still playing. It ends
/// early when another sound starts (`Rumble::new_sound`).
pub fn play(rumble: Arc<Rumble>, frames: Vec<Frame>) -> JoinHandle<()> {
    let sound = rumble.new_sound();
    thread::spawn(move || {
        let start = Instant::now();
        for frame in frames {
            let due = Duration::from_micros(frame.at_us);
            if let Some(wait) = due.checked_sub(start.elapsed()) {
                thread::sleep(wait);
            }
            if !rumble.set_tones(sound, Some(frame.sound)) {
                return;
            }
        }
        rumble.set_tones(sound, None);
    })
}
