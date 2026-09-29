#!/usr/bin/env bash
# One-time setup on the office Mac: start Sourcer, take a first backup, and
# schedule a backup every day at 02:00.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
bash "$here/start.sh"
bash "$here/backup.sh"
bash "$here/install-backup-schedule.sh"
echo
echo "All set. Open http://localhost:8080 and sign in with Microsoft."
