# WalkingPad BLE protocol — verified notes

Verified live on 2026-11-07 against this unit:

| Field | Value |
|---|---|
| BLE name | `WalkingPad` (advertises service `FE00`, manufacturer id 0x0060 data `57 4c 4e 2a 0c 2c`) |
| Model (GATT 2A24) | `WLT8266M` |
| Firmware (GATT 2A26) | `M30_V187.2.0` |
| Device info (`a5` query) | starts `4d 41 31` = "MA1" → WalkingPad **A1** family, max speed byte 0x3c |
| BLE chip | Telink (service `00010203-…-1912` is Telink **OTA** — never write to it) |

Protocol family: KingSmith "WiLink" (legacy, non-FTMS). Same as ph4-walkingpad targets.

## Transport

- Service `0000fe00-…`; notify on `0000fe01-…` (not readable), write-without-response on `0000fe02-…`.
- Command frame: `F7 <type> <payload…> <crc> FD`; reply frame: `F8 <type> … <crc> FD`.
- `crc = sum(frame[1:-2]) & 0xFF` (verified; matches qdomyos-zwift frames too).
- Every command gets an immediate notification reply (~0.1 s). Polling status at 1 Hz is fine.
- Only one central at a time: KS Fit on a phone will block the desktop app.

## Commands (type `A2` = control)

| Frame (before crc) | Meaning | Verified behaviour |
|---|---|---|
| `F7 A2 00 00` | ask status | replies with status frame |
| `F7 A2 01 <v>` | set target speed, v = km/h × 10 | belt ramps to v; status b14 becomes v×3 |
| `F7 A2 02 <m>` | mode: 0 auto, 1 manual, 2 standby | standby while running stops belt immediately |
| `F7 A2 04 01` | **start/stop toggle** (not just "start") | idle→3-2-1 countdown→run; running→ramp down & stop |
| `F7 A2 03 <x>` | ph4 `cmd_162_3_7` / "ask_beep" | just beeps; echoes x in b14 |
| `F7 A2 05..0A xx` | — | ignored, no beep |

## Status reply `F8 A2` (20 bytes)

| Byte | Meaning |
|---|---|
| 2 | belt state: 0 idle (manual), 1 running, 5 standby, 9/8/7 = start countdown 3/2/1 |
| 3 | current speed ×10 |
| 4 | mode (0 auto, 1 manual, 2 standby) |
| 5–7 | session time, s (big-endian) — **only advances while steps are detected** |
| 8–10 | distance, units of 10 m |
| 11–13 | steps |
| 14 | last command echo: target speed ×3 after a speed command, or the mode/arg value of other A2 commands |
| 16 | transient (e.g. 0x03 after start/stop toggle, 0x91 seen at auto-start) |

Counters reset to 0 when the belt stops — read the history record to keep them.

## Preferences (type `A6`)

Write: `F7 A6 <key> <stype> <v2> <v1> <v0> <crc> FD`. Every write replies with the full pref dump.
Dump request: key 0 (`F7 A6 00 00 00 00 00`).

Dump `F8 A6` layout: byte 2 = target type, 3–5 = target value, then key *k* lives at byte *k*+4:

| Key | Byte | Meaning | This unit |
|---|---|---|---|
| 1 | 2–5 | target (type + 24-bit value) | 0 |
| 2 | 6 | **undocumented in ph4**: belt auto-starts when switching to manual | 0 |
| 3 | 7 | max speed ×10 | 0x3c |
| 4 | 8 | start speed ×10 | 0x14 |
| 5 | 9 | intelligent start | 0 |
| 6 | 10 | auto-mode sensitivity (1 high … 3 low) | 2 |
| 7 | 11 | display mask (only valid combos accepted; 0x37/0x57 rejected) | 0x17 |
| 8 | 12 | units miles | 0 |
| 9 | 13 | child lock | 0 |
| 10–16 | — | not stored | — |

## History (type `A7`)

`F7 A7 AA FF` = last session (`AA 01`, `AA 02` … older). ph4's parse: time = bytes 8–10, dist = 11–13, steps = 14–16. Confirmed against a measured session (24 s, 11 steps).

## Auto-stop (nobody on belt)

- Measured: in manual mode, if no steps are detected for ~35 s, the pad ramps down and goes to idle (state 0), resetting counters.
- Does **not** reset the timer: resending the same speed, ±0.1 km/h changes, +0.2 km/h blips, and pref writes. Re-sending `A2 04 01` *stops* the belt (it's a toggle).
- **Does** reset it: a speed blip of ≥ +0.5 km/h for ~1.5 s. The load change registers ~2 phantom steps. A blip every 25 s kept an empty belt running indefinitely.
- No pref or command found that disables auto-stop. KS Fit's `setAutoStop` (per mcdax's decompilation) has no known wire format, and this 2019-era A1 firmware probably lacks it.
- Recommended app behaviour: an optional "keep running" toggle. If the step counter hasn't moved for ~20 s while running, blip +0.5 km/h for 1.5 s. Cost: 2 beeps per blip, and phantom steps (subtract them).

## Beeps

- 2.5 kHz piezo. Beeps on every **accepted state change**: mode change, speed change, start/stop, pref change, `A2 03`. Countdown beeps 3 times.
- No beep for: status/history/pref queries, a command that changes nothing (e.g. standby when already in standby), unknown commands.
- No mute command found (tested pref keys 2, 10–16, display-mask bits, `A2 03`/`05`–`0A`). KS Fit's `setSoundSwitch` targets newer FTMS models (supplement characteristic `24e2521c-…`), which this unit doesn't expose.
- What software *can* do: never send redundant commands, and debounce speed changes (one command per gesture, not per 0.1 step).
- Hardware option: the buzzer is behind the display panel. Users report tape over its hole softens it.

## ph4_walkingpad 1.0.2 audit

Correct: framing, CRC, characteristic UUIDs, status parse offsets, history parse, pref keys 1 and 3–9, mode values.

Bugs or misleading parts:
1. `Controller.send_cmd` throttle sleeps `min(elapsed, spacing)` instead of `spacing - elapsed`. It works by accident; with sub-second replies the 0.69 s spacing is unnecessary.
2. `notif_handler` sets `self.last_record = None` instead of the parsed record.
3. `WalkingPadCurStatus.__str__` prints `manual_mode` in the `button` slot.
4. `start_belt` is really a start/stop **toggle**. Calling it while running stops the belt (this is what your GUI's Start/Stop button relied on).
5. `controller_button` (byte 16) and `app_speed` (byte 14) are misnamed: byte 14 is a last-command echo (speed×3 after a speed command).
6. `Scanner` passes `service_uuids` incl. the OTA service; fine, but filtering on `FE00` alone is enough.
7. Missing: pref key 2, `A5` device-info query, explicit beep command semantics, and the auto-stop behaviour.

Sources: [ph4-walkingpad](https://github.com/ph4r05/ph4-walkingpad), [mcdax/walkingpad-controller](https://github.com/mcdax/walkingpad-controller) (KS Fit decompilation notes, [buzzer issue #5](https://github.com/mcdax/walkingpad-controller/issues/5)), [qdomyos-zwift kingsmithr1protreadmill.cpp](https://github.com/cagnulein/qdomyos-zwift).
