// Listens to WAV files with a speech recogniser (Whisper tiny, through
// sherpa-onnx) and prints what it hears. It stands in for a listener when
// tuning the conversion: a rendering it can transcribe is one a person can
// follow, and one it cannot is at least a warning.
//
//   PROTWO_TTS=<folder> node tools/voice/score.cjs <file.wav>...
//
// Each line: the words wanted that were heard (out of how many), the file,
// and the transcription. Which words are wanted is read from the start of the
// file's name (a clip name from generate.cjs).
'use strict';
const fs = require('fs');
const path = require('path');
const { createRequire } = require('module');

const TTS = process.env.PROTWO_TTS;
if (!TTS) throw new Error('set PROTWO_TTS to the folder of the speech tools');
const sherpa = createRequire(path.join(TTS, 'app', 'package.json'))('sherpa-onnx-node');
const MODEL = path.join(TTS, 'models', 'sherpa-onnx-whisper-tiny.en');

function readWav(file) {
  const b = fs.readFileSync(file);
  let at = 12, rate = 0, data = null;
  while (at + 8 <= b.length) {
    const id = b.toString('latin1', at, at + 4), size = b.readUInt32LE(at + 4);
    if (id === 'fmt ') rate = b.readUInt32LE(at + 12);
    else if (id === 'data') data = new Int16Array(b.buffer.slice(b.byteOffset + at + 8, b.byteOffset + at + 8 + (size & ~1)));
    at += 8 + size + (size & 1);
  }
  return { sampleRate: rate, samples: Float32Array.from(data, v => v / 32768) };
}

const recognizer = new sherpa.OfflineRecognizer({
  featConfig: { sampleRate: 16000, featureDim: 80 },
  modelConfig: {
    whisper: { encoder: path.join(MODEL, 'tiny.en-encoder.int8.onnx'), decoder: path.join(MODEL, 'tiny.en-decoder.int8.onnx') },
    tokens: path.join(MODEL, 'tiny.en-tokens.txt'),
    numThreads: 2, provider: 'cpu',
  },
});

const WANTED = { directinput: ['direct', 'input'], xinput: ['x', 'input'] };
// Spellings the recogniser may use for the same words.
const words = text => text.toLowerCase()
  .replace(/x[- ]?box/g, 'xbox').replace(/dual[- ]?shock/g, 'dualshock')
  .replace(/[^a-z ]/g, ' ').split(/\s+/).filter(Boolean);
let total = 0;
for (const file of process.argv.slice(2)) {
  const wanted = WANTED[Object.keys(WANTED).find(name => path.basename(file).startsWith(name))] || [];
  const stream = recognizer.createStream();
  stream.acceptWaveform(readWav(file));
  recognizer.decode(stream);
  const text = recognizer.getResult(stream).text.trim();
  const heard = words(text);
  const hits = wanted.filter(w => heard.includes(w)).length;
  total += hits;
  console.log(`${hits}/${wanted.length}  ${path.basename(file)}  "${text}"`);
}
console.log(`total ${total}`);
