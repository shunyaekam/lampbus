# Control language

**Status: draft, unstable.** Implemented in [`crates/lampbus/src/control.rs`](../crates/lampbus/src/control.rs).

One small JSON object describes any change to a lamp. The command line, the HTTP API (`POST /api/control`) and the MCP
tool `lamp_control` all speak it, so a lamp behaves identically however it is driven, and a command recorded from one
can be replayed through another.

```json
{"device": "desk",
 "color": "#ff8800",
 "brightness": 40,
 "power": true}
```

| Field | Meaning |
|---|---|
| `device` | Lamp alias or id. Omit to address every lamp. |
| `preset` | Name of a preset from the library. |
| `color` | A colour (see the [animation format](animation-format.md#colours)). Switches the lamp on. |
| `hue`, `saturation` | `0-360` and `0-100`; an alternative to `color`. |
| `effect` | A built-in effect by name, alias (`rainbow`) or id, from the lamp's definition. |
| `animation` | A library name, or an inline [animation](animation-format.md). |
| `brightness` | `0-100`. Alone it only changes brightness; with `color`, `effect` or `animation` it sets that too. |
| `speed` | `0-100`, for effects. |
| `stop_animation` | `true` stops the animation and keeps the lamp's static colour. |
| `power` | `true`, `false` or `"toggle"`. Applied last, so `{"color": "red", "power": false}` ends off. |
| `timer` | `{"on": "07:00", "off": "23:00"}` (either may be left out) or `"clear"`. Programs the lamp's **own** daily clock, so it keeps working with the computer off; only for lamps whose definition has a `timer` command. With `lampbus serve` running, the same change is applied to lampbus's own idea of the lamp at that time (the lamp reports nothing when its timer fires). On: the lamp's firmware default (full white) then lampbus sets white. May be given alone. |

Rules, deliberately strict so that an agent's typo is reported and never guessed at:

- Give **at most one** of `color`/`hue`, `effect`, `animation`, `preset`.
- Unknown fields are errors, and the error lists the known ones.
- At least one field other than `device` is required.
- Values outside their range are clamped; wrong types are errors.
- With several lamps and no `device`, a lamp that cannot do something (for example an effect on a lamp with none) is skipped silently; with one `device` it is an error.

The effect of a command is reported as the lamp's state: `power`, colour, brightness, `source`
(`off`, `color`, `effect`, `animation`, `stream`), connection `link` and the last error.

## Live sources

Screen- and music-sync programs push colours at up to about 40 per second over a WebSocket (`/api/stream?device=`),
as a text frame `{"rgb": [r, g, b], "brightness": 0-100}` or a 3-4 byte binary frame `r g b [brightness]`. A stream
overrides everything else and ends 2 seconds after its last frame, when the lamp returns to what it was doing.
Frames are smoothed (fast attack, slow release) and dithered; identical frames are not re-sent.
