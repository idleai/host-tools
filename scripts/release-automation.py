#!/usr/bin/env python3
"""Prepare a version commit, or resume an unfinished artifact publication."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import tomllib

import release_dependencies


def dependency_inputs():
    root = Path.cwd()
    run_id = os.environ.get("DEPENDENCY_RUN_ID")
    if run_id:
        destination = release_dependencies.record_path(root).parent
        destination.mkdir(parents=True, exist_ok=True)
        subprocess.run(["gh", "run", "download", run_id, "--repo", os.environ["GITHUB_REPOSITORY"],
                        "--name", "released-dependencies", "--dir", str(destination)], check=True)
    if os.environ.get(release_dependencies.ENVIRONMENT):
        return release_dependencies.restore(root)
    return release_dependencies.resolve(root)


def record_dependencies(inputs):
    if inputs is not None:
        release_dependencies.capture(Path.cwd(), inputs["artifacts"])


def command(*arguments):
    return subprocess.check_output(arguments, text=True).strip()


def output(name, value):
    print(f"{name}={value}")
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as stream:
            stream.write(f"{name}={value}\n")


def metadata_only(paths):
    return all(Path(path).name in {"Cargo.toml", "Cargo.lock", "CHANGELOG.md"}
               and not path.startswith(".github/") for path in paths)


def packages():
    config = tomllib.loads(Path("release-plz.toml").read_text())
    excluded = {item["name"] for item in config.get("package", []) if item.get("release") is False}
    metadata = json.loads(command("cargo", "metadata", "--no-deps", "--format-version", "1"))
    return [package for package in metadata["packages"] if package["name"] not in excluded]


def tag_commit(tag):
    result = subprocess.run(["git", "rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}"],
                            text=True, capture_output=True, check=False)
    return result.stdout.strip() if result.returncode == 0 else None


def pending(package, head):
    tag = f"{package['name']}-v{package['version']}"
    revision = tag_commit(tag)
    if revision is None:
        return True
    if revision != head:
        return False
    result = subprocess.run(["gh", "release", "view", tag, "--json", "isDraft,assets"],
                            text=True, capture_output=True, check=False)
    if result.returncode and "release not found" in result.stderr:
        return True
    result.check_returncode()
    release = json.loads(result.stdout)
    return release["isDraft"] or not any(asset["name"] == "released-dependencies.json" for asset in release["assets"])


def prepared_from(head, source):
    if command("git", "show", "-s", "--format=%P", head) != source:
        return False
    message = command("git", "show", "-s", "--format=%B", head)
    author = command("git", "show", "-s", "--format=%ae", head)
    paths = command("git", "diff", "--name-only", source, head).splitlines()
    return (f"Release-Source: {source}" in message.splitlines()
            and author == "41898282+github-actions[bot]@users.noreply.github.com"
            and bool(paths) and metadata_only(paths))


def prepare(source):
    if not re.fullmatch(r"[0-9a-f]{40}", source):
        raise ValueError("source must be a full commit ID")
    if command("git", "status", "--porcelain"):
        raise ValueError("release preparation requires a clean checkout")
    subprocess.run(["git", "fetch", "origin", "main", "--tags"], check=True)
    current = command("git", "rev-parse", "origin/main")
    if current != source:
        if prepared_from(current, source):
            inputs = dependency_inputs()
            # Keep the prepared commit's Rust selection when resuming publication.
            if inputs is not None:
                subprocess.run(["git", "restore", "Cargo.lock"], check=True)
            subprocess.run(["git", "checkout", "--detach", current], check=True)
            record_dependencies(inputs)
            output("revision", current)
        else:
            print("A newer main commit owns the next release")
            output("revision", "")
        return
    subprocess.run(["git", "checkout", "--detach", source], check=True)
    inputs = dependency_inputs()
    subprocess.run(["release-plz", "update", "--allow-dirty"], check=True)
    paths = command("git", "ls-files", "--modified", "--others", "--exclude-standard").splitlines()
    if not metadata_only(paths):
        raise ValueError(f"release-plz changed files outside release metadata: {paths}")
    if paths:
        subprocess.run(["git", "add", "--", *paths], check=True)
        subprocess.run(["git", "-c", "user.name=github-actions[bot]", "-c",
                        "user.email=41898282+github-actions[bot]@users.noreply.github.com", "commit",
                        "-m", "chore: prepare package releases", "-m", f"Release-Source: {source}"], check=True)
        # A normal push rejects a concurrent main update. Never rewrite main.
        subprocess.run(["git", "push", "origin", "HEAD:refs/heads/main"], check=True)
    head = command("git", "rev-parse", "HEAD")
    record_dependencies(inputs)
    unfinished = any(pending(package, head) for package in packages())
    output("revision", head if paths or unfinished else "")


def record_inputs():
    root = Path.cwd()
    release_dependencies.ensure(root)
    record = release_dependencies.record_path(root)
    head = command("git", "rev-parse", "HEAD")
    for package in packages():
        tag = f"{package['name']}-v{package['version']}"
        if tag_commit(tag) != head:
            continue
        release = json.loads(command("gh", "release", "view", tag, "--json", "assets"))
        if any(asset["name"] == record.name for asset in release["assets"]):
            with tempfile.TemporaryDirectory(prefix="idle-release-inputs-") as temporary:
                subprocess.run(["gh", "release", "download", tag, "--pattern", record.name,
                                "--dir", temporary], check=True)
                if (Path(temporary) / record.name).read_bytes() != record.read_bytes():
                    raise ValueError(f"cannot replace published dependency records: {tag}")
        else:
            subprocess.run(["gh", "release", "upload", tag, str(record)], check=True)


def artifact(package_name):
    package = next(package for package in packages() if package["name"] == package_name)
    tag = f"{package_name}-v{package['version']}"
    # Existing public artifacts are immutable. A retry resumes only a draft at this commit.
    revision = tag_commit(tag)
    if revision is None:
        raise ValueError(f"release-plz did not create the artifact tag: {tag}")
    if revision != command("git", "rev-parse", "HEAD"):
        output("tag", "")
        return
    release = json.loads(command("gh", "release", "view", tag, "--json", "isDraft"))
    output("tag", tag if release["isDraft"] else "")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("prepare").add_argument("--source", required=True)
    subparsers.add_parser("artifact").add_argument("--package", required=True)
    subparsers.add_parser("record")
    args = parser.parse_args()
    os.chdir(Path(__file__).resolve().parent.parent)
    if args.command == "prepare":
        prepare(args.source)
    elif args.command == "artifact":
        artifact(args.package)
    else:
        record_inputs()


if __name__ == "__main__":
    main()
