#!/usr/bin/env python3
"""Run the pinned upstream SDK's unit tests with this workspace's SSH patches."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib


def main():
    root = Path(__file__).resolve().parent.parent
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--locked"], cwd=root
    ))
    sdk = next(package for package in metadata["packages"] if package["name"] == "tunnels")
    source = Path(sdk["manifest_path"]).parent
    revision = tomllib.loads((root / "crates/idle-coordination/Cargo.toml").read_text())["dependencies"]["tunnels"]["rev"]
    if not sdk["source"].endswith("#" + revision):
        raise RuntimeError("SDK source does not match the declared Git revision")
    patches = tomllib.loads((root / "Cargo.toml").read_text())["patch"]
    with tempfile.TemporaryDirectory(prefix="idle-tunnels-tests-") as directory:
        destination = Path(directory) / "rs"
        shutil.copytree(source, destination, ignore=shutil.ignore_patterns("target", ".git"))
        license_path = source.parent / "LICENSE"
        if license_path.exists():
            shutil.copyfile(license_path, Path(directory) / "LICENSE")
        shutil.copyfile(root / "Cargo.lock", destination / "Cargo.lock")
        with (destination / "Cargo.toml").open("a") as manifest:
            for registry, packages in patches.items():
                manifest.write(f"\n[patch.{json.dumps(registry)}]\n")
                for name, original in packages.items():
                    specification = dict(original)
                    if "path" in specification:
                        specification["path"] = str(root / specification["path"])
                    fields = ", ".join(f"{key} = {json.dumps(value)}" for key, value in specification.items())
                    manifest.write(f"{json.dumps(name)} = {{ {fields} }}\n")
        environment = dict(os.environ)
        environment["CARGO_TARGET_DIR"] = str(root / "target/tunnels-sdk")
        print(f"Testing Microsoft Dev Tunnels {revision} with workspace SSH patches", flush=True)
        # Upstream also tests Ed25519 key generation, which the Microsoft fork
        # exposes only through rs-crypto. Enable it for this temporary test
        # build; the shipped OpenSSL-only dependency graph remains unchanged.
        features = "connections,reqwest/native-tls,russh/rs-crypto,russh-keys/rs-crypto"
        subprocess.run(["cargo", "test", "--manifest-path", str(destination / "Cargo.toml"),
                        "--no-default-features", "--features", features, "--lib"],
                       cwd=root, env=environment, check=True)


if __name__ == "__main__":
    main()
