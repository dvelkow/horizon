#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
toolchain="${HORIZON_WEB_TOOLCHAIN:-nightly-2026-10-17}"
if [[ "$(wasm-bindgen --version)" != "wasm-bindgen 0.2.129" ]]; then
  echo 'Install wasm-bindgen-cli 0.2.129 to match Cargo.lock.' >&2
  exit 1
fi
export RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals -C link-arg=--shared-memory -C link-arg=--import-memory -C link-arg=--max-memory=4294967296"
cargo +"$toolchain" rustc --locked -Z build-std=std,panic_abort --profile web -p horizon-client --bin horizon-client --target wasm32-unknown-unknown -- -C link-arg=--export=__wasm_init_tls -C link-arg=--export=__tls_size -C link-arg=--export=__tls_align -C link-arg=--export=__tls_base -C link-arg=--export=__heap_base
wasm-bindgen target/wasm32-unknown-unknown/web/horizon-client.wasm --target web --out-dir .local/web/pkg --out-name horizon_client
cp web/{index.html,style.css,app.js,bootstrap.js,rust-worker.js} .local/web/
