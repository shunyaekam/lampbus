#!/bin/sh
# Smallest release binary: a size-optimised standard library (nightly + rust-src). Used for the scorecard.
#   rustup toolchain install nightly && rustup component add rust-src --toolchain nightly
#   tools/build_small.sh [extra cargo args, e.g. --no-default-features]
set -eu
target=$(rustc -vV | sed -n 's/^host: //p')
CARGO_TARGET_DIR=target-size RUSTFLAGS="-Zlocation-detail=none -Zfmt-debug=none" \
  cargo +nightly build --release -p lampbus \
  -Z build-std=std,panic_abort -Z build-std-features=optimize_for_size --target "$target" "$@"
strip "target-size/$target/release/lampbus" 2>/dev/null || true
ls -l "target-size/$target/release/lampbus"
