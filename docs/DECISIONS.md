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

## Open

- Core API shape (event/action interface): next. The definition-format spec is drafted (D11).
- Matter spike on `rs-matter`: commission a color light to Apple Home, check hue/saturation/brightness.
- Agent surface (MCP): depends on how the target agents call tools.
- Whether one repository holds everything or the spec gets its own.
