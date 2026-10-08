#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# Idle Rust lint suite — the single canonical "done" check.
# ---------------------------------------------------------------------------
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"
source scripts/released-dependencies.sh

PASS=true
SUMMARY=""

check() {
    local name="$1"
    shift
    echo "==> $name"
    if "$@" 2>&1; then
        echo "  ✓ $name"
        SUMMARY+="  ✓ $name"$'\n'
    else
        echo "  ✗ $name — FAILED"
        SUMMARY+="  ✗ $name — FAILED"$'\n'
        PASS=false
    fi
    echo
}

# ---------------------------------------------------------------------------
# Tool detection
# ---------------------------------------------------------------------------
MISSING=()
command -v cargo-deny >/dev/null 2>&1 || MISSING+=("cargo-deny (install: cargo install cargo-deny --locked --version 0.20.2)")

if [ ${#MISSING[@]} -gt 0 ]; then
    echo "Missing required tools:"
    for tool in "${MISSING[@]}"; do
        echo "  - $tool"
    done
    exit 1
fi

# ---------------------------------------------------------------------------
# Formatting
# ---------------------------------------------------------------------------
check "release automation" python3 -m unittest discover -s scripts -p "test*release*.py"

check "cargo fmt" cargo fmt --all -- --check

# ---------------------------------------------------------------------------
# Build check (all targets, all features, locked)
# ---------------------------------------------------------------------------
check "cargo check" cargo check --workspace --all-targets --all-features --locked
check "dependency boundaries" python3 scripts/check-dependency-boundaries.py

# ---------------------------------------------------------------------------
# Clippy (deny warnings)
# ---------------------------------------------------------------------------
check "cargo clippy" cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------
check "engine integration tools" python3 scripts/install-artifacts.py engine
check "TypeScript interoperability tools" npm --prefix packages/history-runtime run build
check "cargo test" cargo test --workspace --all-features --locked

# ---------------------------------------------------------------------------
# Doc tests
# ---------------------------------------------------------------------------
check "cargo test (doc)" cargo test --workspace --all-features --doc --locked

# ---------------------------------------------------------------------------
# Rustdoc lint checks
# ---------------------------------------------------------------------------
# Build documentation so the Rustdoc lint table is exercised beyond doctests.
check "cargo doc" cargo doc --workspace --all-features --no-deps --locked

# Portable libraries must satisfy the same policy on WASM.
check "cargo check (wasm32)" cargo check -p idle-protocol -p idle-history -p idle-history-graph --lib --all-features --locked --target wasm32-unknown-unknown
check "cargo clippy (wasm32)" cargo clippy -p idle-protocol -p idle-history -p idle-history-graph --lib --all-features --locked --target wasm32-unknown-unknown -- -D warnings

# ---------------------------------------------------------------------------
# Dependency policy
# ---------------------------------------------------------------------------
check "cargo deny" cargo deny check

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo "=========================================="
echo "  Lint suite results"
echo "=========================================="
echo "$SUMMARY"

if $PASS; then
    echo "  RESULT: PASS"
    exit 0
else
    echo "  RESULT: FAIL"
    exit 1
fi
