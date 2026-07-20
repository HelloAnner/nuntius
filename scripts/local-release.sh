#!/bin/bash

set -euo pipefail
IFS=$'\n\t'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SOURCE_REPO="${NUNTIUS_RELEASE_SOURCE_REPO:-$REPO_ROOT}"
STATE_DIR="${NUNTIUS_RELEASE_STATE_DIR:-$HOME/Library/Application Support/Nuntius Local Release}"
BUILDER_PROFILE="${NUNTIUS_RELEASE_COLIMA_PROFILE:-nuntius-builder}"
DOCKER_CONTEXT="colima-$BUILDER_PROFILE"
RUST_TOOLCHAIN="${NUNTIUS_RELEASE_RUST_TOOLCHAIN:-1.94.0}"
MANYLINUX_IMAGE="${NUNTIUS_RELEASE_MANYLINUX_IMAGE:-quay.io/pypa/manylinux2014_x86_64}"

FORCE_BUILD=0
FORCE_DEPLOY=0
NO_DEPLOY=0
SCHEDULED=0
WORK_DIR=""
LOCK_HELD=0

usage() {
  cat <<'EOF'
Usage: scripts/local-release.sh [options]

Build and optionally deploy the latest origin/main from an isolated source archive.

Options:
  --force-build   Rebuild an already cached commit.
  --force-deploy  Deploy even when the local Client reports active turns.
  --no-deploy     Build and verify packages without deploying them.
  --scheduled     Defer instead of failing when another run or active turn blocks deployment.
  -h, --help      Show this help.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --force-build) FORCE_BUILD=1 ;;
    --force-deploy) FORCE_DEPLOY=1 ;;
    --no-deploy) NO_DEPLOY=1 ;;
    --scheduled) SCHEDULED=1 ;;
    -h|--help) usage; exit 0 ;;
    *) printf 'Unknown option: %s\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

log() {
  printf '%s %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*"
}

die() {
  log "ERROR: $*" >&2
  exit 1
}

safe_remove_tree() {
  local target="$1"
  case "$target" in
    "$STATE_DIR/work/"*|"$STATE_DIR/releases/"*) ;;
    *) die "refusing to remove path outside the local release state: $target" ;;
  esac
  [ -f "$target/.nuntius-local-release-owned" ] \
    || die "refusing to remove an unowned path: $target"
  find "$target" -depth -delete
}

cleanup() {
  local result=$?
  if [ -n "$WORK_DIR" ] && [ -d "$WORK_DIR" ]; then
    safe_remove_tree "$WORK_DIR" || true
  fi
  if [ "$LOCK_HELD" -eq 1 ]; then
    [ ! -f "$STATE_DIR/run.lock/pid" ] || unlink "$STATE_DIR/run.lock/pid"
    rmdir "$STATE_DIR/run.lock" 2>/dev/null || true
  fi
  exit "$result"
}
trap cleanup EXIT INT TERM

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command is missing: $1"
}

for tool in git bun cargo rustup colima docker jq curl python3 shasum sha256sum tar file strings find unlink; do
  require_command "$tool"
done

[ -d "$SOURCE_REPO" ] || die "source repository does not exist: $SOURCE_REPO"
git -C "$SOURCE_REPO" rev-parse --git-dir >/dev/null 2>&1 \
  || die "source path is not a Git repository: $SOURCE_REPO"

mkdir -p "$STATE_DIR" "$STATE_DIR/logs" "$STATE_DIR/releases" "$STATE_DIR/cache" "$STATE_DIR/work"
chmod 700 "$STATE_DIR" "$STATE_DIR/logs" "$STATE_DIR/releases" "$STATE_DIR/cache" "$STATE_DIR/work"

if ! mkdir "$STATE_DIR/run.lock" 2>/dev/null; then
  existing_pid="$(cat "$STATE_DIR/run.lock/pid" 2>/dev/null || true)"
  if [[ "$existing_pid" =~ ^[0-9]+$ ]] && kill -0 "$existing_pid" 2>/dev/null; then
    log "Another local release run is active with pid $existing_pid."
    if [ "$SCHEDULED" -eq 1 ]; then
      exit 0
    fi
    exit 2
  fi
  [ ! -f "$STATE_DIR/run.lock/pid" ] || unlink "$STATE_DIR/run.lock/pid"
  rmdir "$STATE_DIR/run.lock" 2>/dev/null \
    || die "cannot recover the stale local release lock"
  mkdir "$STATE_DIR/run.lock" || die "cannot acquire the local release lock"
fi
printf '%s\n' "$$" > "$STATE_DIR/run.lock/pid"
LOCK_HELD=1

REMOTE_URL="$(git -C "$SOURCE_REPO" remote get-url origin)"
MIRROR_DIR="$STATE_DIR/source.git"
if [ ! -d "$MIRROR_DIR/objects" ]; then
  mirror_work="$(mktemp -d "$STATE_DIR/work/mirror.XXXXXX")"
  touch "$mirror_work/.nuntius-local-release-owned"
  log "Creating the isolated source mirror."
  git clone --mirror --no-hardlinks "$SOURCE_REPO" "$mirror_work/repository.git"
  git --git-dir="$mirror_work/repository.git" remote set-url origin "$REMOTE_URL"
  mv "$mirror_work/repository.git" "$MIRROR_DIR"
  safe_remove_tree "$mirror_work"
fi
git --git-dir="$MIRROR_DIR" remote set-url origin "$REMOTE_URL"
log "Fetching origin/main into the isolated source mirror."
git --git-dir="$MIRROR_DIR" fetch --quiet --prune origin \
  '+refs/heads/main:refs/remotes/origin/main'
TARGET_SHA="$(git --git-dir="$MIRROR_DIR" rev-parse refs/remotes/origin/main^{commit})"
[[ "$TARGET_SHA" =~ ^[0-9a-f]{40}$ ]] || die "origin/main did not resolve to a full commit SHA"

client_info() {
  curl --fail --silent --show-error --max-time 5 \
    http://127.0.0.1:7331/api/v1/info 2>/dev/null || printf '{}'
}

SERVER_URL="$(python3 - "$HOME/.nuntius/config.toml" <<'PY'
from pathlib import Path
import sys
import tomllib

config = tomllib.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
print(config["server_url"].rstrip("/"))
PY
)"

server_info() {
  curl --fail --silent --show-error --max-time 10 \
    "$SERVER_URL/api/v1/info" 2>/dev/null || printf '{}'
}

CLIENT_INFO="$(client_info)"
SERVER_INFO="$(server_info)"
CLIENT_SHA="$(printf '%s' "$CLIENT_INFO" | jq -r '.buildSha // ""')"
SERVER_SHA="$(printf '%s' "$SERVER_INFO" | jq -r '.buildSha // ""')"
CLIENT_SEQUENCE="$(printf '%s' "$CLIENT_INFO" | jq -r '.releaseSequence // 0')"
SERVER_SEQUENCE="$(printf '%s' "$SERVER_INFO" | jq -r '.releaseSequence // 0')"

if [ "$FORCE_BUILD" -eq 0 ] \
  && [ "$CLIENT_SHA" = "$TARGET_SHA" ] \
  && [ "$SERVER_SHA" = "$TARGET_SHA" ]; then
  log "Server and Client already run origin/main $TARGET_SHA."
  printf '%s\n' "$TARGET_SHA" > "$STATE_DIR/last-successful-sha"
  exit 0
fi

RELEASE_DIR="$STATE_DIR/releases/$TARGET_SHA"
if [ "$FORCE_BUILD" -eq 1 ] && [ -d "$RELEASE_DIR" ]; then
  log "Removing the cached release for a forced rebuild."
  safe_remove_tree "$RELEASE_DIR"
fi

verify_client_identity() {
  local binary="$1"
  local expected_sha="$2"
  local expected_sequence="$3"
  "$binary" build-info | jq -e \
    --arg sha "$expected_sha" \
    --argjson sequence "$expected_sequence" \
    '.name == "nuntius-client"
      and .buildSha == $sha
      and .releaseSequence == $sequence
      and .target == "aarch64-apple-darwin"' >/dev/null
}

verify_cached_release() {
  local directory="$1"
  [ -f "$directory/.nuntius-local-release-owned" ] || return 1
  [ -f "$directory/.complete" ] || return 1
  [ -f "$directory/manifest.json" ] || return 1
  [ -x "$directory/nuntius-client" ] || return 1
  [ -f "$directory/nuntius-server-linux-x86_64.tar.gz" ] || return 1
  [ -f "$directory/nuntius-client-macos-arm64.tar.gz" ] || return 1
  local manifest_sha manifest_sequence server_archive_sha client_archive_sha
  manifest_sha="$(jq -r '.commitSha // ""' "$directory/manifest.json")"
  manifest_sequence="$(jq -r '.releaseSequence // 0' "$directory/manifest.json")"
  [ "$manifest_sha" = "$TARGET_SHA" ] || return 1
  [[ "$manifest_sequence" =~ ^[0-9]+$ ]] || return 1
  [ "$manifest_sequence" -gt 0 ] || return 1
  server_archive_sha="$(shasum -a 256 "$directory/nuntius-server-linux-x86_64.tar.gz" | awk '{print $1}')"
  client_archive_sha="$(shasum -a 256 "$directory/nuntius-client-macos-arm64.tar.gz" | awk '{print $1}')"
  [ "$server_archive_sha" = "$(jq -r '.server.sha256' "$directory/manifest.json")" ] || return 1
  [ "$client_archive_sha" = "$(jq -r '.client.sha256' "$directory/manifest.json")" ] || return 1
  verify_client_identity "$directory/nuntius-client" "$TARGET_SHA" "$manifest_sequence"
}

if [ -d "$RELEASE_DIR" ]; then
  verify_cached_release "$RELEASE_DIR" \
    || die "cached release is incomplete or invalid; rerun with --force-build"
  log "Reusing the verified cached release for $TARGET_SHA."
else
  WORK_DIR="$(mktemp -d "$STATE_DIR/work/build.XXXXXX")"
  touch "$WORK_DIR/.nuntius-local-release-owned"
  SOURCE_DIR="$WORK_DIR/source"
  OUTPUT_DIR="$WORK_DIR/release"
  mkdir -p "$SOURCE_DIR" "$OUTPUT_DIR"
  touch "$OUTPUT_DIR/.nuntius-local-release-owned"
  git --git-dir="$MIRROR_DIR" archive "$TARGET_SHA" | tar -x -C "$SOURCE_DIR"

  now_ms="$(($(date +%s) * 1000))"
  last_sequence="$(cat "$STATE_DIR/last-sequence" 2>/dev/null || printf '0')"
  for value in "$last_sequence" "$CLIENT_SEQUENCE" "$SERVER_SEQUENCE"; do
    [[ "$value" =~ ^[0-9]+$ ]] || value=0
    if [ "$value" -ge "$now_ms" ]; then
      now_ms="$((value + 1))"
    fi
  done
  RELEASE_SEQUENCE="$now_ms"

  if ! rustup toolchain list | grep -q "^$RUST_TOOLCHAIN-"; then
    log "Installing Rust $RUST_TOOLCHAIN for reproducible release builds."
    rustup toolchain install "$RUST_TOOLCHAIN" --profile minimal
  fi

  log "Installing frontend dependencies."
  (cd "$SOURCE_DIR" && bun install --frozen-lockfile)
  log "Typechecking both frontends."
  (cd "$SOURCE_DIR" && bun run typecheck)
  log "Building both embedded frontends."
  (cd "$SOURCE_DIR" && bun run build)

  MACOS_TARGET_DIR="$STATE_DIR/cache/target-macos"
  mkdir -p "$MACOS_TARGET_DIR"
  log "Running the Rust workspace tests on macOS ARM64."
  (cd "$SOURCE_DIR" && CARGO_TARGET_DIR="$MACOS_TARGET_DIR" \
    cargo "+$RUST_TOOLCHAIN" test --locked --workspace)

  log "Building the macOS ARM64 Client."
  (cd "$SOURCE_DIR" && \
    NUNTIUS_BUILD_SHA="$TARGET_SHA" \
    NUNTIUS_BUILD_SEQUENCE="$RELEASE_SEQUENCE" \
    NUNTIUS_BUILD_TARGET="aarch64-apple-darwin" \
    CARGO_TARGET_DIR="$MACOS_TARGET_DIR" \
    cargo "+$RUST_TOOLCHAIN" build --locked --release --package nuntius-client)
  CLIENT_BINARY="$MACOS_TARGET_DIR/release/nuntius-client"
  verify_client_identity "$CLIENT_BINARY" "$TARGET_SHA" "$RELEASE_SEQUENCE" \
    || die "macOS Client build identity verification failed"
  cp "$CLIENT_BINARY" "$OUTPUT_DIR/nuntius-client"
  chmod 755 "$OUTPUT_DIR/nuntius-client"

  if ! colima --profile "$BUILDER_PROFILE" status >/dev/null 2>&1; then
    log "Starting the isolated Colima builder profile."
    colima --profile "$BUILDER_PROFILE" start \
      --arch aarch64 \
      --vm-type vz \
      --binfmt \
      --dns 114.114.114.114 \
      --dns 223.5.5.5 \
      --cpus 6 \
      --memory 8 \
      --disk 60 \
      --runtime docker \
      --activate=false \
      --kubernetes=false
  fi
  # Some macOS proxy/DNS combinations leave systemd-resolved's runtime stub absent
  # after the VM boots. Repair only this dedicated builder profile before Docker
  # resolves registries or package hosts.
  colima --profile "$BUILDER_PROFILE" ssh -- \
    sudo mkdir -p /run/systemd/resolve
  printf '%s\n' \
    'nameserver 114.114.114.114' \
    'nameserver 223.5.5.5' \
    'options timeout:2 attempts:3' \
    | colima --profile "$BUILDER_PROFILE" ssh -- \
      sudo tee /run/systemd/resolve/stub-resolv.conf >/dev/null
  colima --profile "$BUILDER_PROFILE" ssh -- getent hosts quay.io >/dev/null
  docker --context "$DOCKER_CONTEXT" info >/dev/null

  mkdir -p "$STATE_DIR/cache/cargo-linux" "$STATE_DIR/cache/target-linux"
  log "Building the CentOS 7 compatible Linux x86_64 Server under emulation."
  docker --context "$DOCKER_CONTEXT" run --rm --platform linux/amd64 \
    -e RUST_VERSION="$RUST_TOOLCHAIN" \
    -e NUNTIUS_BUILD_SHA="$TARGET_SHA" \
    -e NUNTIUS_BUILD_SEQUENCE="$RELEASE_SEQUENCE" \
    -e NUNTIUS_BUILD_TARGET="x86_64-unknown-linux-gnu" \
    -v "$SOURCE_DIR:/workspace" \
    -v "$STATE_DIR/cache/cargo-linux:/root/.cargo" \
    -v "$STATE_DIR/cache/target-linux:/workspace/target" \
    -w /workspace \
    "$MANYLINUX_IMAGE" \
    /bin/bash -lc '
      set -euo pipefail
      if [ ! -x "$HOME/.cargo/bin/rustc" ]; then
        curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs \
          | sh -s -- -y --profile minimal --default-toolchain "$RUST_VERSION"
      fi
      source "$HOME/.cargo/env"
      rustup default "$RUST_VERSION"
      cargo build --locked --release --package nuntius-server
      target/release/nuntius-server build-info
    '
  SERVER_BINARY="$STATE_DIR/cache/target-linux/release/nuntius-server"
  [ -f "$SERVER_BINARY" ] || die "Linux Server binary was not produced"
  file "$SERVER_BINARY" | grep -q 'ELF 64-bit.*x86-64' \
    || die "Linux Server is not an x86_64 ELF binary"
  strings "$SERVER_BINARY" | python3 -c '
import re, sys
versions = [tuple(map(int, match.groups())) for line in sys.stdin for match in [re.search(r"GLIBC_(\d+)\.(\d+)", line)] if match]
highest = max(versions, default=(0, 0))
if highest > (2, 17):
    raise SystemExit(f"Linux Server requires GLIBC_{highest[0]}.{highest[1]}, expected at most GLIBC_2.17")
print(f"Verified maximum GLIBC requirement: {highest[0]}.{highest[1]}")
'
  SERVER_INFO_JSON="$(docker --context "$DOCKER_CONTEXT" run --rm --platform linux/amd64 \
    -v "$SERVER_BINARY:/nuntius-server:ro" \
    "$MANYLINUX_IMAGE" /nuntius-server build-info)"
  printf '%s' "$SERVER_INFO_JSON" | jq -e \
    --arg sha "$TARGET_SHA" \
    --argjson sequence "$RELEASE_SEQUENCE" \
    '.name == "nuntius-server"
      and .buildSha == $sha
      and .releaseSequence == $sequence
      and .target == "x86_64-unknown-linux-gnu"' >/dev/null \
    || die "Linux Server build identity verification failed"
  cp "$SERVER_BINARY" "$OUTPUT_DIR/nuntius-server"
  chmod 755 "$OUTPUT_DIR/nuntius-server"

  mkdir -p "$WORK_DIR/package-client" "$WORK_DIR/package-server"
  cp "$OUTPUT_DIR/nuntius-client" "$WORK_DIR/package-client/"
  cp "$OUTPUT_DIR/nuntius-server" "$WORK_DIR/package-server/"
  (cd "$WORK_DIR/package-client" && sha256sum nuntius-client > SHA256SUMS)
  (cd "$WORK_DIR/package-server" && sha256sum nuntius-server > SHA256SUMS)
  (cd "$WORK_DIR/package-client" && tar -czf "$OUTPUT_DIR/nuntius-client-macos-arm64.tar.gz" nuntius-client SHA256SUMS)
  (cd "$WORK_DIR/package-server" && tar -czf "$OUTPUT_DIR/nuntius-server-linux-x86_64.tar.gz" nuntius-server SHA256SUMS)

  CLIENT_BINARY_SHA="$(shasum -a 256 "$OUTPUT_DIR/nuntius-client" | awk '{print $1}')"
  SERVER_BINARY_SHA="$(shasum -a 256 "$OUTPUT_DIR/nuntius-server" | awk '{print $1}')"
  CLIENT_ARCHIVE_SHA="$(shasum -a 256 "$OUTPUT_DIR/nuntius-client-macos-arm64.tar.gz" | awk '{print $1}')"
  SERVER_ARCHIVE_SHA="$(shasum -a 256 "$OUTPUT_DIR/nuntius-server-linux-x86_64.tar.gz" | awk '{print $1}')"
  jq -n \
    --arg commitSha "$TARGET_SHA" \
    --argjson releaseSequence "$RELEASE_SEQUENCE" \
    --arg publishedAt "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" \
    --arg serverSha "$SERVER_ARCHIVE_SHA" \
    --arg serverBinarySha "$SERVER_BINARY_SHA" \
    --arg clientSha "$CLIENT_ARCHIVE_SHA" \
    --arg clientBinarySha "$CLIENT_BINARY_SHA" \
    '{
      schemaVersion: 1,
      commitSha: $commitSha,
      releaseSequence: $releaseSequence,
      publishedAt: $publishedAt,
      server: {
        file: "nuntius-server-linux-x86_64.tar.gz",
        sha256: $serverSha,
        binarySha256: $serverBinarySha,
        target: "x86_64-unknown-linux-gnu"
      },
      client: {
        file: "nuntius-client-macos-arm64.tar.gz",
        sha256: $clientSha,
        binarySha256: $clientBinarySha,
        target: "aarch64-apple-darwin"
      }
    }' > "$OUTPUT_DIR/manifest.json"
  touch "$OUTPUT_DIR/.complete"
  mv "$OUTPUT_DIR" "$RELEASE_DIR"
  printf '%s\n' "$RELEASE_SEQUENCE" > "$STATE_DIR/last-sequence"
  log "Release packages are verified and cached at $RELEASE_DIR."
fi

RELEASE_SEQUENCE="$(jq -r '.releaseSequence' "$RELEASE_DIR/manifest.json")"
if [ "$NO_DEPLOY" -eq 1 ]; then
  log "Build-only mode completed for $TARGET_SHA / $RELEASE_SEQUENCE."
  exit 0
fi

CLIENT_INFO="$(client_info)"
ACTIVE_TURNS="$(printf '%s' "$CLIENT_INFO" | jq -r '.activeTurns // 0')"
[[ "$ACTIVE_TURNS" =~ ^[0-9]+$ ]] || die "local Client returned an invalid activeTurns value"
if [ "$ACTIVE_TURNS" -gt 0 ] && [ "$FORCE_DEPLOY" -eq 0 ]; then
  log "Deployment deferred because the Client reports $ACTIVE_TURNS active turn(s)."
  if [ "$SCHEDULED" -eq 1 ]; then
    exit 0
  fi
  die "rerun with --force-deploy only after confirming the active work may be interrupted"
fi
if [ "$ACTIVE_TURNS" -gt 0 ]; then
  log "Force deployment accepted with $ACTIVE_TURNS active turn(s)."
fi

SERVER_INFO="$(server_info)"
SERVER_SHA="$(printf '%s' "$SERVER_INFO" | jq -r '.buildSha // ""')"
if [ "$SERVER_SHA" != "$TARGET_SHA" ]; then
  SERVER_ARCHIVE_SHA="$(jq -r '.server.sha256' "$RELEASE_DIR/manifest.json")"
  python3 "$SCRIPT_DIR/relay-server-update.py" \
    --archive "$RELEASE_DIR/nuntius-server-linux-x86_64.tar.gz" \
    --commit-sha "$TARGET_SHA" \
    --release-sequence "$RELEASE_SEQUENCE" \
    --archive-sha256 "$SERVER_ARCHIVE_SHA"
  log "Waiting for the Server to report the new build."
  server_ready=0
  for _ in $(seq 1 180); do
    SERVER_INFO="$(server_info)"
    if [ "$(printf '%s' "$SERVER_INFO" | jq -r '.buildSha // ""')" = "$TARGET_SHA" ]; then
      server_ready=1
      break
    fi
    sleep 2
  done
  [ "$server_ready" -eq 1 ] || die "Server did not report $TARGET_SHA within 360 seconds"
  log "Server is healthy on $TARGET_SHA."
else
  log "Server already runs $TARGET_SHA; skipping the relay."
fi

CLIENT_INFO="$(client_info)"
CLIENT_SHA="$(printf '%s' "$CLIENT_INFO" | jq -r '.buildSha // ""')"
if [ "$CLIENT_SHA" != "$TARGET_SHA" ]; then
  CLIENT_BINARY_SHA="$(jq -r '.client.binarySha256' "$RELEASE_DIR/manifest.json")"
  python3 "$SCRIPT_DIR/install-local-client.py" \
    --binary "$RELEASE_DIR/nuntius-client" \
    --binary-sha256 "$CLIENT_BINARY_SHA" \
    --commit-sha "$TARGET_SHA" \
    --release-sequence "$RELEASE_SEQUENCE"
else
  log "Client already runs $TARGET_SHA; skipping local replacement."
fi

CLIENT_INFO="$(client_info)"
SERVER_INFO="$(server_info)"
[ "$(printf '%s' "$CLIENT_INFO" | jq -r '.buildSha // ""')" = "$TARGET_SHA" ] \
  || die "Client verification failed after deployment"
[ "$(printf '%s' "$SERVER_INFO" | jq -r '.buildSha // ""')" = "$TARGET_SHA" ] \
  || die "Server verification failed after deployment"
printf '%s\n' "$TARGET_SHA" > "$STATE_DIR/last-successful-sha"
printf '%s\n' "$RELEASE_SEQUENCE" > "$STATE_DIR/last-successful-sequence"
log "Local release completed: $TARGET_SHA / $RELEASE_SEQUENCE."
