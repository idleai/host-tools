#!/usr/bin/env bash
# Source this once at a build entrypoint; child checks share the selected releases.
if [[ -z "${IDLE_RELEASE_INPUTS:-}" ]]; then
    python3 scripts/release_dependencies.py resolve
    export IDLE_RELEASE_INPUTS="$PWD/target/released-dependencies.json"
else
    python3 scripts/release_dependencies.py verify
fi
