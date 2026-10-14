# Scorecard: what "industry class" means here

"Best" is only a claim if it can be measured. These are the criteria, the target for each, how it is measured,
and the current baseline from the prototypes. Anything marked *unmeasured* is a promise, not a result.

| # | Criterion | Target | How measured | Baseline today |
|---|---|---|---|---|
| 1 | Core code size | model + definitions interpreter + renderer under ~100 KB of code on a Cortex-M (`thumbv7em`) | `cargo size` on the core crates, release, `opt-level=z`, LTO | *unmeasured* |
| 2 | Daemon binary | stripped release binary under 500 KB excluding platform Bluetooth frameworks | `ls -l` on the release binary | prototype 1.10 MB, includes an HTTP+WebSocket API |
| 3 | Idle memory | under a few MB beyond the platform's Bluetooth libraries | RSS after 5 min idle | Rust prototype 9 MB idle, 12 MB after a test run on macOS (includes Apple frameworks); Python prototype 32-50 MB with HomeKit |
| 4 | CPU | 0% idle, under 1% while streaming 40 updates/s | `ps` / `time` over a fixed scenario | Rust prototype 0.25 s CPU over ~60 s of animations on macOS |
| 5 | Latency | command to Bluetooth write under 5 ms in-process | timestamps in the test harness | *unmeasured* |
| 6 | Cold connect | first command reaches the lamp quickly | wall time from command to first write, cold and warm | Rust prototype ~14 s cold, Python ~7 s cold (scan-bound, to be improved) |
| 7 | Dependencies | tiny audited tree; core model has none | `cargo tree`, `cargo-deny` clean | *unmeasured* |
| 8 | Safety | `#![forbid(unsafe_code)]` in core crates, no `unwrap` outside tests, strict clippy | CI | prototype not yet enforced |
| 9 | Portability | one CI matrix builds Linux, macOS, iOS, Android, `wasm32`, Cortex-M | CI | *unmeasured* |
| 10 | Breadth | at least 4 lamp families expressed purely as data, each with test vectors | definition files + vector tests | 1 family (as code) |
| 11 | Correctness | fuzzing on every parser that touches input; deterministic golden tests for rendering | `cargo fuzz`, golden files in CI | 6 unit tests in the Rust prototype |

## Rules of measurement

- Numbers are published with the command that produced them and the machine they came from.
- A target is only "met" when CI can reproduce it.
- Competitors are compared on the same criteria only where they can be measured fairly; otherwise we say so.
