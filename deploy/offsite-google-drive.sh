#!/usr/bin/env bash
# macOS: send an encrypted copy of every Sourcer backup to Google Drive.
# Needs Google Drive for desktop installed and signed in. Safe to run again.
#
# It makes a random key in ~/.sourcer-backup-key the first time. Save that key
# in your password manager: without it the copies on Google Drive cannot be
# opened, for example if this Mac is lost.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
conf="$HOME/.sourcer-backup.conf"
key="$HOME/.sourcer-backup-key"

drives=()
for d in "$HOME"/Library/CloudStorage/GoogleDrive-*/"My Drive"; do
  [ -d "$d" ] && drives+=("$d")
done
if [ "${#drives[@]}" -eq 0 ]; then
  echo "Google Drive for desktop was not found. Install it, sign in, then run this again." >&2
  exit 1
fi
if [ "${#drives[@]}" -gt 1 ] && [ -z "${GOOGLE_DRIVE:-}" ]; then
  echo "More than one Google Drive account is signed in. Choose one, for example:" >&2
  printf '  GOOGLE_DRIVE="%s" bash %s\n' "${drives[0]}" "$0" >&2
  exit 1
fi
drive="${GOOGLE_DRIVE:-${drives[0]}}"
target="$drive/Sourcer backups"
mkdir -p "$target"

if [ ! -s "$key" ]; then
  (umask 077 && openssl rand -base64 48 > "$key")
  new_key=1
fi
chmod 600 "$key"

cat > "$conf" <<CONF
# Written by offsite-google-drive.sh
OFFSITE_DIR="$target"
KEY_FILE="$key"
CONF
chmod 600 "$conf"

# Take a backup now, so the first copy is made and checked while you watch.
bash "$here/backup.sh"

echo
echo "Done. Encrypted copies now go to: $target"
if [ "${new_key:-0}" = 1 ]; then
  echo "IMPORTANT: save the key in your password manager now."
  echo "Open it with:  open -e \"$key\""
fi
