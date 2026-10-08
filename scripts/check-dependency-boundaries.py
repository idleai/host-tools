#!/usr/bin/env python3
"""Keep application and UI packages out of the resolved host dependency graph."""

import json
from pathlib import Path
import subprocess


UI_PACKAGES = {"app-core", "app-core-bindings", "web-ui", "history-geometry"}
UI_FAMILIES = ("dioxus", "egui", "eframe", "iced", "leptos", "yew", "tauri")
UI_REPOSITORIES = ("app-core", "web-ui")


def forbidden(package):
    name = package["name"]
    source = package.get("source") or ""
    repository = (package.get("repository") or "").removesuffix(".git").rstrip("/")
    return (
        name in UI_PACKAGES
        or any(name == family or name.startswith((family + "-", family + "_"))
               for family in UI_FAMILIES)
        or any(repository == f"https://github.com/idleai/{owner}"
               or f"/idleai/{owner}/" in source for owner in UI_REPOSITORIES)
    )


def main():
    root = Path(__file__).resolve().parent.parent
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--all-features", "--locked"],
        cwd=root, text=True,
    ))
    blocked = sorted({package["name"] for package in metadata["packages"] if forbidden(package)})
    if blocked:
        raise SystemExit("Host dependencies include application or UI packages: " + ", ".join(blocked))
    print(f"Checked {len(metadata['packages'])} resolved packages: no application or UI dependencies.")


if __name__ == "__main__":
    main()
