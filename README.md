# lampbus

A small, fast, embeddable core for controlling Bluetooth lamps, plus the open formats to describe lamps and
drive them from anything: Matter, MCP (agents), HTTP, or your own app.

> **Status: pre-alpha.** Nothing here is stable. Working today: the definitions core (`crates/lampbus-defs`), which
> reproduces four lamp families from data alone, and two prototypes under `prototypes/` that drive one real lamp.
> The rest of the architecture below is the plan, not the current state.

## Why

Cheap Bluetooth LED lamps are controlled by short-lived vendor apps. Open-source support exists, but it is
fragmented: one library per lamp family, in many languages, mostly tied to a specific runtime (Home Assistant, Node,
ESPHome). See [docs/research](docs/research/landscape-2026-10-14.md) for what we found and how, including its limits.

## Goals

1. **A lamp is data, not code.** A versioned definition file says how to recognise a lamp, which characteristics to
   use and how commands are laid out. Each definition ships with test vectors, so it can be tested without hardware
   and written by a person or an agent.
2. **A core that does no I/O.** It consumes events (a command, time passing, bytes received) and emits actions
   ("write these bytes"). The same core can then run on a server, in a phone app, in a browser, on a microcontroller,
   or in-process for an agent, with the Bluetooth radio supplied by the host.
3. **Interfaces are thin adapters.** Matter, MCP, HTTP and DDP/E1.31 are optional crates over one core API.
4. **Measurably small and cheap.** Targets are in [docs/SCORECARD.md](docs/SCORECARD.md) and are published as numbers.
5. **Reuse the best existing parts** (`btleplug`, `rs-matter`, existing real-time lighting apps via DDP) instead of rewriting them.

## Layout

| Path | What |
|---|---|
| `spec/` | the lamp definition format (draft) |
| `definitions/` | lamp definitions as data, each with conformance vectors: iStrip, ELK-BLEDOM, Triones, Govee H61xx |
| `crates/lampbus-defs/` | Rust crate: parse, validate and encode definitions (`unsafe` forbidden, 10 tests, 54 vectors) |
| `tools/` | vector generator (independent oracles), size measurement |
| `docs/` | scorecard, decisions, research |
| `prototypes/python/` | first working prototype: service, HomeKit bridge, API, CLI |
| `prototypes/lampd-rs/` | Rust port of the prototype (1.1 MB release binary), the seed of the future core |
| `library/` | example animations and presets (JSON) |

## Compatibility and legal

This is an independent project and is not affiliated with any lamp or app vendor. Protocol knowledge is gathered for
interoperability with hardware the author owns. Vendor apps and firmware are never redistributed here.

## License

Dual-licensed under [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
