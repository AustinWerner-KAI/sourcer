#!/usr/bin/env bash
# Build and start Sourcer (server + Postgres) with Docker Compose, then wait
# until it answers. Safe to run again: it rebuilds and restarts in place.
set -euo pipefail
cd "$(dirname "$0")"

if [ ! -f .env ]; then
  echo "Missing deploy/.env. Copy ../.env.example to deploy/.env and fill it in." >&2
  exit 1
fi
if ! grep -Eq '^POSTGRES_PASSWORD=.{16,}$' .env || grep -q '^POSTGRES_PASSWORD=change-me' .env; then
  echo "Set POSTGRES_PASSWORD in deploy/.env to a long random value first." >&2
  exit 1
fi
# The key that encrypts Outlook connections: made once, kept in .env, never printed.
if ! grep -Eq '^MAIL_TOKEN_KEY=.{40,}$' .env; then
  sed -i.bak '/^MAIL_TOKEN_KEY=/d' .env && rm -f .env.bak
  printf '\nMAIL_TOKEN_KEY=%s\n' "$(openssl rand -base64 32)" >> .env
  echo "Created the Outlook encryption key in deploy/.env."
fi
if ! docker info >/dev/null 2>&1; then
  echo "Docker is not running. Open Docker Desktop, wait for it to start, then run this again." >&2
  exit 1
fi

docker compose up -d --build

echo "Waiting for Sourcer to start..."
for _ in $(seq 1 60); do
  if curl -fsS http://127.0.0.1:8080/api/health 2>/dev/null | grep -q '"database":true'; then
    echo "Sourcer is running: http://localhost:8080"
    exit 0
  fi
  sleep 2
done
echo "Sourcer did not answer within 2 minutes. Recent logs:" >&2
docker compose logs --tail 40 server >&2
exit 1
