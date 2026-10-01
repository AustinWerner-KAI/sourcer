# Sourcer

Resourcing and outreach for Austin Werner. A job spec goes in; a ranked list, team-memory checks, contact details and personal outreach drafts come out. Interested candidates are handed to the ATS.

- Product and solution design: see `docs/README.md`
- Requirements: `docs/SRS.md`
- Status: **Sprint 1** (M0 Foundations)

## Layout

| Folder | What |
| --- | --- |
| `server/` | Rust service: Axum API, Tokio worker, SQLx + Postgres migrations |
| `web/` | TypeScript app: React, Vite, TanStack Query and Router |
| `deploy/` | Dockerfile and Docker Compose for private hosting |
| `docs/` | Design links and decisions |

## Run locally

Needs Rust (stable), Node 22 and Postgres 16.

```bash
# 1. Database
createdb sourcer

# 2. Server (runs migrations on start)
cd server
DATABASE_URL=postgres://localhost/sourcer cargo run

# 3. Web app (proxies /api to the server)
cd ../web
npm install
npm run dev   # http://localhost:5173
```

Database tests run when `TEST_DATABASE_URL` points at an empty Postgres database (for example `createdb sourcer_test`, then `TEST_DATABASE_URL=postgres://localhost/sourcer_test cargo test`). Without it they are skipped.

`cargo test` also regenerates the shared TypeScript types in `web/src/api/types/`. Commit them with your change; CI fails if they drift.

## Deploy

Runs on one machine with Docker (the office Mac). Needs Docker Desktop running.

```bash
cp .env.example deploy/.env   # fill in values; POSTGRES_PASSWORD must be long and random
bash deploy/setup-mac.sh      # first time: start, first backup, daily 02:00 backup
bash deploy/start.sh          # after an update: rebuild and restart
```

The server listens on `127.0.0.1:8080` only. Expose it through the VPN or a reverse proxy, never directly.

**Backups** go to `~/sourcer-backups` (outside the code, since they hold candidate data). Each one is checked after it is written, and 30 days are kept. The log is `~/sourcer-backups/backup.log`.

- Back up now: `bash deploy/backup.sh`
- Restore: `bash deploy/restore.sh ~/sourcer-backups/<file>.dump` (takes a backup of the current data first)
- **Off the machine (Google Drive):** run `bash deploy/offsite-google-drive.sh` once, with Google Drive for desktop installed and signed in. Every backup then also goes, encrypted, to `My Drive/Sourcer backups`. The key is made in `~/.sourcer-backup-key`: save it in your password manager, because without it the Google Drive copies cannot be opened.
- Restore from Google Drive: `bash deploy/restore.sh "<path to>.dump.enc"` (with the key back in `~/.sourcer-backup-key`).

## Rules for contributors

- Secrets live in environment variables only. Never in code, logs or the browser.
- No LinkedIn automation. The extension saves the profile a person is viewing, nothing more.
- Nothing is sent to a candidate without a person approving that message.
