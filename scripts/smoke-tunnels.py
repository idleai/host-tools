#!/usr/bin/env python3
"""Explicit live smoke test; creates private Microsoft relays and deletes them."""

import argparse
import asyncio
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import uuid


class NativeService:
    def __init__(self, binary, config, environment):
        self.binary, self.config, self.environment = binary, config, environment
        self.process = None
        self.sequence = 0

    async def start(self):
        self.process = await asyncio.create_subprocess_exec(
            str(self.binary), "--config", str(self.config), env=self.environment,
            stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.DEVNULL,
        )

    async def call(self, kind, data=None):
        self.sequence += 1
        command = {"kind": kind}
        if data is not None:
            command["data"] = data
        request_id = str(self.sequence)
        body = json.dumps({"kind": "call", "data": {"version": 1, "id": request_id,
                          "timeout_ms": 60_000, "command": command}}).encode()
        self.process.stdin.write(struct.pack(">I", len(body)) + body)
        await self.process.stdin.drain()
        length = struct.unpack(">I", await asyncio.wait_for(self.process.stdout.readexactly(4), 90))[0]
        if length > 16 * 1024 * 1024:
            raise RuntimeError("oversized service response")
        response = json.loads(await asyncio.wait_for(self.process.stdout.readexactly(length), 30))
        if response["id"] != request_id or response["version"] != 1:
            raise RuntimeError("unexpected service response")
        if "Err" in response["result"]:
            raise RuntimeError(f"{kind}: {response['result']['Err']}")
        return response["result"]["Ok"]

    async def close(self):
        if self.process is None or self.process.returncode is not None:
            return
        self.process.stdin.close()
        try:
            await asyncio.wait_for(self.process.wait(), 45)
        except asyncio.TimeoutError:
            self.process.terminate()
            await asyncio.wait_for(self.process.wait(), 30)


async def live(left, right):
    for _ in range(60):
        statuses = await asyncio.gather(left.call("sharing_status"), right.call("sharing_status"))
        if all(any(peer["state"] == "Live" for peer in status["peers"]) for status in statuses):
            return
        await asyncio.sleep(1)
    raise RuntimeError("native peers did not complete authenticated inventory checks")


async def run(arguments):
    root = Path(tempfile.mkdtemp(prefix="idle-native-relay-"))
    environment = dict(os.environ)
    if arguments.github_auth:
        environment["IDLE_TUNNELS_GITHUB_TOKEN"] = subprocess.check_output(["gh", "auth", "token"], text=True).strip()
    if not environment.get("IDLE_TUNNELS_GITHUB_TOKEN"):
        raise RuntimeError("set IDLE_TUNNELS_GITHUB_TOKEN or pass --github-auth")
    workspace = {"id": str(uuid.uuid4()), "name": "Native relay smoke", "chain": str(uuid.uuid4()),
                 "mode": {"kind": "standalone", "repository": {"id": str(uuid.uuid4()), "name": "Smoke", "remote": None}}}
    services = []
    cleaned = False
    try:
        for name in ["host", "guest"]:
            directory = root / name
            directory.mkdir(mode=0o700)
            config = directory / "service.json"
            config.write_text(json.dumps({"state_directory": str(directory / "private"),
                "chain_directory": str(directory / "chain"), "device_directory": str(directory / "device"),
                "workspace": workspace, "contributor": {"contributor_id": name,
                    "authenticated_as": {"issuer": "local-process", "subject": name}},
                "runtime": None, "credential_variable": None, "discovery_repository": None, "resume_sharing": True}))
            service = NativeService(arguments.binary, config, environment)
            services.append(service)
            await service.start()
        host, guest = services
        versions = await host.call("versions")
        print(f"SDK revision: {versions['tunnels_revision']}", flush=True)
        request = await guest.call("join_request")
        invitation = await host.call("host", {"request": request, "scope": "all"})
        await guest.call("join", {"invitation": invitation, "scope": "all"})
        await live(host, guest)
        print("Native host/client authentication and inventory: PASS", flush=True)
        await host.call("reconnect")
        await guest.call("reconnect")
        await live(host, guest)
        print("Relay reconnect with refreshed host route: PASS", flush=True)
        await guest.close()
        await guest.start()
        await live(host, guest)
        print("Service restart with retained scope and grant: PASS", flush=True)
    finally:
        results = []
        for service in reversed(services):
            try:
                await service.call("stop")
                results.append(True)
            except (RuntimeError, asyncio.IncompleteReadError, BrokenPipeError, asyncio.TimeoutError) as error:
                print(f"Cleanup pending: {error}", flush=True)
                results.append(False)
            finally:
                await service.close()
        cleaned = bool(results) and all(results)
        if cleaned:
            shutil.rmtree(root)
            print("Owned relay deletion and native shutdown: PASS", flush=True)
        else:
            print(f"Private recovery state retained at {root}", flush=True)
    if not cleaned:
        raise RuntimeError("live smoke cleanup remains pending")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--github-auth", action="store_true", help="read the current gh credential privately")
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parent.parent / "target/debug/idle-coordination")
    asyncio.run(run(parser.parse_args()))
