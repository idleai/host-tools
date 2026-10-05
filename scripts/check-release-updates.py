#!/usr/bin/env python3
"""Start ordinary main CI when published dependencies change, without opening a PR."""

import json
import os
from pathlib import Path
import subprocess
import tempfile

import release_dependencies as dependencies


def api(resource):
    return json.loads(subprocess.check_output(["gh", "api", resource], text=True))


def main():
    root = Path(__file__).resolve().parent.parent
    repository = os.environ["GITHUB_REPOSITORY"]
    revision = dependencies.command(root, "git", "rev-parse", "HEAD")
    runs = api(f"repos/{repository}/actions/workflows/ci.yml/runs?branch=main&head_sha={revision}&per_page=20")["workflow_runs"]
    runs = [run for run in runs if run["event"] in ("push", "workflow_dispatch")]
    if any(run["status"] != "completed" for run in runs):
        print("Main CI is already checking the current source")
        return
    current = dependencies.resolve(root)
    for run in runs:
        artifacts = api(f"repos/{repository}/actions/runs/{run['id']}/artifacts")["artifacts"]
        if not any(item["name"] == "released-dependencies" and not item["expired"] for item in artifacts):
            continue
        with tempfile.TemporaryDirectory(prefix="idle-previous-dependencies-") as temporary:
            subprocess.run(["gh", "run", "download", str(run["id"]), "--repo", repository,
                            "--name", "released-dependencies", "--dir", temporary], check=True)
            previous = json.loads((Path(temporary) / "released-dependencies.json").read_text())
        if dependencies.fingerprint(previous) == dependencies.fingerprint(current):
            print(f"CI run {run['id']} already checked this dependency selection ({run['conclusion']})")
            return
        break
    subprocess.run(["gh", "workflow", "run", "ci.yml", "--repo", repository, "--ref", "main"], check=True)
    print("Started main CI with the latest compatible releases")


if __name__ == "__main__":
    main()
