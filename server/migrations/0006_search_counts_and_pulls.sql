-- Sprint 2B: count, then pull, from a confirmed brief (SRS F5, N11, N14).

-- One press of "Count matches": a count per location, for one brief version.
-- `locations` is [{label, total, query}] and stays NULL while counting, so a
-- second press with the same key waits rather than paying again.
CREATE TABLE search_count (
  id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id          uuid NOT NULL REFERENCES org(id),
  role_id         uuid NOT NULL REFERENCES role(id),
  brief_id        uuid NOT NULL REFERENCES brief(id),
  created_by      uuid REFERENCES app_user(id),
  locations       jsonb,
  credits_used    int NOT NULL DEFAULT 0,
  idempotency_key text NOT NULL,
  created_at      timestamptz NOT NULL DEFAULT now(),
  UNIQUE (org_id, idempotency_key)
);
CREATE INDEX search_count_role ON search_count (role_id, created_at DESC);

-- One press of "Pull": how many people were asked for across its locations.
-- Each location runs as its own background job and records a `run`.
CREATE TABLE pull (
  id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id          uuid NOT NULL REFERENCES org(id),
  role_id         uuid NOT NULL REFERENCES role(id),
  brief_id        uuid NOT NULL REFERENCES brief(id),
  count_id        uuid NOT NULL REFERENCES search_count(id),
  created_by      uuid REFERENCES app_user(id),
  requested       int NOT NULL CHECK (requested > 0),
  locations       int NOT NULL CHECK (locations > 0),
  idempotency_key text NOT NULL,
  created_at      timestamptz NOT NULL DEFAULT now(),
  UNIQUE (org_id, idempotency_key)
);
CREATE INDEX pull_role ON pull (role_id, created_at DESC);
-- A count is pulled from once: pulling it again would pay for the same people.
CREATE UNIQUE INDEX pull_once_per_count ON pull (count_id);

ALTER TABLE run ADD COLUMN pull_id uuid REFERENCES pull(id);
ALTER TABLE run ADD COLUMN location text;
ALTER TABLE run ADD COLUMN new_candidates int NOT NULL DEFAULT 0;
ALTER TABLE run ADD COLUMN left_out int NOT NULL DEFAULT 0;
ALTER TABLE run ADD COLUMN unknown_employer int NOT NULL DEFAULT 0;
-- Set once the people are saved. A run charged but not finished is retried
-- as a failure rather than counted as done.
ALTER TABLE run ADD COLUMN finished_at timestamptz;
CREATE INDEX run_pull ON run (pull_id) WHERE pull_id IS NOT NULL;
-- Credits used this month are summed per organisation.
CREATE INDEX run_org_month ON run (org_id, created_at);
