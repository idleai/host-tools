#!/usr/bin/env python3
"""Fast-forward a generated dependency update after CI checks its current commit."""

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tomllib

BRANCH = "deps/released-artifacts"
LOCKS = {"Cargo.lock", "native-dependencies.json", "consumer-dependencies.json"}


def command(*arguments):
    return subprocess.check_output(arguments, text=True).strip()


def api(repository, path, payload=None):
    arguments = ["gh", "api", f"repos/{repository}/{path}"]
    if payload is not None:
        arguments += ["--method", "PATCH", "--input", "-"]
    result = subprocess.run(arguments, text=True, capture_output=True, check=True,
                            input=None if payload is None else json.dumps(payload))
    return json.loads(result.stdout)


def updater():
    spec = importlib.util.spec_from_file_location("update_releases", Path(__file__).with_name("update-releases.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def validate_artifacts(before, after, native):
    resolver = updater()
    if before.keys() != after.keys() or before["schema"] != after["schema"]:
        raise ValueError("dependency document structure changed")
    if before["dependencies"].keys() != after["dependencies"].keys():
        raise ValueError("dependency names changed")
    for name, old in before["dependencies"].items():
        new = after["dependencies"][name]
        identity = lambda item: {key: value for key, value in item.items() if key not in {"tag", "sha256"}}
        if identity(old) != identity(new):
            raise ValueError(f"producer identity changed: {name}")
        prefix, current = resolver.parse_tag(old["tag"])
        new_prefix, candidate = resolver.parse_tag(new["tag"])
        if prefix != new_prefix or not resolver.compatible(current, candidate):
            raise ValueError(f"incompatible artifact update: {name}")
        if old["tag"] == new["tag"] and old != new:
            raise ValueError(f"immutable artifact checksum changed: {name}")
        # Resolve the selected release again: do not trust checksums supplied by a PR.
        if new != old and resolver.latest(new, native, exact=True) != new:
            raise ValueError(f"release checksums changed: {name}")


def validate_cargo(before, after):
    resolver = updater()
    def internal(lock):
        packages = [package for package in lock["package"]
                    if "raw.githubusercontent.com/idleai/" in package.get("source", "")]
        result = {package["name"]: package for package in packages}
        if len(result) != len(packages):
            raise ValueError("multiple versions of an internal package require review")
        return result
    old_packages, new_packages = internal(before), internal(after)
    changed = False
    for name, old in old_packages.items():
        new = new_packages.get(name)
        if new is None:
            continue  # An internal dependency may no longer need a transitive package.
        old_version = resolver.parse_tag(f"{name}-v{old['version']}")[1]
        new_version = resolver.parse_tag(f"{name}-v{new['version']}")[1]
        if old["source"] != new["source"] or not resolver.compatible(old_version, new_version):
            raise ValueError(f"incompatible Cargo package update: {name}")
        if old_version == new_version and old.get("checksum") != new.get("checksum"):
            raise ValueError(f"immutable Cargo checksum changed: {name}")
        changed |= old_version != new_version
    if before != after and not changed:
        raise ValueError("Cargo update has no compatible internal package upgrade")


def eligible(run, pull, repository):
    return (run["conclusion"] == "success" and run["event"] == "workflow_dispatch"
            and run["head_branch"] == BRANCH and run["head_repository"]["full_name"] == repository
            and pull["state"] == "open" and not pull["draft"]
            and pull["user"]["login"] == "github-actions[bot]"
            and pull["head"]["repo"]["full_name"] == repository
            and pull["head"]["ref"] == BRANCH and pull["base"]["ref"] == "main"
            and pull["head"]["sha"] == run["head_sha"])


def main():
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    repository = os.environ["GITHUB_REPOSITORY"]
    run = api(repository, f"actions/runs/{event['workflow_run']['id']}")
    pulls = api(repository, f"pulls?state=open&base=main&head={repository.split('/')[0]}:{BRANCH}")
    if len(pulls) != 1 or not eligible(run, pulls[0], repository):
        print("No current generated update has successful CI")
        return
    pull = pulls[0]
    head = run["head_sha"]
    base = api(repository, "git/ref/heads/main")["object"]["sha"]
    subprocess.run(["git", "fetch", "origin", base, head], check=True)
    if subprocess.run(["git", "merge-base", "--is-ancestor", base, head], check=False).returncode:
        print("Main advanced; the scheduled updater will refresh and retest this PR")
        return
    paths = command("git", "diff", "--name-only", base, head).splitlines()
    if not paths or not set(paths) <= LOCKS:
        raise ValueError(f"unexpected dependency PR files: {paths}")
    for path in paths:
        before = command("git", "show", f"{base}:{path}")
        after = command("git", "show", f"{head}:{path}")
        if path == "Cargo.lock":
            validate_cargo(tomllib.loads(before), tomllib.loads(after))
        else:
            validate_artifacts(json.loads(before), json.loads(after), path.startswith("native-"))
    # Recheck the PR after inspecting its files; a newer bot update must run CI again.
    if not eligible(run, api(repository, f"pulls/{pull['number']}"), repository):
        print("The dependency PR changed during validation")
        return
    # GitHub rejects a non-fast-forward if main advances during validation. This also
    # preserves the tested tree and commit, and honors repository branch restrictions.
    api(repository, "git/refs/heads/main", {"sha": head, "force": False})
    # Token-generated main updates do not trigger push workflows automatically.
    subprocess.run(["gh", "workflow", "run", "ci.yml", "--ref", "main"], check=True)
    print(f"Merged dependency PR #{pull['number']} at tested commit {head}")


if __name__ == "__main__":
    main()
