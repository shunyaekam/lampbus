# lampbus: agent and contributor instructions

Read first. Every rule here is derived in [docs/PRINCIPLES.md](docs/PRINCIPLES.md); if a rule seems wrong, argue with
the derivation, not this file. Evidence tags: **[DERIVED]** follows from stated math, **[MEASURED]** observed with a
named method, **[ASSUMPTION]** believed, not measured (never report one as fact), **[OWNER]** a decision, not derivable.

## What this is

A small, embeddable core for controlling Bluetooth lamps, with open formats to describe lamps and drive them from a
CLI, an HTTP API, or an agent over MCP. Lamps are data (JSON definitions + test vectors); the core does no I/O; adapters
are thin. Goal: the cleanest, smallest, cheapest-to-run code that does the job, measured, not claimed.

**Status (2026-10-14): pre-alpha.** Works on the owner's iStrip lamp. Only that definition is hardware-verified; ELK-BLEDOM,
Triones and Govee are source-verified. The Matter adapter (`apps/matter`) advertises and starts but is not yet confirmed to pair with Apple Home. Apps (`apps/screen`, `apps/music`), the web UI, UDP/DDP input and schedules exist but are only unit-tested and smoke-tested, not used in anger.

## Rules (each derived in PRINCIPLES.md)

1. **P1 Bound by the narrowest link.** The lamp accepts about 31 bits per update at about 40 updates/s (1.2 kbit/s). Reduce
   data where it originates; never ship more than the lamp can use.
2. **P2 Send functions, not samples.** Animations are declarative data expanded next to the radio, never streams of samples.
3. **P3 Commands are absolute.** Never add a relative or toggle command. Re-sending is always safe; reconnect forgets state.
4. **P4 The core is pure.** No clocks, randomness, threads, files or sockets in `lampbus-defs`, `-render` or `-core`.
   Time arrives as an input. Concurrency and platform code live in shells.
5. **P5 Priority is a total order with exact fallback.** stream > animation > effect > colour; power-off dominates.
   A source that stops leaves no residue.
6. **P6 Respect perception.** Mix in linear light; dither only animations and streams; do not guess a lamp's response curve.
7. **P7 Lamps are data; vectors come from independent oracles.** Never generate a vector from the code under test.
   A suite must be shown to fail when a fault is seeded.
8. **P8 Fail closed.** Unknown fields, out-of-limit specs and ambiguous commands are errors that name the valid options.
   Nothing supplied by a user or an agent is ever executed.
9. **P9 Claims are measurements.** A number needs a method, a date and a machine. Update [docs/SCORECARD.md](docs/SCORECARD.md)
   in the same commit that changes a measured quantity. Say so when a target is missed (the binary is 1.10 MB vs 500 KB).
10. **P10 Reuse before build; own what you ship.** Build only what the best existing option fails on a scorecard criterion.

## Owner directives (not derived)

- **Rust only**, tools and tests included. No Python or other languages in the tree.
- **Commits are the owner's.** Use the git identity already configured; never set a repo-local identity; no `Co-Authored-By`
  trailers. Do not rewrite published history except with `--force-with-lease` against the expected SHA, and only when asked.
- **The repository is public.** Never commit secrets or personal data (tokens, pairing codes, LAN addresses, home paths).
  Before pushing history changes, scan it: `git log -p --all --format="" | grep -E "^\+" | grep -n -i -E "/Users/|10\.0\.0\.|192\.168\.|ghp_|PRIVATE KEY"`.
- **No vendor artefacts** (decompiled apps, firmware, APKs). Protocol facts only, with sources cited in the definition.
- **Outward-facing actions need explicit approval:** issues or pull requests on other projects, publishing crates, announcements.
- **Do not plan around upstream maintainers** accepting changes; prefer owning or forking.

## Verify before saying done

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings      # denies unwrap/expect/panic/indexing in non-test code
cargo test --workspace                                      # 84 tests as of 2026-10-14
cargo run -q -p lampbus-oracles -- --check                  # committed vectors match the oracles
cargo build -p lampbus --no-default-features                # optional adapters stay optional
cd fuzz && cargo +nightly fuzz run core_sequences -- -max_total_time=60   # after touching the core
```
Linux needs `libdbus-1-dev` and `pkg-config`. CI runs all of this on Linux and macOS. If you fix a bug in the core, add a
simulation test in `crates/lampbus-core/tests/sim.rs` that fails without the fix (the four bugs found so far were found there).

## The model on one screen

```
 CLI · HTTP/WS · MCP · (Matter next)   all speak ONE control language (spec/control-language.md)
                 │
            lampbus-core     Mealy machine: (commands, link events, time) → (actions, next wake-up)
        ┌────────┴─────────┐
 lampbus-render        lampbus-defs        render: colour, animations, dither, smooth
 (pure)                (pure)              defs: lamp definitions → exact bytes
                 │
            lampbus-shell    btleplug + tokio. The only crate that touches Bluetooth.
```
Invariants every action trace must satisfy (checked by `Sim::drain`): **I1** never write while disconnected; **I2** never
connect while connected or connecting; **I3** writes to one lamp are at least 25 ms apart.

## How to add things

- **A lamp:** write `definitions/<id>.json` per [spec/definition-format.md](spec/definition-format.md), cite sources, start
  `unverified`. Add vectors from an independent oracle in `tools/oracles`. Embed it in `crates/lampbus/src/assets.rs` (a test
  fails if you forget). Users can also drop a definition in their `definitions_dir` with no rebuild.
- **An animation or preset:** JSON in `library/`, embedded in `assets.rs`. Animations must pass `lampbus_render::anim::load`.
- **An adapter (Matter, MQTT, ...):** translate to and from the control language and the core's `Snapshot`. Add no new
  semantics; if it needs one, it belongs in the core and the spec first.
- **A shell (phone, browser, MCU):** implement the core's `Action`s and report `LinkEvent`s. Do not fork core logic.

## Numbers to use

Link: 30.7 bits per update, 40 updates/s ceiling (25 ms pacing), 20/s is the working rate **[MEASURED by eye, owner's lamp]**.
Defaults: idle drop 120 s, backoff 1 s doubling to 15 s, stream timeout 2 s, connect timeout 30 s, scan timeout 15 s,
stream smoothing 30 ms attack and 150 ms release. Limits: animation 86,400 s, 500 keys, 40 fps; frame 64 bytes.
Measured (macOS, 2026-10-14): one-shot command 0.65-0.8 s from cold; daemon 12 MB RSS; binary 1.10 MB.

## Where things are

| Path | What |
|---|---|
| `crates/` | `lampbus` (binary), `-core`, `-render`, `-defs`, `-shell` |
| `definitions/` | lamp definitions + conformance vectors |
| `library/` | starter animations and presets |
| `tools/oracles` | independent vector generators |
| `spec/` | the three open formats (definitions, animations, control language) |
| `docs/` | PRINCIPLES (derivations), SCORECARD (measured targets), DECISIONS (dated, with reasons), research |

## When unsure

Do not invent protocol facts: a byte layout, a range, a UUID. Cite a source, run it on hardware, or ask. Open assumptions
(brightness 0-100 vs 0-64, the lamp's response curve, flicker fusion, a "join group" command on some firmware) are listed
in [docs/PRINCIPLES.md](docs/PRINCIPLES.md#open-assumptions-do-not-report-these-as-facts) with how to settle each.
