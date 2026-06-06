#!/usr/bin/env bash
# Build the browser (wasm) UI bundle that server mode embeds and serves.
#
# Produces crates/emberviewer/web-dist/{emberviewer.js, emberviewer_bg.wasm,
# index.html}. That folder is gitignored (build output) and embedded into the
# native binary at compile time via rust-embed, so a plain `cargo build` after
# this serves the real web UI instead of the build.rs placeholder.
#
# Requires the wasm target (`rustup target add wasm32-unknown-unknown`) and a
# `wasm-bindgen` CLI whose version matches the `wasm-bindgen` crate in Cargo.lock.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

dist="crates/emberviewer/web-dist"
wasm="target/wasm32-unknown-unknown/release/emberviewer.wasm"

echo "==> Building wasm lib (release)…"
cargo build --lib --release -p emberviewer --target wasm32-unknown-unknown

echo "==> Running wasm-bindgen ($(wasm-bindgen --version))…"
wasm-bindgen "$wasm" --out-dir "$dist" --target web --no-typescript

echo "==> Copying loader shell…"
cp crates/emberviewer/web/index.html "$dist/index.html"

echo "==> Web bundle ready in $dist/"
ls -la "$dist"
