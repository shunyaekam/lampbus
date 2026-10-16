# iStrip / SSL lamp protocol

What the vendor app (iStrip+ 1.4.3) can do over Bluetooth Low Energy, what each part is, and whether it has been
checked on a real lamp (an SSL-E089ED, 2026-10-14 and 2026-10-15). Everything the app can do is listed here; the
machine-readable form is [`definitions/istrip-ssl.json`](../../definitions/istrip-ssl.json), with test vectors.

## Link

- GATT service `0000ac50-1212-efde-1523-785fedbeda25`: `ac52` write-without-response (commands), `ac51` notify
  (replies), `ac53` write-without-response (never used by the app).
- Service `0000ae00-0000-1000-8000-00805f9b34fb` (`ae01` write, `ae02` notify): firmware update. Never written by lampbus.
- One connection at a time. Advertised name `SSL-…`.
- Every frame is 16 bytes: `54 52 00 57 | cmd | group | 10 argument bytes`, encrypted with AES-128-ECB,
  key `34522a5b7a6e492c08090a9d8d2a23f8`.

## Commands

| cmd | Meaning | Arguments | Status |
|---|---|---|---|
| 1 | assign group | `group` | echoed back on `ac51` (the only reply the lamp ever sends); the lamp does **not** filter direct commands by group (tested) |
| 2 | colour, effect and off | `mode` (0 colour, 1-12 effect), `r g b`, `brightness` 0-100, `speed` 1-100, `type` | verified. Off = mode 0 with r=g=b=0. `type` (1 white, 2 colour temperature in the app) has **no effect** on this RGB-only strip |
| 3 | rhythm (music) | `status`, `fft`, `sensitivity` | decoded from the app, not tried: the phone does the audio analysis and streams a level byte |
| 4 | the lamp's own timer | `flags` = 16 (on) + 32 (off) + today's weekday 1-7, then current `hour minute second`, then `on_week on_hour on_minute off_week off_hour off_minute` | **verified**: with the week byte 127 the lamp turns itself on or off every day at that time with nothing else connected. The lamp then shows its firmware default (full white), and reports nothing |
| 5 | LED line order | `order` 0-5 | **verified**: the same red showed red, green, blue, red, blue for orders 0-4 (order 5 not seen), i.e. the six channel permutations. Order 0 is the normal one |
| 6 | speed | `value` 1-100 | verified |
| 7 | brightness | `level` 0-100 | verified; 0-100 is the real range |

The twelve effects: Fade7, Fade3, Breath R, Strobe R, Breath7, Breath3, Breath B, Strobe B, Flash7, Flash3, Breath G, Strobe G.

## What is not here

- **Advertising broadcast.** The app can also steer lamps without connecting, by advertising a 16-byte payload
  (`[random, product0..2, 0xC3, frame, mode, r|light, g|speed, b, group, fftStatus, light|sensitivity, random×3]`;
  `0xC5` is the timer, another code the line order), encoded by a native library. Groups matter there. Not reproduced:
  macOS cannot advertise arbitrary data, so it needs Linux/BlueZ or a phone.
- **State read-back, connected: none.** Over the connection the lamp answers only command 1. After its own timer
  fires, or after the vendor app changes it, nothing tells us. lampbus therefore sends every explicit command in
  full (DECISIONS D22) and mirrors the timers it programmed itself (D23).
- **State read-back, broadcast: it exists, and is not reproduced.** The app decodes advertisements that lamps send
  back (manufacturer data, 16 bytes after a native cipher `WirelessEncoder.cipher`, bytes 1-3 = product `54 00 1d`,
  13-15 = the app's random code): byte 0 mode, 5 brightness, 6-8 device id (or the line order in a line-order
  reply), 9 and 12 vendor ids, 10 group, 11 bit 0 = on. The idle advertisement is a different, shorter packet
  (`5254: 00 57 00 00 53`, service `af30`), identical for red and for dim blue (2026-10-15), so it carries no
  state. Reproducing the cipher, and a radio that can advertise freely, would give the lamp an observable state.
- **The LED hardware.** Three channels. No white LEDs. A coating on this lamp tints white purple, which
  `white_balance` cancels in the colour pipeline.
- **`Agreement.getEnterDiyCommand`** (`06 'S' 'M' 'V' 'E' 'W' 01…`) exists in the app and is never called.
