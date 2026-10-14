# Animation format

**Status: draft, unstable.** Implemented by [`lampbus-render`](../crates/lampbus-render).

An animation is a JSON object that maps time to a colour and an intensity. It is **data, not code**: nothing in it is
ever executed, and every animation is validated against hard limits before it is accepted, so an agent or a stranger
can write one safely. Examples: [`library/animations/`](../library/animations).

An animation is sampled by the host at its `fps` (default 20, at most 40). The sample is a colour and a `level`
(`0..1`) that scales the lamp's master brightness. Between samples nothing is sent; a lamp that cannot show a level
exactly gets it approximated by temporal dithering (see the render crate).

## Common fields

| Field | Meaning |
|---|---|
| `type` | `"keyframes"` or `"gen"`. |
| `fps` | `0.1`..`40`, default `20`. Slow animations (a 30-minute sunrise) should use a low `fps`. |
| `then` | When a non-looping animation ends: `"hold"` (default, keeps the last colour *and* its intensity) or `"off"`. |
| `name`, `description`, `title` | Free text for people and UIs. |

## `keyframes`

```json
{"type": "keyframes", "duration": 1800, "loop": false, "space": "linear", "then": "hold", "fps": 2,
 "keys": [{"t": 0,    "color": "#ff1000", "level": 1,   "ease": "smooth"},
          {"t": 1800, "color": "#ffe0b0", "level": 100}]}
```

- `duration`: seconds, `0 < d <= 86400`.
- `loop`: wrap around at `duration`. If the last key is before `duration`, an implicit return to the first key fills the gap.
- `space`: how colours are mixed. `"linear"` (default) mixes in linear light, which is physically right and avoids muddy
  gradients; `"srgb"` (alias `"rgb"`) mixes the encoded values; `"hsv"` mixes hue, saturation and value the short way round the hue wheel.
- `keys`: 2 to 500. Each has `t` (seconds, within `0..=duration`), `color`, `level` (`0..100`, default 100) and `ease`
  (`"linear"`, `"smooth"` for smoothstep, or `"hold"` to stay until the next key). `ease` applies to the segment that starts at that key.
- Outside the keys, the first or last key is held.

## `gen`

```json
{"type": "gen", "gen": "candle", "params": {"color": "candle", "flicker": 35}}
```

Optional `duration` makes it finite (then it follows `then`); without it, it runs forever.

| `gen` | `params` (defaults) | Result |
|---|---|---|
| `rainbow` | `period` s (30), `saturation` % (100) | Full hue cycle. |
| `breathe` | `color` (warm), `period` s (6), `min` % (15), `max` % (100) | Sinusoidal intensity swell. |
| `candle` | `color` (candle), `flicker` % (35) | Smooth pseudo-random flicker; a dimmer flame drifts redder. |
| `aurora` | `palette` 2-12 colours, `speed` (0.15) | Drifts through the palette with slow noise. |

## Colours

A colour is a name (`red orange amber yellow green cyan blue purple magenta pink white warm candle cool`),
`"#rrggbb"`, `[r, g, b]` (`0..255`) or `{"h": 0-360, "s": 0-100, "v": 0-100}`.

## Limits (enforced)

`duration <= 86400 s`, `keys <= 500`, `fps <= 40`, `palette <= 12`. Unknown `type`, `gen`, `ease`, `space` or `then`
values are errors. Nothing is evaluated beyond arithmetic on these fields.
