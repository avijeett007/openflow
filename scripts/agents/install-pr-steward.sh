#!/usr/bin/env bash
# Install the nightly PR steward as a macOS LaunchAgent (recommended over cron:
# a LaunchAgent runs in your logged-in user session, so `claude`/`gh` keychain auth
# works). Idempotent — re-run to update the schedule or path.
#
#   ./scripts/agents/install-pr-steward.sh              # nightly 02:30
#   STEWARD_HOUR=3 STEWARD_MIN=0 ./scripts/agents/install-pr-steward.sh
#   ./scripts/agents/install-pr-steward.sh --uninstall
set -euo pipefail

LABEL="com.openflow.pr-steward"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STEWARD="$SCRIPT_DIR/pr-steward.sh"
HOUR="${STEWARD_HOUR:-2}"
MIN="${STEWARD_MIN:-30}"

if [[ "${1:-}" == "--uninstall" ]]; then
  launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
  rm -f "$PLIST"
  echo "Uninstalled $LABEL."
  exit 0
fi

if [[ "$(uname)" != "Darwin" ]]; then
  echo "This installer is macOS-only. On Linux, add a cron line:" >&2
  echo "  $MIN $HOUR * * *  $STEWARD >> \$HOME/.openflow-steward/cron.log 2>&1" >&2
  exit 1
fi

chmod +x "$STEWARD"
mkdir -p "$HOME/Library/LaunchAgents" "$HOME/.openflow-steward/logs"

cat >"$PLIST" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/bash</string>
    <string>$STEWARD</string>
  </array>
  <key>StartCalendarInterval</key>
  <dict>
    <key>Hour</key><integer>$HOUR</integer>
    <key>Minute</key><integer>$MIN</integer>
  </dict>
  <key>RunAtLoad</key><false/>
  <key>StandardOutPath</key><string>$HOME/.openflow-steward/launchd.out.log</string>
  <key>StandardErrorPath</key><string>$HOME/.openflow-steward/launchd.err.log</string>
</dict>
</plist>
PLIST_EOF

# Reload (bootout then bootstrap) so a changed schedule takes effect.
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$PLIST"

echo "Installed $LABEL — runs nightly at $(printf '%02d:%02d' "$HOUR" "$MIN") local."
echo "  Plist:  $PLIST"
echo "  Logs:   $HOME/.openflow-steward/logs/"
echo "  Run now (test):  $STEWARD"
echo "  Pause:  touch $(cd "$SCRIPT_DIR/../.." && pwd)/agents/KILL-STEWARD"
echo "  Remove: $0 --uninstall"
