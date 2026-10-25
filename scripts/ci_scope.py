#!/usr/bin/env python3
"""Select auxiliary CI checks while retaining the main Rust and WASM suite."""

import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent.parent
BROWSERS = ("history", "history-details", "history-explorer", "sessions", "projections", "navigation")


def select(paths, *, full=False, root=ROOT):
    common = full or any(
        path in {"Cargo.lock", "rust-toolchain.toml", ".github/released-dependencies.lock.json",
                 "scripts/ci_scope.py", "scripts/check.sh", "scripts/lint.sh",
                 "scripts/released-dependencies.sh", "scripts/release_dependencies.py",
                 "scripts/install-artifacts.py"}
        or path.endswith("Cargo.toml")
        or path.startswith((".cargo/", ".github/workflows/"))
        for path in paths
    )
    windows = common or any(
        path.startswith(("scripts/release", "scripts/test_release"))
        or path in {"native-dependencies.json", "consumer-dependencies.json",
                    "scripts/install-artifacts.py", "scripts/check-consumers.py", "scripts/ci_scope.py"}
        for path in paths
    )
    native = common or any(
        path.startswith(("crates/app-core-bindings/", "crates/app-core/src/", "scripts/swift-smoke/"))
        or path in {"scripts/build-bindings.sh", "scripts/smoke-kotlin.sh",
                    "crates/app-core/src/bin/codegen.rs", "scripts/ci_scope.py"}
        for path in paths
    )
    consumers = common or any(
        path.startswith("crates/app-core/")
        or path in {"scripts/check-consumers.py", "scripts/check-consumers.sh", "consumer-dependencies.json"}
        for path in paths
    )
    browsers = set()
    for path in paths:
        if (path.startswith(("crates/web-ui/src/", "crates/history-geometry/"))
                or path in {"package.json", "package-lock.json", "scripts/check-browser-fixtures.py",
                            "scripts/ci_scope.py", "crates/web-ui/assets/theme.css"}):
            browsers.update(BROWSERS)
            continue
        for suite in BROWSERS:
            stem = suite.replace("-", "_")
            build = root / f"scripts/build-{suite}.sh"
            if (path == f"scripts/build-{suite}.sh"
                    or path == f"tests/{suite}.browser.mjs"
                    or path.startswith(f"crates/web-ui/examples/{stem}.")
                    or path.startswith(f"crates/web-ui/examples/{stem}/")
                    or (path.startswith("crates/web-ui/assets/")
                        and build.exists() and path in build.read_text())):
                browsers.add(suite)
    if common:
        browsers.update(BROWSERS)
    return {"windows_dependencies": windows, "native_bindings": native,
            "consumers": consumers, "browser": [suite for suite in BROWSERS if suite in browsers]}


def main():
    base = os.environ.get("BASE_SHA", "")
    full = os.environ.get("FULL_CI") == "true" or not base or set(base) == {"0"}
    paths = []
    if not full:
        output = subprocess.check_output(
            ["git", "diff", "--name-only", "--no-renames", "-z", base,
             os.environ.get("HEAD_SHA", "HEAD")], cwd=ROOT)
        paths = [os.fsdecode(path) for path in output.split(b"\0") if path]
    plan = select(paths, full=full)
    print(json.dumps(plan))
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        for key, value in plan.items():
            output.write(f"{key}={json.dumps(value, separators=(',', ':'))}\n")


if __name__ == "__main__":
    main()
