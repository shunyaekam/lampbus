#!/bin/sh
# Release-size cost of the definitions core: a CLI using lampbus-defs vs. a std "hello" built with identical flags.
# Usage: tools/measure_size.sh   (from the repo root)
set -eu
FLAGS="-C opt-level=z -C lto=fat -C codegen-units=1 -C panic=abort -C strip=symbols"
T=$(mktemp -d)
cat > "$T/base.rs" <<'RS'
use std::process::ExitCode;
fn main() -> ExitCode {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(std::fs::read_to_string) { Some(Ok(t)) => { println!("{}", t.len()); ExitCode::SUCCESS } _ => ExitCode::FAILURE }
}
RS
rustc --edition 2021 $FLAGS -o "$T/base" "$T/base.rs"
cargo build --release -q -p lampbus-defs --example encode
CORE=target/release/examples/encode
B=$(wc -c < "$T/base"); C=$(wc -c < "$CORE")
echo "baseline std program : $B bytes"
echo "lampbus-defs CLI     : $C bytes"
echo "delta (core + serde_json + aes): $((C - B)) bytes"
rm -rf "$T"
