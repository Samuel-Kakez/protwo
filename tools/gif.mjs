// Turns a film recorded by `protwo render <file> film` (tools build)
// into a looping GIF:
//   node tools/gif.mjs <file.film> <out.gif> [scale]
// where scale is how much larger than the window the film was drawn (1.5).
// The film is width, height, frame count and milliseconds per frame (u32 LE
// each), then every frame's pixels as blue, green, red, x.
// The GIF gets the window's rounded corners: a thin grey outline, and nothing
// (transparent) outside it.
import { readFileSync, writeFileSync } from 'node:fs';

const [source, target, scale = "1.5"] = process.argv.slice(2);
if (!source || !target) throw new Error('usage: node tools/gif.mjs <file.film> <out.gif>');
const film = readFileSync(source);
const [w, h, count, stepMs] = [0, 4, 8, 12].map(at => film.readUInt32LE(at));
const frame = n => film.subarray(16 + n * w * h * 4, 16 + (n + 1) * w * h * 4);

// The outline: how far each pixel is inside the rounded rectangle decides
// whether it is outside (transparent), on the line (grey) or untouched.
const RADIUS = 8 * scale, LINE = 1 * scale, OUTLINE = [0xDE, 0xD7, 0xD0], CLEAR = 255;
const outside = new Uint8Array(w * h);
for (let y = 0; y < h; y++) {
  for (let x = 0; x < w; x++) {
    const qx = Math.abs(x + 0.5 - w / 2) - (w / 2 - RADIUS), qy = Math.abs(y + 0.5 - h / 2) - (h / 2 - RADIUS);
    const depth = RADIUS - Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) - Math.min(Math.max(qx, qy), 0);
    if (depth <= 0) { outside[y * w + x] = 1; continue; }
    const line = Math.min(1, Math.max(0, LINE + 0.5 - depth));
    if (line === 0) continue;
    for (let n = 0; n < count; n++) {
      const at = 16 + (n * w * h + y * w + x) * 4;
      for (let c = 0; c < 3; c++) film[at + c] = Math.round(film[at + c] + (OUTLINE[c] - film[at + c]) * line);
    }
  }
}

// The palette: the 256 boxes of a median cut over the colours of every frame.
const seen = new Map();
for (let i = 16; i < film.length; i += 4) {
  if (outside[((i - 16) >> 2) % (w * h)]) continue;
  const key = film[i] | (film[i + 1] << 8) | (film[i + 2] << 16);
  seen.set(key, (seen.get(key) || 0) + 1);
}
let boxes = [[...seen].map(([key, n]) => [key & 255, (key >> 8) & 255, key >> 16, n])];
const span = (box, c) => { let lo = 255, hi = 0; for (const p of box) { if (p[c] < lo) lo = p[c]; if (p[c] > hi) hi = p[c]; } return hi - lo; };
while (boxes.length < CLEAR) {
  let best = -1, bestSpan = 0, channel = 0;
  boxes.forEach((box, i) => {
    for (let c = 0; c < 3; c++) {
      const s = span(box, c);
      if (box.length > 1 && s > bestSpan) [best, bestSpan, channel] = [i, s, c];
    }
  });
  if (best < 0) break;
  const box = boxes[best].sort((a, b) => a[channel] - b[channel]);
  // Split where half of the pixels lie on each side.
  const total = box.reduce((sum, p) => sum + p[3], 0);
  let half = 0, cut = 0;
  while (cut < box.length - 1 && half + box[cut][3] <= total / 2) half += box[cut++][3];
  cut = Math.max(1, cut);
  boxes.splice(best, 1, box.slice(0, cut), box.slice(cut));
}
const palette = boxes.map(box => {
  const total = box.reduce((sum, p) => sum + p[3], 0);
  return [0, 1, 2].map(c => Math.round(box.reduce((sum, p) => sum + p[c] * p[3], 0) / total));
});
const index = new Map();
boxes.forEach((box, i) => box.forEach(p => index.set(p[0] | (p[1] << 8) | (p[2] << 16), i)));

// GIF's LZW, with codes packed lowest bit first into blocks of up to 255 bytes.
function compress(pixels) {
  const out = [];
  let bits = 0, held = 0, block = [];
  const emit = (code, size) => {
    held |= code << bits;
    bits += size;
    while (bits >= 8) {
      block.push(held & 255);
      held >>>= 8;
      bits -= 8;
      if (block.length === 255) { out.push(255, ...block); block = []; }
    }
  };
  const CLEAR = 256, END = 257;
  let table = new Map(), next = 258, size = 9;
  emit(CLEAR, size);
  let prefix = pixels[0];
  for (let i = 1; i < pixels.length; i++) {
    const key = prefix * 256 + pixels[i];
    const known = table.get(key);
    if (known !== undefined) { prefix = known; continue; }
    emit(prefix, size);
    if (next === 4096) {
      emit(CLEAR, size);
      table = new Map(); next = 258; size = 9;
    } else {
      table.set(key, next++);
      if (next > (1 << size) && size < 12) size++;
    }
    prefix = pixels[i];
  }
  emit(prefix, size);
  emit(END, size);
  if (bits) block.push(held & 255);
  if (block.length) out.push(block.length, ...block);
  out.push(0);
  return out;
}

const u16 = v => [v & 255, v >> 8];
const bytes = [...Buffer.from('GIF89a'), ...u16(w), ...u16(h), 0xF7, 0, 0];
for (let i = 0; i < 256; i++) bytes.push(...(palette[i] ? [palette[i][2], palette[i][1], palette[i][0]] : [0, 0, 0]));
bytes.push(0x21, 0xFF, 11, ...Buffer.from('NETSCAPE2.0'), 3, 1, 0, 0, 0);

// Each frame holds only the rectangle that changed since the one before; a
// frame with no change lengthens the one before it.
const parts = [];
for (let n = 0; n < count; n++) {
  const now = frame(n), before = n ? frame(n - 1) : null;
  let [x0, y0, x1, y1] = [w, h, -1, -1];
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const at = (y * w + x) * 4;
      if (!before || now[at] !== before[at] || now[at + 1] !== before[at + 1] || now[at + 2] !== before[at + 2]) {
        if (x < x0) x0 = x;
        if (x > x1) x1 = x;
        if (y < y0) y0 = y;
        if (y > y1) y1 = y;
      }
    }
  }
  if (x1 < 0) { parts[parts.length - 1].ms += stepMs; continue; }
  const pixels = new Uint8Array((x1 - x0 + 1) * (y1 - y0 + 1));
  let k = 0;
  for (let y = y0; y <= y1; y++) {
    for (let x = x0; x <= x1; x++) {
      const at = (y * w + x) * 4;
      pixels[k++] = outside[y * w + x] ? CLEAR : index.get(now[at] | (now[at + 1] << 8) | (now[at + 2] << 16));
    }
  }
  parts.push({ x: x0, y: y0, w: x1 - x0 + 1, h: y1 - y0 + 1, pixels, ms: stepMs });
}
const chunks = [Buffer.from(bytes)];
for (const part of parts) {
  chunks.push(Buffer.from([0x21, 0xF9, 4, 0x05, ...u16(Math.round(part.ms / 10)), CLEAR, 0,
    0x2C, ...u16(part.x), ...u16(part.y), ...u16(part.w), ...u16(part.h), 0, 8, ...compress(part.pixels)]));
}
chunks.push(Buffer.from([0x3B]));
const gif = Buffer.concat(chunks);
writeFileSync(target, gif);
console.log(`${target}  ${w}x${h}  ${parts.length} of ${count} frames  ${(gif.length / 1024).toFixed(0)} KB`);
