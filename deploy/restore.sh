#!/usr/bin/env bash
# Replace the Sourcer database with a backup made by backup.sh.
# Usage: deploy/restore.sh ~/sourcer-backups/sourcer-2026-09-29-020000.dump
set -euo pipefail
cd "$(dirname "$0")"

file="${1:-}"
if [ -z "$file" ] || [ ! -f "$file" ]; then
  echo "Usage: $0 <backup file>" >&2
  exit 1
fi
docker compose exec -T db pg_restore --list < "$file" >/dev/null

if [ "${RESTORE_CONFIRM:-}" != "yes" ]; then
  read -r -p "This replaces ALL current Sourcer data with $file. Type 'yes' to continue: " answer
  [ "$answer" = "yes" ] || { echo "Cancelled."; exit 1; }
fi

# Keep a copy of what is there now, in case this was the wrong file.
bash ./backup.sh

docker compose stop server
docker compose exec -T db dropdb -U sourcer --if-exists sourcer
docker compose exec -T db createdb -U sourcer sourcer
docker compose exec -T db pg_restore -U sourcer -d sourcer --no-owner < "$file"
docker compose start server
echo "Restored from $file"
