#!/bin/sh
# Rebuilds solve.js (with its embedded WebAssembly module) from the Rust source in rust/.
# Needs cargo with the wasm32-unknown-unknown target; no crates beyond std.
set -e
cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release --target wasm32-unknown-unknown --manifest-path rust/Cargo.toml
node gen-solve.mjs
