#!/usr/bin/env python3
"""Atomically install and health-check a locally built Nuntius Client."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
from urllib.error import URLError
from urllib.request import urlopen


LOCAL_INFO_URL = "http://127.0.0.1:7331/api/v1/info"
MAX_BINARY_BYTES = 64 * 1024 * 1024


def fail(message: str) -> None:
    raise RuntimeError(message)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Install a verified nuntius-client binary with automatic rollback"
    )
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--binary-sha256", required=True)
    parser.add_argument("--commit-sha", required=True)
    parser.add_argument("--release-sequence", required=True, type=int)
    parser.add_argument("--health-timeout", type=int, default=90)
    return parser.parse_args()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def probe(path: Path) -> dict[str, object]:
    result = subprocess.run(
        [str(path), "build-info"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=15,
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", errors="replace").strip()[:2048]
        fail(f"{path} failed its build-info probe: {detail}")
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as error:
        fail(f"{path} returned invalid build information: {error}")


def verify_identity(
    info: dict[str, object], expected_sha: str, expected_sequence: int
) -> None:
    expected = {
        "name": "nuntius-client",
        "buildSha": expected_sha,
        "releaseSequence": expected_sequence,
        "target": "aarch64-apple-darwin",
    }
    actual = {key: info.get(key) for key in expected}
    if actual != expected:
        fail(f"Client build identity mismatch: expected {expected}, got {actual}")


def run_control(binary: Path, command: str, check: bool = True) -> None:
    result = subprocess.run(
        [str(binary), command],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=30,
        check=False,
    )
    if check and result.returncode != 0:
        detail = result.stderr.decode("utf-8", errors="replace").strip()[:2048]
        fail(f"nuntius-client {command} failed: {detail}")


def copy_synced(source: Path, destination: Path) -> None:
    with source.open("rb") as input_file, destination.open("wb") as output_file:
        shutil.copyfileobj(input_file, output_file, length=1024 * 1024)
        output_file.flush()
        os.fsync(output_file.fileno())
    os.chmod(destination, 0o755)


def wait_for_build(expected_sha: str, expected_sequence: int, timeout: int) -> None:
    deadline = time.monotonic() + timeout
    last_error = "local API did not respond"
    while time.monotonic() < deadline:
        try:
            with urlopen(LOCAL_INFO_URL, timeout=3) as response:
                info = json.load(response)
            if (
                info.get("buildSha") == expected_sha
                and info.get("releaseSequence") == expected_sequence
            ):
                return
            last_error = (
                f"local API reports {info.get('buildSha')} / "
                f"{info.get('releaseSequence')}"
            )
        except (OSError, URLError, json.JSONDecodeError) as error:
            last_error = str(error)
        time.sleep(2)
    fail(f"updated Client did not become healthy within {timeout} seconds: {last_error}")


def main() -> None:
    args = parse_args()
    if not re.fullmatch(r"[0-9a-fA-F]{40}", args.commit_sha):
        fail("commit SHA must contain exactly 40 hexadecimal characters")
    if args.release_sequence <= 0:
        fail("release sequence must be positive")
    if not re.fullmatch(r"[0-9a-fA-F]{64}", args.binary_sha256):
        fail("binary SHA-256 must contain exactly 64 hexadecimal characters")
    if not 15 <= args.health_timeout <= 600:
        fail("health timeout must be between 15 and 600 seconds")

    new_binary = args.binary.expanduser().resolve(strict=True)
    size = new_binary.stat().st_size
    if not new_binary.is_file() or not 0 < size <= MAX_BINARY_BYTES:
        fail("Client binary must be a non-empty regular file no larger than 64 MiB")
    actual_digest = sha256_file(new_binary)
    if actual_digest.lower() != args.binary_sha256.lower():
        fail(
            f"Client binary checksum mismatch: expected {args.binary_sha256}, got {actual_digest}"
        )
    verify_identity(probe(new_binary), args.commit_sha.lower(), args.release_sequence)

    installed_command = shutil.which("nuntius-client")
    if not installed_command:
        fail("nuntius-client is not installed on PATH")
    installed = Path(installed_command).resolve(strict=True)
    install_dir = installed.parent
    stage = install_dir / f".nuntius-client.local-release-{args.commit_sha.lower()}"
    previous = install_dir / "nuntius-client.previous"
    previous_temporary = install_dir / ".nuntius-client.previous.tmp"
    rollback_temporary = install_dir / ".nuntius-client.rollback.tmp"

    print(f"Stopping Client before installing {args.commit_sha.lower()}…")
    run_control(installed, "stop")
    replaced = False
    try:
        copy_synced(new_binary, stage)
        verify_identity(probe(stage), args.commit_sha.lower(), args.release_sequence)
        copy_synced(installed, previous_temporary)
        os.replace(previous_temporary, previous)
        os.replace(stage, installed)
        replaced = True

        run_control(installed, "start")
        wait_for_build(args.commit_sha.lower(), args.release_sequence, args.health_timeout)
        verify_identity(probe(installed), args.commit_sha.lower(), args.release_sequence)
        print("Client replacement is healthy.")
    except Exception:
        if replaced:
            print("Client health check failed; restoring the previous binary…", file=sys.stderr)
            run_control(installed, "stop", check=False)
            copy_synced(previous, rollback_temporary)
            os.replace(rollback_temporary, installed)
            run_control(installed, "start", check=False)
        raise
    finally:
        for temporary in (stage, previous_temporary, rollback_temporary):
            try:
                temporary.unlink()
            except FileNotFoundError:
                pass


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"install-local-client: {error}", file=sys.stderr)
        raise SystemExit(1) from error
