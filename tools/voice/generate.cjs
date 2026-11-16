// Speaks the two mode names with Kokoro (through sherpa-onnx) and saves them
// as WAV files.
//
//   PROTWO_TTS=<folder> node tools/voice/generate.cjs
//
// <folder> holds the speech tools, which are not part of this repository
// (about 1 GB):
//   app/node_modules/sherpa-onnx-node           npm install sherpa-onnx-node
//   models/kokoro-multi-lang-v1_0               from the sherpa-onnx "tts-models" release
//   models/sherpa-onnx-whisper-tiny.en          from its "asr-models" release (for score.cjs)
//
// Writes tools/voice/raw/<clip>.wav (24 kHz, mono, 16-bit).
'use strict';
const fs = require('fs');
const path = require('path');
const { createRequire } = require('module');

const TTS = process.env.PROTWO_TTS;
if (!TTS) throw new Error('set PROTWO_TTS to the folder of the speech tools');
const sherpa = createRequire(path.join(TTS, 'app', 'package.json'))('sherpa-onnx-node');
const KOKORO = path.join(TTS, 'models', 'kokoro-multi-lang-v1_0');
const OUT = path.join(__dirname, 'raw');

// The mode, said when a controller connects and when the mode changes.
const CLIPS = { directinput: 'Direct input.', xinput: 'X input.' };
// Voices, by their speaker number in the model. PROTWO_VOICE picks one.
const VOICES = { af_bella: 2, af_heart: 3, af_nicole: 6, af_nova: 7, af_sarah: 9, am_michael: 16, bf_emma: 21 };
const SPEAKER = VOICES[process.env.PROTWO_VOICE || 'af_heart'];
if (SPEAKER === undefined) throw new Error('unknown voice');
// A little slower than normal speech, so the words stay apart.
const SPEED = +(process.env.PROTWO_SPEED || 0.95);
// Words the model's dictionary does not know, in its phoneme spelling.
const NAMES = {};

const lexicon = new Map();
for (const line of fs.readFileSync(path.join(KOKORO, 'lexicon-us-en.txt'), 'utf8').split(/\r?\n/)) {
  const i = line.indexOf(' ');
  if (i > 0 && !lexicon.has(line.slice(0, i))) lexicon.set(line.slice(0, i), line.slice(i + 1).split(' ').join(''));
}
const tokens = new Set();
for (const line of fs.readFileSync(path.join(KOKORO, 'tokens.txt'), 'utf8').split(/\r?\n/)) {
  if (!line) continue;
  const i = line.lastIndexOf(' ');
  tokens.add(i <= 0 ? ' ' : line.slice(0, i) || ' ');
}

function phonemes(text) {
  const spoken = text.replace(/[A-Za-z']+/g, word => {
    const p = NAMES[word.toLowerCase()] || lexicon.get(word.toLowerCase());
    if (!p) throw new Error(`"${word}" is not in the dictionary`);
    return p;
  });
  return [...spoken.normalize('NFD')].filter(ch => tokens.has(ch)).map(ch => ch.codePointAt(0));
}

function writeWav(file, samples, rate) {
  const b = Buffer.alloc(44 + samples.length * 2);
  b.write('RIFF', 0); b.writeUInt32LE(36 + samples.length * 2, 4); b.write('WAVEfmt ', 8);
  b.writeUInt32LE(16, 16); b.writeUInt16LE(1, 20); b.writeUInt16LE(1, 22);
  b.writeUInt32LE(rate, 24); b.writeUInt32LE(rate * 2, 28); b.writeUInt16LE(2, 32); b.writeUInt16LE(16, 34);
  b.write('data', 36); b.writeUInt32LE(samples.length * 2, 40);
  samples.forEach((v, i) => b.writeInt16LE(Math.round(Math.max(-1, Math.min(1, v)) * 32767), 44 + i * 2));
  fs.writeFileSync(file, b);
}

const tts = new sherpa.OfflineTts({
  model: {
    kokoro: {
      model: path.join(KOKORO, 'model.onnx'),
      voices: path.join(KOKORO, 'voices.bin'),
      tokens: path.join(KOKORO, 'tokens.txt'),
      lexicon: [path.join(KOKORO, 'lexicon-us-en.txt'), path.join(KOKORO, 'lexicon-zh.txt')].join(','),
    },
    numThreads: 2, provider: 'cpu',
  },
  maxNumSentences: 1,
});

fs.mkdirSync(OUT, { recursive: true });
for (const [clip, text] of Object.entries(CLIPS)) {
  const config = new sherpa.GenerationConfig({ sid: SPEAKER, speed: SPEED, silenceScale: 0.2, phonemeCodepoints: [phonemes(text)] });
  const audio = tts.generate({ text: '', generationConfig: config });
  writeWav(path.join(OUT, `${clip}.wav`), audio.samples, audio.sampleRate);
  console.log(`${clip}  ${(audio.samples.length / audio.sampleRate).toFixed(2)} s`);
}
