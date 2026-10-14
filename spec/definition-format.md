# Lamp definition format, `lampbus.device/0`

**Status: draft, unstable.** Breaking changes are allowed until the schema string reaches `/1`.

A *definition* is one JSON file that tells a host how to recognise a Bluetooth lamp, where to write, and how every
command is laid out as bytes. A definition contains **no code**: it is a small, total, deterministic template language,
so it can be validated, fuzzed, diffed, and written by a person or an agent.

Every definition ships with a sibling **vectors** file (`<id>.vectors.json`): exact bytes each command must produce.
Vectors are the conformance contract. Anyone can check a definition without owning the lamp.

The reference implementation is [`crates/lampbus-defs`](../crates/lampbus-defs). Where this text and that crate
disagree, that is a bug; please open an issue.

## Files

```
definitions/<id>.json            the definition
definitions/<id>.vectors.json    conformance vectors
```

## Top-level fields

| Field | Required | Meaning |
|---|---|---|
| `schema` | yes | Must be `"lampbus.device/0"`. |
| `id` | yes | Lowercase letters, digits, `-`; max 40. Must equal the file name. |
| `name` | yes | Display name. |
| `status` | no | `hardware-verified` (run on a real lamp by the maintainers), `source-verified` (frames cross-checked against independent open implementations), or `unverified` (default). |
| `match` | yes | How to recognise the lamp: `name_prefix` (advertised name starts with any) and/or `service_uuids` (advertised services). At least one entry overall. |
| `gatt` | yes | `service`, `write` (characteristic that takes commands), `mode`: `with-response` or `without-response`. UUIDs are lowercase 128-bit. |
| `roles` | no | Maps standard roles to command names (see below). |
| `effects` | no | Built-in effects: `[{"id": 1, "name": "Fade7"}]`. |
| `commands` | yes | Command name to command (see below). Names: lowercase letters, digits, `_`. |
| `wire` | no | Transforms applied, in order, to each frame just before writing. |
| `sources` | no | Where the protocol knowledge came from. Cite it. |

Unknown fields are errors, so typos never pass silently.

## Commands

```json
"color": {
  "params": { "r": {}, "g": {}, "b": {}, "level": {"min": 0, "max": 100, "default": 100} },
  "frame": ["0x7e", 0, 5, 3, "$r", "$g", "$b", 0, "0xef"],
  "pad_to": 16,
  "checksum": {"kind": "xor"}
}
```

**`frame`** has one element per output byte:

- a number `0..255`, or a hex string `"0x7e"`: a literal byte;
- `"$name"`: a parameter;
- `{"scale": {"param": "level", "from": [0, 100], "to": [0, 255]}}`: a parameter remapped linearly.

**`params`** declares parameters: `min` and `max` (default `0` and `255`, and always within `0..=255`) and optional
`default`. A parameter used in the frame but not declared is required and spans `0..=255`. A parameter with no
`default` is required.

**Encoding rules** (exactly, in this order):

1. Reject unknown argument names, and non-finite values (NaN, infinity).
2. Resolve each parameter: the given value, else `default`, else error. Clamp to `[min, max]`.
3. Round to the nearest integer (halves away from zero), then emit as a byte. `scale` maps first, then rounds.
4. If `pad_to` is set, zero-pad to that length (it may not be shorter than the frame).
5. If `checksum` is set, append one byte computed over every byte so far: `xor` (XOR of all) or `sum8` (sum mod 256).
6. Apply `wire` transforms.

The final length of a command is fixed by its definition and may not exceed 64 bytes.

## Wire transforms

| `op` | Fields | Meaning |
|---|---|---|
| `aes-128-ecb` | `key`: 32 hex chars | Encrypt whole 16-byte blocks. Every command's length must be a multiple of 16 (use `pad_to`). |

Keys in definitions are protocol facts needed for interoperability with the owner's own hardware.

## Roles

Roles let hosts (Matter, MCP, UIs) use any lamp without knowing its command names. A role maps to a command.

| Role | Meaning |
|---|---|
| `on`, `off` | Power. A lamp with only `off` is turned on by sending a colour or effect. |
| `color_rgb` | Command with parameters `r`, `g`, `b`. |
| `brightness` | Command with a brightness parameter (see its `min`/`max`). |
| `effect`, `effect_speed` | Built-in effects (`effects` lists the ids). |

More roles will be added as more lamps are described; roles a host does not know are ignored.

## Vectors

```json
[ { "command": "color", "args": {"r": 255, "g": 0, "b": 128}, "plain": "7e000503ff008000ef" },
  { "command": "color", "args": {"r": 255, "g": 0, "b": 0},   "plain": "…", "wire": "…" } ]
```

`plain` is the frame before `wire` transforms; `wire` is what is written and may be omitted when it equals `plain`.
**Vectors must come from an independent source** (another project's code, a capture from the real device, or the
vendor's own app), never from the interpreter under test. `tools/oracles` shows the pattern: each oracle
re-implements the upstream frame construction literally.

## Adding a lamp

1. Capture or find the protocol (Bluetooth HCI log, decompiled vendor app, existing open implementation). Note sources.
2. Write `definitions/<id>.json`. Start with `status: "unverified"`.
3. Generate vectors from an independent oracle and save `definitions/<id>.vectors.json`.
4. `cargo test -p lampbus-defs`. Every command needs at least one vector.
5. If you ran it on the real lamp, say so in the pull request; the maintainers will set `status`.

Do not include vendor binaries, firmware or decompiled source. Protocol facts only.
