//! ProTwo - Switch 2 Pro Controller for Windows.
//!
//! Without a command it runs as a background app with a tray icon. The
//! commands are diagnostics that print to the console they were started from.

#![windows_subsystem = "windows"]

mod app;
mod bridge;
mod chime;
mod cli;
mod controller;
mod gfx;
mod hid;
mod mapping;
mod picture;
mod protocol;
mod settings;
#[cfg(feature = "tools")]
mod tools;
mod usb;
mod view;
mod voice;
mod vigem;
mod win;

use std::io;
use std::ptr::null;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
use windows_sys::Win32::System::Console::{AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE};
use windows_sys::Win32::System::Threading::CreateMutexW;

use bridge::Shared;

use vigem::Kind;

const HELP: &str = "\
ProTwo - Switch 2 Pro Controller for Windows

USAGE:
    protwo [OPTIONS]            run in the background with a tray icon
    protwo <COMMAND>            run a diagnostic in the console

OPTIONS:
    --xinput      Start in XInput mode: a virtual Xbox 360 controller
                  (default: DirectInput, a virtual DualShock 4 with gyro)
    --hidden      Start in the tray without opening the window

The mode chosen in the app is remembered; --xinput overrides it for one run.

COMMANDS:
    monitor       Show live inputs and motion
    info          Print controller details and calibration
    rumble        Play a rumble test: pulses, then a musical scale
    help          Show this help
";

/// A GUI-subsystem process has no console; borrow the parent's so the
/// diagnostics can print. Redirected output keeps working as-is.
fn attach_console() {
    unsafe {
        if GetStdHandle(STD_OUTPUT_HANDLE).is_null() {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let position = args.iter().position(|a| !a.starts_with('-'));
    let command = position.map(|i| args[i].as_str());
    let rest: &[String] = position.map_or(&[], |i| &args[i + 1..]);
    let has = |flag: &str| args.iter().any(|a| a == flag);

    if command.is_none() && !has("--help") && !has("-h") {
        let mut saved = settings::load();
        if has("--xinput") {
            saved.kind = Kind::Xbox360;
        }
        run_app(saved, has("--hidden"));
        return;
    }

    attach_console();
    let result: io::Result<()> = match command.unwrap_or("help") {
        "monitor" => cli::with_controller(cli::monitor, rest),
        "info" => cli::with_controller(cli::info, rest),
        "rumble" => cli::with_controller(cli::rumble_test, rest),
        #[cfg(feature = "tools")]
        "render" => tools::render(rest),
        #[cfg(feature = "tools")]
        "record" => cli::with_controller(tools::record, rest),
        "help" => {
            print!("{HELP}");
            Ok(())
        }
        other => {
            eprintln!("unknown command '{other}'\n\n{HELP}");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run_app(saved: settings::Saved, hidden: bool) {
    // Only one instance may own the controller; a second launch just
    // brings the first one's window forward.
    let name: Vec<u16> = "protwo.single-instance\0".encode_utf16().collect();
    // The other instance may have no window at this moment: it is starting,
    // or playing its chime on its way out. Look again for a few seconds, and
    // take its place if it goes.
    let mut tries = 0;
    loop {
        let mutex = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
        if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
            break;
        }
        tries += 1;
        if app::show_existing() || tries > 40 {
            return;
        }
        unsafe { CloseHandle(mutex) };
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    let shared = Shared::new(saved.kind, app::notify);
    let bridge_state = shared.clone();
    std::thread::spawn(move || {
        let _ = bridge::run(bridge_state);
    });
    app::run(shared, hidden);
}
