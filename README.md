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

```bash
cp .env.example deploy/.env   # fill in values
cd deploy && docker compose up -d --build
```

The server listens on `127.0.0.1:8080` only. Expose it through the VPN or a reverse proxy, never directly.

## Rules for contributors

- Secrets live in environment variables only. Never in code, logs or the browser.
- No LinkedIn automation. The extension saves the profile a person is viewing, nothing more.
- Nothing is sent to a candidate without a person approving that message.
