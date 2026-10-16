# Decisions

Short, dated, with the reason. A decision can be reversed; write a new entry instead of editing an old one.

## 2026-10-14

- **D1. Rust.** Memory safety on network-facing parsers, one static binary, cross-compiles to phones, WebAssembly and
  microcontrollers, and the ecosystem we want to reuse (btleplug, rs-matter) is Rust.
- **D2. Sans-I/O core, thin shells.** The core consumes events and emits actions; Bluetooth, timers and networking are
  supplied by the host (desktop, phone, browser, MCU, agent process). The daemon is one shell, not the product.
- **D3. Lamps are data.** Versioned definition files plus test vectors; a small deterministic interpreter, no code
  execution. Escape hatch for odd lamps: a compiled driver.
- **D4. Reuse before building.** btleplug for desktop Bluetooth; rs-matter for Matter; Buttplug's device-config
  pattern as design precedent; existing real-time apps (HyperHDR, LedFx) reached through a DDP/E1.31 adapter instead
  of writing new screen/music software, unless theirs falls short.
- **D5. Matter, not a native HomeKit crate.** A HAP crate would have a small audience and a security-critical
  maintenance burden; `rs-matter` is active and reaches Apple Home plus others. Revisit only if Matter proves unusable.
- **D6. Own the code.** Prefer owning or forking over depending on upstream maintainers accepting changes; contribute
  upstream only to actively maintained projects.
- **D7. Licence: Apache-2.0 OR MIT.**
- **D8. Private until deliberately published.** The repository starts private. Nothing is announced or sent to other
  projects without an explicit decision.
- **D9. No vendor material in the repository.** Decompiled apps and firmware stay outside; only protocol facts needed
  for interoperability are recorded, with vectors that anyone can reproduce.
- **D10. Measure, don't claim.** Efficiency targets live in [SCORECARD.md](SCORECARD.md) as numbers with methods.

- **D11. Definition format v0 is JSON with a tiny template language** (literals, parameters, linear scale, zero-pad,
  xor/sum8 checksum, AES-128-ECB wire transform). No expressions, no code. Chosen because the four lamp families
  checked so far (iStrip, ELK-BLEDOM, Triones, Govee) all fit it; revisit when one does not. See
  [spec/definition-format.md](../spec/definition-format.md).
- **D12. Vectors must come from independent oracles**, never from the interpreter under test.

- **D13. One language in the repository: Rust.** The first prototype was written in Python; it was removed once the
  oracles and the daemon existed in Rust (it remains in git history). Vector oracles stay independent of the code
  under test by being separate hand-written implementations, not by being written in another language.
- **D14. Public.** The founder decided on 2026-10-14 to publish the repository (supersedes D8). Nothing is announced
  or sent to other projects without a separate decision.
- **D15. One control language, many front ends.** The CLI, HTTP API and MCP server all speak one strict JSON object
  ([spec/control-language.md](../spec/control-language.md)), so behaviour cannot drift between them and agent mistakes are
  reported, not guessed at. Unknown fields are errors.
- **D16. Agents need no daemon.** `lampbus mcp` talks to Bluetooth directly for the life of the session. The HTTP daemon
  exists for always-on use (animations that outlive a command, live streams, a phone or home hub), and the CLI uses it
  automatically when it is running.
- **D17. Defaults come from the simulator, not guesses.** Behaviour that is easy to get subtly wrong (idle drop,
  backoff, newest-state-wins, holding the last frame of an animation) is pinned by deterministic simulation tests.
- **D18. Matter is an adapter process, not core code.** `apps/matter` runs rs-matter (pinned to an exact git commit,
  because its latest release predates the colour-light support) and forwards On/Off, Level and Colour commands to a
  running `lampbus serve` through the control language, so it adds no semantics (D15). It uses rs-matter's test
  vendor identity (setup code 3497-011-2332), so Apple Home warns that it is uncertified. Commands are ignored until
  the Matter stack has finished replaying its stored defaults, which would otherwise switch the lamp on every restart.
- **D19. Vendor a dependency when we need a change and upstream is not the place for it.** `vendor/astro-dnssd` is a
  copy of an MIT/Apache crate with one documented change (a 10 s poll instead of 100 ms), recorded in its `PATCH.md`,
  excluded from the workspace and wired in with `[patch.crates-io]`. Remove it if upstream adopts a wake-on-shutdown design.
- **D20. Dependencies are audited in CI.** `cargo deny check` enforces permissive licences, no known vulnerabilities, only
  crates.io plus the one pinned rs-matter commit, and flags unmaintained crates. An acknowledged advisory needs a reason in `deny.toml`.
- **D21. The lamp's own effects are the right tool for continuous fades.** The stock app has no software fading: its
  brightness slider sends the same single command we do, and its smooth fades are firmware effects (Fade, Breath),
  which we expose (`effect`). Host-driven animations remain for what the lamp cannot do (30-minute sunrise, candle,
  aurora, live sync) and are bounded by the lamp's brightness resolution. Dithering is off by default (D-evidence in P6).
- **D22. Explicit commands are never skipped on the strength of belief.** The core used to send nothing when a command
  matched what it believed the lamp showed. The lamp changes on its own (its timers, the vendor app, a power cut), so
  a believed-off lamp that was actually on stayed on after `lampbus off` (found live, 2026-10-15, after the lamp's own
  on-timer fired). Power, colour, brightness and effect commands now forget the belief about their own aspect and
  are always sent; animations and streams keep the diffing. Cost: one extra frame per repeated command.
- **D23. The lamp has two clocks we can use.** Command 4 programs the lamp's own on/off times (verified 2026-10-15:
  off at a clock time, and on, which turns the lamp on at full brightness in the firmware's default white, ignoring
  `white_balance`). The lamp reports nothing when they fire, so lampbus does not know; D22 limits the damage.
- **D24. The over-the-air frame layer is a separate, dependency-free, `no_std` crate.** `lampbus-air` is the vendor's
  broadcast encoder reimplemented and verified against the real native library (run under CPU emulation, outside the
  repository; the library itself is never redistributed): the same bytes on 300 randomised trials and in the committed
  golden vectors. It is the piece that lets a phone, an ESP32 or a Linux host drive a lamp with no connection, and
  that decodes the lamp's own status advertisements. Not yet proven: that a lamp acts on frames we advertise.

## Open

- Matter: confirm pairing and colour/brightness control from Apple Home on a real iPhone; Google Home, Alexa and Home Assistant untested.
- Agent surface (MCP): depends on how the target agents call tools.
- Whether one repository holds everything or the spec gets its own.
