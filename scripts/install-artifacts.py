#!/usr/bin/env python3
"""Install the native release versions recorded by this repository."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import tarfile
import tempfile
import urllib.request

import release_dependencies


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def host_platform():
    system = {"Linux": "linux", "Darwin": "darwin", "Windows": "win32"}[platform.system()]
    machine = platform.machine().lower()
    architecture = {"x86_64": "x64", "amd64": "x64", "aarch64": "arm64", "arm64": "arm64"}[machine]
    return f"{system}-{architecture}"


def fetch(dependency, filename, destination):
    for mirror in filter(None, os.environ.get("IDLE_ARTIFACTS_MIRROR", "").split(os.pathsep)):
        source = Path(mirror) / filename
        if source.is_file():
            shutil.copyfile(source, destination)
            return
    url = f"https://github.com/{dependency['repository']}/releases/download/{dependency['tag']}/{filename}"
    request = urllib.request.Request(url, headers={"User-Agent": "idle-native-installer"})
    with urllib.request.urlopen(request, timeout=120) as response, destination.open("wb") as output:
        shutil.copyfileobj(response, output)


def verify(directory, dependency, target):
    manifest = json.loads((directory / "bundle.json").read_text())
    if (directory / ".archive-sha256").read_text().strip() != dependency["sha256"][target]:
        raise ValueError("cached archive does not match the recorded release checksum")
    expected = (dependency["repository"], dependency["tag"], target)
    actual = (manifest["repository"], manifest["tag"], manifest["platform"])
    if actual != expected or not manifest["files"]:
        raise ValueError(f"unexpected native bundle identity: {actual}")
    for filename, checksum in manifest["files"].items():
        path = directory / filename
        if not path.resolve().is_relative_to(directory.resolve()) or digest(path) != checksum:
            raise ValueError(f"native bundle checksum mismatch: {filename}")


def install(name, dependency, root, target):
    destination = root / ".artifacts" / name
    if destination.is_dir():
        try:
            verify(destination, dependency, target)
            return
        except (OSError, ValueError, KeyError):
            pass
    filename = f"{dependency['tag']}-{target}.tar.gz"
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination.parent, prefix=f"{name}-") as temporary:
        staging = Path(temporary)
        archive = staging / filename
        checksum = staging / f"{filename}.sha256"
        fetch(dependency, archive.name, archive)
        fetch(dependency, checksum.name, checksum)
        if digest(archive) != dependency["sha256"][target] or digest(archive) != checksum.read_text().split()[0]:
            raise ValueError(f"release archive checksum mismatch: {filename}")
        extracted = staging / "contents"
        extracted.mkdir()
        with tarfile.open(archive, "r:gz") as bundle:
            for member in bundle.getmembers():
                path = (extracted / member.name).resolve()
                if not path.is_relative_to(extracted.resolve()) or not (member.isfile() or member.isdir()):
                    raise ValueError(f"unsupported archive member: {member.name}")
            bundle.extractall(extracted, filter="data")
        (extracted / ".archive-sha256").write_text(digest(archive) + "\n")
        verify(extracted, dependency, target)
        if destination.exists():
            shutil.rmtree(destination)
        shutil.move(extracted, destination)
    print(f"Installed {name}: {dependency['tag']} ({target})")


def main():
    root = Path(__file__).resolve().parent.parent
    selection = release_dependencies.ensure(root)
    dependencies = selection["artifacts"]["native-dependencies.json"]["dependencies"]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("names", nargs="*", choices=list(dependencies))
    args = parser.parse_args()
    for name in args.names or dependencies:
        install(name, dependencies[name], root, host_platform())


if __name__ == "__main__":
    main()
