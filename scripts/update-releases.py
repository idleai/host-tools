#!/usr/bin/env python3
"""Update compatible native and consumer release versions in one reviewable change."""

import json
from pathlib import Path
import re
import subprocess
import tomllib
import urllib.request

PLATFORMS = ("linux-x64", "darwin-x64", "darwin-arm64", "win32-x64")


def parse_tag(tag):
    match = re.fullmatch(r"(.+-v)(\d+)\.(\d+)\.(\d+)", tag)
    if not match:
        raise ValueError(f"expected a stable package version tag: {tag}")
    return match[1], tuple(int(match[index]) for index in (2, 3, 4))


def compatible(current, candidate):
    if candidate < current:
        return False
    if current[0] != 0:
        return candidate[0] == current[0]
    if current[1] != 0:
        return candidate[:2] == current[:2]
    return candidate == current


def latest(dependency, native, exact=False):
    prefix, current = parse_tag(dependency["tag"])
    records = subprocess.check_output([
        "gh", "api", "--paginate", f"repos/{dependency['repository']}/releases?per_page=100",
        "--jq", ".[] | @json"], text=True)
    candidates = []
    for release in map(json.loads, records.splitlines()):
        if release["draft"] or release["prerelease"] or not release["tag_name"].startswith(prefix):
            continue
        try:
            _, version = parse_tag(release["tag_name"])
        except ValueError:
            continue
        if not compatible(current, version):
            continue
        if exact and release["tag_name"] != dependency["tag"]:
            continue
        assets = {asset["name"]: asset["browser_download_url"] for asset in release["assets"]}
        filenames = {target: f"{release['tag_name']}-{target}.tar.gz" for target in PLATFORMS} if native else {
            "source": f"{dependency['package']}-{'.'.join(map(str, version))}.crate"}
        if any(filename not in assets or filename + ".sha256" not in assets for filename in filenames.values()):
            continue
        candidates.append((version, release["tag_name"], assets, filenames))
    if not candidates:
        raise RuntimeError(f"no complete compatible release for {dependency['repository']}: {dependency['tag']}")
    _, tag, assets, filenames = max(candidates, key=lambda candidate: candidate[0])
    checksums = {}
    for target, filename in filenames.items():
        with urllib.request.urlopen(assets[filename + ".sha256"], timeout=60) as response:
            checksum, recorded_name = response.read().decode().strip().split()
        if recorded_name != filename or not re.fullmatch(r"[0-9a-f]{64}", checksum):
            raise ValueError(f"invalid release checksum: {filename}")
        checksums[target] = checksum
    return dict(dependency, tag=tag, sha256=checksums if native else checksums["source"])


def main():
    root = Path(__file__).resolve().parent.parent
    for filename in ("native-dependencies.json", "consumer-dependencies.json"):
        path = root / filename
        if not path.exists():
            continue
        document = json.loads(path.read_text())
        for name, dependency in document["dependencies"].items():
            document["dependencies"][name] = latest(dependency, filename.startswith("native-"))
        path.write_text(json.dumps(document, indent=2) + "\n")
    lock = tomllib.loads((root / "Cargo.lock").read_text())
    packages = [package for package in lock["package"]
                if "raw.githubusercontent.com/idleai/" in package.get("source", "")]
    if packages:
        arguments = ["cargo", "update"]
        for package in packages:
            arguments += ["-p", f"{package['name']}@{package['version']}"]
        subprocess.run(arguments, cwd=root, check=True)


if __name__ == "__main__":
    main()
