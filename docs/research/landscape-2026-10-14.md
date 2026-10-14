# Landscape: Bluetooth lamp control in open source (snapshot, 2026-10-14)

**Method:** GitHub search and the GitHub API (`gh search repos`, `gh api`), read-only. Star counts and push dates are
from that day. **Limits:** GitHub only (no GitLab, forums or closed source); star count is a weak proxy for quality;
the search tool missed some queries; no competitor footprint (size, memory) was measured. Treat conclusions as strong
evidence, not proof.

## What exists

| Area | Project | Notes |
|---|---|---|
| Broad BLE LED library | [led-ble](https://github.com/Bluetooth-Devices/led-ble) (Python, Apache-2.0, 15★) | Covers a family of generic controllers (`Triones:`, `LEDBLE-`, `LEDBlue-`, `QHM-`, `Dream~`). Its README states it "is not a universal driver for every Bluetooth LE light" and that Govee and others "should live in their own dedicated libraries". Built for Home Assistant. |
| Single-family clients | many, e.g. ELK-BLEDOM clients in Rust (62★), Go (45★), Kotlin (47★), JS, Python | Each covers one family in one language. |
| Govee | `Beshelmek/govee_ble_lights` (135★) and many forks | Home Assistant integrations. |
| iStrip lamps | [vakintosh/istrip_plus_HA](https://github.com/vakintosh/istrip_plus_HA), [gsp8181/ha-istrip-lamp-esphome](https://github.com/gsp8181/ha-istrip-lamp-esphome) | Protocol already documented publicly by both; our decode matched byte for byte. |
| Hubs | Home Assistant (Python), Homebridge and Matterbridge (Node) | Large runtimes; not embeddable. |
| Real-time lighting apps | [WLED](https://github.com/wled/WLED) (DDP, E1.31, Art-Net), [HyperHDR](https://github.com/awawa-dev/HyperHDR), [LedFx](https://github.com/LedFx/LedFx) | Network protocols and some LAN bulbs; no Bluetooth lamp support seen in their READMEs. |
| Pattern precedent | [Buttplug](https://github.com/buttplugio/buttplug) (Rust, 1.2k★) | Core libraries plus a versioned device-config JSON plus apps on top. Same architecture, different domain. |
| Bluetooth transport (Rust) | [btleplug](https://github.com/deviceplug/btleplug) (1.2k★, active), [bluest](https://github.com/alexmoon/bluest) | Reuse rather than rewrite. |
| Matter (Rust) | [rs-matter](https://github.com/project-chip/rs-matter) (595★, Apache-2.0, active) | Pure Rust, `no_std`; reported tested with Apple Home, Google, Alexa, Home Assistant. Has a `color_light` example. |
| HomeKit accessory (Rust) | [hap-rs](https://github.com/ewilken/hap-rs) (218★), last real commit April 2023 | Frozen; two 2026 projects vendor patched copies. A native HAP crate was considered and dropped in favour of Matter. |

## What we did not find

An embeddable, runtime-agnostic Rust core for Bluetooth lamps with data-driven device definitions and thin adapters
for Matter, MCP, HTTP and standard lighting protocols. This is the gap this project aims at; whether it is a *useful*
gap depends on how many lamps get covered.

## Open questions

- How much do users of cheap Bluetooth lamps care about footprint versus "it works in Home Assistant"?
- Do HyperHDR/LedFx users want Bluetooth lamps enough to justify a DDP/E1.31 sink?
- Legal comfort around shipping per-lamp keys: protocol facts for interoperability are fine; vendor binaries are not redistributed.
