#!/bin/bash

set -euo pipefail

LABEL="com.helloanner.nuntius.local-release"
PLIST_PATH="$HOME/Library/LaunchAgents/$LABEL.plist"
DOMAIN="gui/$(id -u)"

launchctl bootout "$DOMAIN/$LABEL" >/dev/null 2>&1 || true
if [ -f "$PLIST_PATH" ]; then
  unlink "$PLIST_PATH"
fi

printf 'Uninstalled %s. Build caches and release history were preserved.\n' "$LABEL"
