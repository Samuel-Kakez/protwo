# Architecture

ProTwo has three goals that shape every decision: stay small, add no measurable latency,
and need no setup beyond the virtual gamepad driver.

## Data flow

```
Switch 2 Pro Controller
   │ USB interface 1 (WinUSB, bulk)      handshake, calibration, LEDs
   │ USB interface 0 (HID)               input reports in, rumble out
   ▼
┌────────────────────── ProTwo ────────────────────────┐
│ bridge thread  read report → parse → map → submit    │
│ rumble thread  re-send current sample while active   │
│ notify thread  wait for game rumble from ViGEmBus    │
│ voice thread   feeds tones to the rumble thread      │
│ UI thread      tray icon and window, asleep at rest  │
└──────────────────────────────────────────────────────┘
   │ IOCTLs
   ▼
ViGEmBus ──► virtual DualShock 4 / Xbox 360 ──► games, Steam, emulators
```

## Modules

| File | Responsibility |
| --- | --- |
| [src/main.rs](../src/main.rs) | Entry point: starts the tray app or runs a console diagnostic |
| [src/app.rs](../src/app.rs) | Tray icon, window, menu and message loop |
| [src/view.rs](../src/view.rs) | What the window looks like and which areas are clickable |
| [src/picture.rs](../src/picture.rs) | The live controller picture: cut-out pieces, easing, the three views, the drift. No I/O, unit-tested |
| [src/gfx.rs](../src/gfx.rs) | Anti-aliased drawing on top of GDI+ |
| [src/bridge.rs](../src/bridge.rs) | Background service: hotplug loop, bridge session, shared status |
| [src/controller.rs](../src/controller.rs) | Finds the device, runs the handshake, reads calibration; rumble thread and sound player |
| [src/protocol.rs](../src/protocol.rs) | Wire format only: commands, report parsing, calibration maths, rumble encoding. No I/O, unit-tested |
| [src/mapping.rs](../src/mapping.rs) | Controller state to DualShock 4 / Xbox 360 reports. No I/O, unit-tested |
| [src/settings.rs](../src/settings.rs) | The remembered emulation mode |
| [src/voice.rs](../src/voice.rs) | The spoken clips (the two mode names), stored as tone tracks. No I/O, unit-tested |
| [src/chime.rs](../src/chime.rs) | The soft chimes for connecting and quitting. No I/O, unit-tested |
| [src/vigem.rs](../src/vigem.rs) | ViGEmBus client speaking the driver's IOCTLs directly |
| [src/usb.rs](../src/usb.rs) | WinUSB bulk transfers |
| [src/hid.rs](../src/hid.rs) | HID report read/write |
| [src/win.rs](../src/win.rs) | Handles, events, device interface enumeration, plug-in notifications |
| [src/cli.rs](../src/cli.rs) | Console diagnostics |
| [src/tools.rs](../src/tools.rs) | Developer tools (`record`, `render`), built only with `--features tools` |

The only dependency is `windows-sys` (raw Win32 bindings, compile-time only).

## Latency

- The bridge thread blocks in `ReadFile` on the HID device and wakes exactly when a report
  arrives. It parses (a few dozen integer operations), maps and submits in the same
  iteration. Nothing is queued, batched or timed.
- The thread runs at `THREAD_PRIORITY_TIME_CRITICAL`: it works for microseconds per report, so it takes nothing from a game, and no game thread can hold it up.
- Rumble writes happen on their own thread and their own device handle, so an output report
  can never delay an input report.
- The controller reports every 4 ms. That period, set by the controller, is the floor;
  ProTwo's own processing is microseconds.
- In XInput mode, unchanged reports are not re-submitted. In DirectInput mode every report is
  forwarded because motion data changes continuously.
- The user interface never touches the input path. The bridge tells it about status changes
  (connect, disconnect, mode) by posting a window message.

## Memory and CPU

- No allocations on the input path; buffers live on the stack.
- No polling. Waiting for a controller is a kernel wait on a plug-in notification
  (`CM_Register_Notification`); idle rumble is a parked thread; the UI thread sleeps in
  `GetMessage`.
- The window runs a 15 ms timer only while it is visible. Each tick reads the latest inputs
  and repaints the picture, and only the picture, if something in it moved. With no
  controller the timer stops as well, about a second after the picture has come to rest.
- GDI+ is loaded only while the window is visible. Hiding the window frees the back buffer,
  unloads GDI+ and trims the working set.
- Release builds use LTO, one codegen unit, `panic = "abort"` and stripped symbols.
- Measured while bridging at 250 Hz: about 25 MB working set with the window open (GDI+,
  fonts and the three renders), under 2 MB right after hiding to the tray. With the window
  open and the controller at rest, under 1% of one core.

## User interface

The window is drawn by the app itself with GDI+ rather than with a UI framework: there are
a few clickable areas and one picture, and a framework would cost more memory than the rest
of the app. `view.rs` lays the window out in a fixed logical coordinate space, and `app.rs`
scales it for the monitor's DPI and maps clicks back into that space.

The window has no title bar. It answers  so that its whole surface is its
own, draws its two buttons itself and is moved by dragging anywhere that is not a control.
The dash hides it to the tray; only the power button and **Quit** in the tray menu end the
process. Changes of state fade: the title and status with a controller's presence, the
veil over the picture, and the mode switch's pill, which slides.
A named mutex keeps a single instance, since only one program can own the controller.
The mode is written to `%APPDATA%\protwo\settings.txt` whenever it changes.

## Live picture

The picture is the official renders with the moving parts cut out. Three views share one
frame of 540 x 400 pixels: front, top (L, R, ZL, ZR) and back (GL, GR). The top and back
views are shown mirrored, so the left hand's buttons stay on the left; the lettering in the
middle of the back is kept readable. Each view comes
with a cut map that says, for every pixel, which piece it belongs to and how much of the
pixel that piece covers. The renders and maps are embedded (about 100 KB) and decoded only
while the window is visible.

`picture.rs` draws the current state in software, on plain pixel buffers:

- a pressed piece is redrawn slightly smaller and darker inside the dark opening it leaves;
- the D-pad is one cross that rocks toward the pressed direction;
- a stick's cap slides inside its bright ring, which stays in place;
- the most recent press decides the view, a side view stays half a second after release,
  and views cross-fade.

Everything is eased with short time constants. The bridge publishes buttons and sticks as
one packed atomic word per report, so the input path does one extra store and never waits
for the window.

## Drift

The picture shows that the gyro is alive without rotating or redrawing anything in it. The
bridge adds up the gyro readings into a second packed atomic word, and each frame the
window takes the difference. The picture sways by half of that turn and swings back within
about a second. The sway becomes a shift of at most 3.5 picture pixels, about two on
screen, eased with a 110 ms time constant so that neither the hand's tremor nor uneven
frame times show.

Drawing is kept cheap in three ways, since the picture can change sixty times a second:

- The picture is scaled to its size in the window only where it changed. `picture.rs`
  reports the part of the frame that differs from the last composition (a button's box, a
  stick's ring), and only that part is scaled again. A stick circling with a button held
  costs about 0.7 ms a frame; scaling the whole picture would cost 2.3 ms.
- A shift scales nothing. `picture::place` copies the scaled pixels into the picture's
  frame at the new position, blending neighbours for the fraction of a pixel: about 0.4 ms.
- The result is written straight into the window's buffer, and only the rows that changed
  are copied to the screen.

A controller lying still is below the deadzone, so nothing is redrawn at all.

## Sounds

Each actuator plays two pure tones at once, so the controller has four tones to make a
sound with. Every sound is a list of frames, each saying what the four tones are from that
moment on. The bridge thread decides when to play one and hands the frames to a small
thread that feeds the rumble thread, so the input path never waits. A sound takes priority
over game rumble while it lasts, and a new one cuts off the old one. There is no mute.

| When | Sound |
| --- | --- |
| A controller connects | A chime, then the name of the mode: "DirectInput" or "XInput" |
| The mode changes | The name of the new mode |
| ProTwo quits | A chime, played to its end before the virtual pad is unplugged |

The voice and the chime are driven at 46000 of the 65535 full scale, where game rumble is
held to 29000, the ceiling SDL uses. The higher ceiling is about 4 dB louder, for a few seconds of tones in the hundreds of
hertz rather than sustained low-frequency shaking.

**Chime.** `chime.rs` makes two soft chimes of two notes each. On connecting, a rising
fifth (E4, B4). On quitting, a falling fourth (A4, E4), slower and softer. A note swells
for 22 ms and dies away with a time constant of 300 ms, at about a third of the voice's
strength. The two actuators take one note each, so the first keeps ringing under the
second. Each actuator's second tone plays the note's octave, at a quarter of its level and
dying away faster: it rounds the start of the note and carries it to the ear, since the
actuators are weak below about 330 Hz.

**Voice.** The actuators cannot play a recording, so each clip is stored as a tone track
of 3 to 4 KB: every 5 ms, four tones with their loudness. `voice.rs` only decodes them.
There are two, the mode names.

They are made in `tools/voice`. `generate.cjs` speaks the words with a text-to-speech
model (Kokoro), and `build.mjs` reduces every 5 ms of speech to four tones. All four are
overtones of the voice's pitch, because overtones of one pitch fuse into a single buzzing
voice where unrelated whistles do not:

- One tone goes to each of the first two resonances of the mouth, the strongest overtone
  below 1200 Hz and the strongest above; the other two are the strongest that remain.
- Each tone stays as loud beside the others as it is in the voice. Evening the loudness
  out, as one does for music, makes the words unintelligible.
- Overtones below 330 Hz are kept even though the actuators are weak there.

`score.cjs` checks a clip with a speech recogniser (Whisper tiny) on the tones as the
actuator would sound them. The actuators' response was measured with a microphone
(`protwo record`, `tools/voice/actuator-response.json`): what the controller plays matches
the track in pitch and timing, it is weak below 330 Hz and falls off above 1.4 kHz.

## Why talk to ViGEmBus directly

The usual route is the ViGEmClient C library. The bus protocol is a handful of fixed-size
IOCTLs, so implementing them in about 150 lines removes a C build dependency and a DLL, and
keeps the executable self-contained.

## Why DirectInput by default

DirectInput mode is a virtual DualShock 4, XInput mode a virtual Xbox 360 controller. An
Xbox 360 pad cannot carry motion data. The DualShock 4 report has gyro and accelerometer
fields that SDL, Steam and emulators read natively, which makes gyro work with no extra
motion server. XInput mode remains for games that only speak XInput.

## Known gaps

- The battery is not read. Over USB the controller's voltage reading does not track its
  charge; games are told the virtual DualShock 4 is full and on its cable.
- One controller per process; the first match wins.
- The physical controller remains visible as a generic HID device beside the virtual pad.
