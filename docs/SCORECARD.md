# Scorecard: what "industry class" means here

"Best" is only a claim if it can be measured. These are the criteria, the target for each, how it is measured,
and the current baseline from the prototypes. Anything marked *unmeasured* is a promise, not a result.

| # | Criterion | Target | How measured | Baseline today |
|---|---|---|---|---|
| 1 | Core code size | model + definitions interpreter + renderer under ~100 KB of code on a Cortex-M (`thumbv7em`) | `tools/measure_size.sh` (delta over a std baseline); `cargo size` on a MCU target once the core is `no_std` | definitions core (parse + encode + AES + `serde_json`), std build: **116 KB** delta over a bare std program (2026-10-14, macOS arm64). MCU build *unmeasured* (needs `no_std`). Renderer not yet ported. |
| 2 | Daemon binary | stripped release binary under 500 KB excluding platform Bluetooth frameworks | `ls -l` on the release binary | **1.10 MB** (macOS arm64, `opt-level=z`, LTO) with the CLI, HTTP+WebSocket API, MCP server, core, four lamp families and the starter library all built in. Target not met; the HTTP stack is the largest part (`--no-default-features` is smaller). |
| 3 | Idle memory | under a few MB beyond the platform's Bluetooth libraries | RSS after 5 min idle | `lampbus serve`: **12 MB** RSS on macOS after a session of commands and two animations (includes Apple's Bluetooth frameworks). An earlier Python prototype (since removed, see git history) used 32-50 MB with HomeKit. |
| 4 | CPU | 0% idle, under 1% while streaming 40 updates/s | `ps` / `time` over a fixed scenario | `lampbus serve`: 0.0% idle; 0.18 s CPU in total over a session with two animations (macOS). Streaming at 40/s not yet measured. |
| 5 | Latency | command to Bluetooth write under 5 ms in-process | timestamps in the test harness | *unmeasured* |
| 6 | Cold connect | first command reaches the lamp quickly | wall time from command to first write, cold and warm | **0.65-0.8 s** for a one-shot `lampbus color ...` from a cold process including zero-config scan, connect and write; 1.6-2.1 s for commands that need several frames (macOS, lamp about 1 m away, 5 samples). Earlier prototypes took 7-14 s. |
| 7 | Dependencies | tiny audited tree; core model has none | `cargo tree`, `cargo-deny` clean | `lampbus-defs`: 23 crates with AES, 13 without (several are compile-time only: `syn`, `quote`, `serde_derive`). `cargo-deny` not yet wired. |
| 8 | Safety | `#![forbid(unsafe_code)]` in core crates, no `unwrap` outside tests, strict clippy | CI | **met for `lampbus-defs`**: `unsafe_code` forbidden, `unwrap`/`expect`/`panic` denied, clippy `-D warnings` clean. Prototypes are not held to this. |
| 9 | Portability | one CI matrix builds Linux, macOS, iOS, Android, `wasm32`, Cortex-M | CI | Linux and macOS (full workspace, tests, release build) and `wasm32-unknown-unknown` for the three pure crates (`lampbus-defs`, `-render`, `-core`), all in CI. iOS, Android and Cortex-M not yet attempted (Cortex-M needs a `no_std` port). |
| 10 | Breadth | at least 4 lamp families expressed purely as data, each with test vectors | definition files + vector tests | **met**: 4 families (iStrip, ELK-BLEDOM, Triones, Govee H61xx), 54 vectors from independent oracles. Only iStrip is hardware-verified; the other three are source-verified. |
| 11 | Correctness | fuzzing on every parser that touches input; deterministic golden tests for rendering | `cargo fuzz`, golden files in CI | 65 tests across the workspace: 54 conformance vectors from independent oracles (mutation-checked), 28 core simulations on a virtual clock that enforce protocol invariants, animation and pipeline properties, the control language and CLI. **Fuzzing** (`fuzz/`, cargo-fuzz, nightly): 5 targets (definition parsing and encoding, animation loading and sampling, colour parsing, the output pipeline, and random command/time/link-event sequences through the core asserting invariants I1-I3). 40 s each on 2026-10-14 on macOS: about 31 million executions, 0 crashes. Not yet run in CI or for long. |

## Rules of measurement

- Numbers are published with the command that produced them and the machine they came from.
- A target is only "met" when CI can reproduce it.
- Competitors are compared on the same criteria only where they can be measured fairly; otherwise we say so.
