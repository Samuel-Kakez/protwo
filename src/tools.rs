//! Developer tools, left out of the normal build: `record` measures what the
//! actuators really play, `render` draws the window to a PNG. Build them in
//! with `cargo build --release --features tools`.

use std::io;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Graphics::Gdi::{CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject};

use crate::bridge::{Live, PadStatus};
use crate::cli::drain_input;
use crate::controller::{play, Controller, Rumble};
use crate::gfx::{Bitmap, Session};
use crate::picture::{self, Input, Picture};
use crate::protocol::btn;
use crate::view;
use crate::vigem::Kind;
use crate::voice;

/// Records from a microphone into memory (48 kHz, mono, 16-bit).
struct Recorder {
    device: windows_sys::Win32::Media::Audio::HWAVEIN,
    header: Box<windows_sys::Win32::Media::Audio::WAVEHDR>,
    samples: Vec<i16>,
}

const RECORD_RATE: u32 = 48_000;

impl Recorder {
    /// Starts recording from the first input whose name contains `name`.
    fn start(name: &str, length: Duration) -> io::Result<Self> {
        use windows_sys::Win32::Media::Audio::*;
        unsafe {
            let mut found = None;
            for id in 0..waveInGetNumDevs() {
                let mut caps: WAVEINCAPSW = std::mem::zeroed();
                waveInGetDevCapsW(id as usize, &mut caps, std::mem::size_of::<WAVEINCAPSW>() as u32);
                // Copied out first: the structure is packed.
                let name_units = caps.szPname;
                let label = String::from_utf16_lossy(&name_units);
                if label.to_lowercase().contains(&name.to_lowercase()) {
                    found = Some(id);
                    break;
                }
            }
            let id = found.ok_or_else(|| io::Error::other(format!("no microphone named '{name}'")))?;
            let format = WAVEFORMATEX {
                wFormatTag: 1, // PCM
                nChannels: 1,
                nSamplesPerSec: RECORD_RATE,
                nAvgBytesPerSec: RECORD_RATE * 2,
                nBlockAlign: 2,
                wBitsPerSample: 16,
                cbSize: 0,
            };
            let mut device: HWAVEIN = std::ptr::null_mut();
            if waveInOpen(&mut device, id, &format, 0, 0, 0) != 0 {
                return Err(io::Error::other("cannot open the microphone"));
            }
            let mut samples = vec![0i16; (length.as_secs_f32() * RECORD_RATE as f32) as usize];
            let mut header: Box<WAVEHDR> = Box::new(std::mem::zeroed());
            header.lpData = samples.as_mut_ptr() as *mut u8;
            header.dwBufferLength = (samples.len() * 2) as u32;
            let size = std::mem::size_of::<WAVEHDR>() as u32;
            waveInPrepareHeader(device, &mut *header, size);
            waveInAddBuffer(device, &mut *header, size);
            waveInStart(device);
            Ok(Recorder { device, header, samples })
        }
    }

    /// Stops and writes what was captured as a WAV file.
    fn save(mut self, path: &str) -> io::Result<()> {
        use windows_sys::Win32::Media::Audio::*;
        let recorded = unsafe {
            waveInReset(self.device);
            let size = std::mem::size_of::<WAVEHDR>() as u32;
            waveInUnprepareHeader(self.device, &mut *self.header, size);
            waveInClose(self.device);
            self.header.dwBytesRecorded as usize / 2
        };
        self.samples.truncate(recorded);
        let bytes = (self.samples.len() * 2) as u32;
        let mut file = Vec::with_capacity(44 + bytes as usize);
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&(36 + bytes).to_le_bytes());
        file.extend_from_slice(b"WAVEfmt ");
        file.extend_from_slice(&16u32.to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&RECORD_RATE.to_le_bytes());
        file.extend_from_slice(&(RECORD_RATE * 2).to_le_bytes());
        file.extend_from_slice(&2u16.to_le_bytes());
        file.extend_from_slice(&16u16.to_le_bytes());
        file.extend_from_slice(b"data");
        file.extend_from_slice(&bytes.to_le_bytes());
        for s in &self.samples {
            file.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(path, file)
    }
}

/// Plays a tone track on the actuators while recording a microphone, to
/// measure what the controller really produces. The recording starts 300 ms
/// before the track.
///   protwo record <track.bin> <out.wav> [slice microseconds] [microphone name]
/// The track is slices only, with no header: 16 bytes each, four (frequency
/// code, amplitude) pairs for left low, right low, left high, right high.
pub fn record(c: Controller, args: &[String]) -> io::Result<()> {
    let (Some(track), Some(out)) = (args.first(), args.get(1)) else {
        return Err(io::Error::other("usage: protwo record <track.bin> <out.wav> [slice us] [microphone]"));
    };
    let slice_us: u64 = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(10_000);
    let microphone = args.get(3).map_or("fifine", |s| s.as_str());
    let frames = voice::slices(&std::fs::read(track)?, slice_us);
    let length = Duration::from_micros(frames.last().map_or(0, |frame| frame.at_us));

    let recorder = Recorder::start(microphone, length + Duration::from_millis(900))?;
    let rumble = Arc::new(Rumble::start(c.hid.clone()));
    drain_input(&c);
    thread::sleep(Duration::from_millis(300));
    let _ = play(rumble, frames).join();
    thread::sleep(Duration::from_millis(500));
    recorder.save(out)?;
    println!("Recorded {:.2} s to {out}.", (length + Duration::from_millis(800)).as_secs_f32());
    Ok(())
}


/// Renders the window contents to a PNG with made-up status, to check the
/// drawing without a controller or a screen.
pub fn render(args: &[String]) -> io::Result<()> {
    let path = args.first().ok_or_else(|| io::Error::other("usage: protwo render <file> [idle|pressed|top|back|turned|icon|bench|film]"))?;
    if args.get(1).is_some_and(|a| a == "icon") {
        // The icon at several sizes side by side: connected on top, idle below.
        let sizes = [128, 64, 48, 32, 24, 16];
        let _session = Session::start().ok_or_else(|| io::Error::other("GDI+ failed to start"))?;
        let sheet = Bitmap::new(sizes.iter().map(|s| s + 16).sum(), 2 * 144).ok_or_else(|| io::Error::other("cannot create bitmap"))?;
        {
            let canvas = sheet.canvas().ok_or_else(|| io::Error::other("cannot draw"))?;
            canvas.clear(view::BACKGROUND);
            let mut x = 8;
            for size in sizes {
                for (row, connected) in [(0, true), (1, false)] {
                    if let Some(icon) = Bitmap::new(size, size) {
                        if let Some(c) = icon.canvas() {
                            view::draw_icon(&c, size as f32, connected);
                        }
                        canvas.bitmap(&icon, x as f32, (row * 144 + 8) as f32);
                    }
                }
                x += size + 16;
            }
        }
        return if sheet.save_png(path) { Ok(()) } else { Err(io::Error::other("cannot write the PNG")) };
    }
    if args.get(1).is_some_and(|a| a == "bench") {
        // How long a frame takes at 100% scale: one where the picture itself
        // changed, and one where it only shifted.
        let _session = Session::start().ok_or_else(|| io::Error::other("GDI+ failed to start"))?;
        let bitmap = Bitmap::new(view::WIDTH as i32, view::HEIGHT as i32).ok_or_else(|| io::Error::other("cannot create bitmap"))?;
        let (w, h) = (view::PICTURE.2 as i32, view::PICTURE.3 as i32);
        let drawn = Bitmap::new(w, h).ok_or_else(|| io::Error::other("cannot create bitmap"))?;
        let mut picture = Picture::load().ok_or_else(|| io::Error::other("cannot load the picture"))?;
        // A screen-like surface in memory to present to.
        let (screen, surface) = unsafe {
            let display = GetDC(std::ptr::null_mut());
            let screen = CreateCompatibleDC(display);
            let surface = CreateCompatibleBitmap(display, view::WIDTH as i32, view::HEIGHT as i32);
            SelectObject(screen, surface);
            ReleaseDC(std::ptr::null_mut(), display);
            (screen, surface)
        };
        let frames = 300;
        let (mut changing, mut shifting) = (Duration::ZERO, Duration::ZERO);
        let (mut placed, mut pixels) = (Vec::new(), Vec::new());
        let room = (picture::MAX_SHIFT * view::PICTURE_SCALE).ceil() as usize + 1;
        for n in 0..frames {
            let t = n as f32 * 0.05;
            picture.pose([t.sin() * 0.3, 0.0, t.cos() * 0.3]);
            // A stick circling and a button held: what changes in a typical frame.
            picture.settle(Input { buttons: btn::A, left: (t.cos(), t.sin()), ..Input::default() });
            let start = Instant::now();
            picture.compose();
            if let Some(dirty) = picture.dirty() {
                let own = drawn.canvas().ok_or_else(|| io::Error::other("cannot draw"))?;
                let (x, y, w, h) = view::draw_picture(&own, picture.frame(), w, h, dirty);
                drop(own);
                drawn.read(&mut pixels, x, y, w, h);
            }
            changing += start.elapsed();
            let start = Instant::now();
            let shift = picture.shift();
            picture::place(&pixels, w as usize, h as usize, room, (shift.0 * view::PICTURE_SCALE, shift.1 * view::PICTURE_SCALE), 0.0, &mut placed);
            let (x, y) = (view::PICTURE.0 as i32 - room as i32, view::PICTURE.1 as i32 - room as i32);
            bitmap.write(&placed, w as usize + room * 2, h as usize + room * 2, x, y);
            bitmap.present(screen, x, y, x + w + room as i32 * 2, y + h + room as i32 * 2);
            shifting += start.elapsed();
        }
        unsafe {
            DeleteDC(screen);
            DeleteObject(surface);
        }
        let ms = |d: Duration| d.as_secs_f64() * 1000.0 / frames as f64;
        println!("picture changed (a stick and a button): {:.2} ms more; shifted and put on screen: {:.2} ms per frame", ms(changing), ms(shifting));
        return Ok(());
    }
    if args.get(1).is_some_and(|a| a == "film") {
        return record_film(path);
    }
    let idle = args.get(1).is_some_and(|a| a == "idle");
    let scale = 2.0f32;
    let _session = Session::start().ok_or_else(|| io::Error::other("GDI+ failed to start"))?;
    let live = if idle { Live::default() } else { CONNECTED };
    // "pressed" shows a handful of inputs held, "top" and "back" the side views.
    let input = match args.get(1).map(|a| a.as_str()) {
        Some("pressed") => Input { buttons: btn::A | btn::Y | btn::HOME | btn::MINUS | btn::UP | btn::RS, left: (0.8, -0.6), right: (-0.7, 0.7), ..Input::default() },
        Some("top") => Input { buttons: btn::ZR | btn::L, ..Input::default() },
        Some("back") => Input { buttons: btn::GR, ..Input::default() },
        _ => Input::default(),
    };
    let mut picture = Picture::load().ok_or_else(|| io::Error::other("cannot load the picture"))?;
    picture.settle(input);
    // "turned" shows the picture shifted, as while the controller turns.
    if args.get(1).is_some_and(|a| a == "turned") {
        picture.pose([0.6, 0.0, 0.6]);
    }
    if !window(&mut picture, &live, scale)?.save_png(path) {
        return Err(io::Error::other("cannot write the PNG"));
    }
    Ok(())
}

const CONNECTED: Live = Live { connected: true, pad: PadStatus::Active(Kind::DualShock4) };

/// Draws the whole window as the app does: everything around the picture,
/// then the picture at its drawn size, put in place.
fn window(picture: &mut Picture, live: &Live, scale: f32) -> io::Result<Bitmap> {
    let failed = || io::Error::other("cannot draw");
    let look = view::Look::settled(live, view::Settings { kind: Kind::DualShock4 }, None);
    let bitmap = Bitmap::new((view::WIDTH * scale) as i32, (view::HEIGHT * scale) as i32).ok_or_else(failed)?;
    {
        let canvas = bitmap.canvas().ok_or_else(failed)?;
        canvas.scale(scale);
        view::draw(&canvas, live, look);
    }
    let (w, h) = ((view::PICTURE.2 * scale) as i32, (view::PICTURE.3 * scale) as i32);
    let drawn = Bitmap::new(w, h).ok_or_else(failed)?;
    {
        let own = drawn.canvas().ok_or_else(failed)?;
        view::draw_picture(&own, picture.compose(), w, h, [0, 0, picture::W, picture::H]);
    }
    let (mut pixels, mut placed) = (Vec::new(), Vec::new());
    drawn.read(&mut pixels, 0, 0, w, h);
    let room = (picture::MAX_SHIFT * view::PICTURE_SCALE * scale).ceil() as usize + 1;
    let shift = picture.shift();
    let by = (shift.0 * view::PICTURE_SCALE * scale, shift.1 * view::PICTURE_SCALE * scale);
    picture::place(&pixels, w as usize, h as usize, room, by, 1.0 - look.presence, &mut placed);
    let (x, y) = ((view::PICTURE.0 * scale) as i32 - room as i32, (view::PICTURE.1 * scale) as i32 - room as i32);
    bitmap.write(&placed, w as usize + room * 2, h as usize + room * 2, x, y);
    Ok(bitmap)
}

/// Milliseconds between two frames of a film.
const FILM_STEP_MS: u32 = 30;
/// The film is drawn larger than the window, to stay sharp on dense screens.
const FILM_SCALE: f32 = 1.5;

/// Where a stick is at `ms`, moving smoothly through `points` (time, x, y).
fn stick_at(ms: u32, points: &[(u32, f32, f32)]) -> (f32, f32) {
    for pair in points.windows(2) {
        let ((from, x0, y0), (to, x1, y1)) = (pair[0], pair[1]);
        if ms >= from && ms < to {
            let t = (ms - from) as f32 / (to - from) as f32;
            let t = t * t * (3.0 - 2.0 * t);
            return (x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
        }
    }
    (0.0, 0.0)
}

/// What the controller does at `ms` into the film, played the way a hand
/// would: taps and holds that overlap, the sticks flicked and rolled, from the
/// front, then on the triggers (the top view), then on the paddles (the back
/// view) with a trigger still held, so the picture goes straight from one to
/// the other. None once the film is over.
fn film_input(ms: u32) -> Option<Input> {
    // (from, to, buttons)
    let presses = [
        (350, 460, btn::A), (540, 680, btn::A), (780, 960, btn::B),
        (1100, 1480, btn::Y), (1260, 1380, btn::X),
        (1600, 1730, btn::RIGHT), (1800, 1910, btn::RIGHT), (1990, 2160, btn::DOWN), (2220, 2340, btn::LEFT),
        (2900, 3060, btn::LS), (3120, 3240, btn::PLUS),
        // The top view.
        (3380, 5050, btn::ZR), (3620, 4050, btn::ZL), (4150, 4270, btn::R), (4350, 4470, btn::R),
        (4460, 4680, btn::L), (4720, 4900, btn::ZL),
        // The back view, taken while ZR is still down.
        (4980, 5560, btn::GR), (5260, 5460, btn::GL), (5700, 5880, btn::GR), (5840, 6150, btn::GL),
    ];
    let mut input = Input::default();
    for (from, to, buttons) in presses {
        if ms >= from && ms < to {
            input.buttons |= buttons;
        }
    }
    input.left = stick_at(ms, &[
        (150, 0.0, 0.0), (420, 0.7, -0.6), (800, 0.6, -0.7), (1150, -0.9, 0.1), (1400, -0.8, 0.3), (1560, 0.0, 0.0),
        (2350, 0.0, 0.0), (2550, 0.0, 1.0), (2720, 0.9, 0.3), (2880, 0.3, -0.9), (3060, 0.0, 0.0),
    ]);
    input.right = stick_at(ms, &[
        (1950, 0.0, 0.0), (2150, -0.9, -0.2), (2350, -0.3, 0.9), (2550, 0.8, 0.5), (2750, 0.7, -0.6), (2950, 0.0, 0.0),
    ]);
    (ms < 7100).then_some(input)
}

/// Plays the scripted film and saves its frames, for tools/gif.mjs to turn
/// into a GIF: width, height, frame count and milliseconds per frame (u32 LE
/// each), then every frame's pixels, four bytes each (blue, green, red, x).
fn record_film(path: &str) -> io::Result<()> {
    let _session = Session::start().ok_or_else(|| io::Error::other("GDI+ failed to start"))?;
    let mut picture = Picture::load().ok_or_else(|| io::Error::other("cannot load the picture"))?;
    picture.settle(Input::default());
    let (w, h) = ((view::WIDTH * FILM_SCALE) as i32, (view::HEIGHT * FILM_SCALE) as i32);
    let (mut frames, mut pixels) = (Vec::new(), Vec::new());
    let mut count = 0u32;
    while let Some(input) = film_input(count * FILM_STEP_MS) {
        picture.step(FILM_STEP_MS as f32, input);
        window(&mut picture, &CONNECTED, FILM_SCALE)?.read(&mut pixels, 0, 0, w, h);
        frames.extend(pixels.iter().flat_map(|p| p.to_le_bytes()));
        count += 1;
    }
    let mut file = Vec::with_capacity(16 + frames.len());
    for value in [w as u32, h as u32, count, FILM_STEP_MS] {
        file.extend(value.to_le_bytes());
    }
    file.extend(frames);
    std::fs::write(path, file)
}
