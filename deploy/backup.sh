#!/usr/bin/env bash
# Back up the Sourcer database to one compressed file, check it can be read,
# and keep the most recent KEEP_DAYS days of backups (default 30).
# Backups hold candidate data, so they live outside the code folder.
set -euo pipefail
cd "$(dirname "$0")"

BACKUP_DIR="${BACKUP_DIR:-$HOME/sourcer-backups}"
KEEP_DAYS="${KEEP_DAYS:-30}"
mkdir -p "$BACKUP_DIR"
chmod 700 "$BACKUP_DIR"

stamp="$(date +%Y-%m-%d-%H%M%S)"
file="$BACKUP_DIR/sourcer-$stamp.dump"
tmp="$file.partial"

# Custom format: compressed, and restorable one table at a time if needed.
docker compose exec -T db pg_dump -U sourcer -d sourcer -Fc > "$tmp"

# A backup that cannot be read is not a backup.
if ! docker compose exec -T db pg_restore --list < "$tmp" >/dev/null; then
  rm -f "$tmp"
  echo "$(date '+%F %T') backup FAILED: file could not be read" >&2
  exit 1
fi
chmod 600 "$tmp"
mv "$tmp" "$file"

# Only ever removes this script's own files, and never the newest one.
find "$BACKUP_DIR" -maxdepth 1 -name 'sourcer-*.dump' -mtime +"$KEEP_DAYS" -print -delete

echo "$(date '+%F %T') backup OK: $file ($(du -h "$file" | cut -f1))"
