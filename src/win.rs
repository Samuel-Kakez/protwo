//! Thin, safe-ish wrappers over the handful of Win32 primitives the app needs.

use std::ffi::c_void;
use std::io;
use std::ptr::{null, null_mut};

use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_Interface_ListW, CM_Get_Device_Interface_List_SizeW, CM_Register_Notification,
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_NOTIFY_ACTION, CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL,
    CM_NOTIFY_EVENT_DATA, CM_NOTIFY_FILTER, CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE, CR_SUCCESS,
    HCMNOTIFICATION,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentThread, SetEvent, SetThreadPriority, WaitForSingleObject, INFINITE,
    THREAD_PRIORITY_TIME_CRITICAL,
};

/// Owned Win32 handle, closed on drop.
pub struct Handle(pub HANDLE);

// SAFETY: kernel handles are process-wide and usable from any thread.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// Opens a device interface path for read/write, shared with other processes.
pub fn open_device(path: &[u16], flags: u32) -> io::Result<Handle> {
    let h = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            flags,
            null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(Handle(h))
    }
}

/// Auto-reset event.
pub struct Event(pub Handle);

impl Event {
    pub fn new() -> io::Result<Self> {
        let h = unsafe { CreateEventW(null(), 0, 0, null()) };
        if h.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Event(Handle(h)))
        }
    }

    pub fn raw(&self) -> HANDLE {
        self.0 .0
    }

    pub fn wait(&self) -> bool {
        unsafe { WaitForSingleObject(self.raw(), INFINITE) == WAIT_OBJECT_0 }
    }
}

/// Lists the present device interface paths (NUL-terminated UTF-16) of a class
/// whose path contains `needle` (lower-case ASCII).
pub fn interface_paths(class: &GUID, needle: &str) -> Vec<Vec<u16>> {
    let mut out = Vec::new();
    unsafe {
        let mut len = 0u32;
        if CM_Get_Device_Interface_List_SizeW(
            &mut len,
            class,
            null(),
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        ) != CR_SUCCESS
            || len <= 1
        {
            return out;
        }
        let mut buf = vec![0u16; len as usize];
        if CM_Get_Device_Interface_ListW(
            class,
            null(),
            buf.as_mut_ptr(),
            len,
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        ) != CR_SUCCESS
        {
            return out;
        }
        for path in buf.split(|&c| c == 0).filter(|p| !p.is_empty()) {
            let lower = String::from_utf16_lossy(path).to_ascii_lowercase();
            if lower.contains(needle) {
                let mut owned = path.to_vec();
                owned.push(0);
                out.push(owned);
            }
        }
    }
    out
}

/// Signals an event whenever a device interface of the given class arrives.
/// No polling: the thread waiting on it sleeps until Windows wakes it.
pub struct Hotplug {
    event: Event,
}

unsafe extern "system" fn hotplug_callback(
    _notify: HCMNOTIFICATION,
    context: *const c_void,
    action: CM_NOTIFY_ACTION,
    _data: *const CM_NOTIFY_EVENT_DATA,
    _size: u32,
) -> u32 {
    if action == CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL {
        SetEvent(context as HANDLE);
    }
    0
}

impl Hotplug {
    pub fn register(class: &GUID) -> io::Result<Self> {
        let event = Event::new()?;
        unsafe {
            let mut filter: CM_NOTIFY_FILTER = std::mem::zeroed();
            filter.cbSize = std::mem::size_of::<CM_NOTIFY_FILTER>() as u32;
            filter.FilterType = CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE;
            filter.u.DeviceInterface.ClassGuid = *class;
            let mut notify: HCMNOTIFICATION = std::mem::zeroed();
            // The registration intentionally lives for the whole process.
            let cr = CM_Register_Notification(
                &filter,
                event.raw() as *const c_void,
                Some(hotplug_callback),
                &mut notify,
            );
            if cr != CR_SUCCESS {
                return Err(io::Error::other(format!(
                    "CM_Register_Notification failed ({cr})"
                )));
            }
        }
        Ok(Hotplug { event })
    }

    /// Blocks until a matching device interface arrives.
    pub fn wait(&self) {
        self.event.wait();
    }
}

/// Raises the calling thread's priority so input forwarding preempts all
/// normal work, games' own high-priority threads included. The thread runs
/// for microseconds per report, so it takes nothing from them.
pub fn boost_current_thread() {
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) };
}
