//! Raw HID access: blocking input reports and output (rumble) reports.

use std::io;
use std::ptr::null_mut;

use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    HidD_FreePreparsedData, HidD_GetPreparsedData, HidP_GetCaps, HIDP_CAPS, PHIDP_PREPARSED_DATA,
};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};

use crate::win::{open_device, Handle};

/// Two handles on the same device so a blocking read never delays a write.
pub struct Hid {
    reader: Handle,
    writer: Handle,
    pub in_len: usize,
    pub out_len: usize,
}

impl Hid {
    pub fn open(path: &[u16]) -> io::Result<Self> {
        let reader = open_device(path, 0)?;
        let writer = open_device(path, 0)?;
        let (mut in_len, mut out_len) = (64, 64);
        unsafe {
            let mut preparsed: PHIDP_PREPARSED_DATA = std::mem::zeroed();
            HidD_GetPreparsedData(reader.0, &mut preparsed);
            if preparsed != std::mem::zeroed() {
                let mut caps: HIDP_CAPS = std::mem::zeroed();
                HidP_GetCaps(preparsed, &mut caps);
                HidD_FreePreparsedData(preparsed);
                if caps.InputReportByteLength > 0 {
                    in_len = caps.InputReportByteLength as usize;
                }
                if caps.OutputReportByteLength > 0 {
                    out_len = caps.OutputReportByteLength as usize;
                }
            }
        }
        Ok(Hid {
            reader,
            writer,
            in_len,
            out_len,
        })
    }

    /// Blocks until the next input report. Byte 0 is the report ID.
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let mut done = 0u32;
        let ok = unsafe {
            ReadFile(
                self.reader.0,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut done,
                null_mut(),
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(done as usize)
        }
    }

    /// Sends an output report. Byte 0 is the report ID.
    pub fn write(&self, report: &[u8]) -> io::Result<()> {
        let mut done = 0u32;
        let ok = unsafe {
            WriteFile(
                self.writer.0,
                report.as_ptr(),
                report.len() as u32,
                &mut done,
                null_mut(),
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}
