//! Command-line tools for testing and diagnostics.

use std::io::{self, Write};
use std::thread;
use std::time::{Duration, Instant};


use crate::controller::{Controller, Rumble};
use crate::protocol::{self, btn, freq_code, State, Stereo, Voices};

pub fn with_controller(f: fn(Controller, &[String]) -> io::Result<()>, args: &[String]) -> io::Result<()> {
    match Controller::connect()? {
        Some(c) => f(c, args),
        None => Err(io::Error::other(
            "no Switch 2 Pro Controller found (plug it in over USB; quit the ProTwo tray app first)",
        )),
    }
}

fn describe(c: &Controller) -> String {
    if c.serial.is_empty() {
        "Switch 2 Pro Controller".to_string()
    } else {
        format!("Switch 2 Pro Controller ({})", c.serial)
    }
}

fn pressed_names(s: &State) -> String {
    btn::NAMES
        .iter()
        .filter(|(mask, _)| s.pressed(*mask))
        .map(|(_, name)| *name)
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn monitor(c: Controller, _args: &[String]) -> io::Result<()> {
    println!("{} - press Ctrl+C to quit\n", describe(&c));
    let mut out = io::stdout().lock();
    let mut buf = [0u8; 256];
    let mut shown = Instant::now();
    let (mut reports, mut rate) = (0u32, 0.0f32);
    loop {
        let n = c.hid.read(&mut buf)?;
        let Some(s) = protocol::parse(&buf[..n], &c.cal) else { continue };
        reports += 1;
        let elapsed = shown.elapsed();
        if elapsed < Duration::from_millis(50) {
            continue;
        }
        rate = rate * 0.8 + (reports as f32 / elapsed.as_secs_f32()) * 0.2;
        reports = 0;
        shown = Instant::now();
        let pct = |v: i16| v as i32 * 100 / 32767;
        let g = |v: i16| v as f32 / 4096.0;
        let dps = |v: i16| v as f32 / 16.4;
        write!(
            out,
            "\rL {:>4},{:>4}  R {:>4},{:>4}  accel {:>5.2} {:>5.2} {:>5.2} g  gyro {:>6.0} {:>6.0} {:>6.0} dps  {:>4.0} Hz  [{:<24}]",
            pct(s.lx), pct(s.ly), pct(s.rx), pct(s.ry),
            g(s.accel[0]), g(s.accel[1]), g(s.accel[2]),
            dps(s.gyro[0]), dps(s.gyro[1]), dps(s.gyro[2]),
            rate,
            pressed_names(&s),
        )?;
        out.flush()?;
    }
}

pub fn info(c: Controller, _args: &[String]) -> io::Result<()> {
    println!("{}", describe(&c));
    println!("USB bulk endpoints   in {:#04x}, out {:#04x}", c.usb.ep_in, c.usb.ep_out);
    println!("HID report sizes     in {} bytes, out {} bytes", c.hid.in_len, c.hid.out_len);
    for (name, stick) in [("Left stick", c.cal.left), ("Right stick", c.cal.right)] {
        println!(
            "{name:<20} x {} (-{} / +{}), y {} (-{} / +{}){}",
            stick.x.neutral, stick.x.min, stick.x.max,
            stick.y.neutral, stick.y.min, stick.y.max,
            if stick.valid() { "" } else { "  [no calibration, using defaults]" },
        );
    }
    println!(
        "Gyro bias (raw)      {:.1} {:.1} {:.1}",
        c.cal.gyro_bias[0], c.cal.gyro_bias[1], c.cal.gyro_bias[2]
    );

    // Sample the stream briefly to report its rate.
    let mut buf = [0u8; 256];
    let start = Instant::now();
    let (mut count, mut other, mut last) = (0u32, 0u32, None);
    while start.elapsed() < Duration::from_secs(1) {
        let n = c.hid.read(&mut buf)?;
        match protocol::parse(&buf[..n], &c.cal) {
            Some(s) => {
                count += 1;
                last = Some(s);
            }
            None => other += 1,
        }
    }
    println!("Input report rate    {count} Hz ({other} unrecognised reports)");
    if let Some(s) = last {
        println!(
            "Accelerometer        {:.2} {:.2} {:.2} g",
            s.accel[0] as f32 / 4096.0,
            s.accel[1] as f32 / 4096.0,
            s.accel[2] as f32 / 4096.0
        );
        println!("Sticks               L {},{}  R {},{}", s.lx, s.ly, s.rx, s.ry);
    }
    Ok(())
}

/// Keeps the HID input queue drained while a test plays.
pub fn drain_input(c: &Controller) {
    let hid = c.hid.clone();
    thread::spawn(move || {
        let mut buf = [0u8; 256];
        while hid.read(&mut buf).is_ok() {}
    });
}

pub fn rumble_test(c: Controller, _args: &[String]) -> io::Result<()> {
    println!("{}: three pulses (low, high, both), then a rising scale.", describe(&c));
    let rumble = Rumble::start(c.hid.clone());
    drain_input(&c);

    for (low, high) in [(40000, 0), (0, 40000), (40000, 40000)] {
        rumble.set(low, high);
        thread::sleep(Duration::from_millis(300));
        rumble.set(0, 0);
        thread::sleep(Duration::from_millis(250));
    }
    // C major scale from C4 to C5.
    let scale = rumble.new_sound();
    for hz in [261.6, 293.7, 329.6, 349.2, 392.0, 440.0, 493.9, 523.3] {
        rumble.set_tones(scale, Some(Stereo::both(Voices { low: (0, 0), high: (freq_code(hz), 45000) })));
        thread::sleep(Duration::from_millis(220));
    }
    rumble.set_tones(scale, None);
    thread::sleep(Duration::from_millis(100));
    Ok(())
}
