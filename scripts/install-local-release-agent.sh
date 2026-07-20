#!/bin/bash

set -euo pipefail
IFS=$'\n\t'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LABEL="com.helloanner.nuntius.local-release"
STATE_DIR="${NUNTIUS_RELEASE_STATE_DIR:-$HOME/Library/Application Support/Nuntius Local Release}"
PLIST_PATH="$HOME/Library/LaunchAgents/$LABEL.plist"
INTERVAL_SECONDS="${NUNTIUS_RELEASE_INTERVAL_SECONDS:-300}"

if ! [[ "$INTERVAL_SECONDS" =~ ^[0-9]+$ ]] || [ "$INTERVAL_SECONDS" -lt 60 ]; then
  printf 'NUNTIUS_RELEASE_INTERVAL_SECONDS must be an integer of at least 60.\n' >&2
  exit 2
fi
[ -x "$SCRIPT_DIR/local-release.sh" ] \
  || { printf 'local-release.sh is missing or not executable.\n' >&2; exit 1; }

mkdir -p "$HOME/Library/LaunchAgents" "$STATE_DIR/logs"
chmod 700 "$STATE_DIR" "$STATE_DIR/logs"

python3 - \
  "$PLIST_PATH" \
  "$LABEL" \
  "$SCRIPT_DIR/local-release.sh" \
  "$REPO_ROOT" \
  "$STATE_DIR" \
  "$INTERVAL_SECONDS" \
  "$HOME" <<'PY'
from pathlib import Path
import plistlib
import sys

plist_path, label, release_script, repository, state_dir, interval, home = sys.argv[1:]
payload = {
    "Label": label,
    "ProgramArguments": ["/bin/bash", release_script, "--scheduled"],
    "WorkingDirectory": repository,
    "RunAtLoad": True,
    "StartInterval": int(interval),
    "ProcessType": "Background",
    "LowPriorityIO": True,
    "Nice": 10,
    "EnvironmentVariables": {
        "PATH": f"/opt/homebrew/bin:/usr/local/bin:{home}/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        "NUNTIUS_RELEASE_SOURCE_REPO": repository,
        "NUNTIUS_RELEASE_STATE_DIR": state_dir,
    },
    "StandardOutPath": f"{state_dir}/logs/launchd.stdout.log",
    "StandardErrorPath": f"{state_dir}/logs/launchd.stderr.log",
}
with Path(plist_path).open("wb") as output:
    plistlib.dump(payload, output, sort_keys=False)
PY

chmod 600 "$PLIST_PATH"
plutil -lint "$PLIST_PATH"

DOMAIN="gui/$(id -u)"
launchctl bootout "$DOMAIN/$LABEL" >/dev/null 2>&1 || true
launchctl bootstrap "$DOMAIN" "$PLIST_PATH"
launchctl enable "$DOMAIN/$LABEL"
launchctl kickstart -k "$DOMAIN/$LABEL"

printf 'Installed %s\n' "$PLIST_PATH"
printf 'The Mac mini now checks origin/main every %s seconds.\n' "$INTERVAL_SECONDS"
printf 'Logs: %s\n' "$STATE_DIR/logs"
