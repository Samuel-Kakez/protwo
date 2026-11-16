//! WinUSB access to the controller's vendor interface (bulk command channel).

use std::ffi::c_void;
use std::io;
use std::ptr::null;

use windows_sys::Win32::Devices::Usb::{
    UsbdPipeTypeBulk, WinUsb_FlushPipe, WinUsb_Free, WinUsb_Initialize,
    WinUsb_QueryInterfaceSettings, WinUsb_QueryPipe, WinUsb_ReadPipe, WinUsb_SetPipePolicy,
    WinUsb_WritePipe, PIPE_TRANSFER_TIMEOUT, USB_INTERFACE_DESCRIPTOR, WINUSB_INTERFACE_HANDLE,
    WINUSB_PIPE_INFORMATION,
};
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OVERLAPPED;

use crate::win::{open_device, Handle};

const PACKET: usize = 64;
const TIMEOUT_MS: u32 = 200;

pub struct Usb {
    _file: Handle,
    handle: WINUSB_INTERFACE_HANDLE,
    pub ep_in: u8,
    pub ep_out: u8,
}

// SAFETY: WinUSB handles may be used from any thread.
unsafe impl Send for Usb {}

impl Drop for Usb {
    fn drop(&mut self) {
        unsafe { WinUsb_Free(self.handle) };
    }
}

impl Usb {
    pub fn open(path: &[u16]) -> io::Result<Self> {
        // WinUSB requires the file to be opened for overlapped I/O, even
        // though we only ever issue synchronous transfers on it.
        let file = open_device(path, FILE_FLAG_OVERLAPPED)?;
        unsafe {
            let mut handle: WINUSB_INTERFACE_HANDLE = std::mem::zeroed();
            if WinUsb_Initialize(file.0, &mut handle) == 0 {
                return Err(io::Error::last_os_error());
            }
            // From here on `usb` owns the handle and frees it on any error path.
            let mut usb = Usb {
                _file: file,
                handle,
                ep_in: 0,
                ep_out: 0,
            };

            let mut desc: USB_INTERFACE_DESCRIPTOR = std::mem::zeroed();
            if WinUsb_QueryInterfaceSettings(handle, 0, &mut desc) == 0 {
                return Err(io::Error::last_os_error());
            }
            for index in 0..desc.bNumEndpoints {
                let mut pipe: WINUSB_PIPE_INFORMATION = std::mem::zeroed();
                if WinUsb_QueryPipe(handle, 0, index, &mut pipe) == 0 {
                    continue;
                }
                if pipe.PipeType == UsbdPipeTypeBulk {
                    if pipe.PipeId & 0x80 != 0 {
                        usb.ep_in = pipe.PipeId;
                    } else {
                        usb.ep_out = pipe.PipeId;
                    }
                }
            }
            if usb.ep_in == 0 || usb.ep_out == 0 {
                return Err(io::Error::other("bulk endpoints not found"));
            }

            for ep in [usb.ep_in, usb.ep_out] {
                WinUsb_SetPipePolicy(
                    handle,
                    ep,
                    PIPE_TRANSFER_TIMEOUT,
                    4,
                    &TIMEOUT_MS as *const u32 as *const c_void,
                );
            }
            WinUsb_FlushPipe(handle, usb.ep_in);
            Ok(usb)
        }
    }

    pub fn write(&self, data: &[u8]) -> io::Result<()> {
        let mut done = 0u32;
        let ok = unsafe {
            WinUsb_WritePipe(
                self.handle,
                self.ep_out,
                data.as_ptr(),
                data.len() as u32,
                &mut done,
                null(),
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Reads one reply, packet by packet, until a short packet or `buf` is full.
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let mut total = 0;
        while total < buf.len() {
            let mut packet = [0u8; PACKET];
            let mut done = 0u32;
            let ok = unsafe {
                WinUsb_ReadPipe(
                    self.handle,
                    self.ep_in,
                    packet.as_mut_ptr(),
                    PACKET as u32,
                    &mut done,
                    null(),
                )
            };
            if ok == 0 {
                if total > 0 {
                    break;
                }
                return Err(io::Error::last_os_error());
            }
            let done = done as usize;
            let take = done.min(buf.len() - total);
            buf[total..total + take].copy_from_slice(&packet[..take]);
            total += take;
            if done < PACKET {
                break;
            }
        }
        Ok(total)
    }

    /// Sends a command and reads its reply.
    pub fn command(&self, cmd: &[u8], reply: &mut [u8]) -> io::Result<usize> {
        self.write(cmd)?;
        self.read(reply)
    }
}
