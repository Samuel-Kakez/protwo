//! The background service: waits for a controller, bridges it to a virtual
//! gamepad and publishes its status for the window and the tray icon.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::chime;
use crate::voice;
use crate::controller::{play, Controller, Rumble, WINUSB_CLASS};
use crate::mapping;
use crate::picture::{self, Input};
use crate::protocol;
use crate::vigem::{self, Kind, Pad};
use crate::win::{self, Event, Hotplug};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadStatus {
    /// No controller, so no virtual pad.
    None,
    /// The virtual pad of this kind is plugged in.
    Active(Kind),
    /// ViGEmBus is missing or refused the pad.
    Unavailable,
}

/// What the window shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Live {
    pub connected: bool,
    pub pad: PadStatus,
}

impl Default for Live {
    fn default() -> Self {
        Live {
            connected: false,
            pad: PadStatus::None,
        }
    }
}

/// State shared between the bridge thread and the user interface.
pub struct Shared {
    pub live: Mutex<Live>,
    kind: AtomicU8,
    pub quit: AtomicBool,
    /// The latest buttons and sticks, packed (see `picture::Input`), for the
    /// live picture. Written on every report: one store, no lock.
    pub input: AtomicU64,
    /// How far the controller has turned in all, packed (see `picture::turned`),
    /// so the live picture can shift with it. One more store per report.
    pub turned: AtomicU64,
    stopped: AtomicBool,
    /// Called (from the bridge thread) when the connection status changes.
    notify_target: AtomicIsize,
    notify: fn(isize),
}

impl Shared {
    pub fn new(kind: Kind, notify: fn(isize)) -> Arc<Self> {
        Arc::new(Shared {
            live: Mutex::new(Live::default()),
            kind: AtomicU8::new(kind as u8),
            quit: AtomicBool::new(false),
            input: AtomicU64::new(0),
            turned: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
            notify_target: AtomicIsize::new(0),
            notify,
        })
    }

    pub fn kind(&self) -> Kind {
        if self.kind.load(Ordering::Relaxed) == Kind::Xbox360 as u8 {
            Kind::Xbox360
        } else {
            Kind::DualShock4
        }
    }

    pub fn set_kind(&self, kind: Kind) {
        self.kind.store(kind as u8, Ordering::Relaxed);
    }

    pub fn set_notify_target(&self, target: isize) {
        self.notify_target.store(target, Ordering::Release);
    }

    fn changed(&self) {
        let target = self.notify_target.load(Ordering::Acquire);
        if target != 0 {
            (self.notify)(target);
        }
    }

    fn set_status(&self, update: impl FnOnce(&mut Live)) {
        update(&mut self.live.lock().unwrap());
        self.changed();
    }

    /// True once the bridge has released the controller after `quit`.
    pub fn has_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
}

/// Runs until `shared.quit` is set while a controller is connected, or
/// forever otherwise (the process exits underneath it).
pub fn run(shared: Arc<Shared>) -> io::Result<()> {
    let hotplug = Hotplug::register(&WINUSB_CLASS)?;
    loop {
        if let Ok(Some(controller)) = Controller::connect() {
            let began = Instant::now();
            session(controller, &shared);
            shared.set_status(|live| *live = Live::default());
            if shared.quit.load(Ordering::Acquire) {
                shared.stopped.store(true, Ordering::Release);
                return Ok(());
            }
            // The stream can end with the controller still plugged in (after
            // the computer slept, say), and then nothing new arrives to wait
            // for: look again. Not after a session that failed at once.
            if began.elapsed() > Duration::from_secs(5) {
                continue;
            }
        }
        hotplug.wait();
    }
}

/// A plugged-in virtual pad together with its rumble listener thread.
struct ActivePad {
    pad: Arc<Pad>,
    event: Event,
    rumble: Arc<Rumble>,
}

impl ActivePad {
    fn plug(kind: Kind, rumble: &Arc<Rumble>) -> Option<Self> {
        let pad = Arc::new(Pad::plug(kind).ok()?);
        let event = Event::new().ok()?;
        let (listener, rumble) = (pad.clone(), rumble.clone());
        let stilled = rumble.clone();
        thread::spawn(move || {
            let Ok(event) = Event::new() else { return };
            // Until the pad is unplugged. The bus can refuse a request while
            // the pad is still settling in; that is not the end.
            while !listener.is_gone() {
                match listener.wait_rumble(&event) {
                    Ok((large, small)) => rumble.set(large as u16 * 257, small as u16 * 257),
                    Err(_) => thread::sleep(Duration::from_millis(50)),
                }
            }
        });
        Some(ActivePad { pad, event, rumble: stilled })
    }
}

impl Drop for ActivePad {
    fn drop(&mut self) {
        self.pad.unplug();
        // Whatever the game last asked for went with the pad.
        self.rumble.set(0, 0);
    }
}

fn pad_status(pad: &Option<ActivePad>) -> PadStatus {
    match pad {
        Some(p) => PadStatus::Active(p.pad.kind),
        None => PadStatus::Unavailable,
    }
}

/// Forwards input until the controller is unplugged or the app quits.
fn session(c: Controller, shared: &Shared) {
    let rumble = Arc::new(Rumble::start(c.hid.clone()));
    let mut kind = shared.kind();
    let mut pad = ActivePad::plug(kind, &rumble);
    let status = pad_status(&pad);
    shared.set_status(|live| {
        live.connected = true;
        live.pad = status;
    });

    if pad.is_some() {
        // A soft chime, then the name of the mode.
        play(rumble.clone(), chime::then(chime::on(), 250, voice::mode(kind)));
    }

    win::boost_current_thread();
    let mut buf = [0u8; 256];
    let mut last_xusb = [0xFFu8; vigem::XUSB_REPORT_LEN];
    // About 3 seconds of reports at 250 Hz.
    const MAX_SUBMIT_FAILURES: u32 = 750;
    let mut failures = 0u32;
    // About 2 seconds of reports.
    const RETRY_REPORTS: u32 = 500;
    let mut waited = 0u32;

    while let Ok(n) = c.hid.read(&mut buf) {
        if shared.quit.load(Ordering::Relaxed) {
            break;
        }
        let Some(state) = protocol::parse(&buf[..n], &c.cal) else { continue };
        shared.input.store(Input::pack(state.buttons, state.lx, state.ly, state.rx, state.ry), Ordering::Relaxed);
        // This thread is the only writer, so load and store need no loop.
        shared.turned.store(picture::turned(shared.turned.load(Ordering::Relaxed), state.gyro), Ordering::Relaxed);

        // Emulation mode changed in the window or the tray menu: swap the virtual pad.
        let wanted = shared.kind();
        if wanted != kind {
            kind = wanted;
            drop(pad.take()); // unplug the old pad before plugging the new one
            pad = ActivePad::plug(kind, &rumble);
            last_xusb = [0xFF; vigem::XUSB_REPORT_LEN];
            failures = 0;
            let status = pad_status(&pad);
            shared.set_status(|live| live.pad = status);
            if pad.is_some() {
                play(rumble.clone(), voice::mode(kind));
            }
        }

        // No virtual pad: the driver may have been installed since. Look
        // every couple of seconds.
        if pad.is_none() {
            waited += 1;
            if waited >= RETRY_REPORTS {
                waited = 0;
                if vigem::installed() {
                    pad = ActivePad::plug(kind, &rumble);
                    if pad.is_some() {
                        failures = 0;
                        shared.set_status(|live| live.pad = PadStatus::Active(kind));
                        play(rumble.clone(), voice::mode(kind));
                    }
                }
            }
        }

        if let Some(active) = &pad {
            let sent = match kind {
                Kind::Xbox360 => {
                    // Nothing changes between reports at rest: skip repeats.
                    let report = mapping::xusb_report(&state);
                    if report == last_xusb {
                        Ok(())
                    } else {
                        last_xusb = report;
                        active.pad.submit(&active.event, &report)
                    }
                }
                // Motion data changes every report, so always forward.
                Kind::DualShock4 => active.pad.submit(&active.event, &mapping::ds4_report(&state)),
            };
            match sent {
                Ok(()) => failures = 0,
                Err(_) => {
                    last_xusb = [0xFF; vigem::XUSB_REPORT_LEN];
                    failures += 1;
                    // The pad needs a moment after plug-in before it accepts
                    // reports; only give up on it if it never recovers.
                    if failures > MAX_SUBMIT_FAILURES {
                        pad = None;
                        shared.set_status(|live| live.pad = PadStatus::Unavailable);
                    }
                }
            }
        }
    }

    shared.input.store(0, Ordering::Relaxed);
    if shared.quit.load(Ordering::Relaxed) {
        // The app is closing with the controller still there: a soft chime out.
        let frames = chime::off();
        let length = frames.last().map_or(0, |frame| frame.at_us);
        play(rumble.clone(), frames);
        thread::sleep(Duration::from_micros(length) + Duration::from_millis(30));
    }
    rumble.new_sound();
    rumble.set(0, 0);
    // Dropping `pad` unplugs the virtual controller; dropping the last
    // `Rumble` reference stops its thread.
}
