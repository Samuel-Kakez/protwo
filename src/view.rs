//! Draws the window: the controller, its status, the mode switch, and a quit
//! button in the corner. Works in a fixed logical coordinate space; the caller
//! scales for DPI.

use crate::bridge::{Live, PadStatus};
use crate::gfx::{rgb, Canvas, Color};
use crate::picture;
use crate::vigem::Kind;

pub const WIDTH: f32 = 420.0;
pub const HEIGHT: f32 = 470.0;

pub const BACKGROUND: Color = rgb(0xFFFFFF);
// Black, two greys and a pale surface: no colour unless something is wrong.
const INK: Color = rgb(0x1D1D1F);
const MUTED: Color = rgb(0x6E6E73);
const FAINT: Color = rgb(0xAEAEB2);
const SURFACE: Color = rgb(0xF2F2F4);
const WARN: Color = rgb(0xB25000);

/// Current settings, shown as selected or not.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub kind: Kind,
}

/// Where the window's two animations are. Each eases toward the state it
/// shows, so a change fades or slides instead of jumping.
#[derive(Clone, Copy, PartialEq)]
pub struct Look {
    /// 0 with no controller, 1 with one: fades the title, the status and
    /// the veil over the picture and the mode switch.
    pub presence: f32,
    /// 0 on DirectInput, 1 on XInput: where the switch's pill is.
    pub slide: f32,
    /// 1 while the pointer is over the minimise button, or the quit button:
    /// darkens it.
    pub over_minimize: f32,
    pub over_quit: f32,
}

impl Look {
    /// The look once every animation has ended, with the pointer over
    /// `hovered`.
    pub fn settled(live: &Live, settings: Settings, hovered: Option<Action>) -> Self {
        let on = |yes: bool| if yes { 1.0 } else { 0.0 };
        Look {
            presence: on(live.connected),
            slide: on(settings.kind == Kind::Xbox360),
            over_minimize: on(hovered == Some(Action::Minimize)),
            over_quit: on(hovered == Some(Action::Quit)),
        }
    }

    /// Moves toward `goal` over `dt` milliseconds. Returns false once there.
    pub fn ease(&mut self, goal: Look, dt: f32) -> bool {
        const EASE_MS: f32 = 70.0;
        let k = 1.0 - (-dt / EASE_MS).exp();
        let step = |value: &mut f32, goal: f32| {
            *value = if (goal - *value).abs() < 0.004 { goal } else { *value + (goal - *value) * k };
        };
        let before = *self;
        step(&mut self.presence, goal.presence);
        step(&mut self.slide, goal.slide);
        step(&mut self.over_minimize, goal.over_minimize);
        step(&mut self.over_quit, goal.over_quit);
        *self != before
    }
}

/// A colour at a share of its strength, 0 to 1.
fn faded(color: Color, share: f32) -> Color {
    let alpha = ((color >> 24) as f32 * share.clamp(0.0, 1.0)).round() as u32;
    (alpha << 24) | (color & 0x00FF_FFFF)
}

/// A colour between two, `t` from 0 to 1.
fn between(a: Color, b: Color, t: f32) -> Color {
    let channel = |shift: u32| {
        let (from, to) = (((a >> shift) & 0xFF) as f32, ((b >> shift) & 0xFF) as f32);
        ((from + (to - from) * t.clamp(0.0, 1.0)).round() as u32) << shift
    };
    channel(24) | channel(16) | channel(8) | channel(0)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    SetKind(Kind),
    Quit,
    /// Send the window to the tray.
    Minimize,
    /// Open the driver's download page.
    GetDriver,
}

/// A rectangle: x, y, width, height.
type Area = (f32, f32, f32, f32);

const PAD: f32 = 28.0;
const INNER: f32 = WIDTH - PAD * 2.0;

/// The live picture, at 0.6 of its pixel size and centred.
pub const PICTURE_SCALE: f32 = 0.6;
pub const PICTURE: Area = (
    (WIDTH - picture::W as f32 * PICTURE_SCALE) / 2.0,
    44.0,
    picture::W as f32 * PICTURE_SCALE,
    picture::H as f32 * PICTURE_SCALE,
);
/// The picture drifts by a pixel or two as the controller turns; this is
/// its rectangle with room for that on every side.
const ROOM: f32 = picture::MAX_SHIFT * PICTURE_SCALE + 2.0;
pub const FRAME: Area = (PICTURE.0 - ROOM, PICTURE.1 - ROOM, PICTURE.2 + ROOM * 2.0, PICTURE.3 + ROOM * 2.0);
const TITLE_Y: f32 = 314.0;
const STATUS_Y: f32 = 340.0;
/// With no controller there is no title: one line sits between the two.
const WAITING_Y: f32 = (TITLE_Y + STATUS_Y) / 2.0;

// The mode switch: a track holding two pills.
const TRACK: Area = (PAD, 364.0, INNER, 44.0);
const INSET: f32 = 3.0;
const PILL_W: f32 = (INNER - INSET * 3.0) / 2.0;
const PILL_H: f32 = TRACK.3 - INSET * 2.0;
const DS4: Area = (PAD + INSET, TRACK.1 + INSET, PILL_W, PILL_H);
const XBOX: Area = (PAD + INSET * 2.0 + PILL_W, TRACK.1 + INSET, PILL_W, PILL_H);

/// Two small lines of advice under the mode switch.
const NOTES: [(f32, &str); 2] = [
    (427.0, "DirectInput supports gyroscope."),
    (445.0, "Minimizing keeps ProTwo running in the tray."),
];

/// The window has no title bar: its two buttons sit in the top right corner,
/// above the picture's frame.
const QUIT: Area = (WIDTH - 40.0, 8.0, 28.0, 28.0);
const MINIMIZE: Area = (WIDTH - 72.0, 8.0, 28.0, 28.0);

/// The status line, which becomes a link when the driver is missing.
const STATUS: Area = (PAD, STATUS_Y - 12.0, INNER, 24.0);
const DRIVER_MISSING: &str = "ViGEmBus driver missing";

fn driver_missing(live: &Live) -> bool {
    live.connected && live.pad == PadStatus::Unavailable
}

/// Maps a click (logical coordinates) to what it activates.
pub fn hit(x: f32, y: f32, live: &Live) -> Option<Action> {
    let inside = |a: Area| x >= a.0 && x < a.0 + a.2 && y >= a.1 && y < a.1 + a.3;
    if driver_missing(live) && inside(STATUS) {
        Some(Action::GetDriver)
    } else if inside(DS4) {
        Some(Action::SetKind(Kind::DualShock4))
    } else if inside(XBOX) {
        Some(Action::SetKind(Kind::Xbox360))
    } else if inside(QUIT) {
        Some(Action::Quit)
    } else if inside(MINIMIZE) {
        Some(Action::Minimize)
    } else {
        None
    }
}

/// The mode switch: a white pill on a track, under the mode it selects.
/// `slide` is where the pill is, from 0 (DirectInput) to 1 (XInput).
fn switch(c: &Canvas, slide: f32) {
    c.fill_round_rect(TRACK.0, TRACK.1, TRACK.2, TRACK.3, 9.0, SURFACE);
    let (y, w, h) = (DS4.1, DS4.2, DS4.3);
    let x = DS4.0 + (XBOX.0 - DS4.0) * slide;
    // A soft shadow under the pill.
    c.fill_round_rect(x, y + 1.0, w, h, 7.0, 0x1A00_0000);
    c.fill_round_rect(x, y, w, h, 7.0, BACKGROUND);
    c.stroke_round_rect(x, y, w, h, 7.0, 0x0F00_0000, 1.0);
    let label = |area: Area, name: &str, selected: f32| {
        c.text(name, area.0 + area.2 / 2.0, area.1 + area.3 / 2.0, 13.0, true, between(MUTED, INK, selected));
    };
    label(DS4, "DirectInput", 1.0 - slide);
    label(XBOX, "XInput", slide);
}

/// The quit button: a small power sign.
fn power(c: &Canvas, cx: f32, cy: f32, ink: Color) {
    c.arc(cx, cy + 0.5, 6.0, -55.0, 290.0, ink, 1.5);
    c.line(cx, cy - 7.0, cx, cy - 0.5, ink, 1.5);
}

/// Draws the live picture (`picture::W` x `picture::H` pixels) to fill a
/// canvas `w` x `h`: a bitmap of its own, at the size it has in the window.
/// This is the costly step, needed only when the picture itself changes, and
/// then only for the part that did: `dirty`, in picture pixels. Returns the
/// part of the canvas that was redrawn: x, y, width, height in its pixels.
pub fn draw_picture(c: &Canvas, pixels: &[u32], w: i32, h: i32, dirty: picture::Area) -> (i32, i32, i32, i32) {
    // The same part on the canvas, and a little more for the smoothing's reach.
    let (kx, ky) = (w as f32 / picture::W as f32, h as f32 / picture::H as f32);
    let x0 = ((dirty[0] as f32 * kx).floor() as i32 - 3).max(0);
    let y0 = ((dirty[1] as f32 * ky).floor() as i32 - 3).max(0);
    let x1 = ((dirty[2] as f32 * kx).ceil() as i32 + 3).min(w);
    let y1 = ((dirty[3] as f32 * ky).ceil() as i32 + 3).min(h);
    c.clipped(x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32, || {
        c.pixels(pixels, picture::W, picture::H, 0.0, 0.0, w as f32, h as f32);
    });
    (x0, y0, x1 - x0, y1 - y0)
}

/// Draws everything but the live picture.
pub fn draw(c: &Canvas, live: &Live, look: Look) {
    c.clear(BACKGROUND);

    // With a controller: its name and one quiet line under it. Without: one
    // line alone. The two fade into each other.
    let present = look.presence;
    if present > 0.0 {
        c.text("Pro Controller 2", WIDTH / 2.0, TITLE_Y, 20.0, true, faded(INK, present));
        let (ink, status) = if driver_missing(live) { (WARN, DRIVER_MISSING) } else { (MUTED, "Connected") };
        c.text(status, WIDTH / 2.0, STATUS_Y, 13.0, false, faded(ink, present));
    }
    if present < 1.0 {
        c.text("Waiting for a controller", WIDTH / 2.0, WAITING_Y, 13.0, false, faded(FAINT, 1.0 - present));
    }

    switch(c, look.slide);
    if present < 1.0 {
        // The mode still applies to the next controller; show it subdued.
        c.fill_round_rect(TRACK.0 - 1.0, TRACK.1 - 1.0, TRACK.2 + 2.0, TRACK.3 + 3.0, 0.0, faded(0x8CFF_FFFF, 1.0 - present));
    }

    for (y, note) in NOTES {
        c.text(note, WIDTH / 2.0, y, 11.0, false, FAINT);
    }

    let (x, y) = (MINIMIZE.0 + MINIMIZE.2 / 2.0, MINIMIZE.1 + MINIMIZE.3 / 2.0);
    // The two buttons darken under the pointer.
    c.line(x - 6.0, y, x + 6.0, y, between(MUTED, INK, look.over_minimize), 1.5);
    power(c, QUIT.0 + QUIT.2 / 2.0, QUIT.1 + QUIT.3 / 2.0, between(MUTED, INK, look.over_quit));
}

/// Draws the logo (assets/logo.svg) filling a square of `size`: a dark tile
/// with the controller's two sticks, placed as they are on the pad. The lower
/// stick is lit while a controller is connected and grey without one.
pub fn draw_icon(c: &Canvas, size: f32, connected: bool) {
    c.clear(0);
    // The logo is drawn on a grid of 64 units.
    let u = size / 64.0;
    c.fill_round_rect(0.0, 0.0, size, size, 15.0 * u, rgb(0x1B1C1F));
    c.fill_circle(22.0 * u, 23.0 * u, 11.0 * u, rgb(0xFFFFFF));
    let lit = if connected { rgb(0xF0503C) } else { rgb(0x7A7D84) };
    c.fill_circle(42.0 * u, 41.0 * u, 11.0 * u, lit);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_line_is_a_link_only_when_the_driver_is_missing() {
        let missing = Live { connected: true, pad: PadStatus::Unavailable };
        assert_eq!(hit(WIDTH / 2.0, STATUS_Y, &missing), Some(Action::GetDriver));
        assert_eq!(hit(WIDTH / 2.0, STATUS_Y, &Live::default()), None);
        assert_eq!(hit(WIDTH / 2.0, STATUS_Y, &Live { connected: true, pad: PadStatus::Active(Kind::Xbox360) }), None);
    }

    #[test]
    fn the_look_eases_to_its_goal_and_stops_there() {
        let goal = Look { presence: 1.0, slide: 1.0, over_minimize: 0.0, over_quit: 1.0 };
        let mut look = Look { presence: 0.0, slide: 0.0, over_minimize: 0.0, over_quit: 0.0 };
        assert!(look.ease(goal, 15.0));
        assert!(look.presence > 0.1 && look.presence < 0.3);
        for _ in 0..40 {
            look.ease(goal, 15.0);
        }
        assert!(look == goal && !look.ease(goal, 15.0));
    }

    #[test]
    fn colours_fade_and_blend() {
        assert_eq!(faded(INK, 0.0) >> 24, 0);
        assert_eq!(faded(INK, 1.0), INK);
        assert_eq!(between(MUTED, INK, 0.0), MUTED);
        assert_eq!(between(MUTED, INK, 1.0), INK);
    }

    #[test]
    fn clicks_map_to_the_control_under_them() {
        let hit = |x, y| hit(x, y, &Live::default());
        assert_eq!(hit(100.0, 386.0), Some(Action::SetKind(Kind::DualShock4)));
        assert_eq!(hit(310.0, 386.0), Some(Action::SetKind(Kind::Xbox360)));
        // The picture, the title, the gap between the pills, the bottom line.
        assert_eq!(hit(WIDTH / 2.0, 386.0), None);
        assert_eq!(hit(210.0, 140.0), None);
        assert_eq!(hit(210.0, TITLE_Y), None);
        assert_eq!(hit(WIDTH / 2.0, HEIGHT - 10.0), None);
        assert_eq!(hit(394.0, 24.0), Some(Action::Quit));
        assert_eq!(hit(362.0, 24.0), Some(Action::Minimize));
        assert_eq!(hit(30.0, 24.0), None);
    }

    #[test]
    fn the_parts_redrawn_alone_touch_nothing_else() {
        // The picture is redrawn over its whole frame, which has room for the largest shift.
        assert!(FRAME.1 + FRAME.3 <= TITLE_Y - 13.0);
    }

    #[test]
    fn the_quit_button_is_clear_of_the_picture() {
        // The picture's frame is redrawn alone and would paint over it.
        assert!(QUIT.1 + QUIT.3 <= FRAME.1 && MINIMIZE.1 + MINIMIZE.3 <= FRAME.1);
    }

    #[test]
    fn layout_stays_inside_the_window() {
        for (x, y, w, h) in [FRAME, TRACK, DS4, XBOX, QUIT, MINIMIZE] {
            assert!(x >= 0.0 && y >= 0.0 && x + w <= WIDTH && y + h <= HEIGHT);
        }
    }
}
