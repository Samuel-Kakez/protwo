//! Minimal ViGEmBus client: plugs in a virtual Xbox 360 or DualShock 4
//! controller and feeds it reports. Talks to the bus driver directly, so no
//! ViGEmClient DLL is needed.

use std::ffi::c_void;
use std::io;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{GetLastError, ERROR_IO_PENDING, HANDLE};
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OVERLAPPED;
use windows_sys::Win32::System::IO::{CancelIoEx, DeviceIoControl, GetOverlappedResult, OVERLAPPED};

use crate::win::{interface_paths, open_device, Event, Handle};

const BUS_CLASS: GUID = GUID {
    data1: 0x96E42B22,
    data2: 0xF5E9,
    data3: 0x42F8,
    data4: [0xB0, 0x43, 0xED, 0x0F, 0x93, 0x2F, 0x01, 0x4F],
};

/// Where the driver is downloaded from.
pub const DOWNLOAD_PAGE: &str = "https://github.com/nefarius/ViGEmBus/releases/latest";

/// Whether the ViGEmBus driver is installed.
pub fn installed() -> bool {
    !interface_paths(&BUS_CLASS, "").is_empty()
}

const FILE_DEVICE_BUS_EXTENDER: u32 = 0x2A;
const FILE_READ_DATA: u32 = 1;
const FILE_WRITE_DATA: u32 = 2;

const fn ioctl(index: u32, access: u32) -> u32 {
    (FILE_DEVICE_BUS_EXTENDER << 16) | (access << 14) | ((0x801 + index) << 2)
}

const IOCTL_PLUGIN_TARGET: u32 = ioctl(0x000, FILE_WRITE_DATA);
const IOCTL_UNPLUG_TARGET: u32 = ioctl(0x001, FILE_WRITE_DATA);
const IOCTL_CHECK_VERSION: u32 = ioctl(0x002, FILE_WRITE_DATA);
const IOCTL_WAIT_DEVICE_READY: u32 = ioctl(0x003, FILE_WRITE_DATA);
const IOCTL_XUSB_REQUEST_NOTIFICATION: u32 = ioctl(0x200, FILE_WRITE_DATA | FILE_READ_DATA);
const IOCTL_XUSB_SUBMIT_REPORT: u32 = ioctl(0x201, FILE_WRITE_DATA);
const IOCTL_DS4_SUBMIT_REPORT: u32 = ioctl(0x202, FILE_WRITE_DATA);
const IOCTL_DS4_REQUEST_NOTIFICATION: u32 = ioctl(0x203, FILE_WRITE_DATA);

const VIGEM_COMMON_VERSION: u32 = 0x0001;
const MAX_TARGETS: u32 = 16;

pub const XUSB_REPORT_LEN: usize = 12;
pub const DS4_REPORT_LEN: usize = 63;

/// The kind of virtual controller games see.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// XInput, for games that only know Xbox controllers. No motion.
    Xbox360,
    /// DirectInput, with gyro and accelerometer.
    DualShock4,
}

impl Kind {
    /// (target type, vendor id, product id)
    fn ids(self) -> (u32, u16, u16) {
        match self {
            Kind::Xbox360 => (0, 0x045E, 0x028E),
            Kind::DualShock4 => (2, 0x054C, 0x05C4),
        }
    }
}

/// A plugged-in virtual controller. Unplugged on drop.
pub struct Pad {
    bus: Handle,
    serial: u32,
    pub kind: Kind,
    /// Set once unplugged. The bus hands serial numbers out again, so a
    /// second unplug could remove a pad plugged in since.
    gone: AtomicBool,
}

fn le(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

impl Pad {
    pub fn plug(kind: Kind) -> io::Result<Self> {
        let path = interface_paths(&BUS_CLASS, "").into_iter().next().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "ViGEmBus driver is not installed")
        })?;
        let bus = open_device(&path, FILE_FLAG_OVERLAPPED)?;
        let event = Event::new()?;

        let mut check = le(&[8, VIGEM_COMMON_VERSION]);
        call(bus.0, event.raw(), IOCTL_CHECK_VERSION, &mut check)
            .map_err(|e| io::Error::other(format!("ViGEmBus version check failed: {e}")))?;

        let (target, vid, pid) = kind.ids();
        let mut last_err = io::Error::other("no free ViGEm slot");
        for serial in 1..=MAX_TARGETS {
            let mut plug = le(&[16, serial, target]);
            plug.extend_from_slice(&vid.to_le_bytes());
            plug.extend_from_slice(&pid.to_le_bytes());
            match call(bus.0, event.raw(), IOCTL_PLUGIN_TARGET, &mut plug) {
                Ok(_) => {
                    // Older bus versions lack this call; the pad still works.
                    let mut ready = le(&[8, serial]);
                    let _ = call(bus.0, event.raw(), IOCTL_WAIT_DEVICE_READY, &mut ready);
                    return Ok(Pad { bus, serial, kind, gone: AtomicBool::new(false) });
                }
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    /// Submits an input report: `XUSB_REPORT_LEN` bytes for Xbox 360,
    /// `DS4_REPORT_LEN` bytes for DualShock 4. `event` must belong to the
    /// calling thread.
    pub fn submit(&self, event: &Event, report: &[u8]) -> io::Result<()> {
        let (code, expected) = match self.kind {
            Kind::Xbox360 => (IOCTL_XUSB_SUBMIT_REPORT, XUSB_REPORT_LEN),
            Kind::DualShock4 => (IOCTL_DS4_SUBMIT_REPORT, DS4_REPORT_LEN),
        };
        debug_assert_eq!(report.len(), expected);
        let mut buf = [0u8; 8 + DS4_REPORT_LEN];
        let len = 8 + expected;
        buf[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        buf[4..8].copy_from_slice(&self.serial.to_le_bytes());
        buf[8..len].copy_from_slice(report);
        call(self.bus.0, event.raw(), code, &mut buf[..len]).map(|_| ())
    }

    /// Blocks until a game sets the rumble motors; returns (large, small)
    /// motor strengths 0..=255. Fails once the pad is unplugged.
    pub fn wait_rumble(&self, event: &Event) -> io::Result<(u8, u8)> {
        if self.is_gone() {
            return Err(io::ErrorKind::NotConnected.into());
        }
        match self.kind {
            Kind::Xbox360 => {
                // { Size, SerialNo, LargeMotor, SmallMotor, LedNumber } padded to 12.
                let mut buf = [0u8; 12];
                buf[0..4].copy_from_slice(&12u32.to_le_bytes());
                buf[4..8].copy_from_slice(&self.serial.to_le_bytes());
                call(self.bus.0, event.raw(), IOCTL_XUSB_REQUEST_NOTIFICATION, &mut buf)?;
                Ok((buf[8], buf[9]))
            }
            Kind::DualShock4 => {
                // { Size, SerialNo, SmallMotor, LargeMotor, Lightbar RGB } padded to 16.
                let mut buf = [0u8; 16];
                buf[0..4].copy_from_slice(&16u32.to_le_bytes());
                buf[4..8].copy_from_slice(&self.serial.to_le_bytes());
                call(self.bus.0, event.raw(), IOCTL_DS4_REQUEST_NOTIFICATION, &mut buf)?;
                Ok((buf[9], buf[8]))
            }
        }
    }

    /// Removes the virtual controller and unblocks `wait_rumble`. Only the
    /// first call does anything.
    pub fn unplug(&self) {
        if self.gone.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Ok(event) = Event::new() {
            let mut unplug = le(&[8, self.serial]);
            let _ = call(self.bus.0, event.raw(), IOCTL_UNPLUG_TARGET, &mut unplug);
        }
        unsafe { CancelIoEx(self.bus.0, null_mut()) };
    }

    pub fn is_gone(&self) -> bool {
        self.gone.load(Ordering::Acquire)
    }
}

impl Drop for Pad {
    fn drop(&mut self) {
        self.unplug();
    }
}

/// Issues an IOCTL on the overlapped bus handle and waits for completion.
/// Only the notification requests return data; for those `buf` is also the
/// output buffer, exactly as the reference client does.
fn call(bus: HANDLE, event: HANDLE, code: u32, buf: &mut [u8]) -> io::Result<u32> {
    unsafe {
        let mut ov: OVERLAPPED = std::mem::zeroed();
        ov.hEvent = event;
        let mut done = 0u32;
        let ptr = buf.as_mut_ptr() as *mut c_void;
        let len = buf.len() as u32;
        let returns_data =
            code == IOCTL_XUSB_REQUEST_NOTIFICATION || code == IOCTL_DS4_REQUEST_NOTIFICATION;
        let (out_ptr, out_len) = if returns_data { (ptr, len) } else { (null_mut(), 0) };
        if DeviceIoControl(bus, code, ptr, len, out_ptr, out_len, &mut done, &mut ov) != 0 {
            return Ok(done);
        }
        if GetLastError() != ERROR_IO_PENDING {
            return Err(io::Error::last_os_error());
        }
        if GetOverlappedResult(bus, &ov, &mut done, 1) == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(done)
    }
}
