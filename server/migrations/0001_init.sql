-- Sourcer initial schema. Every table carries org_id so other agencies can be
-- hosted later without a migration. See the design doc linked in docs/README.md.

CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TYPE candidacy_state AS ENUM (
  'found', 'known_checked', 'ranked', 'shortlisted', 'rejected', 'drafted',
  'approved', 'contacted', 'replied', 'no_reply', 'handed_to_ats'
);
CREATE TYPE reason_code AS ENUM ('FIT','SENIOR','JUNIOR','FUNCTION','SKILL','LOCATION','EMPLOYER','KNOWN');
CREATE TYPE channel AS ENUM ('email', 'linkedin', 'whatsapp');
CREATE TYPE touch_direction AS ENUM ('out', 'in');
CREATE TYPE user_role AS ENUM ('admin', 'resourcer');
CREATE TYPE rule_kind AS ENUM ('filter', 'weight', 'draft');
CREATE TYPE rule_status AS ENUM ('proposed', 'active', 'rejected', 'superseded');
CREATE TYPE job_status AS ENUM ('queued', 'running', 'done', 'failed');

CREATE TABLE org (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  name        text NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE app_user (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id      uuid NOT NULL REFERENCES org(id),
  email       text NOT NULL,
  name        text NOT NULL,
  role        user_role NOT NULL DEFAULT 'resourcer',
  mailbox_provider text,             -- 'google' | 'microsoft'
  mailbox_token_enc bytea,           -- encrypted at rest; never returned by the API
  created_at  timestamptz NOT NULL DEFAULT now(),
  UNIQUE (org_id, email)
);

CREATE TABLE client (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id      uuid NOT NULL REFERENCES org(id),
  name        text NOT NULL,
  domain      text,
  off_limits  boolean NOT NULL DEFAULT false,  -- never approach their staff
  notes       text,
  created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE role (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id      uuid NOT NULL REFERENCES org(id),
  client_id   uuid REFERENCES client(id),
  title       text NOT NULL,
  owner_id    uuid REFERENCES app_user(id),
  status      text NOT NULL DEFAULT 'open',
  created_at  timestamptz NOT NULL DEFAULT now()
);

-- The confirmed five-line check. Immutable once confirmed; edits create a new version.
CREATE TABLE brief (
  id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id        uuid NOT NULL REFERENCES org(id),
  role_id       uuid NOT NULL REFERENCES role(id),
  version       int  NOT NULL,
  level         text NOT NULL,
  must_haves    jsonb NOT NULL,   -- ordered list
  tools         jsonb NOT NULL,   -- [{name, status: required|nice|replacing}]
  locations     jsonb NOT NULL,
  employer_types jsonb NOT NULL,
  source_text_hash text,
  confirmed_by  uuid REFERENCES app_user(id),
  confirmed_at  timestamptz,
  UNIQUE (role_id, version)
);

-- One human, merged across sources and shared by the whole team.
CREATE TABLE person (
  id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id        uuid NOT NULL REFERENCES org(id),
  pdl_id        text,
  linkedin_url  text,
  apollo_id     text,
  full_name     text NOT NULL,
  current_title text,
  current_employer text,
  location      text,
  opted_out     boolean NOT NULL DEFAULT false,  -- overrides everything
  last_seen     timestamptz,
  created_at    timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX person_pdl ON person (org_id, pdl_id) WHERE pdl_id IS NOT NULL;
CREATE UNIQUE INDEX person_linkedin ON person (org_id, linkedin_url) WHERE linkedin_url IS NOT NULL;

CREATE TABLE employment (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id      uuid NOT NULL REFERENCES org(id),
  person_id   uuid NOT NULL REFERENCES person(id) ON DELETE CASCADE,
  employer    text NOT NULL,
  title       text,
  start_date  date,
  end_date    date
);

CREATE TABLE candidacy (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id      uuid NOT NULL REFERENCES org(id),
  person_id   uuid NOT NULL REFERENCES person(id) ON DELETE CASCADE,
  role_id     uuid NOT NULL REFERENCES role(id),
  brief_id    uuid NOT NULL REFERENCES brief(id),
  state       candidacy_state NOT NULL DEFAULT 'found',
  rank        int,
  tier        text,
  evidence    jsonb,
  owner_id    uuid REFERENCES app_user(id),
  reason      reason_code,
  decided_by  uuid REFERENCES app_user(id),
  decided_at  timestamptz,
  version     int NOT NULL DEFAULT 0,   -- optimistic concurrency
  UNIQUE (person_id, brief_id)
);

CREATE TABLE draft (
  id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id        uuid NOT NULL REFERENCES org(id),
  candidacy_id  uuid NOT NULL REFERENCES candidacy(id) ON DELETE CASCADE,
  channel       channel NOT NULL,
  text          text NOT NULL,
  model_version text,
  edited_text   text,
  approved_by   uuid REFERENCES app_user(id),
  approved_at   timestamptz,
  created_at    timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE sequence (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id      uuid NOT NULL REFERENCES org(id),
  role_id     uuid NOT NULL REFERENCES role(id),
  steps       jsonb NOT NULL   -- [{channel, delay_days}]
);

-- Every message out or in, on any channel. Drives the known check and reply stop.
CREATE TABLE touch (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id      uuid NOT NULL REFERENCES org(id),
  person_id   uuid NOT NULL REFERENCES person(id) ON DELETE CASCADE,
  role_id     uuid REFERENCES role(id),
  user_id     uuid REFERENCES app_user(id),
  channel     channel NOT NULL,
  direction   touch_direction NOT NULL,
  message_id  text,
  sequence_step int,
  at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX touch_person_at ON touch (person_id, at DESC);

CREATE TABLE rule (
  id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id        uuid NOT NULL REFERENCES org(id),
  code          text NOT NULL,      -- R1, R2 ...
  text          text NOT NULL,
  kind          rule_kind NOT NULL,
  source        text NOT NULL,
  status        rule_status NOT NULL DEFAULT 'proposed',
  superseded_by uuid REFERENCES rule(id),
  created_at    timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE run (
  id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id        uuid NOT NULL REFERENCES org(id),
  role_id       uuid NOT NULL REFERENCES role(id),
  brief_id      uuid NOT NULL REFERENCES brief(id),
  queries       jsonb NOT NULL,
  records_pulled int NOT NULL DEFAULT 0,
  credits_used  int NOT NULL DEFAULT 0,
  accepted      int,
  top_reject    reason_code,
  idempotency_key text NOT NULL,
  created_at    timestamptz NOT NULL DEFAULT now(),
  UNIQUE (org_id, idempotency_key)
);

CREATE TABLE audit (
  id          bigserial PRIMARY KEY,
  org_id      uuid NOT NULL REFERENCES org(id),
  actor_id    uuid REFERENCES app_user(id),
  action      text NOT NULL,
  target      text NOT NULL,
  at          timestamptz NOT NULL DEFAULT now()
);

-- Background work queue, claimed with SELECT ... FOR UPDATE SKIP LOCKED.
CREATE TABLE job (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id      uuid NOT NULL REFERENCES org(id),
  kind        text NOT NULL,
  payload     jsonb NOT NULL,
  status      job_status NOT NULL DEFAULT 'queued',
  attempts    int NOT NULL DEFAULT 0,
  last_error  text,
  run_after   timestamptz NOT NULL DEFAULT now(),
  created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX job_ready ON job (status, run_after);
