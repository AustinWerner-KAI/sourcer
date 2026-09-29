#!/usr/bin/env bash
# macOS: back up Sourcer every day at 02:00 with launchd. If the Mac is asleep
# at 02:00, the backup runs when it wakes. Run again to update; safe to repeat.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
label="io.austinwerner.sourcer.backup"
plist="$HOME/Library/LaunchAgents/$label.plist"
log="$HOME/sourcer-backups/backup.log"
mkdir -p "$HOME/Library/LaunchAgents" "$HOME/sourcer-backups"

cat > "$plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$label</string>
  <key>ProgramArguments</key>
  <array><string>/bin/bash</string><string>$here/backup.sh</string></array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>/usr/local/bin:/opt/homebrew/bin:/Applications/Docker.app/Contents/Resources/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
  </dict>
  <key>StartCalendarInterval</key>
  <dict><key>Hour</key><integer>2</integer><key>Minute</key><integer>0</integer></dict>
  <key>StandardOutPath</key><string>$log</string>
  <key>StandardErrorPath</key><string>$log</string>
</dict>
</plist>
PLIST

launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$plist"
echo "Daily backup scheduled for 02:00. Log: $log"
