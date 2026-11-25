#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

expected='wasm-bindgen 0.2.130'
if [[ "$(wasm-bindgen --version)" != "$expected" ]]; then
    printf 'Expected %s\n' "$expected" >&2
    exit 1
fi
cargo build --release --locked --target wasm32-unknown-unknown -p nexdesk-browser-core
wasm-bindgen --target web --out-dir apps/web/public/core --out-name nexdesk_core \
    "${CARGO_TARGET_DIR:-target}/wasm32-unknown-unknown/release/nexdesk_browser_core.wasm"
cd apps/web
npm ci --no-audit --no-fund
npm run build
