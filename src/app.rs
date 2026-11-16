//! The user interface: a notification-area (tray) icon and a window that is
//! hidden rather than minimised or closed. While hidden it costs nothing:
//! no timer runs, the drawing library is unloaded and its buffers are freed.

use std::cell::{Cell, RefCell};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
use windows_sys::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, ScreenToClient, PAINTSTRUCT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows_sys::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::bridge::{PadStatus, Shared};
use crate::gfx::{Bitmap, Session};
use crate::picture::{self, Input, Picture};
use crate::settings::{self, Saved};
use crate::view;
use crate::vigem::{self, Kind};

const CLASS_NAME: &str = "protwo.window";
const TITLE: &str = "ProTwo";

const WM_TRAY: u32 = WM_APP + 1;
const WM_STATUS: u32 = WM_APP + 2;
/// Sent by a second instance to bring the running one forward.
pub const WM_SHOW: u32 = WM_APP + 3;

const TRAY_ID: u32 = 1;
/// Drives the live picture while the window is visible.
const FRAME_TIMER: usize = 1;
/// Sent when the pointer leaves the window (after TrackMouseEvent).
const WM_MOUSELEAVE: u32 = 0x02A3;
const FRAME_MS: u32 = 15;
/// After this many frames with no controller and nothing moving (about a
/// second), the frame timer stops until the status changes.
const STILL_FRAMES: u32 = 64;

const CMD_OPEN: usize = 1;
const CMD_DS4: usize = 2;
const CMD_XBOX: usize = 3;
const CMD_QUIT: usize = 4;

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();
static ICON_CONNECTED: AtomicIsize = AtomicIsize::new(0);
static ICON_IDLE: AtomicIsize = AtomicIsize::new(0);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

/// Drawing resources, alive only while the window is visible.
#[derive(Default)]
struct Paint {
    // Declared, and so dropped, before the session: the buffers must be
    // freed while the drawing library is still loaded.
    back: Option<Bitmap>,
    /// The live controller picture, and how it was last drawn at its size
    /// in the window.
    picture: Option<Picture>,
    drawn: Option<Bitmap>,
    /// That drawing's pixels, and the same shifted into the picture's frame.
    drawn_pixels: Vec<u32>,
    placed: Vec<u32>,
    session: Option<Session>,
    /// Everything around the picture has to be drawn again.
    stale: bool,
    /// When the picture's animation was last advanced.
    ticked: Option<Instant>,
    /// Frames in a row with no controller and nothing moving.
    still: u32,
    /// Where the window's fades are; none until the window is first drawn,
    /// so it opens as it is and not mid-fade.
    look: Option<view::Look>,
    /// The controller's turn counter at that moment.
    turned: Option<u64>,
    /// The picture changed since the last paint.
    picture_moved: bool,
}

thread_local! {
    static PAINT: RefCell<Paint> = RefCell::new(Paint::default());
    /// The control the mouse button went down on. A click counts only when
    /// the button comes up on the same one.
    static PRESSED: Cell<Option<view::Action>> = const { Cell::new(None) };
    /// The control under the pointer.
    static HOVERED: Cell<Option<view::Action>> = const { Cell::new(None) };
}

fn shared() -> &'static Arc<Shared> {
    SHARED.get().expect("app not initialised")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Wakes the UI thread when the bridge's connection status changes.
pub fn notify(target: isize) {
    unsafe { PostMessageW(target as HWND, WM_STATUS, 0, 0) };
}

/// Brings an already-running instance's window forward. Returns false if
/// there is none.
pub fn show_existing() -> bool {
    unsafe {
        let hwnd = FindWindowW(wide(CLASS_NAME).as_ptr(), null());
        if hwnd.is_null() {
            return false;
        }
        // Windows lets a window come forward only with the leave of the
        // process the user last used, which is this one.
        let mut process = 0u32;
        GetWindowThreadProcessId(hwnd, &mut process);
        AllowSetForegroundWindow(process);
        PostMessageW(hwnd, WM_SHOW, 0, 0);
        true
    }
}

fn make_icon(size: i32, connected: bool) -> isize {
    let Some(bitmap) = Bitmap::new(size, size) else { return 0 };
    if let Some(canvas) = bitmap.canvas() {
        view::draw_icon(&canvas, size as f32, connected);
    }
    bitmap.to_icon() as isize
}

fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let live = *shared().live.lock().unwrap();
    let tip = if !live.connected {
        "ProTwo: waiting for a controller".to_string()
    } else {
        match live.pad {
            PadStatus::Active(Kind::DualShock4) => "ProTwo: connected (DirectInput)".to_string(),
            PadStatus::Active(Kind::Xbox360) => "ProTwo: connected (XInput)".to_string(),
            _ => "ProTwo: connected, driver missing".to_string(),
        }
    };
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = TRAY_ID;
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = WM_TRAY;
    let icon = if live.connected { &ICON_CONNECTED } else { &ICON_IDLE };
    data.hIcon = icon.load(Ordering::Relaxed) as HICON;
    for (slot, unit) in data.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
        *slot = unit;
    }
    data
}

/// Asks for the whole window to be drawn again.
fn refresh(hwnd: HWND) {
    PAINT.with(|p| p.borrow_mut().stale = true);
    unsafe { InvalidateRect(hwnd, null(), 0) };
}

fn show(hwnd: HWND) {
    unsafe {
        ShowWindow(hwnd, SW_SHOWNORMAL);
        SetForegroundWindow(hwnd);
        SetTimer(hwnd, FRAME_TIMER, FRAME_MS, None);
    }
    refresh(hwnd);
}

/// A part of the window, in client pixels.
fn client_rect(hwnd: HWND, (x, y, w, h): (f32, f32, f32, f32)) -> RECT {
    unsafe {
        let mut client: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut client);
        let scale = (client.right - client.left) as f32 / view::WIDTH;
        RECT {
            left: (x * scale).floor() as i32,
            top: (y * scale).floor() as i32,
            right: ((x + w) * scale).ceil() as i32,
            bottom: ((y + h) * scale).ceil() as i32,
        }
    }
}

/// One animation frame: moves the picture toward the controller's current
/// inputs and redraws its part of the window only if something changed.
fn tick(hwnd: HWND) {
    let s = shared();
    let live = *s.live.lock().unwrap();
    let connected = live.connected;
    let goal = view::Look::settled(&live, settings(), HOVERED.get());
    let mut input = if connected { Input::unpack(s.input.load(Ordering::Relaxed)) } else { Input::default() };
    let turned = s.turned.load(Ordering::Relaxed);
    let moved = PAINT.with(|p| {
        let mut p = p.borrow_mut();
        // How far the controller turned since the last frame.
        if let (true, Some(before)) = (connected, p.turned) {
            input.turn = picture::turn_between(before, turned);
        }
        p.turned = Some(turned);
        let now = Instant::now();
        let dt = p.ticked.map_or(FRAME_MS as f32, |last| (now - last).as_secs_f32() * 1000.0).min(64.0);
        p.ticked = Some(now);
        let moved = p.picture.as_mut().is_some_and(|picture| picture.step(dt, input));
        p.picture_moved |= moved;
        let fading = p.look.get_or_insert(goal).ease(goal, dt);
        p.stale |= fading;
        let moved = if fading { Moved::Everything } else if moved { Moved::Picture } else { Moved::Nothing };
        p.still = if moved != Moved::Nothing || connected { 0 } else { p.still + 1 };
        if p.still > STILL_FRAMES {
            // Nothing to animate until a controller connects (see WM_STATUS).
            p.still = 0;
            p.ticked = None;
            unsafe { KillTimer(hwnd, FRAME_TIMER) };
        }
        moved
    });
    match moved {
        Moved::Everything => unsafe {
            InvalidateRect(hwnd, null(), 0);
        },
        Moved::Picture => unsafe {
            InvalidateRect(hwnd, &client_rect(hwnd, view::FRAME), 0);
        },
        Moved::Nothing => {}
    }
}

/// What a frame changed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Moved {
    Nothing,
    Picture,
    Everything,
}

/// Frees everything used for drawing: the buffers first, then the drawing
/// library itself. The order matters.
fn release() {
    PAINT.with(|p| {
        let mut p = p.borrow_mut();
        p.back = None;
        p.drawn = None;
        p.drawn_pixels = Vec::new();
        p.placed = Vec::new();
        p.picture = None;
        p.session = None;
        p.ticked = None;
        p.turned = None;
        p.look = None;
    });
}

fn hide(hwnd: HWND) {
    unsafe {
        if IsWindowVisible(hwnd) == 0 {
            return;
        }
        ShowWindow(hwnd, SW_HIDE);
        KillTimer(hwnd, FRAME_TIMER);
    }
    HOVERED.set(None);
    release();
    // Hand the now-unused pages back to the system.
    unsafe { SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX) };
}

fn settings() -> view::Settings {
    view::Settings { kind: shared().kind() }
}

/// Switches the emulation mode and remembers it.
fn set_kind(kind: Kind) {
    shared().set_kind(kind);
    settings::save(&Saved { kind });
}

/// The control under a mouse message's position, if any.
fn control_at(hwnd: HWND, lparam: LPARAM) -> Option<view::Action> {
    let x = (lparam & 0xFFFF) as i16 as f32;
    let y = ((lparam >> 16) & 0xFFFF) as i16 as f32;
    unsafe {
        let mut client: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut client);
        let width = client.right - client.left;
        if width <= 0 {
            return None;
        }
        let scale = view::WIDTH / width as f32;
        let live = *shared().live.lock().unwrap();
        view::hit(x * scale, y * scale, &live)
    }
}

fn click(hwnd: HWND, action: view::Action) {
    match action {
        view::Action::Quit => unsafe {
            DestroyWindow(hwnd);
        },
        view::Action::Minimize => hide(hwnd),
        view::Action::SetKind(kind) => {
            set_kind(kind);
            // The pill slides over: make sure frames are running.
            unsafe { SetTimer(hwnd, FRAME_TIMER, FRAME_MS, None) };
            refresh(hwnd);
        }
        view::Action::GetDriver => open_download_page(hwnd),
    }
}

/// Notes what the pointer is over; a change starts the frames that fade it.
fn hover(hwnd: HWND, control: Option<view::Action>) {
    if HOVERED.replace(control) != control {
        unsafe { SetTimer(hwnd, FRAME_TIMER, FRAME_MS, None) };
    }
}

fn open_download_page(hwnd: HWND) {
    unsafe {
        ShellExecuteW(hwnd, wide("open").as_ptr(), wide(vigem::DOWNLOAD_PAGE).as_ptr(), null(), null(), SW_SHOWNORMAL);
    }
}

/// Without the driver there is no virtual controller: offers to open its
/// download page.
fn offer_driver(hwnd: HWND) {
    let text = "ProTwo needs the ViGEmBus driver to create the virtual controller, and it is not installed.\n\nOpen its download page?";
    let answer = unsafe { MessageBoxW(hwnd, wide(text).as_ptr(), wide(TITLE).as_ptr(), MB_YESNO | MB_ICONINFORMATION) };
    if answer == IDYES {
        open_download_page(hwnd);
    }
}

fn paint(hwnd: HWND) {
    unsafe {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut client: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut client);
        let (w, h) = (client.right - client.left, client.bottom - client.top);
        if w > 0 && h > 0 {
            let live = *shared().live.lock().unwrap();
            PAINT.with(|p| {
                let mut p = p.borrow_mut();
                if p.session.is_none() {
                    p.session = Session::start();
                }
                let p = &mut *p;
                let look = *p.look.get_or_insert(view::Look::settled(&live, settings(), HOVERED.get()));
                if p.picture.is_none() {
                    p.picture = Picture::load();
                }
                if p.back.as_ref().map(|b| (b.width, b.height)) != Some((w, h)) {
                    p.back = Bitmap::new(w, h);
                    p.stale = true;
                }
                if let Some(back) = &p.back {
                    let scale = w as f32 / view::WIDTH;
                    // Everything around the picture, only when it changed.
                    let all = std::mem::take(&mut p.stale);
                    if all {
                        if let Some(canvas) = back.canvas() {
                            canvas.scale(scale);
                            view::draw(&canvas, &live, look);
                        }
                    }
                    // The picture at its size in the window: scaled only when
                    // it changed itself.
                    let size = ((view::PICTURE.2 * scale).round() as i32, (view::PICTURE.3 * scale).round() as i32);
                    let mut fresh = false;
                    if p.drawn.as_ref().map(|b| (b.width, b.height)) != Some(size) {
                        p.drawn = Bitmap::new(size.0, size.1);
                        fresh = true;
                    }
                    let moved = std::mem::take(&mut p.picture_moved);
                    if let (Some(picture), Some(drawn)) = (p.picture.as_mut(), p.drawn.as_ref()) {
                        if picture.take_stale() || fresh {
                            picture.compose();
                            let dirty = if fresh { Some([0, 0, picture::W, picture::H]) } else { picture.dirty() };
                            if let (Some(dirty), Some(own)) = (dirty, drawn.canvas()) {
                                let (x, y, w, h) = view::draw_picture(&own, picture.frame(), size.0, size.1, dirty);
                                drop(own);
                                drawn.read(&mut p.drawn_pixels, x, y, w, h);
                                fresh = true;
                            }
                        }
                        // A shift puts the same pixels elsewhere in the picture's frame.
                        let (pw, ph) = (size.0 as usize, size.1 as usize);
                        if (fresh || moved || all) && p.drawn_pixels.len() == pw * ph {
                            let room = (picture::MAX_SHIFT * view::PICTURE_SCALE * scale).ceil() as usize + 1;
                            let shift = picture.shift();
                            let by = (shift.0 * view::PICTURE_SCALE * scale, shift.1 * view::PICTURE_SCALE * scale);
                            picture::place(&p.drawn_pixels, pw, ph, room, by, 1.0 - look.presence, &mut p.placed);
                            let (x, y) = ((view::PICTURE.0 * scale).round() as i32 - room as i32, (view::PICTURE.1 * scale).round() as i32 - room as i32);
                            back.write(&p.placed, pw + room * 2, ph + room * 2, x, y);
                        }
                    }
                    back.present(hdc, ps.rcPaint.left, ps.rcPaint.top, ps.rcPaint.right, ps.rcPaint.bottom);
                }
            });
        }
        EndPaint(hwnd, &ps);
    }
}

fn tray_menu(hwnd: HWND) {
    unsafe {
        let menu = CreatePopupMenu();
        let item = |id: usize, text: &str, checked: bool| {
            let flags = MF_STRING | if checked { MF_CHECKED } else { 0 };
            AppendMenuW(menu, flags, id, wide(text).as_ptr());
        };
        item(CMD_OPEN, "Open ProTwo", false);
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        item(CMD_DS4, "DirectInput (with gyro)", shared().kind() == Kind::DualShock4);
        item(CMD_XBOX, "XInput", shared().kind() == Kind::Xbox360);
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        item(CMD_QUIT, "Quit", false);

        let mut cursor = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor);
        // Required so the menu closes when the user clicks elsewhere.
        SetForegroundWindow(hwnd);
        let command = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            cursor.x,
            cursor.y,
            0,
            hwnd,
            null(),
        ) as usize;
        DestroyMenu(menu);
        PostMessageW(hwnd, WM_NULL, 0, 0);

        match command {
            CMD_OPEN => show(hwnd),
            CMD_DS4 => set_kind(Kind::DualShock4),
            CMD_XBOX => set_kind(Kind::Xbox360),
            CMD_QUIT => {
                DestroyWindow(hwnd);
            }
            _ => {}
        }
        if command != CMD_QUIT {
            if IsWindowVisible(hwnd) != 0 {
                SetTimer(hwnd, FRAME_TIMER, FRAME_MS, None);
            }
            refresh(hwnd);
        }
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            match lparam as u32 {
                WM_LBUTTONUP => {
                    if IsWindowVisible(hwnd) != 0 {
                        hide(hwnd);
                    } else {
                        show(hwnd);
                    }
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => tray_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_SHOW => {
            show(hwnd);
            0
        }
        WM_STATUS => {
            Shell_NotifyIconW(NIM_MODIFY, &tray_data(hwnd));
            if IsWindowVisible(hwnd) != 0 {
                SetTimer(hwnd, FRAME_TIMER, FRAME_MS, None);
                refresh(hwnd);
            }
            0
        }
        WM_TIMER if wparam == FRAME_TIMER => {
            tick(hwnd);
            0
        }
        // Minimising and closing both send the window to the tray.
        WM_SYSCOMMAND if (wparam & 0xFFF0) == SC_MINIMIZE as usize => {
            hide(hwnd);
            0
        }
        WM_CLOSE => {
            hide(hwnd);
            0
        }
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_MOUSEMOVE => {
            // Asks to be told when the pointer leaves, so the hover can end.
            let mut track = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            TrackMouseEvent(&mut track);
            hover(hwnd, control_at(hwnd, lparam));
            0
        }
        WM_MOUSELEAVE | WM_NCMOUSEMOVE => {
            hover(hwnd, None);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_LBUTTONDOWN => {
            PRESSED.set(control_at(hwnd, lparam));
            0
        }
        WM_LBUTTONUP => {
            let pressed = PRESSED.take();
            if let Some(action) = control_at(hwnd, lparam).filter(|&a| Some(a) == pressed) {
                click(hwnd, action);
            }
            0
        }
        // Minimised by something other than its own button.
        WM_SIZE if wparam == SIZE_MINIMIZED as usize => {
            hide(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        // No title bar: the whole window is its own content, and it is moved
        // by dragging anything that is not a control.
        WM_NCCALCSIZE => 0,
        WM_NCHITTEST => {
            let mut point = POINT { x: (lparam & 0xFFFF) as i16 as i32, y: ((lparam >> 16) & 0xFFFF) as i16 as i32 };
            ScreenToClient(hwnd, &mut point);
            let at = ((point.y as u32 as LPARAM) << 16) | (point.x as u32 as LPARAM & 0xFFFF);
            if control_at(hwnd, at).is_some() { HTCLIENT as LRESULT } else { HTCAPTION as LRESULT }
        }
        WM_DPICHANGED => {
            let suggested = &*(lparam as *const RECT);
            SetWindowPos(
                hwnd,
                null_mut(),
                suggested.left,
                suggested.top,
                suggested.right - suggested.left,
                suggested.bottom - suggested.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            refresh(hwnd);
            0
        }
        WM_DESTROY => {
            KillTimer(hwnd, FRAME_TIMER);
            release();
            Shell_NotifyIconW(NIM_DELETE, &tray_data(hwnd));
            PostQuitMessage(0);
            0
        }
        _ => {
            // Explorer restarted: the tray icon has to be added again.
            if msg != 0 && msg == TASKBAR_CREATED.load(Ordering::Relaxed) {
                Shell_NotifyIconW(NIM_ADD, &tray_data(hwnd));
                return 0;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }
}

/// Creates the tray icon and window and runs the message loop until Quit.
pub fn run(shared_state: Arc<Shared>, start_hidden: bool) {
    let _ = SHARED.set(shared_state);
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        // Icons are drawn once at startup; the drawing library is then
        // unloaded again until the window is first shown.
        let (mut big, mut small) = (0isize, 0isize);
        if let Some(_session) = Session::start() {
            big = make_icon(64, true);
            small = make_icon(32, true);
            ICON_CONNECTED.store(small, Ordering::Relaxed);
            ICON_IDLE.store(make_icon(32, false), Ordering::Relaxed);
        }

        let instance = GetModuleHandleW(null());
        let class_name = wide(CLASS_NAME);
        let mut class: WNDCLASSEXW = std::mem::zeroed();
        class.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
        class.lpfnWndProc = Some(window_proc);
        class.hInstance = instance;
        class.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
        class.hIcon = big as HICON;
        class.hIconSm = small as HICON;
        class.lpszClassName = class_name.as_ptr();
        RegisterClassExW(&class);

        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let dpi = GetDpiForSystem();
        let scale = dpi as f32 / 96.0;
        // The window is all content, so its size is the content's.
        let frame = RECT {
            left: 0,
            top: 0,
            right: (view::WIDTH * scale).round() as i32,
            bottom: (view::HEIGHT * scale).round() as i32,
        };

        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            wide(TITLE).as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            frame.right - frame.left,
            frame.bottom - frame.top,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            return;
        }
        // Rounded corners, where Windows has them (11 and later).
        let corners = DWMWCP_ROUND;
        DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE as u32, &corners as *const _ as *const _, std::mem::size_of_val(&corners) as u32);
        // Have the frame worked out again, now that the window answers for it.
        SetWindowPos(hwnd, null_mut(), 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        // The window may have opened on a screen with another scale.
        let own = GetDpiForWindow(hwnd);
        if own != 0 && own != dpi {
            let scale = own as f32 / 96.0;
            let (width, height) = ((view::WIDTH * scale).round() as i32, (view::HEIGHT * scale).round() as i32);
            SetWindowPos(hwnd, null_mut(), 0, 0, width, height, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        }

        TASKBAR_CREATED.store(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()), Ordering::Relaxed);
        Shell_NotifyIconW(NIM_ADD, &tray_data(hwnd));
        shared().set_notify_target(hwnd as isize);

        if start_hidden {
            SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
        } else {
            show(hwnd);
        }
        if !vigem::installed() {
            offer_driver(hwnd);
        }

        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    // Give the bridge time to play its chime, silence rumble and unplug
    // the virtual pad.
    let s = shared();
    s.set_notify_target(0);
    s.quit.store(true, Ordering::Release);
    if s.live.lock().unwrap().connected {
        let deadline = Instant::now() + Duration::from_millis(2000);
        while !s.has_stopped() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
