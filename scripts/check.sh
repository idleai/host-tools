#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
bash scripts/lint.sh
cargo build --workspace --locked
cargo build --locked --target wasm32-unknown-unknown -p idle-protocol -p idle-history -p idle-peer-state
npm --prefix packages/history-runtime test
if [ -z "${IDLE_CODEX_EXPORTER:-}" ]; then
    cargo build --manifest-path ../codex/tools/codex-session-exporter/Cargo.toml --locked
    node scripts/smoke-collector.mjs
else
    node scripts/smoke-collector.mjs "$IDLE_CODEX_EXPORTER"
fi
