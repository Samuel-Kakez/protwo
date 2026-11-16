//! The live controller picture: the official renders with the buttons,
//! triggers, paddles and stick caps cut out, so they can be shown going down
//! and moving as the real ones do.
//!
//! Three views share one frame: front, top (for L, R, ZL, ZR) and back (for
//! GL, GR). A pressed piece is redrawn slightly smaller and darker inside the
//! dark opening it leaves; the D-pad rocks as one cross; a stick's cap slides
//! inside its bright ring. Everything is eased, and the views cross-fade.
//!
//! The picture also floats: as the controller turns, it drifts a pixel or
//! two over the window and settles back. Nothing in it is redrawn for that.
//!
//! Each view comes with a cut map: for every pixel, which piece it belongs
//! to and how much of the pixel that piece covers.

use crate::gfx;
use crate::protocol::{btn, GYRO_LSB_PER_RAD};

/// Size of the picture in its own pixels.
pub const W: usize = 540;
pub const H: usize = 400;

const IMAGES: [&[u8]; 3] = [
    include_bytes!("../assets/front.jpg"),
    include_bytes!("../assets/top.jpg"),
    include_bytes!("../assets/back.jpg"),
];
const CUTS: [&[u8]; 3] = [
    include_bytes!("../assets/front-cut.png"),
    include_bytes!("../assets/top-cut.png"),
    include_bytes!("../assets/back-cut.png"),
];

const FRONT: usize = 0;
const TOP: usize = 1;
const BACK: usize = 2;

/// Every input the picture shows: its bit in the controller's button word,
/// the view it is seen in, and its piece number in that view's cut map
/// (0 for the D-pad arms and stick clicks, which are drawn their own way).
const KEYS: [(u32, usize, u8); 21] = [
    (btn::A, FRONT, 1),
    (btn::B, FRONT, 2),
    (btn::X, FRONT, 3),
    (btn::Y, FRONT, 4),
    (btn::MINUS, FRONT, 5),
    (btn::PLUS, FRONT, 6),
    (btn::HOME, FRONT, 7),
    (btn::CAPTURE, FRONT, 8),
    (btn::C, FRONT, 9),
    (btn::UP, FRONT, 0),
    (btn::DOWN, FRONT, 0),
    (btn::LEFT, FRONT, 0),
    (btn::RIGHT, FRONT, 0),
    (btn::LS, FRONT, 0),
    (btn::RS, FRONT, 0),
    (btn::L, TOP, 1),
    (btn::R, TOP, 2),
    (btn::ZL, TOP, 3),
    (btn::ZR, TOP, 4),
    (btn::GL, BACK, 1),
    (btn::GR, BACK, 2),
];
const UP: usize = 9;
const DOWN: usize = 10;
const LEFT: usize = 11;
const RIGHT: usize = 12;
const LS: usize = 13;
const RS: usize = 14;
/// The whole D-pad is piece 10 of the front view.
const CROSS_PIECE: u8 = 10;

/// Stick caps in picture pixels: centre x, centre y, radius of the moving
/// part (just inside the bright ring).
const STICKS: [(f32, f32, f32); 2] = [(123.723, 123.759, 31.05), (335.511, 200.425, 31.05)];
/// Stick readings this close to the centre count as centred, so a resting
/// stick's jitter does not keep the picture redrawing.
const STICK_DEADZONE: f32 = 0.06;
/// A cap is redrawn only once it has moved this many pixels.
const STICK_STEP: f32 = 0.04;
/// How far a cap slides at full tilt.
const STICK_TRAVEL: f32 = 6.75;
/// A clicked stick's cap is drawn this much smaller.
const STICK_SINK: f32 = 0.94;

/// A pressed piece is drawn this much smaller, per view, and this much darker.
const SINK: [f32; 3] = [0.915, 0.955, 0.94];
const SHADE: f32 = 0.22;
/// What shows around a sunk piece: the black of its crevice, or the shaded
/// body behind a trigger.
const HOLE: [[f32; 3]; 3] = [[5.0, 6.0, 7.0], [30.0, 32.0, 36.0], [4.0, 4.0, 5.0]];
/// Next to the white background, the space a sunk piece leaves is white too.
const WHITE_REACH: i32 = 3;

/// The D-pad rocks: the pressed end narrows by TILT, the cross shortens by
/// FORESHORTEN and the pressed end darkens by up to CROSS_SHADE.
const TILT: f32 = 0.13;
const FORESHORTEN: f32 = 0.05;
const CROSS_SHADE: f32 = 0.42;

/// The box around the lettering on the back, centred, which is not mirrored
/// with the rest of that view: x0, y0, x1, y1.
const UNMIRRORED: (usize, usize, usize, usize) = (205, 128, W - 205, 258);

/// Easing time constants, in milliseconds.
const PRESS_MS: f32 = 38.0;
const RELEASE_MS: f32 = 70.0;
const STICK_MS: f32 = 28.0;
const FADE_MS: f32 = 200.0;
/// A side view stays this long after its last button is released.
const HOLD_MS: f32 = 500.0;

/// Parallax. The picture sways by this share of how far the controller has
/// turned, and swings back with this time constant.
const SWAY_GAIN: f32 = 0.5;
const SWAY_RETURN_MS: f32 = 650.0;
/// Turns smaller than this (radians) leave the picture alone, so a
/// controller lying still is drawn untouched.
const SWAY_DEADZONE: f32 = 0.006;
/// Pixels the controller shifts per radian of sway, and the most it shifts.
const SHIFT_PER_RAD: f32 = 12.0;
pub const MAX_SHIFT: f32 = 3.5;
/// The shift follows the sway with this time constant, which takes the
/// hand's tremor and the uneven frame times out of it.
const SHIFT_EASE_MS: f32 = 110.0;
/// The shift is redrawn only once it has changed by this many pixels.
const SHIFT_STEP: f32 = 0.025;
/// Seconds between two input reports.
const REPORT_S: f32 = 0.004;

/// What the picture needs to know about the controller.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Input {
    pub buttons: u32,
    /// Stick positions, -1..=1, x to the right and y down.
    pub left: (f32, f32),
    pub right: (f32, f32),
    /// How far the controller has turned since the last step, in radians
    /// about its own axes: X to the right, Y out of its face, Z toward the
    /// player.
    pub turn: [f32; 3],
}

/// Blends two pixels, `t` in 0..=256.
fn mix(a: u32, b: u32, t: u32) -> u32 {
    let s = 256 - t;
    let rb = (((a & 0xFF00FF) * s + (b & 0xFF00FF) * t) >> 8) & 0xFF00FF;
    let g = (((a & 0x00FF00) * s + (b & 0x00FF00) * t) >> 8) & 0x00FF00;
    rb | g
}

/// Lays a drawn picture (`w` x `h` pixels, at the size it has on screen) on
/// a white canvas `room` pixels larger on every side, moved by `by` pixels,
/// which may be fractions: then every pixel is blended from its neighbours
/// with the same weights. `veil`, 0 to 1, fades the whole canvas toward white.
/// The canvas is `out`, (w + 2 room) x (h + 2 room) pixels.
pub fn place(source: &[u32], w: usize, h: usize, room: usize, by: (f32, f32), veil: f32, out: &mut Vec<u32>) {
    const WHITE: u32 = 0x00FF_FFFF;
    let (ow, oh) = (w + room * 2, h + room * 2);
    out.clear();
    out.resize(ow * oh, 0xFFFF_FFFF);
    let (whole_x, whole_y) = (by.0.floor(), by.1.floor());
    let (part_x, part_y) = (((by.0 - whole_x) * 256.0) as u32, ((by.1 - whole_y) * 256.0) as u32);
    let (left, top) = (room as i32 + whole_x as i32, room as i32 + whole_y as i32);
    let at = |x: i32, y: i32| if x >= 0 && y >= 0 && x < w as i32 && y < h as i32 { source[y as usize * w + x as usize] } else { WHITE };
    if part_x == 0 && part_y == 0 {
        for y in 0..h as i32 {
            let (oy, ox) = (y + top, left);
            if oy >= 0 && oy < oh as i32 && ox >= 0 && ox + w as i32 <= ow as i32 {
                out[oy as usize * ow + ox as usize..][..w].copy_from_slice(&source[y as usize * w..][..w]);
            }
        }
    } else {
        // One more row and column than the picture: its far edges spill over by the fraction.
        let fits = left >= 0 && top >= 0 && left as usize + w < ow && top as usize + h < oh && w >= 2;
        for y in 0..=h as i32 {
            let oy = y + top;
            if oy < 0 || oy >= oh as i32 {
                continue;
            }
            // Away from the first and last rows and columns every pixel has
            // its four neighbours: no edge to look out for.
            let inner = fits && y >= 1 && y < h as i32;
            if inner {
                let y = y as usize;
                let (upper, lower) = (&source[(y - 1) * w..][..w], &source[y * w..][..w]);
                let row = &mut out[oy as usize * ow + left as usize + 1..][..w - 1];
                for ((pixel, u), l) in row.iter_mut().zip(upper.windows(2)).zip(lower.windows(2)) {
                    *pixel = 0xFF00_0000 | mix(mix(l[1], l[0], part_x), mix(u[1], u[0], part_x), part_y);
                }
            }
            for x in 0..=w as i32 {
                if inner && x >= 1 && x < w as i32 {
                    continue;
                }
                let ox = x + left;
                if ox < 0 || ox >= ow as i32 {
                    continue;
                }
                let upper = mix(at(x, y - 1), at(x - 1, y - 1), part_x);
                let lower = mix(at(x, y), at(x - 1, y), part_x);
                out[oy as usize * ow + ox as usize] = 0xFF00_0000 | mix(lower, upper, part_y);
            }
        }
    }
    // At its fullest the veil leaves a little under a quarter of the picture.
    let veil = (veil.clamp(0.0, 1.0) * 200.0).round() as u32;
    if veil > 0 {
        for pixel in out.iter_mut() {
            *pixel = 0xFF00_0000 | mix(*pixel, WHITE, veil);
        }
    }
}

/// Bits per axis in the packed turn counter.
const TURN_BITS: u32 = 21;
const TURN_MASK: u64 = (1 << TURN_BITS) - 1;

/// Adds one report's gyro reading to the packed running total of all three
/// axes, so the bridge can publish it with a single atomic store. Each axis
/// wraps; only differences between two totals are used.
pub fn turned(total: u64, gyro: [i16; 3]) -> u64 {
    let mut next = 0;
    for (axis, &rate) in gyro.iter().enumerate() {
        let shift = axis as u32 * TURN_BITS;
        let sum = ((total >> shift) & TURN_MASK).wrapping_add(rate as i64 as u64) & TURN_MASK;
        next |= sum << shift;
    }
    next
}

/// The turn, in radians per axis, between two packed totals.
pub fn turn_between(before: u64, after: u64) -> [f32; 3] {
    std::array::from_fn(|axis| {
        let shift = axis as u32 * TURN_BITS;
        let raw = ((after >> shift).wrapping_sub(before >> shift) & TURN_MASK) as i64;
        let signed = if raw > (TURN_MASK >> 1) as i64 { raw - (TURN_MASK as i64 + 1) } else { raw };
        signed as f32 / GYRO_LSB_PER_RAD * REPORT_S
    })
}

/// The controller's turn about its own axes, as a turn about the screen's
/// (x right, y up, z toward the eye) in each view: seen from the front, from
/// above its top edge (mirrored) and from behind (mirrored).
fn on_screen(side: usize, [x, y, z]: [f32; 3]) -> [f32; 3] {
    match side {
        FRONT => [x, -z, y],
        TOP => [-x, -y, z],
        _ => [-x, z, y],
    }
}

impl Input {
    /// Packs the input into one word, so the bridge can publish it with a
    /// single atomic store. Sticks keep 8 bits per axis, plenty for a cap
    /// that moves a few pixels.
    pub fn pack(buttons: u32, lx: i16, ly: i16, rx: i16, ry: i16) -> u64 {
        let axis = |v: i16| ((v >> 8) as i8) as u8 as u64;
        // Controller sticks grow upward; the picture's y grows downward.
        let down = |v: i16| axis(v.saturating_neg());
        buttons as u64 | axis(lx) << 32 | down(ly) << 40 | axis(rx) << 48 | down(ry) << 56
    }

    pub fn unpack(word: u64) -> Self {
        let axis = |shift: u32| ((word >> shift) as u8 as i8) as f32 / 127.0;
        let round = |x: f32, y: f32| {
            let d = (x * x + y * y).sqrt();
            if d > 1.0 { (x / d, y / d) } else { (x, y) }
        };
        Input { buttons: word as u32, left: round(axis(32), axis(40)), right: round(axis(48), axis(56)), turn: [0.0; 3] }
    }
}

/// One cut-out piece: its box in the picture, and per pixel of that box its
/// colour in the render, how much of the pixel it covers, and the colour of
/// the hole it leaves.
struct Piece {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    colour: Vec<[f32; 3]>,
    cover: Vec<f32>,
    hole: Vec<[f32; 3]>,
}

impl Piece {
    /// Colour (premultiplied by coverage) and coverage at a point of the
    /// piece's box, blended from the four nearest pixels.
    fn sample(&self, x: f32, y: f32) -> ([f32; 3], f32) {
        let (fx, fy) = (x - 0.5, y - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let mut rgb = [0.0f32; 3];
        let mut alpha = 0.0;
        for (dx, dy, weight) in [(0, 0, (1.0 - tx) * (1.0 - ty)), (1, 0, tx * (1.0 - ty)), (0, 1, (1.0 - tx) * ty), (1, 1, tx * ty)] {
            let (px, py) = (x0 as i32 + dx, y0 as i32 + dy);
            if px < 0 || py < 0 || px >= self.w as i32 || py >= self.h as i32 {
                continue;
            }
            let i = py as usize * self.w + px as usize;
            let a = self.cover[i] * weight;
            alpha += a;
            for k in 0..3 {
                rgb[k] += self.colour[i][k] * a;
            }
        }
        (rgb, alpha)
    }
}

struct View {
    /// The render, one 0x00RRGGBB word per pixel.
    base: Vec<u32>,
    /// Pieces by number; index 0 is unused.
    pieces: Vec<Option<Piece>>,
}

fn channels(pixel: u32) -> [f32; 3] {
    [((pixel >> 16) & 0xFF) as f32, ((pixel >> 8) & 0xFF) as f32, (pixel & 0xFF) as f32]
}

fn pixel(rgb: [f32; 3]) -> u32 {
    let c = |v: f32| v.round().clamp(0.0, 255.0) as u32;
    0xFF00_0000 | c(rgb[0]) << 16 | c(rgb[1]) << 8 | c(rgb[2])
}

/// Lays `rgb` (premultiplied by `alpha`) over a pixel.
fn over(under: &mut u32, rgb: [f32; 3], alpha: f32) {
    let u = channels(*under);
    let keep = 1.0 - alpha;
    *under = pixel([rgb[0] + u[0] * keep, rgb[1] + u[1] * keep, rgb[2] + u[2] * keep]);
}

fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl View {
    fn load(side: usize) -> Option<View> {
        let (mut base, w, h) = gfx::decode_pixels(IMAGES[side])?;
        let (mut cut, cw, ch) = gfx::decode_pixels(CUTS[side])?;
        if (w, h, cw, ch) != (W, H, W, H) {
            return None;
        }
        if side != FRONT {
            // The side views are shown mirrored, so the left hand's buttons
            // are on the left as they are from the front.
            for row in base.as_chunks_mut::<W>().0.iter_mut().chain(cut.as_chunks_mut::<W>().0) {
                row.reverse();
            }
        }
        if side == BACK {
            // The lettering in the middle of the back stays readable.
            let (x0, y0, x1, y1) = UNMIRRORED;
            for row in base.as_chunks_mut::<W>().0.iter_mut().take(y1).skip(y0) {
                row[x0..x1].reverse();
            }
        }
        let id = |i: usize| ((cut[i] >> 16) & 0xFF) as u8;
        let cover = |i: usize| ((cut[i] >> 8) & 0xFF) as f32 / 255.0;
        let background = |i: usize| id(i) == 0 && channels(base[i]).iter().all(|&c| c > 247.0);
        let count = cut.iter().map(|&p| ((p >> 16) & 0xFF) as usize).max().unwrap_or(0);

        let mut pieces: Vec<Option<Piece>> = (0..=count).map(|_| None).collect();
        for (number, slot) in pieces.iter_mut().enumerate().skip(1) {
            let mut bounds: Option<(usize, usize, usize, usize)> = None;
            for i in (0..W * H).filter(|&i| id(i) as usize == number) {
                let (x, y) = (i % W, i / W);
                bounds = Some(match bounds {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
            let Some((x0, y0, x1, y1)) = bounds else { continue };
            let (pw, ph) = (x1 - x0 + 1, y1 - y0 + 1);
            let mut piece = Piece {
                x: x0,
                y: y0,
                w: pw,
                h: ph,
                colour: vec![[0.0; 3]; pw * ph],
                cover: vec![0.0; pw * ph],
                hole: vec![HOLE[side]; pw * ph],
            };
            for y in 0..ph {
                for x in 0..pw {
                    let i = (y + y0) * W + x + x0;
                    if id(i) as usize != number {
                        continue;
                    }
                    let o = y * pw + x;
                    piece.colour[o] = channels(base[i]);
                    piece.cover[o] = cover(i);
                    let near_white = (-WHITE_REACH..=WHITE_REACH).any(|dy| {
                        (-WHITE_REACH..=WHITE_REACH).any(|dx| {
                            let (nx, ny) = ((x + x0) as i32 + dx, (y + y0) as i32 + dy);
                            dx * dx + dy * dy <= WHITE_REACH * WHITE_REACH
                                && nx >= 0
                                && ny >= 0
                                && (nx as usize) < W
                                && (ny as usize) < H
                                && background(ny as usize * W + nx as usize)
                        })
                    });
                    if near_white {
                        piece.hole[o] = [255.0; 3];
                    }
                }
            }
            *slot = Some(piece);
        }
        Some(View { base: base.iter().map(|p| p | 0xFF00_0000).collect(), pieces })
    }
}

/// A piece pressed by `depth` (0..=1): the hole it leaves, then the piece
/// itself, smaller about its centre and darker.
fn draw_pressed(frame: &mut [u32], piece: &Piece, depth: f32, sink: f32) {
    let scale = 1.0 - (1.0 - sink) * depth;
    let hole = (depth * 1.6).min(1.0);
    let dim = 1.0 - SHADE * depth;
    let (cx, cy) = (piece.w as f32 / 2.0, piece.h as f32 / 2.0);
    for y in 0..piece.h {
        for x in 0..piece.w {
            let o = y * piece.w + x;
            let target = &mut frame[(y + piece.y) * W + x + piece.x];
            let a = piece.cover[o] * hole;
            if a > 0.0 {
                let h = piece.hole[o];
                over(target, [h[0] * a, h[1] * a, h[2] * a], a);
            }
            let (rgb, alpha) = piece.sample((x as f32 + 0.5 - cx) / scale + cx, (y as f32 + 0.5 - cy) / scale + cy);
            if alpha > 0.0 {
                over(target, [rgb[0] * dim, rgb[1] * dim, rgb[2] * dim], alpha);
            }
        }
    }
}

/// The D-pad rocked toward (tx, ty), each -1..=1. Pixels near the pressed end
/// come from further out in the cross (so that end looks narrower), the whole
/// cross is slightly shorter, and the pressed end falls into shadow.
fn draw_cross(frame: &mut [u32], cross: &Piece, tx: f32, ty: f32) {
    let (w, h) = (cross.w as f32, cross.h as f32);
    let hole = (tx.abs().max(ty.abs()) * 1.6).min(1.0);
    let (fx, fy) = (1.0 - FORESHORTEN * tx.abs(), 1.0 - FORESHORTEN * ty.abs());
    // 0 at the end that rises, 1 at the end that goes down.
    let far = |t: f32, tilt: f32| if tilt > 0.0 { t } else { 1.0 - t };
    let shadow = |t: f32, tilt: f32| CROSS_SHADE * tilt.abs() * ((far(t, tilt) - 0.35) / 0.65).clamp(0.0, 1.0);
    for y in 0..cross.h {
        for x in 0..cross.w {
            let o = y * cross.w + x;
            let target = &mut frame[(y + cross.y) * W + x + cross.x];
            let a = cross.cover[o] * hole;
            if a > 0.0 {
                let c = cross.hole[o];
                over(target, [c[0] * a, c[1] * a, c[2] * a], a);
            }
            // Undo the horizontal rock, then the vertical one, to find where this pixel comes from.
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let column = w / 2.0 + (px - w / 2.0) / fx;
            let u = (column / w).clamp(0.0, 1.0);
            let narrow_x = 1.0 - TILT * tx.abs() * far(u, tx);
            let row = h / 2.0 + ((h / 2.0 + (py - h / 2.0) / narrow_x) - h / 2.0) / fy;
            let v = (row / h).clamp(0.0, 1.0);
            let narrow_y = 1.0 - TILT * ty.abs() * far(v, ty);
            let source_x = w / 2.0 + (column - w / 2.0) / narrow_y;
            let (rgb, alpha) = cross.sample(source_x, row);
            if alpha > 0.0 {
                let dim = (1.0 - shadow(u, tx)) * (1.0 - shadow(v, ty));
                over(target, [rgb[0] * dim, rgb[1] * dim, rgb[2] * dim], alpha);
            }
        }
    }
}

/// A stick: inside its bright ring, the dark socket, then the cap shifted by
/// `lean` and, when clicked, smaller and darker. The cap's pixels are taken
/// from the render itself.
fn draw_stick(frame: &mut [u32], base: &[u32], (cx, cy, radius): (f32, f32, f32), lean: (f32, f32), click: f32) {
    let scale = 1.0 - (1.0 - STICK_SINK) * click;
    let dim = 1.0 - SHADE * click;
    let reach = radius.ceil() as i32 + 1;
    let at = |x: f32, y: f32| -> [f32; 3] {
        // The render at a point, blended from the four nearest pixels.
        let (fx, fy) = (x - 0.5, y - 0.5);
        let (x0, y0) = (fx.floor().clamp(0.0, W as f32 - 2.0), fy.floor().clamp(0.0, H as f32 - 2.0));
        let (tx, ty) = ((fx - x0).clamp(0.0, 1.0), (fy - y0).clamp(0.0, 1.0));
        let i = y0 as usize * W + x0 as usize;
        let (a, b, c, d) = (channels(base[i]), channels(base[i + 1]), channels(base[i + W]), channels(base[i + W + 1]));
        std::array::from_fn(|k| (a[k] * (1.0 - tx) + b[k] * tx) * (1.0 - ty) + (c[k] * (1.0 - tx) + d[k] * tx) * ty)
    };
    for py in (cy as i32 - reach).max(0)..=(cy as i32 + reach).min(H as i32 - 1) {
        for px in (cx as i32 - reach).max(0)..=(cx as i32 + reach).min(W as i32 - 1) {
            let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
            let window = (radius + 0.5 - ((x - cx).powi(2) + (y - cy).powi(2)).sqrt()).clamp(0.0, 1.0);
            if window <= 0.0 {
                continue;
            }
            // The socket: dark, darkest on the side the cap has left.
            let from = ((x - cx - lean.0 * 2.0).powi(2) + (y - cy - lean.1 * 2.0).powi(2)).sqrt();
            let t = ((from - radius * 0.5) / (radius * 0.5 + 1.0)).clamp(0.0, 1.0);
            let mut colour = [20.0 - 17.0 * t, 21.0 - 18.0 * t, 24.0 - 20.0 * t];
            // The cap, where it now lies.
            let (sx, sy) = ((x - cx - lean.0) / scale + cx, (y - cy - lean.1) / scale + cy);
            let cap = (radius + 0.5 - ((sx - cx).powi(2) + (sy - cy).powi(2)).sqrt() * scale).clamp(0.0, 1.0);
            if cap > 0.0 {
                let c = at(sx, sy);
                for k in 0..3 {
                    colour[k] = c[k] * dim * cap + colour[k] * (1.0 - cap);
                }
            }
            over(&mut frame[py as usize * W + px as usize], [colour[0] * window, colour[1] * window, colour[2] * window], window);
        }
    }
}

/// A part of the frame: x0, y0, x1, y1, the last two exclusive.
pub type Area = [usize; 4];
const WHOLE: Area = [0, 0, W, H];

fn union(a: Option<Area>, b: Option<Area>) -> Option<Area> {
    match (a, b) {
        (Some(a), Some(b)) => Some([a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]),
        (a, b) => a.or(b),
    }
}

pub struct Picture {
    views: [View; 3],
    /// How far the picture sways, in radians about the controller's axes: it
    /// follows the controller's turns and swings back.
    sway: [f32; 3],
    /// Where the controller is, in pixels from its place: eased, and as
    /// last reported for drawing.
    eased_shift: (f32, f32),
    shown_shift: (f32, f32),
    /// The picture itself changed since it was last composed.
    stale: bool,
    /// The part of the frame the last composition drew over (and which view
    /// it showed, if only one), and the part that differs from the
    /// composition before it.
    touched: (Option<usize>, Option<Area>),
    dirty: Option<Area>,
    /// How far down each input is, 0..=1, eased.
    amount: [f32; KEYS.len()],
    /// When each held input went down, in milliseconds on the picture's clock.
    since: [Option<f32>; KEYS.len()],
    /// Where each stick is drawn, in pixels, eased.
    lean: [(f32, f32); 2],
    /// How visible each view is, 0..=1.
    shown: [f32; 3],
    target: usize,
    last_side_press: f32,
    clock: f32,
    frame: Vec<u32>,
    layer: Vec<u32>,
}

impl Picture {
    /// Decodes the renders and cuts them into pieces. Needs the drawing
    /// library to be loaded.
    pub fn load() -> Option<Picture> {
        let views = [View::load(FRONT)?, View::load(TOP)?, View::load(BACK)?];
        let frame = views[FRONT].base.clone();
        Some(Picture {
            views,
            sway: [0.0; 3],
            eased_shift: (0.0, 0.0),
            shown_shift: (0.0, 0.0),
            stale: true,
            touched: (None, Some(WHOLE)),
            dirty: Some(WHOLE),
            amount: [0.0; KEYS.len()],
            since: [None; KEYS.len()],
            lean: [(0.0, 0.0); 2],
            shown: [1.0, 0.0, 0.0],
            target: FRONT,
            last_side_press: -HOLD_MS,
            clock: 0.0,
            layer: vec![0; W * H],
            frame,
        })
    }

    /// The view to show: the most recent press decides, and a side view
    /// stays for a moment after its last button is released.
    fn wanted_view(&mut self) -> usize {
        let latest = (0..KEYS.len()).filter_map(|k| self.since[k].map(|t| (t, k))).max_by(|a, b| a.0.total_cmp(&b.0));
        match latest {
            Some((_, key)) => {
                let side = KEYS[key].1;
                if side != FRONT {
                    self.last_side_press = self.clock;
                } else {
                    self.last_side_press = -HOLD_MS;
                }
                side
            }
            None if self.target != FRONT && self.clock - self.last_side_press < HOLD_MS => self.target,
            None => FRONT,
        }
    }

    /// Advances the animation by `dt` milliseconds toward `input`. Returns
    /// true if the picture changed and has to be composed and drawn again.
    pub fn step(&mut self, dt: f32, input: Input) -> bool {
        self.clock += dt;
        let toward = |ms: f32| 1.0 - (-dt / ms).exp();
        let mut changed = false;
        let mut ease = |value: &mut f32, goal: f32, rate: f32| {
            let next = if (goal - *value).abs() < 0.002 { goal } else { *value + (goal - *value) * rate };
            if next != *value {
                *value = next;
                changed = true;
            }
        };
        for (k, &(mask, _, _)) in KEYS.iter().enumerate() {
            let held = input.buttons & mask != 0;
            match (held, self.since[k]) {
                (true, None) => self.since[k] = Some(self.clock),
                (false, Some(_)) => self.since[k] = None,
                _ => {}
            }
            ease(&mut self.amount[k], if held { 1.0 } else { 0.0 }, toward(if held { PRESS_MS } else { RELEASE_MS }));
        }
        for (side, stick) in [input.left, input.right].into_iter().enumerate() {
            let rest = stick.0.abs() < STICK_DEADZONE && stick.1.abs() < STICK_DEADZONE;
            let goal = if rest { (0.0, 0.0) } else { (stick.0 * STICK_TRAVEL, stick.1 * STICK_TRAVEL) };
            let (dx, dy) = (goal.0 - self.lean[side].0, goal.1 - self.lean[side].1);
            // Small differences are left alone, unless the stick is back at rest: then it returns exactly.
            if dx.abs().max(dy.abs()) < STICK_STEP && !(rest && self.lean[side] != (0.0, 0.0)) {
                continue;
            }
            let rate = if dx.abs().max(dy.abs()) < STICK_STEP { 1.0 } else { toward(STICK_MS) };
            self.lean[side] = (self.lean[side].0 + dx * rate, self.lean[side].1 + dy * rate);
            changed = true;
        }
        self.target = self.wanted_view();
        for side in 0..3 {
            let goal = if side == self.target { 1.0 } else { 0.0 };
            let step = dt / FADE_MS;
            let next = self.shown[side] + (goal - self.shown[side]).clamp(-step, step);
            if next != self.shown[side] {
                self.shown[side] = next;
                changed = true;
            }
        }
        let back = (-dt / SWAY_RETURN_MS).exp();
        for (sway, turn) in self.sway.iter_mut().zip(input.turn) {
            *sway = (*sway + turn * SWAY_GAIN) * back;
            if sway.abs() < 1e-5 {
                *sway = 0.0;
            }
        }
        self.stale |= changed;
        let goal = self.wanted_shift();
        let rate = 1.0 - (-dt / SHIFT_EASE_MS).exp();
        let follow = |value: f32, goal: f32| if (goal - value).abs() < 0.004 { goal } else { value + (goal - value) * rate };
        self.eased_shift = (follow(self.eased_shift.0, goal.0), follow(self.eased_shift.1, goal.1));
        let moved = (self.eased_shift.0 - self.shown_shift.0).abs().max((self.eased_shift.1 - self.shown_shift.1).abs());
        // Small changes wait, except the last one back into place.
        if moved > SHIFT_STEP || (moved > 0.0 && self.eased_shift == (0.0, 0.0)) {
            self.shown_shift = self.eased_shift;
            changed = true;
        }
        changed
    }

    /// Whether the picture itself changed since this was last asked, and so
    /// has to be composed again; a shift alone does not count.
    pub fn take_stale(&mut self) -> bool {
        std::mem::take(&mut self.stale)
    }

    /// Where to draw the controller: the sway past its deadzone, as it looks
    /// in the views being shown, eased into its limit. The side that comes
    /// nearer is the side the controller moves toward.
    fn wanted_shift(&self) -> (f32, f32) {
        let size = self.sway.iter().map(|v| v * v).sum::<f32>().sqrt();
        if size <= SWAY_DEADZONE {
            return (0.0, 0.0);
        }
        let sway = self.sway.map(|v| v * (size - SWAY_DEADZONE) / size);
        let weights = self.shown.map(smooth);
        let total: f32 = weights.iter().sum();
        let (mut about_x, mut about_y) = (0.0, 0.0);
        for (side, weight) in weights.iter().enumerate() {
            let seen = on_screen(side, sway);
            about_x += seen[0] * weight / total;
            about_y += seen[1] * weight / total;
        }
        let limit = |v: f32| MAX_SHIFT * (v * SHIFT_PER_RAD / MAX_SHIFT).tanh();
        (limit(-about_y), limit(-about_x))
    }

    /// Where the controller is drawn, in pixels from its place: at most
    /// MAX_SHIFT either way.
    pub fn shift(&self) -> (f32, f32) {
        self.shown_shift
    }

    /// Jumps to the end of every animation, in place (for still renders and tests).
    #[cfg(any(test, feature = "tools"))]
    pub fn settle(&mut self, input: Input) {
        for _ in 0..200 {
            self.step(16.0, Input { turn: [0.0; 3], ..input });
        }
        self.pose([0.0; 3]);
    }

    /// Holds the picture as if the controller had just turned this far.
    #[cfg(any(test, feature = "tools"))]
    pub fn pose(&mut self, sway: [f32; 3]) {
        self.sway = sway;
        self.eased_shift = self.wanted_shift();
        self.shown_shift = self.eased_shift;
    }

    /// Draws one view, and returns the part of it that is not the plain render.
    fn draw_view(&self, side: usize, frame: &mut [u32]) -> Option<Area> {
        let view = &self.views[side];
        frame.copy_from_slice(&view.base);
        let mut touched = None;
        let mut touch = |x0: usize, y0: usize, x1: usize, y1: usize| touched = union(touched, Some([x0, y0, x1.min(W), y1.min(H)]));
        if side == FRONT {
            for (stick, key) in [(0, LS), (1, RS)] {
                let (lean, click) = (self.lean[stick], self.amount[key]);
                if lean.0.abs() + lean.1.abs() > 0.02 || click > 0.004 {
                    let (cx, cy, radius) = STICKS[stick];
                    let reach = radius + 3.0;
                    touch((cx - reach).max(0.0) as usize, (cy - reach).max(0.0) as usize, (cx + reach) as usize + 1, (cy + reach) as usize + 1);
                    draw_stick(frame, &view.base, STICKS[stick], lean, click);
                }
            }
            let (tx, ty) = (self.amount[RIGHT] - self.amount[LEFT], self.amount[DOWN] - self.amount[UP]);
            if tx.abs() > 0.004 || ty.abs() > 0.004 {
                if let Some(Some(cross)) = view.pieces.get(CROSS_PIECE as usize) {
                    touch(cross.x, cross.y, cross.x + cross.w, cross.y + cross.h);
                    draw_cross(frame, cross, tx, ty);
                }
            }
        }
        for (k, &(_, key_side, number)) in KEYS.iter().enumerate() {
            if key_side == side && number != 0 && self.amount[k] > 0.004 {
                if let Some(Some(piece)) = view.pieces.get(number as usize) {
                    touch(piece.x, piece.y, piece.x + piece.w, piece.y + piece.h);
                    draw_pressed(frame, piece, self.amount[k], SINK[side]);
                }
            }
        }
        touched
    }

    /// The picture as last composed.
    pub fn frame(&self) -> &[u32] {
        &self.frame
    }

    /// The part of the frame in which the last composition differs from the
    /// one before it; None if they are the same.
    pub fn dirty(&self) -> Option<Area> {
        self.dirty
    }

    /// Draws the current state and returns it, W x H pixels of 0xFFRRGGBB.
    pub fn compose(&mut self) -> &[u32] {
        let weights = self.shown.map(smooth);
        let total: f32 = weights.iter().sum();
        let mut frame = std::mem::take(&mut self.frame);
        let mut layer = std::mem::take(&mut self.layer);
        let mut covered = 0.0;
        // Which view is shown, if only one, and what was drawn over it.
        let mut now: (Option<usize>, Option<Area>) = (None, None);
        let mut order = [FRONT, TOP, BACK];
        order.sort_by(|a, b| weights[*b].total_cmp(&weights[*a]));
        for side in order {
            let share = weights[side] / total;
            if share < 0.002 {
                continue;
            }
            if covered == 0.0 {
                now = (Some(side), self.draw_view(side, &mut frame));
            } else {
                // Two views at once: the whole frame is in play.
                now = (None, Some(WHOLE));
                // Cross-fade: mix this view into what is already there.
                self.draw_view(side, &mut layer);
                let mix = share / (covered + share);
                for (under, &top) in frame.iter_mut().zip(layer.iter()) {
                    if *under != top {
                        let (u, t) = (channels(*under), channels(top));
                        *under = pixel([u[0] + (t[0] - u[0]) * mix, u[1] + (t[1] - u[1]) * mix, u[2] + (t[2] - u[2]) * mix]);
                    }
                }
            }
            covered += share;
        }
        // What changed: everything if the view did, else what either composition drew over.
        self.dirty = if now.0.is_none() || now.0 != self.touched.0 { Some(WHOLE) } else { union(now.1, self.touched.1) };
        self.touched = now;
        self.frame = frame;
        self.layer = layer;
        &self.frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded() -> (gfx::Session, Picture) {
        let session = gfx::Session::start().expect("GDI+");
        let picture = Picture::load().expect("the embedded renders must decode and match the picture size");
        (session, picture)
    }

    #[test]
    fn input_survives_packing() {
        let input = Input::unpack(Input::pack(btn::A | btn::ZL, 32767, 32767, -32768, 0));
        assert_eq!(input.buttons, btn::A | btn::ZL);
        // Full right and up on the left stick: clamped to the unit circle, y pointing down.
        assert!((input.left.0 - 0.707).abs() < 0.01 && (input.left.1 + 0.707).abs() < 0.01, "{:?}", input.left);
        assert!((input.right.0 + 1.0).abs() < 0.01 && input.right.1.abs() < 0.01, "{:?}", input.right);
        assert_eq!(Input::unpack(Input::pack(0, 0, 0, 0, 0)), Input::default());
    }

    #[test]
    fn every_piece_is_cut() {
        let (_session, picture) = loaded();
        for &(_, side, number) in KEYS.iter().filter(|k| k.2 != 0) {
            let piece = picture.views[side].pieces[number as usize].as_ref().expect("piece");
            assert!(piece.w > 10 && piece.h > 10 && piece.cover.iter().any(|&c| c > 0.99), "view {side} piece {number}");
        }
        let cross = picture.views[FRONT].pieces[CROSS_PIECE as usize].as_ref().expect("the D-pad");
        // The cross is as wide as it is tall, to within a pixel.
        assert!((cross.w as i32 - cross.h as i32).abs() <= 1, "{} x {}", cross.w, cross.h);
    }

    #[test]
    fn at_rest_the_picture_is_the_render() {
        let (_session, mut picture) = loaded();
        assert!(!picture.step(16.0, Input::default()));
        let base = picture.views[FRONT].base.clone();
        assert_eq!(picture.compose(), &base[..]);
    }

    #[test]
    fn a_press_changes_only_its_own_piece() {
        let (_session, mut picture) = loaded();
        picture.settle(Input { buttons: btn::A, ..Input::default() });
        let base = picture.views[FRONT].base.clone();
        let (x0, y0, x1, y1) = {
            let a = picture.views[FRONT].pieces[1].as_ref().unwrap();
            (a.x, a.y, a.x + a.w, a.y + a.h)
        };
        let frame = picture.compose();
        let mut changed = 0;
        for i in 0..W * H {
            if frame[i] != base[i] {
                let (x, y) = (i % W, i / W);
                assert!(x >= x0 && x < x1 && y >= y0 && y < y1, "pixel {x},{y} changed outside A");
                changed += 1;
            }
        }
        assert!(changed > 200, "only {changed} pixels changed");
    }

    #[test]
    fn triggers_and_paddles_switch_the_view_and_it_comes_back() {
        let (_session, mut picture) = loaded();
        picture.settle(Input { buttons: btn::ZR, ..Input::default() });
        assert_eq!(picture.target, TOP);
        assert!(picture.shown[TOP] > 0.99);
        // A front button pressed later wins.
        picture.settle(Input { buttons: btn::ZR | btn::B, ..Input::default() });
        assert_eq!(picture.target, FRONT);
        picture.settle(Input { buttons: btn::GL, ..Input::default() });
        assert_eq!(picture.target, BACK);
        // Released: the back view is held, then fades back to the front.
        picture.step(100.0, Input::default());
        assert_eq!(picture.target, BACK);
        picture.settle(Input::default());
        assert_eq!(picture.target, FRONT);
        assert!(picture.shown[FRONT] > 0.99);
    }

    #[test]
    fn turns_survive_packing_and_wrapping() {
        let radians = |counts: f32| counts / GYRO_LSB_PER_RAD * REPORT_S;
        let mut total = 0;
        for _ in 0..10 {
            total = turned(total, [1000, -2000, 0]);
        }
        let turn = turn_between(0, total);
        assert!((turn[0] - radians(10_000.0)).abs() < 1e-6 && (turn[1] - radians(-20_000.0)).abs() < 1e-6 && turn[2] == 0.0, "{turn:?}");
        // The counters wrap around many times; differences stay right.
        for _ in 0..5000 {
            total = turned(total, [32767, -32768, 12345]);
        }
        let turn = turn_between(total, turned(total, [-7, 7, 32767]));
        assert!((turn[0] - radians(-7.0)).abs() < 1e-7 && (turn[1] - radians(7.0)).abs() < 1e-7 && (turn[2] - radians(32767.0)).abs() < 1e-6, "{turn:?}");
    }

    #[test]
    fn a_still_controller_leaves_the_picture_in_place() {
        let (_session, mut picture) = loaded();
        for _ in 0..200 {
            assert!(!picture.step(15.0, Input { turn: [0.0002, -0.0002, 0.0001], ..Input::default() }));
        }
        assert_eq!(picture.shift(), (0.0, 0.0));
        let base = picture.views[FRONT].base.clone();
        assert_eq!(picture.compose(), &base[..]);
    }

    #[test]
    fn a_turn_shifts_the_picture_smoothly_and_it_comes_back() {
        let (_session, mut picture) = loaded();
        picture.take_stale();
        // Tipped about X only, over a fifth of a second: the picture moves up or down, not sideways.
        let mut last = 0.0f32;
        for _ in 0..14 {
            picture.step(15.0, Input { turn: [0.03, 0.0, 0.0], ..Input::default() });
            let (x, y) = picture.shift();
            assert_eq!(x, 0.0);
            // No frame jumps by more than a fraction of a pixel.
            assert!((y - last).abs() < 0.4, "{last} -> {y}");
            last = y;
        }
        assert!(last.abs() > 0.5 && last.abs() <= MAX_SHIFT, "{last}");
        // Only its place changed: the picture itself is the render, and was never stale.
        assert!(!picture.take_stale());
        let base = picture.views[FRONT].base.clone();
        assert_eq!(picture.compose(), &base[..]);
        for _ in 0..800 {
            picture.step(15.0, Input::default());
        }
        assert_eq!(picture.shift(), (0.0, 0.0));
        assert!(!picture.step(15.0, Input::default()));
    }

    #[test]
    fn placing_by_a_fraction_blends_every_pixel_from_its_four_neighbours() {
        // A picture of noise, against the rule written out pixel by pixel.
        let (w, h, room) = (9usize, 7usize, 3usize);
        let mut seed = 12345u32;
        let source: Vec<u32> = (0..w * h)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                0xFF00_0000 | (seed >> 8)
            })
            .collect();
        let at = |x: i32, y: i32| if x >= 0 && y >= 0 && x < w as i32 && y < h as i32 { source[y as usize * w + x as usize] } else { 0x00FF_FFFF };
        for by in [(0.3f32, 0.6f32), (-1.25, 1.5), (2.0, -0.75), (-2.0, -2.0 + 0.1)] {
            let mut out = Vec::new();
            place(&source, w, h, room, by, 0.0, &mut out);
            let (fx, fy) = (by.0.floor(), by.1.floor());
            let (px, py) = (((by.0 - fx) * 256.0) as u32, ((by.1 - fy) * 256.0) as u32);
            let (ow, oh) = (w + room * 2, h + room * 2);
            for oy in 0..oh as i32 {
                for ox in 0..ow as i32 {
                    let (x, y) = (ox - room as i32 - fx as i32, oy - room as i32 - fy as i32);
                    let upper = mix(at(x, y - 1), at(x - 1, y - 1), px);
                    let lower = mix(at(x, y), at(x - 1, y), px);
                    let want = mix(lower, upper, py) & 0xFF_FFFF;
                    assert_eq!(out[oy as usize * ow + ox as usize] & 0xFF_FFFF, want, "{by:?} at {ox},{oy}");
                }
            }
        }
    }

    #[test]
    fn placing_moves_the_picture_by_whole_pixels_and_by_fractions() {
        // A 3 x 2 picture: black, grey, black over white, white, white.
        let source = [0xFF00_0000, 0xFF80_8080, 0xFF00_0000, 0xFFFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF];
        let mut out = Vec::new();
        place(&source, 3, 2, 2, (0.0, 0.0), 0.0, &mut out);
        assert_eq!(out.len(), 7 * 6);
        let row = |out: &[u32], y: usize| out[y * 7..y * 7 + 7].to_vec();
        const W: u32 = 0xFFFF_FFFF;
        assert_eq!(row(&out, 2), [W, W, 0xFF00_0000, 0xFF80_8080, 0xFF00_0000, W, W]);
        assert!(row(&out, 1).iter().chain(&row(&out, 3)).all(|&p| p == W));
        // One pixel right and one up: the same pixels, moved.
        place(&source, 3, 2, 2, (1.0, -1.0), 0.0, &mut out);
        assert_eq!(row(&out, 1), [W, W, W, 0xFF00_0000, 0xFF80_8080, 0xFF00_0000, W]);
        // Half a pixel right: each pixel is the mean of two neighbours.
        place(&source, 3, 2, 2, (0.5, 0.0), 0.0, &mut out);
        assert_eq!(row(&out, 2), [W, W, 0xFF7F_7F7F, 0xFF40_4040, 0xFF40_4040, 0xFF7F_7F7F, W]);
        // Veiled, black comes most of the way to white and white stays white.
        place(&source, 3, 2, 2, (0.0, 0.0), 1.0, &mut out);
        assert_eq!((out[2 * 7 + 2], out[0]), (0xFFC7_C7C7, W));
    }

    #[test]
    fn only_the_part_that_changed_is_reported_dirty() {
        let (_session, mut picture) = loaded();
        picture.compose();
        assert_eq!(picture.dirty(), Some(WHOLE), "the first composition is all new");
        picture.compose();
        assert_eq!(picture.dirty(), None, "nothing changed");
        // Pressing A dirties A's box; holding it keeps that box; letting go dirties it once more.
        let a = { let p = picture.views[FRONT].pieces[1].as_ref().unwrap(); [p.x, p.y, p.x + p.w, p.y + p.h] };
        picture.settle(Input { buttons: btn::A, ..Input::default() });
        let before = picture.frame.clone();
        picture.compose();
        assert_eq!(picture.dirty(), Some(a));
        for i in 0..W * H {
            let inside = (a[0]..a[2]).contains(&(i % W)) && (a[1]..a[3]).contains(&(i / W));
            assert!(inside || picture.frame[i] == before[i], "pixel {},{} changed outside the dirty part", i % W, i / W);
        }
        picture.settle(Input::default());
        picture.compose();
        assert_eq!(picture.dirty(), Some(a));
        picture.compose();
        assert_eq!(picture.dirty(), None);
        // Another view: everything.
        picture.settle(Input { buttons: btn::ZR, ..Input::default() });
        picture.compose();
        assert_eq!(picture.dirty(), Some(WHOLE));
    }

    #[test]
    fn a_press_makes_the_picture_stale_once() {
        let (_session, mut picture) = loaded();
        assert!(picture.take_stale(), "a new picture has yet to be composed");
        assert!(!picture.take_stale());
        picture.step(15.0, Input { buttons: btn::A, ..Input::default() });
        assert!(picture.take_stale());
        assert!(!picture.take_stale());
    }

    #[test]
    fn the_shift_never_exceeds_its_limit() {
        let (_session, mut picture) = loaded();
        for buttons in [0, btn::ZR, btn::GL] {
            picture.settle(Input { buttons, ..Input::default() });
            for corner in 0..8 {
                let far = |bit: u32| if corner & bit != 0 { 9.0 } else { -9.0 };
                picture.pose([far(1), far(2), far(4)]);
                let (x, y) = picture.shift();
                assert!(x.abs() <= MAX_SHIFT && y.abs() <= MAX_SHIFT && x.abs().max(y.abs()) > MAX_SHIFT * 0.9, "{x},{y}");
            }
        }
    }

    #[test]
    fn a_stick_moves_only_inside_its_ring() {
        let (_session, mut picture) = loaded();
        picture.settle(Input { left: (1.0, 0.0), ..Input::default() });
        let base = picture.views[FRONT].base.clone();
        let (cx, cy, radius) = STICKS[0];
        let frame = picture.compose();
        let mut changed = 0;
        for i in 0..W * H {
            if frame[i] != base[i] {
                let (x, y) = ((i % W) as f32 + 0.5, (i / W) as f32 + 0.5);
                assert!(((x - cx).powi(2) + (y - cy).powi(2)).sqrt() <= radius + 1.0, "pixel {x},{y} changed outside the ring");
                changed += 1;
            }
        }
        assert!(changed > 500, "only {changed} pixels changed");
    }
}
