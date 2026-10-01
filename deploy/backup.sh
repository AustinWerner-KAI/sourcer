#!/usr/bin/env bash
# Back up the Sourcer database to one compressed file, check it can be read,
# and keep the most recent KEEP_DAYS days of backups (default 30).
# Backups hold candidate data, so they live outside the code folder.
#
# Off the machine: when ~/.sourcer-backup.conf names an OFFSITE_DIR (set up by
# offsite-google-drive.sh), an encrypted copy also goes there, for example
# Google Drive. It is encrypted with the key in ~/.sourcer-backup-key, so the
# copy in the cloud cannot be read without it.
set -euo pipefail
cd "$(dirname "$0")"

CONF="${SOURCER_BACKUP_CONF:-$HOME/.sourcer-backup.conf}"
# shellcheck disable=SC1090
[ -f "$CONF" ] && . "$CONF"

BACKUP_DIR="${BACKUP_DIR:-$HOME/sourcer-backups}"
KEEP_DAYS="${KEEP_DAYS:-30}"
OFFSITE_DIR="${OFFSITE_DIR:-}"
KEY_FILE="${KEY_FILE:-$HOME/.sourcer-backup-key}"
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

[ -n "$OFFSITE_DIR" ] || exit 0

# ---- The encrypted copy off the machine ----
if [ ! -d "$OFFSITE_DIR" ]; then
  echo "$(date '+%F %T') offsite copy FAILED: $OFFSITE_DIR is not there (is Google Drive running and signed in?)" >&2
  exit 1
fi
if [ ! -s "$KEY_FILE" ]; then
  echo "$(date '+%F %T') offsite copy FAILED: no key at $KEY_FILE" >&2
  exit 1
fi
enc="$OFFSITE_DIR/$(basename "$file").enc"
enc_tmp="$BACKUP_DIR/$(basename "$file").enc.partial"
openssl enc -aes-256-cbc -pbkdf2 -iter 200000 -salt -pass "file:$KEY_FILE" -in "$file" -out "$enc_tmp"

# Prove the encrypted copy decrypts to a readable backup before it leaves.
if ! openssl enc -d -aes-256-cbc -pbkdf2 -iter 200000 -pass "file:$KEY_FILE" -in "$enc_tmp" \
    | docker compose exec -T db pg_restore --list >/dev/null; then
  rm -f "$enc_tmp"
  echo "$(date '+%F %T') offsite copy FAILED: the encrypted copy did not decrypt" >&2
  exit 1
fi
mv "$enc_tmp" "$enc"
find "$OFFSITE_DIR" -maxdepth 1 -name 'sourcer-*.dump.enc' -mtime +"$KEEP_DAYS" -print -delete
echo "$(date '+%F %T') offsite copy OK: $enc"
