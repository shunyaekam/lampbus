# lampbus

A small, fast, embeddable core for controlling Bluetooth lamps, plus open formats to describe lamps and drive them from
anything: a command line, an HTTP API, or an agent over MCP.

> **Status: pre-alpha.** It works on the author's lamp and the formats are drafts. Expect breaking changes.

```console
$ lampbus status
SSL-E089ED [istrip-ssl] off  link=down
$ lampbus color warm 40
SSL-E089ED [istrip-ssl] on  40%  rgb(255,147,41)  color  link=ready
$ lampbus effect rainbow
$ lampbus preset night-light
$ lampbus off
```

With no configuration it finds the first lamp it understands. One command takes about a second, including scanning,
connecting and writing. Run `lampbus serve` once and animations, live colour streams and instant commands work:

```console
$ lampbus serve &            # keeps the Bluetooth link, runs animations
$ lampbus anim candle 70     # commands now go to the daemon automatically
```

## For agents

`lampbus mcp` is an [MCP](https://modelcontextprotocol.io) server on stdio. The agent's host starts it on demand, and it
talks to Bluetooth directly, so **no daemon or network service is needed**. Tools: `lamp_status`, `lamp_control`,
`lamp_library`, `lamp_validate_animation`, `lamp_save_animation`. Agents can write their own animations; they are
validated against hard limits and never execute code.

```json
{"mcpServers": {"lampbus": {"command": "/path/to/lampbus", "args": ["mcp"]}}}
```

## How it is built

```
   CLI · HTTP/WebSocket · MCP · (Matter next)      ← adapters, all speaking one control language
                       │
                  lampbus-core    sans-I/O state machine: commands + link events + time in, actions out
                       │          connect on demand · hold while animating · idle drop · backoff · newest wins
        lampbus-render │ lampbus-defs
   colour math, animations,        lamp definitions as data (JSON) with test vectors
   dithering, smoothing
                       │
                lampbus-shell     btleplug + tokio (desktop). Other shells (phone, browser, MCU) can drive the same core.
```

- **Lamps are data.** A [definition file](spec/definition-format.md) says how to recognise a lamp and how its commands
  are laid out. Four families are included (iStrip, ELK-BLEDOM, Triones, Govee H61xx), each with test vectors from
  independent sources. Add a lamp by adding a JSON file, with no recompiling: put it in the `definitions_dir` of your config.
- **The core does no I/O**, so it is deterministic and testable with a virtual clock. The simulation suite enforces
  protocol invariants on every action (never write while disconnected, never double-connect, always pace writes).
- **Open formats:** [definitions](spec/definition-format.md), [animations](spec/animation-format.md),
  [the control language](spec/control-language.md).

**Installing on an always-on machine** (also written for AI agents): [docs/INSTALL.md](docs/INSTALL.md).

**Contributors and agents:** start with [AGENTS.md](AGENTS.md), then [docs/PRINCIPLES.md](docs/PRINCIPLES.md) for why.

Measured numbers against stated targets are in [docs/SCORECARD.md](docs/SCORECARD.md). Decisions and their reasons
are in [docs/DECISIONS.md](docs/DECISIONS.md); what else exists is in [docs/research](docs/research/landscape-2026-10-14.md).

## Configuration

Optional: `~/.config/lampbus/config.json`.

```json
{"devices": [{"alias": "desk", "name": "SSL-E089ED"}],
 "idle_seconds": 120,
 "bind": "127.0.0.1:8787",
 "definitions_dir": "~/.config/lampbus/definitions",
 "library_dir": "~/.config/lampbus/library",
 "color_gamma": 1.0, "level_curve": "linear", "dither": false}
```

The API token is created on first use in `~/.config/lampbus/token` (mode 0600). Bind to a Tailscale address to reach
the daemon from your other devices; the API is plain HTTP and relies on that network for encryption.

## Build

```console
$ cargo build --release -p lampbus      # ./target/release/lampbus, about 1.1 MB
$ cargo test --workspace
```

Needs Rust 1.80+. On Linux you also need `libdbus-1-dev` and `pkg-config` (for BlueZ).

## Layout

| Path | What |
|---|---|
| `crates/lampbus` | the binary: CLI, HTTP API, MCP server |
| `crates/lampbus-core` | the sans-I/O controller |
| `crates/lampbus-render` | colour math, animation format, output pipeline |
| `crates/lampbus-defs` | lamp definition parser and encoder |
| `crates/lampbus-shell` | desktop Bluetooth shell |
| `definitions/` | lamp definitions + conformance vectors |
| `library/` | starter animations and presets |
| `apps/` | `lampbus-matter` (Apple Home, Google Home, Alexa, Home Assistant via Matter), `lampbus-screen` (screen ambient light, macOS), `lampbus-music` (music reactive) |
| `deploy/` | systemd and launchd units and an installer for an always-on machine |
| `fuzz/` | cargo-fuzz targets (nightly) |
| `tools/oracles` | generates the conformance vectors from independent implementations |
| `spec/`, `docs/` | formats, scorecard, decisions, research |

## Compatibility and legal

An independent project, not affiliated with any lamp or app vendor. Protocol knowledge is gathered for interoperability
with hardware the author owns. Vendor apps and firmware are never redistributed here.

## License

[Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
