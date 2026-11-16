// Turns the spoken clips into something the controller's rumble
// actuators can play. The actuators cannot play recorded audio: between them
// they produce four pure tones, whose pitch and loudness can change many times
// a second. So every slice of speech is reduced to four tones.
//
// All four are overtones of the voice's pitch. Overtones of one pitch fuse
// into a single buzzing voice, which the ear reads as speech; unrelated
// whistles do not. Which four, and how loud, is what makes it understandable.
// The settings below were found by trying many and having a speech recogniser
// (score.cjs) listen to each as the actuator would sound it:
//
//  - One tone for each of the mouth's first two resonances, which between
//    them tell the vowels apart: the strongest overtone below SPLIT and the
//    strongest above it. The other two are the strongest that remain. A split
//    at 1200 Hz was understood best.
//  - Loudness is not evened out: each tone is as loud beside the others as it
//    is in the voice (LOUDNESS 1), so the strong overtones, which are the
//    resonances, stand clear of the weak ones. Lifting the quiet tones, as one
//    does for music, makes the words unintelligible. Spreading them further
//    apart is understood no better and makes the voice quieter.
//  - No lift for the upper tones (TILT 0), for the same reason.
//  - Overtones are taken from LOW up. The actuators are weak below 330 Hz, but
//    leaving those overtones out was understood worse: the lowest resonance
//    of a voice is often there, and a tone given to it is better faint than
//    given to something else.
//  - Hiss ("s", "sh", "x") has no pitch: it is played as rough, jittering
//    tones at the top of the actuator's range.
//  - A slice of 5 ms follows consonants better than one of 10 ms.
//
// Loudness is corrected for the actuator (actuator-response.json, measured
// with a microphone).
//
//   node tools/voice/build.mjs [--low 150] [--split 1200] [--tilt 0] [--loud 1] [--hop 5] [--out <dir>]
//
// Reads  tools/voice/raw/<clip>.wav          (from generate.cjs)
// Writes assets/speech/<clip>.bin             16-byte header: slice period in microseconds (u32 LE);
//                                             then 16 bytes per slice: 4 x (frequency code, amplitude), u16 LE
//        tools/voice/preview/<clip>-voice.wav        the recording, trimmed and levelled
//        tools/voice/preview/<clip>-tones.wav        the four tones as audio
//        tools/voice/preview/<clip>-controller.wav   the tones as the actuator would sound them
import { mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const option = (flag, fallback) => process.argv.includes(flag) ? process.argv[process.argv.indexOf(flag) + 1] : fallback;
const LOW = +option('--low', 150);          // Hz: no tone below this
const SPLIT = +option('--split', 1200);     // Hz: between the two resonances
const TILT = +option('--tilt', 0);          // dB per octave of lift above SPLIT
const LOUDNESS = +option('--loud', 1.0);    // exponent: 1 keeps the tones as far apart in loudness as they are in the voice
const HOP_MS = +option('--hop', 5);
const QUIET = !!option('--out', '');        // a trial run: previews only, elsewhere
const out = join(here, '..', '..', 'assets', 'speech');
const preview = option('--out', join(here, 'preview'));
const CLIPS = readdirSync(join(here, 'raw')).filter(f => f.endsWith('.wav')).map(f => f.slice(0, -4)).sort();

// Levelling of the recording.
const TARGET_RMS = 0.085;   // of full scale, over the trimmed clip
const PEAK_LIMIT = 0.89;
const SILENCE = 0.012;      // samples quieter than this are leading or trailing silence
const PAD_MS = 25;
const FADE_MS = 6;

// Analysis.
const WINDOW_MS = 30;
const NFFT = 2048;
const PITCH_RANGE = [70, 520];   // Hz, from a low man's voice to a high woman's
const PITCH_WINDOW_MS = 50;
const GATE = 0.03;               // slices quieter than this fraction of the loudest are silent
const TOP_HZ = 3400;             // the actuator produces nothing useful above this

function readWav(path) {
  const b = readFileSync(path);
  let at = 12, rate = 0, data = null;
  while (at + 8 <= b.length) {
    const id = b.toString('latin1', at, at + 4), size = b.readUInt32LE(at + 4);
    if (id === 'fmt ') rate = b.readUInt32LE(at + 12);
    else if (id === 'data') data = new Int16Array(b.buffer.slice(b.byteOffset + at + 8, b.byteOffset + at + 8 + size));
    at += 8 + size + (size & 1);
  }
  return { rate, samples: Float64Array.from(data, v => v / 32768) };
}

function writeWav(path, rate, samples) {
  const b = Buffer.alloc(44 + samples.length * 2);
  b.write('RIFF', 0); b.writeUInt32LE(36 + samples.length * 2, 4); b.write('WAVEfmt ', 8);
  b.writeUInt32LE(16, 16); b.writeUInt16LE(1, 20); b.writeUInt16LE(1, 22);
  b.writeUInt32LE(rate, 24); b.writeUInt32LE(rate * 2, 28); b.writeUInt16LE(2, 32); b.writeUInt16LE(16, 34);
  b.write('data', 36); b.writeUInt32LE(samples.length * 2, 40);
  samples.forEach((v, i) => b.writeInt16LE(Math.round(Math.max(-1, Math.min(1, v)) * 32767), 44 + i * 2));
  writeFileSync(path, b);
}

// Trims silence, brings the clip to a set loudness and adds short fades.
function level({ rate, samples }) {
  const pad = Math.round(rate * PAD_MS / 1000), fade = Math.round(rate * FADE_MS / 1000);
  const first = samples.findIndex(v => Math.abs(v) > SILENCE), last = samples.findLastIndex(v => Math.abs(v) > SILENCE);
  const cut = samples.slice(Math.max(0, first - pad), Math.min(samples.length, last + pad * 4));
  const rms = Math.sqrt(cut.reduce((s, v) => s + v * v, 0) / cut.length);
  const peak = cut.reduce((m, v) => Math.max(m, Math.abs(v)), 0);
  const gain = Math.min(TARGET_RMS / rms, PEAK_LIMIT / peak);
  for (let i = 0; i < cut.length; i++) cut[i] *= gain * Math.min(1, i / fade, (cut.length - 1 - i) / fade);
  return { rate, samples: cut };
}

// In-place radix-2 FFT.
function fft(re, im) {
  const n = re.length;
  for (let i = 1, j = 0; i < n; i++) {
    let bit = n >> 1;
    for (; j & bit; bit >>= 1) j ^= bit;
    j ^= bit;
    if (i < j) { [re[i], re[j]] = [re[j], re[i]]; [im[i], im[j]] = [im[j], im[i]]; }
  }
  for (let len = 2; len <= n; len <<= 1) {
    const angle = -2 * Math.PI / len;
    for (let i = 0; i < n; i += len) {
      for (let k = 0; k < len / 2; k++) {
        const c = Math.cos(angle * k), s = Math.sin(angle * k);
        const a = i + k, b = a + len / 2;
        const xr = re[b] * c - im[b] * s, xi = re[b] * s + im[b] * c;
        re[b] = re[a] - xr; im[b] = im[a] - xi; re[a] += xr; im[a] += xi;
      }
    }
  }
}

const median = values => [...values].sort((a, b) => a - b)[values.length >> 1];
const freqCode = hz => Math.max(0, Math.min(1023, Math.round(Math.log2(hz / 10) * 96)));

const response = JSON.parse(readFileSync(join(here, 'actuator-response.json'), 'utf8')).tones;
const smoothDb = response.map((_, i) => median(response.slice(Math.max(0, i - 1), i + 2).map(t => t.db)));
const referenceDb = median(response.filter(t => t.hz >= 600 && t.hz <= 1200).map(t => t.db));

// How loud the actuator is at `hz`, in dB relative to its best range.
function actuatorDb(hz) {
  const i = response.findIndex(t => t.hz >= hz);
  if (i < 0) return -60;
  if (i === 0) return smoothDb[0] - referenceDb;
  const a = response[i - 1], b = response[i], t = Math.log(hz / a.hz) / Math.log(b.hz / a.hz);
  return smoothDb[i - 1] + (smoothDb[i] - smoothDb[i - 1]) * t - referenceDb;
}
// Gain that evens the actuator out, within limits.
const correction = hz => 10 ** (Math.max(-6, Math.min(12, -actuatorDb(hz))) / 20);
// The lift given to the upper tones.
const tilt = hz => hz > SPLIT ? 10 ** (TILT * Math.log2(hz / SPLIT) / 20) : 1;

function analyse({ rate, samples }) {
  const hop = Math.round(rate * HOP_MS / 1000), size = Math.round(rate * WINDOW_MS / 1000);
  const binHz = rate / NFFT, count = Math.ceil(samples.length / hop);
  const raw = [];
  for (let f = 0; f < count; f++) {
    const centre = f * hop, frame = new Float64Array(size);
    let energy = 0;
    for (let i = 0; i < size; i++) {
      const v = samples[centre - (size >> 1) + i] ?? 0;
      frame[i] = v * (0.5 - 0.5 * Math.cos(2 * Math.PI * i / (size - 1)));
      energy += frame[i] * frame[i];
    }
    // Pitch: how well a longer, unweighted stretch of the recording repeats
    // at each lag. Of the lags that repeat about as well as the best, the
    // shortest is the pitch (the others are multiples of its period).
    const span = Math.round(rate * PITCH_WINDOW_MS / 1000), from = centre - (span >> 1);
    const lags = [];
    for (let lag = Math.floor(rate / PITCH_RANGE[1]); lag <= Math.ceil(rate / PITCH_RANGE[0]); lag++) {
      let sum = 0, a = 0, b = 0;
      for (let i = 0; i + lag < span; i++) {
        const x = samples[from + i] ?? 0, y = samples[from + i + lag] ?? 0;
        sum += x * y; a += x * x; b += y * y;
      }
      lags.push({ lag, r: sum / (Math.sqrt(a * b) + 1e-12) });
    }
    const strength = Math.max(...lags.map(l => l.r));
    const first = lags.find((l, i) => l.r >= strength * 0.9 && l.r >= (lags[i - 1]?.r ?? -1) && l.r >= (lags[i + 1]?.r ?? -1));
    const pitch = rate / (first ?? lags[0]).lag;
    const re = new Float64Array(NFFT), im = new Float64Array(NFFT);
    re.set(frame);
    fft(re, im);
    const mag = new Float64Array(NFFT / 2);
    for (let i = 0; i < NFFT / 2; i++) mag[i] = Math.hypot(re[i], im[i]);
    let high = 0, all = 0;
    for (let i = 1; i < NFFT / 2; i++) { const p = mag[i] * mag[i]; all += p; if (i * binHz > 3500) high += p; }
    raw.push({ level: Math.sqrt(energy / size), pitch, voiced: strength > 0.3, mag, hiss: high / (all + 1e-12) });
  }
  const pitchTrack = raw.map((_, i) => {
    const near = raw.slice(Math.max(0, i - 3), i + 4).filter(s => s.voiced).map(s => s.pitch);
    return near.length >= 3 ? median(near) : 0;
  });
  const loudest = Math.max(...raw.map(s => s.level));
  let seed = 4242;
  const random = () => (seed = (seed * 1103515245 + 12345) & 0x7FFFFFFF) / 0x7FFFFFFF;
  const peakNear = (mag, hz, width) => {
    let best = 0;
    for (let b = Math.max(1, Math.floor((hz - width) / binHz)); b <= Math.ceil((hz + width) / binHz) && b < mag.length; b++) best = Math.max(best, mag[b]);
    return best;
  };

  const frames = raw.map((s, i) => {
    const silent = [0, 1, 2, 3].map(() => ({ hz: 100, amp: 0 }));
    if (s.level < loudest * GATE) return silent;
    const f0 = pitchTrack[i];
    const candidates = [];
    if (f0) {
      // The voice's overtones the actuator can sound, each as strong as the voice is there, the upper ones lifted.
      for (let k = Math.ceil(LOW / f0); k * f0 <= TOP_HZ; k++) candidates.push({ hz: k * f0, amp: peakNear(s.mag, k * f0, f0 / 3) * tilt(k * f0) });
    } else {
      // No pitch: the strongest parts of the spectrum, jittered so they sound rough rather than sung.
      for (let hz = Math.max(LOW, 400); hz <= TOP_HZ; hz += 150) candidates.push({ hz: hz * (0.94 + 0.12 * random()), amp: peakNear(s.mag, hz, 75) * 0.7 * tilt(hz) });
      // Hiss above the actuator's range is folded down to the top of what it can do.
      if (s.hiss > 0.3) {
        const energy = Math.sqrt(s.hiss) * Math.max(...s.mag);
        for (let n = 0; n < 2; n++) candidates.push({ hz: 2700 + random() * 700, amp: energy });
      }
    }
    // One tone for each resonance, then the strongest of the rest.
    const byAmp = [...candidates].sort((a, b) => b.amp - a.amp);
    const chosen = [];
    const take = c => { if (c && !chosen.includes(c)) chosen.push(c); };
    take(byAmp.find(c => c.hz < SPLIT));
    take(byAmp.find(c => c.hz >= SPLIT));
    for (const c of byAmp) { if (chosen.length >= 4) break; take(c); }
    chosen.sort((a, b) => a.hz - b.hz);
    return silent.map((t, k) => chosen[k] ? { hz: chosen[k].hz, amp: chosen[k].amp } : t);
  });

  // Loudness: compress, then correct for the actuator.
  const top = Math.max(...frames.flatMap(f => f.map(t => t.amp)), 1e-12);
  for (const tones of frames) for (const t of tones) t.amp = t.amp ? Math.pow(t.amp / top, LOUDNESS) * correction(t.hz) : 0;
  const voicedPitch = pitchTrack.filter(Boolean);
  return { frames, pitch: voicedPitch.length ? median(voicedPitch) : 0 };
}

function toBinary(frames) {
  const peak = Math.max(...frames.flatMap(f => f.map(t => t.amp)), 1e-9);
  const b = Buffer.alloc(16 + frames.length * 16);
  b.writeUInt32LE(HOP_MS * 1000, 0);
  frames.forEach((tones, i) => tones.forEach((t, k) => {
    b.writeUInt16LE(freqCode(t.hz), 16 + i * 16 + k * 4);
    b.writeUInt16LE(Math.round(Math.min(1, t.amp / peak) * 65535), 16 + i * 16 + k * 4 + 2);
  }));
  return b;
}

// The four tones as audio, gliding between slices.
function synthesise(frames, rate) {
  const hop = Math.round(rate * HOP_MS / 1000), samples = new Float64Array(frames.length * hop);
  const peak = Math.max(...frames.flatMap(f => f.map(t => t.amp)), 1e-9);
  for (let k = 0; k < 4; k++) {
    let phase = 0;
    for (let i = 0; i < frames.length; i++) {
      const a = frames[i][k], b = frames[Math.min(frames.length - 1, i + 1)][k];
      const jump = Math.abs(a.hz - b.hz) > 400;   // another overtone: hop, do not glide
      for (let n = 0; n < hop; n++) {
        const t = n / hop;
        phase += 2 * Math.PI * (jump ? a.hz : a.hz + (b.hz - a.hz) * t) / rate;
        samples[i * hop + n] += Math.sin(phase) * (a.amp + (b.amp - a.amp) * t) / peak * 0.3;
      }
    }
  }
  return samples;
}

// What a microphone would pick up: each tone scaled by the actuator's measured loudness.
const throughActuator = frames => frames.map(tones => tones.map(t => ({ hz: t.hz, amp: t.amp * 10 ** (Math.max(-60, actuatorDb(t.hz)) / 20) })));

mkdirSync(preview, { recursive: true });
if (!QUIET) mkdirSync(out, { recursive: true });
for (const clip of CLIPS) {
  const wav = level(readWav(join(here, 'raw', `${clip}.wav`)));
  const { frames, pitch } = analyse(wav);
  writeWav(join(preview, `${clip}-controller.wav`), wav.rate, synthesise(throughActuator(frames), wav.rate));
  if (QUIET) continue;
  writeWav(join(preview, `${clip}-voice.wav`), wav.rate, wav.samples);
  writeWav(join(preview, `${clip}-tones.wav`), wav.rate, synthesise(frames, wav.rate));
  const binary = toBinary(frames);
  writeFileSync(join(out, `${clip}.bin`), binary);
  console.log(`${clip}  ${(wav.samples.length / wav.rate).toFixed(2)} s  pitch ${pitch.toFixed(0)} Hz  ${binary.length} bytes`);
}
