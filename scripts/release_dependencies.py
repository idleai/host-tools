#!/usr/bin/env python3
"""Resolve current compatible releases once for a build and retain its inputs."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tomllib
import urllib.request

PLATFORMS = ("linux-x64", "darwin-x64", "darwin-arm64", "win32-x64")
MANIFESTS = ("native-dependencies.json", "consumer-dependencies.json")
RECORD = Path("target/released-dependencies.json")
ENVIRONMENT = "IDLE_RELEASE_INPUTS"


def command(root, *arguments):
    return subprocess.check_output(arguments, cwd=root, text=True).strip()


def matches(requirement, version):
    match = re.fullmatch(r"\^?(\d+)(?:\.(\d+))?(?:\.(\d+))?", requirement)
    if not match:
        raise ValueError(f"expected a Cargo caret version requirement: {requirement}")
    parts = [int(part) for part in match.groups() if part is not None]
    minimum = tuple(parts + [0] * (3 - len(parts)))
    boundary = next((index for index, value in enumerate(parts) if value), len(parts) - 1)
    maximum = list(minimum)
    maximum[boundary] += 1
    maximum[boundary + 1:] = [0] * (2 - boundary)
    return minimum <= version < tuple(maximum)


def latest(dependency, native):
    prefix = f"{dependency['package']}-v"
    records = subprocess.check_output([
        "gh", "api", "--paginate", f"repos/{dependency['repository']}/releases?per_page=100",
        "--jq", ".[] | @json"], text=True)
    candidates = []
    for release in map(json.loads, records.splitlines()):
        if release["draft"] or release["prerelease"] or not release["tag_name"].startswith(prefix):
            continue
        value = release["tag_name"].removeprefix(prefix)
        if not re.fullmatch(r"\d+\.\d+\.\d+", value):
            continue
        version = tuple(map(int, value.split(".")))
        if not matches(dependency["version"], version):
            continue
        assets = {asset["name"]: asset["browser_download_url"] for asset in release["assets"]}
        filenames = {target: f"{release['tag_name']}-{target}.tar.gz" for target in PLATFORMS} if native else {
            "source": f"{dependency['package']}-{value}.crate"}
        if all(name in assets and name + ".sha256" in assets for name in filenames.values()):
            candidates.append((version, release["tag_name"], assets, filenames))
    if not candidates:
        raise RuntimeError(f"no complete compatible release for {dependency['repository']}: {dependency['version']}")
    _, tag, assets, filenames = max(candidates, key=lambda candidate: candidate[0])
    checksums = {}
    for target, filename in filenames.items():
        request = urllib.request.Request(assets[filename + ".sha256"], headers={"User-Agent": "idle-release-resolver"})
        with urllib.request.urlopen(request, timeout=60) as response:
            checksum, recorded_name = response.read().decode().strip().split()
        if recorded_name != filename or not re.fullmatch(r"[0-9a-f]{64}", checksum):
            raise ValueError(f"invalid release checksum: {filename}")
        checksums[target] = checksum
    return dict(dependency, tag=tag, sha256=checksums if native else checksums["source"])


def definition_hash(root):
    names = command(root, "git", "ls-files", "--cached", "--others", "--exclude-standard", "--",
                    "Cargo.toml", "**/Cargo.toml", ".cargo/config.toml", *MANIFESTS).splitlines()
    # Git checkouts may use CRLF on Windows; requirements are text on every host.
    hashes = {name: hashlib.sha256((root / name).read_text().encode()).hexdigest() for name in sorted(set(names))}
    return hashlib.sha256(json.dumps(hashes, sort_keys=True).encode()).hexdigest()


def record_path(root):
    path = Path(os.environ.get(ENVIRONMENT) or str(RECORD))
    return path if path.is_absolute() else root / path


def capture(root, artifacts):
    record = {"schema": 1, "revision": command(root, "git", "rev-parse", "HEAD"),
              "definitions": definition_hash(root), "cargo_lock": (root / "Cargo.lock").read_text(),
              "artifacts": artifacts}
    destination = record_path(root)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")
    return record


def read(root):
    record = json.loads(record_path(root).read_text())
    if record.get("schema") != 1:
        raise ValueError("unsupported released dependency record")
    if record["revision"] != command(root, "git", "rev-parse", "HEAD"):
        raise ValueError("released dependency record belongs to a different source commit")
    if record["definitions"] != definition_hash(root):
        raise ValueError("dependency requirements changed after resolution")
    tomllib.loads(record["cargo_lock"])
    expected = {name for name in MANIFESTS if (root / name).exists()}
    if set(record["artifacts"]) != expected:
        raise ValueError("released dependency record has different artifact manifests")
    for name, document in record["artifacts"].items():
        specifications = json.loads((root / name).read_text())["dependencies"]
        if set(document["dependencies"]) != set(specifications):
            raise ValueError("released dependency names changed after resolution")
        for key, dependency in document["dependencies"].items():
            specification = specifications[key]
            if any(dependency[field] != specification[field] for field in ("repository", "package", "version")):
                raise ValueError("released dependency identity changed after resolution")
            prefix = f"{specification['package']}-v"
            version = dependency["tag"].removeprefix(prefix)
            if (not dependency["tag"].startswith(prefix) or not re.fullmatch(r"\d+\.\d+\.\d+", version)
                    or not matches(specification["version"], tuple(map(int, version.split("."))))):
                raise ValueError("resolved artifact does not satisfy its version requirement")
            checksums = dependency["sha256"]
            if name.startswith("native-"):
                local_platform = dependency.get("local_platform")
                if local_platform and (os.environ.get("IDLE_LOCAL_RELEASE_INPUTS") != "1" or local_platform not in PLATFORMS):
                    raise ValueError("local native bundles require an explicit integration run")
                expected_platforms = {local_platform} if local_platform else set(PLATFORMS)
                if set(checksums) != expected_platforms:
                    raise ValueError("resolved native release is missing a platform")
                checksums = checksums.values()
            else:
                checksums = [checksums]
            if any(not re.fullmatch(r"[0-9a-f]{64}", checksum) for checksum in checksums):
                raise ValueError("resolved artifact has an invalid checksum")
    return record


def restore(root):
    record = read(root)
    (root / "Cargo.lock").write_text(record["cargo_lock"])
    return record


def update_cargo(root):
    # Preserve third-party selections unless an internal release needs a change.
    path = root / "Cargo.lock"
    lock = tomllib.loads(path.read_text()) if path.exists() else {"package": []}
    packages = [package for package in lock["package"]
                if "raw.githubusercontent.com/idleai/" in package.get("source", "")]
    # Manifest edits and internal upgrades must resolve together: an older locked
    # internal package can conflict with a newly declared third-party requirement.
    arguments = ["cargo", "update", "--workspace"]
    for package in packages:
        arguments += ["-p", f"{package['name']}@{package['version']}"]
    subprocess.run(arguments, cwd=root, check=True)


def resolve(root):
    update_cargo(root)
    artifacts = {}
    for filename in MANIFESTS:
        path = root / filename
        if path.exists():
            document = json.loads(path.read_text())
            if document.get("schema") != 2:
                raise ValueError(f"expected version requirements in {filename}")
            artifacts[filename] = {"schema": 1, "dependencies": {
                name: latest(dependency, filename.startswith("native-"))
                for name, dependency in document["dependencies"].items()}}
    record = capture(root, artifacts)
    for document in artifacts.values():
        for name, dependency in document["dependencies"].items():
            print(f"Resolved {name}: {dependency['tag']}")
    print(f"Recorded build dependencies in {record_path(root)}")
    return record


def ensure(root):
    if os.environ.get(ENVIRONMENT):
        record = read(root)
        if (root / "Cargo.lock").read_text() != record["cargo_lock"]:
            raise ValueError("Cargo.lock changed after dependency resolution")
        return record
    return resolve(root)


def fingerprint(record):
    selection = {"cargo_lock": record["cargo_lock"], "artifacts": record["artifacts"]}
    return hashlib.sha256(json.dumps(selection, sort_keys=True).encode()).hexdigest()


def main():
    root = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("resolve", "restore", "refresh", "verify", "run"), nargs="?", default="resolve")
    parser.add_argument("arguments", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.command == "resolve":
        resolve(root)
    elif args.command == "restore":
        restore(root)
    elif args.command == "refresh":
        capture(root, json.loads(record_path(root).read_text())["artifacts"])
    elif args.command == "verify":
        ensure(root)
    else:
        arguments = args.arguments[1:] if args.arguments[:1] == ["--"] else args.arguments
        if not arguments:
            parser.error("run needs a command")
        ensure(root)
        environment = dict(os.environ, **{ENVIRONMENT: str(record_path(root))})
        arguments[0] = shutil.which(arguments[0]) or arguments[0]
        raise SystemExit(subprocess.run(arguments, cwd=root, env=environment, check=False).returncode)


if __name__ == "__main__":
    main()
