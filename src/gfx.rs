//! Small anti-aliased 2D drawing layer over GDI+ (ships with Windows, so it
//! costs no extra dependency and only loads while something is drawn).

// Some primitives are not used by the current window; they are kept for the
// planned live controller view.
#![allow(clippy::too_many_arguments)]

use std::ffi::c_void;
use std::ptr::{null, null_mut};

#[cfg(feature = "tools")]
use windows_sys::core::GUID;
use windows_sys::Win32::Graphics::Gdi::{SetDIBitsToDevice, BITMAPINFO, BI_RGB, DIB_RGB_COLORS, HDC};
use windows_sys::Win32::Graphics::GdiPlus::*;
use windows_sys::Win32::UI::Shell::SHCreateMemStream;
use windows_sys::Win32::UI::WindowsAndMessaging::HICON;

/// 0xAARRGGBB
pub type Color = u32;

pub const fn rgb(hex: u32) -> Color {
    0xFF00_0000 | hex
}

const PIXEL_FORMAT_32BPP_PARGB: i32 = 0x000E_200B;
#[cfg(feature = "tools")]
const PNG_ENCODER: GUID = GUID {
    data1: 0x557CF406,
    data2: 0x1A04,
    data3: 0x11D3,
    data4: [0x9A, 0x73, 0x00, 0x00, 0xF8, 0x1E, 0xF3, 0x2E],
};

/// Keeps GDI+ loaded for as long as it lives.
pub struct Session(usize);

impl Session {
    pub fn start() -> Option<Self> {
        let input = GdiplusStartupInput {
            GdiplusVersion: 1,
            DebugEventCallback: 0,
            SuppressBackgroundThread: 0,
            SuppressExternalCodecs: 0,
        };
        let mut token = 0usize;
        let status = unsafe { GdiplusStartup(&mut token, &input, null_mut()) };
        (status == 0).then_some(Session(token))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe { GdiplusShutdown(self.0) };
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// An off-screen 32-bit image.
pub struct Bitmap {
    raw: *mut GpBitmap,
    pub width: i32,
    pub height: i32,
}

impl Bitmap {
    pub fn new(width: i32, height: i32) -> Option<Self> {
        let mut raw = null_mut();
        unsafe {
            GdipCreateBitmapFromScan0(width, height, 0, PIXEL_FORMAT_32BPP_PARGB, null(), &mut raw)
        };
        (!raw.is_null()).then_some(Bitmap { raw, width, height })
    }

    pub fn canvas(&self) -> Option<Canvas> {
        let mut g = null_mut();
        unsafe { GdipGetImageGraphicsContext(self.raw as *mut GpImage, &mut g) };
        Canvas::wrap(g)
    }

    /// Gives `with` direct access to a rectangle of the bitmap's pixels: a
    /// pointer to its first row and the distance in bytes between rows.
    /// No canvas of this bitmap may be alive.
    fn locked(&self, x: i32, y: i32, w: i32, h: i32, write: bool, with: impl FnOnce(*mut u8, isize)) {
        const LOCK_READ: u32 = 1;
        const LOCK_WRITE: u32 = 2;
        if x < 0 || y < 0 || w <= 0 || h <= 0 || x + w > self.width || y + h > self.height {
            return;
        }
        unsafe {
            let area = Rect { X: x, Y: y, Width: w, Height: h };
            let mut locked: BitmapData = std::mem::zeroed();
            let mode = if write { LOCK_WRITE } else { LOCK_READ };
            if GdipBitmapLockBits(self.raw, &area, mode, PIXEL_FORMAT_32BPP_PARGB, &mut locked) == 0 {
                with(locked.Scan0 as *mut u8, locked.Stride as isize);
                GdipBitmapUnlockBits(self.raw, &mut locked);
            }
        }
    }

    /// Copies a rectangle of the bitmap's pixels (0xAARRGGBB) into the same
    /// place in `pixels`, which holds the whole bitmap row by row.
    pub fn read(&self, pixels: &mut Vec<u32>, x: i32, y: i32, w: i32, h: i32) {
        let width = self.width as usize;
        pixels.resize(width * self.height as usize, 0);
        self.locked(x, y, w, h, false, |first, stride| unsafe {
            for row in 0..h as usize {
                let to = pixels.as_mut_ptr().add((y as usize + row) * width + x as usize);
                std::ptr::copy_nonoverlapping(first.offset(row as isize * stride) as *const u32, to, w as usize);
            }
        });
    }

    /// Copies opaque pixels (`w` x `h`) into the bitmap with their corner at
    /// (x, y). They must lie inside it.
    pub fn write(&self, pixels: &[u32], w: usize, h: usize, x: i32, y: i32) {
        debug_assert_eq!(pixels.len(), w * h);
        self.locked(x, y, w as i32, h as i32, true, |first, stride| unsafe {
            for row in 0..h {
                std::ptr::copy_nonoverlapping(pixels.as_ptr().add(row * w), first.offset(row as isize * stride) as *mut u32, w);
            }
        });
    }

    /// Copies a rectangle of the image to the same place in a window device
    /// context: a plain copy of pixels, with no drawing library in between.
    /// No canvas of this bitmap may be alive.
    pub fn present(&self, hdc: HDC, left: i32, top: i32, right: i32, bottom: i32) {
        let (left, top) = (left.max(0), top.max(0));
        let (width, height) = (right.min(self.width) - left, bottom.min(self.height) - top);
        // Whole rows are locked, so they lie one after another in memory.
        self.locked(0, top, self.width, height, false, |first, stride| unsafe {
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader.biSize = std::mem::size_of_val(&info.bmiHeader) as u32;
            info.bmiHeader.biWidth = (stride / 4) as i32;
            // Negative: the first row is the top one.
            info.bmiHeader.biHeight = -height;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB;
            SetDIBitsToDevice(hdc, left, top, width as u32, height as u32, left, 0, 0, height as u32, first as *const _, &info, DIB_RGB_COLORS);
        });
    }

    #[cfg(feature = "tools")]
    pub fn save_png(&self, path: &str) -> bool {
        let path = wide(path);
        unsafe {
            GdipSaveImageToFile(self.raw as *mut GpImage, path.as_ptr(), &PNG_ENCODER, null()) == 0
        }
    }

    pub fn to_icon(&self) -> HICON {
        let mut icon: HICON = null_mut();
        unsafe { GdipCreateHICONFromBitmap(self.raw, &mut icon) };
        icon
    }
}

impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe { GdipDisposeImage(self.raw as *mut GpImage) };
    }
}

/// A picture decoded from an embedded file (JPEG, PNG...).
pub struct Image {
    raw: *mut GpImage,
    /// The COM stream GDI+ decodes from; it has to outlive the image.
    stream: *mut c_void,
}

impl Image {
    pub fn decode(data: &'static [u8]) -> Option<Self> {
        unsafe {
            let stream = SHCreateMemStream(data.as_ptr(), data.len() as u32);
            if stream.is_null() {
                return None;
            }
            let mut raw = null_mut();
            GdipLoadImageFromStream(stream, &mut raw);
            let image = Image { raw, stream };
            (!raw.is_null()).then_some(image)
        }
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        unsafe {
            if !self.raw.is_null() {
                GdipDisposeImage(self.raw);
            }
            // IUnknown::Release is the third entry of every COM vtable.
            type Release = unsafe extern "system" fn(*mut c_void) -> u32;
            let vtable = *(self.stream as *const *const Release);
            (*vtable.add(2))(self.stream);
        }
    }
}

/// Decodes an embedded picture into rows of 0xAARRGGBB pixels, with its
/// width and height. Needs a running `Session`.
pub fn decode_pixels(data: &'static [u8]) -> Option<(Vec<u32>, usize, usize)> {
    const LOCK_READ: u32 = 1;
    const PIXEL_FORMAT_32BPP_ARGB: i32 = 0x0026_200A;
    let image = Image::decode(data)?;
    unsafe {
        let (mut width, mut height) = (0u32, 0u32);
        GdipGetImageWidth(image.raw, &mut width);
        GdipGetImageHeight(image.raw, &mut height);
        let area = Rect { X: 0, Y: 0, Width: width as i32, Height: height as i32 };
        let mut locked: BitmapData = std::mem::zeroed();
        if GdipBitmapLockBits(image.raw as *mut GpBitmap, &area, LOCK_READ, PIXEL_FORMAT_32BPP_ARGB, &mut locked) != 0 {
            return None;
        }
        let (w, h) = (width as usize, height as usize);
        let mut pixels = vec![0u32; w * h];
        for row in 0..h {
            let source = (locked.Scan0 as *const u8).offset(row as isize * locked.Stride as isize) as *const u32;
            std::ptr::copy_nonoverlapping(source, pixels.as_mut_ptr().add(row * w), w);
        }
        GdipBitmapUnlockBits(image.raw as *mut GpBitmap, &mut locked);
        Some((pixels, w, h))
    }
}

pub struct Canvas {
    g: *mut GpGraphics,
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe { GdipDeleteGraphics(self.g) };
    }
}

impl Canvas {
    fn wrap(g: *mut GpGraphics) -> Option<Self> {
        if g.is_null() {
            return None;
        }
        unsafe {
            GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
            GdipSetTextRenderingHint(g, TextRenderingHintAntiAlias);
            GdipSetPixelOffsetMode(g, PixelOffsetModeHalf);
        }
        Some(Canvas { g })
    }

    /// All later coordinates are multiplied by `factor`.
    pub fn scale(&self, factor: f32) {
        unsafe { GdipScaleWorldTransform(self.g, factor, factor, MatrixOrderPrepend) };
    }

    /// Limits drawing to a rectangle while `draw` runs.
    pub fn clipped(&self, x: f32, y: f32, w: f32, h: f32, draw: impl FnOnce()) {
        unsafe { GdipSetClipRect(self.g, x, y, w, h, CombineModeReplace) };
        draw();
        unsafe { GdipResetClip(self.g) };
    }

    pub fn clear(&self, color: Color) {
        unsafe { GdipGraphicsClear(self.g, color) };
    }

    fn with_brush(&self, color: Color, draw: impl FnOnce(*mut GpBrush)) {
        unsafe {
            let mut brush = null_mut();
            GdipCreateSolidFill(color, &mut brush);
            if !brush.is_null() {
                draw(brush as *mut GpBrush);
                GdipDeleteBrush(brush as *mut GpBrush);
            }
        }
    }

    fn with_pen(&self, color: Color, width: f32, draw: impl FnOnce(*mut GpPen)) {
        unsafe {
            let mut pen = null_mut();
            GdipCreatePen1(color, width, UnitWorld, &mut pen);
            if !pen.is_null() {
                GdipSetPenLineJoin(pen, LineJoinRound);
                draw(pen);
                GdipDeletePen(pen);
            }
        }
    }

    fn with_path(&self, build: impl FnOnce(*mut GpPath), draw: impl FnOnce(*mut GpPath)) {
        unsafe {
            let mut path = null_mut();
            GdipCreatePath(FillModeWinding, &mut path);
            if !path.is_null() {
                build(path);
                draw(path);
                GdipDeletePath(path);
            }
        }
    }

    pub fn fill_circle(&self, cx: f32, cy: f32, r: f32, color: Color) {
        self.with_brush(color, |b| unsafe {
            GdipFillEllipse(self.g, b, cx - r, cy - r, r * 2.0, r * 2.0);
        });
    }

    /// Strokes part of a circle; angles in degrees, clockwise from 3 o'clock.
    pub fn arc(&self, cx: f32, cy: f32, r: f32, start: f32, sweep: f32, color: Color, width: f32) {
        self.with_pen(color, width, |p| unsafe {
            GdipDrawArc(self.g, p, cx - r, cy - r, r * 2.0, r * 2.0, start, sweep);
        });
    }

    fn round_rect_path(path: *mut GpPath, x: f32, y: f32, w: f32, h: f32, r: f32) {
        let d = (r * 2.0).min(w).min(h).max(0.01);
        unsafe {
            GdipAddPathArc(path, x, y, d, d, 180.0, 90.0);
            GdipAddPathArc(path, x + w - d, y, d, d, 270.0, 90.0);
            GdipAddPathArc(path, x + w - d, y + h - d, d, d, 0.0, 90.0);
            GdipAddPathArc(path, x, y + h - d, d, d, 90.0, 90.0);
            GdipClosePathFigure(path);
        }
    }

    pub fn fill_round_rect(&self, x: f32, y: f32, w: f32, h: f32, r: f32, color: Color) {
        self.with_path(
            |p| Self::round_rect_path(p, x, y, w, h, r),
            |p| self.with_brush(color, |b| unsafe {
                GdipFillPath(self.g, b, p);
            }),
        );
    }

    pub fn stroke_round_rect(&self, x: f32, y: f32, w: f32, h: f32, r: f32, color: Color, width: f32) {
        self.with_path(
            |p| Self::round_rect_path(p, x, y, w, h, r),
            |p| self.with_pen(color, width, |pen| unsafe {
                GdipDrawPath(self.g, pen, p);
            }),
        );
    }

    pub fn line(&self, x1: f32, y1: f32, x2: f32, y2: f32, color: Color, width: f32) {
        self.with_pen(color, width, |p| unsafe {
            GdipDrawLine(self.g, p, x1, y1, x2, y2);
        });
    }

    /// Draws another bitmap at its own size.
    #[cfg(feature = "tools")]
    pub fn bitmap(&self, bitmap: &Bitmap, x: f32, y: f32) {
        unsafe { GdipDrawImage(self.g, bitmap.raw as *mut GpImage, x, y) };
    }

    /// Draws raw 0xFFRRGGBB pixels (`width` x `height`) scaled into the given
    /// rectangle. The pixels are used in place, not copied.
    pub fn pixels(&self, pixels: &[u32], width: usize, height: usize, x: f32, y: f32, w: f32, h: f32) {
        const PIXEL_FORMAT_32BPP_RGB: i32 = 0x0002_2009;
        const UNIT_PIXEL: i32 = 2;
        const WRAP_FLIP_XY: i32 = 3;
        debug_assert_eq!(pixels.len(), width * height);
        unsafe {
            let mut bitmap = null_mut();
            GdipCreateBitmapFromScan0(width as i32, height as i32, width as i32 * 4, PIXEL_FORMAT_32BPP_RGB, pixels.as_ptr() as *const u8, &mut bitmap);
            if bitmap.is_null() {
                return;
            }
            // Mirror the picture past its edges, so smoothing does not fade them into nothing.
            let mut edges = null_mut();
            GdipCreateImageAttributes(&mut edges);
            if !edges.is_null() {
                GdipSetImageAttributesWrapMode(edges, WRAP_FLIP_XY, 0, 0);
            }
            GdipSetInterpolationMode(self.g, InterpolationModeHighQualityBicubic);
            GdipDrawImageRectRect(self.g, bitmap as *mut GpImage, x, y, w, h, 0.0, 0.0, width as f32, height as f32, UNIT_PIXEL, edges, 0, null_mut());
            if !edges.is_null() {
                GdipDisposeImageAttributes(edges);
            }
            GdipDisposeImage(bitmap as *mut GpImage);
        }
    }

    /// Runs `draw` with a font and a tight (typographic) string format.
    fn with_font(&self, bold: bool, size: f32, draw: impl FnOnce(*mut GpFont, *mut GpStringFormat)) {
        let family_name = wide(if bold { "Segoe UI Semibold" } else { "Segoe UI" });
        unsafe {
            let mut family = null_mut();
            GdipCreateFontFamilyFromName(family_name.as_ptr(), null_mut(), &mut family);
            if family.is_null() {
                return;
            }
            let mut font = null_mut();
            GdipCreateFont(family, size, 0, UnitWorld, &mut font);
            let mut generic = null_mut();
            GdipStringFormatGetGenericTypographic(&mut generic);
            let mut format = null_mut();
            if !generic.is_null() {
                GdipCloneStringFormat(generic, &mut format);
            }
            if !font.is_null() && !format.is_null() {
                draw(font, format);
            }
            if !format.is_null() {
                GdipDeleteStringFormat(format);
            }
            if !font.is_null() {
                GdipDeleteFont(font);
            }
            GdipDeleteFontFamily(family);
        }
    }

    /// Draws one line of text centred on `(x, y)`.
    pub fn text(&self, text: &str, x: f32, y: f32, size: f32, bold: bool, color: Color) {
        let string = wide(text);
        self.with_font(bold, size, |font, format| unsafe {
            GdipSetStringFormatAlign(format, StringAlignmentCenter);
            GdipSetStringFormatLineAlign(format, StringAlignmentCenter);
            const SPAN: f32 = 2000.0;
            let rect = RectF { X: x - SPAN / 2.0, Y: y - size, Width: SPAN, Height: size * 2.0 };
            self.with_brush(color, |b| {
                GdipDrawString(self.g, string.as_ptr(), -1, font, &rect, format, b);
            });
        });
    }
}
