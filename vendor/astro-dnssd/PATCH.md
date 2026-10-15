# Vendored `astro-dnssd` 0.3.6

Upstream: https://github.com/AstroHQ/astro-dnssd (MIT OR Apache-2.0, see the license files here).
Source: the 0.3.6 release published on crates.io, unmodified except for the change below.

## Change

`src/os/apple/register.rs`, `run_thread_with_poll`: the poll timeout is **10,000 ms instead of 100 ms**.

Why: the loop wakes only to check a shutdown flag. At 100 ms that is 10 wake-ups per second per registered service on an
otherwise idle process (measured on macOS with `top -stats idlew`: about 10 per second for `lampbus-matter`). The cost
is that a requested shutdown is noticed up to 10 s later; the adapter is a long-lived service that exits by signal,
so that latency is irrelevant.

Remove this directory and the `[patch.crates-io]` entry in the root `Cargo.toml` if upstream adopts a wake-on-shutdown design.
