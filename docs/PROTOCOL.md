# Protocol notes

What ProTwo knows about the Switch 2 Pro Controller over USB. Everything here comes from
public reverse-engineering work (see [Sources](#sources)). The handshake, flash reads,
input report fields and rumble report were exercised against a retail controller; fields
marked unverified or unknown were not.

## USB layout

Device `057E:2069`, a composite device with three interfaces:

| Interface | Class | Windows driver | Purpose |
| --- | --- | --- | --- |
| 0 | HID | `HidUsb` | Input reports in, rumble reports out |
| 1 | Vendor, bulk IN `0x82` / OUT `0x02` | `WinUSB` (automatic) | Command channel |
| 2 | Audio | `usbaudio` | Headset jack |

The firmware carries Microsoft OS descriptors, so Windows loads WinUSB for interface 1
without an INF. Its device interface class is `{6F13725E-EF0E-4FD3-AE5F-B2DE989EC825}`;
ProTwo uses that GUID both to find the controller and to get plug-in notifications.

Until the handshake below is sent on interface 1, interface 0 sends nothing.

## Commands (interface 1)

Every command is an 8-byte header followed by a payload:

| Byte | Meaning |
| --- | --- |
| 0 | Command |
| 1 | `0x91` = host to controller |
| 2 | Transport: `0x00` USB, `0x01` Bluetooth |
| 3 | Sub-command |
| 4 | `0x00` |
| 5 | Payload length |
| 6–7 | `0x00` |

The controller answers each command with one reply on the bulk IN endpoint.

Known commands:

| Cmd | Sub | Payload | Purpose |
| --- | --- | --- | --- |
| `0x02` | `0x01` | 4 zero bytes, then address (u32 LE) | Read a `0x40`-byte flash block. Data starts at reply byte `0x10`. |
| `0x03` | `0x0A` | `05 00 00 00` | Select input report format `0x05` |
| `0x03` | `0x0D` | `01 00` + 6-byte host address | Start streaming over USB |
| `0x08` | `0x02` | `01 00 00 00` | Enable the GL/GR grip buttons |
| `0x09` | `0x07` | LED bitmask + 7 zero bytes | Set player LEDs |
| `0x0A` | `0x08` | 20 bytes (see source) | Rumble configuration |
| `0x0C` | `0x02` | feature mask (u32 LE) | Select features |
| `0x0C` | `0x04` | feature mask (u32 LE) | Enable features |

Feature mask bits: `0x01` buttons, `0x02` sticks, `0x04` IMU, `0x10` mouse (Joy-Con),
`0x20` rumble, `0x80` magnetometer. ProTwo enables `0x27`.

### Handshake

ProTwo first reads calibration from flash, then sends, in order:

1. `07/01` (purpose unknown)
2. `0C/02` select features `0x27`
3. `11/01` (purpose unknown)
4. `0A/08` rumble configuration
5. `0C/04` enable features `0x27`
6. `01/0C`, `01/01` (purpose unknown)
7. `08/02` enable grip buttons
8. `03/0A` select report `0x05`
9. `03/0D` start streaming
10. `09/07` light player LED 1

The exact bytes live in `init_sequence()` in [src/protocol.rs](../src/protocol.rs).

### Flash map

| Address | Content |
| --- | --- |
| `0x13000` | Serial number: 16 ASCII bytes at offset 2 |
| `0x13040` | Gyro bias: three f32 (rad/s) at offsets 4, 8, 12 |
| `0x13080` | Left stick factory calibration: 9 bytes at offset `0x28` |
| `0x130C0` | Right stick factory calibration: 9 bytes at offset `0x28` |
| `0x13100` | Accelerometer bias: three f32 at offsets 12, 16, 20 (not used by ProTwo) |
| `0x1FC040` | Left stick user calibration: magic `B2 A1`, then 9 bytes |
| `0x1FC080` | Right stick user calibration: magic `B2 A1`, then 9 bytes |

Stick calibration is three packed pairs of 12-bit values: neutral (x, y), then the
distance from neutral to the positive end (x, y), then to the negative end (x, y).
Each pair packs as `x = b0 | (b1 & 0x0F) << 8`, `y = (b1 >> 4) | b2 << 4`.

## Input report `0x05` (interface 0)

64 bytes including the report ID, sent every 4 ms (250 Hz measured).

| Offset | Size | Content |
| --- | --- | --- |
| `0x00` | 1 | Report ID `0x05` |
| `0x01` | 4 | Packet counter |
| `0x05` | 4 | Buttons (see below) |
| `0x0B` | 3 | Left stick, packed 12-bit x/y, uncalibrated |
| `0x0E` | 3 | Right stick, packed 12-bit x/y, uncalibrated |
| `0x20` | 2 | Battery voltage in mV (ProTwo does not use the battery fields) |
| `0x22` | 1 | Charge state: `0x20` when fully charged, climbs and settles near `0x34` while charging. The voltage is not a usable gauge on USB: a full battery reads about 3.70 V |
| `0x23` | 2 | Battery current (unverified) |
| `0x2B` | 4 | IMU timestamp in microseconds |
| `0x2F` | 2 | Temperature |
| `0x31` | 6 | Accelerometer x, y, z (i16 LE, 4096 per g) |
| `0x37` | 6 | Gyroscope x, y, z (i16 LE, full scale about 2000 °/s) |

Buttons, as a little-endian u32 of bytes `0x05..0x09`:

| Bit | Button | Bit | Button | Bit | Button |
| --- | --- | --- | --- | --- | --- |
| 0 | Y | 8 | − | 16 | D-pad down |
| 1 | X | 9 | + | 17 | D-pad up |
| 2 | B | 10 | Right stick click | 18 | D-pad right |
| 3 | A | 11 | Left stick click | 19 | D-pad left |
| 6 | R | 12 | Home | 22 | L |
| 7 | ZR | 13 | Capture | 23 | ZL |
| | | 14 | C | 24 | GR |
| | | | | 25 | GL |

ZL and ZR are digital; the Pro Controller has no analog triggers.

### Motion axes

With the controller lying flat, the sensor reports gravity on its **z** axis. ProTwo converts
to the frame used by SDL and the DualShock 4 (x right, y up, z toward the player):

```
out = (x, z, -y)
```

The same rotation applies to the gyroscope, after subtracting the flash bias.

SDL notes that some units report IMU timestamps on a different time base and need a gyro
scale of 40 rad/s full scale instead of 34.8. ProTwo currently assumes the common case.

## Rumble output report `0x02` (interface 0)

64 bytes. Two identical 16-byte slots, one per actuator (left at `0x01`, right at `0x11`):

| Slot offset | Content |
| --- | --- |
| 0 | `0x50 | (sequence & 0x0F)` |
| 1–5 | One HD rumble sample |

A sample packs a high band and a low band, each with a 10-bit frequency code and a 10-bit
amplitude:

```
b0 = hi_freq & 0xFF
b1 = (hi_amp >> 4 & 0xFC) | (hi_freq >> 8 & 0x03)
b2 = (hi_amp >> 12) | (lo_freq << 4)
b3 = (lo_amp & 0xC0) | (lo_freq >> 4 & 0x3F)
b4 = lo_amp >> 8
```

(amplitudes given as 16-bit values). The sample must be re-sent while vibration should
continue; ProTwo sends it every 12 ms and sends one zero-amplitude report to stop.

Defaults from SDL: `hi_freq = 0x187`, `lo_freq = 0x112`, amplitude capped at 29000 of 65535
to protect the actuators.

### Frequency codes

The 10-bit frequency code is logarithmic, 96 steps per octave starting at 10 Hz:

```
code = round(log2(hertz / 10) * 96)        hertz = 10 * 2^(code / 96)
```

So the SDL defaults are about 168 Hz (high band) and 72 Hz (low band), and A4 (440 Hz) is
code 524. This scale comes from community measurements reported in
[SDL pull request 16680](https://github.com/libsdl-org/SDL/pull/16680); it is not official.
ProTwo's own check, measuring the vibration with the controller's accelerometer, matched at
45 Hz and 80 Hz and was inconclusive at 30, 60 and 100 Hz (the accelerometer is only
sampled at 250 Hz, and the controller rattles on the table at some frequencies).

Because the two bands have independent frequencies and the two actuator slots are
independent, the controller can play four notes at once. That is how ProTwo speaks
(see `src/voice.rs`).

Each 16-byte slot has room for three 5-byte samples after its first byte, which suggests a
report can carry several samples for finer timing. ProTwo sends one sample per slot, like
SDL; the multi-sample form is untested.

## Sources

- SDL, `src/joystick/hidapi/SDL_hidapi_switch2.c` (zlib license): handshake, report
  parsing, calibration and rumble encoding
- [ndeadly/switch2_controller_research](https://github.com/ndeadly/switch2_controller_research): report layouts, battery fields
- Linux `hid-switch2` driver (Valve): command and feature names
- [SDL pull request 16680](https://github.com/libsdl-org/SDL/pull/16680): rumble frequency scale
- [HandHeldLegend procon2tool](https://handheldlegend.github.io/procon2tool/): WebUSB handshake
