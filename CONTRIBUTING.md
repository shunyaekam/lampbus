# Contributing

Start with [AGENTS.md](AGENTS.md) (rules for humans and AI agents alike) and [docs/PRINCIPLES.md](docs/PRINCIPLES.md)
(why things are the way they are). The short version: small, measured, no surprises.

- **Adding a lamp is the most valuable contribution and needs no Rust.** See [docs/ADD-A-LAMP.md](docs/ADD-A-LAMP.md).
- Bug reports: include the output of `lampbus status --json` and `lampbus gatt <name>`; say what you saw versus what
  you expected. For lamp behaviour, say what the lamp physically did.
- Code: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` must pass
  (CI runs the same on Linux and macOS). No `unsafe`, no `unwrap`/`expect`/`panic` outside tests.
- A claim needs evidence: a measured number with the command that produced it ([docs/SCORECARD.md](docs/SCORECARD.md)), a
  test, or a note of what was observed on which lamp. A decision needs a line in [docs/DECISIONS.md](docs/DECISIONS.md).
- Never commit vendor apps, decompiled output, tokens or pairing data.
- Commits should carry your own name and email. Rebase onto `main`; do not merge `main` into your branch.
- Licence: contributions are under Apache-2.0 or MIT, at the user's option, like the rest.
