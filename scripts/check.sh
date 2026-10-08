#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/released-dependencies.sh
bash scripts/lint.sh
cargo build --workspace --locked
cargo build --locked --target wasm32-unknown-unknown -p idle-protocol -p idle-history -p idle-history-graph
npm --prefix packages/history-runtime test
python3 scripts/check-tunnels.py
python3 scripts/install-artifacts.py codex-exporter
node scripts/smoke-collector.mjs
