#!/usr/bin/env python3
"""Stream a verified local Server release through the configured SSH relay."""

from __future__ import annotations

import argparse
import hashlib
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tomllib


MAX_ARCHIVE_BYTES = 64 * 1024 * 1024
SAFE_DEVICE_ID = re.compile(r"^[A-Za-z0-9_.-]{1,128}$")
SAFE_REMOTE_PATH = re.compile(r"^/[A-Za-z0-9/._-]{1,1023}$")


def fail(message: str) -> None:
    raise SystemExit(message)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Relay a Nuntius Server archive using ~/.nuntius/config.toml"
    )
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--commit-sha", required=True)
    parser.add_argument("--release-sequence", required=True, type=int)
    parser.add_argument("--archive-sha256", required=True)
    parser.add_argument(
        "--config",
        type=Path,
        default=Path.home() / ".nuntius" / "config.toml",
    )
    return parser.parse_args()


def validate_remote_path(value: object, name: str) -> str:
    if not isinstance(value, str) or not SAFE_REMOTE_PATH.fullmatch(value):
        fail(f"{name} must be an absolute path containing only safe characters")
    if ".." in PurePosixPath(value).parts:
        fail(f"{name} must not contain parent-directory components")
    return value


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> None:
    args = parse_args()
    if not re.fullmatch(r"[0-9a-fA-F]{40}", args.commit_sha):
        fail("commit SHA must contain exactly 40 hexadecimal characters")
    if args.release_sequence <= 0:
        fail("release sequence must be positive")
    if not re.fullmatch(r"[0-9a-fA-F]{64}", args.archive_sha256):
        fail("archive SHA-256 must contain exactly 64 hexadecimal characters")

    archive = args.archive.resolve(strict=True)
    archive_stat = archive.stat()
    if not archive.is_file() or not 0 < archive_stat.st_size <= MAX_ARCHIVE_BYTES:
        fail("archive must be a non-empty regular file no larger than 64 MiB")
    actual_sha256 = sha256_file(archive)
    if actual_sha256.lower() != args.archive_sha256.lower():
        fail(
            f"archive checksum mismatch: expected {args.archive_sha256}, got {actual_sha256}"
        )

    config_path = args.config.expanduser().resolve(strict=True)
    config = tomllib.loads(config_path.read_text(encoding="utf-8"))
    if config.get("server_update_relay") is not True:
        fail("server_update_relay is not enabled in the Client configuration")

    command = config.get("server_update_ssh_command")
    if (
        not isinstance(command, list)
        or len(command) < 2
        or any(not isinstance(item, str) or not item or "\0" in item for item in command)
    ):
        fail("server_update_ssh_command is incomplete or unsafe")

    remote_binary = validate_remote_path(
        config.get("server_update_remote_binary"), "server_update_remote_binary"
    )
    remote_data_dir = validate_remote_path(
        config.get("server_update_remote_data_dir"), "server_update_remote_data_dir"
    )
    device_id = config.get("device_id")
    if not isinstance(device_id, str) or not SAFE_DEVICE_ID.fullmatch(device_id):
        fail("device_id is missing or unsafe")

    timeout = config.get("server_update_ssh_timeout_seconds", 900)
    if not isinstance(timeout, int) or not 60 <= timeout <= 3600:
        fail("server_update_ssh_timeout_seconds must be between 60 and 3600")

    remote_command = [
        *command,
        remote_binary,
        "--data-dir",
        remote_data_dir,
        "receive-update",
        "--commit-sha",
        args.commit_sha.lower(),
        "--release-sequence",
        str(args.release_sequence),
        "--archive-sha256",
        actual_sha256,
        "--source-device-id",
        device_id,
    ]

    print("Relaying verified Server archive over the configured SSH connection…")
    with archive.open("rb") as source:
        try:
            result = subprocess.run(
                remote_command,
                stdin=source,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=timeout,
                check=False,
            )
        except subprocess.TimeoutExpired:
            fail(f"Server update relay timed out after {timeout} seconds")

    stdout = result.stdout.decode("utf-8", errors="replace").strip()
    stderr = result.stderr.decode("utf-8", errors="replace").strip()
    if result.returncode != 0:
        detail = stderr[:2048] or stdout[:2048] or "no remote error output"
        fail(f"Server update relay exited with {result.returncode}: {detail}")
    if stdout:
        print(stdout)
    print("Server archive accepted by the remote update inbox.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, tomllib.TOMLDecodeError, subprocess.SubprocessError) as error:
        print(f"relay-server-update: {error}", file=sys.stderr)
        raise SystemExit(1) from error
